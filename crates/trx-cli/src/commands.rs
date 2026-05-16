//! CLI command implementations

use anyhow::{Result, bail};
use colored::Colorize;
use trx_core::{
    AgentCtx, DependencyType, Event, EventAction, EventLog, Issue, IssueGraph, IssueType,
    SessionSummary, Status, Store, diff_issue, enrich_issue, generate_id, id::generate_child_id,
    summarize_sessions,
};

/// Append an event to `.trx/events.jsonl`. Failures are logged to stderr but
/// never break the command — losing an audit entry is preferable to refusing
/// the user's mutation.
fn emit_event(store: &Store, event: Event) {
    let log = EventLog::at(&store.trx_dir());
    if let Err(e) = log.append(&event) {
        eprintln!("warning: failed to append event log: {}", e);
    }
}

/// Pick the action that best describes a status transition; falls back to
/// `Updated` when status didn't change.
fn action_for_update(before: &Issue, after: &Issue) -> EventAction {
    match (before.status.is_closed(), after.status.is_closed()) {
        (false, true) => EventAction::Closed,
        (true, false) => EventAction::Reopened,
        _ => EventAction::Updated,
    }
}

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

/// Read description from stdin when value is "-"
fn read_description(description: Option<String>) -> Result<Option<String>> {
    match description.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
            let trimmed = buf.trim().to_string();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed))
            }
        }
        _ => Ok(description),
    }
}

pub fn create(
    title: &str,
    issue_type: &str,
    priority: u8,
    description: Option<String>,
    parent: Option<String>,
    custom_prefix: Option<String>,
    edit: bool,
    json: bool,
) -> Result<()> {
    let mut store = Store::open()?;
    let prefix = custom_prefix.unwrap_or(store.prefix()?);

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
        read_description(description)?
    };

    if let Some(ref parent_id) = parent {
        issue.add_dependency(parent_id.clone(), DependencyType::ParentChild);
    }

    let ctx = AgentCtx::from_env();
    enrich_issue(&mut issue, &ctx);

    store.create(issue.clone())?;
    emit_event(&store, Event::new(&issue.id, EventAction::Created, &ctx));

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else {
        println!("{} Created issue: {}", "✓".green(), id);
        println!("  Title: {}", title);
        println!("  Priority: P{}", priority);
    }

    Ok(())
}

/// Parse a date string that can be ISO 8601 or relative (e.g., "1 week", "2 days", "3 hours")
fn parse_date(s: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    // Try ISO 8601 first
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.into());
    }
    // Try date-only ISO
    if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let dt = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| anyhow::anyhow!("Invalid date: {}", s))?;
        return Ok(chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
            dt,
            chrono::Utc,
        ));
    }
    // Try relative: "N unit" where unit is day(s), week(s), hour(s), month(s)
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() == 2
        && let Ok(n) = parts[0].parse::<i64>()
    {
        let unit = parts[1].to_lowercase();
        let duration = match unit.trim_end_matches('s') {
            "hour" => chrono::Duration::hours(n),
            "day" => chrono::Duration::days(n),
            "week" => chrono::Duration::weeks(n),
            "month" => chrono::Duration::days(n * 30),
            _ => bail!(
                "Unknown time unit: {}. Use hour(s), day(s), week(s), month(s)",
                unit
            ),
        };
        return Ok(chrono::Utc::now() - duration);
    }
    bail!(
        "Cannot parse date: '{}'. Use ISO format (2024-01-15) or relative (1 week, 2 days)",
        s
    )
}

/// Resolve 'me' assignee to current git user
fn resolve_assignee(assignee: &str) -> String {
    if assignee.eq_ignore_ascii_case("me") {
        // Try git config first, then env
        std::process::Command::new("git")
            .args(["config", "user.name"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "me".to_string())
    } else {
        assignee.to_string()
    }
}

#[derive(Clone, Copy)]
enum SortKey {
    Priority,
    Created,
    Updated,
    Closed,
    Id,
    Status,
}

impl SortKey {
    fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "priority" | "prio" | "p" => Ok(SortKey::Priority),
            "created" | "created_at" | "date" | "age" => Ok(SortKey::Created),
            "updated" | "updated_at" | "modified" => Ok(SortKey::Updated),
            "closed" | "closed_at" => Ok(SortKey::Closed),
            "id" => Ok(SortKey::Id),
            "status" => Ok(SortKey::Status),
            other => bail!(
                "invalid --sort value '{}'. Expected: priority, created, updated, closed, id, status",
                other
            ),
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortKey::Priority => "priority",
            SortKey::Created => "created",
            SortKey::Updated => "updated",
            SortKey::Closed => "closed",
            SortKey::Id => "id",
            SortKey::Status => "status",
        }
    }
}

pub fn list(
    status: Option<String>,
    issue_type: Option<String>,
    priority: Option<u8>,
    search: Option<String>,
    epic: Option<String>,
    all: bool,
    limit: Option<usize>,
    labels: Vec<String>,
    assignee: Option<String>,
    created_after: Option<String>,
    created_before: Option<String>,
    sort: String,
    reverse: bool,
    json: bool,
) -> Result<()> {
    let sort_key = SortKey::parse(&sort)?;
    let store = Store::open()?;

    // Use list(false) to get all issues if:
    // - --all flag is set
    // - --epic is specified (need all to find descendants)
    // - --status is specified (may need closed issues)
    let need_all_issues = all || epic.is_some() || status.is_some();
    let mut issues: Vec<_> = if need_all_issues {
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

    // Filter by label (AND: issue must have ALL specified labels)
    if !labels.is_empty() {
        issues.retain(|i| {
            labels
                .iter()
                .all(|l| i.labels.iter().any(|il| il.eq_ignore_ascii_case(l)))
        });
    }

    // Filter by assignee
    if let Some(ref a) = assignee {
        let resolved = resolve_assignee(a);
        let resolved_lower = resolved.to_lowercase();
        issues.retain(|i| {
            i.assignee
                .as_ref()
                .is_some_and(|ia| ia.to_lowercase().contains(&resolved_lower))
        });
    }

    // Filter by created_after
    if let Some(ref after) = created_after {
        let after_dt = parse_date(after)?;
        issues.retain(|i| i.created_at >= after_dt);
    }

    // Filter by created_before
    if let Some(ref before) = created_before {
        let before_dt = parse_date(before)?;
        issues.retain(|i| i.created_at <= before_dt);
    }

    // Sort by the requested key. Date sorts are newest-first by default; other
    // sorts are ascending by default. `--reverse` flips whichever default applies.
    match sort_key {
        SortKey::Priority => issues.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| b.created_at.cmp(&a.created_at))
        }),
        SortKey::Created => issues.sort_by_key(|i| std::cmp::Reverse(i.created_at)),
        SortKey::Updated => issues.sort_by_key(|i| std::cmp::Reverse(i.updated_at)),
        SortKey::Closed => issues.sort_by_key(|i| std::cmp::Reverse(i.closed_at)),
        SortKey::Id => issues.sort_by(|a, b| a.id.cmp(&b.id)),
        SortKey::Status => issues.sort_by(|a, b| {
            (a.status as u8)
                .cmp(&(b.status as u8))
                .then_with(|| b.created_at.cmp(&a.created_at))
        }),
    }
    if reverse {
        issues.reverse();
    }

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
        let show_date = matches!(
            sort_key,
            SortKey::Created | SortKey::Updated | SortKey::Closed
        );
        if show_date {
            eprintln!("Sorted by {}", sort_key.label());
        }
        for issue in issues {
            let status_color = match issue.status {
                Status::Open => "open".white(),
                Status::InProgress => "in_progress".yellow(),
                Status::Blocked => "blocked".red(),
                Status::Closed => "closed".green(),
                Status::Tombstone => "tombstone".dimmed(),
            };
            if show_date {
                let dt = match sort_key {
                    SortKey::Created => Some(issue.created_at),
                    SortKey::Updated => Some(issue.updated_at),
                    SortKey::Closed => issue.closed_at,
                    _ => None,
                };
                let date_str = dt
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "----------".into());
                println!(
                    "{} {} [P{}] [{}] {} - {}",
                    date_str.dimmed(),
                    issue.id.cyan(),
                    issue.priority,
                    issue.issue_type.to_string().blue(),
                    status_color,
                    issue.title
                );
            } else {
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
    }

    Ok(())
}

