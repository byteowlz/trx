//! End-to-end tests for the optional central store (epic trx-a1s8, slices
//! .3/.4): `trx central init`, `trx store sync init --remote`, `trx store sync [status|pull|push]`,
//! and automatic pull at open / commit+push at exit.

use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::str::contains;

fn git(dir: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git should be available");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A hermetic per-test environment: isolated HOME (no global trx config) and
/// git identity env so spawned git operations never touch the real user.
struct Env {
    home: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com");
    }
}

fn repo_cmd(env: &Env, repo: &Path) -> Command {
    let mut cmd = Command::cargo_bin("trx").unwrap();
    cmd.current_dir(repo);
    env.apply(&mut cmd);
    cmd
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("file.txt"), "hello\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "initial"]);
}

#[test]
fn central_init_dry_run_writes_nothing() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("repo");
    let store = base.path().join("store");
    init_git_repo(&repo);

    repo_cmd(&env, &repo).args(["init"]).assert().success();

    // Dry run on a fresh checkout: reports the plan, writes nothing.
    repo_cmd(&env, &repo)
        .args(["central", "init", "--dry-run", "--store-root"])
        .arg(&store)
        .assert()
        .success()
        .stdout(contains("dry run — nothing written"))
        .stdout(contains("identity:      git:"));
    assert!(!store.exists(), "dry run must not create the store");
    assert!(
        !repo.join(".trx/central").exists(),
        "dry run must not write the marker"
    );

    // JSON schema includes the plan fields.
    repo_cmd(&env, &repo)
        .args(["central", "init", "--dry-run", "--json", "--store-root"])
        .arg(&store)
        .assert()
        .success()
        .stdout(contains("\"planned_ledger\""));
    assert!(!store.exists());
}

#[test]
fn config_override_must_exist_and_is_used() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("repo");
    let store = base.path().join("store");
    let missing = base.path().join("missing-config.toml");
    let custom = base.path().join("custom-config.toml");
    std::fs::write(&custom, format!("store_root = \"{}\"\n", store.display())).unwrap();
    init_git_repo(&repo);
    repo_cmd(&env, &repo).args(["init"]).assert().success();

    // A selected override that does not exist is an error (mmry parity).
    repo_cmd(&env, &repo)
        .env("TRX_CONFIG", &missing)
        .args(["central", "init"])
        .assert()
        .failure()
        .stderr(contains("does not exist"));

    // A valid override is honored: the store lands at the custom root.
    repo_cmd(&env, &repo)
        .env("TRX_CONFIG", &custom)
        .args(["central", "init"])
        .assert()
        .success();
    assert!(store.join("repos").is_dir());
}

#[test]
fn central_init_routes_issues_to_the_store_and_a_worktree_sees_them() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("repo");
    let store = base.path().join("store");
    init_git_repo(&repo);

    repo_cmd(&env, &repo)
        .args(["init", "--prefix", "app"])
        .assert()
        .success();

    // Empty repo: opting in is allowed.
    repo_cmd(&env, &repo)
        .args(["central", "init", "--store-root"])
        .arg(&store)
        .assert()
        .success()
        .stdout(contains("Central mode enabled"));

    // Marker exists and is idempotent.
    assert!(repo.join(".trx/central").is_file());
    repo_cmd(&env, &repo)
        .args(["central", "init", "--store-root"])
        .arg(&store)
        .assert()
        .success()
        .stdout(contains("Already in central mode"));

    // Issues now land in the store, not the checkout (init leaves an empty
    // placeholder file — the authoritative ledger must not gain the issue).
    repo_cmd(&env, &repo)
        .args(["create", "from main checkout"])
        .assert()
        .success();
    let local_ledger = std::fs::read_to_string(repo.join(".trx/issues.jsonl")).unwrap();
    assert!(!local_ledger.contains("from main checkout"));

    // A worktree of the same repository shares the central ledger.
    let wt = base.path().join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            wt.to_str().unwrap(),
            "-b",
            "feature",
        ],
    );
    std::fs::create_dir_all(wt.join(".trx")).unwrap();
    std::fs::write(wt.join(".trx/central"), "# central\n").unwrap();
    repo_cmd(&env, &wt)
        .env("TRX_STORE_ROOT", &store)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("from main checkout"));

    // status reports central mode with identity and ledger location.
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["central", "status"])
        .assert()
        .success()
        .stdout(contains("mode:     central"))
        .stdout(contains("identity: git:"));
}

