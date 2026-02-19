//! CLI command implementations

use anyhow::{Result, bail};
use colored::Colorize;
use trx_core::{
    DependencyType, Issue, IssueGraph, IssueType, Status, StorageVersion, Store, UnifiedStore,
    generate_id, id::generate_child_id, migrate_v1_to_v2, rollback_v2_to_v1,
};

pub fn init(prefix: &str) -> Result<()> {
    let store = Store::init(prefix)?;
    println!(
        "{} Initialized trx in {}",
        "✓".green(),
        store.trx_dir().display()
    );
    println!("  Issue prefix: {}", prefix);
    Ok(())
}

pub fn create(
    title: &str,
    issue_type: &str,
    priority: u8,
    description: Option<String>,
    parent: Option<String>,
    json: bool,
) -> Result<()> {
    let mut store = UnifiedStore::open()?;
    let prefix = store.prefix()?;

    let id = if let Some(ref parent_id) = parent {
        let child_num = store.next_child_num(parent_id);
        generate_child_id(parent_id, child_num)
    } else {
        generate_id(&prefix)
    };

    let mut issue = Issue::new(id.clone(), title.to_string());
    issue.issue_type = issue_type.parse()?;
    issue.priority = priority;
    issue.description = description;

    if let Some(ref parent_id) = parent {
        issue.add_dependency(parent_id.clone(), DependencyType::ParentChild);
    }

    store.create(issue.clone())?;

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else {
        println!("{} Created issue: {}", "✓".green(), id);
        println!("  Title: {}", title);
        println!("  Priority: P{}", priority);
    }

    Ok(())
}

pub fn list(
    status: Option<String>,
    issue_type: Option<String>,
    all: bool,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let store = UnifiedStore::open()?;
    let mut issues: Vec<_> = if all {
        store.list(false)
    } else {
        store.list_open()
    };

    // Filter by status
    if let Some(ref s) = status {
        let status: Status = s.parse()?;
        issues.retain(|i| i.status == status);
    }

    // Filter by type
    if let Some(ref t) = issue_type {
        let itype: IssueType = t.parse()?;
        issues.retain(|i| i.issue_type == itype);
    }

    // Sort by priority, then by creation date
    issues.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });

    // Apply limit
    if let Some(n) = limit {
        issues.truncate(n);
    }

    if json {
        println!("{}", serde_json::to_string(&issues)?);
    } else if issues.is_empty() {
        println!("No issues found");
    } else {
        for issue in issues {
            let status_color = match issue.status {
                Status::Open => "open".white(),
                Status::InProgress => "in_progress".yellow(),
                Status::Blocked => "blocked".red(),
                Status::Closed => "closed".green(),
                Status::Tombstone => "tombstone".dimmed(),
            };
            println!(
                "{} [P{}] [{}] {} - {}",
                issue.id.cyan(),
                issue.priority,
                issue.issue_type.to_string().blue(),
                status_color,
                issue.title
            );
        }
    }

    Ok(())
}

pub fn show(id: &str, json: bool) -> Result<()> {
    let store = UnifiedStore::open()?;
    let issue = store
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    if json {
        println!("{}", serde_json::to_string_pretty(issue)?);
    } else {
        println!("{} {}", issue.id.cyan().bold(), issue.title.bold());
        println!();
        println!("Status:   {}", issue.status);
        println!("Priority: P{}", issue.priority);
        println!("Type:     {}", issue.issue_type);
        println!("Created:  {}", issue.created_at.format("%Y-%m-%d %H:%M"));
        println!("Updated:  {}", issue.updated_at.format("%Y-%m-%d %H:%M"));

        if let Some(ref desc) = issue.description {
            println!();
            println!("{}", "Description:".bold());
            println!("{}", desc);
        }

        if !issue.dependencies.is_empty() {
            println!();
            println!("{}", "Dependencies:".bold());
            for dep in &issue.dependencies {
                println!("  {} {} {}", dep.issue_id, dep.dep_type, dep.depends_on_id);
            }
        }
    }

    Ok(())
}

