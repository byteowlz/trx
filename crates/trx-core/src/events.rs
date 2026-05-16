//! Append-only event log: `.trx/events.jsonl`.
//!
//! Each line is a self-contained JSON event recording a mutation to an issue
//! together with the AGENT_CTX context active at the time. The log is the
//! source of truth for "which session touched which issue, when, and what
//! they did." Append-only writes never conflict (two writers create distinct
//! event ids), so no merge logic is needed beyond concatenation.

use crate::{AgentCtx, Error, Issue, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const EVENTS_FILE: &str = "events.jsonl";

/// What kind of mutation an event describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EventAction {
    Created,
    Updated,
    Closed,
    Reopened,
    Deleted,
    Restored,
    DepAdded,
    DepRemoved,
    SessionLinked,
}

impl std::fmt::Display for EventAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EventAction::Created => "created",
            EventAction::Updated => "updated",
            EventAction::Closed => "closed",
            EventAction::Reopened => "reopened",
            EventAction::Deleted => "deleted",
            EventAction::Restored => "restored",
            EventAction::DepAdded => "dep_added",
            EventAction::DepRemoved => "dep_removed",
            EventAction::SessionLinked => "session_linked",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for EventAction {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "created" => Ok(EventAction::Created),
            "updated" => Ok(EventAction::Updated),
            "closed" => Ok(EventAction::Closed),
            "reopened" => Ok(EventAction::Reopened),
            "deleted" => Ok(EventAction::Deleted),
            "restored" => Ok(EventAction::Restored),
            "dep_added" | "dep-added" => Ok(EventAction::DepAdded),
            "dep_removed" | "dep-removed" => Ok(EventAction::DepRemoved),
            "session_linked" | "session-linked" => Ok(EventAction::SessionLinked),
            _ => Err(Error::Other(format!("unknown event action: {}", s))),
        }
    }
}

/// A single field change recorded on an `Updated` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldChange {
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Value>,
}

/// A persisted event. Identity fields mirror the AGENT_CTX contract; missing
/// fields mean the variable was not set in that process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub issue_id: String,
    pub action: EventAction,
    pub timestamp: DateTime<Utc>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FieldChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Event {
    /// Build an event for `(issue_id, action)` with AGENT_CTX context applied.
    pub fn new(issue_id: impl Into<String>, action: EventAction, ctx: &AgentCtx) -> Self {
        Self {
            id: generate_event_id(),
            issue_id: issue_id.into(),
            action,
            timestamp: Utc::now(),
            user_id: ctx.user_id.clone(),
            platform: ctx.platform.clone(),
            platform_version: ctx.platform_version.clone(),
            harness: ctx.harness.clone(),
            platform_session_id: ctx.platform_session_id.clone(),
            harness_session_id: ctx.harness_session_id.clone(),
            session_name: ctx.session_name.clone(),
            workspace_id: ctx.workspace_id.clone(),
            model: ctx.model.clone(),
            request_id: ctx.request_id.clone(),
            correlation_id: ctx.correlation_id.clone(),
            changes: Vec::new(),
            note: None,
        }
    }

    /// Attach field changes describing what mutated.
    pub fn with_changes(mut self, changes: Vec<FieldChange>) -> Self {
        self.changes = changes;
        self
    }

    /// Attach a free-form note (e.g. close reason).
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// True when this event was tagged with the given session id (platform or
    /// harness). Used by `--session` filters.
    pub fn matches_session(&self, session_id: &str) -> bool {
        self.platform_session_id.as_deref() == Some(session_id)
            || self.harness_session_id.as_deref() == Some(session_id)
    }

    /// Key used to group events into "sessions" for visualization. Prefers
    /// platform session id, then harness session id; returns `None` when the
    /// event has neither (in which case callers typically render the event
    /// under an "unattributed" bucket).
    pub fn session_key(&self) -> Option<&str> {
        self.platform_session_id
            .as_deref()
            .or(self.harness_session_id.as_deref())
    }
}

/// Aggregated view of one session derived from the event log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub event_count: usize,
    /// Issue ids touched by this session, in first-seen order.
    pub issue_ids: Vec<String>,
}

