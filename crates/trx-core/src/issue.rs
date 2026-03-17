//! Issue data model for trx
//!
//! Minimal issue structure (~15 fields vs beads' ~117).
//! Designed for beads-viewer compatibility.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Issue status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Open,
    InProgress,
    Blocked,
    Closed,
    /// Soft-deleted, preserved for conflict-free merges
    Tombstone,
}

impl Status {
    pub fn is_open(&self) -> bool {
        matches!(self, Status::Open | Status::InProgress | Status::Blocked)
    }

    pub fn is_closed(&self) -> bool {
        matches!(self, Status::Closed | Status::Tombstone)
    }
}

impl std::str::FromStr for Status {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "open" => Ok(Status::Open),
            "in_progress" | "in-progress" | "inprogress" => Ok(Status::InProgress),
            "blocked" => Ok(Status::Blocked),
            "closed" => Ok(Status::Closed),
            "tombstone" => Ok(Status::Tombstone),
            _ => Err(crate::Error::InvalidStatus(s.to_string())),
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Open => write!(f, "open"),
            Status::InProgress => write!(f, "in_progress"),
            Status::Blocked => write!(f, "blocked"),
            Status::Closed => write!(f, "closed"),
            Status::Tombstone => write!(f, "tombstone"),
        }
    }
}

/// Issue type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    Bug,
    Feature,
    #[default]
    Task,
    Epic,
    Chore,
}

impl std::str::FromStr for IssueType {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bug" => Ok(IssueType::Bug),
            "feature" => Ok(IssueType::Feature),
            "task" => Ok(IssueType::Task),
            "epic" => Ok(IssueType::Epic),
            "chore" => Ok(IssueType::Chore),
            _ => Err(crate::Error::InvalidType(s.to_string())),
        }
    }
}

impl std::fmt::Display for IssueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueType::Bug => write!(f, "bug"),
            IssueType::Feature => write!(f, "feature"),
            IssueType::Task => write!(f, "task"),
            IssueType::Epic => write!(f, "epic"),
            IssueType::Chore => write!(f, "chore"),
        }
    }
}

/// Dependency type (beads-viewer compatible)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DependencyType {
    /// This issue blocks another
    #[default]
    Blocks,
    /// Parent-child relationship
    ParentChild,
    /// Related but not blocking
    Related,
}

impl std::fmt::Display for DependencyType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DependencyType::Blocks => write!(f, "blocks"),
            DependencyType::ParentChild => write!(f, "parent_child"),
            DependencyType::Related => write!(f, "related"),
        }
    }
}

impl std::str::FromStr for DependencyType {
    type Err = crate::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "blocks" => Ok(DependencyType::Blocks),
            "parent_child" | "parent-child" => Ok(DependencyType::ParentChild),
            "related" => Ok(DependencyType::Related),
            _ => Err(crate::Error::Other(format!(
                "Invalid dependency type: {}",
                s
            ))),
        }
    }
}

/// Dependency between issues (beads-viewer compatible format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    /// The issue that has this dependency
    pub issue_id: String,
    /// The issue this depends on
    pub depends_on_id: String,
    /// Type of dependency
    #[serde(rename = "type")]
    pub dep_type: DependencyType,
    /// When the dependency was created
    pub created_at: DateTime<Utc>,
    /// Who created the dependency
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

/// Core issue structure
///
/// Designed to be minimal but beads-viewer compatible.
/// ~15 fields vs beads' ~117.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Unique identifier (trx-xxxx or trx-xxxx.N for children)
    pub id: String,

    /// Issue title
    pub title: String,

    /// Detailed description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Current status
    pub status: Status,

    /// Priority (0=critical, 1=high, 2=medium, 3=low, 4=backlog)
    pub priority: u8,

    /// Issue type
    pub issue_type: IssueType,

    /// Labels/tags
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,

    /// When the issue was created
    pub created_at: DateTime<Utc>,

    /// When the issue was last updated
    pub updated_at: DateTime<Utc>,

    /// When the issue was closed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,

    /// When the issue was deleted (tombstone)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,

    /// Dependencies (beads-viewer compatible format)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<Dependency>,

    /// Who created the issue
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,

    /// Reason for closing
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,

    /// Assignee
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,

    /// Notes (additional context)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    // Beads compatibility fields
    /// Original type before tombstone (beads compat)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_type: Option<String>,

    /// Who deleted the issue (beads compat)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_by: Option<String>,

    /// Reason for deletion (beads compat)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete_reason: Option<String>,
}