#[test]
fn central_init_refuses_to_shadow_existing_repo_local_issues() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("repo");
    let store = base.path().join("store");
    init_git_repo(&repo);

    repo_cmd(&env, &repo).args(["init"]).assert().success();
    repo_cmd(&env, &repo)
        .args(["create", "precious"])
        .assert()
        .success();

    repo_cmd(&env, &repo)
        .args(["central", "init", "--store-root"])
        .arg(&store)
        .assert()
        .failure()
        .stderr(contains("refusing to shadow"));

    // Nothing changed: the issue is still served from the repo-local ledger.
    repo_cmd(&env, &repo)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("precious"));
}

#[test]
fn store_sync_connects_two_machines_and_drains_offline_pending() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("repo");
    let store = base.path().join("store");
    let remote = base.path().join("remote.git");
    init_git_repo(&repo);

    repo_cmd(&env, &repo).args(["init"]).assert().success();
    repo_cmd(&env, &repo)
        .args(["central", "init", "--store-root"])
        .arg(&store)
        .assert()
        .success();

    // Opt the store into git sync against a local bare remote.
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["store", "sync", "init", "--remote"])
        .arg(remote.join(".").to_str().unwrap())
        .assert()
        .success()
        .stdout(contains("pushed to remote"));

    // status reports the remote and no pending commits.
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["store", "sync", "status", "--json"])
        .assert()
        .success()
        .stdout(contains(format!(
            "\"remote\": \"{}\"",
            remote.join(".").display()
        )))
        .stdout(contains("\"pending_push\": 0"));

    // Create an issue: the automatic final sync commits and pushes it.
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["create", "synced across machines"])
        .assert()
        .success();

    // "Second machine": a worktree checkout of the same repository pulls it
    // automatically at first store access.
    let wt = base.path().join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            wt.to_str().unwrap(),
            "-b",
            "feature",
        ],
    );
    std::fs::create_dir_all(wt.join(".trx")).unwrap();
    std::fs::write(wt.join(".trx/central"), "# central\n").unwrap();
    repo_cmd(&env, &wt)
        .env("TRX_STORE_ROOT", &store)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("synced across machines"));

    // Go offline: a manual sync keeps local data and reports pending commits.
    let hidden = base.path().join("hidden");
    std::fs::rename(&remote, &hidden).unwrap();
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["create", "written while offline"])
        .assert()
        .success();
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["store", "sync"])
        .assert()
        .success()
        .stdout(contains("pending"));
    // Data is safe locally.
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("written while offline"));

    // Back online: the next sync drains the pending commits.
    std::fs::rename(&hidden, &remote).unwrap();
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["store", "sync"])
        .assert()
        .success()
        .stdout(contains("pushed to remote"));
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["store", "sync", "status", "--json"])
        .assert()
        .success()
        .stdout(contains("\"pending_push\": 0"));
}

#[test]
fn store_root_override_selects_an_isolated_store() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("repo");
    let store1 = base.path().join("store1");
    let store2 = base.path().join("store2");
    init_git_repo(&repo);

    repo_cmd(&env, &repo).args(["init"]).assert().success();
    repo_cmd(&env, &repo)
        .args(["central", "init", "--store-root"])
        .arg(&store1)
        .assert()
        .success();

    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store1)
        .args(["create", "in store one"])
        .assert()
        .success();

    // A different store root must not see store one's ledger.
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store2)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("No issues found"));
    // Store one still serves the issue from its own ledger.
    repo_cmd(&env, &repo)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("in store one"));
    let store1_ledger = std::fs::read_dir(store1.join("repos")).unwrap().count();
    assert!(store1_ledger >= 1, "store1 must hold the registered repo");
    // None of store two's ledgers may contain store one's issue.
    let leaked = std::fs::read_dir(store2.join("repos"))
        .unwrap()
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("issues.jsonl")).ok())
        .any(|content| content.contains("in store one"));
    assert!(!leaked, "store2 must never contain store one's issues");
}
