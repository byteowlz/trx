//! Unified store that automatically detects and uses the correct storage backend
//!
//! Provides a common interface for both JSONL (v1) and CRDT (v2) storage.

use crate::{Config, CrdtStore, Error, Issue, Result, StorageVersion, Store};
use std::path::PathBuf;

const TRX_DIR: &str = ".trx";
const CONFIG_FILE: &str = "config.toml";

/// Unified store that wraps both JSONL and CRDT backends
pub enum UnifiedStore {
    V1(Store),
    V2(CrdtStore),
}

impl UnifiedStore {
    /// Open the store, auto-detecting the storage version
    pub fn open() -> Result<Self> {
        let root = Self::find_root()?;
        Self::open_at(root)
    }

    /// Open the store at an explicit repo root, auto-detecting storage version
    pub fn open_at(root: PathBuf) -> Result<Self> {
        if !root.join(TRX_DIR).exists() {
            return Err(Error::NotInitialized);
        }
        let config_path = root.join(TRX_DIR).join(CONFIG_FILE);
        let config = Config::load(&config_path)?;

        match config.storage_version {
            StorageVersion::V1 => Ok(UnifiedStore::V1(Store::open_at(root)?)),
            StorageVersion::V2 => Ok(UnifiedStore::V2(CrdtStore::open_at(root)?)),
        }
    }

    /// Initialize a new store with the specified version
    pub fn init(prefix: &str, version: StorageVersion) -> Result<Self> {
        match version {
            StorageVersion::V1 => Ok(UnifiedStore::V1(Store::init(prefix)?)),
            StorageVersion::V2 => Ok(UnifiedStore::V2(CrdtStore::init(prefix)?)),
        }
    }

    /// Find the repository root
    fn find_root() -> Result<PathBuf> {
        let mut current = std::env::current_dir()?;
        loop {
            if current.join(TRX_DIR).exists() {
                return Ok(current);
            }
            if !current.pop() {
                return Err(Error::NotInitialized);
            }
        }
    }

    /// Get the storage version
    pub fn version(&self) -> StorageVersion {
        match self {
            UnifiedStore::V1(_) => StorageVersion::V1,
            UnifiedStore::V2(_) => StorageVersion::V2,
        }
    }

    /// Path to the .trx directory
    pub fn trx_dir(&self) -> PathBuf {
        match self {
            UnifiedStore::V1(s) => s.trx_dir(),
            UnifiedStore::V2(s) => s.trx_dir(),
        }
    }

    /// Get an issue by ID
    pub fn get(&self, id: &str) -> Option<&Issue> {
        match self {
            UnifiedStore::V1(s) => s.get(id),
            UnifiedStore::V2(s) => s.get(id),
        }
    }

    /// Get a mutable issue by ID
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Issue> {
        match self {
            UnifiedStore::V1(s) => s.get_mut(id),
            UnifiedStore::V2(s) => s.get_mut(id),
        }
    }

    /// Create a new issue
    pub fn create(&mut self, issue: Issue) -> Result<()> {
        match self {
            UnifiedStore::V1(s) => s.create(issue),
            UnifiedStore::V2(s) => s.create(issue),
        }
    }

    /// Update an existing issue
    pub fn update(&mut self, issue: Issue) -> Result<()> {
        match self {
            UnifiedStore::V1(s) => s.update(issue),
            UnifiedStore::V2(s) => s.update(issue),
        }
    }

    /// Delete an issue (tombstone)
    pub fn delete(&mut self, id: &str, by: Option<String>, reason: Option<String>) -> Result<()> {
        match self {
            UnifiedStore::V1(s) => s.delete(id, by, reason),
            UnifiedStore::V2(s) => s.delete(id, by, reason),
        }
    }

    /// List all issues
    pub fn list(&self, include_tombstones: bool) -> Vec<&Issue> {
        match self {
            UnifiedStore::V1(s) => s.list(include_tombstones),
            UnifiedStore::V2(s) => s.list(include_tombstones),
        }
    }

    /// List open issues
    pub fn list_open(&self) -> Vec<&Issue> {
        match self {
            UnifiedStore::V1(s) => s.list_open(),
            UnifiedStore::V2(s) => s.list_open(),
        }
    }

