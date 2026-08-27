//! End-to-end verification gate integration test (trx-rkrb).
//!
//! Drives the compiled `trx` binary against a throwaway Git + TRX repository
//! and asserts every acceptance criterion: the opt-in closure gate blocks
//! without/with-failed/stale evidence, admits current passing evidence,
//! honors an auditable override, and never copies referenced artifacts into
//! `.trx`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use serde_json::Value;

/// Build an `assert_cmd` Command rooted at the temp repo.
fn trx(repo: &Path) -> Command {
    let mut c = Command::cargo_bin("trx").expect("trx binary");
    c.current_dir(repo);
    c
}

/// Run a git command in `repo`, asserting success; return trimmed stdout.
fn git(repo: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|e| panic!("git {:?}: {e}", args));
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Initialize a git repo with two commits; return (older_sha, current_sha).
fn git_repo_with_two_commits(dir: &Path) -> (String, String) {
    git(dir, &["init", "-b", "main", "--quiet"]);
    git(dir, &["config", "user.email", "test@trx.local"]);
    git(dir, &["config", "user.name", "trx test"]);

    fs::write(dir.join("a.txt"), "first\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", "first"]);
    let older = git(dir, &["rev-parse", "HEAD"]);

    fs::write(dir.join("b.txt"), "second\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", "second"]);
    let current = git(dir, &["rev-parse", "HEAD"]);

    (older, current)
}

/// Enable the verification closure gate for `bug` via config.toml.
fn enable_gate(repo: &Path) {
    fs::write(
        repo.join(".trx/config.toml"),
        r#"prefix = "trx"

[verification]
require_for = ["bug"]
require_pass = true
require_current_revision = true
"#,
    )
    .unwrap();
}