pub fn update(
    id: &str,
    status: Option<String>,
    priority: Option<u8>,
    title: Option<String>,
    description: Option<String>,
    json: bool,
) -> Result<()> {
    let mut store = UnifiedStore::open()?;
    let issue = store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    if let Some(s) = status {
        issue.status = s.parse()?;
    }
    if let Some(p) = priority {
        issue.priority = p;
    }
    if let Some(t) = title {
        issue.title = t;
    }
    if let Some(d) = description {
        issue.description = Some(d);
    }

    issue.updated_at = chrono::Utc::now();
    let issue = issue.clone();
    store.update(issue.clone())?;

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else {
        println!("{} Updated {}", "✓".green(), id);
    }

    Ok(())
}

pub fn close(id: &str, reason: Option<String>, json: bool) -> Result<()> {
    let mut store = UnifiedStore::open()?;
    let issue = store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    issue.close(reason);
    let issue = issue.clone();
    store.update(issue.clone())?;

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else {
        println!("{} Closed {}", "✓".green(), id);
    }

    Ok(())
}

pub fn ready(json: bool) -> Result<()> {
    let store = UnifiedStore::open()?;
    let open_issues: Vec<_> = store.list_open();
    let graph = IssueGraph::from_issues(&open_issues);
    let mut ready = graph.ready_issues(&open_issues);

    // Sort by priority
    ready.sort_by(|a, b| a.priority.cmp(&b.priority));

    if json {
        println!("{}", serde_json::to_string(&ready)?);
    } else if ready.is_empty() {
        println!("No ready issues");
    } else {
        println!("{}", "Ready issues (unblocked):".bold());
        for issue in ready {
            println!(
                "{} [P{}] [{}] - {}",
                issue.id.cyan(),
                issue.priority,
                issue.issue_type.to_string().blue(),
                issue.title
            );
        }
    }

    Ok(())
}

pub fn dep_add(id: &str, blocks: &str, json: bool) -> Result<()> {
    let mut store = UnifiedStore::open()?;
    let issue = store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    issue.add_dependency(blocks.to_string(), DependencyType::Blocks);
    let issue = issue.clone();
    store.update(issue.clone())?;

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else {
        println!("{} {} now blocks {}", "✓".green(), id, blocks);
    }

    Ok(())
}

pub fn dep_rm(id: &str, blocks: &str, json: bool) -> Result<()> {
    let mut store = UnifiedStore::open()?;
    let issue = store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    issue.remove_dependency(blocks);
    let issue = issue.clone();
    store.update(issue.clone())?;

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else {
        println!("{} {} no longer blocks {}", "✓".green(), id, blocks);
    }

    Ok(())
}

pub fn dep_tree(id: &str, _json: bool) -> Result<()> {
    let store = UnifiedStore::open()?;
    let _issue = store
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    // TODO: Implement tree visualization
    println!("Dependency tree for {}:", id);
    println!("  (not yet implemented)");

    Ok(())
}

