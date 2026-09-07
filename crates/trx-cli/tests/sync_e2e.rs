//! End-to-end tests for conflict-free `trx sync` behavior.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;

fn trx(repo: &Path) -> Command {
    let mut command = Command::cargo_bin("trx").expect("trx binary");
    command.current_dir(repo);
    command
}

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    StdCommand::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?} failed to start: {error}"))
}

fn git_ok(repo: &Path, args: &[&str]) -> String {
    let output = git(repo, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn configure_git(repo: &Path) {
    git_ok(repo, &["config", "user.email", "test@trx.local"]);
    git_ok(repo, &["config", "user.name", "trx test"]);
}

fn create_legacy_repo(repo: &Path) {
    git_ok(repo, &["init", "-b", "main", "--quiet"]);
    configure_git(repo);
    fs::create_dir(repo.join(".trx")).unwrap();
    fs::write(repo.join(".trx/config.toml"), "prefix = \"trx\"\n").unwrap();
    fs::write(repo.join(".trx/issues.jsonl"), "").unwrap();
    fs::write(repo.join(".trx/events.jsonl"), "").unwrap();
    fs::write(repo.join(".trx/verifications.jsonl"), "").unwrap();
    git_ok(repo, &["add", ".trx"]);
    git_ok(repo, &["commit", "--quiet", "-m", "legacy trx store"]);
}

fn create_issue(repo: &Path, title: &str) -> String {
    let output = trx(repo)
        .args(["create", title, "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "trx create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn sync_upgrades_legacy_repo_and_concurrent_appends_rebase_cleanly() {
    let temp = tempfile::tempdir().unwrap();
    let seed = temp.path().join("seed");
    let remote = temp.path().join("remote.git");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir(&seed).unwrap();
    create_legacy_repo(&seed);

    git_ok(
        temp.path(),
        &[
            "clone",
            "--bare",
            seed.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    git_ok(
        temp.path(),
        &["clone", remote.to_str().unwrap(), first.to_str().unwrap()],
    );
    git_ok(
        temp.path(),
        &["clone", remote.to_str().unwrap(), second.to_str().unwrap()],
    );
    configure_git(&first);
    configure_git(&second);

    fs::write(first.join("unrelated.txt"), "keep me unstaged\n").unwrap();
    let first_id = create_issue(&first, "first issue");
    trx(&first).arg("sync").assert().success();

    let attrs = fs::read_to_string(first.join(".gitattributes")).unwrap();
    assert!(attrs.contains(".trx/issues.jsonl text eol=lf merge=union"));
    assert!(attrs.contains(".trx/events.jsonl text eol=lf merge=union"));
    assert!(attrs.contains(".trx/verifications.jsonl text eol=lf merge=union"));
    assert_eq!(
        git_ok(&first, &["status", "--porcelain"]),
        "?? unrelated.txt"
    );
    git_ok(&first, &["push", "origin", "main"]);

    let second_id = create_issue(&second, "second issue");
    trx(&second).arg("sync").assert().success();
    git_ok(&second, &["fetch", "origin"]);
    git_ok(&second, &["rebase", "origin/main"]);

    let issues = fs::read_to_string(second.join(".trx/issues.jsonl")).unwrap();
    assert!(issues.contains("first issue"));
    assert!(issues.contains("second issue"));
    assert!(!issues.contains("<<<<<<<"));

    let events = fs::read_to_string(second.join(".trx/events.jsonl")).unwrap();
    assert!(events.contains(&first_id));
    assert!(events.contains(&second_id));
    assert!(!events.contains("<<<<<<<"));
}

#[test]
fn repeated_sync_does_not_duplicate_merge_attributes() {
    let temp = tempfile::tempdir().unwrap();
    create_legacy_repo(temp.path());

    trx(temp.path())
        .args(["sync", "--no-commit"])
        .assert()
        .success();
    trx(temp.path())
        .args(["sync", "--no-commit"])
        .assert()
        .success();

    let attrs = fs::read_to_string(temp.path().join(".gitattributes")).unwrap();
    for required in [
        ".trx/issues.jsonl text eol=lf merge=union",
        ".trx/events.jsonl text eol=lf merge=union",
        ".trx/verifications.jsonl text eol=lf merge=union",
    ] {
        assert_eq!(attrs.lines().filter(|line| *line == required).count(), 1);
    }
}

#[test]
fn sync_refuses_to_absorb_unrelated_gitattributes_edits() {
    let temp = tempfile::tempdir().unwrap();
    create_legacy_repo(temp.path());
    fs::write(temp.path().join(".gitattributes"), "*.png binary\n").unwrap();

    trx(temp.path())
        .args(["sync", "--no-commit"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            ".gitattributes has uncommitted changes",
        ));

    assert_eq!(
        fs::read_to_string(temp.path().join(".gitattributes")).unwrap(),
        "*.png binary\n"
    );
    assert!(git_ok(temp.path(), &["diff", "--cached", "--name-only"]).is_empty());
}
