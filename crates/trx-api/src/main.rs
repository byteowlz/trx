//! trx-api: REST API server for trx issue tracker
//!
//! Provides HTTP endpoints for CRUD operations on issues.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use trx_core::{Issue, IssueType, Status, Store, generate_id};

/// Shared application state
struct AppState {
    store: RwLock<Store>,
}

impl AppState {
    fn read_store(&self) -> std::sync::RwLockReadGuard<'_, Store> {
        // RwLock::read only fails if a thread panicked while holding the lock,
        // which is unrecoverable. Panic is appropriate here.
        self.store.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write_store(&self) -> std::sync::RwLockWriteGuard<'_, Store> {
        self.store.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// Request to create a new issue
#[derive(Debug, Deserialize)]
struct CreateIssueRequest {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<u8>,
    #[serde(default)]
    issue_type: Option<String>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
}

/// Request to update an issue
#[derive(Debug, Deserialize)]
struct UpdateIssueRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<u8>,
    #[serde(default)]
    issue_type: Option<String>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

/// Request to close an issue
#[derive(Debug, Deserialize)]
struct CloseIssueRequest {
    #[serde(default)]
    reason: Option<String>,
}

/// Request to delete an issue
#[derive(Debug, Deserialize)]
struct DeleteIssueRequest {
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// Query parameters for listing issues
#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    issue_type: Option<String>,
    #[serde(default)]
    priority: Option<u8>,
    #[serde(default)]
    include_tombstones: Option<bool>,
}

/// API response wrapper
#[derive(Debug, Serialize)]
struct ApiResponse<T> {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl<T> ApiResponse<T> {
    fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

/// Health check endpoint
async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// List all issues
async fn list_issues(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> impl IntoResponse {
    let store = state.read_store();
    let include_tombstones = query.include_tombstones.unwrap_or(false);
    let mut issues: Vec<_> = store
        .list(include_tombstones)
        .into_iter()
        .cloned()
        .collect();

    // Filter by status
    if let Some(status_str) = &query.status
        && let Ok(status) = status_str.parse::<Status>()
    {
        issues.retain(|i| i.status == status);
    }

    // Filter by type
    if let Some(type_str) = &query.issue_type
        && let Ok(issue_type) = type_str.parse::<IssueType>()
    {
        issues.retain(|i| i.issue_type == issue_type);
    }

    // Filter by priority
    if let Some(priority) = query.priority {
        issues.retain(|i| i.priority == priority);
    }

    // Sort by priority, then by created_at
    issues.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });

    (StatusCode::OK, Json(ApiResponse::ok(issues)))
}

/// List open issues (unblocked)
async fn list_ready(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let store = state.read_store();
    let all_open: Vec<_> = store.list_open();
    let ready: Vec<_> = all_open
        .iter()
        .filter(|issue| !issue.is_blocked_by(&all_open))
        .copied()
        .cloned()
        .collect();

    (StatusCode::OK, Json(ApiResponse::ok(ready)))
}

/// Get a single issue by ID
async fn get_issue(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let store = state.read_store();
    match store.get(&id) {
        Some(issue) => (StatusCode::OK, Json(ApiResponse::ok(issue.clone()))),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<Issue>::err(format!("Issue {} not found", id))),
        ),
    }
}

/// Create a new issue
async fn create_issue(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateIssueRequest>,
) -> impl IntoResponse {
    let mut store = state.write_store();

    // Get prefix for ID generation
    let prefix = store.prefix().unwrap_or_else(|_| "trx".to_string());

    // Generate ID (either root or child)
    let id = if let Some(parent_id) = &req.parent_id {
        // Verify parent exists
        if store.get(parent_id).is_none() {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Issue>::err(format!(
                    "Parent issue {} not found",
                    parent_id
                ))),
            );
        }
        let child_num = store.next_child_num(parent_id);
        format!("{}.{}", parent_id, child_num)
    } else {
        generate_id(&prefix)
    };

    // Create the issue
    let mut issue = Issue::new(id, req.title);

    if let Some(desc) = req.description {
        issue.description = Some(desc);
    }
    if let Some(priority) = req.priority {
        issue.priority = priority.min(4);
    }
    if let Some(type_str) = req.issue_type
        && let Ok(t) = type_str.parse::<IssueType>()
    {
        issue.issue_type = t;
    }
    if let Some(labels) = req.labels {
        issue.labels = labels;
    }
    if let Some(assignee) = req.assignee {
        issue.assignee = Some(assignee);
    }

    // Add parent-child dependency if this is a child
    if let Some(parent_id) = req.parent_id {
        issue.add_dependency(parent_id, trx_core::DependencyType::ParentChild);
    }

    match store.create(issue.clone()) {
        Ok(()) => (StatusCode::CREATED, Json(ApiResponse::ok(issue))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<Issue>::err(e.to_string())),
        ),
    }
}

