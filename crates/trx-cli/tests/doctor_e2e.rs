//! End-to-end tests for `trx doctor` repository health checks.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

fn trx(repo: &Path) -> Command {
    let mut c = Command::cargo_bin("trx").expect("trx binary");
    c.current_dir(repo);
    c
}

fn init_trx_repo(repo: &Path) {
    trx(repo)
        .args(["init", "--prefix", "trx"])
        .assert()
        .success();
}

#[test]
fn doctor_warns_when_gitattributes_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    init_trx_repo(temp.path());
    fs::remove_file(temp.path().join(".gitattributes")).unwrap();

    trx(temp.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "warning: .gitattributes is missing",
        ))
        .stderr(predicate::str::contains("trx doctor found problems"));
}

#[test]
fn doctor_fix_installs_missing_gitattributes_entries() {
    let temp = tempfile::tempdir().unwrap();
    init_trx_repo(temp.path());
    fs::remove_file(temp.path().join(".gitattributes")).unwrap();

    trx(temp.path())
        .args(["doctor", "--fix"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "installed 2 missing trx merge attribute entries",
        ));

    let attrs = fs::read_to_string(temp.path().join(".gitattributes")).unwrap();
    assert!(attrs.contains(".trx/issues.jsonl text eol=lf merge=union"));
    assert!(attrs.contains(".trx/events.jsonl text eol=lf merge=union"));

    trx(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ".gitattributes configures trx JSONL union merges",
        ));
}