impl Issue {
    /// Create a new issue with minimal required fields
    pub fn new(id: String, title: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            title,
            description: None,
            status: Status::Open,
            priority: 2,
            issue_type: IssueType::Task,
            labels: Vec::new(),
            created_at: now,
            updated_at: now,
            closed_at: None,
            deleted_at: None,
            dependencies: Vec::new(),
            created_by: None,
            close_reason: None,
            assignee: None,
            notes: None,
            original_type: None,
            deleted_by: None,
            delete_reason: None,
        }
    }

    /// Check if this issue is blocking other issues
    pub fn is_blocking(&self) -> bool {
        !self.dependencies.is_empty()
            && self
                .dependencies
                .iter()
                .any(|d| d.dep_type == DependencyType::Blocks)
    }

    /// Check if this issue is blocked by open dependencies
    pub fn is_blocked_by(&self, open_issues: &[&Issue]) -> bool {
        self.dependencies.iter().any(|dep| {
            dep.dep_type == DependencyType::Blocks
                && open_issues
                    .iter()
                    .any(|i| i.id == dep.depends_on_id && i.status.is_open())
        })
    }

    /// Get parent ID if this is a child issue
    pub fn parent_id(&self) -> Option<&str> {
        crate::id::get_parent_id(&self.id)
    }

    /// Check if this is a child issue
    pub fn is_child(&self) -> bool {
        crate::id::is_child_id(&self.id)
    }

    /// Mark as closed
    pub fn close(&mut self, reason: Option<String>) {
        self.status = Status::Closed;
        self.closed_at = Some(Utc::now());
        self.updated_at = Utc::now();
        self.close_reason = reason;
    }

    /// Mark as deleted (tombstone)
    pub fn delete(&mut self, by: Option<String>, reason: Option<String>) {
        self.original_type = Some(self.issue_type.to_string());
        self.status = Status::Tombstone;
        self.deleted_at = Some(Utc::now());
        self.updated_at = Utc::now();
        self.deleted_by = by;
        self.delete_reason = reason;
    }

    /// Add a dependency
    ///
    /// If a dependency to the same `depends_on_id` already exists, it is preserved
    /// (no duplicate created). Returns true if the dependency was added.
    pub fn add_dependency(&mut self, depends_on_id: String, dep_type: DependencyType) -> bool {
        // Check for existing dependency to the same target
        if self
            .dependencies
            .iter()
            .any(|d| d.depends_on_id == depends_on_id)
        {
            return false;
        }
        let dep = Dependency {
            issue_id: self.id.clone(),
            depends_on_id,
            dep_type,
            created_at: Utc::now(),
            created_by: None,
        };
        self.dependencies.push(dep);
        self.updated_at = Utc::now();
        true
    }

    /// Remove a dependency
    pub fn remove_dependency(&mut self, depends_on_id: &str) {
        self.dependencies
            .retain(|d| d.depends_on_id != depends_on_id);
        self.updated_at = Utc::now();
    }

    /// Get blocking dependency IDs
    pub fn blocking_ids(&self) -> Vec<&str> {
        self.dependencies
            .iter()
            .filter(|d| d.dep_type == DependencyType::Blocks)
            .map(|d| d.depends_on_id.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_dependency_prevents_duplicates() {
        let mut issue = Issue::new("trx-001".into(), "Test".into());
        assert!(issue.add_dependency("trx-parent".into(), DependencyType::ParentChild));
        // Second add to same target should be rejected
        assert!(!issue.add_dependency("trx-parent".into(), DependencyType::Blocks));
        assert_eq!(issue.dependencies.len(), 1);
        assert_eq!(issue.dependencies[0].dep_type, DependencyType::ParentChild);
    }

    #[test]
    fn test_parent_child_survives_dep_add_to_different_target() {
        let mut issue = Issue::new("trx-001".into(), "Test".into());
        issue.add_dependency("trx-parent".into(), DependencyType::ParentChild);
        issue.add_dependency("trx-blocker".into(), DependencyType::Blocks);

        assert_eq!(issue.dependencies.len(), 2);
        assert_eq!(issue.dependencies[0].dep_type, DependencyType::ParentChild);
        assert_eq!(issue.dependencies[1].dep_type, DependencyType::Blocks);
    }

    #[test]
    fn test_dependency_type_display_roundtrip() {
        for dt in [
            DependencyType::Blocks,
            DependencyType::ParentChild,
            DependencyType::Related,
        ] {
            let s = dt.to_string();
            let parsed: DependencyType = s.parse().unwrap();
            assert_eq!(dt, parsed);
        }
    }

    #[test]
    fn test_dependency_type_parse_accepts_hyphen() {
        let dt: DependencyType = "parent-child".parse().unwrap();
        assert_eq!(dt, DependencyType::ParentChild);
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [P{}] [{}] {} - {}",
            self.id, self.priority, self.issue_type, self.status, self.title
        )
    }
}