pub fn sync(message: Option<String>) -> Result<()> {
    let mut store = UnifiedStore::open()?;
    let trx_dir = store.trx_dir();

    // Resolve any CRDT conflicts first (v2 only)
    let resolved = store.resolve_conflicts()?;
    if !resolved.is_empty() {
        println!("{} Resolved {} conflict(s):", "✓".green(), resolved.len());
        for file in &resolved {
            println!("  - {}", file);
        }
    }

    let msg = message.unwrap_or_else(|| "trx: sync issues".to_string());

    // Git add .trx/
    let output = std::process::Command::new("git")
        .args(["add", &trx_dir.to_string_lossy()])
        .output()?;

    if !output.status.success() {
        bail!(
            "git add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Git commit
    let output = std::process::Command::new("git")
        .args(["commit", "-m", &msg])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("nothing to commit") {
            println!("Nothing to sync");
            return Ok(());
        }
        bail!("git commit failed: {}", stderr);
    }

    println!("{} Synced .trx/", "✓".green());
    Ok(())
}

pub fn migrate(dry_run: bool, rollback: bool, yes: bool) -> Result<()> {
    // Check current version
    let store = UnifiedStore::open()?;
    let current_version = store.version();
    let trx_dir = store.trx_dir();
    drop(store);

    if rollback {
        // Rollback v2 -> v1
        if current_version == StorageVersion::V1 {
            println!("Already using v1 (JSONL) storage");
            return Ok(());
        }

        println!("{}", "Rollback: v2 (CRDT) -> v1 (JSONL)".bold());
        println!();

        if dry_run {
            let result = rollback_v2_to_v1(true)?;
            println!(
                "Would migrate {} issues back to JSONL format",
                result.issues_migrated
            );
            println!();
            println!("Run without --dry-run to perform the rollback");
            return Ok(());
        }

        if !yes {
            println!("This will convert CRDT storage back to JSONL format.");
            println!("The crdt/ directory will be preserved for safety.");
            println!();
            print!("Continue? [y/N] ");
            std::io::Write::flush(&mut std::io::stdout())?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            if !input.trim().eq_ignore_ascii_case("y") {
                println!("Aborted");
                return Ok(());
            }
        }

        let result = rollback_v2_to_v1(false)?;
        println!(
            "{} Rolled back {} issues to v1 (JSONL)",
            "✓".green(),
            result.issues_migrated
        );
        println!();
        println!("Note: The crdt/ directory was preserved. You can remove it manually:");
        println!("  rm -rf {}/.trx/crdt", trx_dir.parent().unwrap().display());
    } else {
        // Migrate v1 -> v2
        if current_version == StorageVersion::V2 {
            println!("Already using v2 (CRDT) storage");
            return Ok(());
        }

        println!("{}", "Migration: v1 (JSONL) -> v2 (CRDT)".bold());
        println!();
        println!("Benefits of v2:");
        println!("  - Conflict-free merging across branches/worktrees");
        println!("  - One file per issue (git handles additions automatically)");
        println!("  - Human-readable ISSUES.md for browsing without trx");
        println!();

        if dry_run {
            let result = migrate_v1_to_v2(true)?;
            println!(
                "Would migrate {} issues to CRDT format",
                result.issues_migrated
            );
            println!();
            println!("Run without --dry-run to perform the migration");
            return Ok(());
        }

        if !yes {
            println!("This will:");
            println!("  1. Create .trx/crdt/ with one .automerge file per issue");
            println!("  2. Generate .trx/ISSUES.md for human browsing");
            println!("  3. Update config.toml to storage_version = \"v2\"");
            println!("  4. Keep issues.jsonl as backup (can be removed later)");
            println!();
            print!("Continue? [y/N] ");
            std::io::Write::flush(&mut std::io::stdout())?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            if !input.trim().eq_ignore_ascii_case("y") {
                println!("Aborted");
                return Ok(());
            }
        }

        let result = migrate_v1_to_v2(false)?;
        println!(
            "{} Migrated {} issues to v2 (CRDT)",
            "✓".green(),
            result.issues_migrated
        );
        println!();
        println!("The old issues.jsonl was preserved. You can remove it with:");
        println!("  rm {}/issues.jsonl", trx_dir.display());
        println!();
        println!("Don't forget to commit the changes:");
        println!("  trx sync -m \"Migrate to CRDT storage\"");
    }

    Ok(())
}

pub fn import(path: &str, prefix: Option<String>, json: bool) -> Result<()> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let mut store = UnifiedStore::open()?;
    let new_prefix = prefix.unwrap_or_else(|| store.prefix().unwrap_or_else(|_| "trx".to_string()));

    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let mut imported = 0;
    let mut skipped = 0;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        // Parse as generic JSON to handle beads fields
        let value: serde_json::Value = serde_json::from_str(&line)?;

        // Convert beads issue to trx issue
        let id = value["id"].as_str().unwrap_or("").to_string();
        if id.is_empty() {
            skipped += 1;
            continue;
        }

        // Optionally convert prefix
        let new_id = if id.starts_with("bd-") {
            id.replacen("bd-", &format!("{}-", new_prefix), 1)
        } else {
            id.clone()
        };

        let title = value["title"].as_str().unwrap_or("Untitled").to_string();
        let mut issue = Issue::new(new_id, title);

        // Map fields
        if let Some(desc) = value["description"].as_str() {
            issue.description = Some(desc.to_string());
        }
        if let Some(status) = value["status"].as_str() {
            issue.status = status.parse().unwrap_or(Status::Open);
        }
        if let Some(priority) = value["priority"].as_u64() {
            issue.priority = priority as u8;
        }
        if let Some(itype) = value["issue_type"].as_str() {
            issue.issue_type = itype.parse().unwrap_or(IssueType::Task);
        }
        if let Some(created) = value["created_at"].as_str()
            && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(created)
        {
            issue.created_at = dt.into();
        }
        if let Some(updated) = value["updated_at"].as_str()
            && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(updated)
        {
            issue.updated_at = dt.into();
        }
        if let Some(closed) = value["closed_at"].as_str()
            && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(closed)
        {
            issue.closed_at = Some(dt.into());
        }
        if let Some(reason) = value["close_reason"].as_str() {
            issue.close_reason = Some(reason.to_string());
        }

        // Import dependencies
        if let Some(deps) = value["dependencies"].as_array() {
            for dep in deps {
                if let (Some(depends_on), Some(dep_type)) =
                    (dep["depends_on_id"].as_str(), dep["type"].as_str())
                {
                    let dtype = match dep_type {
                        "blocks" => DependencyType::Blocks,
                        "parent-child" => DependencyType::ParentChild,
                        _ => DependencyType::Related,
                    };
                    let depends_on_id = if depends_on.starts_with("bd-") {
                        depends_on.replacen("bd-", &format!("{}-", new_prefix), 1)
                    } else {
                        depends_on.to_string()
                    };
                    issue.add_dependency(depends_on_id, dtype);
                }
            }
        }

        if store.get(&issue.id).is_some() {
            skipped += 1;
        } else {
            store.create(issue)?;
            imported += 1;
        }
    }

    if json {
        println!(r#"{{"imported": {}, "skipped": {}}}"#, imported, skipped);
    } else {
        println!(
            "{} Imported {} issues ({} skipped)",
            "✓".green(),
            imported,
            skipped
        );
    }

    Ok(())
}

