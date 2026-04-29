//! JSONL store for trx issues
//!
//! No SQLite, no daemon - just files.

use crate::{Error, Issue, Result};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

const TRX_DIR: &str = ".trx";
const ISSUES_FILE: &str = "issues.jsonl";
const CONFIG_FILE: &str = "config.toml";

/// JSONL-based issue store
pub struct Store {
    root: PathBuf,
    issues: HashMap<String, Issue>,
}

impl Store {
    /// Find and open the store for the current directory
    pub fn open() -> Result<Self> {
        let root = Self::find_root()?;
        Self::open_at(root)
    }

    /// Open the store at an explicit repo root (no CWD probing)
    pub fn open_at(root: PathBuf) -> Result<Self> {
        if !root.join(TRX_DIR).exists() {
            return Err(Error::NotInitialized);
        }
        let mut store = Self {
            root,
            issues: HashMap::new(),
        };
        store.load()?;
        Ok(store)
    }

    /// Initialize a new store in the current directory
    pub fn init(prefix: &str) -> Result<Self> {
        let root = std::env::current_dir()?;
        let trx_dir = root.join(TRX_DIR);

        if trx_dir.exists() {
            return Err(Error::AlreadyInitialized(trx_dir.display().to_string()));
        }

        fs::create_dir_all(&trx_dir)?;

        // Create config
        let config = format!(
            r#"# trx configuration
prefix = "{}"
"#,
            prefix
        );
        fs::write(trx_dir.join(CONFIG_FILE), config)?;

        // Create empty issues file
        fs::write(trx_dir.join(ISSUES_FILE), "")?;

        Ok(Self {
            root,
            issues: HashMap::new(),
        })
    }

    /// Find the repository root (directory containing .trx)
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

    /// Path to the .trx directory
    pub fn trx_dir(&self) -> PathBuf {
        self.root.join(TRX_DIR)
    }

    /// Path to issues.jsonl
    pub fn issues_path(&self) -> PathBuf {
        self.trx_dir().join(ISSUES_FILE)
    }

    /// Load all issues from JSONL
    fn load(&mut self) -> Result<()> {
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

    /// Save all issues to JSONL
    pub fn save(&self) -> Result<()> {
        let path = self.issues_path();
        let file = File::create(&path)?;
        let mut writer = BufWriter::new(file);

        for issue in self.issues.values() {
            serde_json::to_writer(&mut writer, issue)?;
            writeln!(writer)?;
        }

        writer.flush()?;
        Ok(())
    }

    /// Get an issue by ID
    pub fn get(&self, id: &str) -> Option<&Issue> {
        self.issues.get(id)
    }

    /// Get a mutable issue by ID
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Issue> {
        self.issues.get_mut(id)
    }

    /// Create a new issue
    pub fn create(&mut self, issue: Issue) -> Result<()> {
        if self.issues.contains_key(&issue.id) {
            return Err(Error::AlreadyExists(issue.id));
        }
        self.issues.insert(issue.id.clone(), issue);
        self.save()
    }

    /// Update an existing issue
    pub fn update(&mut self, issue: Issue) -> Result<()> {
        if !self.issues.contains_key(&issue.id) {
            return Err(Error::NotFound(issue.id));
        }
        self.issues.insert(issue.id.clone(), issue);
        self.save()
    }

    /// Delete an issue (tombstone)
    pub fn delete(&mut self, id: &str, by: Option<String>, reason: Option<String>) -> Result<()> {
        let issue = self
            .issues
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        issue.delete(by, reason);
        self.save()
    }

    /// List all issues (excluding tombstones by default)
    pub fn list(&self, include_tombstones: bool) -> Vec<&Issue> {
        self.issues
            .values()
            .filter(|i| include_tombstones || i.status != crate::Status::Tombstone)
            .collect()
    }

    /// List open issues
    pub fn list_open(&self) -> Vec<&Issue> {
        self.issues
            .values()
            .filter(|i| i.status.is_open())
            .collect()
    }

    /// Get next child number for a parent
    pub fn next_child_num(&self, parent_id: &str) -> u32 {
        let prefix = format!("{}.", parent_id);
        let max = self
            .issues
            .keys()
            .filter(|id| id.starts_with(&prefix))
            .filter_map(|id| {
                let suffix = &id[prefix.len()..];
                // Only count direct children (no more dots)
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

    /// Get the configured prefix
    pub fn prefix(&self) -> Result<String> {
        let config_path = self.trx_dir().join(CONFIG_FILE);
        let content = fs::read_to_string(&config_path)?;

        // Simple TOML parsing for prefix
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