pub fn show(id: &str, json: bool) -> Result<()> {
    let store = Store::open()?;
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

        // Recent activity (best-effort: skip silently on read errors so we
        // never fail `show` because of a corrupt event log line).
        if let Ok(events) = EventLog::at(&store.trx_dir()).read_all() {
            let mut for_issue: Vec<&Event> = events.iter().filter(|e| e.issue_id == id).collect();
            for_issue.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
            for_issue.truncate(5);
            if !for_issue.is_empty() {
                println!();
                println!("{}", "Recent activity:".bold());
                for e in for_issue {
                    print_event_line(e);
                }
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
    clear: Vec<String>,
    json: bool,
) -> Result<()> {
    let mut store = Store::open()?;
    let issue = store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;
    let before = issue.clone();

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
        issue.description = read_description(Some(d))?;
    }

    // Handle --clear flags
    for field in &clear {
        match field.to_lowercase().as_str() {
            "description" | "desc" => issue.description = None,
            "parent" => issue
                .dependencies
                .retain(|d| d.dep_type != DependencyType::ParentChild),
            "labels" => issue.labels.clear(),
            "assignee" => issue.assignee = None,
            "notes" => issue.notes = None,
            "sessions" => issue.sessions.clear(),
            other => bail!(
                "Unknown field to clear: '{}'. Use: description, parent, labels, assignee, notes, sessions",
                other
            ),
        }
    }

    issue.updated_at = chrono::Utc::now();
    let issue = issue.clone();
    store.update(issue.clone())?;

    let ctx = AgentCtx::from_env();
    let changes = diff_issue(&before, &issue);
    if !changes.is_empty() {
        let action = action_for_update(&before, &issue);
        emit_event(
            &store,
            Event::new(&issue.id, action, &ctx).with_changes(changes),
        );
    }

    if json {
        println!("{}", serde_json::to_string(&issue)?);
    } else {
        println!("{} Updated {}", "✓".green(), id);
    }

    Ok(())
}

pub fn close(ids: &[String], reason: Option<String>, json: bool) -> Result<()> {
    let mut store = Store::open()?;
    let ctx = AgentCtx::from_env();
    let mut closed: Vec<Issue> = Vec::new();

    for id in ids {
        let issue = store
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;
        issue.close(reason.clone());
        let snap = issue.clone();
        store.update(snap.clone())?;

        let mut event = Event::new(&snap.id, EventAction::Closed, &ctx);
        if let Some(r) = &reason {
            event = event.with_note(r.clone());
        }
        emit_event(&store, event);
        closed.push(snap);
    }

    if json {
        println!("{}", serde_json::to_string(&closed)?);
    } else {
        for issue in &closed {
            println!("{} Closed {}", "✓".green(), issue.id);
        }
    }

    Ok(())
}

pub fn ready(
    issue_type: Option<String>,
    priority: Option<u8>,
    label: Vec<String>,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let store = Store::open()?;
    let all_issues: Vec<_> = store.list(false);
    let open_issues: Vec<_> = all_issues
        .iter()
        .filter(|i| i.status.is_open())
        .copied()
        .collect();
    let graph = IssueGraph::from_issues(&open_issues);
    let ready_all = graph.ready_issues(&open_issues);
    let ready_ids: std::collections::HashSet<_> = ready_all.iter().map(|i| i.id.as_str()).collect();

    // Partition open issues into ready vs. truly-blocked BEFORE filtering, so
    // a filter that hides a ready issue does not misclassify it as blocked.
    let blocked_all: Vec<_> = open_issues
        .iter()
        .filter(|i| !ready_ids.contains(i.id.as_str()))
        .copied()
        .collect();

    let type_filter = match issue_type {
        Some(s) => Some(s.parse::<IssueType>()?),
        None => None,
    };
    let matches = |i: &&Issue| -> bool {
        if let Some(t) = type_filter
            && i.issue_type != t
        {
            return false;
        }
        if let Some(p) = priority
            && i.priority != p
        {
            return false;
        }
        for l in &label {
            if !i.labels.iter().any(|il| il == l) {
                return false;
            }
        }
        true
    };

    let mut ready: Vec<&Issue> = ready_all.into_iter().filter(matches).collect();
    let blocked: Vec<&Issue> = blocked_all.into_iter().filter(matches).collect();
    ready.sort_by_key(|a| a.priority);
    if let Some(n) = limit {
        ready.truncate(n);
    }

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

        if !blocked.is_empty() {
            println!();
            println!("{}", "Blocked issues:".bold());
            for issue in &blocked {
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

pub fn dep_block(id: &str, by: &str, json: bool) -> Result<()> {
    let mut store = Store::open()?;

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

    if !added.is_empty() {
        let ctx = AgentCtx::from_env();
        for blocker_id in &added {
            emit_event(
                &store,
                Event::new(&issue.id, EventAction::DepAdded, &ctx).with_note(*blocker_id),
            );
        }
    }

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
    let mut store = Store::open()?;

    let blocker_ids: Vec<&str> = by.split(',').map(|s| s.trim()).collect();

    let issue = store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;

    let mut removed = Vec::new();
    for blocker_id in &blocker_ids {
        if issue
            .dependencies
            .iter()
            .any(|d| d.depends_on_id == *blocker_id)
        {
            removed.push(*blocker_id);
        }
        issue.remove_dependency(blocker_id);
    }

    let issue = issue.clone();
    store.update(issue.clone())?;

    if !removed.is_empty() {
        let ctx = AgentCtx::from_env();
        for blocker_id in &removed {
            emit_event(
                &store,
                Event::new(&issue.id, EventAction::DepRemoved, &ctx).with_note(*blocker_id),
            );
        }
    }

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
    let store = Store::open()?;
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
    store: &Store,
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
    store: &Store,
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

    let mut store = Store::open()?;
    let prefix = store.prefix()?;
    let ctx = AgentCtx::from_env();
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

        enrich_issue(&mut issue, &ctx);
        store.create(issue)?;
        emit_event(&store, Event::new(&id, EventAction::Created, &ctx));

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

    let mut store = Store::open()?;
    let prefix = store.prefix()?;
    let ctx = AgentCtx::from_env();

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
    enrich_issue(&mut epic, &ctx);
    store.create(epic)?;
    emit_event(&store, Event::new(&epic_id, EventAction::Created, &ctx));

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

        enrich_issue(&mut child, &ctx);
        store.create(child)?;
        emit_event(&store, Event::new(&child_id, EventAction::Created, &ctx));
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

pub fn sync(message: Option<String>, dry_run: bool, no_commit: bool) -> Result<()> {
    let store = Store::open()?;
    let trx_dir = store.trx_dir();

    if dry_run {
        // Show what would be staged
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain", "--", &trx_dir.to_string_lossy()])
            .output()?;
        let changes = String::from_utf8_lossy(&output.stdout);
        if changes.trim().is_empty() {
            println!("Nothing to sync");
        } else {
            println!("{} Would sync these changes:", "⊘".yellow());
            for line in changes.lines() {
                println!("  {}", line);
            }
        }
        return Ok(());
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

    if no_commit {
        println!("{} Staged .trx/ (not committed)", "✓".green());
        return Ok(());
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

// ============================================================================
// Handover (trx-te7r)
// ============================================================================

pub fn handover(json: bool) -> Result<()> {
    let store = Store::open()?;
    let all_issues: Vec<_> = store.list(false);
    let open_issues: Vec<_> = all_issues
        .iter()
        .filter(|i| i.status.is_open())
        .copied()
        .collect();
    let graph = IssueGraph::from_issues(&open_issues);
    let ready = graph.ready_issues(&open_issues);
    let ready_ids: std::collections::HashSet<_> = ready.iter().map(|i| i.id.as_str()).collect();

    if json {
        let mut output = Vec::new();
        // Topological: ready first, then blocked
        let mut sorted_open: Vec<_> = open_issues.iter().collect();
        sorted_open.sort_by(|a, b| {
            let a_ready = ready_ids.contains(a.id.as_str());
            let b_ready = ready_ids.contains(b.id.as_str());
            b_ready
                .cmp(&a_ready)
                .then_with(|| a.priority.cmp(&b.priority))
        });
        for issue in sorted_open {
            let blocked_by: Vec<&str> = issue
                .dependencies
                .iter()
                .filter(|d| d.dep_type == DependencyType::Blocks)
                .filter(|d| open_issues.iter().any(|i| i.id == d.depends_on_id))
                .map(|d| d.depends_on_id.as_str())
                .collect();
            output.push(serde_json::json!({
                "id": issue.id,
                "title": issue.title,
                "priority": issue.priority,
                "type": issue.issue_type.to_string(),
                "status": issue.status.to_string(),
                "ready": ready_ids.contains(issue.id.as_str()),
                "blocked_by": blocked_by,
            }));
        }
        println!("{}", serde_json::to_string(&output)?);
    } else {
        // Compact one-line-per-issue summary
        let mut sorted_open: Vec<_> = open_issues.iter().collect();
        sorted_open.sort_by(|a, b| {
            let a_ready = ready_ids.contains(a.id.as_str());
            let b_ready = ready_ids.contains(b.id.as_str());
            b_ready
                .cmp(&a_ready)
                .then_with(|| a.priority.cmp(&b.priority))
        });
        for issue in sorted_open {
            let marker = if ready_ids.contains(issue.id.as_str()) {
                "▶"
            } else {
                "◼"
            };
            let blocked_by: Vec<String> = issue
                .dependencies
                .iter()
                .filter(|d| d.dep_type == DependencyType::Blocks)
                .filter(|d| open_issues.iter().any(|i| i.id == d.depends_on_id))
                .map(|d| d.depends_on_id.clone())
                .collect();
            let suffix = if blocked_by.is_empty() {
                String::new()
            } else {
                format!(" ← {}", blocked_by.join(","))
            };
            println!(
                "{} {} P{} [{}] {}{}",
                marker, issue.id, issue.priority, issue.issue_type, issue.title, suffix
            );
        }
    }
    Ok(())
}

// ============================================================================
// Search (trx-msj1)
// ============================================================================

pub fn search(query: &str, all_repos: bool, json: bool) -> Result<()> {
    let q_lower = query.to_lowercase();

    let mut results: Vec<(String, Issue)> = Vec::new(); // (repo_name, issue)

    if all_repos {
        // Find sibling repos with .trx/
        let current = std::env::current_dir()?;
        if let Some(parent) = current.parent() {
            for entry in std::fs::read_dir(parent)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() && path.join(".trx").exists() {
                    let repo_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    // Try to open the store from that directory
                    let prev_dir = std::env::current_dir()?;
                    if std::env::set_current_dir(&path).is_ok() {
                        if let Ok(store) = Store::open() {
                            for issue in store.list(false) {
                                if matches_search(issue, &q_lower) {
                                    results.push((repo_name.clone(), issue.clone()));
                                }
                            }
                        }
                        let _ = std::env::set_current_dir(&prev_dir);
                    }
                }
            }
        }
    } else {
        let store = Store::open()?;
        let repo_name = std::env::current_dir()?
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        for issue in store.list(false) {
            if matches_search(issue, &q_lower) {
                results.push((repo_name.clone(), issue.clone()));
            }
        }
    }

    if json {
        let output: Vec<serde_json::Value> = results
            .iter()
            .map(|(repo, issue)| {
                serde_json::json!({
                    "source": "trx",
                    "source_repo": repo,
                    "id": issue.id,
                    "title": issue.title,
                    "content": issue.description,
                    "status": issue.status.to_string(),
                    "priority": issue.priority,
                    "labels": issue.labels,
                    "created_at": issue.created_at.to_rfc3339(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&output)?);
    } else if results.is_empty() {
        println!("No issues found matching '{}'", query);
    } else {
        for (repo, issue) in &results {
            println!(
                "[{}] {} [P{}] [{}] {} - {}",
                repo.cyan(),
                issue.id.cyan(),
                issue.priority,
                issue.issue_type.to_string().blue(),
                issue.status,
                issue.title
            );
        }
        println!("\n{} result(s)", results.len());
    }

    Ok(())
}

fn matches_search(issue: &Issue, q_lower: &str) -> bool {
    issue.title.to_lowercase().contains(q_lower)
        || issue
            .description
            .as_ref()
            .is_some_and(|d| d.to_lowercase().contains(q_lower))
        || issue.id.to_lowercase().contains(q_lower)
        || issue
            .labels
            .iter()
            .any(|l| l.to_lowercase().contains(q_lower))
}

pub fn import(path: &str, prefix: Option<String>, json: bool) -> Result<()> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let mut store = Store::open()?;
    let new_prefix = prefix.unwrap_or_else(|| store.prefix().unwrap_or_else(|_| "trx".to_string()));
    let ctx = AgentCtx::from_env();

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
            let id = issue.id.clone();
            store.create(issue)?;
            emit_event(&store, Event::new(&id, EventAction::Created, &ctx));
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
    let store = Store::open()?;
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
    let store = Store::open()?;
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
    let store = Store::open()?;
    let config_path = store.trx_dir().join("config.toml");

    let default_config = trx_core::Config::default_with_comments();
    std::fs::write(&config_path, default_config)?;

    println!("{} Configuration reset to defaults", "✓".green());
    Ok(())
}

/// Get a specific config value
pub fn config_get(key: &str, json: bool) -> Result<()> {
    let store = Store::open()?;
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
    let store = Store::open()?;
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

pub fn info(json: bool) -> Result<()> {
    use trx_core::AgentCtx;

    let ctx = AgentCtx::from_env();

    // Store summary (best-effort: missing .trx is fine, just report it).
    let store_info = match Store::open() {
        Ok(store) => {
            let trx_dir = store.trx_dir();
            let issues = store.list(true);
            let issue_count = issues.len();
            let open_count = issues.iter().filter(|i| i.status.is_open()).count();
            let closed_count = issues.iter().filter(|i| i.status.is_closed()).count();
            let events_count = EventLog::at(&trx_dir)
                .read_all()
                .map(|v| v.len())
                .unwrap_or(0);
            Some(serde_json::json!({
                "path": trx_dir.display().to_string(),
                "format": "jsonl",
                "migrate_pending": store.migrate_pending(),
                "issues": issue_count,
                "open": open_count,
                "closed": closed_count,
                "events": events_count,
            }))
        }
        Err(_) => None,
    };

    if json {
        let out = serde_json::json!({
            "agent_ctx": ctx,
            "store": store_info,
            "trx_version": env!("CARGO_PKG_VERSION"),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("{} {}", "trx".bold(), env!("CARGO_PKG_VERSION"));
    println!();

    println!("{}", "Store:".bold());
    match &store_info {
        Some(s) => {
            println!("  path     {}", s["path"].as_str().unwrap_or("?"));
            println!("  format   {}", s["format"].as_str().unwrap_or("?"));
            if s["migrate_pending"].as_bool() == Some(true) {
                println!(
                    "  {} legacy CRDT layout detected — will migrate on next mutation",
                    "!".yellow()
                );
            }
            println!(
                "  issues   {} ({} open, {} closed)",
                s["issues"], s["open"], s["closed"]
            );
            println!("  events   {}", s["events"]);
        }
        None => {
            println!("  {} not initialized in this directory", "!".yellow());
        }
    }
    println!();

    println!("{}", "AGENT_CTX:".bold());
    if ctx.is_empty() {
        println!("  {} no AGENT_CTX_* variables set", "·".dimmed());
    } else {
        let rows: &[(&str, Option<&str>)] = &[
            ("version", ctx.version.as_deref()),
            ("platform", ctx.platform.as_deref()),
            ("platform_version", ctx.platform_version.as_deref()),
            ("harness", ctx.harness.as_deref()),
            ("run_mode", ctx.run_mode.as_deref()),
            ("user_id", ctx.user_id.as_deref()),
            ("workspace_id", ctx.workspace_id.as_deref()),
            ("workspace_path", ctx.workspace_path.as_deref()),
            ("platform_session_id", ctx.platform_session_id.as_deref()),
            ("harness_session_id", ctx.harness_session_id.as_deref()),
            ("session_name", ctx.session_name.as_deref()),
            ("readable_id", ctx.readable_id.as_deref()),
            ("model", ctx.model.as_deref()),
            ("request_id", ctx.request_id.as_deref()),
            ("correlation_id", ctx.correlation_id.as_deref()),
            ("sandbox_profile", ctx.sandbox_profile.as_deref()),
        ];
        for (k, v) in rows {
            if let Some(v) = v {
                println!("  {:<20} {}", k, v);
            }
        }
    }

    Ok(())
}

// ============================================================================
// Event log queries: trx history / trx events
// ============================================================================

/// Apply common event filters and ordering. Used by both `history` and
/// `events`. Sort is descending by timestamp (most recent first), and the
/// limit is applied after sorting.
fn filter_events(
    mut events: Vec<Event>,
    issue: Option<&str>,
    session: Option<&str>,
    user: Option<&str>,
    action: Option<EventAction>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
    limit: Option<usize>,
) -> Vec<Event> {
    events.retain(|e| {
        if let Some(id) = issue
            && e.issue_id != id
        {
            return false;
        }
        if let Some(s) = session
            && !e.matches_session(s)
        {
            return false;
        }
        if let Some(u) = user
            && e.user_id.as_deref() != Some(u)
        {
            return false;
        }
        if let Some(a) = action
            && e.action != a
        {
            return false;
        }
        if let Some(t) = since
            && e.timestamp < t
        {
            return false;
        }
        if let Some(t) = until
            && e.timestamp > t
        {
            return false;
        }
        true
    });
    events.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
    if let Some(n) = limit {
        events.truncate(n);
    }
    events
}

fn print_event_line(e: &Event) {
    let ts = e.timestamp.format("%Y-%m-%d %H:%M:%S");
    let action = e.action.to_string();
    let action_colored = match e.action {
        EventAction::Created => action.green().to_string(),
        EventAction::Closed => action.blue().to_string(),
        EventAction::Reopened => action.yellow().to_string(),
        EventAction::Deleted => action.red().to_string(),
        _ => action,
    };
    let who = e
        .user_id
        .as_deref()
        .or(e.session_name.as_deref())
        .or(e.platform_session_id.as_deref())
        .or(e.harness_session_id.as_deref())
        .unwrap_or("-");
    print!(
        "{} {} {} by {}",
        ts.to_string().dimmed(),
        e.issue_id.cyan(),
        action_colored,
        who.dimmed()
    );
    if let Some(note) = &e.note {
        print!(" — {}", note);
    }
    if !e.changes.is_empty() {
        let fields: Vec<String> = e
            .changes
            .iter()
            .map(|c| match (&c.from, &c.to) {
                (Some(f), Some(t)) => format!("{}: {} → {}", c.field, f, t),
                (None, Some(t)) => format!("{}: ∅ → {}", c.field, t),
                (Some(f), None) => format!("{}: {} → ∅", c.field, f),
                (None, None) => c.field.clone(),
            })
            .collect();
        print!(" [{}]", fields.join(", "));
    }
    println!();
}

pub fn history(id: &str, limit: Option<usize>, json: bool) -> Result<()> {
    let store = Store::open()?;
    if store.get(id).is_none() {
        bail!("Issue not found: {}", id);
    }
    let log = EventLog::at(&store.trx_dir());
    let events = log.read_all()?;
    let filtered = filter_events(events, Some(id), None, None, None, None, None, limit);

    if json {
        println!("{}", serde_json::to_string(&filtered)?);
    } else if filtered.is_empty() {
        println!("No events for {}", id);
    } else {
        println!(
            "{} {} ({} event{})",
            "History:".bold(),
            id.cyan(),
            filtered.len(),
            if filtered.len() == 1 { "" } else { "s" }
        );
        for e in &filtered {
            print_event_line(e);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn events(
    issue: Option<String>,
    session: Option<String>,
    user: Option<String>,
    action: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let store = Store::open()?;
    let log = EventLog::at(&store.trx_dir());
    let all = log.read_all()?;

    let action_parsed = match action {
        Some(s) => Some(
            s.parse::<EventAction>()
                .map_err(|e| anyhow::anyhow!("{}", e))?,
        ),
        None => None,
    };
    let since_parsed = match since {
        Some(s) => Some(parse_date(&s)?),
        None => None,
    };
    let until_parsed = match until {
        Some(s) => Some(parse_date(&s)?),
        None => None,
    };

    let filtered = filter_events(
        all,
        issue.as_deref(),
        session.as_deref(),
        user.as_deref(),
        action_parsed,
        since_parsed,
        until_parsed,
        limit,
    );

    if json {
        println!("{}", serde_json::to_string(&filtered)?);
    } else if filtered.is_empty() {
        println!("No events match");
    } else {
        for e in &filtered {
            print_event_line(e);
        }
    }
    Ok(())
}

// ============================================================================
// Markdown export: trx export
// ============================================================================

pub fn export(
    output: Option<String>,
    include_closed: bool,
    issue_type: Option<String>,
    labels: Vec<String>,
) -> Result<()> {
    let store = Store::open()?;

    let type_filter = match issue_type {
        Some(s) => Some(s.parse::<IssueType>()?),
        None => None,
    };

    let mut issues: Vec<&Issue> = store
        .list(false)
        .into_iter()
        .filter(|i| include_closed || i.status.is_open())
        .filter(|i| {
            if let Some(t) = type_filter
                && i.issue_type != t
            {
                return false;
            }
            for l in &labels {
                if !i.labels.iter().any(|il| il == l) {
                    return false;
                }
            }
            true
        })
        .collect();

    issues.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)));

    let groups: &[(Status, &str)] = &[
        (Status::Open, "Open"),
        (Status::InProgress, "In Progress"),
        (Status::Blocked, "Blocked"),
        (Status::Closed, "Closed"),
    ];

    let mut md = String::new();
    md.push_str("# Issues\n\n");
    md.push_str(&format!(
        "_Generated {} by trx {}._\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
        env!("CARGO_PKG_VERSION")
    ));

    for (status, label) in groups {
        let in_group: Vec<&&Issue> = issues.iter().filter(|i| i.status == *status).collect();
        if in_group.is_empty() {
            continue;
        }
        md.push_str(&format!("## {} ({})\n\n", label, in_group.len()));
        for issue in in_group {
            render_issue_md(&mut md, issue, &store);
        }
    }

    if let Some(path) = output {
        std::fs::write(&path, md.as_bytes())?;
        eprintln!("{} Wrote {}", "✓".green(), path);
    } else {
        print!("{}", md);
    }
    Ok(())
}

fn render_issue_md(md: &mut String, issue: &Issue, store: &Store) {
    md.push_str(&format!(
        "### {} — {} `[P{}]` `[{}]`\n\n",
        issue.id, issue.title, issue.priority, issue.issue_type
    ));

    if let Some(desc) = &issue.description {
        md.push_str(desc.trim());
        md.push_str("\n\n");
    }

    let mut meta: Vec<String> = Vec::new();
    meta.push(format!("Created: {}", issue.created_at.format("%Y-%m-%d")));
    if !issue.labels.is_empty() {
        meta.push(format!("Labels: {}", issue.labels.join(", ")));
    }
    if let Some(a) = &issue.assignee {
        meta.push(format!("Assignee: {}", a));
    }
    if let Some(c) = &issue.created_by {
        meta.push(format!("Created by: {}", c));
    }
    if let Some(reason) = &issue.close_reason {
        meta.push(format!("Closed: {}", reason));
    }

    let parent: Option<&str> = issue
        .dependencies
        .iter()
        .find(|d| d.dep_type == DependencyType::ParentChild)
        .map(|d| d.depends_on_id.as_str());
    if let Some(p) = parent {
        meta.push(format!("Parent: {}", p));
    }

    let blocked_by: Vec<&str> = issue
        .dependencies
        .iter()
        .filter(|d| d.dep_type == DependencyType::Blocks)
        .map(|d| d.depends_on_id.as_str())
        .collect();
    if !blocked_by.is_empty() {
        meta.push(format!("Blocked by: {}", blocked_by.join(", ")));
    }

    // Reverse links: issues that depend on this one.
    let blocks: Vec<&str> = store
        .list(false)
        .into_iter()
        .filter(|i| {
            i.dependencies
                .iter()
                .any(|d| d.depends_on_id == issue.id && d.dep_type == DependencyType::Blocks)
        })
        .map(|i| i.id.as_str())
        .collect();
    if !blocks.is_empty() {
        meta.push(format!("Blocks: {}", blocks.join(", ")));
    }

    let children: Vec<&str> = store
        .list(false)
        .into_iter()
        .filter(|i| {
            i.dependencies
                .iter()
                .any(|d| d.depends_on_id == issue.id && d.dep_type == DependencyType::ParentChild)
        })
        .map(|i| i.id.as_str())
        .collect();
    if !children.is_empty() {
        meta.push(format!("Children: {}", children.join(", ")));
    }

    for line in meta {
        md.push_str(&format!("- {}\n", line));
    }
    md.push('\n');
}

// ============================================================================
// Activity feed: trx log
// ============================================================================

/// Color an `EventAction` for terminal output. Kept consistent across `log`,
/// `sessions <id>`, and `history` so the same action always looks the same.
fn colored_action(a: EventAction) -> String {
    let s = a.to_string();
    match a {
        EventAction::Created => s.green().to_string(),
        EventAction::Closed => s.blue().to_string(),
        EventAction::Reopened => s.yellow().to_string(),
        EventAction::Deleted => s.red().to_string(),
        EventAction::Restored => s.green().to_string(),
        EventAction::DepAdded | EventAction::DepRemoved => s.magenta().to_string(),
        EventAction::SessionLinked => s.cyan().to_string(),
        EventAction::Updated => s.normal().to_string(),
    }
}

/// Format a `FieldChange` as `field: from → to` (with `∅` for missing sides).
fn format_change(c: &trx_core::FieldChange) -> String {
    match (&c.from, &c.to) {
        (Some(f), Some(t)) => format!("{}: {} → {}", c.field, f, t),
        (None, Some(t)) => format!("{}: ∅ → {}", c.field, t),
        (Some(f), None) => format!("{}: {} → ∅", c.field, f),
        (None, None) => c.field.clone(),
    }
}

/// One-line AGENT_CTX summary, e.g. `claude-code · my-session · opus-4.7`.
/// Returns `None` when no context fields are populated.
fn agent_ctx_line(e: &Event) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(h) = &e.harness {
        parts.push(h.clone());
    } else if let Some(p) = &e.platform {
        parts.push(p.clone());
    }
    if let Some(n) = &e.session_name {
        parts.push(n.clone());
    }
    if let Some(m) = &e.model {
        parts.push(m.clone());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Verbose AGENT_CTX block: one labeled line per non-empty field.
fn agent_ctx_verbose(e: &Event) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |label: &str, val: &Option<String>| {
        if let Some(v) = val {
            out.push(format!("{:<10} {}", format!("{}:", label), v));
        }
    };
    push("user", &e.user_id);
    push("platform", &e.platform);
    push("harness", &e.harness);
    push("session", &e.session_name);
    push("plat_sid", &e.platform_session_id);
    push("harn_sid", &e.harness_session_id);
    push("workspace", &e.workspace_id);
    push("model", &e.model);
    push("request", &e.request_id);
    push("correlate", &e.correlation_id);
    out
}

/// Render one event in the `trx log` feed.
fn render_log_event(e: &Event, verbose: bool) {
    let ts = e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
    println!(
        "  {}  {:<10} {}",
        ts.dimmed(),
        colored_action(e.action),
        e.issue_id.cyan()
    );
    if let Some(note) = &e.note {
        println!("    {} {}", "›".dimmed(), note);
    }
    for c in &e.changes {
        println!("    {} {}", "·".dimmed(), format_change(c).dimmed());
    }
    if verbose {
        for line in agent_ctx_verbose(e) {
            println!("    {}", line.dimmed());
        }
    }
}

/// Print a session header (`── session: <id> · <user> · <harness> · <model> ──`).
fn print_session_header(s: &SessionSummary) {
    let mut parts: Vec<String> = Vec::new();
    parts.push(s.session_id.clone());
    if let Some(n) = &s.session_name
        && Some(n.as_str()) != Some(s.session_id.as_str())
    {
        parts.push(n.clone());
    }
    if let Some(u) = &s.user_id {
        parts.push(u.clone());
    }
    if let Some(h) = &s.harness {
        parts.push(h.clone());
    } else if let Some(p) = &s.platform {
        parts.push(p.clone());
    }
    if let Some(m) = &s.model {
        parts.push(m.clone());
    }
    let range = format!(
        "{} → {}",
        s.first_at.format("%Y-%m-%d %H:%M"),
        s.last_at.format("%H:%M")
    );
    println!(
        "{} {} {}  {} {} {}",
        "──".dimmed(),
        "session:".bold(),
        parts.join(" · ").cyan(),
        format!(
            "({} event{})",
            s.event_count,
            if s.event_count == 1 { "" } else { "s" }
        )
        .dimmed(),
        "·".dimmed(),
        range.dimmed(),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn log(
    issue: Option<String>,
    session: Option<String>,
    user: Option<String>,
    action: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<usize>,
    no_group: bool,
    verbose: bool,
    json: bool,
) -> Result<()> {
    let store = Store::open()?;
    let log = EventLog::at(&store.trx_dir());
    let all = log.read_all()?;

    let action_parsed = match action {
        Some(s) => Some(
            s.parse::<EventAction>()
                .map_err(|e| anyhow::anyhow!("{}", e))?,
        ),
        None => None,
    };
    let since_parsed = match since {
        Some(s) => Some(parse_date(&s)?),
        None => None,
    };
    let until_parsed = match until {
        Some(s) => Some(parse_date(&s)?),
        None => None,
    };

    let filtered = filter_events(
        all,
        issue.as_deref(),
        session.as_deref(),
        user.as_deref(),
        action_parsed,
        since_parsed,
        until_parsed,
        limit,
    );

    if json {
        println!("{}", serde_json::to_string(&filtered)?);
        return Ok(());
    }

    if filtered.is_empty() {
        println!("No events match");
        return Ok(());
    }

    if no_group {
        for e in &filtered {
            render_log_event(e, verbose);
        }
        return Ok(());
    }

    // Group by session_key, preserving the filtered order (newest first).
    // Within a session group, print events oldest → newest so the narrative
    // reads forward in time.
    let summaries = summarize_sessions(&filtered);
    for s in &summaries {
        print_session_header(s);
        let mut session_events: Vec<&Event> = filtered
            .iter()
            .filter(|e| e.session_key().unwrap_or("-") == s.session_id)
            .collect();
        session_events.sort_by_key(|e| e.timestamp);
        for e in session_events {
            render_log_event(e, verbose);
        }
        println!();
    }
    Ok(())
}

// ============================================================================
// Sessions: trx sessions [<id>]
// ============================================================================

#[allow(clippy::too_many_arguments)]
pub fn sessions(
    session: Option<String>,
    user: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<usize>,
    verbose: bool,
    json: bool,
) -> Result<()> {
    let store = Store::open()?;
    let log = EventLog::at(&store.trx_dir());
    let all = log.read_all()?;

    let since_parsed = match since {
        Some(s) => Some(parse_date(&s)?),
        None => None,
    };
    let until_parsed = match until {
        Some(s) => Some(parse_date(&s)?),
        None => None,
    };

    // Drilldown: full trace for one session.
    if let Some(sid) = session {
        let events = filter_events(
            all,
            None,
            Some(sid.as_str()),
            user.as_deref(),
            None,
            since_parsed,
            until_parsed,
            None,
        );
        if json {
            println!("{}", serde_json::to_string(&events)?);
            return Ok(());
        }
        if events.is_empty() {
            println!("No events for session: {}", sid);
            return Ok(());
        }
        // Oldest → newest for narrative reading.
        let mut sorted = events.clone();
        sorted.sort_by_key(|e| e.timestamp);
        let summaries = summarize_sessions(&sorted);
        if let Some(s) = summaries.first() {
            print_session_header(s);
        }
        for e in &sorted {
            render_log_event(e, verbose);
        }
        return Ok(());
    }

    // List view: one row per session.
    let events = filter_events(
        all,
        None,
        None,
        user.as_deref(),
        None,
        since_parsed,
        until_parsed,
        None,
    );
    let mut summaries = summarize_sessions(&events);
    if let Some(n) = limit {
        summaries.truncate(n);
    }

    if json {
        println!("{}", serde_json::to_string(&summaries)?);
        return Ok(());
    }

    if summaries.is_empty() {
        println!("No sessions match");
        return Ok(());
    }

    println!(
        "{:<24} {:<12} {:<14} {:<22} {:>6}  {}",
        "SESSION".bold(),
        "USER".bold(),
        "HARNESS".bold(),
        "MODEL".bold(),
        "EVTS".bold(),
        "RANGE / ISSUES".bold(),
    );
    for s in &summaries {
        let id = truncate(&s.session_id, 24);
        let user = truncate(s.user_id.as_deref().unwrap_or("-"), 12);
        let harness = truncate(
            s.harness
                .as_deref()
                .or(s.platform.as_deref())
                .unwrap_or("-"),
            14,
        );
        let model = truncate(s.model.as_deref().unwrap_or("-"), 22);
        let range = format!(
            "{} → {}",
            s.first_at.format("%Y-%m-%d %H:%M"),
            s.last_at.format("%H:%M")
        );
        println!(
            "{:<24} {:<12} {:<14} {:<22} {:>6}  {}",
            id.cyan(),
            user,
            harness,
            model,
            s.event_count,
            range.dimmed()
        );
        if !s.issue_ids.is_empty() {
            let preview: Vec<String> = s.issue_ids.iter().take(5).cloned().collect();
            let more = if s.issue_ids.len() > 5 {
                format!(" (+{} more)", s.issue_ids.len() - 5)
            } else {
                String::new()
            };
            println!(
                "{:<24} {}{}",
                "",
                preview.join(", ").dimmed(),
                more.dimmed()
            );
        }
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ============================================================================
// Stats: trx stats
// ============================================================================

pub fn stats(since: Option<String>, until: Option<String>, by: &str, json: bool) -> Result<()> {
    let store = Store::open()?;
    let log = EventLog::at(&store.trx_dir());
    let all = log.read_all()?;

    let since_dt = match since {
        Some(s) => parse_date(&s)?,
        None => chrono::Utc::now() - chrono::Duration::days(30),
    };
    let until_dt = match until {
        Some(s) => Some(parse_date(&s)?),
        None => None,
    };

    let events = filter_events(all, None, None, None, None, Some(since_dt), until_dt, None);

    let bucket = by.to_ascii_lowercase();
    if !matches!(bucket.as_str(), "day" | "hour") {
        bail!("--by must be 'day' or 'hour'");
    }

    // Bucketed counts for the sparkline.
    let (labels, counts) = bucket_counts(&events, &bucket, since_dt, until_dt);

    // Aggregate breakdowns.
    let mut by_action: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut by_user: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut by_harness: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut sessions_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for e in &events {
        *by_action.entry(e.action.to_string()).or_default() += 1;
        *by_user
            .entry(e.user_id.clone().unwrap_or_else(|| "-".into()))
            .or_default() += 1;
        *by_harness
            .entry(
                e.harness
                    .clone()
                    .or_else(|| e.platform.clone())
                    .unwrap_or_else(|| "-".into()),
            )
            .or_default() += 1;
        if let Some(k) = e.session_key() {
            sessions_set.insert(k.to_string());
        }
    }

    if json {
        let payload = serde_json::json!({
            "total": events.len(),
            "since": since_dt,
            "until": until_dt,
            "by": bucket,
            "buckets": labels.iter().zip(counts.iter()).map(|(l, c)| {
                serde_json::json!({"label": l, "count": c})
            }).collect::<Vec<_>>(),
            "by_action": by_action,
            "by_user": by_user,
            "by_harness": by_harness,
            "sessions": sessions_set.len(),
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    let header = match until_dt {
        Some(u) => format!(
            "Events: {} ({} → {})",
            events.len(),
            since_dt.format("%Y-%m-%d"),
            u.format("%Y-%m-%d")
        ),
        None => format!(
            "Events: {} (since {})",
            events.len(),
            since_dt.format("%Y-%m-%d")
        ),
    };
    println!("{}", header.bold());
    println!("Sessions: {}", sessions_set.len());

    if !counts.is_empty() {
        println!();
        println!("{} (per {}):", "Activity".bold(), bucket);
        let max = *counts.iter().max().unwrap_or(&0);
        let spark = sparkline(&counts);
        println!("  {}", spark);
        // Show start and end labels under the sparkline.
        if let (Some(first), Some(last)) = (labels.first(), labels.last()) {
            let pad = spark.chars().count().saturating_sub(last.len());
            println!("  {}{}{}", first, " ".repeat(pad), last);
        }
        println!("  {} {}", "peak:".dimmed(), max);
    }

    print_bar_breakdown("By action", &by_action);
    print_bar_breakdown("By user", &by_user);
    print_bar_breakdown("By harness", &by_harness);

    Ok(())
}

/// Compute (labels, counts) buckets between `since` and `until` (or now).
/// Empty intermediate buckets are kept so the sparkline shows lulls.
fn bucket_counts(
    events: &[Event],
    bucket: &str,
    since: chrono::DateTime<chrono::Utc>,
    until: Option<chrono::DateTime<chrono::Utc>>,
) -> (Vec<String>, Vec<usize>) {
    let end = until.unwrap_or_else(chrono::Utc::now);
    if end < since {
        return (Vec::new(), Vec::new());
    }
    let (step, fmt): (chrono::Duration, &str) = match bucket {
        "hour" => (chrono::Duration::hours(1), "%m-%d %H"),
        _ => (chrono::Duration::days(1), "%m-%d"),
    };

    // Truncate `since` to the start of its bucket.
    let start = if bucket == "hour" {
        since
            .date_naive()
            .and_hms_opt(since.time().hour(), 0, 0)
            .map(|d| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(d, chrono::Utc))
            .unwrap_or(since)
    } else {
        since
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|d| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(d, chrono::Utc))
            .unwrap_or(since)
    };

    let mut labels: Vec<String> = Vec::new();
    let mut counts: Vec<usize> = Vec::new();
    let mut cursor = start;
    // Cap at a reasonable bucket count to keep output sane.
    let max_buckets = 400;
    while cursor <= end && labels.len() < max_buckets {
        labels.push(cursor.format(fmt).to_string());
        counts.push(0);
        cursor += step;
    }
    if labels.is_empty() {
        return (labels, counts);
    }
    for ev in events {
        if ev.timestamp < start || ev.timestamp > end {
            continue;
        }
        let idx_signed = (ev.timestamp - start).num_seconds() / step.num_seconds();
        let idx = idx_signed as usize;
        if idx < counts.len() {
            counts[idx] += 1;
        }
    }
    (labels, counts)
}

const SPARK_CHARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn sparkline(counts: &[usize]) -> String {
    if counts.is_empty() {
        return String::new();
    }
    let max = *counts.iter().max().unwrap_or(&0);
    if max == 0 {
        return " ".repeat(counts.len());
    }
    counts
        .iter()
        .map(|&c| {
            if c == 0 {
                ' '
            } else {
                let idx = (c * (SPARK_CHARS.len() - 1)) / max;
                SPARK_CHARS[idx]
            }
        })
        .collect()
}

use chrono::Timelike;

fn print_bar_breakdown(title: &str, map: &std::collections::BTreeMap<String, usize>) {
    if map.is_empty() {
        return;
    }
    println!();
    println!("{}:", title.bold());
    let max = *map.values().max().unwrap_or(&0).max(&1);
    // Sort descending by count.
    let mut rows: Vec<(&String, &usize)> = map.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let label_w = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0).min(20);
    for (k, v) in rows.iter().take(10) {
        let bar_len = (*v * 24) / max.max(1);
        let bar = "█".repeat(bar_len);
        println!(
            "  {:<width$}  {:>4}  {}",
            truncate(k, label_w),
            v,
            bar.cyan(),
            width = label_w
        );
    }
}

// ============================================================================
// Calendar heatmap: trx heatmap
// ============================================================================

/// Map an event count to a 2-column-wide colored cell ("glyph " pattern).
fn heatmap_cell(count: usize) -> String {
    match count {
        0 => "·".dimmed().to_string(),
        1..=2 => "▒".green().to_string(),
        3..=5 => "▓".bright_green().to_string(),
        _ => "█".bright_cyan().to_string(),
    }
}

pub fn heatmap(
    since: Option<String>,
    weeks: usize,
    user: Option<String>,
    json: bool,
) -> Result<()> {
    use chrono::{Datelike, Duration, NaiveDate};

    if weeks == 0 || weeks > 104 {
        bail!("--weeks must be between 1 and 104");
    }
    let store = Store::open()?;
    let log = EventLog::at(&store.trx_dir());
    let all = log.read_all()?;

    let since_dt = match since {
        Some(s) => parse_date(&s)?,
        None => chrono::Utc::now() - Duration::weeks(weeks as i64),
    };

    let events = filter_events(
        all,
        None,
        None,
        user.as_deref(),
        None,
        Some(since_dt),
        None,
        None,
    );

    // Bucket by local date.
    let today: NaiveDate = chrono::Local::now().date_naive();
    let weekday_today = today.weekday().num_days_from_monday() as i64;
    let last_col_monday = today - Duration::days(weekday_today);
    let first_col_monday = last_col_monday - Duration::weeks((weeks as i64) - 1);

    let mut counts: std::collections::BTreeMap<NaiveDate, usize> =
        std::collections::BTreeMap::new();
    for e in &events {
        let d = e.timestamp.date_naive();
        if d < first_col_monday {
            continue;
        }
        *counts.entry(d).or_default() += 1;
    }

    if json {
        let payload = serde_json::json!({
            "since": first_col_monday,
            "until": today,
            "weeks": weeks,
            "days": counts.iter().map(|(d, c)| serde_json::json!({
                "date": d.to_string(),
                "count": c,
            })).collect::<Vec<_>>(),
            "total": events.len(),
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    let total: usize = counts.values().sum();
    let active_days = counts.values().filter(|c| **c > 0).count();
    let sessions: std::collections::BTreeSet<String> = events
        .iter()
        .filter_map(|e| e.session_key().map(String::from))
        .collect();

    println!(
        "{} {}",
        "Activity heatmap".bold(),
        format!("(last {} weeks · {} → {})", weeks, first_col_monday, today).dimmed()
    );
    println!();

    // Month labels above the grid. One label per month-start column.
    let mut month_line = String::from("      ");
    let mut last_month = 0u32;
    for w in 0..weeks {
        let col_monday = first_col_monday + Duration::weeks(w as i64);
        if col_monday.month() != last_month && col_monday.day() <= 7 {
            let label = col_monday.format("%b").to_string();
            // Each cell is 2 cols wide; pad label to a multiple of 2.
            let label_padded = if label.len().is_multiple_of(2) {
                label
            } else {
                format!("{} ", label)
            };
            month_line.push_str(&label_padded);
            last_month = col_monday.month();
        } else {
            month_line.push_str("  ");
        }
    }
    println!("{}", month_line.dimmed());

    let weekday_labels = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    for (row, label) in weekday_labels.iter().enumerate() {
        let mut line = format!("{:<6}", label);
        for w in 0..weeks {
            let date = first_col_monday + Duration::weeks(w as i64) + Duration::days(row as i64);
            if date > today {
                line.push_str("  ");
                continue;
            }
            let c = *counts.get(&date).unwrap_or(&0);
            line.push_str(&heatmap_cell(c));
            line.push(' ');
        }
        println!("{}", line);
    }
    println!();
    println!(
        "Legend  {} 0   {} 1–2   {} 3–5   {} 6+",
        "·".dimmed(),
        "▒".green(),
        "▓".bright_green(),
        "█".bright_cyan()
    );
    println!();
    println!(
        "{} events  ·  {} active day{}  ·  {} session{}",
        total,
        active_days,
        if active_days == 1 { "" } else { "s" },
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

// ============================================================================
// Swimlane: trx swimlane
// ============================================================================

fn action_priority(a: EventAction) -> u8 {
    match a {
        EventAction::Deleted => 6,
        EventAction::Closed => 5,
        EventAction::Reopened => 4,
        EventAction::Created => 3,
        EventAction::DepAdded | EventAction::DepRemoved => 2,
        EventAction::SessionLinked | EventAction::Restored => 1,
        EventAction::Updated => 0,
    }
}

fn action_glyph(a: EventAction) -> &'static str {
    match a {
        EventAction::Created => "+",
        EventAction::Updated => "*",
        EventAction::Reopened => "↑",
        EventAction::Closed => "■",
        EventAction::Deleted => "✕",
        EventAction::Restored => "↻",
        EventAction::DepAdded | EventAction::DepRemoved => "◆",
        EventAction::SessionLinked => "·",
    }
}

fn colored_glyph(a: EventAction) -> String {
    let g = action_glyph(a);
    match a {
        EventAction::Created => g.green().to_string(),
        EventAction::Updated => g.white().to_string(),
        EventAction::Reopened => g.yellow().to_string(),
        EventAction::Closed => g.bright_blue().to_string(),
        EventAction::Deleted => g.bright_red().to_string(),
        EventAction::Restored => g.green().to_string(),
        EventAction::DepAdded | EventAction::DepRemoved => g.magenta().to_string(),
        EventAction::SessionLinked => g.cyan().to_string(),
    }
}

/// Best-effort terminal width via `stty size`. Returns `default` when stty
/// isn't available (e.g. piped output) — that's fine for our use case.
fn term_width(default: usize) -> usize {
    let out = std::process::Command::new("stty")
        .arg("size")
        .stdin(std::process::Stdio::inherit())
        .output()
        .ok();
    let Some(out) = out else { return default };
    if !out.status.success() {
        return default;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.split_whitespace();
    let _rows: Option<u16> = it.next().and_then(|s| s.parse().ok());
    let cols: Option<u16> = it.next().and_then(|s| s.parse().ok());
    cols.map(|c| c as usize).unwrap_or(default)
}

#[allow(clippy::too_many_arguments)]
pub fn swimlane(
    since: Option<String>,
    until: Option<String>,
    cols: Option<usize>,
    limit: usize,
    session: Option<String>,
    user: Option<String>,
    json: bool,
) -> Result<()> {
    let store = Store::open()?;
    let log = EventLog::at(&store.trx_dir());
    let all = log.read_all()?;

    let since_dt = match since {
        Some(s) => parse_date(&s)?,
        None => chrono::Utc::now() - chrono::Duration::days(30),
    };
    let until_dt = match until {
        Some(s) => parse_date(&s)?,
        None => chrono::Utc::now(),
    };
    if until_dt <= since_dt {
        bail!("--until must be after --since");
    }

    let events = filter_events(
        all,
        None,
        session.as_deref(),
        user.as_deref(),
        None,
        Some(since_dt),
        Some(until_dt),
        None,
    );

    if events.is_empty() {
        println!("No events in window");
        return Ok(());
    }

    // Pick issues, ordered by most-recent activity.
    let mut last_seen: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>> =
        std::collections::HashMap::new();
    for e in &events {
        last_seen
            .entry(e.issue_id.clone())
            .and_modify(|t| {
                if e.timestamp > *t {
                    *t = e.timestamp;
                }
            })
            .or_insert(e.timestamp);
    }
    let mut issues: Vec<(String, chrono::DateTime<chrono::Utc>)> = last_seen.into_iter().collect();
    issues.sort_by_key(|(_, ts)| std::cmp::Reverse(*ts));
    issues.truncate(limit);
    let issue_ids: Vec<String> = issues.iter().map(|(i, _)| i.clone()).collect();

    let label_w = issue_ids
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 20);
    let auto_cols = term_width(120).saturating_sub(label_w + 4).max(20);
    let n_cols = cols.unwrap_or(auto_cols).clamp(10, 200);

    let bucket_seconds = (until_dt - since_dt).num_seconds().max(1) as f64 / n_cols as f64;
    let mut grid: Vec<Vec<Option<EventAction>>> = vec![vec![None; n_cols]; issue_ids.len()];
    let issue_idx: std::collections::HashMap<&str, usize> = issue_ids
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();

    for e in &events {
        let Some(&row) = issue_idx.get(e.issue_id.as_str()) else {
            continue;
        };
        let offset = (e.timestamp - since_dt).num_seconds() as f64;
        let mut col = (offset / bucket_seconds).floor() as isize;
        if col < 0 {
            col = 0;
        }
        if (col as usize) >= n_cols {
            col = (n_cols - 1) as isize;
        }
        let col = col as usize;
        let cell = &mut grid[row][col];
        match cell {
            None => *cell = Some(e.action),
            Some(existing) => {
                if action_priority(e.action) > action_priority(*existing) {
                    *existing = e.action;
                }
            }
        }
    }

    if json {
        let payload = serde_json::json!({
            "since": since_dt,
            "until": until_dt,
            "cols": n_cols,
            "issues": issue_ids,
            "grid": grid.iter().map(|row| {
                row.iter().map(|c| c.map(|a| a.to_string())).collect::<Vec<_>>()
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    // Time axis: 4-6 evenly spaced %m-%d labels under the grid header.
    let total_secs = (until_dt - since_dt).num_seconds() as f64;
    let n_labels = ((n_cols / 12).clamp(2, 6)).min(n_cols);
    let label_prefix = " ".repeat(label_w + 2);
    let mut axis = label_prefix.clone();
    let mut ticks = label_prefix.clone();
    let line_max = label_w + 2 + n_cols;
    for k in 0..n_labels {
        let frac = if n_labels == 1 {
            0.0
        } else {
            k as f64 / (n_labels - 1) as f64
        };
        let secs = since_dt + chrono::Duration::seconds((total_secs * frac) as i64);
        let label = secs.format("%m-%d").to_string();
        let col = (frac * (n_cols - 1) as f64).round() as usize;
        let tick_target = label_w + 2 + col;
        // Anchor the label so it fits before the right edge — last labels are
        // pulled left so they don't overrun the grid.
        let mut label_start = tick_target;
        if label_start + label.len() > line_max {
            label_start = line_max.saturating_sub(label.len());
        }
        // Don't overlap a previously-written label.
        if label_start < axis.chars().count() {
            label_start = axis.chars().count();
        }
        while axis.chars().count() < label_start {
            axis.push(' ');
        }
        for ch in label.chars() {
            if axis.chars().count() < line_max {
                axis.push(ch);
            }
        }
        while ticks.chars().count() < tick_target {
            ticks.push(' ');
        }
        if ticks.chars().count() < line_max {
            ticks.push('│');
        }
    }

    println!(
        "{} {}",
        "Swimlane".bold(),
        format!(
            "({} → {}  ·  {} issue{}  ·  {} col{})",
            since_dt.format("%Y-%m-%d %H:%M"),
            until_dt.format("%Y-%m-%d %H:%M"),
            issue_ids.len(),
            if issue_ids.len() == 1 { "" } else { "s" },
            n_cols,
            if n_cols == 1 { "" } else { "s" }
        )
        .dimmed()
    );
    println!();
    println!("{}", axis.dimmed());
    println!("{}", ticks.dimmed());

    for (row_idx, issue_id) in issue_ids.iter().enumerate() {
        let trunc = truncate(issue_id, label_w);
        let pad_n = label_w.saturating_sub(trunc.chars().count());
        let mut line = String::new();
        line.push_str(&trunc.cyan().to_string());
        for _ in 0..pad_n {
            line.push(' ');
        }
        line.push_str("  ");
        for cell in &grid[row_idx] {
            match cell {
                Some(a) => line.push_str(&colored_glyph(*a)),
                None => line.push_str(&"·".dimmed().to_string()),
            }
        }
        println!("{}", line);
    }

    println!();
    println!(
        "Legend  {} created   {} updated   {} reopened   {} closed   {} deleted   {} dep",
        "+".green(),
        "*".white(),
        "↑".yellow(),
        "■".bright_blue(),
        "✕".bright_red(),
        "◆".magenta()
    );

    Ok(())
}