    /// Get next child number for a parent
    pub fn next_child_num(&self, parent_id: &str) -> u32 {
        match self {
            UnifiedStore::V1(s) => s.next_child_num(parent_id),
            UnifiedStore::V2(s) => s.next_child_num(parent_id),
        }
    }

    /// Get the configured prefix
    pub fn prefix(&self) -> Result<String> {
        match self {
            UnifiedStore::V1(s) => s.prefix(),
            UnifiedStore::V2(s) => s.prefix(),
        }
    }

    /// Resolve any CRDT conflicts (v2 only, no-op for v1)
    pub fn resolve_conflicts(&mut self) -> Result<Vec<String>> {
        match self {
            UnifiedStore::V1(_) => Ok(Vec::new()),
            UnifiedStore::V2(s) => s.resolve_conflicts(),
        }
    }

    /// Regenerate ISSUES.md (v2 only, no-op for v1)
    pub fn regenerate_issues_md(&self) -> Result<()> {
        match self {
            UnifiedStore::V1(_) => Ok(()),
            UnifiedStore::V2(s) => s.regenerate_issues_md(),
        }
    }
}

/// Migrate from v1 (JSONL) to v2 (CRDT)
pub fn migrate_v1_to_v2(dry_run: bool) -> Result<MigrationResult> {
    let v1_store = Store::open()?;
    let issues: Vec<Issue> = v1_store.list(true).into_iter().cloned().collect();
    let trx_dir = v1_store.trx_dir();
    let config_path = trx_dir.join(CONFIG_FILE);

    if dry_run {
        return Ok(MigrationResult {
            issues_migrated: issues.len(),
            dry_run: true,
        });
    }

    // Create crdt directory
    let crdt_dir = trx_dir.join("crdt");
    std::fs::create_dir_all(&crdt_dir)?;

    // Create a temporary CRDT store to convert issues
    let mut crdt_store = CrdtStore {
        root: trx_dir.parent().unwrap_or(&trx_dir).to_path_buf(),
        issues: std::collections::HashMap::new(),
    };

    // Migrate each issue
    for issue in &issues {
        crdt_store.create(issue.clone())?;
    }

    // Update config to v2
    let mut config = Config::load(&config_path)?;
    config.storage_version = StorageVersion::V2;
    config.save(&config_path)?;

    // Regenerate ISSUES.md
    crdt_store.regenerate_issues_md()?;

    Ok(MigrationResult {
        issues_migrated: issues.len(),
        dry_run: false,
    })
}

/// Rollback from v2 (CRDT) to v1 (JSONL)
pub fn rollback_v2_to_v1(dry_run: bool) -> Result<MigrationResult> {
    let v2_store = CrdtStore::open()?;
    let issues: Vec<Issue> = v2_store.list(true).into_iter().cloned().collect();
    let trx_dir = v2_store.trx_dir();
    let config_path = trx_dir.join(CONFIG_FILE);

    if dry_run {
        return Ok(MigrationResult {
            issues_migrated: issues.len(),
            dry_run: true,
        });
    }

    // Write issues.jsonl
    let issues_path = trx_dir.join("issues.jsonl");
    let file = std::fs::File::create(&issues_path)?;
    let mut writer = std::io::BufWriter::new(file);
    for issue in &issues {
        serde_json::to_writer(&mut writer, issue)?;
        std::io::Write::write_all(&mut writer, b"\n")?;
    }
    std::io::Write::flush(&mut writer)?;

    // Update config to v1
    let mut config = Config::load(&config_path)?;
    config.storage_version = StorageVersion::V1;
    config.save(&config_path)?;

    // Optionally remove crdt directory (keep for safety)
    // std::fs::remove_dir_all(trx_dir.join("crdt"))?;

    Ok(MigrationResult {
        issues_migrated: issues.len(),
        dry_run: false,
    })
}

/// Result of a migration operation
pub struct MigrationResult {
    pub issues_migrated: usize,
    pub dry_run: bool,
}

// We need to make CrdtStore fields accessible for migration
impl CrdtStore {
    /// Create a CrdtStore with explicit root (for migration)
    pub fn with_root(root: PathBuf) -> Self {
        Self {
            root,
            issues: std::collections::HashMap::new(),
        }
    }
}
