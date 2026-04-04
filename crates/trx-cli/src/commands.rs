//! CLI command implementations

use anyhow::{Result, bail};
use colored::Colorize;
use trx_core::{
    DependencyType, Issue, IssueGraph, IssueType, Status, StorageVersion, Store, UnifiedStore,
    generate_id, id::generate_child_id, migrate_v1_to_v2, rollback_v2_to_v1,
};

/// Helper to get a mutable reference to a JSON object.
/// Panics only if the value is not an object, which is a programmer error.
fn obj_mut(val: &mut serde_json::Value) -> &mut serde_json::Map<String, serde_json::Value> {
    val.as_object_mut()
        .unwrap_or_else(|| unreachable!("expected JSON object"))
}

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
    edit: bool,
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
    issue.description = if edit {
        let template = description.unwrap_or_default();
        Some(open_editor_for_description(&template, title)?)
    } else {
        description
    };

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
    priority: Option<u8>,
    search: Option<String>,
    epic: Option<String>,
    all: bool,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let store = UnifiedStore::open()?;

    let mut issues: Vec<_> = if epic.is_some() || all {
        store.list(false)
    } else {
        store.list_open()
    };

    // Filter by epic: show the epic and all descendants (by ID prefix or parent_child dep)
    if let Some(ref epic_id) = epic {
        store
            .get(epic_id)
            .ok_or_else(|| anyhow::anyhow!("Epic not found: {}", epic_id))?;

        let epic_prefix = format!("{}.", epic_id);

        // Collect all descendant IDs via any dependency pointing to this epic (BFS)
        let all = store.list(false);
        let mut descendant_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut queue = vec![epic_id.clone()];

        while let Some(parent_id) = queue.pop() {
            for issue in &all {
                if issue
                    .dependencies
                    .iter()
                    .any(|d| d.depends_on_id == parent_id)
                    && descendant_ids.insert(issue.id.clone())
                {
                    queue.push(issue.id.clone());
                }
            }
        }

        issues.retain(|i| {
            i.id == *epic_id || i.id.starts_with(&epic_prefix) || descendant_ids.contains(&i.id)
        });
    }

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

    // Filter by priority
    if let Some(p) = priority {
        issues.retain(|i| i.priority == p);
    }

    // Filter by search (title + description, case-insensitive)
    if let Some(ref q) = search {
        let q_lower = q.to_lowercase();
        issues.retain(|i| {
            i.title.to_lowercase().contains(&q_lower)
                || i.description
                    .as_ref()
                    .is_some_and(|d| d.to_lowercase().contains(&q_lower))
                || i.id.to_lowercase().contains(&q_lower)
        });
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
        // Enrich JSON output with resolved fields (trx-tkmz)
        let all_issues = store.list(false);
        let mut output: Vec<serde_json::Value> = Vec::new();

        for issue in &issues {
            let mut val = serde_json::to_value(issue)?;
            let obj = obj_mut(&mut val);

            // Resolve parent
            if let Some(parent_dep) = issue
                .dependencies
                .iter()
                .find(|d| d.dep_type == DependencyType::ParentChild)
            {
                obj.insert("parent".into(), parent_dep.depends_on_id.clone().into());
            }

            // Resolve blocked_by
            let blocked_by: Vec<&str> = issue
                .dependencies
                .iter()
                .filter(|d| d.dep_type == DependencyType::Blocks)
                .map(|d| d.depends_on_id.as_str())
                .collect();
            if !blocked_by.is_empty() {
                obj.insert("blocked_by".into(), serde_json::to_value(&blocked_by)?);
            }

            // Resolve blocks (reverse: who depends on this issue with blocks type)
            let blocks: Vec<&str> = all_issues
                .iter()
                .filter(|i| {
                    i.dependencies.iter().any(|d| {
                        d.depends_on_id == issue.id && d.dep_type == DependencyType::Blocks
                    })
                })
                .map(|i| i.id.as_str())
                .collect();
            if !blocks.is_empty() {
                obj.insert("blocks".into(), serde_json::to_value(&blocks)?);
            }

            // Resolve children
            let children: Vec<&str> = all_issues
                .iter()
                .filter(|i| {
                    i.dependencies.iter().any(|d| {
                        d.depends_on_id == issue.id && d.dep_type == DependencyType::ParentChild
                    })
                })
                .map(|i| i.id.as_str())
                .collect();
            if !children.is_empty() {
                obj.insert("children".into(), serde_json::to_value(&children)?);
            }

            output.push(val);
        }

        println!("{}", serde_json::to_string(&output)?);
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

    // Find reverse dependencies: issues that depend on this one
    let all_issues = store.list(false);
    let reverse_deps: Vec<_> = all_issues
        .iter()
        .filter(|i| i.dependencies.iter().any(|d| d.depends_on_id == id))
        .collect();

    // Find children (issues with parent_child dep pointing to this issue)
    let children: Vec<_> = reverse_deps
        .iter()
        .filter(|i| {
            i.dependencies
                .iter()
                .any(|d| d.depends_on_id == id && d.dep_type == DependencyType::ParentChild)
        })
        .collect();

    // Find issues blocked by this one
    let blocked_by_this: Vec<_> = reverse_deps
        .iter()
        .filter(|i| {
            i.dependencies
                .iter()
                .any(|d| d.depends_on_id == id && d.dep_type == DependencyType::Blocks)
        })
        .collect();

    if json {
        let mut val = serde_json::to_value(issue)?;
        let obj = obj_mut(&mut val);

        // Add resolved parent
        if let Some(parent_dep) = issue
            .dependencies
            .iter()
            .find(|d| d.dep_type == DependencyType::ParentChild)
        {
            obj.insert("parent".into(), parent_dep.depends_on_id.clone().into());
        }

        // Add resolved blocked_by (issues this one depends on as blocks)
        let blocked_by: Vec<_> = issue
            .dependencies
            .iter()
            .filter(|d| d.dep_type == DependencyType::Blocks)
            .map(|d| d.depends_on_id.as_str())
            .collect();
        if !blocked_by.is_empty() {
            obj.insert("blocked_by".into(), serde_json::to_value(&blocked_by)?);
        }

        // Add children
        if !children.is_empty() {
            let child_ids: Vec<_> = children.iter().map(|i| i.id.as_str()).collect();
            obj.insert("children".into(), serde_json::to_value(&child_ids)?);
        }

        // Add blocks (issues blocked by this one)
        if !blocked_by_this.is_empty() {
            let blocks_ids: Vec<_> = blocked_by_this.iter().map(|i| i.id.as_str()).collect();
            obj.insert("blocks".into(), serde_json::to_value(&blocks_ids)?);
        }

        println!("{}", serde_json::to_string_pretty(&val)?);
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

        if !children.is_empty() {
            println!();
            println!("{}", "Children:".bold());
            for child in &children {
                let status_indicator = if child.status.is_open() { "○" } else { "●" };
                println!(
                    "  {} {} [P{}] [{}] {}",
                    status_indicator,
                    child.id.cyan(),
                    child.priority,
                    child.issue_type.to_string().blue(),
                    child.title
                );
            }
        }

        if !blocked_by_this.is_empty() {
            println!();
            println!("{}", "Blocks:".bold());
            for blocked in &blocked_by_this {
                let status_indicator = if blocked.status.is_open() {
                    "○"
                } else {
                    "●"
                };
                println!(
                    "  {} {} [P{}] {}",
                    status_indicator,
                    blocked.id.cyan(),
                    blocked.priority,
                    blocked.title
                );
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
    edit: bool,
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
    if edit {
        let current = issue.description.clone().unwrap_or_default();
        issue.description = Some(open_editor_for_description(&current, &issue.title)?);
    } else if let Some(d) = description {
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
    let all_issues: Vec<_> = store.list(false);
    let open_issues: Vec<_> = all_issues
        .iter()
        .filter(|i| i.status.is_open())
        .copied()
        .collect();
    let graph = IssueGraph::from_issues(&open_issues);
    let mut ready = graph.ready_issues(&open_issues);

    // Sort by priority
    ready.sort_by_key(|a| a.priority);

    if json {
        let mut output = Vec::new();

        for issue in &ready {
            let mut val = serde_json::to_value(issue)?;
            if let Some(parent_dep) = issue
                .dependencies
                .iter()
                .find(|d| d.dep_type == DependencyType::ParentChild)
            {
                obj_mut(&mut val).insert("parent".into(), parent_dep.depends_on_id.clone().into());
            }
            output.push(val);
        }

        println!("{}", serde_json::to_string(&output)?);
    } else if ready.is_empty() {
        println!("No ready issues");
    } else {
        println!("{}", "Ready issues (unblocked):".bold());
        for issue in &ready {
            println!(
                "{} [P{}] [{}] - {}",
                issue.id.cyan(),
                issue.priority,
                issue.issue_type.to_string().blue(),
                issue.title
            );
        }

        // Show blocked issues with their blockers
        let ready_ids: std::collections::HashSet<_> = ready.iter().map(|i| i.id.as_str()).collect();
        let blocked: Vec<_> = open_issues
            .iter()
            .filter(|i| !ready_ids.contains(i.id.as_str()))
            .collect();

        if !blocked.is_empty() {
            println!();
            println!("{}", "Blocked issues:".bold());
            for issue in blocked {
                let blockers: Vec<String> = issue
                    .dependencies
                    .iter()
                    .filter(|d| d.dep_type == DependencyType::Blocks)
                    .filter(|d| open_issues.iter().any(|i| i.id == d.depends_on_id))
                    .map(|d| d.depends_on_id.clone())
                    .collect();
                println!(
                    "{} [P{}] [{}] - {} {} {}",
                    issue.id.cyan(),
                    issue.priority,
                    issue.issue_type.to_string().blue(),
                    issue.title,
                    "blocked by".red(),
                    blockers.join(", ").red()
                );
            }
        }
    }

    Ok(())
}

pub fn dep_add(id: &str, blocks: &str, json: bool) -> Result<()> {
    let mut store = UnifiedStore::open()?;
    let issue = store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    let added = issue.add_dependency(blocks.to_string(), DependencyType::Blocks);
    let issue = issue.clone();
    store.update(issue.clone())?;

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else if added {
        println!("{} {} now blocks {}", "✓".green(), id, blocks);
    } else {
        println!(
            "{} {} already has a dependency on {}",
            "!".yellow(),
            id,
            blocks
        );
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

pub fn dep_block(id: &str, by: &str, json: bool) -> Result<()> {
    let mut store = UnifiedStore::open()?;

    // Parse comma-separated blocker IDs
    let blocker_ids: Vec<&str> = by.split(',').map(|s| s.trim()).collect();

    // Validate all blocker IDs exist
    for blocker_id in &blocker_ids {
        if store.get(blocker_id).is_none() {
            bail!("Blocker issue not found: {}", blocker_id);
        }
    }

    let issue = store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    let mut added = Vec::new();
    let mut skipped = Vec::new();

    for blocker_id in &blocker_ids {
        if issue.add_dependency(blocker_id.to_string(), DependencyType::Blocks) {
            added.push(*blocker_id);
        } else {
            skipped.push(*blocker_id);
        }
    }

    let issue = issue.clone();
    store.update(issue.clone())?;

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else {
        if !added.is_empty() {
            println!("{} {} now blocked by {}", "✓".green(), id, added.join(", "));
        }
        if !skipped.is_empty() {
            println!(
                "{} Already had dependency on: {}",
                "!".yellow(),
                skipped.join(", ")
            );
        }
    }

    Ok(())
}

pub fn dep_unblock(id: &str, by: &str, json: bool) -> Result<()> {
    let mut store = UnifiedStore::open()?;

    let blocker_ids: Vec<&str> = by.split(',').map(|s| s.trim()).collect();

    let issue = store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    for blocker_id in &blocker_ids {
        issue.remove_dependency(blocker_id);
    }

    let issue = issue.clone();
    store.update(issue.clone())?;

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else {
        println!(
            "{} {} unblocked from {}",
            "✓".green(),
            id,
            blocker_ids.join(", ")
        );
    }

    Ok(())
}

pub fn dep_tree(id: &str, json: bool) -> Result<()> {
    let store = UnifiedStore::open()?;
    let issue = store
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    if json {
        let tree = build_dep_tree_json(&store, issue, &mut Vec::new());
        println!("{}", serde_json::to_string_pretty(&tree)?);
    } else {
        println!("{} {}", issue.id.cyan().bold(), issue.title.bold());
        print_dep_tree(&store, issue, "", true, &mut Vec::new());
    }

    Ok(())
}

fn build_dep_tree_json(
    store: &UnifiedStore,
    issue: &Issue,
    visited: &mut Vec<String>,
) -> serde_json::Value {
    if visited.contains(&issue.id) {
        return serde_json::json!({
            "id": issue.id,
            "title": issue.title,
            "cycle": true,
        });
    }
    visited.push(issue.id.clone());

    let children: Vec<serde_json::Value> = issue
        .dependencies
        .iter()
        .filter_map(|dep| {
            store.get(&dep.depends_on_id).map(|child| {
                let mut node = build_dep_tree_json(store, child, visited);
                obj_mut(&mut node).insert("dep_type".into(), dep.dep_type.to_string().into());
                node
            })
        })
        .collect();

    // Also find reverse deps: issues that depend on this one
    let dependents: Vec<serde_json::Value> = store
        .list(false)
        .iter()
        .filter(|i| {
            i.dependencies
                .iter()
                .any(|d| d.depends_on_id == issue.id)
        })
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "title": i.title,
                "dep_type": i.dependencies.iter().find(|d| d.depends_on_id == issue.id).map(|d| d.dep_type.to_string()).unwrap_or_default(),
            })
        })
        .collect();

    visited.pop();

    let mut node = serde_json::json!({
        "id": issue.id,
        "title": issue.title,
        "status": issue.status.to_string(),
    });

    if !children.is_empty() {
        obj_mut(&mut node).insert("depends_on".into(), children.into());
    }
    if !dependents.is_empty() {
        obj_mut(&mut node).insert("depended_on_by".into(), dependents.into());
    }

    node
}

fn print_dep_tree(
    store: &UnifiedStore,
    issue: &Issue,
    prefix: &str,
    _is_last: bool,
    visited: &mut Vec<String>,
) {
    if visited.contains(&issue.id) {
        return;
    }
    visited.push(issue.id.clone());

    // Print dependencies (what this issue depends on)
    let deps: Vec<_> = issue.dependencies.iter().collect();
    for (i, dep) in deps.iter().enumerate() {
        let is_last_dep = i == deps.len() - 1;
        let connector = if is_last_dep {
            "└── "
        } else {
            "├── "
        };
        let child_prefix = if is_last_dep { "    " } else { "│   " };

        let type_label = match dep.dep_type {
            DependencyType::Blocks => "blocked by",
            DependencyType::ParentChild => "child of",
            DependencyType::Related => "related to",
        };

        if let Some(target) = store.get(&dep.depends_on_id) {
            let status_indicator = if target.status.is_open() {
                "○".yellow()
            } else {
                "●".green()
            };
            println!(
                "{}{}{} {} {} [{}]",
                prefix,
                connector,
                status_indicator,
                dep.depends_on_id.cyan(),
                target.title,
                type_label.dimmed()
            );
            print_dep_tree(
                store,
                target,
                &format!("{}{}", prefix, child_prefix),
                is_last_dep,
                visited,
            );
        } else {
            println!(
                "{}{}{} {} [missing]",
                prefix,
                connector,
                "✗".red(),
                dep.depends_on_id
            );
        }
    }

    visited.pop();
}

// ============================================================================
// Batch create (trx-pe3m)
// ============================================================================

pub fn create_many(json_input: &str, dry_run: bool, json: bool) -> Result<()> {
    use std::io::Read;

    let input = if json_input == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        std::fs::read_to_string(json_input)?
    };

    let items: Vec<serde_json::Value> = serde_json::from_str(&input)?;

    if dry_run {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "dry_run": true,
                    "count": items.len(),
                    "items": items,
                }))?
            );
        } else {
            println!("{} Would create {} issue(s):", "⊘".yellow(), items.len());
            for (i, item) in items.iter().enumerate() {
                let title = item["title"].as_str().unwrap_or("(no title)");
                let itype = item["issue_type"].as_str().unwrap_or("task");
                let priority = item["priority"].as_u64().unwrap_or(2);
                println!("  {}. [P{}] [{}] {}", i + 1, priority, itype, title);
            }
        }
        return Ok(());
    }

    let mut store = UnifiedStore::open()?;
    let prefix = store.prefix()?;
    let mut results: Vec<serde_json::Value> = Vec::new();

    for (i, item) in items.iter().enumerate() {
        let title = item["title"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Item {} missing 'title'", i))?;
        let itype_str = item["issue_type"].as_str().unwrap_or("task");
        let priority = item["priority"].as_u64().unwrap_or(2) as u8;
        let description = item["description"].as_str().map(|s| s.to_string());
        let parent = item["parent"].as_str().map(|s| s.to_string());

        let id = if let Some(ref parent_id) = parent {
            let child_num = store.next_child_num(parent_id);
            trx_core::id::generate_child_id(parent_id, child_num)
        } else {
            generate_id(&prefix)
        };

        let mut issue = Issue::new(id.clone(), title.to_string());
        issue.issue_type = itype_str.parse()?;
        issue.priority = priority;
        issue.description = description;

        if let Some(ref parent_id) = parent {
            issue.add_dependency(parent_id.clone(), DependencyType::ParentChild);
        }

        // Handle blocks array
        if let Some(blocks) = item["blocks"].as_array() {
            for b in blocks {
                if let Some(bid) = b.as_str() {
                    issue.add_dependency(bid.to_string(), DependencyType::Blocks);
                }
            }
        }

        store.create(issue)?;

        results.push(serde_json::json!({
            "index": i,
            "id": id,
            "title": title,
            "status": "created",
        }));
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        println!("{} Created {} issue(s):", "✓".green(), results.len());
        for r in &results {
            println!(
                "  {} {}",
                r["id"].as_str().unwrap_or_default().cyan(),
                r["title"].as_str().unwrap_or_default()
            );
        }
    }

    Ok(())
}

