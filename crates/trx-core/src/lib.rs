//! trx-core: Core library for the trx issue tracker
//!
//! Provides the data model, storage, and graph operations for a minimal
//! git-backed issue tracker. Storage is JSONL in `.trx/issues.jsonl`; legacy
//! v2 (Automerge) layouts are migrated transparently on the next mutation.

pub mod agent_ctx;
pub mod central;
pub mod config;
pub mod error;
pub mod events;
pub mod global_config;
pub mod graph;
pub mod id;
pub mod issue;
pub(crate) mod legacy_crdt;
pub mod migrate;
pub mod paths;
pub mod service;
pub mod store;
pub mod sync;
pub mod verification;

pub use agent_ctx::AgentCtx;
pub use central::{CentralMarker, CentralRepo, CentralStore, Checkout, RepoRecord};
pub use config::Config;
pub use error::Error;
pub use events::{
    Event, EventAction, EventLog, FieldChange, SessionSummary, diff_issue, enrich_issue,
    summarize_sessions,
};
pub use global_config::{DefaultMode, GlobalConfig, MigratePolicy, RootScan, StoreDef, SyncConfig};
pub use graph::IssueGraph;
pub use id::generate_id;
pub use issue::{Dependency, DependencyType, Issue, IssueType, Status};
pub use migrate::{MigrateOptions, MigrateReport, migrate_repo, scan_for_ledgers};
pub use service::{ServiceManager, ServiceStatus};
pub use store::{Store, TRX_GITATTRIBUTES_LINES};
pub use sync::{SyncOutcome, SyncState, SyncStatus};
pub use verification::{
    CloseGateReport, VerificationCheck, VerificationConfig, VerificationRun, VerificationStatus,
    VerificationStore, evaluate_close_gate, latest_run,
};

/// Result type for trx operations
pub type Result<T> = std::result::Result<T, Error>;
