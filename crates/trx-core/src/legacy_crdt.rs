//! Read-only loader for the legacy v2 (Automerge) on-disk format.
//!
//! Used solely for transparent migration when an existing `.trx/crdt/`
//! directory is detected on `Store::open_at`. After issues are loaded into
//! memory, the JSONL store will rewrite them in canonical form and remove the
//! legacy `crdt/` directory on the next mutation. There is no write path.

use crate::{Dependency, DependencyType, Error, Issue, Result};
use automerge::{AutoCommit, ReadDoc};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

/// Load all issues from a legacy `.trx/crdt/` directory.
///
/// Files that fail to parse are skipped with a stderr warning rather than
/// aborting the migration — partial recovery is preferable to total loss.
pub fn load_issues(crdt_dir: &Path) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();
    if !crdt_dir.exists() {
        return Ok(issues);
    }

    for entry in fs::read_dir(crdt_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "automerge") {
            match load_issue_from_file(&path) {
                Ok(issue) => issues.push(issue),
                Err(e) => {
                    eprintln!(
                        "warning: skipping unreadable legacy CRDT file {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }
    }
    Ok(issues)
}

fn load_issue_from_file(path: &Path) -> Result<Issue> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;

    let doc = AutoCommit::load(&bytes)
        .map_err(|e| Error::Other(format!("Failed to load automerge doc: {}", e)))?;
    doc_to_issue(&doc)
}

fn get_str(doc: &AutoCommit, key: &str) -> Option<String> {
    doc.get(automerge::ROOT, key)
        .ok()
        .flatten()
        .and_then(|(v, _)| v.to_str().map(|s| s.to_string()))
}

fn get_u8(doc: &AutoCommit, key: &str) -> Option<u8> {
    doc.get(automerge::ROOT, key)
        .ok()
        .flatten()
        .and_then(|(v, _)| v.to_i64().map(|n| n as u8))
}

fn get_datetime(doc: &AutoCommit, key: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    get_str(doc, key).and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))
    })
}

fn doc_to_issue(doc: &AutoCommit) -> Result<Issue> {
    let id = get_str(doc, "id").ok_or_else(|| Error::Other("Missing id field".into()))?;
    let title = get_str(doc, "title").ok_or_else(|| Error::Other("Missing title field".into()))?;

    let mut issue = Issue::new(id, title);

    if let Some(desc) = get_str(doc, "description") {
        issue.description = Some(desc);
    }
    if let Some(status) = get_str(doc, "status") {
        issue.status = status.parse().unwrap_or_default();
    }
    if let Some(priority) = get_u8(doc, "priority") {
        issue.priority = priority;
    }
    if let Some(itype) = get_str(doc, "issue_type") {
        issue.issue_type = itype.parse().unwrap_or_default();
    }
    if let Some(created) = get_datetime(doc, "created_at") {
        issue.created_at = created;
    }
    if let Some(updated) = get_datetime(doc, "updated_at") {
        issue.updated_at = updated;
    }
    if let Some(closed) = get_datetime(doc, "closed_at") {
        issue.closed_at = Some(closed);
    }
    if let Some(deleted) = get_datetime(doc, "deleted_at") {
        issue.deleted_at = Some(deleted);
    }
    if let Some(assignee) = get_str(doc, "assignee") {
        issue.assignee = Some(assignee);
    }
    if let Some(reason) = get_str(doc, "close_reason") {
        issue.close_reason = Some(reason);
    }
    if let Some(notes) = get_str(doc, "notes") {
        issue.notes = Some(notes);
    }
    if let Some(created_by) = get_str(doc, "created_by") {
        issue.created_by = Some(created_by);
    }
    if let Some(deleted_by) = get_str(doc, "deleted_by") {
        issue.deleted_by = Some(deleted_by);
    }
    if let Some(delete_reason) = get_str(doc, "delete_reason") {
        issue.delete_reason = Some(delete_reason);
    }
    if let Some(original_type) = get_str(doc, "original_type") {
        issue.original_type = Some(original_type);
    }

    if let Ok(Some((_, list_id))) = doc.get(automerge::ROOT, "sessions") {
        let len = doc.length(&list_id);
        for i in 0..len {
            if let Ok(Some((v, _))) = doc.get(&list_id, i)
                && let Some(s) = v.to_str()
            {
                issue.sessions.push(s.to_string());
            }
        }
    }

    if let Ok(Some((_, list_id))) = doc.get(automerge::ROOT, "labels") {
        let len = doc.length(&list_id);
        for i in 0..len {
            if let Ok(Some((v, _))) = doc.get(&list_id, i)
                && let Some(s) = v.to_str()
            {
                issue.labels.push(s.to_string());
            }
        }
    }

    if let Ok(Some((_, deps_id))) = doc.get(automerge::ROOT, "dependencies") {
        let len = doc.length(&deps_id);
        for i in 0..len {
            if let Ok(Some((_, dep_obj))) = doc.get(&deps_id, i) {
                let field = |key: &str| -> Option<String> {
                    doc.get(&dep_obj, key)
                        .ok()
                        .flatten()
                        .and_then(|(v, _)| v.to_str().map(|s| s.to_string()))
                };
                let issue_id = field("issue_id");
                let depends_on_id = field("depends_on_id");
                let dep_type_str = field("type");
                let created_at_str = field("created_at");
                let created_by = field("created_by");

                if let (Some(issue_id), Some(depends_on_id)) = (issue_id, depends_on_id) {
                    let dep_type = dep_type_str
                        .and_then(|t| t.parse::<DependencyType>().ok())
                        .unwrap_or_default();
                    let created_at = created_at_str
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(chrono::Utc::now);
                    issue.dependencies.push(Dependency {
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
