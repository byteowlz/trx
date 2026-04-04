//! trx-mcp: MCP server for trx issue tracker
//!
//! Provides MCP (Model Context Protocol) tools for AI agents to interact
//! with the trx issue tracker.

use anyhow::Context;
use mcp_server::router::RouterService;
use mcp_spec::ResourceContents;
use mcp_spec::content::Content;
use mcp_spec::handler::{PromptError, ResourceError, ToolError};
use mcp_spec::prompt::Prompt;
use mcp_spec::protocol::ServerCapabilities;
use mcp_spec::resource::Resource;
use mcp_spec::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use trx_core::{Issue, IssueType, Status, Store, generate_id};

/// MCP router for trx
#[derive(Clone)]
struct TrxMcpRouter {
    inner: Arc<TrxMcpInner>,
}

struct TrxMcpInner {
    store: RwLock<Store>,
    root_path: PathBuf,
}

// ============================================================================
// Argument types for tools
// ============================================================================

#[derive(Debug, Deserialize)]
struct ListIssuesArgs {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    issue_type: Option<String>,
    #[serde(default)]
    priority: Option<u8>,
    #[serde(default)]
    include_tombstones: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GetIssueArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CreateIssueArgs {
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

#[derive(Debug, Deserialize)]
struct UpdateIssueArgs {
    id: String,
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

#[derive(Debug, Deserialize)]
struct CloseIssueArgs {
    id: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeleteIssueArgs {
    id: String,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddDependencyArgs {
    id: String,
    depends_on: String,
    #[serde(default)]
    dep_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoveDependencyArgs {
    id: String,
    depends_on: String,
}

// ============================================================================
// Router implementation
// ============================================================================

impl TrxMcpRouter {
    fn new(store: Store, root_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(TrxMcpInner {
                store: RwLock::new(store),
                root_path,
            }),
        }
    }

    fn json_content(&self, uri: &str, value: Value) -> Result<Vec<Content>, ToolError> {
        let text = serde_json::to_string_pretty(&value)
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;
        Ok(vec![Content::resource(
            ResourceContents::TextResourceContents {
                uri: uri.to_string(),
                mime_type: Some("application/json".to_string()),
                text,
            },
        )])
    }

    // Tool: list issues
    fn tool_list(&self, args: ListIssuesArgs) -> Result<Vec<Content>, ToolError> {
        let store = self
            .inner
            .store
            .read()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let include_tombstones = args.include_tombstones.unwrap_or(false);
        let mut issues: Vec<_> = store
            .list(include_tombstones)
            .into_iter()
            .cloned()
            .collect();

        // Filter by status
        if let Some(status_str) = &args.status
            && let Ok(status) = status_str.parse::<Status>()
        {
            issues.retain(|i| i.status == status);
        }

        // Filter by type
        if let Some(type_str) = &args.issue_type
            && let Ok(issue_type) = type_str.parse::<IssueType>()
        {
            issues.retain(|i| i.issue_type == issue_type);
        }

        // Filter by priority
        if let Some(priority) = args.priority {
            issues.retain(|i| i.priority == priority);
        }

        // Sort by priority, then by created_at
        issues.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });

        self.json_content(
            "trx://tools/issues/list",
            json!({
                "count": issues.len(),
                "issues": issues,
            }),
        )
    }

    // Tool: list ready (unblocked) issues
    fn tool_ready(&self) -> Result<Vec<Content>, ToolError> {
        let store = self
            .inner
            .store
            .read()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let all_open: Vec<_> = store.list_open();
        let ready: Vec<_> = all_open
            .iter()
            .filter(|issue| !issue.is_blocked_by(&all_open))
            .copied()
            .cloned()
            .collect();

        self.json_content(
            "trx://tools/issues/ready",
            json!({
                "count": ready.len(),
                "issues": ready,
            }),
        )
    }

    // Tool: get single issue
    fn tool_get(&self, args: GetIssueArgs) -> Result<Vec<Content>, ToolError> {
        let store = self
            .inner
            .store
            .read()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let issue = store
            .get(&args.id)
            .ok_or_else(|| ToolError::NotFound(format!("Issue {} not found", args.id)))?;

        self.json_content("trx://tools/issues/get", json!({ "issue": issue }))
    }

    // Tool: create issue
    fn tool_create(&self, args: CreateIssueArgs) -> Result<Vec<Content>, ToolError> {
        let mut store = self
            .inner
            .store
            .write()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        // Get prefix for ID generation
        let prefix = store.prefix().unwrap_or_else(|_| "trx".to_string());

        // Generate ID (either root or child)
        let id = if let Some(parent_id) = &args.parent_id {
            // Verify parent exists
            if store.get(parent_id).is_none() {
                return Err(ToolError::InvalidParameters(format!(
                    "Parent issue {} not found",
                    parent_id
                )));
            }
            let child_num = store.next_child_num(parent_id);
            format!("{}.{}", parent_id, child_num)
        } else {
            generate_id(&prefix)
        };

        // Create the issue
        let mut issue = Issue::new(id.clone(), args.title);

        if let Some(desc) = args.description {
            issue.description = Some(desc);
        }
        if let Some(priority) = args.priority {
            issue.priority = priority.min(4);
        }
        if let Some(type_str) = args.issue_type
            && let Ok(t) = type_str.parse::<IssueType>()
        {
            issue.issue_type = t;
        }
        if let Some(labels) = args.labels {
            issue.labels = labels;
        }
        if let Some(assignee) = args.assignee {
            issue.assignee = Some(assignee);
        }

        // Add parent-child dependency if this is a child
        if let Some(parent_id) = args.parent_id {
            issue.add_dependency(parent_id, trx_core::DependencyType::ParentChild);
        }

        store
            .create(issue.clone())
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        self.json_content(
            "trx://tools/issues/create",
            json!({
                "created": true,
                "issue": issue,
            }),
        )
    }

    // Tool: update issue
    fn tool_update(&self, args: UpdateIssueArgs) -> Result<Vec<Content>, ToolError> {
        let mut store = self
            .inner
            .store
            .write()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let issue = store
            .get_mut(&args.id)
            .ok_or_else(|| ToolError::NotFound(format!("Issue {} not found", args.id)))?;

        // Apply updates
        if let Some(title) = args.title {
            issue.title = title;
        }
        if let Some(desc) = args.description {
            issue.description = Some(desc);
        }
        if let Some(status_str) = args.status
            && let Ok(status) = status_str.parse::<Status>()
        {
            issue.status = status;
        }
        if let Some(priority) = args.priority {
            issue.priority = priority.min(4);
        }
        if let Some(type_str) = args.issue_type
            && let Ok(t) = type_str.parse::<IssueType>()
        {
            issue.issue_type = t;
        }
        if let Some(labels) = args.labels {
            issue.labels = labels;
        }
        if let Some(assignee) = args.assignee {
            issue.assignee = Some(assignee);
        }
        if let Some(notes) = args.notes {
            issue.notes = Some(notes);
        }

        issue.updated_at = chrono::Utc::now();
        let updated = issue.clone();

        store
            .save()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        self.json_content(
            "trx://tools/issues/update",
            json!({
                "updated": true,
                "issue": updated,
            }),
        )
    }

    // Tool: close issue
    fn tool_close(&self, args: CloseIssueArgs) -> Result<Vec<Content>, ToolError> {
        let mut store = self
            .inner
            .store
            .write()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let issue = store
            .get_mut(&args.id)
            .ok_or_else(|| ToolError::NotFound(format!("Issue {} not found", args.id)))?;

        issue.close(args.reason);
        let closed = issue.clone();

        store
            .save()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        self.json_content(
            "trx://tools/issues/close",
            json!({
                "closed": true,
                "issue": closed,
            }),
        )
    }

    // Tool: delete issue (tombstone)
    fn tool_delete(&self, args: DeleteIssueArgs) -> Result<Vec<Content>, ToolError> {
        let mut store = self
            .inner
            .store
            .write()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        store
            .delete(&args.id, args.by, args.reason)
            .map_err(|e| match e {
                trx_core::Error::NotFound(_) => {
                    ToolError::NotFound(format!("Issue {} not found", args.id))
                }
                _ => ToolError::ExecutionError(e.to_string()),
            })?;

        self.json_content(
            "trx://tools/issues/delete",
            json!({
                "deleted": true,
                "id": args.id,
            }),
        )
    }

    // Tool: add dependency
    fn tool_add_dependency(&self, args: AddDependencyArgs) -> Result<Vec<Content>, ToolError> {
        let mut store = self
            .inner
            .store
            .write()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        // Check that target exists
        if store.get(&args.depends_on).is_none() {
            return Err(ToolError::InvalidParameters(format!(
                "Dependency target {} not found",
                args.depends_on
            )));
        }

        let issue = store
            .get_mut(&args.id)
            .ok_or_else(|| ToolError::NotFound(format!("Issue {} not found", args.id)))?;

        let dep_type = args
            .dep_type
            .map(|s| match s.to_lowercase().as_str() {
                "blocks" => trx_core::DependencyType::Blocks,
                "parent_child" | "parent-child" => trx_core::DependencyType::ParentChild,
                "related" => trx_core::DependencyType::Related,
                _ => trx_core::DependencyType::Blocks,
            })
            .unwrap_or(trx_core::DependencyType::Blocks);

        issue.add_dependency(args.depends_on.clone(), dep_type);
        let updated = issue.clone();

        store
            .save()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        self.json_content(
            "trx://tools/issues/dependency/add",
            json!({
                "added": true,
                "issue": updated,
            }),
        )
    }

    // Tool: remove dependency
    fn tool_remove_dependency(
        &self,
        args: RemoveDependencyArgs,
    ) -> Result<Vec<Content>, ToolError> {
        let mut store = self
            .inner
            .store
            .write()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let issue = store
            .get_mut(&args.id)
            .ok_or_else(|| ToolError::NotFound(format!("Issue {} not found", args.id)))?;

        issue.remove_dependency(&args.depends_on);
        let updated = issue.clone();

        store
            .save()
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        self.json_content(
            "trx://tools/issues/dependency/remove",
            json!({
                "removed": true,
                "issue": updated,
            }),
        )
    }
}

// ============================================================================
// MCP Router trait implementation
// ============================================================================

impl mcp_server::Router for TrxMcpRouter {
    fn name(&self) -> String {
        "trx".to_string()
    }

    fn instructions(&self) -> String {
        format!(
            "trx issue tracker for {}. Use tools to list, create, update, and manage issues.",
            self.inner.root_path.display()
        )
    }

    fn capabilities(&self) -> ServerCapabilities {
        mcp_server::router::CapabilitiesBuilder::new()
            .with_tools(true)
            .build()
    }

    fn list_tools(&self) -> Vec<Tool> {
        vec![
            Tool::new(
                "trx.issues.list",
                "List all issues with optional filters.",
                json!({
                    "type": "object",
                    "properties": {
                        "status": { "type": ["string", "null"], "description": "Filter by status: open, in_progress, blocked, closed" },
                        "issue_type": { "type": ["string", "null"], "description": "Filter by type: bug, feature, task, epic, chore" },
                        "priority": { "type": ["integer", "null"], "minimum": 0, "maximum": 4, "description": "Filter by priority (0=critical to 4=backlog)" },
                        "include_tombstones": { "type": ["boolean", "null"], "default": false }
                    },
                    "additionalProperties": false
                }),
            ),
            Tool::new(
                "trx.issues.ready",
                "List open issues that are not blocked by other open issues.",
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            ),
            Tool::new(
                "trx.issues.get",
                "Get a single issue by ID.",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Issue ID (e.g., trx-abc1)" }
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }),
            ),
            Tool::new(
                "trx.issues.create",
                "Create a new issue.",
                json!({
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "description": { "type": ["string", "null"] },
                        "priority": { "type": ["integer", "null"], "minimum": 0, "maximum": 4, "description": "0=critical, 1=high, 2=medium (default), 3=low, 4=backlog" },
                        "issue_type": { "type": ["string", "null"], "description": "bug, feature, task (default), epic, chore" },
                        "labels": { "type": ["array", "null"], "items": { "type": "string" } },
                        "parent_id": { "type": ["string", "null"], "description": "Parent issue ID to create as child" },
                        "assignee": { "type": ["string", "null"] }
                    },
                    "required": ["title"],
                    "additionalProperties": false
                }),
            ),
            Tool::new(
                "trx.issues.update",
                "Update an existing issue.",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "title": { "type": ["string", "null"] },
                        "description": { "type": ["string", "null"] },
                        "status": { "type": ["string", "null"], "description": "open, in_progress, blocked, closed" },
                        "priority": { "type": ["integer", "null"], "minimum": 0, "maximum": 4 },
                        "issue_type": { "type": ["string", "null"] },
                        "labels": { "type": ["array", "null"], "items": { "type": "string" } },
                        "assignee": { "type": ["string", "null"] },
                        "notes": { "type": ["string", "null"] }
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }),
            ),
            Tool::new(
                "trx.issues.close",
                "Close an issue with optional reason.",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "reason": { "type": ["string", "null"] }
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }),
            ),
            Tool::new(
                "trx.issues.delete",
                "Delete an issue (soft delete / tombstone).",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "by": { "type": ["string", "null"], "description": "Who is deleting" },
                        "reason": { "type": ["string", "null"] }
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }),
            ),
            Tool::new(
                "trx.issues.dependency.add",
                "Add a dependency to an issue.",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Issue to add dependency to" },
                        "depends_on": { "type": "string", "description": "Issue ID that this depends on" },
                        "dep_type": { "type": ["string", "null"], "description": "blocks (default), parent_child, related" }
                    },
                    "required": ["id", "depends_on"],
                    "additionalProperties": false
                }),
            ),
            Tool::new(
                "trx.issues.dependency.remove",
                "Remove a dependency from an issue.",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "depends_on": { "type": "string" }
                    },
                    "required": ["id", "depends_on"],
                    "additionalProperties": false
                }),
            ),
        ]
    }

    fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<Content>, ToolError>> + Send + 'static>,
    > {
        let router = self.clone();
        let tool_name = tool_name.to_string();
        Box::pin(async move {
            match tool_name.as_str() {
                "trx.issues.list" => {
                    let args: ListIssuesArgs = serde_json::from_value(arguments)
                        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
                    router.tool_list(args)
                }
                "trx.issues.ready" => router.tool_ready(),
                "trx.issues.get" => {
                    let args: GetIssueArgs = serde_json::from_value(arguments)
                        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
                    router.tool_get(args)
                }
                "trx.issues.create" => {
                    let args: CreateIssueArgs = serde_json::from_value(arguments)
                        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
                    router.tool_create(args)
                }
                "trx.issues.update" => {
                    let args: UpdateIssueArgs = serde_json::from_value(arguments)
                        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
                    router.tool_update(args)
                }
                "trx.issues.close" => {
                    let args: CloseIssueArgs = serde_json::from_value(arguments)
                        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
                    router.tool_close(args)
                }
                "trx.issues.delete" => {
                    let args: DeleteIssueArgs = serde_json::from_value(arguments)
                        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
                    router.tool_delete(args)
                }
                "trx.issues.dependency.add" => {
                    let args: AddDependencyArgs = serde_json::from_value(arguments)
                        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
                    router.tool_add_dependency(args)
                }
                "trx.issues.dependency.remove" => {
                    let args: RemoveDependencyArgs = serde_json::from_value(arguments)
                        .map_err(|e| ToolError::InvalidParameters(e.to_string()))?;
                    router.tool_remove_dependency(args)
                }
                _ => Err(ToolError::NotFound(tool_name)),
            }
        })
    }

    fn list_resources(&self) -> Vec<Resource> {
        Vec::new()
    }

    fn read_resource(
        &self,
        _uri: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, ResourceError>> + Send + 'static>,
    > {
        Box::pin(async { Err(ResourceError::NotFound("No resources supported".into())) })
    }

    fn list_prompts(&self) -> Vec<Prompt> {
        Vec::new()
    }

    fn get_prompt(
        &self,
        prompt_name: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, PromptError>> + Send + 'static>,
    > {
        let name = prompt_name.to_string();
        Box::pin(async move { Err(PromptError::NotFound(name)) })
    }
}

// ============================================================================
// Main
// ============================================================================

fn parse_workdir() -> anyhow::Result<Option<PathBuf>> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workdir" | "-w" => {
                let path = args.next().context("--workdir requires a path argument")?;
                return Ok(Some(PathBuf::from(path)));
            }
            "-h" | "--help" => {
                println!(
                    "trx-mcp\n\nUsage:\n  trx-mcp [--workdir PATH]\n\nRuns an MCP (Model Context Protocol) server over stdio for trx issue tracking."
                );
                std::process::exit(0);
            }
            _ => continue,
        }
    }
    Ok(None)
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    // Change to workdir if specified
    if let Some(workdir) = parse_workdir()? {
        std::env::set_current_dir(&workdir)
            .with_context(|| format!("Failed to change to workdir: {}", workdir.display()))?;
    }

    let cwd = std::env::current_dir()?;

    // Open the store
    let store = Store::open().map_err(|e| anyhow::anyhow!("Failed to open store: {}", e))?;

    let router = TrxMcpRouter::new(store, cwd);
    let service = RouterService(router);
    let server = mcp_server::Server::new(service);

    let transport = mcp_server::ByteTransport::new(tokio::io::stdin(), tokio::io::stdout());
    server.run(transport).await?;

    Ok(())
}
