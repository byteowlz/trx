//! End-to-end tests for the optional central store (epic trx-a1s8, slices
//! .3/.4): `trx central init`, `trx store sync init --remote`, `trx store sync [status|pull|push]`,
//! and automatic pull at open / commit+push at exit.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::str::contains;
use trx_core::CentralStore;
use trx_core::central;

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
fn migrate_moves_repo_local_ledger_to_the_central_store() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("repo");
    let store = base.path().join("store");
    init_git_repo(&repo);

    repo_cmd(&env, &repo)
        .args(["init", "--prefix", "app"])
        .assert()
        .success();
    repo_cmd(&env, &repo)
        .args(["create", "migrated one"])
        .assert()
        .success();
    repo_cmd(&env, &repo)
        .args(["create", "migrated two"])
        .assert()
        .success();
    git(&repo, &["add", ".trx"]);
    git(&repo, &["commit", "-q", "-m", "tracker"]);

    // Dry run: plan only, nothing moves.
    repo_cmd(&env, &repo)
        .args(["migrate", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("(dry run)"));
    assert!(repo.join(".trx/issues.jsonl").exists());
    assert!(central::read_marker(&repo).unwrap().is_none());

    // Real migration with untrack: ledger moves, checkout switches to central.
    repo_cmd(&env, &repo)
        .env("TRX_STORE_ROOT", &store)
        .args(["migrate", "--untrack"])
        .assert()
        .success()
        .stdout(contains("2 issues"))
        .stdout(contains("backup:"));
    assert!(repo.join(".trx/central").exists());
    assert!(repo.join(".trx/MIGRATED").exists());
    assert!(!repo.join(".trx/issues.jsonl").exists());
    let status = StdCommand::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.contains("D  .trx/issues.jsonl"),
        "staged deletion: {status}"
    );

    // The checkout now serves issues from the central store.
    repo_cmd(&env, &repo)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("migrated one"))
        .stdout(contains("migrated two"));

    // A worktree of the same repository inherits the committed .trx ledger
    // (which carries no marker — markers are machine-local). Store selection
    // is explicit until the checkout has its own marker: same env/flag rules.
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
    assert!(
        wt.join(".trx/issues.jsonl").exists(),
        "worktree starts from the committed tracker"
    );
    repo_cmd(&env, &wt)
        .env("TRX_STORE_ROOT", &store)
        .args(["migrate"])
        .assert()
        .success()
        .stdout(contains("2 issues"));
    // Nothing new to merge — all snapshot lines already exist centrally.
    assert_eq!(
        fs::read_to_string(
            trx_core::CentralStore::open_at(store.clone())
                .find(&central::Checkout::at(&wt).unwrap())
                .unwrap()
                .unwrap()
                .issues_path()
        )
        .unwrap()
        .lines()
        .count(),
        2,
        "idempotent re-merge must not duplicate snapshot lines"
    );
    repo_cmd(&env, &wt)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("migrated one"));
}

#[test]
fn setup_scans_and_migrates_all_found_ledgers() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("workspaces");
    let config_file = base.path().join("trx-config.toml");
    std::fs::write(&config_file, "# test override config\n").unwrap();
    let store = base.path().join("store");

    for name in ["alpha", "beta"] {
        let repo = root.join(name);
        init_git_repo(&repo);
        repo_cmd(&env, &repo).args(["init"]).assert().success();
        repo_cmd(&env, &repo)
            .args(["create", &format!("issue in {name}")])
            .assert()
            .success();
    }
    // Junk that scanning must skip.
    let junk = root.join("node_modules/pkg");
    init_git_repo(&junk);
    repo_cmd(&env, &junk).args(["init"]).assert().success();
    repo_cmd(&env, &junk)
        .args(["create", "must be skipped"])
        .assert()
        .success();

    // Dry run lists exactly the two real repos.
    repo_cmd(&env, &root)
        .env("TRX_CONFIG", &config_file)
        .env("TRX_STORE_ROOT", &store)
        .args(["setup", "--dry-run", "--scan"])
        .arg(&root)
        .assert()
        .success()
        .stdout(contains("Found 2 repo-local ledger(s)"));
    assert_eq!(
        std::fs::read_to_string(&config_file).unwrap(),
        "# test override config\n",
        "dry run must not write config"
    );

    // Real run with --yes (non-interactive): migrates both, sets migrate=auto.
    repo_cmd(&env, &root)
        .env("TRX_CONFIG", &config_file)
        .env("TRX_STORE_ROOT", &store)
        .args(["setup", "--scan"])
        .arg(&root)
        .arg("--yes")
        .assert()
        .success()
        .stdout(contains("migrate = \"auto\""));
    let config_text = std::fs::read_to_string(&config_file).unwrap();
    assert!(config_text.contains("migrate = \"auto\""));

    for name in ["alpha", "beta"] {
        repo_cmd(&env, &root.join(name))
            .env("TRX_CONFIG", &config_file)
            .env("TRX_STORE_ROOT", &store)
            .args(["list"])
            .assert()
            .success()
            .stdout(contains(format!("issue in {name}")));
    }
    // The junk ledger was neither migrated nor listed.
    repo_cmd(&env, &junk)
        .env("TRX_CONFIG", &config_file)
        .env("TRX_STORE_ROOT", &store)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("must be skipped"));

    // Everything migrated: a second dry run finds nothing.
    repo_cmd(&env, &root)
        .env("TRX_CONFIG", &config_file)
        .env("TRX_STORE_ROOT", &store)
        .args(["setup", "--dry-run", "--scan"])
        .arg(&root)
        .assert()
        .success()
        .stdout(contains("No repo-local ledgers found"));
}

