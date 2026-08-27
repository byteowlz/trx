//! Structured verification evidence and configurable closure gates.
//!
//! Verification runs are **immutable audit facts**: each run is a compact,
//! repository/tool-agnostic record of a single check (test suite run, E2E
//! pass, manual sign-off) attached to an issue. Records live in
//! `.trx/verifications.jsonl`, an append-only JSONL file that merges
//! conflict-free (distinct lines, no in-place edits) exactly like
//! `events.jsonl`.
//!
//! A later run supersedes an earlier run for policy evaluation (only the
//! latest-by-timestamp record is consulted) but never erases history.
//!
//! `VerificationConfig` is an opt-in closure policy. When absent (the
//! default), `close` behavior is unchanged. When enabled for an issue type,
//! `close` is gated on qualifying passing evidence, optionally pinned to the
//! current revision.
//!
//! Boundaries: trx never executes tests, captures artifacts, or validates
//! remote URLs. It stores metadata + references only.

use crate::{Error, Issue, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const VERIFICATIONS_FILE: &str = "verifications.jsonl";

/// Outcome of a single verification run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    #[default]
    Passed,
    Failed,
    Error,
    Skipped,
}

impl VerificationStatus {
    /// True only for a clean pass — the single status a closure gate accepts.
    pub fn is_pass(&self) -> bool {
        matches!(self, VerificationStatus::Passed)
    }
}

impl std::str::FromStr for VerificationStatus {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "passed" | "pass" | "ok" | "success" => Ok(VerificationStatus::Passed),
            "failed" | "fail" => Ok(VerificationStatus::Failed),
            "error" | "errored" => Ok(VerificationStatus::Error),
            "skipped" | "skip" | "ignored" => Ok(VerificationStatus::Skipped),
            _ => Err(Error::Other(format!(
                "invalid verification status: '{}' (expected passed, failed, error, or skipped)",
                s
            ))),
        }
    }
}

impl std::fmt::Display for VerificationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            VerificationStatus::Passed => "passed",
            VerificationStatus::Failed => "failed",
            VerificationStatus::Error => "error",
            VerificationStatus::Skipped => "skipped",
        };
        f.write_str(s)
    }
}

/// One named check within a verification run (e.g. a single scenario result).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationCheck {
    pub name: String,
    pub status: VerificationStatus,
    /// Free-form detail (assertion counts, failure reason, link, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// An immutable verification-run record.
///
/// Round-trips through `.trx/verifications.jsonl`. New optional fields must
/// use `#[serde(default)]`-style skips so old records keep loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRun {
    /// Stable run identifier (unique per issue). Idempotency key.
    pub run_id: String,

    /// The issue this run proves.
    pub issue_id: String,

    /// Overall outcome of the run.
    pub status: VerificationStatus,

    /// Subject revision, normally a Git commit SHA. Optional — but required
    /// when `require_current_revision` is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,

    /// When the run was recorded.
    pub timestamp: DateTime<Utc>,

    /// Environment / target, e.g. `local`, `ubuntu-worker`, `ci`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    /// Scenario / check-group identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,

    /// Command or producer identity that generated the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Concise human-readable summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Artifact references as URIs or paths. Opaque strings — never resolved
    /// or copied into `.trx`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,

    /// Structured per-check breakdown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<VerificationCheck>,

    /// Known gaps / caveats for this run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaps: Option<String>,
}

impl VerificationRun {
    /// Two runs are *equivalent* when every meaningful field matches, ignoring
    /// the recording timestamp. Used for idempotent re-submission: a CI retry
    /// that re-submits the same logical record is a no-op rather than a
    /// conflict.
    pub fn equivalent_to(&self, other: &Self) -> bool {
        self.run_id == other.run_id
            && self.issue_id == other.issue_id
            && self.status == other.status
            && self.revision == other.revision
            && self.environment == other.environment
            && self.scenario == other.scenario
            && self.command == other.command
            && self.summary == other.summary
            && self.artifacts == other.artifacts
            && self.checks == other.checks
            && self.gaps == other.gaps
    }
}

