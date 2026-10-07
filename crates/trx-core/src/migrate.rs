//! Migration of repo-local `.trx/` ledgers into the central store.
//!
//! Merge is by id over the append-only files: issue snapshots concatenate
//! (state resolves by last-write-wins on load, exactly like a union merge),
//! events and verification runs dedupe by their stable ids. A conflict
//! (same id, different content, no newer side) aborts with nothing moved.
//! Only after the central ledger verifies complete are the local files
//! renamed to `*.migrated-<timestamp>` backups and the `.trx/central`
//! marker written — restoring the backups restores the pre-migration state.

use crate::central::{self, CentralStore, Checkout};
use crate::store::{LEGACY_CRDT_DIR, LOCK_FILE, StoreLock};
use crate::{Error, Issue, Result, legacy_crdt};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ISSUES_FILE: &str = "issues.jsonl";
const EVENTS_FILE: &str = "events.jsonl";
const VERIFICATIONS_FILE: &str = "verifications.jsonl";
const MIGRATED_NOTE: &str = "MIGRATED";

/// Options for [`migrate_repo`].
#[derive(Debug, Clone, Copy, Default)]
pub struct MigrateOptions {
    /// Report the plan without writing anything.
    pub dry_run: bool,
    /// Remove migrated ledger files from the git index (`git rm --cached`);
    /// the resulting change is left uncommitted.
    pub untrack: bool,
}

/// What a migration did (or would do, under `dry_run`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrateReport {
    pub repo_root: String,
    pub identity: String,
    /// Central ledger directory.
    pub ledger: String,
    pub dry_run: bool,
    /// Issue snapshot lines merged from the local ledger.
    pub issue_snapshots: usize,
    /// Distinct issues in the local ledger.
    pub issues: usize,
    /// Dependency edges in the local final issue states.
    pub dependencies: usize,
    /// Event lines merged (deduped by event id).
    pub events: usize,
    /// Verification runs merged (deduped by run id).
    pub verifications: usize,
    /// Local backup files created (`*.migrated-<timestamp>`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub backups: Vec<String>,
    /// Files removed from the git index (only with `untrack`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub untracked: Vec<String>,
    /// True when the checkout was already central with no leftover ledger.
    pub already_central: bool,
}

/// The `.trx` ledger of one checkout: raw append-only lines per file.
struct LocalLedger {
    issue_lines: Vec<String>,
    events_lines: Vec<String>,
    verifications_lines: Vec<String>,
}

impl LocalLedger {
    fn read(trx_dir: &Path) -> Result<Self> {
        Ok(Self {
            issue_lines: read_normalized(&trx_dir.join(ISSUES_FILE))?,
            events_lines: read_normalized(&trx_dir.join(EVENTS_FILE))?,
            verifications_lines: read_normalized(&trx_dir.join(VERIFICATIONS_FILE))?,
        })
    }

    fn is_empty(&self) -> bool {
        self.issue_lines.is_empty()
            && self.events_lines.is_empty()
            && self.verifications_lines.is_empty()
    }
}

fn read_lines(path: &Path) -> Result<Vec<String>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    Ok(fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
}

/// Read JSONL, splitting crash artifacts where two valid JSON objects ended
/// up concatenated on one line (interrupted concurrent appends). Every
/// object survives; genuinely unparsable lines still abort the migration.
fn read_normalized(path: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for line in read_lines(path)? {
        if serde_json::from_str::<serde_json::Value>(&line).is_ok() {
            out.push(line);
            continue;
        }
        match split_concatenated(&line) {
            Some(parts) => out.extend(parts),
            None => {
                return Err(Error::Other(format!(
                    "unparsable JSONL line in {}: {}",
                    path.display(),
                    line
                )));
            }
        }
    }
    Ok(out)
}

/// Split one line into consecutive valid JSON documents; `None` when any
/// part fails to parse or nothing splits.
fn split_concatenated(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    let mut parts = Vec::new();
    let mut idx = 0usize;
    while idx < trimmed.len() {
        let mut stream =
            serde_json::Deserializer::from_str(&trimmed[idx..]).into_iter::<serde_json::Value>();
        let value = stream.next()?.ok()?;
        let end = stream.byte_offset();
        parts.push(serde_json::to_string(&value).ok()?);
        idx += end;
        while idx < trimmed.len() && trimmed.as_bytes()[idx].is_ascii_whitespace() {
            idx += 1;
        }
    }
    (parts.len() > 1).then_some(parts)
}

