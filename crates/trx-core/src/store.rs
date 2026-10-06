//! JSONL store for trx issues.
//!
//! No daemon, no SQLite — issues live in `.trx/issues.jsonl`. The store
//! transparently migrates legacy v2 (Automerge) layouts on open: when a
//! `.trx/crdt/` directory is detected, its `.automerge` files are read into
//! memory and the next mutation flushes JSONL and removes the legacy
//! directory. Reads never mutate disk.

use crate::central::{self, CentralRepo};
use crate::{Error, Issue, Result, legacy_crdt};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const TRX_DIR: &str = ".trx";
const ISSUES_FILE: &str = "issues.jsonl";
const LOCK_FILE: &str = "issues.lock";
const CONFIG_FILE: &str = "config.toml";
const GITATTRIBUTES_FILE: &str = ".gitattributes";
/// Git attributes required for clean merges of trx append-only JSONL logs.
pub const TRX_GITATTRIBUTES_LINES: [&str; 3] = [
    ".trx/issues.jsonl text eol=lf merge=union",
    ".trx/events.jsonl text eol=lf merge=union",
    ".trx/verifications.jsonl text eol=lf merge=union",
];
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(25);
const LEGACY_CRDT_DIR: &str = "crdt";
const LEGACY_ISSUES_MD: &str = "ISSUES.md";

/// JSONL-based issue store.
pub struct Store {
    root: PathBuf,
    issues: HashMap<String, Issue>,
    /// True when issues were loaded from legacy CRDT files; the next save will
    /// write canonical JSONL and remove the legacy `crdt/` directory.
    migrate_pending: bool,
    /// Set when the checkout selected central mode via `.trx/central`: the
    /// authoritative ledger lives in the per-user central store.
    central: Option<CentralRepo>,
}

impl Store {
    /// Find and open the store for the current directory.
    pub fn open() -> Result<Self> {
        let root = Self::find_root()?;
        Self::open_at(root)
    }

    /// The store root for the current directory (no CWD mutation).
    pub fn current_root() -> Result<PathBuf> {
        Self::find_root()
    }

    /// Open the store at an explicit repo root (no CWD probing).
    pub fn open_at(root: PathBuf) -> Result<Self> {
        if !root.join(TRX_DIR).exists() {
            return Err(Error::NotInitialized);
        }
        let mut store = Self {
            root,
            issues: HashMap::new(),
            migrate_pending: false,
            central: None,
        };
        if let Some(marker) = central::read_marker(&store.root)? {
            // Precedence: TRX_STORE_ROOT env (set from the --store-root flag)
            // > the root recorded at `central init --store-root` > named
            // store (--store / TRX_STORE / marker) > global config > default.
            let store_root = if let Ok(root) = std::env::var("TRX_STORE_ROOT") {
                crate::paths::expand_tilde(&root)?
            } else if let Some(root) = &marker.store_root {
                crate::paths::expand_tilde(root)?
            } else {
                crate::global_config::GlobalConfig::load()?.store_root(marker.store.as_deref())?
            };
            store.open_central(store_root)?;
        }
        store.load()?;
        Ok(store)
    }

    /// Open the store at an explicit repo root, forced into central mode
    /// against an explicit central store root. Used for the CLI `--store-root`
    /// override and tests; normal opens go through [`Store::open_at`] (marker
    /// + global config resolution).
    pub fn open_at_central(root: PathBuf, store_root: PathBuf) -> Result<Self> {
        if !root.join(TRX_DIR).exists() {
            return Err(Error::NotInitialized);
        }
        let mut store = Self {
            root,
            issues: HashMap::new(),
            migrate_pending: false,
            central: None,
        };
        store.open_central(store_root)?;
        store.load()?;
        Ok(store)
    }