pub fn purge_beads(force: bool) -> Result<()> {
    let beads_dir = std::path::Path::new(".beads");

    if !beads_dir.exists() {
        println!("No .beads directory found");
        return Ok(());
    }

    if !force {
        println!(
            "{}",
            "This will remove .beads/ directory and all beads data.".red()
        );
        println!("Make sure you have imported issues first: trx import .beads/issues.jsonl");
        println!();
        print!("Continue? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted");
            return Ok(());
        }
    }

    // Remove .beads directory
    std::fs::remove_dir_all(beads_dir)?;

    // Try to clean up daemon socket if exists
    let socket = std::path::Path::new(".beads/bd.sock");
    if socket.exists() {
        let _ = std::fs::remove_file(socket);
    }

    println!("{} Removed .beads/", "✓".green());
    println!("You may also want to:");
    println!("  - Remove beads from git: git rm -r .beads/");
    println!("  - Kill any running bd daemon");

    Ok(())
}

/// Output JSON schema for config file
pub fn schema() -> Result<()> {
    let schema = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "trx Configuration",
        "description": "Configuration file for the trx issue tracker",
        "type": "object",
        "properties": {
            "prefix": {
                "type": "string",
                "description": "Issue ID prefix (e.g., 'trx', 'myproject')",
                "default": "trx"
            },
            "default_priority": {
                "type": "integer",
                "description": "Default priority for new issues (0=critical, 1=high, 2=medium, 3=low, 4=backlog)",
                "minimum": 0,
                "maximum": 4,
                "default": 2
            },
            "default_type": {
                "type": "string",
                "enum": ["bug", "feature", "task", "epic", "chore"],
                "description": "Default issue type for new issues",
                "default": "task"
            },
            "auto_sync": {
                "type": "boolean",
                "description": "Auto-sync after mutations (git add + commit)",
                "default": false
            },
            "sync_message_template": {
                "type": "string",
                "description": "Sync commit message template. Variables: {action}, {id}, {title}",
                "default": "trx: {action} {id}"
            },
            "show_closed": {
                "type": "boolean",
                "description": "Show closed issues in list by default",
                "default": false
            },
            "editor": {
                "type": ["string", "null"],
                "description": "Editor command for editing descriptions (uses $EDITOR if not set)"
            },
            "git": {
                "type": "object",
                "properties": {
                    "auto_stage": {
                        "type": "boolean",
                        "description": "Automatically stage .trx/ after changes",
                        "default": false
                    },
                    "sync_branch": {
                        "type": ["string", "null"],
                        "description": "Branch to sync to (if different from current)"
                    }
                }
            },
            "display": {
                "type": "object",
                "properties": {
                    "colors": {
                        "type": "boolean",
                        "description": "Use colors in output",
                        "default": true
                    },
                    "date_format": {
                        "type": "string",
                        "description": "Date format for display (strftime format)",
                        "default": "%Y-%m-%d %H:%M"
                    },
                    "show_count": {
                        "type": "boolean",
                        "description": "Show issue count in list header",
                        "default": true
                    },
                    "max_title_length": {
                        "type": "integer",
                        "description": "Maximum title length before truncation",
                        "minimum": 20,
                        "default": 80
                    }
                }
            }
        }
    });
    println!("{}", serde_json::to_string_pretty(&schema)?);
    Ok(())
}