// ============================================================================
// Plan import (trx-btfs)
// ============================================================================

pub fn plan_import(
    path: &str,
    epic_title: Option<String>,
    priority: u8,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let content = std::fs::read_to_string(path)?;

    // Detect format by extension
    let items = if path.ends_with(".json") {
        parse_plan_json(&content)?
    } else {
        parse_plan_markdown(&content, epic_title.as_deref())?
    };

    if items.is_empty() {
        bail!("No items found in plan file");
    }

    let epic_item = &items[0];
    let children = &items[1..];

    if dry_run {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "dry_run": true,
                    "epic": epic_item,
                    "children": children,
                }))?
            );
        } else {
            println!("{} Would create:", "⊘".yellow());
            println!(
                "  Epic: [P{}] {}",
                epic_item["priority"].as_u64().unwrap_or(priority as u64),
                epic_item["title"].as_str().unwrap_or("?")
            );
            for (i, child) in children.iter().enumerate() {
                let itype = child["issue_type"].as_str().unwrap_or("task");
                let p = child["priority"].as_u64().unwrap_or(priority as u64);
                println!(
                    "  {}. [P{}] [{}] {}",
                    i + 1,
                    p,
                    itype,
                    child["title"].as_str().unwrap_or("?")
                );
            }
        }
        return Ok(());
    }

    let mut store = UnifiedStore::open()?;
    let prefix = store.prefix()?;

    // Create the epic
    let epic_id = generate_id(&prefix);
    let mut epic = Issue::new(
        epic_id.clone(),
        epic_item["title"]
            .as_str()
            .unwrap_or("Plan Epic")
            .to_string(),
    );
    epic.issue_type = trx_core::IssueType::Epic;
    epic.priority = epic_item["priority"].as_u64().unwrap_or(priority as u64) as u8;
    if let Some(desc) = epic_item["description"].as_str() {
        epic.description = Some(desc.to_string());
    }
    store.create(epic)?;

    let mut created_ids = vec![epic_id.clone()];

    // Create children
    for child_item in children {
        let child_num = store.next_child_num(&epic_id);
        let child_id = trx_core::id::generate_child_id(&epic_id, child_num);

        let mut child = Issue::new(
            child_id.clone(),
            child_item["title"].as_str().unwrap_or("Task").to_string(),
        );
        child.issue_type = child_item["issue_type"]
            .as_str()
            .unwrap_or("task")
            .parse()
            .unwrap_or(trx_core::IssueType::Task);
        child.priority = child_item["priority"].as_u64().unwrap_or(priority as u64) as u8;
        if let Some(desc) = child_item["description"].as_str() {
            child.description = Some(desc.to_string());
        }
        child.add_dependency(epic_id.clone(), DependencyType::ParentChild);

        // Handle blocks references (by title match within this plan)
        if let Some(blocks) = child_item["blocks"].as_array() {
            for b in blocks {
                if let Some(bid) = b.as_str() {
                    child.add_dependency(bid.to_string(), DependencyType::Blocks);
                }
            }
        }

        store.create(child)?;
        created_ids.push(child_id);
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "epic_id": epic_id,
                "children_created": created_ids.len() - 1,
                "ids": created_ids,
            }))?
        );
    } else {
        println!(
            "{} Created epic {} with {} children",
            "✓".green(),
            epic_id.cyan(),
            created_ids.len() - 1
        );
        for id in &created_ids {
            let Some(issue) = store.get(id) else { continue };
            println!(
                "  {} [P{}] [{}] {}",
                issue.id.cyan(),
                issue.priority,
                issue.issue_type.to_string().blue(),
                issue.title
            );
        }
    }

    Ok(())
}