    fn open_central(&mut self, store_root: PathBuf) -> Result<()> {
        let cs = central::CentralStore::open_at(store_root.clone());
        let checkout = central::Checkout::at(&self.root)?;
        self.central = Some(cs.register(&checkout)?);
        // Automatic sync (opt-in per store): pull at first access, remember
        // the store for the end-of-process final sync. Best-effort — never
        // fail the command because a git operation failed.
        if let Ok(config) = crate::global_config::GlobalConfig::load() {
            crate::sync::note_active_store(store_root.clone(), config.sync.clone());
            crate::sync::auto_pull_on_open(&store_root, &config.sync);
        }
        Ok(())
    }

    /// Initialize a new store in the current directory.
    pub fn init(prefix: &str) -> Result<Self> {
        let root = std::env::current_dir()?;
        let trx_dir = root.join(TRX_DIR);

        if trx_dir.exists() {
            return Err(Error::AlreadyInitialized(trx_dir.display().to_string()));
        }

        fs::create_dir_all(&trx_dir)?;

        let config = format!(
            r#"# trx configuration
prefix = "{}"
"#,
            prefix
        );
        fs::write(trx_dir.join(CONFIG_FILE), config)?;
        fs::write(trx_dir.join(ISSUES_FILE), "")?;
        Self::ensure_merge_attributes(&root)?;

        Ok(Self {
            root,
            issues: HashMap::new(),
            migrate_pending: false,
            central: None,
        })
    }

    fn find_root() -> Result<PathBuf> {
        let start = std::env::current_dir()?;

        if let Some(git_root) = Self::find_git_root_from(&start) {
            let mut current = start;
            loop {
                if current.join(TRX_DIR).exists() {
                    return Ok(current);
                }
                if current == git_root {
                    return Err(Error::NotInitialized);
                }
                if !current.pop() {
                    return Err(Error::NotInitialized);
                }
            }
        }

        // Outside a git repo, keep legacy behavior and search to filesystem root.
        let mut current = start;
        loop {
            if current.join(TRX_DIR).exists() {
                return Ok(current);
            }
            if !current.pop() {
                return Err(Error::NotInitialized);
            }
        }
    }

    fn find_git_root_from(start: &Path) -> Option<PathBuf> {
        let mut current = start.to_path_buf();
        loop {
            if current.join(".git").exists() {
                return Some(current);
            }
            if !current.pop() {
                return None;
            }
        }
    }

    /// Ensure the repository has Git attributes that make trx append-only logs
    /// merge with Git's built-in union driver.
    pub fn ensure_merge_attributes(root: &Path) -> Result<()> {
        let path = root.join(GITATTRIBUTES_FILE);
        let mut content = if path.exists() {
            fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let mut changed = false;
        for line in TRX_GITATTRIBUTES_LINES {
            if !content.lines().any(|existing| existing.trim() == line) {
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(line);
                content.push('\n');
                changed = true;
            }
        }
        if changed {
            fs::write(path, content)?;
        }
        Ok(())
    }

    /// Path to the .trx directory (always inside the checkout).
    pub fn trx_dir(&self) -> PathBuf {
        self.root.join(TRX_DIR)
    }

    /// Directory holding the authoritative ledger: the central repo dir in
    /// central mode, `.trx/` otherwise.
    pub fn ledger_dir(&self) -> PathBuf {
        self.central
            .as_ref()
            .map_or_else(|| self.trx_dir(), |repo| repo.dir.clone())
    }

    /// True when this checkout reads/writes the central store.
    pub fn is_central(&self) -> bool {
        self.central.is_some()
    }

    /// Details of the central repo backing this store, if in central mode.
    pub fn central_repo(&self) -> Option<&CentralRepo> {
        self.central.as_ref()
    }

    /// Path to issues.jsonl (in the authoritative ledger directory).
    pub fn issues_path(&self) -> PathBuf {
        self.ledger_dir().join(ISSUES_FILE)
    }

    /// True if the store was loaded from a legacy CRDT layout and the next
    /// save will materialize JSONL + clean up `crdt/`.
    pub fn migrate_pending(&self) -> bool {
        self.migrate_pending
    }

    fn load(&mut self) -> Result<()> {
        self.issues.clear();
        self.migrate_pending = false;
        let trx_dir = self.trx_dir();
        let crdt_dir = trx_dir.join(LEGACY_CRDT_DIR);

        // Legacy v2 layout: `.trx/crdt/*.automerge`. Load issues into memory
        // and flag the migration; we do not touch disk on read.
        if crdt_dir.exists() {
            let issues = legacy_crdt::load_issues(&crdt_dir)?;
            for issue in issues {
                self.issues.insert(issue.id.clone(), issue);
            }
            self.migrate_pending = true;
            return Ok(());
        }

        // Canonical JSONL layout.
        let path = self.issues_path();
        if !path.exists() {
            return Ok(());
        }
        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let issue: Issue = serde_json::from_str(&line)?;
            let should_replace = self
                .issues
                .get(&issue.id)
                .map(|current| issue.updated_at >= current.updated_at)
                .unwrap_or(true);
            if should_replace {
                self.issues.insert(issue.id.clone(), issue);
            }
        }
        Ok(())
    }