/// Opt-in closure-gate policy. Defaults are inert: no type is gated and no
/// requirement is enforced, so existing repositories behave unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VerificationConfig {
    /// Issue types (as lowercase strings: `bug`, `feature`, …) that must
    /// carry verification evidence before they can be closed. An empty list
    /// disables the policy entirely.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_for: Vec<String>,

    /// When true, the latest qualifying run must have status `passed`.
    pub require_pass: bool,

    /// When true, the latest qualifying run must target the current revision.
    pub require_current_revision: bool,
}

impl VerificationConfig {
    /// True when the policy imposes no requirements at all (either no types
    /// are listed, or neither requirement is enabled).
    pub fn is_inactive(&self) -> bool {
        self.require_for.is_empty() || (!self.require_pass && !self.require_current_revision)
    }

    /// True when this issue's type is subject to the policy.
    pub fn gates_issue(&self, issue: &Issue) -> bool {
        if self.is_inactive() {
            return false;
        }
        let type_str = issue.issue_type.to_string();
        self.require_for
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&type_str))
    }
}

/// Result of evaluating the closure gate for one issue.
#[derive(Debug, Clone, Default)]
pub struct CloseGateReport {
    /// True when closure may proceed.
    pub allowed: bool,
    /// Actionable, human-readable reasons when `allowed` is false.
    pub diagnostics: Vec<String>,
    /// The latest-by-timestamp run for the issue, if any.
    pub latest_run: Option<VerificationRun>,
}

impl CloseGateReport {
    fn allow(latest_run: Option<VerificationRun>) -> Self {
        Self {
            allowed: true,
            diagnostics: Vec::new(),
            latest_run,
        }
    }

    fn block(latest_run: Option<VerificationRun>, diagnostic: impl Into<String>) -> Self {
        let mut report = Self {
            allowed: false,
            diagnostics: Vec::new(),
            latest_run,
        };
        report.diagnostics.push(diagnostic.into());
        report
    }
}

/// Pick the latest run by timestamp. Ties favor the later element in `runs`
/// (i.e. the most recently appended record), matching "a later run supersedes
/// an earlier run".
pub fn latest_run<'a>(runs: &[&'a VerificationRun]) -> Option<&'a VerificationRun> {
    runs.iter().copied().max_by_key(|r| r.timestamp)
}

/// Evaluate the closure gate for `issue` against its verification `runs`.
///
/// `current_revision` is the revision the gate compares against when
/// `require_current_revision` is enabled; the CLI resolves this from Git HEAD
/// and passes `None` when it cannot be determined. Core never shells out.
pub fn evaluate_close_gate(
    issue: &Issue,
    runs: &[VerificationRun],
    config: &VerificationConfig,
    current_revision: Option<&str>,
) -> CloseGateReport {
    if !config.gates_issue(issue) {
        return CloseGateReport::allow(None);
    }

    let issue_runs: Vec<&VerificationRun> =
        runs.iter().filter(|r| r.issue_id == issue.id).collect();
    if issue_runs.is_empty() {
        return CloseGateReport::block(
            None,
            format!(
                "no verification evidence recorded for {} (policy requires verification for type '{}')",
                issue.id, issue.issue_type
            ),
        );
    }

    let latest = latest_run(&issue_runs).expect("non-empty");
    if config.require_pass && !latest.status.is_pass() {
        return CloseGateReport::block(
            Some(latest.clone()),
            format!(
                "latest verification {} for {} has status '{}'; policy requires a passing run",
                latest.run_id, issue.id, latest.status
            ),
        );
    }

    if config.require_current_revision {
        let Some(current) = current_revision else {
            return CloseGateReport::block(
                Some(latest.clone()),
                format!(
                    "require_current_revision is enabled but the current revision could not be \
                     determined (not a git repository?); record a run with --revision <current> \
                     for {} or close with --verification-override",
                    issue.id
                ),
            );
        };
        match &latest.revision {
            None => {
                return CloseGateReport::block(
                    Some(latest.clone()),
                    format!(
                        "latest verification {} for {} has no recorded revision; \
                         require_current_revision requires proof targeting revision '{}'",
                        latest.run_id, issue.id, current
                    ),
                );
            }
            Some(rev) if rev != current => {
                return CloseGateReport::block(
                    Some(latest.clone()),
                    format!(
                        "stale evidence: latest verification {} for {} targets revision '{}', \
                         current revision is '{}'",
                        latest.run_id, issue.id, rev, current
                    ),
                );
            }
            _ => {}
        }
    }

    CloseGateReport::allow(Some(latest.clone()))
}