/// Show current configuration
pub fn config_show(json: bool) -> Result<()> {
    let store = UnifiedStore::open()?;
    let config_path = store.trx_dir().join("config.toml");
    let config = trx_core::Config::load(&config_path)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        println!("{}", "Current configuration:".bold());
        println!();
        println!("prefix = \"{}\"", config.prefix);
        println!("default_priority = {}", config.default_priority);
        println!("default_type = \"{}\"", config.default_type);
        println!("auto_sync = {}", config.auto_sync);
        println!(
            "sync_message_template = \"{}\"",
            config.sync_message_template
        );
        println!("show_closed = {}", config.show_closed);
        if let Some(ref editor) = config.editor {
            println!("editor = \"{}\"", editor);
        }
        println!();
        println!("[git]");
        println!("auto_stage = {}", config.git.auto_stage);
        if let Some(ref branch) = config.git.sync_branch {
            println!("sync_branch = \"{}\"", branch);
        }
        println!();
        println!("[display]");
        println!("colors = {}", config.display.colors);
        println!("date_format = \"{}\"", config.display.date_format);
        println!("show_count = {}", config.display.show_count);
        println!("max_title_length = {}", config.display.max_title_length);
    }

    Ok(())
}

/// Edit configuration file
pub fn config_edit() -> Result<()> {
    let store = UnifiedStore::open()?;
    let config_path = store.trx_dir().join("config.toml");

    // Get editor from environment
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    let status = std::process::Command::new(&editor)
        .arg(&config_path)
        .status()?;

    if !status.success() {
        bail!("Editor exited with non-zero status");
    }

    // Validate the config after editing
    match trx_core::Config::load(&config_path) {
        Ok(_) => println!("{} Configuration saved", "✓".green()),
        Err(e) => {
            println!(
                "{} Warning: Configuration may be invalid: {}",
                "!".yellow(),
                e
            );
        }
    }

    Ok(())
}

/// Reset configuration to defaults
pub fn config_reset() -> Result<()> {
    let store = UnifiedStore::open()?;
    let config_path = store.trx_dir().join("config.toml");

    let default_config = trx_core::Config::default_with_comments();
    std::fs::write(&config_path, default_config)?;

    println!("{} Configuration reset to defaults", "✓".green());
    Ok(())
}

/// Get a specific config value
pub fn config_get(key: &str, json: bool) -> Result<()> {
    let store = UnifiedStore::open()?;
    let config_path = store.trx_dir().join("config.toml");
    let config = trx_core::Config::load(&config_path)?;

    // Convert config to JSON for key lookup
    let config_json = serde_json::to_value(&config)?;

    // Parse key path (e.g., "display.colors" -> ["display", "colors"])
    let parts: Vec<&str> = key.split('.').collect();
    let mut value = &config_json;

    for part in &parts {
        value = value
            .get(part)
            .ok_or_else(|| anyhow::anyhow!("Config key not found: {}", key))?;
    }

    if json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        match value {
            serde_json::Value::String(s) => println!("{}", s),
            serde_json::Value::Bool(b) => println!("{}", b),
            serde_json::Value::Number(n) => println!("{}", n),
            serde_json::Value::Null => println!("null"),
            _ => println!("{}", serde_json::to_string_pretty(value)?),
        }
    }

    Ok(())
}

