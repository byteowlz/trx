//! JSONL store for trx issues.
//!
//! No daemon, no SQLite — issues live in `.trx/issues.jsonl`. The store
//! transparently migrates legacy v2 (Automerge) layouts on open: when a
//! `.trx/crdt/` directory is detected, its `.automerge` files are read into
//! memory and the next mutation flushes JSONL and removes the legacy
//! directory. Reads never mutate disk.

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
}

impl Store {
    /// Find and open the store for the current directory.
    pub fn open() -> Result<Self> {
        let root = Self::find_root()?;
        Self::open_at(root)
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
        };
        store.load()?;
        Ok(store)
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

        Ok(Self {
            root,
            issues: HashMap::new(),
            migrate_pending: false,
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

    /// Path to the .trx directory.
    pub fn trx_dir(&self) -> PathBuf {
        self.root.join(TRX_DIR)
    }

    /// Path to issues.jsonl.
    pub fn issues_path(&self) -> PathBuf {
        self.trx_dir().join(ISSUES_FILE)
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
            self.issues.insert(issue.id.clone(), issue);
        }
        Ok(())
    }

    /// Save all issues to JSONL atomically (temp file + rename). On the first
    /// save after a legacy migration, also removes the `crdt/` directory and
    /// the derived `ISSUES.md` artifact.
    pub fn save(&mut self) -> Result<()> {
        let _lock = self.acquire_lock()?;
        self.save_locked()
    }

    fn save_locked(&mut self) -> Result<()> {
        let path = self.issues_path();
        let tmp = path.with_extension("jsonl.tmp");

        {
            let file = File::create(&tmp)?;
            let mut writer = BufWriter::new(file);
            // Stable order makes JSONL diffs reviewable; HashMap iteration is
            // otherwise nondeterministic.
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

        Ok(())
    }

    fn acquire_lock(&self) -> Result<StoreLock> {
        StoreLock::acquire(self.trx_dir().join(LOCK_FILE))
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
        self.issues.insert(issue.id.clone(), issue);
        self.save_locked()
    }

    pub fn update(&mut self, issue: Issue) -> Result<()> {
        let _lock = self.acquire_lock()?;
        self.load()?;
        if !self.issues.contains_key(&issue.id) {
            return Err(Error::NotFound(issue.id));
        }
        self.issues.insert(issue.id.clone(), issue);
        self.save_locked()
    }

    pub fn delete(&mut self, id: &str, by: Option<String>, reason: Option<String>) -> Result<()> {
        let _lock = self.acquire_lock()?;
        self.load()?;
        let issue = self
            .issues
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        issue.delete(by, reason);
        self.save_locked()
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
        let config_path = self.trx_dir().join(CONFIG_FILE);
        if !config_path.exists() {
            return Ok("trx".to_string());
        }
        let content = fs::read_to_string(&config_path)?;
        for line in content.lines() {
            if let Some(value) = line.strip_prefix("prefix")
                && let Some(value) = value.trim().strip_prefix('=')
            {
                let value = value.trim().trim_matches('"');
                return Ok(value.to_string());
            }
        }
        Ok("trx".to_string())
    }
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
    use std::sync::{Arc, Barrier};

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
}
