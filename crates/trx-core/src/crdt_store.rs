//! CRDT-based store for trx issues using automerge
//!
//! Each issue is stored as a separate .automerge file for conflict-free merging.

use crate::{Config, Error, Issue, Result, StorageVersion};
use automerge::{AutoCommit, ObjType, ReadDoc, transaction::Transactable};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;

const TRX_DIR: &str = ".trx";
const CRDT_DIR: &str = "crdt";
const CONFIG_FILE: &str = "config.toml";
const ISSUES_MD: &str = "ISSUES.md";

/// CRDT-based issue store
pub struct CrdtStore {
    pub(crate) root: PathBuf,
    pub(crate) issues: HashMap<String, Issue>,
}

impl CrdtStore {
    /// Find and open the CRDT store for the current directory
    pub fn open() -> Result<Self> {
        let root = Self::find_root()?;
        let mut store = Self {
            root,
            issues: HashMap::new(),
        };
        store.load()?;
        Ok(store)
    }

    /// Initialize a new CRDT store (v2)
    pub fn init(prefix: &str) -> Result<Self> {
        let root = std::env::current_dir()?;
        let trx_dir = root.join(TRX_DIR);
        let crdt_dir = trx_dir.join(CRDT_DIR);

        if trx_dir.exists() {
            return Err(Error::AlreadyInitialized(trx_dir.display().to_string()));
        }

        fs::create_dir_all(&crdt_dir)?;

        // Create config with v2 storage
        let mut config = Config::default();
        config.storage_version = StorageVersion::V2;
        config.prefix = prefix.to_string();
        config.save(&trx_dir.join(CONFIG_FILE))?;

        // Create empty ISSUES.md
        fs::write(trx_dir.join(ISSUES_MD), "# Issues\n\nNo issues yet.\n")?;

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

    /// Path to the crdt directory
    pub fn crdt_dir(&self) -> PathBuf {
        self.trx_dir().join(CRDT_DIR)
    }

    /// Path to ISSUES.md
    pub fn issues_md_path(&self) -> PathBuf {
        self.trx_dir().join(ISSUES_MD)
    }

    /// Path to a specific issue's automerge file
    fn issue_path(&self, id: &str) -> PathBuf {
        self.crdt_dir().join(format!("{}.automerge", id))
    }

    /// Load all issues from CRDT files
    fn load(&mut self) -> Result<()> {
        let crdt_dir = self.crdt_dir();
        if !crdt_dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(&crdt_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "automerge") {
                match self.load_issue_from_file(&path) {
                    Ok(issue) => {
                        self.issues.insert(issue.id.clone(), issue);
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to load {}: {}", path.display(), e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Load a single issue from an automerge file
    fn load_issue_from_file(&self, path: &PathBuf) -> Result<Issue> {
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        let doc = AutoCommit::load(&bytes)
            .map_err(|e| Error::Other(format!("Failed to load automerge doc: {}", e)))?;

        self.doc_to_issue(&doc)
    }

    /// Helper to get a string from an automerge doc at root
    fn get_str(doc: &AutoCommit, key: &str) -> Option<String> {
        doc.get(automerge::ROOT, key)
            .ok()
            .flatten()
            .and_then(|(v, _)| v.to_str().map(|s| s.to_string()))
    }

    /// Helper to get a u8 from an automerge doc at root
    fn get_u8(doc: &AutoCommit, key: &str) -> Option<u8> {
        doc.get(automerge::ROOT, key)
            .ok()
            .flatten()
            .and_then(|(v, _)| v.to_i64().map(|n| n as u8))
    }

    /// Helper to get a datetime from an automerge doc at root
    fn get_datetime(doc: &AutoCommit, key: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        Self::get_str(doc, key).and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        })
    }

    /// Convert an automerge document to an Issue
    fn doc_to_issue(&self, doc: &AutoCommit) -> Result<Issue> {
        let id =
            Self::get_str(doc, "id").ok_or_else(|| Error::Other("Missing id field".to_string()))?;
        let title = Self::get_str(doc, "title")
            .ok_or_else(|| Error::Other("Missing title field".to_string()))?;

        let mut issue = Issue::new(id, title);

        if let Some(desc) = Self::get_str(doc, "description") {
            issue.description = Some(desc);
        }
        if let Some(status) = Self::get_str(doc, "status") {
            issue.status = status.parse().unwrap_or_default();
        }
        if let Some(priority) = Self::get_u8(doc, "priority") {
            issue.priority = priority;
        }
        if let Some(itype) = Self::get_str(doc, "issue_type") {
            issue.issue_type = itype.parse().unwrap_or_default();
        }
        if let Some(created) = Self::get_datetime(doc, "created_at") {
            issue.created_at = created;
        }
        if let Some(updated) = Self::get_datetime(doc, "updated_at") {
            issue.updated_at = updated;
        }
        if let Some(closed) = Self::get_datetime(doc, "closed_at") {
            issue.closed_at = Some(closed);
        }
        if let Some(assignee) = Self::get_str(doc, "assignee") {
            issue.assignee = Some(assignee);
        }
        if let Some(reason) = Self::get_str(doc, "close_reason") {
            issue.close_reason = Some(reason);
        }
        if let Some(notes) = Self::get_str(doc, "notes") {
            issue.notes = Some(notes);
        }

        // Load labels
        if let Ok(Some((_, labels_id))) = doc.get(automerge::ROOT, "labels") {
            let len = doc.length(&labels_id);
            for i in 0..len {
                if let Ok(Some((v, _))) = doc.get(&labels_id, i) {
                    if let Some(s) = v.to_str() {
                        issue.labels.push(s.to_string());
                    }
                }
            }
        }

        // Load dependencies
        if let Ok(Some((_, deps_id))) = doc.get(automerge::ROOT, "dependencies") {
            let len = doc.length(&deps_id);
            for i in 0..len {
                if let Ok(Some((_, dep_obj))) = doc.get(&deps_id, i) {
                    // Get dependency fields
                    let get_dep_field = |key: &str| -> Option<String> {
                        doc.get(&dep_obj, key)
                            .ok()
                            .flatten()
                            .and_then(|(v, _)| v.to_str().map(|s| s.to_string()))
                    };

                    let issue_id = get_dep_field("issue_id");
                    let depends_on_id = get_dep_field("depends_on_id");
                    let dep_type_str = get_dep_field("type");
                    let created_at_str = get_dep_field("created_at");
                    let created_by = get_dep_field("created_by");

                    if let (Some(issue_id), Some(depends_on_id)) = (issue_id, depends_on_id) {
                        let dep_type = dep_type_str
                            .and_then(|t| t.parse::<crate::DependencyType>().ok())
                            .unwrap_or_default();

                        let created_at = created_at_str
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .unwrap_or_else(chrono::Utc::now);

                        issue.dependencies.push(crate::Dependency {
                            issue_id,
                            depends_on_id,
                            dep_type,
                            created_at,
                            created_by,
                        });
                    }
                }
            }
        }

        Ok(issue)
    }

    /// Convert an Issue to an automerge document
    fn issue_to_doc(&self, issue: &Issue) -> Result<AutoCommit> {
        let mut doc = AutoCommit::new();

        doc.put(automerge::ROOT, "id", issue.id.as_str())
            .map_err(|e| Error::Other(format!("Failed to set id: {}", e)))?;
        doc.put(automerge::ROOT, "title", issue.title.as_str())
            .map_err(|e| Error::Other(format!("Failed to set title: {}", e)))?;

        if let Some(ref desc) = issue.description {
            doc.put(automerge::ROOT, "description", desc.as_str())
                .map_err(|e| Error::Other(format!("Failed to set description: {}", e)))?;
        }

        doc.put(automerge::ROOT, "status", issue.status.to_string().as_str())
            .map_err(|e| Error::Other(format!("Failed to set status: {}", e)))?;
        doc.put(automerge::ROOT, "priority", issue.priority as i64)
            .map_err(|e| Error::Other(format!("Failed to set priority: {}", e)))?;
        doc.put(
            automerge::ROOT,
            "issue_type",
            issue.issue_type.to_string().as_str(),
        )
        .map_err(|e| Error::Other(format!("Failed to set issue_type: {}", e)))?;
        doc.put(
            automerge::ROOT,
            "created_at",
            issue.created_at.to_rfc3339().as_str(),
        )
        .map_err(|e| Error::Other(format!("Failed to set created_at: {}", e)))?;
        doc.put(
            automerge::ROOT,
            "updated_at",
            issue.updated_at.to_rfc3339().as_str(),
        )
        .map_err(|e| Error::Other(format!("Failed to set updated_at: {}", e)))?;

        if let Some(ref closed_at) = issue.closed_at {
            doc.put(
                automerge::ROOT,
                "closed_at",
                closed_at.to_rfc3339().as_str(),
            )
            .map_err(|e| Error::Other(format!("Failed to set closed_at: {}", e)))?;
        }
        if let Some(ref assignee) = issue.assignee {
            doc.put(automerge::ROOT, "assignee", assignee.as_str())
                .map_err(|e| Error::Other(format!("Failed to set assignee: {}", e)))?;
        }
        if let Some(ref reason) = issue.close_reason {
            doc.put(automerge::ROOT, "close_reason", reason.as_str())
                .map_err(|e| Error::Other(format!("Failed to set close_reason: {}", e)))?;
        }
        if let Some(ref notes) = issue.notes {
            doc.put(automerge::ROOT, "notes", notes.as_str())
                .map_err(|e| Error::Other(format!("Failed to set notes: {}", e)))?;
        }

        // Labels
        if !issue.labels.is_empty() {
            let labels_id = doc
                .put_object(automerge::ROOT, "labels", ObjType::List)
                .map_err(|e| Error::Other(format!("Failed to create labels: {}", e)))?;
            for (i, label) in issue.labels.iter().enumerate() {
                doc.insert(&labels_id, i, label.as_str())
                    .map_err(|e| Error::Other(format!("Failed to add label: {}", e)))?;
            }
        }

        // Dependencies
        if !issue.dependencies.is_empty() {
            let deps_id = doc
                .put_object(automerge::ROOT, "dependencies", ObjType::List)
                .map_err(|e| Error::Other(format!("Failed to create dependencies: {}", e)))?;

            for (i, dep) in issue.dependencies.iter().enumerate() {
                let dep_obj = doc
                    .insert_object(&deps_id, i, ObjType::Map)
                    .map_err(|e| Error::Other(format!("Failed to create dep object: {}", e)))?;

                doc.put(&dep_obj, "issue_id", dep.issue_id.as_str())
                    .map_err(|e| Error::Other(format!("Failed to set dep issue_id: {}", e)))?;
                doc.put(&dep_obj, "depends_on_id", dep.depends_on_id.as_str())
                    .map_err(|e| Error::Other(format!("Failed to set dep depends_on_id: {}", e)))?;
                doc.put(&dep_obj, "type", dep.dep_type.to_string().as_str())
                    .map_err(|e| Error::Other(format!("Failed to set dep type: {}", e)))?;
                doc.put(&dep_obj, "created_at", dep.created_at.to_rfc3339().as_str())
                    .map_err(|e| Error::Other(format!("Failed to set dep created_at: {}", e)))?;
                if let Some(ref by) = dep.created_by {
                    doc.put(&dep_obj, "created_by", by.as_str()).map_err(|e| {
                        Error::Other(format!("Failed to set dep created_by: {}", e))
                    })?;
                }
            }
        }

        Ok(doc)
    }

    /// Save a single issue to its automerge file
    fn save_issue(&self, issue: &Issue) -> Result<()> {
        let mut doc = self.issue_to_doc(issue)?;
        let bytes = doc.save();

        let path = self.issue_path(&issue.id);
        let mut file = File::create(&path)?;
        file.write_all(&bytes)?;

        Ok(())
    }

    /// Regenerate ISSUES.md from current state
    pub fn regenerate_issues_md(&self) -> Result<()> {
        let mut content = String::from("# Issues\n\n");

        // Collect and sort issues
        let mut open: Vec<_> = self
            .issues
            .values()
            .filter(|i| i.status.is_open())
            .collect();
        let mut closed: Vec<_> = self
            .issues
            .values()
            .filter(|i| i.status.is_closed() && i.status != crate::Status::Tombstone)
            .collect();

        // Sort by priority, then by created_at
        open.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        closed.sort_by(|a, b| b.closed_at.cmp(&a.closed_at));

        // Open issues
        if !open.is_empty() {
            content.push_str("## Open\n\n");
            for issue in &open {
                content.push_str(&format!(
                    "### [{}] {} (P{}, {})\n",
                    issue.id, issue.title, issue.priority, issue.issue_type
                ));
                if let Some(ref desc) = issue.description {
                    // Truncate long descriptions
                    let desc_preview: String = desc.lines().take(5).collect::<Vec<_>>().join("\n");
                    content.push_str(&desc_preview);
                    if desc.lines().count() > 5 {
                        content.push_str("\n...\n");
                    }
                    content.push('\n');
                }
                content.push('\n');
            }
        }

        // Closed issues
        if !closed.is_empty() {
            content.push_str("## Closed\n\n");
            for issue in &closed {
                let closed_date = issue
                    .closed_at
                    .map(|dt| dt.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                content.push_str(&format!(
                    "- [{}] {} (closed {})\n",
                    issue.id, issue.title, closed_date
                ));
            }
        }

        if open.is_empty() && closed.is_empty() {
            content.push_str("No issues yet.\n");
        }

        fs::write(self.issues_md_path(), content)?;
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

        // Ensure crdt directory exists
        fs::create_dir_all(self.crdt_dir())?;

        self.save_issue(&issue)?;
        self.issues.insert(issue.id.clone(), issue);
        self.regenerate_issues_md()?;
        Ok(())
    }

    /// Update an existing issue
    pub fn update(&mut self, issue: Issue) -> Result<()> {
        if !self.issues.contains_key(&issue.id) {
            return Err(Error::NotFound(issue.id));
        }
        self.save_issue(&issue)?;
        self.issues.insert(issue.id.clone(), issue);
        self.regenerate_issues_md()?;
        Ok(())
    }

    /// Delete an issue (tombstone)
    pub fn delete(&mut self, id: &str, by: Option<String>, reason: Option<String>) -> Result<()> {
        let issue = self
            .issues
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        issue.delete(by, reason);
        let issue = issue.clone();
        self.save_issue(&issue)?;
        self.regenerate_issues_md()?;
        Ok(())
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
        let config = Config::load(&config_path)?;
        Ok(config.prefix)
    }

    /// Merge a conflicting automerge file
    ///
    /// Called when git detects a binary conflict on a .automerge file.
    /// Takes base, ours, theirs and produces a merged result.
    pub fn merge_conflict(_base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<Vec<u8>> {
        // Load all three versions
        let mut doc_ours = if ours.is_empty() {
            AutoCommit::new()
        } else {
            AutoCommit::load(ours)
                .map_err(|e| Error::Other(format!("Failed to load ours: {}", e)))?
        };

        let doc_theirs = if theirs.is_empty() {
            AutoCommit::new()
        } else {
            AutoCommit::load(theirs)
                .map_err(|e| Error::Other(format!("Failed to load theirs: {}", e)))?
        };

        // Merge theirs into ours - automerge handles this automatically
        doc_ours
            .merge(&mut doc_theirs.clone())
            .map_err(|e| Error::Other(format!("Failed to merge: {}", e)))?;

        Ok(doc_ours.save())
    }

    /// Check for and resolve any conflicting .automerge files in the crdt directory
    pub fn resolve_conflicts(&mut self) -> Result<Vec<String>> {
        let crdt_dir = self.crdt_dir();
        let mut resolved = Vec::new();

        // Look for git conflict markers (files like *.automerge.BASE, *.automerge.LOCAL, etc.)
        // Git creates these during a merge conflict for binary files

        for entry in fs::read_dir(&crdt_dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();

            // Check if this is a conflict file
            if name.ends_with(".automerge")
                && !name.contains(".BASE")
                && !name.contains(".LOCAL")
                && !name.contains(".REMOTE")
            {
                let base_path = crdt_dir.join(format!("{}.BASE", name));
                let local_path = crdt_dir.join(format!("{}.LOCAL", name));
                let remote_path = crdt_dir.join(format!("{}.REMOTE", name));

                if local_path.exists() && remote_path.exists() {
                    // We have a conflict to resolve
                    let base_bytes = if base_path.exists() {
                        fs::read(&base_path)?
                    } else {
                        Vec::new()
                    };
                    let local_bytes = fs::read(&local_path)?;
                    let remote_bytes = fs::read(&remote_path)?;

                    let merged = Self::merge_conflict(&base_bytes, &local_bytes, &remote_bytes)?;

                    // Write merged result
                    fs::write(&path, merged)?;

                    // Clean up conflict files
                    let _ = fs::remove_file(&base_path);
                    let _ = fs::remove_file(&local_path);
                    let _ = fs::remove_file(&remote_path);

                    resolved.push(name.to_string());
                }
            }
        }

        // Reload issues after resolving conflicts
        if !resolved.is_empty() {
            self.issues.clear();
            self.load()?;
            self.regenerate_issues_md()?;
        }

        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_roundtrip() {
        let store = CrdtStore {
            root: PathBuf::from("/tmp"),
            issues: HashMap::new(),
        };

        let mut issue = Issue::new("test-123".to_string(), "Test Issue".to_string());
        issue.description = Some("A test description".to_string());
        issue.priority = 1;
        issue.labels = vec!["bug".to_string(), "urgent".to_string()];

        let doc = store.issue_to_doc(&issue).unwrap();
        let roundtrip = store.doc_to_issue(&doc).unwrap();

        assert_eq!(issue.id, roundtrip.id);
        assert_eq!(issue.title, roundtrip.title);
        assert_eq!(issue.description, roundtrip.description);
        assert_eq!(issue.priority, roundtrip.priority);
        assert_eq!(issue.labels, roundtrip.labels);
    }
}