#[test]
fn default_mode_central_serves_trx_less_checkouts_without_writing_to_them() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("repo");
    let store = base.path().join("store");
    let config_file = base.path().join("trx-config.toml");
    std::fs::write(
        &config_file,
        format!(
            "default_mode = \"central\"\nstore_root = \"{}\"\n",
            store.display()
        ),
    )
    .unwrap();
    init_git_repo(&repo);
    let nested = repo.join("src/deep");
    std::fs::create_dir_all(&nested).unwrap();

    // No `.trx` anywhere: commands work centrally and write NOTHING into the
    // checkout (mmry-style default).
    repo_cmd(&env, &nested)
        .env("TRX_CONFIG", &config_file)
        .args(["create", "born central"])
        .assert()
        .success();
    assert!(!repo.join(".trx").exists(), "checkout must stay untouched");
    repo_cmd(&env, &nested)
        .env("TRX_CONFIG", &config_file)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("born central"));

    // A worktree of the same repo hits the same central ledger.
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
    repo_cmd(&env, &wt)
        .env("TRX_CONFIG", &config_file)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("born central"));

    // `trx init` forces an explicit repo-local ledger, which then shadows the
    // central store for this checkout (empty, no marker).
    repo_cmd(&env, &repo)
        .env("TRX_CONFIG", &config_file)
        .args(["init"])
        .assert()
        .success();
    repo_cmd(&env, &repo)
        .env("TRX_CONFIG", &config_file)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("No issues found"));
    // …while the worktree still serves the central ledger.
    repo_cmd(&env, &wt)
        .env("TRX_CONFIG", &config_file)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("born central"));

    // Default (repo-local) mode is unchanged: without the knob, a fresh
    // checkout errors instead of silently going central.
    let fresh = base.path().join("fresh");
    init_git_repo(&fresh);
    repo_cmd(&env, &fresh)
        .args(["list"])
        .assert()
        .failure()
        .stderr(contains("not initialized"));
}

#[test]
fn onboard_bootstraps_store_policy_and_bulk_migrates_in_one_command() {
    let env = Env::new();
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("workspaces");
    let store = base.path().join("store");
    let remote = base.path().join("remote.git");
    let config_file = base.path().join("trx-config.toml");
    std::fs::write(&config_file, "# onboard test\n").unwrap();

    for name in ["one", "two"] {
        let repo = root.join(name);
        init_git_repo(&repo);
        repo_cmd(&env, &repo).args(["init"]).assert().success();
        repo_cmd(&env, &repo)
            .args(["create", &format!("pre-existing {name}")])
            .assert()
            .success();
    }
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);

    // One command on a fresh machine: bootstrap + policy + bulk migration.
    repo_cmd(&env, &root)
        .env("TRX_CONFIG", &config_file)
        .env("TRX_STORE_ROOT", &store)
        .args(["onboard", "--remote"])
        .arg(remote.join(".").to_str().unwrap())
        .args(["--scan"])
        .arg(&root)
        .args(["--exclude"])
        .arg(root.join("two"))
        .arg("--yes")
        .assert()
        .success()
        .stdout(contains("migrated 1 of 1 repo-local ledger(s)"));

    // Policy + config written.
    let config_text = std::fs::read_to_string(&config_file).unwrap();
    assert!(config_text.contains("migrate = \"auto\""));

    // `one` is central and readable; `two` (excluded) still serves locally.
    repo_cmd(&env, &root.join("one"))
        .env("TRX_CONFIG", &config_file)
        .env("TRX_STORE_ROOT", &store)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("pre-existing one"));
    repo_cmd(&env, &root.join("two"))
        .env("TRX_CONFIG", &config_file)
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("pre-existing two"));

    // Idempotent: second onboard reports nothing left to migrate.
    repo_cmd(&env, &root)
        .env("TRX_CONFIG", &config_file)
        .env("TRX_STORE_ROOT", &store)
        .args(["onboard", "--remote"])
        .arg(remote.join(".").to_str().unwrap())
        .args(["--scan"])
        .arg(&root)
        .args(["--exclude"])
        .arg(root.join("two"))
        .arg("--yes")
        .assert()
        .success()
        .stdout(contains("migrated 0 of 0 repo-local ledger(s)"));
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