pub fn plan_example(format: &str) -> Result<()> {
    let md = r#"# Backend API Overhaul

Modernize the backend to support multi-tenant architecture.

## Database schema migration [task] [P1]

Redesign the schema for tenant isolation.
Add tenant_id columns, update indexes, write migration scripts.

## Auth service extraction [feature] [P1]

Extract authentication into a standalone microservice.
Must support OAuth2, SAML, and API key flows.

## Rate limiting middleware [task] [P2]

Implement per-tenant rate limiting with Redis.

## Observability stack [feature] [P3]

Add structured logging, distributed tracing, and metrics dashboards.
"#;

    let json = r#"{
  "title": "Backend API Overhaul",
  "description": "Modernize the backend to support multi-tenant architecture.",
  "priority": 1,
  "children": [
    {
      "title": "Database schema migration",
      "issue_type": "task",
      "priority": 1,
      "description": "Redesign the schema for tenant isolation.\nAdd tenant_id columns, update indexes, write migration scripts."
    },
    {
      "title": "Auth service extraction",
      "issue_type": "feature",
      "priority": 1,
      "description": "Extract authentication into a standalone microservice.\nMust support OAuth2, SAML, and API key flows."
    },
    {
      "title": "Rate limiting middleware",
      "issue_type": "task",
      "priority": 2
    },
    {
      "title": "Observability stack",
      "issue_type": "feature",
      "priority": 3,
      "description": "Add structured logging, distributed tracing, and metrics dashboards."
    }
  ]
}"#;

    match format {
        "md" | "markdown" => {
            println!("{}", md);
        }
        "json" => {
            println!("{}", json);
        }
        _ => {
            println!("{}", "=== Markdown format ===".bold());
            println!("Save as plan.md, then run: trx plan import plan.md");
            println!();
            println!("{}", md);
            println!("{}", "=== JSON format ===".bold());
            println!("Save as plan.json, then run: trx plan import plan.json");
            println!();
            println!("{}", json);
            println!();
            println!("{}", "=== Markdown tips ===".bold());
            println!("  # heading        → epic title + description (lines before first ##)");
            println!("  ## heading       → child issue title");
            println!("  [task]           → issue type (bug, feature, task, epic, chore)");
            println!("  [P0]..[P4]       → priority (0=critical .. 4=backlog)");
            println!("  Body under ##    → child description");
        }
    }

    Ok(())
}