fn append_lines(path: &Path, lines: &[String]) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for line in lines {
        writeln!(file, "{line}")?;
    }
    file.sync_all()?;
    Ok(())
}

/// Final state per issue id: last-write-wins by `updated_at`, ties resolved
/// by file order (same rule as `Store::load`).
fn resolve_issues(lines: &[String]) -> Result<BTreeMap<String, Issue>> {
    let mut map: BTreeMap<String, Issue> = BTreeMap::new();
    for line in lines {
        let issue: Issue = serde_json::from_str(line)
            .map_err(|error| Error::Other(format!("unparsable issue snapshot: {error}")))?;
        let replace = map
            .get(&issue.id)
            .map(|current| issue.updated_at >= current.updated_at)
            .unwrap_or(true);
        if replace {
            map.insert(issue.id.clone(), issue);
        }
    }
    Ok(map)
}

/// Stable dedupe/conflict key per line: the record's id field.
fn key_of(line: &str, id_field: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|error| Error::Other(format!("unparsable JSONL line: {error}")))?;
    Ok(value
        .get(id_field)
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| line.to_string(), str::to_string))
}

fn key_map(lines: &[String], id_field: &str) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for line in lines {
        let key = key_of(line, id_field)?;
        if let Some(existing) = map.get(&key) {
            if existing != line {
                return Err(Error::Other(format!(
                    "conflicting local records for {id_field} {key}: migration aborted, nothing moved"
                )));
            }
        } else {
            map.insert(key, line.clone());
        }
    }
    Ok(map)
}

