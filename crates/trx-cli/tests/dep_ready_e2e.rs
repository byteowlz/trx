use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;

fn trx(repo: &Path, args: &[&str]) -> String {
    let output = Command::cargo_bin("trx")
        .unwrap()
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn repeated_blockers_are_accepted_and_ready_excludes_blocked_issues() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    trx(repo, &["init"]);
    let create = |title| {
        let output = trx(repo, &["--json", "create", title]);
        serde_json::from_str::<Value>(&output).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let blocked = create("blocked target");
    let a = create("blocker one");
    let b = create("blocker two");
    trx(repo, &["dep", "block", &blocked, "--by", &a, "--by", &b]);
    let issue: Value = serde_json::from_str(&trx(repo, &["--json", "show", &blocked])).unwrap();
    let deps = issue["dependencies"].as_array().unwrap();
    assert!(deps.iter().any(|d| d["depends_on_id"] == a));
    assert!(deps.iter().any(|d| d["depends_on_id"] == b));

    let ready = trx(repo, &["ready"]);
    assert!(!ready.contains(&blocked), "{ready}");
    assert!(ready.contains(&a));
    assert!(ready.contains(&b));
    let ready_json: Value = serde_json::from_str(&trx(repo, &["--json", "ready"])).unwrap();
    assert!(
        !ready_json
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == blocked)
    );
}