/// Group `events` by session key (platform_session_id, falling back to
/// harness_session_id) and produce one summary per session. Events with no
/// session id are bucketed under `"-"`. Sessions are returned sorted by their
/// most-recent activity (newest first).
pub fn summarize_sessions(events: &[Event]) -> Vec<SessionSummary> {
    use std::collections::BTreeMap;

    let mut buckets: BTreeMap<String, SessionSummary> = BTreeMap::new();
    for ev in events {
        let key = ev.session_key().unwrap_or("-").to_string();
        let entry = buckets
            .entry(key.clone())
            .or_insert_with(|| SessionSummary {
                session_id: key.clone(),
                session_name: ev.session_name.clone(),
                user_id: ev.user_id.clone(),
                harness: ev.harness.clone(),
                platform: ev.platform.clone(),
                model: ev.model.clone(),
                first_at: ev.timestamp,
                last_at: ev.timestamp,
                event_count: 0,
                issue_ids: Vec::new(),
            });
        // Fill any field that was missing earlier — later events may carry it.
        if entry.session_name.is_none() {
            entry.session_name = ev.session_name.clone();
        }
        if entry.user_id.is_none() {
            entry.user_id = ev.user_id.clone();
        }
        if entry.harness.is_none() {
            entry.harness = ev.harness.clone();
        }
        if entry.platform.is_none() {
            entry.platform = ev.platform.clone();
        }
        if entry.model.is_none() {
            entry.model = ev.model.clone();
        }
        if ev.timestamp < entry.first_at {
            entry.first_at = ev.timestamp;
        }
        if ev.timestamp > entry.last_at {
            entry.last_at = ev.timestamp;
        }
        entry.event_count += 1;
        if !entry.issue_ids.iter().any(|i| i == &ev.issue_id) {
            entry.issue_ids.push(ev.issue_id.clone());
        }
    }

    let mut out: Vec<SessionSummary> = buckets.into_values().collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.last_at));
    out
}

/// Compute the diff between `before` and `after` for the user-mutable fields
/// that make sense to log. The set is intentionally narrow — `updated_at` and
/// internal flags are excluded.
pub fn diff_issue(before: &Issue, after: &Issue) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    macro_rules! cmp_field {
        ($field:ident) => {
            if before.$field != after.$field {
                changes.push(FieldChange {
                    field: stringify!($field).to_string(),
                    from: serde_json::to_value(&before.$field).ok(),
                    to: serde_json::to_value(&after.$field).ok(),
                });
            }
        };
    }
    cmp_field!(title);
    cmp_field!(description);
    cmp_field!(status);
    cmp_field!(priority);
    cmp_field!(issue_type);
    cmp_field!(labels);
    cmp_field!(assignee);
    cmp_field!(notes);
    cmp_field!(close_reason);
    cmp_field!(sessions);
    changes
}

/// Append-only event log at `.trx/events.jsonl`.
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    /// Construct a log handle for the given `.trx/` directory. Does not create
    /// the file — `append` does that lazily.
    pub fn at(trx_dir: &Path) -> Self {
        Self {
            path: trx_dir.join(EVENTS_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a single event. Open-append-flush-fsync each call: the cost is
    /// negligible for an interactive CLI and gives durability guarantees that
    /// match issue saves.
    pub fn append(&self, event: &Event) -> Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(event)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    }

    /// Read all events in file order. Malformed lines are skipped with a
    /// warning rather than aborting, so a partial-write tail can't lock out
    /// the rest of the log.
    pub fn read_all(&self) -> Result<Vec<Event>> {
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
            match serde_json::from_str::<Event>(&line) {
                Ok(ev) => out.push(ev),
                Err(e) => {
                    eprintln!(
                        "warning: skipping malformed event at {}:{}: {}",
                        self.path.display(),
                        lineno + 1,
                        e
                    );
                }
            }
        }
        Ok(out)
    }
}

fn generate_event_id() -> String {
    use sha2::{Digest, Sha256};
    let uuid = uuid::Uuid::new_v4();
    let ts = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(uuid.as_bytes());
    hasher.update(ts.to_le_bytes());
    let hash = hasher.finalize();
    let encoded = base32::encode(base32::Alphabet::Crockford, &hash[..6])
        .to_lowercase()
        .chars()
        .take(8)
        .collect::<String>();
    format!("evt_{}", encoded)
}