/// Append-only JSONL store for verification runs, mirroring `EventLog`.
pub struct VerificationStore {
    path: PathBuf,
}

impl VerificationStore {
    /// Construct a store handle for the given `.trx/` directory. Does not
    /// create the file — `append` does that lazily.
    pub fn at(trx_dir: &Path) -> Self {
        Self {
            path: trx_dir.join(VERIFICATIONS_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a single run. Open-append-flush-fsync each call for durability
    /// matching issue/event saves.
    pub fn append(&self, run: &VerificationRun) -> Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(run)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    }

    /// Read all runs in file order. Malformed lines are skipped with a
    /// warning, so a partial-write tail cannot lock out the rest of the file.
    pub fn read_all(&self) -> Result<Vec<VerificationRun>> {
        let mut out = Vec::new();
        if !self.path.exists() {
            return Ok(out);
        }
        let f = File::open(&self.path)?;
        let reader = BufReader::new(f);
        for (lineno, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<VerificationRun>(&line) {
                Ok(run) => out.push(run),
                Err(e) => {
                    eprintln!(
                        "warning: skipping malformed verification at {}:{}: {}",
                        self.path.display(),
                        lineno + 1,
                        e
                    );
                }
            }
        }
        Ok(out)
    }

    /// All runs for one issue, in file (append) order.
    pub fn for_issue(&self, issue_id: &str) -> Result<Vec<VerificationRun>> {
        let mut runs: Vec<VerificationRun> = self
            .read_all()?
            .into_iter()
            .filter(|r| r.issue_id == issue_id)
            .collect();
        // Oldest first for display; ties keep append order (stable sort).
        runs.sort_by_key(|r| r.timestamp);
        Ok(runs)
    }

    /// Look up a single run by (issue_id, run_id).
    pub fn find(&self, issue_id: &str, run_id: &str) -> Result<Option<VerificationRun>> {
        Ok(self
            .read_all()?
            .into_iter()
            .find(|r| r.issue_id == issue_id && r.run_id == run_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Issue, IssueType};
    use tempfile::TempDir;

    fn run(id: &str, issue: &str, status: VerificationStatus) -> VerificationRun {
        VerificationRun {
            run_id: id.into(),
            issue_id: issue.into(),
            status,
            revision: None,
            timestamp: Utc::now(),
            environment: None,
            scenario: None,
            command: None,
            summary: None,
            artifacts: Vec::new(),
            checks: Vec::new(),
            gaps: None,
        }
    }

    fn bug(id: &str) -> Issue {
        let mut i = Issue::new(id.into(), "b".into());
        i.issue_type = IssueType::Bug;
        i
    }

    // --- status parsing / display ---

    #[test]
    fn status_roundtrips() {
        for s in [
            VerificationStatus::Passed,
            VerificationStatus::Failed,
            VerificationStatus::Error,
            VerificationStatus::Skipped,
        ] {
            let parsed: VerificationStatus = s.to_string().parse().unwrap();
            assert_eq!(s, parsed);
        }
    }

    #[test]
    fn status_accepts_aliases() {
        assert_eq!(
            "pass".parse::<VerificationStatus>().unwrap(),
            VerificationStatus::Passed
        );
        assert_eq!(
            "FAIL".parse::<VerificationStatus>().unwrap(),
            VerificationStatus::Failed
        );
    }

    #[test]
    fn status_rejects_unknown() {
        assert!("bogus".parse::<VerificationStatus>().is_err());
    }

    // --- serialization round-trip ---

    #[test]
    fn run_roundtrips_through_json() {
        let mut r = run("r1", "trx-1", VerificationStatus::Passed);
        r.revision = Some("abc1234".into());
        r.environment = Some("ci".into());
        r.scenario = Some("smoke".into());
        r.command = Some("just e2e".into());
        r.summary = Some("all green".into());
        r.artifacts = vec!["file://a".into(), "file://b".into()];
        r.checks = vec![VerificationCheck {
            name: "login".into(),
            status: VerificationStatus::Passed,
            detail: Some("12 steps".into()),
        }];
        r.gaps = Some("flaky on mac".into());

        let json = serde_json::to_string(&r).unwrap();
        let back: VerificationRun = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn run_with_only_required_fields_serializes_compactly() {
        let r = run("r1", "trx-1", VerificationStatus::Passed);
        let json = serde_json::to_string(&r).unwrap();
        // Optional fields are skipped, not emitted as nulls.
        assert!(!json.contains("revision"));
        assert!(!json.contains("artifacts"));
    }

    #[test]
    fn old_record_without_optional_fields_loads() {
        // Simulate a legacy/minimal record: only the required keys.
        let json = r#"{"run_id":"r1","issue_id":"trx-1","status":"passed","timestamp":"2026-01-01T00:00:00Z"}"#;
        let r: VerificationRun = serde_json::from_str(json).unwrap();
        assert_eq!(r.run_id, "r1");
        assert_eq!(r.status, VerificationStatus::Passed);
        assert!(r.artifacts.is_empty());
        assert!(r.checks.is_empty());
    }

    #[test]
    fn equivalent_to_ignores_timestamp() {
        let a = run("r1", "trx-1", VerificationStatus::Passed);
        let mut b = a.clone();
        b.timestamp = Utc::now() + chrono::Duration::seconds(60);
        assert!(a.equivalent_to(&b));
        b.status = VerificationStatus::Failed;
        assert!(!a.equivalent_to(&b));
    }

    // --- config ---

    #[test]
    fn default_config_is_inactive() {
        assert!(VerificationConfig::default().is_inactive());
    }

    #[test]
    fn config_inactive_when_no_requirements_enabled() {
        let cfg = VerificationConfig {
            require_for: vec!["bug".into()],
            require_pass: false,
            require_current_revision: false,
        };
        assert!(cfg.is_inactive());
    }

    #[test]
    fn gates_issue_matches_type_case_insensitively() {
        let cfg = VerificationConfig {
            require_for: vec!["Bug".into(), "FEATURE".into()],
            require_pass: true,
            require_current_revision: false,
        };
        assert!(cfg.gates_issue(&bug("trx-1")));
        let mut f = Issue::new("trx-2".into(), "f".into());
        f.issue_type = IssueType::Feature;
        assert!(cfg.gates_issue(&f));
        // Task is not listed.
        let t = Issue::new("trx-3".into(), "t".into());
        assert!(!cfg.gates_issue(&t));
    }

    // --- gate evaluation ---

    fn pass_config() -> VerificationConfig {
        VerificationConfig {
            require_for: vec!["bug".into()],
            require_pass: true,
            require_current_revision: false,
        }
    }

    #[test]
    fn gate_allows_when_policy_inactive() {
        let issue = bug("trx-1");
        let report = evaluate_close_gate(&issue, &[], &VerificationConfig::default(), None);
        assert!(report.allowed);
    }

    #[test]
    fn gate_allows_unlisted_type_without_evidence() {
        // Policy gates bugs; a task has no evidence but is not gated.
        let cfg = pass_config();
        let task = Issue::new("trx-1".into(), "t".into());
        let report = evaluate_close_gate(&task, &[], &cfg, None);
        assert!(report.allowed);
    }

    #[test]
    fn gate_blocks_without_evidence() {
        let report = evaluate_close_gate(&bug("trx-1"), &[], &pass_config(), None);
        assert!(!report.allowed);
        assert!(report.diagnostics[0].contains("no verification evidence"));
    }

    #[test]
    fn gate_blocks_on_failed_latest_run() {
        let issue = bug("trx-1");
        let mut passed = run("r1", "trx-1", VerificationStatus::Passed);
        passed.timestamp = Utc::now() - chrono::Duration::seconds(60);
        let failed = run("r2", "trx-1", VerificationStatus::Failed);
        let report = evaluate_close_gate(&issue, &[passed, failed], &pass_config(), None);
        assert!(!report.allowed);
        assert!(report.diagnostics[0].contains("status 'failed'"));
        assert_eq!(report.latest_run.unwrap().run_id, "r2");
    }

    #[test]
    fn gate_allows_when_latest_passes() {
        let issue = bug("trx-1");
        let mut failed = run("r1", "trx-1", VerificationStatus::Failed);
        failed.timestamp = Utc::now() - chrono::Duration::seconds(60);
        let passed = run("r2", "trx-1", VerificationStatus::Passed);
        let report = evaluate_close_gate(&issue, &[failed, passed], &pass_config(), None);
        assert!(report.allowed);
    }

    #[test]
    fn gate_blocks_on_stale_revision() {
        let issue = bug("trx-1");
        let mut r = run("r1", "trx-1", VerificationStatus::Passed);
        r.revision = Some("oldsha".into());
        let cfg = VerificationConfig {
            require_for: vec!["bug".into()],
            require_pass: true,
            require_current_revision: true,
        };
        let report = evaluate_close_gate(&issue, &[r], &cfg, Some("newsha"));
        assert!(!report.allowed);
        assert!(report.diagnostics[0].contains("stale evidence"));
    }

    #[test]
    fn gate_blocks_when_current_revision_unknown() {
        let issue = bug("trx-1");
        let r = run("r1", "trx-1", VerificationStatus::Passed);
        let cfg = VerificationConfig {
            require_for: vec!["bug".into()],
            require_pass: true,
            require_current_revision: true,
        };
        let report = evaluate_close_gate(&issue, &[r], &cfg, None);
        assert!(!report.allowed);
        assert!(report.diagnostics[0].contains("could not be determined"));
    }

    #[test]
    fn gate_blocks_when_run_has_no_revision_but_required() {
        let issue = bug("trx-1");
        let r = run("r1", "trx-1", VerificationStatus::Passed); // no revision
        let cfg = VerificationConfig {
            require_for: vec!["bug".into()],
            require_pass: true,
            require_current_revision: true,
        };
        let report = evaluate_close_gate(&issue, &[r], &cfg, Some("newsha"));
        assert!(!report.allowed);
        assert!(report.diagnostics[0].contains("no recorded revision"));
    }

    #[test]
    fn gate_allows_current_revision_match() {
        let issue = bug("trx-1");
        let mut r = run("r1", "trx-1", VerificationStatus::Passed);
        r.revision = Some("newsha".into());
        let cfg = VerificationConfig {
            require_for: vec!["bug".into()],
            require_pass: true,
            require_current_revision: true,
        };
        let report = evaluate_close_gate(&issue, &[r], &cfg, Some("newsha"));
        assert!(report.allowed);
    }

    // --- store ---

    #[test]
    fn store_append_then_read_round_trips() {
        let dir = TempDir::new().unwrap();
        let store = VerificationStore::at(dir.path());

        let r1 = run("r1", "trx-1", VerificationStatus::Passed);
        let r2 = run("r2", "trx-1", VerificationStatus::Failed);
        store.append(&r1).unwrap();
        store.append(&r2).unwrap();

        let all = store.read_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].run_id, "r1");
        assert_eq!(all[1].run_id, "r2");
    }

    #[test]
    fn store_skips_blank_and_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let store = VerificationStore::at(dir.path());
        store
            .append(&run("r1", "trx-1", VerificationStatus::Passed))
            .unwrap();

        let mut f = OpenOptions::new().append(true).open(store.path()).unwrap();
        f.write_all(b"\n").unwrap();
        f.write_all(b"{not json}\n").unwrap();

        let all = store.read_all().unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn store_for_issue_filters_and_sorts_oldest_first() {
        let dir = TempDir::new().unwrap();
        let store = VerificationStore::at(dir.path());

        let mut older = run("r1", "trx-1", VerificationStatus::Passed);
        older.timestamp = Utc::now() - chrono::Duration::seconds(60);
        let newer = run("r2", "trx-1", VerificationStatus::Passed);
        let other = run("rx", "trx-2", VerificationStatus::Passed);
        store.append(&newer).unwrap();
        store.append(&older).unwrap();
        store.append(&other).unwrap();

        let for_issue = store.for_issue("trx-1").unwrap();
        assert_eq!(for_issue.len(), 2);
        assert_eq!(for_issue[0].run_id, "r1"); // older first
        assert_eq!(for_issue[1].run_id, "r2");
    }

    #[test]
    fn store_find_returns_matching_run() {
        let dir = TempDir::new().unwrap();
        let store = VerificationStore::at(dir.path());
        store
            .append(&run("r1", "trx-1", VerificationStatus::Passed))
            .unwrap();

        assert!(store.find("trx-1", "r1").unwrap().is_some());
        assert!(store.find("trx-1", "missing").unwrap().is_none());
        assert!(store.find("trx-9", "r1").unwrap().is_none());
    }
}