/// Update an existing issue
async fn update_issue(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateIssueRequest>,
) -> impl IntoResponse {
    let mut store = state.write_store();

    let issue = match store.get_mut(&id) {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<Issue>::err(format!("Issue {} not found", id))),
            );
        }
    };

    // Apply updates
    if let Some(title) = req.title {
        issue.title = title;
    }
    if let Some(desc) = req.description {
        issue.description = Some(desc);
    }
    if let Some(status_str) = req.status
        && let Ok(status) = status_str.parse::<Status>()
    {
        issue.status = status;
    }
    if let Some(priority) = req.priority {
        issue.priority = priority.min(4);
    }
    if let Some(type_str) = req.issue_type
        && let Ok(t) = type_str.parse::<IssueType>()
    {
        issue.issue_type = t;
    }
    if let Some(labels) = req.labels {
        issue.labels = labels;
    }
    if let Some(assignee) = req.assignee {
        issue.assignee = Some(assignee);
    }
    if let Some(notes) = req.notes {
        issue.notes = Some(notes);
    }

    issue.updated_at = chrono::Utc::now();
    let updated = issue.clone();

    match store.save() {
        Ok(()) => (StatusCode::OK, Json(ApiResponse::ok(updated))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<Issue>::err(e.to_string())),
        ),
    }
}

/// Close an issue
async fn close_issue(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<CloseIssueRequest>,
) -> impl IntoResponse {
    let mut store = state.write_store();

    let issue = match store.get_mut(&id) {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<Issue>::err(format!("Issue {} not found", id))),
            );
        }
    };

    issue.close(req.reason);
    let closed = issue.clone();

    match store.save() {
        Ok(()) => (StatusCode::OK, Json(ApiResponse::ok(closed))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<Issue>::err(e.to_string())),
        ),
    }
}

/// Delete an issue (tombstone)
async fn delete_issue(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    axum::extract::Query(req): axum::extract::Query<DeleteIssueRequest>,
) -> impl IntoResponse {
    let mut store = state.write_store();

    match store.delete(&id, req.by, req.reason) {
        Ok(()) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serde_json::json!({ "deleted": id }))),
        ),
        Err(trx_core::Error::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<serde_json::Value>::err(format!(
                "Issue {} not found",
                id
            ))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<serde_json::Value>::err(e.to_string())),
        ),
    }
}

/// Add a dependency to an issue
#[derive(Debug, Deserialize)]
struct AddDependencyRequest {
    depends_on: String,
    #[serde(default)]
    dep_type: Option<String>,
}

async fn add_dependency(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<AddDependencyRequest>,
) -> impl IntoResponse {
    let mut store = state.write_store();

    // Check that target exists
    if store.get(&req.depends_on).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Issue>::err(format!(
                "Dependency target {} not found",
                req.depends_on
            ))),
        );
    }

    let issue = match store.get_mut(&id) {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<Issue>::err(format!("Issue {} not found", id))),
            );
        }
    };

    let dep_type = req
        .dep_type
        .map(|s| match s.to_lowercase().as_str() {
            "blocks" => trx_core::DependencyType::Blocks,
            "parent_child" | "parent-child" => trx_core::DependencyType::ParentChild,
            "related" => trx_core::DependencyType::Related,
            _ => trx_core::DependencyType::Blocks,
        })
        .unwrap_or(trx_core::DependencyType::Blocks);

    issue.add_dependency(req.depends_on, dep_type);
    let updated = issue.clone();

    match store.save() {
        Ok(()) => (StatusCode::OK, Json(ApiResponse::ok(updated))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<Issue>::err(e.to_string())),
        ),
    }
}

/// Remove a dependency from an issue
async fn remove_dependency(
    State(state): State<Arc<AppState>>,
    Path((id, dep_id)): Path<(String, String)>,
) -> impl IntoResponse {
    let mut store = state.write_store();

    let issue = match store.get_mut(&id) {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<Issue>::err(format!("Issue {} not found", id))),
            );
        }
    };

    issue.remove_dependency(&dep_id);
    let updated = issue.clone();

    match store.save() {
        Ok(()) => (StatusCode::OK, Json(ApiResponse::ok(updated))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<Issue>::err(e.to_string())),
        ),
    }
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    // Open the store
    let store = Store::open().map_err(|e| anyhow::anyhow!("Failed to open store: {}", e))?;

    let state = Arc::new(AppState {
        store: RwLock::new(store),
    });

    // Build router
    let app = Router::new()
        .route("/health", get(health))
        .route("/issues", get(list_issues).post(create_issue))
        .route("/issues/ready", get(list_ready))
        .route(
            "/issues/{id}",
            get(get_issue).patch(update_issue).delete(delete_issue),
        )
        .route("/issues/{id}/close", post(close_issue))
        .route("/issues/{id}/dependencies", post(add_dependency))
        .route(
            "/issues/{id}/dependencies/{dep_id}",
            delete(remove_dependency),
        )
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Get port from env or default
    let port: u16 = std::env::var("TRX_API_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3847);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("Starting trx-api on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