fn parse_plan_json(content: &str) -> Result<Vec<serde_json::Value>> {
    let value: serde_json::Value = serde_json::from_str(content)?;
    if let Some(arr) = value.as_array() {
        Ok(arr.clone())
    } else if value.is_object() {
        // Single epic object with "children" array
        let mut items = vec![value.clone()];
        if let Some(children) = value["children"].as_array() {
            items.extend(children.clone());
        }
        Ok(items)
    } else {
        bail!("Expected JSON array or object with 'children'");
    }
}

fn parse_plan_markdown(content: &str, epic_title: Option<&str>) -> Result<Vec<serde_json::Value>> {
    let mut items: Vec<serde_json::Value> = Vec::new();

    // First item is the epic
    let title = epic_title
        .or_else(|| {
            // Use first H1 as epic title
            content
                .lines()
                .find(|l| l.starts_with("# ") && !l.starts_with("## "))
                .map(|l| l.trim_start_matches("# ").trim())
        })
        .ok_or_else(|| anyhow::anyhow!("Epic title required (use --epic or add a # heading)"))?;

    // Collect the top-level description (lines before first ## heading)
    let mut epic_desc_lines = Vec::new();
    let mut in_preamble = false;
    for line in content.lines() {
        if line.starts_with("# ") && !line.starts_with("## ") {
            in_preamble = true;
            continue;
        }
        if line.starts_with("## ") {
            break;
        }
        if in_preamble {
            epic_desc_lines.push(line);
        }
    }
    let epic_desc = epic_desc_lines.join("\n").trim().to_string();

    let mut epic = serde_json::json!({
        "title": title,
        "issue_type": "epic",
        "priority": 2,
    });
    if !epic_desc.is_empty() {
        obj_mut(&mut epic).insert("description".into(), epic_desc.into());
    }
    items.push(epic);

    // Parse ## headings as children
    let mut current_title: Option<String> = None;
    let mut current_desc = Vec::new();
    let mut current_type = "task";
    let mut current_priority: u64 = 2;

    let flush_child = |items: &mut Vec<serde_json::Value>,
                       title: &Option<String>,
                       desc: &[String],
                       itype: &str,
                       prio: u64| {
        if let Some(t) = title {
            let mut child = serde_json::json!({
                "title": t,
                "issue_type": itype,
                "priority": prio,
            });
            let desc_text = desc.join("\n").trim().to_string();
            if !desc_text.is_empty() {
                obj_mut(&mut child).insert("description".into(), desc_text.into());
            }
            items.push(child);
        }
    };

    for line in content.lines() {
        if line.starts_with("## ") {
            // Flush previous
            flush_child(
                &mut items,
                &current_title,
                &current_desc,
                current_type,
                current_priority,
            );

            let heading = line.trim_start_matches("## ").trim();

            // Parse optional metadata in heading: ## Title [type] [P2]
            let mut title_str = heading.to_string();
            current_type = "task";
            current_priority = 2;

            // Extract type tag like [bug], [feature], etc.
            for tag in ["bug", "feature", "task", "epic", "chore"] {
                let pattern = format!("[{}]", tag);
                if title_str.contains(&pattern) {
                    current_type = match tag {
                        "bug" => "bug",
                        "feature" => "feature",
                        "epic" => "epic",
                        "chore" => "chore",
                        _ => "task",
                    };
                    title_str = title_str.replace(&pattern, "").trim().to_string();
                }
            }

            // Extract priority tag like [P0], [P1], etc.
            for p in 0..=4u64 {
                let pattern = format!("[P{}]", p);
                if title_str.contains(&pattern) {
                    current_priority = p;
                    title_str = title_str.replace(&pattern, "").trim().to_string();
                }
            }

            current_title = Some(title_str);
            current_desc = Vec::new();
        } else if current_title.is_some() {
            current_desc.push(line.to_string());
        }
    }

    // Flush last child
    flush_child(
        &mut items,
        &current_title,
        &current_desc,
        current_type,
        current_priority,
    );

    Ok(items)
}

