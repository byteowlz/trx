//! trx-core: Core library for the trx issue tracker
//!
//! Provides the data model, storage, and graph operations for a minimal
//! git-backed issue tracker. Storage is JSONL in `.trx/issues.jsonl`; legacy
//! v2 (Automerge) layouts are migrated transparently on the next mutation.

pub mod agent_ctx;
pub mod config;
pub mod error;
pub mod events;
pub mod graph;
pub mod id;
pub mod issue;
pub(crate) mod legacy_crdt;
pub mod service;
pub mod store;

pub use agent_ctx::AgentCtx;
pub use config::Config;
pub use error::Error;
pub use events::{
    Event, EventAction, EventLog, FieldChange, SessionSummary, diff_issue, enrich_issue,
    summarize_sessions,
};
pub use graph::IssueGraph;
pub use id::generate_id;
pub use issue::{Dependency, DependencyType, Issue, IssueType, Status};
pub use service::{ServiceManager, ServiceStatus};
pub use store::Store;

/// Result type for trx operations
pub type Result<T> = std::result::Result<T, Error>;