/// Set a config value
pub fn config_set(key: &str, value: &str) -> Result<()> {
    let store = UnifiedStore::open()?;
    let config_path = store.trx_dir().join("config.toml");
    let mut config = trx_core::Config::load(&config_path)?;

    // Handle top-level and nested keys
    match key {
        "prefix" => config.prefix = value.to_string(),
        "default_priority" => {
            config.default_priority = value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid priority value: {}", value))?;
        }
        "default_type" => config.default_type = value.to_string(),
        "auto_sync" => {
            config.auto_sync = value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid boolean value: {}", value))?;
        }
        "sync_message_template" => config.sync_message_template = value.to_string(),
        "show_closed" => {
            config.show_closed = value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid boolean value: {}", value))?;
        }
        "editor" => config.editor = Some(value.to_string()),
        "git.auto_stage" => {
            config.git.auto_stage = value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid boolean value: {}", value))?;
        }
        "git.sync_branch" => config.git.sync_branch = Some(value.to_string()),
        "display.colors" => {
            config.display.colors = value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid boolean value: {}", value))?;
        }
        "display.date_format" => config.display.date_format = value.to_string(),
        "display.show_count" => {
            config.display.show_count = value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid boolean value: {}", value))?;
        }
        "display.max_title_length" => {
            config.display.max_title_length = value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid integer value: {}", value))?;
        }
        _ => bail!("Unknown config key: {}", key),
    }

    config.save(&config_path)?;
    println!("{} Set {} = {}", "✓".green(), key, value);

    Ok(())
}

// ============================================================================
// Resolve and merge driver commands
// ============================================================================

pub fn resolve(json: bool) -> Result<()> {
    let mut store = UnifiedStore::open()?;

    // Resolve any CRDT conflicts first (v2 only)
    let resolved = store.resolve_conflicts()?;

    // Regenerate ISSUES.md from source files
    store.regenerate_issues_md()?;

    if json {
        println!(
            r#"{{"resolved_conflicts": {}, "issues_md": "regenerated"}}"#,
            resolved.len()
        );
    } else {
        if !resolved.is_empty() {
            println!(
                "{} Resolved {} CRDT conflict(s):",
                "✓".green(),
                resolved.len()
            );
            for file in &resolved {
                println!("  - {}", file);
            }
        }
        println!("{} Regenerated ISSUES.md from source files", "✓".green());
    }

    Ok(())
}

/// Find the git repository root
fn git_root() -> Result<std::path::PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        bail!("Not a git repository");
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(std::path::PathBuf::from(path))
}

/// Find the trx binary path
fn trx_binary_path() -> Result<std::path::PathBuf> {
    std::env::current_exe().map_err(|e| anyhow::anyhow!("Failed to find trx binary: {}", e))
}

pub fn merge_driver_install() -> Result<()> {
    let git_root = git_root()?;
    let trx_bin = trx_binary_path()?;

    // 1. Configure the merge driver in .git/config
    let status = std::process::Command::new("git")
        .args([
            "config",
            "merge.trx-issues-md.name",
            "Auto-regenerate ISSUES.md from trx source files",
        ])
        .status()?;
    if !status.success() {
        bail!("Failed to set merge driver name");
    }

    let driver_cmd = format!("{} resolve", trx_bin.display());
    let status = std::process::Command::new("git")
        .args(["config", "merge.trx-issues-md.driver", &driver_cmd])
        .status()?;
    if !status.success() {
        bail!("Failed to set merge driver command");
    }

    // 2. Add .gitattributes entry for ISSUES.md
    let gitattributes_path = git_root.join(".gitattributes");
    let attr_line = ".trx/ISSUES.md merge=trx-issues-md";

    let existing = if gitattributes_path.exists() {
        std::fs::read_to_string(&gitattributes_path)?
    } else {
        String::new()
    };

    if !existing.lines().any(|l| l.trim() == attr_line) {
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(attr_line);
        content.push('\n');
        std::fs::write(&gitattributes_path, content)?;
    }

    println!("{} Installed trx merge driver", "✓".green());
    println!("  Merge driver: {}", driver_cmd);
    println!("  .gitattributes: {}", attr_line);
    println!();
    println!(
        "ISSUES.md conflicts will now be auto-resolved during git merge/rebase."
    );
    println!(
        "Remember to commit .gitattributes so the driver applies for all contributors."
    );

    Ok(())
}