// ============================================================================
// Editor workflow (trx-ne4f)
// ============================================================================

fn open_editor_for_description(current: &str, title: &str) -> Result<String> {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .map_err(|_| anyhow::anyhow!("No $EDITOR or $VISUAL set. Cannot open editor."))?;

    // Create temp file with template
    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!("trx-edit-{}.md", std::process::id()));

    let template = if current.is_empty() {
        format!(
            "# {}\n\n<!-- Write description below. Lines starting with # are kept. -->\n<!-- Save and close editor to confirm. Empty file cancels. -->\n\n## Context\n\n\n## Scope\n\n\n## Acceptance Criteria\n\n- \n",
            title
        )
    } else {
        current.to_string()
    };

    std::fs::write(&tmp_path, &template)?;

    let status = std::process::Command::new(&editor)
        .arg(&tmp_path)
        .status()?;

    if !status.success() {
        let _ = std::fs::remove_file(&tmp_path);
        bail!("Editor exited with non-zero status");
    }

    let result = std::fs::read_to_string(&tmp_path)?;
    let _ = std::fs::remove_file(&tmp_path);

    // Strip comment lines (<!-- ... -->)
    let cleaned: Vec<&str> = result
        .lines()
        .filter(|l| !l.trim_start().starts_with("<!--"))
        .collect();

    let cleaned = cleaned.join("\n").trim().to_string();

    if cleaned.is_empty() {
        bail!("Empty description — cancelled");
    }

    Ok(cleaned)
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
        if let Some(parent) = trx_dir.parent() {
            println!("  rm -rf {}/.trx/crdt", parent.display());
        }
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
    println!("ISSUES.md conflicts will now be auto-resolved during git merge/rebase.");
    println!("Remember to commit .gitattributes so the driver applies for all contributors.");

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
        let filtered: Vec<&str> = content.lines().filter(|l| l.trim() != attr_line).collect();

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
                .unwrap_or(std::path::Path::new("."))
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
                .unwrap_or(std::path::Path::new("."))
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
                .unwrap_or(std::path::Path::new("."))
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