/// Migrate the repo-local `.trx` ledger of `repo_root` into the central store.
pub fn migrate_repo(
    repo_root: &Path,
    resolved_store_root: &Path,
    store_name: Option<&str>,
    opts: MigrateOptions,
) -> Result<MigrateReport> {
    let trx_dir = repo_root.join(".trx");
    if !trx_dir.is_dir() {
        return Err(Error::NotInitialized);
    }
    // Legacy v2 automerge layout: convert to JSONL exactly like Store's own
    // transparent migration (load crdt → append snapshots → drop crdt dir),
    // then migrate normally. Dry runs only count, never write.
    let crdt_dir = trx_dir.join(LEGACY_CRDT_DIR);
    let mut crdt_issue_count = 0usize;
    if crdt_dir.exists() {
        let issues = legacy_crdt::load_issues(&crdt_dir)?;
        crdt_issue_count = issues.len();
        if !opts.dry_run {
            let lines: Vec<String> = issues
                .iter()
                .map(|issue| serde_json::to_string(issue).map_err(Error::Json))
                .collect::<Result<Vec<_>>>()?;
            append_lines(&trx_dir.join(ISSUES_FILE), &lines)?;
            fs::remove_dir_all(&crdt_dir)?;
        }
    }

    let marker = central::read_marker(repo_root)?;
    let ledger = LocalLedger::read(&trx_dir)?;
    if marker.is_some() && ledger.is_empty() {
        // Already migrated; a re-run is a no-op.
        let cs = CentralStore::open_at(resolved_store_root.to_path_buf());
        let checkout = Checkout::at(repo_root)?;
        let repo = cs.plan(&checkout)?;
        return Ok(MigrateReport {
            repo_root: repo_root.display().to_string(),
            identity: checkout.identity,
            ledger: repo.dir.display().to_string(),
            dry_run: opts.dry_run,
            issue_snapshots: 0,
            issues: 0,
            dependencies: 0,
            events: 0,
            verifications: 0,
            backups: Vec::new(),
            untracked: Vec::new(),
            already_central: true,
        });
    }

    // Resolve the central side (plan only for dry runs — nothing registered).
    let cs = CentralStore::open_at(resolved_store_root.to_path_buf());
    let checkout = Checkout::at(repo_root)?;
    let central_repo = if opts.dry_run {
        cs.plan(&checkout)?
    } else {
        cs.register(&checkout)?
    };
    let central_issues = resolve_issues(&read_lines(&central_repo.issues_path())?)?;
    let central_events = key_map(&read_lines(&central_repo.events_path())?, "id")?;
    let central_verifications =
        key_map(&read_lines(&central_repo.verifications_path())?, "run_id")?;

    // Local resolution + conflict detection against the central side.
    let local_issues = resolve_issues(&ledger.issue_lines)?;
    let mut conflicts = Vec::new();
    for (id, local_final) in &local_issues {
        if let Some(central_final) = central_issues.get(id) {
            let divergent = central_final.updated_at == local_final.updated_at
                && serde_json::to_string(central_final).unwrap_or_default()
                    != serde_json::to_string(local_final).unwrap_or_default();
            if divergent {
                conflicts.push(id.clone());
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(Error::Other(format!(
            "conflicting issue states for {} (same timestamp, divergent content): migration aborted, nothing moved",
            conflicts.join(", ")
        )));
    }
    let local_events = key_map(&ledger.events_lines, "id")?;
    for (key, line) in &local_events {
        if let Some(existing) = central_events.get(key)
            && existing != line
        {
            return Err(Error::Other(format!(
                "conflicting event records for {key}: migration aborted, nothing moved"
            )));
        }
    }
    let local_verifications = key_map(&ledger.verifications_lines, "run_id")?;
    for (key, line) in &local_verifications {
        if let Some(existing) = central_verifications.get(key)
            && existing != line
        {
            return Err(Error::Other(format!(
                "conflicting verification records for {key}: migration aborted, nothing moved"
            )));
        }
    }

    let dependencies: usize = local_issues
        .values()
        .map(|issue| issue.dependencies.len())
        .sum();

    let report_base = MigrateReport {
        repo_root: repo_root.display().to_string(),
        identity: checkout.identity.clone(),
        ledger: central_repo.dir.display().to_string(),
        dry_run: opts.dry_run,
        issue_snapshots: ledger.issue_lines.len() + crdt_issue_count,
        issues: local_issues.len(),
        dependencies,
        events: local_events.len(),
        verifications: local_verifications.len(),
        backups: Vec::new(),
        untracked: Vec::new(),
        already_central: false,
    };
    if opts.dry_run {
        return Ok(report_base);
    }

    // Merge: append local snapshot lines (full history) and the records the
    // central side is missing. Identical full lines are skipped. Merging and
    // removing a duplicate directory of the same identity happens under its
    // exclusive lock (register -> merge_duplicates already enforces that); a
    // migration into the primary ledger takes the primary's lock for the same
    // reason.
    let _lock = StoreLock::try_acquire(central_repo.lock_path())?;
    let central_issue_lines_raw = read_lines(&central_repo.issues_path())?;
    let central_issue_lines: BTreeSet<String> = central_issue_lines_raw.into_iter().collect();
    let new_issue_lines: Vec<String> = ledger
        .issue_lines
        .iter()
        .filter(|line| !central_issue_lines.contains(*line))
        .cloned()
        .collect();
    let central_event_keys: BTreeSet<String> = central_events.keys().cloned().collect();
    let new_event_lines = new_lines_by_key(&local_events, &central_event_keys);
    let central_verification_keys: BTreeSet<String> =
        central_verifications.keys().cloned().collect();
    let new_verification_lines = new_lines_by_key(&local_verifications, &central_verification_keys);

    append_lines(&central_repo.issues_path(), &new_issue_lines)?;
    append_lines(&central_repo.events_path(), &new_event_lines)?;
    append_lines(&central_repo.verifications_path(), &new_verification_lines)?;

    // Verify the central ledger now contains everything local had.
    let merged_issues = resolve_issues(&read_lines(&central_repo.issues_path())?)?;
    for (id, local_final) in &local_issues {
        let Some(central_final) = merged_issues.get(id) else {
            return Err(Error::Other(format!(
                "verification failed: issue {id} missing after merge (central ledger left complete, local files untouched)"
            )));
        };
        let central_newer = central_final.updated_at > local_final.updated_at;
        if !central_newer
            && serde_json::to_string(central_final).unwrap_or_default()
                != serde_json::to_string(local_final).unwrap_or_default()
        {
            return Err(Error::Other(format!(
                "verification failed: issue {id} diverges after merge (central ledger left complete, local files untouched)"
            )));
        }
    }
    let merged_events = key_map(&read_lines(&central_repo.events_path())?, "id")?;
    for key in local_events.keys() {
        if !merged_events.contains_key(key) {
            return Err(Error::Other(format!(
                "verification failed: event {key} missing after merge"
            )));
        }
    }
    let merged_verifications = key_map(&read_lines(&central_repo.verifications_path())?, "run_id")?;
    for key in local_verifications.keys() {
        if !merged_verifications.contains_key(key) {
            return Err(Error::Other(format!(
                "verification failed: verification {key} missing after merge"
            )));
        }
    }

    // Success: back up the local files, note the migration, opt the checkout in.
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut backups = Vec::new();
    for (name, lines) in [
        (ISSUES_FILE, !ledger.issue_lines.is_empty()),
        (EVENTS_FILE, !ledger.events_lines.is_empty()),
        (VERIFICATIONS_FILE, !ledger.verifications_lines.is_empty()),
    ] {
        let path = trx_dir.join(name);
        if !lines || !path.is_file() {
            continue;
        }
        let backup = path.with_extension(format!("jsonl.migrated-{stamp}"));
        fs::rename(&path, &backup)?;
        backups.push(backup.display().to_string());
    }
    if opts.untrack {
        for name in [ISSUES_FILE, EVENTS_FILE, VERIFICATIONS_FILE] {
            let rel = format!(".trx/{name}");
            if is_git_tracked(repo_root, &rel) {
                git_rm_cached(repo_root, &rel)?;
            }
        }
    }
    let note = format!(
        "# Migrated to the central store at {} ({}) on {}.\n\
         # Backups: *.migrated-{stamp}. Restore them and remove .trx/central to roll back.\n",
        central_repo.dir.display(),
        checkout.identity,
        chrono::Utc::now().to_rfc3339(),
    );
    fs::write(trx_dir.join(MIGRATED_NOTE), note)?;
    central::write_marker(repo_root, store_name, Some(resolved_store_root))?;

    Ok(MigrateReport {
        backups,
        ..report_base
    })
}

fn new_lines_by_key(
    local: &BTreeMap<String, String>,
    central_keys: &BTreeSet<String>,
) -> Vec<String> {
    local
        .iter()
        .filter(|(key, _)| !central_keys.contains(*key))
        .map(|(_, line)| line.clone())
        .collect()
}

fn is_git_tracked(repo_root: &Path, rel: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "--error-unmatch", "--", rel])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git_rm_cached(repo_root: &Path, rel: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rm", "--cached", "--quiet", "--"])
        .arg(rel)
        .output()
        .map_err(|error| Error::Other(format!("git rm --cached: {error}")))?;
    if !output.status.success() {
        return Err(Error::Other(format!(
            "git rm --cached {} failed: {}",
            rel,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Find repos with a repo-local `.trx/issues.jsonl` under `roots` (bounded
/// depth, skipping dependency/build/cache dirs and other repositories'
/// internals).
pub fn scan_for_ledgers(roots: &[PathBuf], max_depth: u32) -> Result<Vec<PathBuf>> {
    const SKIP_DIRS: [&str; 11] = [
        "node_modules",
        "target",
        "dist",
        "build",
        "vendor",
        ".venv",
        "venv",
        "__pycache__",
        ".cache",
        ".git",
        ".trx",
    ];
    let mut found = BTreeSet::new();
    for root in roots {
        let root = if let Some(expanded) = root.to_str().map(crate::paths::expand_tilde) {
            expanded?
        } else {
            root.clone()
        };
        if !root.is_dir() {
            continue;
        }
        let mut stack = vec![(root.clone(), 0u32)];
        while let Some((dir, depth)) = stack.pop() {
            if dir.join(".trx").join(ISSUES_FILE).is_file()
                && !dir.join(".trx").join("central").exists()
            {
                found.insert(dir.clone());
            }
            if depth >= max_depth {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if SKIP_DIRS.contains(&name.as_ref()) || name.starts_with(".tmp") {
                    continue;
                }
                stack.push((path, depth + 1));
            }
        }
    }
    Ok(found.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::central;
    use std::process::Stdio;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
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

    fn init_git_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q", "-b", "main"]);
        fs::write(path.join("file.txt"), "hello\n").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-q", "-m", "initial"]);
    }

    fn issue_snapshot(id: &str, title: &str, offset_secs: i64) -> String {
        let mut issue = Issue::new(id.into(), title.into());
        issue.updated_at = chrono::Utc::now() + chrono::Duration::seconds(offset_secs);
        serde_json::to_string(&issue).unwrap()
    }

    fn seed_local(repo: &Path, issues: &[String], events: &[String], verifications: &[String]) {
        let trx_dir = repo.join(".trx");
        fs::create_dir_all(&trx_dir).unwrap();
        fs::write(
            trx_dir.join(CONFIG),
            "# trx configuration\nprefix = \"app\"\n",
        )
        .unwrap();
        fs::write(trx_dir.join(ISSUES_FILE), lines_text(issues)).unwrap();
        if !events.is_empty() {
            fs::write(trx_dir.join(EVENTS_FILE), lines_text(events)).unwrap();
        }
        if !verifications.is_empty() {
            fs::write(trx_dir.join(VERIFICATIONS_FILE), lines_text(verifications)).unwrap();
        }
    }

    const CONFIG: &str = "config.toml";

    fn lines_text(lines: &[String]) -> String {
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    #[test]
    fn test_migrate_preserves_issues_events_verifications_and_deps() {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        let store_root = base.path().join("store");
        init_git_repo(&repo);
        let event = format!(
            "{}",
            serde_json::json!({"id": "ev-1", "issue_id": "app-1", "action": "created", "timestamp": "2026-01-01T00:00:00Z"})
        );
        let verification = format!(
            "{}",
            serde_json::json!({"run_id": "run-1", "issue_id": "app-1", "status": "pass", "timestamp": "2026-01-01T00:00:00Z"})
        );
        let mut issue_value: serde_json::Value =
            serde_json::from_str(&issue_snapshot("app-1", "first", 0)).unwrap();
        issue_value["dependencies"] = serde_json::json!([]);
        seed_local(
            &repo,
            &[serde_json::to_string(&issue_value).unwrap()],
            &[event],
            &[verification],
        );

        let report = migrate_repo(&repo, &store_root, None, MigrateOptions::default()).unwrap();
        assert_eq!(report.issues, 1);
        assert_eq!(report.events, 1);
        assert_eq!(report.verifications, 1);
        assert!(!report.dry_run);
        assert_eq!(
            report.backups.len(),
            3,
            "issues+events+verifications backed up"
        );

        // Central side has everything.
        let cs = CentralStore::open_at(store_root.clone());
        let checkout = Checkout::at(&repo).unwrap();
        let central_repo = cs.find(&checkout).unwrap().unwrap();
        let issues = resolve_issues(&read_lines(&central_repo.issues_path()).unwrap()).unwrap();
        assert!(issues.contains_key("app-1"));
        let events = key_map(&read_lines(&central_repo.events_path()).unwrap(), "id").unwrap();
        assert!(events.contains_key("ev-1"));
        let verifications = key_map(
            &read_lines(&central_repo.verifications_path()).unwrap(),
            "run_id",
        )
        .unwrap();
        assert!(verifications.contains_key("run-1"));

        // Marker + MIGRATED note exist; local ledger files are gone (backed up).
        assert!(central::read_marker(&repo).unwrap().is_some());
        assert!(repo.join(".trx").join(MIGRATED_NOTE).is_file());
        assert!(!repo.join(".trx").join(ISSUES_FILE).exists());

        // Re-run is a no-op.
        let again = migrate_repo(&repo, &store_root, None, MigrateOptions::default()).unwrap();
        assert!(again.already_central);
    }

    #[test]
    fn test_dry_run_writes_nothing() {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        let store_root = base.path().join("store");
        init_git_repo(&repo);
        seed_local(&repo, &[issue_snapshot("app-1", "x", 0)], &[], &[]);

        let report = migrate_repo(
            &repo,
            &store_root,
            None,
            MigrateOptions {
                dry_run: true,
                untrack: false,
            },
        )
        .unwrap();
        assert!(report.dry_run);
        assert_eq!(report.issues, 1);
        assert!(
            !store_root.join("repos").exists() || {
                let count = fs::read_dir(store_root.join("repos")).unwrap().count();
                count == 0
            },
            "dry run must not register the repo"
        );
        assert!(repo.join(".trx").join(ISSUES_FILE).exists());
        assert!(central::read_marker(&repo).unwrap().is_none());
    }

    #[test]
    fn test_conflicting_event_id_aborts_with_nothing_moved() {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        let store_root = base.path().join("store");
        init_git_repo(&repo);
        let seed_event = r#"{"id":"ev-1","issue_id":"app-1","action":"created","timestamp":"2026-01-01T00:00:00Z"}"#;
        seed_local(
            &repo,
            &[issue_snapshot("app-1", "x", 0)],
            &[seed_event.to_string()],
            &[],
        );
        migrate_repo(&repo, &store_root, None, MigrateOptions::default()).unwrap();

        // Second checkout, same repo, same event id but different content.
        let repo2 = base.path().join("repo2");
        git(
            base.path(),
            &["clone", "-q", repo.join(".").to_str().unwrap(), "repo2"],
        );
        // repo2 has no .trx (not committed) — give it one with a conflicting event.
        let event = r#"{"id":"ev-1","issue_id":"app-1","action":"closed","timestamp":"2026-01-02T00:00:00Z"}"#;
        let trx_dir = repo2.join(".trx");
        fs::create_dir_all(&trx_dir).unwrap();
        fs::write(trx_dir.join(ISSUES_FILE), issue_snapshot("app-1", "x", 5)).unwrap();
        fs::write(trx_dir.join(EVENTS_FILE), event).unwrap();

        let before = fs::read_to_string(
            CentralStore::open_at(store_root.clone())
                .find(&Checkout::at(&repo2).unwrap())
                .unwrap()
                .unwrap()
                .events_path(),
        )
        .unwrap();

        let result = migrate_repo(&repo2, &store_root, None, MigrateOptions::default());
        assert!(result.is_err(), "conflicting event id must abort");

        // Nothing moved on either side.
        let after = fs::read_to_string(
            CentralStore::open_at(store_root.clone())
                .find(&Checkout::at(&repo2).unwrap())
                .unwrap()
                .unwrap()
                .events_path(),
        )
        .unwrap();
        assert_eq!(before, after);
        assert!(repo2.join(".trx").join(ISSUES_FILE).exists());
        assert!(central::read_marker(&repo2).unwrap().is_none());
    }

    #[test]
    fn test_backup_rename_round_trips_to_pre_migration_state() {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        let store_root = base.path().join("store");
        init_git_repo(&repo);
        let snapshot = issue_snapshot("app-1", "precious", 0);
        seed_local(&repo, std::slice::from_ref(&snapshot), &[], &[]);

        let report = migrate_repo(&repo, &store_root, None, MigrateOptions::default()).unwrap();
        assert_eq!(report.backups.len(), 1);
        let backup = &report.backups[0];

        // Restore the backup: file content identical, marker note points back.
        fs::copy(backup, repo.join(".trx").join(ISSUES_FILE)).unwrap();
        let restored = fs::read_to_string(repo.join(".trx").join(ISSUES_FILE)).unwrap();
        assert_eq!(restored, format!("{snapshot}\n"));
    }

    #[test]
    fn test_untrack_removes_migrated_files_from_git_index_only() {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        let store_root = base.path().join("store");
        init_git_repo(&repo);
        seed_local(&repo, &[issue_snapshot("app-1", "x", 0)], &[], &[]);
        git(&repo, &["add", ".trx"]);
        git(&repo, &["commit", "-q", "-m", "tracker"]);
        assert!(is_git_tracked(&repo, ".trx/issues.jsonl"));

        migrate_repo(
            &repo,
            &store_root,
            None,
            MigrateOptions {
                dry_run: false,
                untrack: true,
            },
        )
        .unwrap();

        assert!(
            !is_git_tracked(&repo, ".trx/issues.jsonl"),
            "untracked in index"
        );
        // Working tree change left uncommitted.
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let status = String::from_utf8_lossy(&status.stdout);
        assert!(
            status.contains("D  .trx/issues.jsonl"),
            "staged deletion, uncommitted: {status}"
        );
    }

    #[test]
    fn test_scan_finds_ledgers_skips_junk_and_respects_depth() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path();
        let shallow = root.join("shallow");
        seed_local(&shallow, &[issue_snapshot("a-1", "x", 0)], &[], &[]);
        let deep = root.join("a/b/c/d/deep");
        seed_local(&deep, &[issue_snapshot("b-1", "y", 0)], &[], &[]);
        let junk = root.join("node_modules/pkg");
        seed_local(&junk, &[issue_snapshot("c-1", "z", 0)], &[], &[]);
        let already = root.join("already-central");
        seed_local(&already, &[issue_snapshot("d-1", "w", 0)], &[], &[]);
        central::write_marker(&already, None, None).unwrap();

        let found = scan_for_ledgers(&[root.to_path_buf()], 8).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"shallow".to_string()));
        assert!(
            names.contains(&"deep".to_string()),
            "deep found at depth 5: {names:?}"
        );
        assert!(!names.contains(&"pkg".to_string()), "node_modules skipped");
        assert!(
            !names.contains(&"already-central".to_string()),
            "central-mode repos are no-ops, not migration targets"
        );

        let found2 = scan_for_ledgers(&[root.to_path_buf()], 2).unwrap();
        let names2: Vec<String> = found2
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names2.contains(&"deep".to_string()),
            "depth limit respected"
        );
        assert!(names2.contains(&"shallow".to_string()));
    }

    #[test]
    fn test_concatenated_jsonl_lines_are_split_and_merged() {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        let store_root = base.path().join("store");
        init_git_repo(&repo);
        // Crash artifact: two event objects on one line (missing newline).
        let a = r#"{"id":"ev-a","issue_id":"app-1","action":"created","timestamp":"2026-01-01T00:00:00Z"}"#;
        let b = r#"{"id":"ev-b","issue_id":"app-1","action":"updated","timestamp":"2026-01-02T00:00:00Z"}"#;
        seed_local(
            &repo,
            &[issue_snapshot("app-1", "x", 0)],
            &[format!("{a}{b}")],
            &[],
        );

        let report = migrate_repo(&repo, &store_root, None, MigrateOptions::default()).unwrap();
        assert_eq!(
            report.events, 2,
            "both objects from the concatenated line survive"
        );

        let cs = CentralStore::open_at(store_root);
        let central_repo = cs.find(&Checkout::at(&repo).unwrap()).unwrap().unwrap();
        let events = key_map(&read_lines(&central_repo.events_path()).unwrap(), "id").unwrap();
        assert!(events.contains_key("ev-a") && events.contains_key("ev-b"));
    }

    #[test]
    fn test_legacy_crdt_layout_is_converted_then_migrated() {
        use automerge::AutoCommit;
        use automerge::transaction::Transactable;
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        let store_root = base.path().join("store");
        init_git_repo(&repo);

        // A real automerge doc in the legacy layout (only id/title required).
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "id", "app-crdt").unwrap();
        doc.put(automerge::ROOT, "title", "from automerge era")
            .unwrap();
        let crdt_dir = repo.join(".trx/crdt");
        fs::create_dir_all(&crdt_dir).unwrap();
        fs::write(crdt_dir.join("app-crdt.automerge"), doc.save()).unwrap();

        let report = migrate_repo(&repo, &store_root, None, MigrateOptions::default()).unwrap();
        assert_eq!(report.issues, 1, "crdt issue migrated");
        assert!(
            !repo.join(".trx/crdt").exists(),
            "legacy layout removed after conversion"
        );
        assert!(central::read_marker(&repo).unwrap().is_some());

        let cs = CentralStore::open_at(store_root);
        let central_repo = cs.find(&Checkout::at(&repo).unwrap()).unwrap().unwrap();
        let issues = resolve_issues(&read_lines(&central_repo.issues_path()).unwrap()).unwrap();
        assert_eq!(issues["app-crdt"].title, "from automerge era");
    }
}