pub fn merge_driver_uninstall() -> Result<()> {
    let git_root = git_root()?;

    // 1. Remove merge driver from .git/config
    let _ = std::process::Command::new("git")
        .args(["config", "--remove-section", "merge.trx-issues-md"])
        .status();

    // 2. Remove .gitattributes entry
    let gitattributes_path = git_root.join(".gitattributes");
    let attr_line = ".trx/ISSUES.md merge=trx-issues-md";

    if gitattributes_path.exists() {
        let content = std::fs::read_to_string(&gitattributes_path)?;
        let filtered: Vec<&str> = content
            .lines()
            .filter(|l| l.trim() != attr_line)
            .collect();

        if filtered.is_empty() {
            std::fs::remove_file(&gitattributes_path)?;
        } else {
            let mut new_content = filtered.join("\n");
            new_content.push('\n');
            std::fs::write(&gitattributes_path, new_content)?;
        }
    }

    println!("{} Uninstalled trx merge driver", "✓".green());
    println!("  Removed merge.trx-issues-md from git config");
    println!("  Removed .gitattributes entry");

    Ok(())
}

pub fn merge_driver_status() -> Result<()> {
    let git_root = git_root()?;

    // Check git config
    let output = std::process::Command::new("git")
        .args(["config", "--get", "merge.trx-issues-md.driver"])
        .output()?;
    let driver_configured = output.status.success();
    let driver_cmd = if driver_configured {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        String::new()
    };

    // Check .gitattributes
    let gitattributes_path = git_root.join(".gitattributes");
    let attr_line = ".trx/ISSUES.md merge=trx-issues-md";
    let attr_configured = if gitattributes_path.exists() {
        let content = std::fs::read_to_string(&gitattributes_path)?;
        content.lines().any(|l| l.trim() == attr_line)
    } else {
        false
    };

    if driver_configured && attr_configured {
        println!("Merge driver is {}", "installed".green());
        println!("  Driver: {}", driver_cmd);
        println!("  .gitattributes: {}", attr_line);
    } else if driver_configured {
        println!("Merge driver is {}", "partially installed".yellow());
        println!("  Driver: {} (configured)", driver_cmd);
        println!(
            "  .gitattributes: {} (run 'trx merge-driver install' to fix)",
            "missing".red()
        );
    } else if attr_configured {
        println!("Merge driver is {}", "partially installed".yellow());
        println!(
            "  Driver: {} (run 'trx merge-driver install' to fix)",
            "not configured".red()
        );
        println!("  .gitattributes: {} (present)", attr_line);
    } else {
        println!("Merge driver is {}", "not installed".yellow());
        println!("  Run 'trx merge-driver install' to set up auto-resolution");
    }

    Ok(())
}

// ============================================================================
// Service commands
// ============================================================================

pub trait ServiceCommand {
    fn is_start(&self) -> bool;
    fn is_run(&self) -> bool;
    fn is_stop(&self) -> bool;
    fn is_restart(&self) -> bool;
    fn is_status(&self) -> bool;
    fn is_enable(&self) -> bool;
}