/// Apply AGENT_CTX-derived enrichments to an issue: fill `created_by` if
/// unset, append session ids (deduped) to `sessions`. Mutates in place.
/// Returns true when at least one field changed.
pub fn enrich_issue(issue: &mut Issue, ctx: &AgentCtx) -> bool {
    let mut touched = false;
    if issue.created_by.is_none()
        && let Some(uid) = &ctx.user_id
    {
        issue.created_by = Some(uid.clone());
        touched = true;
    }
    for session in ctx.session_ids() {
        if !issue.sessions.iter().any(|s| s == &session) {
            issue.sessions.push(session);
            touched = true;
        }
    }
    touched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Issue;
    use tempfile::TempDir;

    fn ctx(user: &str, platform_session: &str) -> AgentCtx {
        AgentCtx {
            user_id: Some(user.into()),
            platform_session_id: Some(platform_session.into()),
            ..Default::default()
        }
    }

    #[test]
    fn append_then_read_round_trips() {
        let dir = TempDir::new().unwrap();
        let log = EventLog::at(dir.path());

        let event = Event::new("trx-abc1", EventAction::Created, &ctx("u1", "s1"));
        log.append(&event).unwrap();
        let event2 =
            Event::new("trx-abc1", EventAction::Closed, &ctx("u1", "s1")).with_note("done");
        log.append(&event2).unwrap();

        let read = log.read_all().unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].action, EventAction::Created);
        assert_eq!(read[1].action, EventAction::Closed);
        assert_eq!(read[1].note.as_deref(), Some("done"));
        assert_eq!(read[0].user_id.as_deref(), Some("u1"));
    }

    #[test]
    fn read_all_skips_blank_and_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let log = EventLog::at(dir.path());
        let event = Event::new("trx-abc1", EventAction::Created, &ctx("u", "s"));
        log.append(&event).unwrap();

        // Inject a blank and a malformed line at the end.
        let path = log.path().to_path_buf();
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"\n").unwrap();
        f.write_all(b"{not json}\n").unwrap();

        let read = log.read_all().unwrap();
        assert_eq!(read.len(), 1);
    }

    #[test]
    fn diff_detects_status_and_priority_changes() {
        let mut before = Issue::new("trx-1".into(), "x".into());
        before.priority = 2;
        before.status = crate::Status::Open;

        let mut after = before.clone();
        after.priority = 1;
        after.status = crate::Status::InProgress;

        let changes = diff_issue(&before, &after);
        let fields: Vec<&str> = changes.iter().map(|c| c.field.as_str()).collect();
        assert!(fields.contains(&"priority"));
        assert!(fields.contains(&"status"));
        assert_eq!(changes.len(), 2);
    }

    #[test]
    fn diff_returns_empty_for_unchanged() {
        let before = Issue::new("trx-1".into(), "x".into());
        let after = before.clone();
        assert!(diff_issue(&before, &after).is_empty());
    }

    #[test]
    fn enrich_issue_fills_created_by_and_sessions() {
        let mut issue = Issue::new("trx-1".into(), "x".into());
        let ctx = AgentCtx {
            user_id: Some("u_42".into()),
            platform_session_id: Some("s_a".into()),
            harness_session_id: Some("s_b".into()),
            ..Default::default()
        };
        assert!(enrich_issue(&mut issue, &ctx));
        assert_eq!(issue.created_by.as_deref(), Some("u_42"));
        assert_eq!(issue.sessions, vec!["s_a", "s_b"]);

        // Idempotent: applying the same context again is a no-op.
        assert!(!enrich_issue(&mut issue, &ctx));
    }

    #[test]
    fn enrich_issue_does_not_overwrite_existing_created_by() {
        let mut issue = Issue::new("trx-1".into(), "x".into());
        issue.created_by = Some("alice".into());
        let ctx = AgentCtx {
            user_id: Some("u_42".into()),
            ..Default::default()
        };
        enrich_issue(&mut issue, &ctx);
        assert_eq!(issue.created_by.as_deref(), Some("alice"));
    }

    #[test]
    fn summarize_sessions_groups_and_orders_by_recency() {
        let mut e1 = Event::new("trx-1", EventAction::Created, &ctx("u", "s_old"));
        e1.timestamp = chrono::DateTime::parse_from_rfc3339("2026-01-01T10:00:00Z")
            .unwrap()
            .into();
        let mut e2 = Event::new("trx-2", EventAction::Updated, &ctx("u", "s_old"));
        e2.timestamp = chrono::DateTime::parse_from_rfc3339("2026-01-01T11:00:00Z")
            .unwrap()
            .into();
        let mut e3 = Event::new("trx-3", EventAction::Closed, &ctx("u", "s_new"));
        e3.timestamp = chrono::DateTime::parse_from_rfc3339("2026-02-01T09:00:00Z")
            .unwrap()
            .into();

        let summary = summarize_sessions(&[e1, e2, e3]);
        assert_eq!(summary.len(), 2);
        // Newest-first ordering.
        assert_eq!(summary[0].session_id, "s_new");
        assert_eq!(summary[1].session_id, "s_old");

        let old = &summary[1];
        assert_eq!(old.event_count, 2);
        assert_eq!(old.issue_ids, vec!["trx-1", "trx-2"]);
        assert_eq!(
            old.first_at.format("%Y-%m-%d %H:%M").to_string(),
            "2026-01-01 10:00"
        );
        assert_eq!(
            old.last_at.format("%Y-%m-%d %H:%M").to_string(),
            "2026-01-01 11:00"
        );
    }

    #[test]
    fn summarize_sessions_uses_dash_for_missing_session() {
        let ev = Event::new("trx-1", EventAction::Created, &AgentCtx::default());
        let summary = summarize_sessions(&[ev]);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].session_id, "-");
    }

    #[test]
    fn matches_session_checks_both_ids() {
        let event = Event::new(
            "trx-1",
            EventAction::Updated,
            &AgentCtx {
                platform_session_id: Some("p1".into()),
                harness_session_id: Some("h1".into()),
                ..Default::default()
            },
        );
        assert!(event.matches_session("p1"));
        assert!(event.matches_session("h1"));
        assert!(!event.matches_session("other"));
    }
}