    /// Append current issue snapshots to JSONL. This preserves the append-only
    /// log shape; callers that mutate several issues in-memory still persist a
    /// latest snapshot for each issue instead of rewriting existing lines. On
    /// the first save after a legacy migration, also removes the `crdt/`
    /// directory and the derived `ISSUES.md` artifact.
    pub fn save(&mut self) -> Result<()> {
        let _lock = self.acquire_lock()?;
        self.save_locked()
    }

    fn save_locked(&mut self) -> Result<()> {
        let mut sorted: Vec<Issue> = self.issues.values().cloned().collect();
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        self.append_issues_locked(&sorted)?;
        self.finish_legacy_migration_if_needed();
        Ok(())
    }

    fn persist_changed_issue_locked(&mut self, issue: &Issue) -> Result<()> {
        if self.migrate_pending {
            self.save_locked()
        } else {
            self.append_issue_locked(issue)
        }
    }

    fn append_issue_locked(&mut self, issue: &Issue) -> Result<()> {
        self.append_issues_locked(std::slice::from_ref(issue))?;
        self.finish_legacy_migration_if_needed();
        Ok(())
    }

    fn append_issues_locked(&self, issues: &[Issue]) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }
        let path = self.issues_path();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let mut writer = BufWriter::new(file);
        for issue in issues {
            serde_json::to_writer(&mut writer, issue)?;
            writeln!(writer)?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        Ok(())
    }

    #[cfg(test)]
    fn compact_locked(&self) -> Result<()> {
        let path = self.issues_path();
        let tmp = path.with_extension("jsonl.tmp");

        {
            let file = File::create(&tmp)?;
            let mut writer = BufWriter::new(file);
            let mut sorted: Vec<&Issue> = self.issues.values().collect();
            sorted.sort_by(|a, b| a.id.cmp(&b.id));
            for issue in sorted {
                serde_json::to_writer(&mut writer, issue)?;
                writeln!(writer)?;
            }
            writer.flush()?;
            writer.get_ref().sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn finish_legacy_migration_if_needed(&mut self) {
        if self.migrate_pending {
            let crdt_dir = self.trx_dir().join(LEGACY_CRDT_DIR);
            if crdt_dir.exists() {
                let _ = fs::remove_dir_all(&crdt_dir);
            }
            let issues_md = self.trx_dir().join(LEGACY_ISSUES_MD);
            if issues_md.exists() {
                let _ = fs::remove_file(&issues_md);
            }
            self.migrate_pending = false;
        }
    }

    fn acquire_lock(&self) -> Result<StoreLock> {
        StoreLock::acquire(self.ledger_dir().join(LOCK_FILE))
    }

    pub fn get(&self, id: &str) -> Option<&Issue> {
        self.issues.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Issue> {
        self.issues.get_mut(id)
    }

    pub fn create(&mut self, issue: Issue) -> Result<()> {
        let _lock = self.acquire_lock()?;
        self.load()?;
        if self.issues.contains_key(&issue.id) {
            return Err(Error::AlreadyExists(issue.id));
        }
        self.issues.insert(issue.id.clone(), issue.clone());
        self.persist_changed_issue_locked(&issue)
    }

    pub fn update(&mut self, issue: Issue) -> Result<()> {
        let _lock = self.acquire_lock()?;
        self.load()?;
        if !self.issues.contains_key(&issue.id) {
            return Err(Error::NotFound(issue.id));
        }
        self.issues.insert(issue.id.clone(), issue.clone());
        self.persist_changed_issue_locked(&issue)
    }

    pub fn delete(&mut self, id: &str, by: Option<String>, reason: Option<String>) -> Result<()> {
        let _lock = self.acquire_lock()?;
        self.load()?;
        let issue = self
            .issues
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        issue.delete(by, reason);
        let issue = issue.clone();
        self.persist_changed_issue_locked(&issue)
    }

    pub fn list(&self, include_tombstones: bool) -> Vec<&Issue> {
        self.issues
            .values()
            .filter(|i| include_tombstones || i.status != crate::Status::Tombstone)
            .collect()
    }

    pub fn list_open(&self) -> Vec<&Issue> {
        self.issues
            .values()
            .filter(|i| i.status.is_open())
            .collect()
    }

    pub fn next_child_num(&self, parent_id: &str) -> u32 {
        let prefix = format!("{}.", parent_id);
        let max = self
            .issues
            .keys()
            .filter(|id| id.starts_with(&prefix))
            .filter_map(|id| {
                let suffix = &id[prefix.len()..];
                if !suffix.contains('.') {
                    suffix.parse::<u32>().ok()
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0);
        max + 1
    }

    pub fn prefix(&self) -> Result<String> {
        // Central mode: the central config is authoritative; fall back to the
        // checkout config for repos that migrated without a central one yet.
        if let Some(repo) = &self.central {
            let central_path = repo.config_path();
            if central_path.is_file() {
                let content = fs::read_to_string(&central_path)?;
                if let Some(value) = parse_prefix(&content) {
                    return Ok(value);
                }
            }
        }
        let config_path = self.trx_dir().join(CONFIG_FILE);
        if !config_path.exists() {
            return Ok("trx".to_string());
        }
        let content = fs::read_to_string(&config_path)?;
        Ok(parse_prefix(&content).unwrap_or_else(|| "trx".to_string()))
    }
}

fn parse_prefix(content: &str) -> Option<String> {
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("prefix")
            && let Some(value) = value.trim().strip_prefix('=')
        {
            let value = value.trim().trim_matches('"');
            return Some(value.to_string());
        }
    }
    None
}

struct StoreLock {
    path: PathBuf,
}

impl StoreLock {
    fn acquire(path: PathBuf) -> Result<Self> {
        let start = Instant::now();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    writeln!(
                        file,
                        "pid={} acquired_at={}",
                        std::process::id(),
                        chrono::Utc::now()
                    )?;
                    file.sync_all()?;
                    return Ok(Self { path });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if start.elapsed() >= LOCK_TIMEOUT {
                        return Err(Error::Other(format!(
                            "Timed out waiting for store lock at {}. Another trx process may be running; remove the lock only if no trx process is active.",
                            path.display()
                        )));
                    }
                    thread::sleep(LOCK_RETRY_DELAY);
                }
                Err(err) => return Err(Error::Io(err)),
            }
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Status;
    use chrono::Duration as ChronoDuration;
    use std::sync::{Arc, Barrier};

    fn init_temp_store(root: &Path) -> Store {
        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(root).unwrap();
        let store = Store::init("trx").unwrap();
        std::env::set_current_dir(old_cwd).unwrap();
        store
    }

    fn write_issues_log(root: &Path, issues: &[Issue]) {
        fs::create_dir_all(root.join(TRX_DIR)).unwrap();
        let mut content = String::new();
        for issue in issues {
            content.push_str(&serde_json::to_string(issue).unwrap());
            content.push('\n');
        }
        fs::write(root.join(TRX_DIR).join(ISSUES_FILE), content).unwrap();
    }

    fn issue_with_timestamp(
        id: &str,
        title: &str,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Issue {
        let mut issue = Issue::new(id.into(), title.into());
        issue.created_at = updated_at;
        issue.updated_at = updated_at;
        issue
    }

    #[test]
    fn test_append_only_round_trip_matches_full_rewrite_snapshot() {
        let append_temp = tempfile::tempdir().unwrap();
        let mut append_store = init_temp_store(append_temp.path());

        for n in 0..4 {
            append_store
                .create(Issue::new(format!("trx-{n}"), format!("issue {n}")))
                .unwrap();
        }

        let mut issue_1 = append_store.get("trx-1").unwrap().clone();
        issue_1.status = Status::InProgress;
        issue_1.updated_at += ChronoDuration::seconds(1);
        append_store.update(issue_1.clone()).unwrap();

        let mut issue_3 = append_store.get("trx-3").unwrap().clone();
        issue_3.status = Status::Closed;
        issue_3.close_reason = Some("done".into());
        issue_3.updated_at += ChronoDuration::seconds(2);
        append_store.update(issue_3.clone()).unwrap();

        let append_reloaded = Store::open_at(append_temp.path().to_path_buf()).unwrap();
        let mut latest: Vec<Issue> = append_reloaded.list(true).into_iter().cloned().collect();
        latest.sort_by(|a, b| a.id.cmp(&b.id));

        let rewrite_temp = tempfile::tempdir().unwrap();
        write_issues_log(rewrite_temp.path(), &latest);
        let rewrite_reloaded = Store::open_at(rewrite_temp.path().to_path_buf()).unwrap();

        for expected in latest {
            let actual = rewrite_reloaded.get(&expected.id).unwrap();
            assert_eq!(actual.title, expected.title);
            assert_eq!(actual.status, expected.status);
            assert_eq!(actual.updated_at, expected.updated_at);
            assert_eq!(actual.close_reason, expected.close_reason);
        }
    }

    #[test]
    fn test_load_uses_updated_at_lww_for_duplicate_issue_ids() {
        let temp = tempfile::tempdir().unwrap();
        let t0 = chrono::Utc::now();
        let mut older = issue_with_timestamp("trx-dup", "older", t0);
        older.status = Status::Open;
        let mut newer = issue_with_timestamp("trx-dup", "newer", t0 + ChronoDuration::seconds(5));
        newer.status = Status::Closed;

        write_issues_log(temp.path(), &[newer.clone(), older.clone(), newer.clone()]);
        let store = Store::open_at(temp.path().to_path_buf()).unwrap();

        let issue = store.get("trx-dup").unwrap();
        assert_eq!(issue.title, "newer");
        assert_eq!(issue.status, Status::Closed);
        assert_eq!(store.list(true).len(), 1);
    }

    #[test]
    fn test_load_uses_file_order_when_updated_at_ties() {
        let temp = tempfile::tempdir().unwrap();
        let t0 = chrono::Utc::now();
        let first = issue_with_timestamp("trx-dup", "first", t0);
        let second = issue_with_timestamp("trx-dup", "second", t0);

        write_issues_log(temp.path(), &[first, second]);
        let store = Store::open_at(temp.path().to_path_buf()).unwrap();

        assert_eq!(store.get("trx-dup").unwrap().title, "second");
    }

    #[test]
    fn test_union_merge_log_loads_one_winner_and_compacts_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let t0 = chrono::Utc::now();
        let base = issue_with_timestamp("trx-merge", "base", t0);
        let mut ours = issue_with_timestamp("trx-merge", "ours", t0 + ChronoDuration::seconds(1));
        ours.status = Status::InProgress;
        let mut theirs =
            issue_with_timestamp("trx-merge", "theirs", t0 + ChronoDuration::seconds(2));
        theirs.status = Status::Closed;

        // Simulate a union merge of an append-only base/ours/theirs history:
        // all snapshots survive, and load resolves the current state by LWW.
        write_issues_log(
            temp.path(),
            &[base.clone(), ours.clone(), base, theirs.clone()],
        );
        let store = Store::open_at(temp.path().to_path_buf()).unwrap();

        let issue = store.get("trx-merge").unwrap();
        assert_eq!(issue.title, "theirs");
        assert_eq!(issue.status, Status::Closed);
        assert_eq!(store.list(true).len(), 1);

        store.compact_locked().unwrap();
        let compacted = fs::read_to_string(store.issues_path()).unwrap();
        assert_eq!(compacted.lines().count(), 1);
        let compacted_issue: Issue =
            serde_json::from_str(compacted.lines().next().unwrap()).unwrap();
        assert_eq!(compacted_issue.title, "theirs");
    }

    #[test]
    fn test_create_appends_one_line_without_rewriting_existing_log() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = init_temp_store(temp.path());
        store
            .create(Issue::new("trx-b".into(), "second sort key".into()))
            .unwrap();
        let before = fs::read_to_string(store.issues_path()).unwrap();
        let before_lines = before.lines().count();

        store
            .create(Issue::new("trx-a".into(), "first sort key".into()))
            .unwrap();
        let after = fs::read_to_string(store.issues_path()).unwrap();

        assert!(after.starts_with(&before));
        assert_eq!(after.lines().count(), before_lines + 1);
    }

    #[test]
    fn test_init_writes_union_merge_gitattributes() {
        let temp = tempfile::tempdir().unwrap();
        init_temp_store(temp.path());

        let attrs = fs::read_to_string(temp.path().join(GITATTRIBUTES_FILE)).unwrap();
        for line in TRX_GITATTRIBUTES_LINES {
            assert!(attrs.lines().any(|existing| existing == line));
        }
    }

    #[test]
    fn test_concurrent_creates_are_serialized_without_lost_issues() {
        let temp = tempfile::tempdir().unwrap();
        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp.path()).unwrap();
        Store::init("trx").unwrap();
        std::env::set_current_dir(old_cwd).unwrap();

        let root = temp.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();
        for n in 0..8 {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let mut store = Store::open_at(root).unwrap();
                let id = format!("trx-{n}");
                barrier.wait();
                store.create(Issue::new(id, format!("issue {n}"))).unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let store = Store::open_at(temp.path().to_path_buf()).unwrap();
        assert_eq!(store.list(false).len(), 8);
        for n in 0..8 {
            assert!(store.get(&format!("trx-{n}")).is_some());
        }
    }

    #[test]
    fn test_find_root_does_not_cross_git_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let old_cwd = std::env::current_dir().unwrap();

        // Parent tracker exists, but child repo has its own git boundary.
        fs::create_dir_all(temp.path().join(TRX_DIR)).unwrap();
        let repo = temp.path().join("child-repo");
        let nested = repo.join("src/nested");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        std::env::set_current_dir(&nested).unwrap();
        let root = Store::find_root();
        std::env::set_current_dir(old_cwd).unwrap();

        assert!(matches!(root, Err(Error::NotInitialized)));
    }

    #[test]
    fn test_find_root_within_git_boundary_finds_repo_trx() {
        let temp = tempfile::tempdir().unwrap();
        let old_cwd = std::env::current_dir().unwrap();

        let repo = temp.path().join("repo");
        let nested = repo.join("src/nested");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join(TRX_DIR)).unwrap();
        fs::create_dir_all(&nested).unwrap();

        std::env::set_current_dir(&nested).unwrap();
        let root = Store::find_root().unwrap();
        std::env::set_current_dir(old_cwd).unwrap();

        assert_eq!(root, repo);
    }

    // --- central mode integration -----------------------------------------

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
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

    fn init_git_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join("file.txt"), "hello\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "initial"]);
    }

    #[test]
    fn test_central_mode_routes_ledger_to_store_and_back() {
        let repo = tempfile::tempdir().unwrap();
        let store_root = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        fs::create_dir_all(repo.path().join(TRX_DIR)).unwrap();
        fs::write(
            repo.path().join(TRX_DIR).join(CONFIG_FILE),
            "prefix = \"app\"\n",
        )
        .unwrap();
        central::write_marker(repo.path(), None, None).unwrap();

        let mut store =
            Store::open_at_central(repo.path().to_path_buf(), store_root.path().to_path_buf())
                .unwrap();
        assert!(store.is_central());
        assert!(store.issues_path().starts_with(store_root.path()));

        store
            .create(Issue::new("app-1".into(), "central issue".into()))
            .unwrap();

        // Nothing was written into the checkout.
        assert!(!repo.path().join(TRX_DIR).join(ISSUES_FILE).exists());

        // A worktree of the same repo sees the same ledger.
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt");
        git(
            repo.path(),
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                "-b",
                "feature",
            ],
        );
        fs::create_dir_all(wt.join(TRX_DIR)).unwrap();
        central::write_marker(&wt, None, None).unwrap();

        let wt_store = Store::open_at_central(wt, store_root.path().to_path_buf()).unwrap();
        assert_eq!(wt_store.get("app-1").unwrap().title, "central issue");
        assert_eq!(
            store.central_repo().unwrap().dir,
            wt_store.central_repo().unwrap().dir
        );
    }

    #[test]
    fn test_central_mode_prefix_comes_from_central_config_with_fallback() {
        let repo = tempfile::tempdir().unwrap();
        let store_root = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join(TRX_DIR)).unwrap();
        fs::write(
            repo.path().join(TRX_DIR).join(CONFIG_FILE),
            "prefix = \"old\"\n",
        )
        .unwrap();
        central::write_marker(repo.path(), None, None).unwrap();

        let store =
            Store::open_at_central(repo.path().to_path_buf(), store_root.path().to_path_buf())
                .unwrap();
        // No central config yet: fall back to the checkout config.
        assert_eq!(store.prefix().unwrap(), "old");

        let central_cfg = store.central_repo().unwrap().config_path();
        fs::write(&central_cfg, "prefix = \"new\"\n").unwrap();
        let reloaded =
            Store::open_at_central(repo.path().to_path_buf(), store_root.path().to_path_buf())
                .unwrap();
        assert_eq!(reloaded.prefix().unwrap(), "new");
    }

    #[test]
    fn test_repo_local_config_cannot_redirect_central_store() {
        // A cloned repository must not be able to point the central store at
        // an attacker-chosen location; the store root comes from the global
        // layer only (here: the explicit override), never from .trx/config.toml.
        let repo = tempfile::tempdir().unwrap();
        let store_root = tempfile::tempdir().unwrap();
        let evil = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join(TRX_DIR)).unwrap();
        fs::write(
            repo.path().join(TRX_DIR).join(CONFIG_FILE),
            format!("store_root = \"{}\"\n", evil.path().display()),
        )
        .unwrap();
        central::write_marker(repo.path(), None, None).unwrap();

        let store =
            Store::open_at_central(repo.path().to_path_buf(), store_root.path().to_path_buf())
                .unwrap();
        assert!(store.issues_path().starts_with(store_root.path()));
        assert!(fs::read_dir(evil.path()).unwrap().count() == 0);
    }

    #[test]
    fn test_without_marker_ledger_stays_repo_local() {
        let repo = tempfile::tempdir().unwrap();
        // Lay out a repo-local store directly (no chdir: tests run in
        // parallel and current_dir is process-global).
        fs::create_dir_all(repo.path().join(TRX_DIR)).unwrap();
        fs::write(
            repo.path().join(TRX_DIR).join(CONFIG_FILE),
            "prefix = \"trx\"\n",
        )
        .unwrap();
        fs::write(repo.path().join(TRX_DIR).join(ISSUES_FILE), "").unwrap();

        let mut store = Store::open_at(repo.path().to_path_buf()).unwrap();
        store
            .create(Issue::new("trx-x".into(), "local".into()))
            .unwrap();
        assert!(!store.is_central());
        assert!(repo.path().join(TRX_DIR).join(ISSUES_FILE).is_file());
    }
}