pub fn service<T: ServiceCommand>(cmd: T) -> Result<()> {
    use trx_core::{ServiceManager, ServiceStatus};

    let manager = ServiceManager::new()
        .map_err(|e| anyhow::anyhow!("Failed to initialize service manager: {}", e))?;

    if cmd.is_start() {
        println!("Starting trx-api service...");
        manager
            .start(false, None)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Wait and check status
        std::thread::sleep(std::time::Duration::from_secs(1));

        match manager.status() {
            ServiceStatus::Running { pid, port } => {
                println!("{} Service started (PID: {})", "✓".green(), pid);
                if let Some(p) = port {
                    println!("  Listening on: 127.0.0.1:{}", p);
                }
            }
            _ => {
                println!("{} Service failed to start", "✗".red());
                std::process::exit(1);
            }
        }
    } else if cmd.is_run() {
        println!("Running trx-api in foreground...");
        println!("Press Ctrl+C to stop");
        manager
            .start(true, None)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    } else if cmd.is_stop() {
        println!("Stopping trx-api service...");
        match manager.stop() {
            Ok(()) => println!("{} Service stopped", "✓".green()),
            Err(e) => {
                println!("{} {}", "✗".red(), e);
                std::process::exit(1);
            }
        }
    } else if cmd.is_restart() {
        println!("Restarting trx-api service...");
        manager
            .restart(None)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        std::thread::sleep(std::time::Duration::from_secs(1));

        match manager.status() {
            ServiceStatus::Running { pid, port } => {
                println!("{} Service restarted (PID: {})", "✓".green(), pid);
                if let Some(p) = port {
                    println!("  Listening on: 127.0.0.1:{}", p);
                }
            }
            _ => {
                println!("{} Service failed to restart", "✗".red());
                std::process::exit(1);
            }
        }
    } else if cmd.is_status() {
        match manager.status() {
            ServiceStatus::Running { pid, port } => {
                println!("Service is {}", "running".green());
                println!("  PID: {}", pid);
                if let Some(p) = port {
                    println!("  Port: {}", p);
                }
            }
            ServiceStatus::Stopped => {
                println!("Service is {}", "stopped".yellow());
            }
            ServiceStatus::Dead => {
                println!("Service appears to be {} (stale PID file)", "dead".red());
                println!("Try running 'trx service stop' to cleanup");
            }
        }
    } else if cmd.is_enable() {
        println!("{}", "Auto-start configuration:".bold());
        println!();

        #[cfg(target_os = "linux")]
        {
            let exe_dir = std::env::current_exe()?
                .parent()
                .unwrap()
                .display()
                .to_string();

            println!("For systemd (Linux):");
            println!();
            println!("1. Create ~/.config/systemd/user/trx-api.service:");
            println!("   [Unit]");
            println!("   Description=trx issue tracker API");
            println!("   After=network.target");
            println!();
            println!("   [Service]");
            println!("   Type=simple");
            println!("   ExecStart={}/trx-api", exe_dir);
            println!("   Restart=on-failure");
            println!();
            println!("   [Install]");
            println!("   WantedBy=default.target");
            println!();
            println!("2. Enable and start:");
            println!("   systemctl --user enable trx-api");
            println!("   systemctl --user start trx-api");
        }

        #[cfg(target_os = "macos")]
        {
            let exe_dir = std::env::current_exe()?
                .parent()
                .unwrap()
                .display()
                .to_string();

            println!("For launchd (macOS):");
            println!();
            println!("1. Create ~/Library/LaunchAgents/com.trx.api.plist:");
            println!("   <?xml version=\"1.0\" encoding=\"UTF-8\"?>");
            println!("   <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"");
            println!("     \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">");
            println!("   <plist version=\"1.0\">");
            println!("   <dict>");
            println!("     <key>Label</key>");
            println!("     <string>com.trx.api</string>");
            println!("     <key>ProgramArguments</key>");
            println!("     <array>");
            println!("       <string>{}/trx-api</string>", exe_dir);
            println!("     </array>");
            println!("     <key>RunAtLoad</key>");
            println!("     <true/>");
            println!("     <key>KeepAlive</key>");
            println!("     <true/>");
            println!("   </dict>");
            println!("   </plist>");
            println!();
            println!("2. Load:");
            println!("   launchctl load ~/Library/LaunchAgents/com.trx.api.plist");
        }

        #[cfg(windows)]
        {
            let exe_dir = std::env::current_exe()?
                .parent()
                .unwrap()
                .display()
                .to_string();

            println!("For Windows:");
            println!();
            println!("1. Add to startup via Task Scheduler:");
            println!("   - Open Task Scheduler");
            println!("   - Create Basic Task");
            println!("   - Trigger: At log on");
            println!("   - Action: Start a program");
            println!("   - Program: {}\\trx-api.exe", exe_dir);
        }
    }

    Ok(())
}