/// Parse JSON from a command's stdout.
fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "failed to parse JSON stdout: {e}\nstdout was: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Create a bug issue and return its id (parsed from --json output).
fn create_bug(repo: &Path, title: &str) -> String {
    let out = trx(repo)
        .args(["create", title, "-t", "bug", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "create failed");
    json_stdout(&out)["id"].as_str().unwrap().to_string()
}

/// Add a verification run; panics if it fails.
fn verify_add(repo: &Path, issue: &str, args: &[&str]) {
    let mut full = vec!["verify", "add", issue];
    full.extend_from_slice(args);
    let out = trx(repo).args(&full).output().unwrap();
    assert!(
        out.status.success(),
        "verify add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// True when `trx close` exits non-zero.
fn close_fails(repo: &Path, issue: &str) -> (bool, String) {
    let out = trx(repo).args(["close", issue]).output().unwrap();
    (
        !out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn close_ok(repo: &Path, issue: &str) -> bool {
    trx(repo)
        .args(["close", issue])
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn close_gate_blocks_then_admits_evidence_end_to_end() {
    let repo = tempfile::tempdir().unwrap();
    let (_older, current) = git_repo_with_two_commits(repo.path());

    // 1. create a bug
    trx(repo.path())
        .args(["init", "--prefix", "trx"])
        .assert()
        .success();
    let issue = create_bug(repo.path(), "gate e2e bug");

    // 2. enable verification policy
    enable_gate(repo.path());

    // 3. close fails without evidence
    let (failed, stderr) = close_fails(repo.path(), &issue);
    assert!(failed, "close should fail without evidence");
    assert!(stderr.contains("verification"), "stderr: {stderr}");

    // 4. add a failed run and prove close still fails
    verify_add(
        repo.path(),
        &issue,
        &[
            "--run-id",
            "r1",
            "--status",
            "failed",
            "--revision",
            &current,
        ],
    );
    let (failed, stderr) = close_fails(repo.path(), &issue);
    assert!(failed, "close should fail when latest run failed");
    assert!(stderr.contains("failed"), "stderr: {stderr}");

    // 5. add a passing run for an older commit and prove stale evidence fails.
    //    Re-create the repo so we have a real older HEAD to target.
    //    (We do this in a fresh repo to keep the assertion crisp.)
    verify_stale_revision_blocks();

    // 6. add a passing run for the current revision and prove close succeeds
    verify_add(
        repo.path(),
        &issue,
        &[
            "--run-id",
            "r3",
            "--status",
            "passed",
            "--revision",
            &current,
        ],
    );
    assert!(
        close_ok(repo.path(), &issue),
        "close should succeed with passing current-revision evidence"
    );
}

/// Standalone check for acceptance step 5 (stale revision), in its own repo so
/// the "older" revision is unambiguous.
fn verify_stale_revision_blocks() {
    let repo = tempfile::tempdir().unwrap();
    let (older, current) = git_repo_with_two_commits(repo.path());
    trx(repo.path())
        .args(["init", "--prefix", "trx"])
        .assert()
        .success();
    let issue = create_bug(repo.path(), "stale bug");
    enable_gate(repo.path());

    // Passing run targeting the OLDER commit while HEAD is `current`.
    verify_add(
        repo.path(),
        &issue,
        &["--run-id", "s1", "--status", "passed", "--revision", &older],
    );
    let (failed, stderr) = close_fails(repo.path(), &issue);
    assert!(failed, "close should fail on stale evidence");
    assert!(stderr.contains("stale"), "stderr: {stderr}");
    assert!(
        stderr.contains(&older[..8]),
        "stderr should name stale revision: {stderr}"
    );
    assert!(
        stderr.contains(&current[..8]),
        "stderr should name current revision: {stderr}"
    );
}

#[test]
fn override_closes_and_is_auditable() {
    let repo = tempfile::tempdir().unwrap();
    git_repo_with_two_commits(repo.path());
    trx(repo.path())
        .args(["init", "--prefix", "trx"])
        .assert()
        .success();
    let issue = create_bug(repo.path(), "override bug");
    enable_gate(repo.path());

    // No evidence at all — close fails without override.
    assert!(close_fails(repo.path(), &issue).0);

    // Override closes it.
    let out = trx(repo.path())
        .args([
            "close",
            &issue,
            "--verification-override",
            "Accepted documentation-only follow-up by Tommy",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "override close should succeed");

    // The override reason is visible in history (JSON).
    let hist = trx(repo.path())
        .args(["history", &issue, "--json"])
        .output()
        .unwrap();
    let events: Vec<Value> = json_stdout(&hist).as_array().cloned().unwrap_or_default();
    let closed = events
        .iter()
        .find(|e| e["action"].as_str() == Some("closed"))
        .expect("a closed event");
    let note = closed["note"].as_str().unwrap_or("");
    assert!(
        note.contains("verification override"),
        "override note missing in history: {note}"
    );
    assert!(
        note.contains("documentation-only follow-up"),
        "override reason text missing in history: {note}"
    );

    // And visible in show (JSON carries the close event via recent activity is
    // not asserted here; history is the authoritative audit surface).
    let show = trx(repo.path())
        .args(["show", &issue, "--json"])
        .output()
        .unwrap();
    assert_eq!(
        json_stdout(&show)["status"].as_str(),
        Some("closed"),
        "issue should be closed"
    );
}

#[test]
fn referenced_artifacts_are_not_copied_into_trx() {
    let repo = tempfile::tempdir().unwrap();
    let (_older, current) = git_repo_with_two_commits(repo.path());
    trx(repo.path())
        .args(["init", "--prefix", "trx"])
        .assert()
        .success();
    let issue = create_bug(repo.path(), "artifact bug");

    // Create a real artifact file outside .trx with a unique sentinel.
    let sentinel = "TRX_SENTINEL_DO_NOT_COPY_9f3a1c";
    let artifact_path = repo.path().join("artifacts").join("report.txt");
    fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    fs::write(&artifact_path, sentinel).unwrap();

    verify_add(
        repo.path(),
        &issue,
        &[
            "--run-id",
            "a1",
            "--status",
            "passed",
            "--revision",
            &current,
            "--artifact",
            &format!("file://{}", artifact_path.display()),
        ],
    );

    // The reference string must be stored as a reference (present in JSON).
    let listed = trx(repo.path())
        .args(["verify", "list", &issue, "--json"])
        .output()
        .unwrap();
    let runs: Vec<Value> = json_stdout(&listed).as_array().cloned().unwrap_or_default();
    assert_eq!(runs.len(), 1);
    let arts = runs[0]["artifacts"].as_array().unwrap();
    assert_eq!(arts.len(), 1);
    assert!(arts[0].as_str().unwrap().ends_with("report.txt"));

    // No file under .trx may contain the sentinel content.
    let mut hits: Vec<PathBuf> = Vec::new();
    walk(repo.path().join(".trx"), &mut |path| {
        if let Ok(content) = fs::read_to_string(path)
            && content.contains(sentinel)
        {
            hits.push(path.to_path_buf());
        }
    });
    assert!(
        hits.is_empty(),
        "artifact content was copied into .trx: {hits:?}"
    );
}

fn walk(dir: PathBuf, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(path, f);
        } else {
            f(&path);
        }
    }
}

#[test]
fn verify_add_list_show_json_and_idempotency() {
    let repo = tempfile::tempdir().unwrap();
    trx(repo.path())
        .args(["init", "--prefix", "trx"])
        .assert()
        .success();
    let issue = create_bug(repo.path(), "cli surface bug");

    // add (human mode still returns success)
    verify_add(
        repo.path(),
        &issue,
        &[
            "--run-id",
            "k1",
            "--status",
            "passed",
            "--summary",
            "all green",
            "--check",
            "login=passed:12 steps",
            "--check",
            "search=failed",
        ],
    );

    // list --json
    let listed = trx(repo.path())
        .args(["verify", "list", &issue, "--json"])
        .output()
        .unwrap();
    let runs: Vec<Value> = json_stdout(&listed).as_array().cloned().unwrap_or_default();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["run_id"].as_str(), Some("k1"));
    assert_eq!(runs[0]["status"].as_str(), Some("passed"));
    assert_eq!(runs[0]["checks"].as_array().unwrap().len(), 2);

    // show --json
    let shown = trx(repo.path())
        .args(["verify", "show", &issue, "k1", "--json"])
        .output()
        .unwrap();
    assert_eq!(json_stdout(&shown)["run_id"].as_str(), Some("k1"));

    // Idempotent re-submission: same record (incl. checks) → no second line.
    verify_add(
        repo.path(),
        &issue,
        &[
            "--run-id",
            "k1",
            "--status",
            "passed",
            "--summary",
            "all green",
            "--check",
            "login=passed:12 steps",
            "--check",
            "search=failed",
        ],
    );
    let listed = trx(repo.path())
        .args(["verify", "list", &issue, "--json"])
        .output()
        .unwrap();
    let runs: Vec<Value> = json_stdout(&listed).as_array().cloned().unwrap_or_default();
    assert_eq!(runs.len(), 1, "idempotent re-add must not duplicate");

    // Conflicting re-submission: same run-id, different content → error.
    let out = trx(repo.path())
        .args([
            "verify", "add", &issue, "--run-id", "k1", "--status", "failed",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "conflicting run-id should be rejected"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("different content"),
        "stderr should explain the conflict: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn verify_add_validates_inputs() {
    let repo = tempfile::tempdir().unwrap();
    trx(repo.path())
        .args(["init", "--prefix", "trx"])
        .assert()
        .success();
    let issue = create_bug(repo.path(), "validation bug");

    // Unknown issue → error, no write.
    let out = trx(repo.path())
        .args([
            "verify", "add", "trx-nope", "--run-id", "x1", "--status", "passed",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());

    // Missing status → error.
    let out = trx(repo.path())
        .args(["verify", "add", &issue, "--run-id", "x2"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--status"));

    // Bogus status → error.
    let out = trx(repo.path())
        .args([
            "verify", "add", &issue, "--run-id", "x3", "--status", "bogus",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());

    // Malformed --check → error.
    let out = trx(repo.path())
        .args([
            "verify",
            "add",
            &issue,
            "--run-id",
            "x4",
            "--status",
            "passed",
            "--check",
            "no_equals_here",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());

    // None of the failed adds wrote a record.
    let listed = trx(repo.path())
        .args(["verify", "list", &issue, "--json"])
        .output()
        .unwrap();
    let runs: Vec<Value> = json_stdout(&listed).as_array().cloned().unwrap_or_default();
    assert!(
        runs.is_empty(),
        "no records should exist after only failed adds"
    );
}

#[test]
fn close_gate_inactive_by_default_allows_closing_without_evidence() {
    // No [verification] config → existing close behavior unchanged.
    let repo = tempfile::tempdir().unwrap();
    git_repo_with_two_commits(repo.path());
    trx(repo.path())
        .args(["init", "--prefix", "trx"])
        .assert()
        .success();
    let issue = create_bug(repo.path(), "ungated bug");
    // No evidence, no config — close must succeed.
    assert!(close_ok(repo.path(), &issue));
}
