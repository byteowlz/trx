//! trx-tui - Terminal UI viewer for trx issues
//!
//! Replaces beads-viewer with a Rust-native TUI.

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyCode, KeyEvent,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use std::collections::HashSet;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use trx_core::{
    Event, EventAction, EventLog, FieldChange, Issue, IssueGraph, SessionSummary, Status, Store,
    summarize_sessions,
};

#[derive(Parser)]
#[command(name = "trx-tui")]
#[command(about = "Terminal UI viewer for trx issues")]
#[command(version)]
struct Cli {
    #[arg(short, long)]
    workspace: Option<String>,
    #[arg(short, long)]
    repo: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Robot {
        #[command(subcommand)]
        mode: RobotMode,
    },
}

#[derive(Subcommand)]
enum RobotMode {
    Triage,
    Next,
    Insights,
    Plan,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Robot { mode }) => run_robot_mode(mode),
        None => run_tui(cli.workspace, cli.repo),
    }
}

fn run_robot_mode(mode: RobotMode) -> Result<()> {
    let store = Store::open()?;
    let issues = store.list_open();

    match mode {
        RobotMode::Triage => {
            let mut sorted: Vec<_> = issues.into_iter().collect();
            sorted.sort_by_key(|a| a.priority);
            println!("{}", serde_json::to_string_pretty(&sorted)?);
        }
        RobotMode::Next => {
            let graph = IssueGraph::from_issues(&issues);
            let ready = graph.ready_issues(&issues);
            if let Some(next) = ready.iter().min_by_key(|i| i.priority) {
                println!("{}", serde_json::to_string_pretty(next)?);
            } else {
                println!("null");
            }
        }
        RobotMode::Insights => {
            let graph = IssueGraph::from_issues(&issues);
            let cycles = graph.find_cycles();
            let pagerank = graph.pagerank(0.85, 20);

            let insights = serde_json::json!({
                "total_open": issues.len(),
                "cycles": cycles,
                "pagerank_top5": pagerank.iter()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .take(5)
                    .collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&insights)?);
        }
        RobotMode::Plan => {
            println!(r#"{{"tracks": [], "note": "not yet implemented"}}"#);
        }
    }
    Ok(())
}

fn run_tui(_workspace: Option<String>, _repo: Option<String>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let store = Store::open()?;
    let mut app = App::new(store)?;

    let res = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("{:?}", err);
    }

    Ok(())
}

fn run_app<B: Backend + IoWrite>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    let mut last_tick = Instant::now();
    const TICK_RATE: Duration = Duration::from_millis(250);

    loop {
        terminal.draw(|f| ui(f, app))?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)?
            && let CrosstermEvent::Key(key) = event::read()?
        {
            let action = parse_key_action(key);
            if app.mode == AppMode::Normal && action == KeyAction::Char('E') {
                edit_current_description_external(terminal, app)?;
            } else if app.mode == AppMode::Normal && action == KeyAction::Char('N') {
                add_note_external(terminal, app)?;
            } else if app.handle_key_action(action)? {
                return Ok(());
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            app.on_tick();
            last_tick = Instant::now();
        }
    }
}

fn edit_current_description_external<B: Backend + IoWrite>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    let Some(issue) = app.current_issue().cloned() else {
        app.show_status("No issue selected".to_string());
        return Ok(());
    };

    let initial = issue.description.clone().unwrap_or_default();
    let path = write_editor_temp_file(
        &issue.id,
        "description",
        &format!(
            "# Edit description for {} — {}\n# Lines starting with # are ignored. Save and quit to apply.\n\n{}",
            issue.id, issue.title, initial
        ),
    )?;

    let status = run_external_editor(terminal, &path)?;
    if !status.success() {
        app.show_status(format!("Editor exited with status {status}"));
        return Ok(());
    }

    let edited = read_editor_body(&path)?;
    let description = if edited.trim().is_empty() {
        None
    } else {
        Some(edited.trim_end().to_string())
    };
    if description == issue.description {
        app.show_status("No description changes".to_string());
        return Ok(());
    }

    let mut fresh_store = Store::open()?;
    let Some(current) = fresh_store.get(&issue.id).cloned() else {
        app.show_status(format!("{} no longer exists", issue.id));
        return Ok(());
    };
    let changed_while_editing = current.updated_at != issue.updated_at;
    let mut updated = current;
    updated.description = description;
    updated.updated_at = chrono::Utc::now();
    fresh_store.update(updated)?;
    app.store = fresh_store;
    app.apply_filters()?;
    if changed_while_editing {
        app.show_status(
            "Issue changed while editing; applied description to latest version".to_string(),
        );
    } else {
        app.show_status(format!("Updated description for {}", issue.id));
    }
    Ok(())
}

fn add_note_external<B: Backend + IoWrite>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    let Some(issue) = app.current_issue().cloned() else {
        app.show_status("No issue selected".to_string());
        return Ok(());
    };

    let path = write_editor_temp_file(
        &issue.id,
        "note",
        &format!(
            "# Add note to {} — {}\n# Lines starting with # are ignored. Empty note cancels.\n\n",
            issue.id, issue.title
        ),
    )?;
    let status = run_external_editor(terminal, &path)?;
    if !status.success() {
        app.show_status(format!("Editor exited with status {status}"));
        return Ok(());
    }

    let note = read_editor_body(&path)?.trim().to_string();
    if note.is_empty() {
        app.show_status("Empty note discarded".to_string());
        return Ok(());
    }

    let mut fresh_store = Store::open()?;
    let Some(mut updated) = fresh_store.get(&issue.id).cloned() else {
        app.show_status(format!("{} no longer exists", issue.id));
        return Ok(());
    };
    let entry = format!(
        "{}:\n{}",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
        note
    );
    updated.notes = Some(match updated.notes {
        Some(existing) if !existing.trim().is_empty() => {
            format!("{}\n\n{}", existing.trim_end(), entry)
        }
        _ => entry,
    });
    updated.updated_at = chrono::Utc::now();
    fresh_store.update(updated)?;
    app.store = fresh_store;
    app.apply_filters()?;
    app.show_status(format!("Added note to {}", issue.id));
    Ok(())
}

fn run_external_editor<B: Backend + IoWrite>(
    terminal: &mut Terminal<B>,
    path: &Path,
) -> Result<std::process::ExitStatus> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        Show
    )?;
    terminal.show_cursor()?;

    let result = spawn_editor(path);

    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture,
        Hide
    )?;
    enable_raw_mode()?;
    terminal.clear()?;

    result
}

fn spawn_editor(path: &Path) -> Result<std::process::ExitStatus> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let mut command = Command::new(program);
    for arg in parts {
        command.arg(arg);
    }
    command.arg(path).status().map_err(Into::into)
}

fn write_editor_temp_file(issue_id: &str, kind: &str, content: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "trx-{issue_id}-{kind}-{}-{}.md",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::write(&path, content)?;
    Ok(path)
}

fn read_editor_body(path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(path)?;
    Ok(content
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_start_matches('\n')
        .to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Normal,
    Search,
    Help,
    Sort,
    Filter,
    WhichKey(WhichKeyContext),
    AddIssue,
    EditIssue,
    Dashboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhichKeyContext {
    Status,
    Priority,
    Type,
    Labels,
}

#[derive(Debug, Clone, PartialEq)]
enum KeyAction {
    Quit,
    Up,
    Down,
    Left,
    Right,
    PageDown,
    PageUp,
    Enter,
    Tab,
    Escape,
    Backspace,
    Char(char),
    ToggleSelect,
    SelectAll,
    Noop,
}

fn parse_key_action(key: KeyEvent) -> KeyAction {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Quit,
        KeyCode::Up => KeyAction::Up,
        KeyCode::Down => KeyAction::Down,
        KeyCode::Left => KeyAction::Left,
        KeyCode::Right => KeyAction::Right,
        KeyCode::Char('j') => KeyAction::Down,
        KeyCode::Char('k') => KeyAction::Up,
        KeyCode::Char('h') => KeyAction::Left,
        KeyCode::Char('l') => KeyAction::Right,
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::PageDown,
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::PageUp,
        KeyCode::Enter => KeyAction::Enter,
        KeyCode::Tab => KeyAction::Tab,
        KeyCode::Esc => KeyAction::Escape,
        KeyCode::Backspace => KeyAction::Backspace,
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::SelectAll,
        KeyCode::Char(' ') => KeyAction::ToggleSelect,
        KeyCode::Char(c) => KeyAction::Char(c),
        _ => KeyAction::Noop,
    }
}

struct App {
    filtered_issues: Vec<Issue>,
    mode: AppMode,
    g_prefix: bool,
    search_query: String,

    filter_state: FilterState,
    selection: SelectionState,
    details_scroll: usize,

    store: Store,

    status_message: Option<String>,
    status_message_time: Option<Instant>,

    issue_form: IssueForm,

    // Event log + visualizations.
    events: Vec<Event>,
    sessions: Vec<SessionSummary>,
    session_selection: SelectionState,
    middle_view: MiddleView,
    detail_view: DetailView,
    verbose_ctx: bool,
    follow: bool,
    follow_counter: u32,
}

/// Which list the middle pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MiddleView {
    Issues,
    Sessions,
}

/// Which pane the right pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailView {
    /// Issue metadata (the original view).
    Issue,
    /// Event timeline (per-issue or per-session depending on `MiddleView`).
    Activity,
}

struct IssueForm {
    title: String,
    description: String,
    issue_type: trx_core::IssueType,
    priority: u8,
    status: trx_core::Status,
    selected_field: FormField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormField {
    Title,
    Description,
    IssueType,
    Priority,
    Status,
}

impl IssueForm {
    fn new() -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            issue_type: trx_core::IssueType::Task,
            priority: 2,
            status: trx_core::Status::Open,
            selected_field: FormField::Title,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn from_issue(issue: &Issue) -> Self {
        Self {
            title: issue.title.clone(),
            description: issue.description.clone().unwrap_or_default(),
            issue_type: issue.issue_type,
            priority: issue.priority,
            status: issue.status,
            selected_field: FormField::Title,
        }
    }
}

struct FilterState {
    show_closed: bool,
    enabled_statuses: HashSet<Status>,
    enabled_types: HashSet<trx_core::IssueType>,
    enabled_labels: HashSet<String>,
    ready_only: bool,
    show_blocked: bool,
}

struct SelectionState {
    index: usize,
    offset: usize,
    selected_indices: HashSet<usize>,
}

impl SelectionState {
    fn new() -> Self {
        Self {
            index: 0,
            offset: 0,
            selected_indices: HashSet::new(),
        }
    }

    fn next(&mut self, max: usize, page_size: usize) {
        if max == 0 {
            return;
        }
        self.index = (self.index + 1).min(max - 1);
        self.adjust_offset(page_size);
    }

    fn previous(&mut self) {
        self.index = self.index.saturating_sub(1);
        self.adjust_offset(0);
    }

    fn top(&mut self) {
        self.index = 0;
        self.offset = 0;
    }

    fn bottom(&mut self, max: usize, page_size: usize) {
        if max == 0 {
            return;
        }
        self.index = max - 1;
        if self.index >= self.offset + page_size {
            self.offset = max.saturating_sub(page_size);
        }
    }

    fn page_down(&mut self, max: usize, page_size: usize) {
        if max == 0 {
            return;
        }
        self.index = (self.index + page_size).min(max - 1);
        self.adjust_offset(page_size);
    }

    fn page_up(&mut self) {
        self.index = self.index.saturating_sub(10);
        self.adjust_offset(0);
    }

    fn adjust_offset(&mut self, page_size: usize) {
        if self.index < self.offset {
            self.offset = self.index;
        } else if page_size > 0 && self.index >= self.offset + page_size {
            self.offset = self.index.saturating_sub(page_size - 1);
        }
    }

    fn toggle_selection(&mut self) {
        if self.selected_indices.contains(&self.index) {
            self.selected_indices.remove(&self.index);
        } else {
            self.selected_indices.insert(self.index);
        }
    }

    fn select_all(&mut self, max: usize) {
        self.selected_indices = (0..max).collect();
    }

    fn deselect_all(&mut self) {
        self.selected_indices.clear();
    }
}

impl FilterState {
    fn new() -> Self {
        let mut enabled_statuses = HashSet::new();
        enabled_statuses.insert(Status::Open);
        enabled_statuses.insert(Status::InProgress);
        enabled_statuses.insert(Status::Blocked);

        let mut enabled_types = HashSet::new();
        enabled_types.insert(trx_core::IssueType::Bug);
        enabled_types.insert(trx_core::IssueType::Feature);
        enabled_types.insert(trx_core::IssueType::Task);
        enabled_types.insert(trx_core::IssueType::Epic);
        enabled_types.insert(trx_core::IssueType::Chore);

        Self {
            show_closed: false,
            enabled_statuses,
            enabled_types,
            enabled_labels: HashSet::new(),
            ready_only: false,
            show_blocked: false,
        }
    }

    fn matches(&self, issue: &Issue, query: &str) -> bool {
        if !self.show_closed && issue.status.is_closed() {
            return false;
        }

        if !self.enabled_statuses.contains(&issue.status)
            && !(self.show_closed && issue.status == Status::Closed)
        {
            return false;
        }

        if !self.enabled_types.contains(&issue.issue_type) {
            return false;
        }

        if !self.enabled_labels.is_empty()
            && !issue.labels.iter().any(|l| self.enabled_labels.contains(l))
        {
            return false;
        }

        if self.ready_only && issue.is_blocked_by(&Vec::new()) {
            return false;
        }

        if self.show_blocked && !issue.is_blocked_by(&Vec::new()) {
            return false;
        }

        if !query.is_empty() {
            let query_lower = query.to_lowercase();
            let title_match = issue.title.to_lowercase().contains(&query_lower);
            let id_match = issue.id.to_lowercase().contains(&query_lower);
            let desc_match = issue
                .description
                .as_ref()
                .map(|d| d.to_lowercase().contains(&query_lower))
                .unwrap_or(false);

            if !title_match && !id_match && !desc_match {
                return false;
            }
        }

        true
    }
}

impl App {
    fn new(store: Store) -> Result<Self> {
        let mut app = Self {
            filtered_issues: Vec::new(),
            mode: AppMode::Normal,
            g_prefix: false,
            search_query: String::new(),
            filter_state: FilterState::new(),
            selection: SelectionState::new(),
            details_scroll: 0,
            store,
            status_message: None,
            status_message_time: None,
            issue_form: IssueForm::new(),
            events: Vec::new(),
            sessions: Vec::new(),
            session_selection: SelectionState::new(),
            middle_view: MiddleView::Issues,
            detail_view: DetailView::Issue,
            verbose_ctx: false,
            follow: false,
            follow_counter: 0,
        };

        app.apply_filters()?;
        app.reload_events();
        Ok(app)
    }

    /// Reload `.trx/events.jsonl` into the in-memory cache and recompute
    /// session summaries. Errors are reported via the status bar — losing the
    /// activity feed shouldn't crash the TUI.
    fn reload_events(&mut self) {
        let log = EventLog::at(&self.store.trx_dir());
        match log.read_all() {
            Ok(mut all) => {
                all.sort_by_key(|e| e.timestamp);
                self.sessions = summarize_sessions(&all);
                self.events = all;
                let max = self.sessions.len();
                if self.session_selection.index >= max {
                    self.session_selection.index = max.saturating_sub(1);
                }
            }
            Err(e) => {
                self.show_status(format!("event log error: {}", e));
            }
        }
    }

    /// Events for the currently selected issue (or empty when nothing
    /// selected), oldest → newest.
    fn events_for_selected_issue(&self) -> Vec<&Event> {
        let Some(issue) = self.current_issue() else {
            return Vec::new();
        };
        self.events
            .iter()
            .filter(|e| e.issue_id == issue.id)
            .collect()
    }

    /// Events for the currently selected session (oldest → newest).
    fn events_for_selected_session(&self) -> Vec<&Event> {
        let Some(session) = self.current_session() else {
            return Vec::new();
        };
        self.events
            .iter()
            .filter(|e| e.session_key().unwrap_or("-") == session.session_id)
            .collect()
    }

    fn current_session(&self) -> Option<&SessionSummary> {
        self.sessions.get(self.session_selection.index)
    }

    fn apply_filters(&mut self) -> Result<()> {
        let issues: Vec<&Issue> = if self.filter_state.show_closed {
            self.store.list(false)
        } else {
            self.store.list_open()
        };

        self.filtered_issues = issues
            .into_iter()
            .filter(|i| self.filter_state.matches(i, &self.search_query))
            .cloned()
            .collect();

        self.filtered_issues.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });

        let max = self.filtered_issues.len();
        if self.selection.index >= max {
            self.selection.index = max.saturating_sub(1);
        }

        self.show_status(format!("Showing {} issues", self.filtered_issues.len()));
        Ok(())
    }

    fn handle_key_action(&mut self, action: KeyAction) -> Result<bool> {
        match self.mode {
            AppMode::Normal => self.handle_normal_mode(action),
            AppMode::Search => self.handle_search_mode(action),
            AppMode::Help => self.handle_help_mode(action),
            AppMode::Sort => self.handle_sort_mode(action),
            AppMode::Filter => self.handle_filter_mode(action),
            AppMode::WhichKey(ctx) => self.handle_which_key_mode(ctx, action),
            AppMode::AddIssue => self.handle_add_issue_mode(action),
            AppMode::EditIssue => self.handle_edit_issue_mode(action),
            AppMode::Dashboard => self.handle_dashboard_mode(action),
        }
    }

    fn handle_dashboard_mode(&mut self, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Quit => return Ok(true),
            KeyAction::Escape | KeyAction::Char('D') | KeyAction::Char('q') => {
                self.mode = AppMode::Normal;
                self.details_scroll = 0;
            }
            KeyAction::Char('F') => {
                self.follow = !self.follow;
                self.show_status(format!(
                    "Follow: {}",
                    if self.follow { "ON" } else { "off" }
                ));
            }
            KeyAction::Char('r') => {
                self.reload_events();
                self.show_status("Refreshed".to_string());
            }
            KeyAction::Char('?') => {
                self.mode = AppMode::Help;
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_normal_mode(&mut self, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Quit => return Ok(true),
            KeyAction::Escape => {
                self.mode = AppMode::Normal;
                self.g_prefix = false;
            }
            KeyAction::Up => match self.middle_view {
                MiddleView::Issues => self.selection.previous(),
                MiddleView::Sessions => self.session_selection.previous(),
            },
            KeyAction::Down => match self.middle_view {
                MiddleView::Issues => self.selection.next(self.filtered_issues.len(), 20),
                MiddleView::Sessions => self.session_selection.next(self.sessions.len(), 20),
            },
            KeyAction::PageDown => match self.middle_view {
                MiddleView::Issues => {
                    self.selection.page_down(self.filtered_issues.len(), 20);
                }
                MiddleView::Sessions => {
                    self.session_selection.page_down(self.sessions.len(), 20);
                }
            },
            KeyAction::PageUp => match self.middle_view {
                MiddleView::Issues => self.selection.page_up(),
                MiddleView::Sessions => self.session_selection.page_up(),
            },
            KeyAction::Char('g') => {
                if self.g_prefix {
                    self.selection.top();
                    self.g_prefix = false;
                } else {
                    self.g_prefix = true;
                }
            }
            KeyAction::Char('G') => {
                self.selection.bottom(self.filtered_issues.len(), 20);
                self.g_prefix = false;
            }
            KeyAction::Char('q') => {
                return Ok(true);
            }
            KeyAction::Char('a') => {
                self.mode = AppMode::AddIssue;
            }
            KeyAction::Char('e') => {
                if let Some(issue) = self.current_issue() {
                    self.issue_form = IssueForm::from_issue(issue);
                    self.mode = AppMode::EditIssue;
                }
            }
            KeyAction::Char('1') => {
                let _ = self.change_issue_status(trx_core::Status::Open);
            }
            KeyAction::Char('2') => {
                let _ = self.change_issue_status(trx_core::Status::InProgress);
            }
            KeyAction::Char('3') => {
                let _ = self.change_issue_status(trx_core::Status::Blocked);
            }
            KeyAction::Char('4') => {
                let _ = self.change_issue_status(trx_core::Status::Closed);
            }
            KeyAction::Char('c') => {
                let _ = self.close_issue();
            }
            KeyAction::Char(' ') => {
                self.selection.toggle_selection();
                self.selection.next(self.filtered_issues.len(), 20);
            }
            KeyAction::SelectAll => {
                self.selection.select_all(self.filtered_issues.len());
                self.show_status("All items selected".to_string());
            }
            KeyAction::Enter => {
                self.mode = AppMode::Normal;
            }
            KeyAction::Char('V') => {
                self.selection.deselect_all();
                self.show_status("Selection cleared".to_string());
            }
            KeyAction::Char('/') => {
                self.mode = AppMode::Search;
                self.search_query.clear();
            }
            KeyAction::Char('?') => {
                self.mode = AppMode::Help;
            }
            KeyAction::Char('s') => {
                self.mode = AppMode::Sort;
            }
            KeyAction::Char('r') => {
                self.apply_filters()?;
                self.reload_events();
                self.show_status("Refreshed".to_string());
            }
            KeyAction::Char('S') => {
                self.middle_view = match self.middle_view {
                    MiddleView::Issues => MiddleView::Sessions,
                    MiddleView::Sessions => MiddleView::Issues,
                };
                // When swapping to sessions, default the right pane to
                // Activity since "session details" only really exist as a
                // timeline of events.
                if self.middle_view == MiddleView::Sessions {
                    self.detail_view = DetailView::Activity;
                }
                self.details_scroll = 0;
                self.show_status(format!(
                    "View: {}",
                    match self.middle_view {
                        MiddleView::Issues => "Issues",
                        MiddleView::Sessions => "Sessions",
                    }
                ));
            }
            KeyAction::Char('T') => {
                self.detail_view = match self.detail_view {
                    DetailView::Issue => DetailView::Activity,
                    DetailView::Activity => DetailView::Issue,
                };
                self.details_scroll = 0;
                self.show_status(format!(
                    "Right pane: {}",
                    match self.detail_view {
                        DetailView::Issue => "Issue details",
                        DetailView::Activity => "Activity",
                    }
                ));
            }
            KeyAction::Char('v') => {
                self.verbose_ctx = !self.verbose_ctx;
                self.show_status(format!(
                    "AGENT_CTX: {}",
                    if self.verbose_ctx {
                        "verbose"
                    } else {
                        "compact"
                    }
                ));
            }
            KeyAction::Char('F') => {
                self.follow = !self.follow;
                self.show_status(format!(
                    "Follow: {}",
                    if self.follow { "ON" } else { "off" }
                ));
            }
            KeyAction::Char('D') => {
                self.mode = AppMode::Dashboard;
                self.details_scroll = 0;
                self.show_status("Dashboard — press D or Esc to exit".to_string());
            }
            KeyAction::Char('t') => {
                self.mode = AppMode::WhichKey(WhichKeyContext::Type);
            }
            KeyAction::Char('p') => {
                self.mode = AppMode::WhichKey(WhichKeyContext::Priority);
            }
            KeyAction::Char('l') => {
                self.mode = AppMode::WhichKey(WhichKeyContext::Labels);
            }
            KeyAction::Char('f') => {
                self.mode = AppMode::Filter;
            }
            _ => {
                self.g_prefix = false;
            }
        }

        self.details_scroll = 0;
        Ok(false)
    }

    fn handle_search_mode(&mut self, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Quit | KeyAction::Char('q') => {
                return Ok(true);
            }
            KeyAction::Escape => {
                self.mode = AppMode::Normal;
            }
            KeyAction::Enter => {
                self.mode = AppMode::Normal;
                self.apply_filters()?;
            }
            KeyAction::Backspace => {
                self.search_query.pop();
                self.apply_filters()?;
            }
            KeyAction::Char(c) => {
                self.search_query.push(c);
                self.apply_filters()?;
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_help_mode(&mut self, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Escape | KeyAction::Char('q') => {
                self.mode = AppMode::Normal;
            }
            KeyAction::Down => {}
            KeyAction::Up => {}
            _ => {}
        }
        Ok(false)
    }

    fn handle_sort_mode(&mut self, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Escape => {
                self.mode = AppMode::Normal;
            }
            KeyAction::Char('1') => {
                self.sort_by_priority();
                self.mode = AppMode::Normal;
            }
            KeyAction::Char('2') => {
                self.sort_by_date();
                self.mode = AppMode::Normal;
            }
            KeyAction::Char('3') => {
                self.sort_by_status();
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_filter_mode(&mut self, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Escape => {
                self.mode = AppMode::Normal;
            }
            // Status filters
            KeyAction::Char('o') => {
                self.toggle_status_filter(Status::Open);
            }
            KeyAction::Char('i') => {
                self.toggle_status_filter(Status::InProgress);
            }
            KeyAction::Char('b') => {
                self.toggle_status_filter(Status::Blocked);
            }
            KeyAction::Char('c') => {
                self.filter_state.show_closed = !self.filter_state.show_closed;
                self.apply_filters()?;
            }
            // Type filters
            KeyAction::Char('B') => {
                self.toggle_type_filter(trx_core::IssueType::Bug);
            }
            KeyAction::Char('F') => {
                self.toggle_type_filter(trx_core::IssueType::Feature);
            }
            KeyAction::Char('T') => {
                self.toggle_type_filter(trx_core::IssueType::Task);
            }
            KeyAction::Char('E') => {
                self.toggle_type_filter(trx_core::IssueType::Epic);
            }
            KeyAction::Char('C') => {
                self.toggle_type_filter(trx_core::IssueType::Chore);
            }
            // Priority filters (show only that priority)
            KeyAction::Char('0') => {
                self.filter_by_priority(Some(0));
            }
            KeyAction::Char('1') => {
                self.filter_by_priority(Some(1));
            }
            KeyAction::Char('2') => {
                self.filter_by_priority(Some(2));
            }
            KeyAction::Char('3') => {
                self.filter_by_priority(Some(3));
            }
            KeyAction::Char('4') => {
                self.filter_by_priority(Some(4));
            }
            // Reset all filters
            KeyAction::Char('r') => {
                self.reset_filters();
            }
            _ => {}
        }
        Ok(false)
    }

    fn filter_by_priority(&mut self, priority: Option<u8>) {
        if let Some(p) = priority {
            self.filtered_issues.retain(|i| i.priority == p);
            self.show_status(format!("Filtered to P{}", p));
        }
        self.mode = AppMode::Normal;
    }

    fn reset_filters(&mut self) {
        self.filter_state = FilterState::new();
        self.apply_filters().ok();
        self.show_status("Filters reset".to_string());
        self.mode = AppMode::Normal;
    }

    fn handle_which_key_mode(&mut self, ctx: WhichKeyContext, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Escape => {
                self.mode = AppMode::Normal;
            }
            KeyAction::Char('1') => match ctx {
                WhichKeyContext::Status => {
                    self.toggle_status_filter(Status::Open);
                }
                WhichKeyContext::Type => {
                    self.toggle_type_filter(trx_core::IssueType::Bug);
                }
                WhichKeyContext::Priority => {
                    self.set_priority_filter(0);
                }
                _ => {}
            },
            KeyAction::Char('2') => match ctx {
                WhichKeyContext::Status => {
                    self.toggle_status_filter(Status::InProgress);
                }
                WhichKeyContext::Type => {
                    self.toggle_type_filter(trx_core::IssueType::Feature);
                }
                WhichKeyContext::Priority => {
                    self.set_priority_filter(1);
                }
                _ => {}
            },
            KeyAction::Char('3') => match ctx {
                WhichKeyContext::Status => {
                    self.toggle_status_filter(Status::Blocked);
                }
                WhichKeyContext::Type => {
                    self.toggle_type_filter(trx_core::IssueType::Task);
                }
                WhichKeyContext::Priority => {
                    self.set_priority_filter(2);
                }
                _ => {}
            },
            KeyAction::Char('4') => match ctx {
                WhichKeyContext::Type => {
                    self.toggle_type_filter(trx_core::IssueType::Epic);
                }
                WhichKeyContext::Priority => {
                    self.set_priority_filter(3);
                }
                _ => {}
            },
            KeyAction::Char('5') => match ctx {
                WhichKeyContext::Type => {
                    self.toggle_type_filter(trx_core::IssueType::Chore);
                }
                WhichKeyContext::Priority => {
                    self.set_priority_filter(4);
                }
                _ => {}
            },
            KeyAction::Char('c') if ctx == WhichKeyContext::Status => {
                self.filter_state.show_closed = !self.filter_state.show_closed;
                self.apply_filters()?;
                self.mode = AppMode::Normal;
            }
            KeyAction::Char('r') => {
                self.apply_filters()?;
                self.mode = AppMode::Normal;
            }
            _ => {}
        }

        Ok(false)
    }

    fn toggle_status_filter(&mut self, status: Status) {
        if self.filter_state.enabled_statuses.contains(&status) {
            self.filter_state.enabled_statuses.remove(&status);
        } else {
            self.filter_state.enabled_statuses.insert(status);
        }
        self.apply_filters().ok();
    }

    fn toggle_type_filter(&mut self, itype: trx_core::IssueType) {
        if self.filter_state.enabled_types.contains(&itype) {
            self.filter_state.enabled_types.remove(&itype);
        } else {
            self.filter_state.enabled_types.insert(itype);
        }
        self.apply_filters().ok();
    }

    fn set_priority_filter(&mut self, priority: u8) {
        self.filtered_issues.retain(|i| i.priority == priority);
        self.show_status(format!("Filtered to P{}", priority));
        self.mode = AppMode::Normal;
    }

    fn sort_by_priority(&mut self) {
        self.filtered_issues.sort_by_key(|a| a.priority);
        self.show_status("Sorted by priority".to_string());
    }

    fn sort_by_date(&mut self) {
        self.filtered_issues
            .sort_by_key(|i| std::cmp::Reverse(i.created_at));
        self.show_status("Sorted by date".to_string());
    }

    fn sort_by_status(&mut self) {
        self.filtered_issues.sort_by(|a, b| {
            let a_order = match a.status {
                Status::Open => 0,
                Status::InProgress => 1,
                Status::Blocked => 2,
                Status::Closed => 3,
                Status::Tombstone => 4,
            };
            let b_order = match b.status {
                Status::Open => 0,
                Status::InProgress => 1,
                Status::Blocked => 2,
                Status::Closed => 3,
                Status::Tombstone => 4,
            };
            a_order.cmp(&b_order)
        });
        self.show_status("Sorted by status".to_string());
    }

    fn show_status(&mut self, msg: String) {
        self.status_message = Some(msg);
        self.status_message_time = Some(Instant::now());
    }

    fn on_tick(&mut self) {
        if let Some(time) = self.status_message_time
            && time.elapsed() > Duration::from_secs(3)
        {
            self.status_message = None;
            self.status_message_time = None;
        }

        // Follow mode: re-read the event log every ~2s so newly emitted events
        // appear without manual refresh. Cheap because the file is small and
        // append-only.
        if self.follow {
            self.follow_counter += 1;
            // Tick is ~250ms; refresh every 8 ticks = 2s.
            if self.follow_counter >= 8 {
                self.follow_counter = 0;
                self.reload_events();
                let _ = self.apply_filters();
            }
        }
    }

    fn handle_add_issue_mode(&mut self, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Quit | KeyAction::Char('q') => {
                return Ok(true);
            }
            KeyAction::Escape => {
                self.mode = AppMode::Normal;
                self.issue_form.reset();
            }
            KeyAction::Tab => match self.issue_form.selected_field {
                FormField::Title => self.issue_form.selected_field = FormField::Description,
                FormField::Description => self.issue_form.selected_field = FormField::IssueType,
                FormField::IssueType => self.issue_form.selected_field = FormField::Priority,
                FormField::Priority => self.issue_form.selected_field = FormField::Status,
                FormField::Status => self.issue_form.selected_field = FormField::Title,
            },
            KeyAction::Enter => {
                if self.issue_form.title.trim().is_empty() {
                    self.show_status("Title cannot be empty".to_string());
                    return Ok(false);
                }
                self.create_issue()?;
                self.mode = AppMode::Normal;
                self.issue_form.reset();
                self.show_status("Issue created".to_string());
            }
            KeyAction::Up | KeyAction::Down => match self.issue_form.selected_field {
                FormField::IssueType => {
                    let types = [
                        trx_core::IssueType::Bug,
                        trx_core::IssueType::Feature,
                        trx_core::IssueType::Task,
                        trx_core::IssueType::Epic,
                        trx_core::IssueType::Chore,
                    ];
                    let current_idx = types
                        .iter()
                        .position(|&t| t == self.issue_form.issue_type)
                        .unwrap_or(0);
                    let new_idx = if matches!(action, KeyAction::Up) {
                        (current_idx + types.len().saturating_sub(1)) % types.len()
                    } else {
                        (current_idx + 1) % types.len()
                    };
                    self.issue_form.issue_type = types[new_idx];
                }
                FormField::Priority => {
                    self.issue_form.priority = if matches!(action, KeyAction::Up) {
                        self.issue_form.priority.saturating_add(1).min(4)
                    } else {
                        self.issue_form.priority.saturating_sub(1)
                    };
                }
                FormField::Status => {
                    let statuses = [
                        trx_core::Status::Open,
                        trx_core::Status::InProgress,
                        trx_core::Status::Blocked,
                        trx_core::Status::Closed,
                    ];
                    let current_idx = statuses
                        .iter()
                        .position(|&s| s == self.issue_form.status)
                        .unwrap_or(0);
                    let new_idx = if matches!(action, KeyAction::Up) {
                        (current_idx + statuses.len().saturating_sub(1)) % statuses.len()
                    } else {
                        (current_idx + 1) % statuses.len()
                    };
                    self.issue_form.status = statuses[new_idx];
                }
                _ => {}
            },
            KeyAction::Backspace => match self.issue_form.selected_field {
                FormField::Title => {
                    self.issue_form.title.pop();
                }
                FormField::Description => {
                    self.issue_form.description.pop();
                }
                _ => {}
            },
            KeyAction::Char(c) if c.is_ascii() => match self.issue_form.selected_field {
                FormField::Title => {
                    self.issue_form.title.push(c);
                }
                FormField::Description => {
                    self.issue_form.description.push(c);
                }
                _ => {}
            },
            _ => {}
        }
        Ok(false)
    }

    fn handle_edit_issue_mode(&mut self, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Escape => {
                self.mode = AppMode::Normal;
            }
            KeyAction::Tab => match self.issue_form.selected_field {
                FormField::Title => self.issue_form.selected_field = FormField::Description,
                FormField::Description => self.issue_form.selected_field = FormField::IssueType,
                FormField::IssueType => self.issue_form.selected_field = FormField::Priority,
                FormField::Priority => self.issue_form.selected_field = FormField::Status,
                FormField::Status => self.issue_form.selected_field = FormField::Title,
            },
            KeyAction::Enter => {
                if self.issue_form.title.trim().is_empty() {
                    self.show_status("Title cannot be empty".to_string());
                    return Ok(false);
                }
                self.update_issue()?;
                self.mode = AppMode::Normal;
                self.show_status("Issue updated".to_string());
            }
            KeyAction::Up | KeyAction::Down => match self.issue_form.selected_field {
                FormField::IssueType => {
                    let types = [
                        trx_core::IssueType::Bug,
                        trx_core::IssueType::Feature,
                        trx_core::IssueType::Task,
                        trx_core::IssueType::Epic,
                        trx_core::IssueType::Chore,
                    ];
                    let current_idx = types
                        .iter()
                        .position(|&t| t == self.issue_form.issue_type)
                        .unwrap_or(0);
                    let new_idx = if matches!(action, KeyAction::Up) {
                        (current_idx + types.len().saturating_sub(1)) % types.len()
                    } else {
                        (current_idx + 1) % types.len()
                    };
                    self.issue_form.issue_type = types[new_idx];
                }
                FormField::Priority => {
                    self.issue_form.priority = if matches!(action, KeyAction::Up) {
                        self.issue_form.priority.saturating_add(1).min(4)
                    } else {
                        self.issue_form.priority.saturating_sub(1)
                    };
                }
                FormField::Status => {
                    let statuses = [
                        trx_core::Status::Open,
                        trx_core::Status::InProgress,
                        trx_core::Status::Blocked,
                        trx_core::Status::Closed,
                    ];
                    let current_idx = statuses
                        .iter()
                        .position(|&s| s == self.issue_form.status)
                        .unwrap_or(0);
                    let new_idx = if matches!(action, KeyAction::Up) {
                        (current_idx + statuses.len().saturating_sub(1)) % statuses.len()
                    } else {
                        (current_idx + 1) % statuses.len()
                    };
                    self.issue_form.status = statuses[new_idx];
                }
                _ => {}
            },
            KeyAction::Backspace => match self.issue_form.selected_field {
                FormField::Title => {
                    self.issue_form.title.pop();
                }
                FormField::Description => {
                    self.issue_form.description.pop();
                }
                _ => {}
            },
            KeyAction::Char(c) if c.is_ascii() => match self.issue_form.selected_field {
                FormField::Title => {
                    self.issue_form.title.push(c);
                }
                FormField::Description => {
                    self.issue_form.description.push(c);
                }
                _ => {}
            },
            _ => {}
        }
        Ok(false)
    }

    fn create_issue(&mut self) -> Result<()> {
        use trx_core::generate_id;

        let prefix = self.store.prefix()?;
        let id = generate_id(&prefix);

        let mut issue = trx_core::Issue::new(id, self.issue_form.title.clone());
        issue.description = if self.issue_form.description.trim().is_empty() {
            None
        } else {
            Some(self.issue_form.description.clone())
        };
        issue.issue_type = self.issue_form.issue_type;
        issue.priority = self.issue_form.priority;
        issue.status = self.issue_form.status;

        self.store.create(issue)?;
        self.apply_filters()?;
        Ok(())
    }

    fn update_issue(&mut self) -> Result<()> {
        if let Some(issue) = self.current_issue() {
            let mut updated_issue = issue.clone();
            updated_issue.title = self.issue_form.title.clone();
            updated_issue.description = if self.issue_form.description.trim().is_empty() {
                None
            } else {
                Some(self.issue_form.description.clone())
            };
            updated_issue.issue_type = self.issue_form.issue_type;
            updated_issue.priority = self.issue_form.priority;
            updated_issue.status = self.issue_form.status;
            updated_issue.updated_at = chrono::Utc::now();

            self.store.update(updated_issue)?;
            self.apply_filters()?;
        }
        Ok(())
    }

    fn change_issue_status(&mut self, new_status: trx_core::Status) -> Result<()> {
        if let Some(issue) = self.current_issue() {
            let mut updated_issue = issue.clone();
            updated_issue.status = new_status;
            if new_status == trx_core::Status::Closed {
                updated_issue.closed_at = Some(chrono::Utc::now());
            }
            updated_issue.updated_at = chrono::Utc::now();

            self.store.update(updated_issue)?;
            self.apply_filters()?;
            self.show_status(format!("Issue status changed to {}", new_status));
        }
        Ok(())
    }

    fn close_issue(&mut self) -> Result<()> {
        if let Some(issue) = self.current_issue() {
            let mut updated_issue = issue.clone();
            updated_issue.close(None);
            self.store.update(updated_issue)?;
            self.apply_filters()?;
            self.show_status("Issue closed".to_string());
        }
        Ok(())
    }

    fn current_issue(&self) -> Option<&Issue> {
        self.filtered_issues.get(self.selection.index)
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let size = f.area();

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)].as_ref())
        .split(size);

    if app.mode == AppMode::Dashboard {
        render_dashboard(f, app, main_chunks[0]);
        render_status_bar(f, app, main_chunks[1]);
        return;
    }

    let app_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
        .split(main_chunks[0]);
    render_view_bar(f, app, app_chunks[0]);

    if size.width < 90 {
        match app.detail_view {
            DetailView::Issue if app.middle_view == MiddleView::Issues => {
                render_right_pane(f, app, app_chunks[1]);
            }
            DetailView::Activity => render_activity_pane(f, app, app_chunks[1]),
            _ => render_middle_pane(f, app, app_chunks[1]),
        }
    } else {
        let list_width = if size.width >= 150 {
            72
        } else {
            size.width / 2
        };
        let content_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(list_width), Constraint::Min(30)].as_ref())
            .split(app_chunks[1]);

        match app.middle_view {
            MiddleView::Issues => render_middle_pane(f, app, content_chunks[0]),
            MiddleView::Sessions => render_sessions_pane(f, app, content_chunks[0]),
        }
        match app.detail_view {
            DetailView::Issue => render_right_pane(f, app, content_chunks[1]),
            DetailView::Activity => render_activity_pane(f, app, content_chunks[1]),
        }
    }
    render_status_bar(f, app, main_chunks[1]);

    match &app.mode {
        AppMode::Help => render_help_overlay(f),
        AppMode::Sort => render_sort_overlay(f),
        AppMode::Filter => render_filter_overlay(f, app),
        AppMode::WhichKey(ctx) => render_which_key_overlay(f, *ctx),
        AppMode::AddIssue => render_issue_form(f, app, "Add Issue"),
        AppMode::EditIssue => render_issue_form(f, app, "Edit Issue"),
        _ => {}
    }
}

fn render_view_bar(f: &mut Frame, app: &App, area: Rect) {
    let counts = ViewCounts::from_app(app);
    let active_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let normal_style = Style::default().fg(Color::Gray);
    let warn_style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);

    let mut spans = vec![Span::raw(" ")];
    for (idx, (label, count, hotkey)) in [
        ("Ready", counts.ready, "r"),
        ("Open", counts.open, "o"),
        ("Blocked", counts.blocked, "b"),
        ("Mine", counts.mine, "m"),
        ("Recent", counts.recent, "u"),
        ("Epics", counts.epics, "e"),
    ]
    .into_iter()
    .enumerate()
    {
        if idx > 0 {
            spans.push(Span::styled(" │ ", secondary_style()));
        }
        let style = if view_matches(app, label) {
            active_style
        } else if label == "Blocked" && count > 0 {
            warn_style
        } else {
            normal_style
        };
        spans.push(Span::styled(format!(" {hotkey}:{label} {count} "), style));
    }
    spans.push(Span::styled(" │ ", secondary_style()));
    spans.push(Span::styled(
        " / search  f filters  f→c closed  1 reopen  S sessions  T timeline  v ctx  E edit  ? help ",
        secondary_style(),
    ));

    let title = if app.follow {
        "Views  ● follow"
    } else {
        "Views"
    };
    let paragraph = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(title),
    );
    f.render_widget(paragraph, area);
}

struct ViewCounts {
    ready: usize,
    open: usize,
    blocked: usize,
    mine: usize,
    recent: usize,
    epics: usize,
}

impl ViewCounts {
    fn from_app(app: &App) -> Self {
        let issues = app.store.list(false);
        let graph = IssueGraph::from_issues(&issues);
        let ready_ids: HashSet<&str> = graph
            .ready_issues(&issues)
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        let open = issues.iter().filter(|i| i.status.is_open()).count();
        let blocked = issues
            .iter()
            .filter(|i| i.status == Status::Blocked)
            .count();
        let mine = issues.iter().filter(|i| i.assignee.is_some()).count();
        let epics = issues
            .iter()
            .filter(|i| i.issue_type == trx_core::IssueType::Epic && i.status.is_open())
            .count();
        let recent = issues
            .iter()
            .filter(|i| {
                chrono::Utc::now()
                    .signed_duration_since(i.updated_at)
                    .num_days()
                    <= 7
            })
            .count();
        Self {
            ready: issues
                .iter()
                .filter(|i| ready_ids.contains(i.id.as_str()))
                .count(),
            open,
            blocked,
            mine,
            recent,
            epics,
        }
    }
}

fn view_matches(app: &App, label: &str) -> bool {
    match label {
        "Ready" => app.filter_state.ready_only,
        "Open" => app.filter_state.enabled_statuses.contains(&Status::Open),
        "Blocked" => app.filter_state.enabled_statuses.contains(&Status::Blocked),
        "Epics" => app
            .filter_state
            .enabled_types
            .contains(&trx_core::IssueType::Epic),
        _ => false,
    }
}

fn render_middle_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .filtered_issues
        .iter()
        .enumerate()
        .map(|(idx, issue)| {
            let is_selected = app.selection.selected_indices.contains(&idx);
            let is_cursor = idx == app.selection.index;

            let status_style = match issue.status {
                Status::Open => Style::default().fg(Color::Green),
                Status::InProgress => Style::default().fg(Color::Yellow),
                Status::Blocked => Style::default().fg(Color::Red),
                Status::Closed => secondary_style(),
                Status::Tombstone => secondary_style(),
            };

            let priority_color = match issue.priority {
                0 => Color::Red,
                1 => Color::Red,
                2 => Color::Yellow,
                3 => Color::Green,
                _ => Color::DarkGray,
            };

            let title = if issue.title.len() > 50 {
                format!("{}...", &issue.title[..47])
            } else {
                issue.title.clone()
            };

            let mut prefix = if is_selected {
                "[*] ".to_string()
            } else {
                "[ ] ".to_string()
            };
            if is_cursor {
                prefix = "> ".to_string();
            }

            let content = Line::from(vec![
                Span::styled(prefix, Style::default()),
                Span::styled(issue.id.clone(), Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(
                    format!("[P{}] ", issue.priority),
                    Style::default()
                        .fg(priority_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("[{}] ", issue.issue_type),
                    Style::default().fg(Color::Blue),
                ),
                Span::styled(format!("{} ", issue.status), status_style),
                Span::styled(title, Style::default()),
            ]);

            ListItem::new(content)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(format!("Issues ({})", app.filtered_issues.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(
        list,
        area,
        &mut ratatui::widgets::ListState::default()
            .with_selected(Some(app.selection.index))
            .with_offset(app.selection.offset),
    );
}

fn render_right_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let content = if let Some(issue) = app.current_issue() {
        let status_style = match issue.status {
            Status::Open => Style::default().fg(Color::Green),
            Status::InProgress => Style::default().fg(Color::Yellow),
            Status::Blocked => Style::default().fg(Color::Red),
            Status::Closed => secondary_style(),
            Status::Tombstone => secondary_style(),
        };

        let priority_text = match issue.priority {
            0 => "Critical",
            1 => "High",
            2 => "Medium",
            3 => "Low",
            _ => "Backlog",
        };

        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    issue.id.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    issue.title.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Status:   "),
                Span::styled(format!("{}", issue.status), status_style),
            ]),
            Line::from(vec![
                Span::raw("Priority: "),
                Span::styled(
                    format!("P{} ({})", issue.priority, priority_text),
                    Style::default(),
                ),
            ]),
            Line::from(vec![
                Span::raw("Type:     "),
                Span::styled(
                    format!("{}", issue.issue_type),
                    Style::default().fg(Color::Blue),
                ),
            ]),
            Line::from(vec![
                Span::raw("Created:  "),
                Span::styled(
                    issue.created_at.format("%Y-%m-%d %H:%M").to_string(),
                    Style::default(),
                ),
            ]),
            Line::from(vec![
                Span::raw("Updated:  "),
                Span::styled(
                    issue.updated_at.format("%Y-%m-%d %H:%M").to_string(),
                    Style::default(),
                ),
            ]),
        ];

        if let Some(ref desc) = issue.description {
            lines.push(Line::from(""));
            lines.push(Line::from("Description:"));
            for line in desc.lines() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(line, Style::default()),
                ]));
            }
        }

        if !issue.dependencies.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Dependencies:"));
            for dep in &issue.dependencies {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!(
                            "{} -> {} ({})",
                            dep.issue_id, dep.depends_on_id, dep.dep_type
                        ),
                        Style::default().fg(Color::Yellow),
                    ),
                ]));
            }
        }

        if !issue.labels.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Labels:"));
            for label in &issue.labels {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(label.clone(), Style::default().fg(Color::Magenta)),
                ]));
            }
        }

        if let Some(ref assignee) = issue.assignee {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::raw("Assignee: "),
                Span::styled(assignee, Style::default()),
            ]));
        }

        Text::from(lines)
    } else {
        Text::from(vec![Line::from("No issue selected")])
    };

    let paragraph = Paragraph::new(content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title("Details"),
        )
        .wrap(Wrap { trim: true })
        .scroll((app.details_scroll as u16, 0));

    f.render_widget(paragraph, area);
}

// ============================================================================
// Sessions view + activity pane
// ============================================================================

fn action_color(a: EventAction) -> Color {
    match a {
        EventAction::Created => Color::Green,
        EventAction::Closed => Color::Blue,
        EventAction::Reopened => Color::Yellow,
        EventAction::Deleted => Color::Red,
        EventAction::Restored => Color::Green,
        EventAction::DepAdded | EventAction::DepRemoved => Color::Magenta,
        EventAction::SessionLinked => Color::Cyan,
        EventAction::Updated => Color::White,
    }
}

fn format_field_change(c: &FieldChange) -> String {
    match (&c.from, &c.to) {
        (Some(f), Some(t)) => format!("{}: {} → {}", c.field, f, t),
        (None, Some(t)) => format!("{}: ∅ → {}", c.field, t),
        (Some(f), None) => format!("{}: {} → ∅", c.field, f),
        (None, None) => c.field.clone(),
    }
}

/// Returns a non-empty session name distinct from the session id, or None.
fn session_name_display(s: &SessionSummary) -> Option<String> {
    let n = s.session_name.as_deref()?.trim();
    if n.is_empty() || n == s.session_id {
        return None;
    }
    Some(n.to_string())
}

fn session_display_name(s: &SessionSummary) -> String {
    if let Some(name) = session_name_display(s) {
        return name;
    }
    if s.session_id == "-" {
        return "Unattributed events".to_string();
    }
    short_session_id(&s.session_id)
}

fn short_session_id(id: &str) -> String {
    if id == "-" {
        "-".to_string()
    } else if id.len() > 18 {
        format!("{}…{}", &id[..8], &id[id.len() - 6..])
    } else {
        id.to_string()
    }
}

fn session_actor_line(s: &SessionSummary) -> String {
    let user = s
        .user_id
        .as_deref()
        .filter(|v| !v.is_empty())
        .unwrap_or("unknown user");
    let runtime = s
        .harness
        .as_deref()
        .or(s.platform.as_deref())
        .filter(|v| !v.is_empty())
        .unwrap_or("unknown runtime");
    let model = s
        .model
        .as_deref()
        .filter(|v| !v.is_empty())
        .unwrap_or("unknown model");
    format!("{} · {} · {}", user, runtime, model)
}

fn session_action_summary(events: &[&Event]) -> String {
    let created = events
        .iter()
        .filter(|e| e.action == EventAction::Created)
        .count();
    let updated = events
        .iter()
        .filter(|e| e.action == EventAction::Updated)
        .count();
    let closed = events
        .iter()
        .filter(|e| e.action == EventAction::Closed)
        .count();
    let reopened = events
        .iter()
        .filter(|e| e.action == EventAction::Reopened)
        .count();
    let mut parts = Vec::new();
    if created > 0 {
        parts.push(format!("created {created}"));
    }
    if updated > 0 {
        parts.push(format!("updated {updated}"));
    }
    if closed > 0 {
        parts.push(format!("closed {closed}"));
    }
    if reopened > 0 {
        parts.push(format!("reopened {reopened}"));
    }
    if parts.is_empty() {
        "activity".to_string()
    } else {
        parts.join(" · ")
    }
}

fn secondary_style() -> Style {
    Style::default().fg(Color::Gray)
}

/// Compact AGENT_CTX line for an event, e.g. `claude-code · my-sess · opus-4.7`.
fn event_ctx_compact(e: &Event) -> Option<String> {
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

fn event_ctx_verbose(e: &Event) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |label: &str, val: &Option<String>| {
        if let Some(v) = val {
            out.push((label.to_string(), v.clone()));
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

/// Push the timeline rendering of one event onto `lines`. Same shape used by
/// per-issue and per-session activity views.
fn render_event_lines(lines: &mut Vec<Line<'static>>, e: &Event, verbose: bool) {
    let ts = e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
    let action_str = e.action.to_string();
    lines.push(Line::from(vec![
        Span::styled(ts, secondary_style()),
        Span::raw("  "),
        Span::styled(
            format!("{:<10}", action_str),
            Style::default()
                .fg(action_color(e.action))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(e.issue_id.clone(), Style::default().fg(Color::Cyan)),
    ]));
    if let Some(note) = &e.note {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled("› ", secondary_style()),
            Span::raw(note.clone()),
        ]));
    }
    for c in &e.changes {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled("· ", secondary_style()),
            Span::styled(format_field_change(c), secondary_style()),
        ]));
    }
    if verbose {
        for (label, val) in event_ctx_verbose(e) {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{:<10}", format!("{}:", label)), secondary_style()),
                Span::raw(" "),
                Span::styled(val, secondary_style()),
            ]));
        }
    } else if let Some(ctx) = event_ctx_compact(e) {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("[{}]", ctx), secondary_style()),
        ]));
    }
}

fn render_sessions_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            let is_cursor = idx == app.session_selection.index;
            let prefix = if is_cursor { "> " } else { "  " };
            let events: Vec<&Event> = app
                .events
                .iter()
                .filter(|e| e.session_key().unwrap_or("-") == s.session_id)
                .collect();
            let display = session_display_name(s);
            let header = Line::from(vec![
                Span::raw(prefix),
                Span::styled(
                    display,
                    Style::default()
                        .fg(if s.session_id == "-" {
                            Color::Yellow
                        } else {
                            Color::Cyan
                        })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{} evt", s.event_count),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{} issues", s.issue_ids.len()),
                    Style::default().fg(Color::Magenta),
                ),
            ]);
            let sub = Line::from(vec![
                Span::raw("  "),
                Span::styled(session_actor_line(s), secondary_style()),
            ]);
            let range = format!(
                "{} → {}   {}   id: {}",
                s.first_at.format("%b %d %H:%M"),
                s.last_at.format("%H:%M"),
                session_action_summary(&events),
                short_session_id(&s.session_id)
            );
            let when = Line::from(vec![
                Span::raw("  "),
                Span::styled(range, secondary_style()),
            ]);
            ListItem::new(Text::from(vec![header, sub, when]))
        })
        .collect();

    let title = if app.follow {
        format!("Sessions ({})  ● follow", app.sessions.len())
    } else {
        format!("Sessions ({}) — newest first", app.sessions.len())
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(
        list,
        area,
        &mut ratatui::widgets::ListState::default()
            .with_selected(Some(app.session_selection.index))
            .with_offset(app.session_selection.offset),
    );
}

fn render_activity_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let title: String;

    match app.middle_view {
        MiddleView::Issues => {
            let events = app.events_for_selected_issue();
            if let Some(issue) = app.current_issue() {
                title = format!("Activity — {} ({} events)", issue.id, events.len());
                lines.push(Line::from(vec![
                    Span::styled(
                        issue.id.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(issue.title.clone(), Style::default()),
                ]));
                lines.push(Line::from(""));
            } else {
                title = "Activity".to_string();
            }
            if events.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No events for this issue.",
                    secondary_style(),
                )));
            } else {
                for e in &events {
                    render_event_lines(&mut lines, e, app.verbose_ctx);
                    lines.push(Line::from(""));
                }
            }
        }
        MiddleView::Sessions => {
            let events = app.events_for_selected_session();
            if let Some(s) = app.current_session() {
                let display = session_display_name(s);
                title = format!("Session — {} ({} events)", display, events.len());
                lines.push(Line::from(vec![Span::styled(
                    display,
                    Style::default()
                        .fg(if s.session_id == "-" {
                            Color::Yellow
                        } else {
                            Color::Cyan
                        })
                        .add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(vec![Span::styled(
                    session_actor_line(s),
                    secondary_style(),
                )]));
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "{} → {}   {} events · {} issues",
                        s.first_at.format("%Y-%m-%d %H:%M"),
                        s.last_at.format("%Y-%m-%d %H:%M"),
                        s.event_count,
                        s.issue_ids.len(),
                    ),
                    secondary_style(),
                )]));
                lines.push(Line::from(vec![
                    Span::styled("Summary: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(session_action_summary(&events), secondary_style()),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("session id: ", secondary_style()),
                    Span::styled(s.session_id.clone(), secondary_style()),
                ]));
                if s.session_id == "-" {
                    lines.push(Line::from(vec![Span::styled(
                        "These events predate AGENT_CTX session tagging or came from tools without session metadata.",
                        Style::default().fg(Color::Yellow),
                    )]));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    "Touched issues",
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
                for issue_id in s.issue_ids.iter().take(8) {
                    let title = app
                        .store
                        .get(issue_id)
                        .map(|i| i.title.as_str())
                        .unwrap_or("issue not in current store");
                    lines.push(Line::from(vec![
                        Span::raw("  • "),
                        Span::styled(issue_id.clone(), Style::default().fg(Color::Cyan)),
                        Span::raw("  "),
                        Span::raw(title.to_string()),
                    ]));
                }
                if s.issue_ids.len() > 8 {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  … {} more", s.issue_ids.len() - 8),
                        secondary_style(),
                    )]));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    "Timeline",
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
            } else {
                title = "Session".to_string();
            }
            if events.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No events for this session.",
                    secondary_style(),
                )));
            } else {
                for e in &events {
                    render_event_lines(&mut lines, e, app.verbose_ctx);
                    lines.push(Line::from(""));
                }
            }
        }
    }

    let title = if app.follow {
        format!("{}  ● follow", title)
    } else {
        title
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(title),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.details_scroll as u16, 0));

    f.render_widget(paragraph, area);
}

fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let mode_text = match app.mode {
        AppMode::Normal => "[NORMAL]",
        AppMode::Search => "[SEARCH]",
        AppMode::Help => "[HELP]",
        AppMode::Sort => "[SORT]",
        AppMode::Filter => "[FILTER]",
        AppMode::WhichKey(_) => "[WHICHKEY]",
        AppMode::AddIssue => "[ADD ISSUE]",
        AppMode::EditIssue => "[EDIT ISSUE]",
        AppMode::Dashboard => "[DASHBOARD]",
    };

    let mode_style = match app.mode {
        AppMode::Normal => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    };

    let status_content = if let Some(ref msg) = app.status_message {
        Line::from(vec![
            Span::styled(mode_text, mode_style),
            Span::raw(" | "),
            Span::styled(msg, Style::default().fg(Color::Cyan)),
        ])
    } else {
        let selected_count = app.selection.selected_indices.len();
        let view_tag = match app.middle_view {
            MiddleView::Issues => "Issues",
            MiddleView::Sessions => "Sessions",
        };
        let detail_tag = match app.detail_view {
            DetailView::Issue => "Det",
            DetailView::Activity => "Act",
        };
        let follow_tag = if app.follow { " ● follow" } else { "" };
        Line::from(vec![
            Span::styled(mode_text, mode_style),
            Span::raw(" | "),
            Span::raw(format!(
                "View:{}/{}{} | Sel:{} | ",
                view_tag, detail_tag, follow_tag, selected_count
            )),
            Span::raw(
                "[S]essions [T]imeline [v]erbose [F]ollow [a]dd [e]dit [c]lose [/]search [?]help [q]uit",
            ),
        ])
    };

    let status_bar = Paragraph::new(status_content)
        .style(Style::default().bg(Color::DarkGray))
        .alignment(Alignment::Left);

    f.render_widget(status_bar, area);
}

fn render_help_overlay(f: &mut Frame) {
    let area = centered_rect(70, 80, f.area());

    // Clear the background
    f.render_widget(Clear, area);

    let help_text = vec![
        Line::from(vec![Span::styled(
            "Keyboard Shortcuts",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        )]),
        Line::from(""),
        Line::from("Navigation:"),
        Line::from("  j/Down     Move down"),
        Line::from("  k/Up       Move up"),
        Line::from("  gg         Go to top"),
        Line::from("  G          Go to bottom"),
        Line::from("  Ctrl-d     Page down"),
        Line::from("  Ctrl-u     Page up"),
        Line::from(""),
        Line::from("Selection:"),
        Line::from("  Space      Toggle selection"),
        Line::from("  Ctrl-a     Select all"),
        Line::from("  V          Clear selection"),
        Line::from(""),
        Line::from("Actions:"),
        Line::from("  a          Add issue"),
        Line::from("  e          Edit issue in TUI form"),
        Line::from("  E          Edit description in $VISUAL/$EDITOR"),
        Line::from("  N          Add note in $VISUAL/$EDITOR"),
        Line::from("  c          Close issue"),
        Line::from("  1-4        Set status (1 reopens to Open, 4 closes)"),
        Line::from("  /          Search"),
        Line::from("  s          Sort menu"),
        Line::from("  f          Filter menu; then c toggles closed issues"),
        Line::from("  r          Refresh"),
        Line::from("  ?          Help"),
        Line::from("  q          Quit"),
        Line::from("  Esc        Return to normal mode"),
        Line::from(""),
        Line::from("Layout / activity:"),
        Line::from("  Top bar    Persistent view counts; no wide sidebar by default"),
        Line::from("  <90 cols   Adaptive single-pane focus on current detail/list"),
        Line::from("  S          Toggle list pane: Issues ↔ Sessions"),
        Line::from("  T          Toggle details ↔ timeline/activity"),
        Line::from("  v          Toggle compact/verbose session context"),
        Line::from("  - session  Unattributed events: old/no AGENT_CTX metadata"),
        Line::from("  F          Toggle follow mode (live-tail events)"),
        Line::from("  D          Enter Dashboard (heatmap + bars + live tail)"),
    ];

    let paragraph = Paragraph::new(help_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .style(Style::default().bg(Color::Black))
                .title("Help (press Esc to close)"),
        )
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

fn render_sort_overlay(f: &mut Frame) {
    let area = centered_rect(40, 30, f.area());

    // Clear the background
    f.render_widget(Clear, area);

    let sort_text = vec![
        Line::from(vec![Span::styled(
            "Sort Options",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        )]),
        Line::from(""),
        Line::from("  [1] Priority"),
        Line::from("  [2] Date (newest first)"),
        Line::from("  [3] Status"),
        Line::from(""),
        Line::from("Press number to sort, Esc to cancel"),
    ];

    let paragraph = Paragraph::new(sort_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .style(Style::default().bg(Color::Black))
                .title("Sort"),
        )
        .alignment(Alignment::Left);

    f.render_widget(paragraph, area);
}

fn render_which_key_overlay(f: &mut Frame, ctx: WhichKeyContext) {
    let area = Rect {
        x: 0,
        y: f.area().height.saturating_sub(4),
        width: f.area().width,
        height: 4,
    };

    // Clear the background
    f.render_widget(Clear, area);

    let items = match ctx {
        WhichKeyContext::Status => vec![
            ("1", "Toggle Open"),
            ("2", "Toggle In Progress"),
            ("3", "Toggle Blocked"),
            ("c", "Toggle Closed"),
            ("r", "Reset filters"),
        ],
        WhichKeyContext::Type => vec![
            ("1", "Toggle Bug"),
            ("2", "Toggle Feature"),
            ("3", "Toggle Task"),
            ("4", "Toggle Epic"),
            ("5", "Toggle Chore"),
            ("r", "Reset filters"),
        ],
        WhichKeyContext::Priority => vec![
            ("0", "P0 Critical"),
            ("1", "P1 High"),
            ("2", "P2 Medium"),
            ("3", "P3 Low"),
            ("4", "P4 Backlog"),
            ("r", "Reset filters"),
        ],
        WhichKeyContext::Labels => vec![("r", "Reset filters")],
    };

    let title = match ctx {
        WhichKeyContext::Status => "Status Filter",
        WhichKeyContext::Type => "Type Filter",
        WhichKeyContext::Priority => "Priority Filter",
        WhichKeyContext::Labels => "Label Filter",
    };

    let mut spans = vec![Span::styled(
        format!("[{}] ", title),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];

    for (key, label) in items {
        spans.push(Span::styled(
            format!("[{}] {} | ", key, label),
            Style::default(),
        ));
    }

    let paragraph = Paragraph::new(Line::from(spans))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .style(Style::default().bg(Color::Black)),
        )
        .alignment(Alignment::Left);

    f.render_widget(paragraph, area);
}

fn render_filter_overlay(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 70, f.area());

    // Clear the background
    f.render_widget(Clear, area);

    let check = |enabled: bool| if enabled { "[x]" } else { "[ ]" };

    let filter_text = vec![
        Line::from(vec![Span::styled(
            "Filter Options",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Status:",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(format!(
            "  [o] {} Open",
            check(app.filter_state.enabled_statuses.contains(&Status::Open))
        )),
        Line::from(format!(
            "  [i] {} In Progress",
            check(
                app.filter_state
                    .enabled_statuses
                    .contains(&Status::InProgress)
            )
        )),
        Line::from(format!(
            "  [b] {} Blocked",
            check(app.filter_state.enabled_statuses.contains(&Status::Blocked))
        )),
        Line::from(format!(
            "  [c] {} Show Closed",
            check(app.filter_state.show_closed)
        )),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Type:",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(format!(
            "  [B] {} Bug",
            check(
                app.filter_state
                    .enabled_types
                    .contains(&trx_core::IssueType::Bug)
            )
        )),
        Line::from(format!(
            "  [F] {} Feature",
            check(
                app.filter_state
                    .enabled_types
                    .contains(&trx_core::IssueType::Feature)
            )
        )),
        Line::from(format!(
            "  [T] {} Task",
            check(
                app.filter_state
                    .enabled_types
                    .contains(&trx_core::IssueType::Task)
            )
        )),
        Line::from(format!(
            "  [E] {} Epic",
            check(
                app.filter_state
                    .enabled_types
                    .contains(&trx_core::IssueType::Epic)
            )
        )),
        Line::from(format!(
            "  [C] {} Chore",
            check(
                app.filter_state
                    .enabled_types
                    .contains(&trx_core::IssueType::Chore)
            )
        )),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Priority (filter to single):",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from("  [0] P0 Critical   [1] P1 High   [2] P2 Medium"),
        Line::from("  [3] P3 Low        [4] P4 Backlog"),
        Line::from(""),
        Line::from(vec![
            Span::styled("[r]", Style::default().fg(Color::Yellow)),
            Span::raw(" Reset all filters   "),
            Span::styled("[Esc]", Style::default().fg(Color::Red)),
            Span::raw(" Close"),
        ]),
    ];

    let paragraph = Paragraph::new(filter_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .style(Style::default().bg(Color::Black))
                .title("Filter (toggles apply immediately)"),
        )
        .alignment(Alignment::Left);

    f.render_widget(paragraph, area);
}

fn render_issue_form(f: &mut Frame, app: &App, title: &str) {
    let area = centered_rect(60, 70, f.area());

    // Clear the background
    f.render_widget(Clear, area);

    let form = &app.issue_form;

    let field_style = |selected: bool| {
        if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };

    let text = vec![
        Line::from(vec![Span::styled(
            title,
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Title: ",
                field_style(form.selected_field == FormField::Title),
            ),
            Span::raw(&form.title),
            Span::raw("_"),
        ]),
        Line::from(vec![
            Span::styled(
                "Type: ",
                field_style(form.selected_field == FormField::IssueType),
            ),
            Span::styled(
                format!("[{}]", form.issue_type),
                if form.selected_field == FormField::IssueType {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
            Span::raw(" (↑/↓ to change)"),
        ]),
        Line::from(vec![
            Span::styled(
                "Priority: ",
                field_style(form.selected_field == FormField::Priority),
            ),
            Span::styled(
                format!("P{}", form.priority),
                if form.selected_field == FormField::Priority {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
            Span::raw(" (↑/↓ to change)"),
        ]),
        Line::from(vec![
            Span::styled(
                "Status: ",
                field_style(form.selected_field == FormField::Status),
            ),
            Span::styled(
                format!("[{}]", form.status),
                if form.selected_field == FormField::Status {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
            Span::raw(" (↑/↓ to change)"),
        ]),
        Line::from(vec![Span::styled(
            "Description:",
            field_style(form.selected_field == FormField::Description),
        )]),
        Line::from(vec![
            Span::styled(
                "  ",
                field_style(form.selected_field == FormField::Description),
            ),
            Span::raw(&form.description),
            Span::raw("_"),
        ]),
        Line::from(""),
        Line::from(""),
        Line::from(vec![
            Span::styled("[Enter]", Style::default().fg(Color::Green)),
            Span::raw(" Save  "),
            Span::styled("[Esc]", Style::default().fg(Color::Red)),
            Span::raw(" Cancel  "),
            Span::styled("[Tab]", Style::default().fg(Color::Yellow)),
            Span::raw(" Next field"),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .style(Style::default().bg(Color::Black))
                .title(title),
        )
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

// =============================================================================
// Dashboard mode: full-screen visual summary built from `app.events`.
// =============================================================================

fn render_dashboard(f: &mut Frame, app: &App, area: Rect) {
    let title = if app.follow {
        format!("Dashboard ({} events)  ● follow", app.events.len())
    } else {
        format!("Dashboard ({} events)", app.events.len())
    };
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(title);
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(11),
            Constraint::Min(8),
            Constraint::Length(12),
        ])
        .split(inner);

    render_dashboard_heatmap(f, app, rows[0]);

    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(rows[1]);
    render_dashboard_bars(f, app, mid[0]);
    render_dashboard_sessions(f, app, mid[1]);

    render_dashboard_tail(f, app, rows[2]);
}

fn render_dashboard_heatmap(f: &mut Frame, app: &App, area: Rect) {
    use chrono::{Datelike, Duration as ChronoDuration, NaiveDate, Utc};

    // 13 weeks Mon-anchored grid, matching CLI heatmap.
    let weeks = 13usize;
    let today = Utc::now().date_naive();
    let today_weekday = today.weekday().num_days_from_monday() as i64;
    let last_monday = today - ChronoDuration::days(today_weekday);
    let start = last_monday - ChronoDuration::days(7 * (weeks as i64 - 1));

    let mut counts: std::collections::HashMap<NaiveDate, usize> = std::collections::HashMap::new();
    for e in &app.events {
        let d = e.timestamp.date_naive();
        if d >= start && d <= today {
            *counts.entry(d).or_insert(0) += 1;
        }
    }

    let day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Month header row.
    let mut header = String::from("      ");
    let mut last_month: Option<u32> = None;
    for w in 0..weeks {
        let d = start + ChronoDuration::days((w * 7) as i64);
        let m = d.month();
        if last_month != Some(m) {
            let name = match m {
                1 => "Jan",
                2 => "Feb",
                3 => "Mar",
                4 => "Apr",
                5 => "May",
                6 => "Jun",
                7 => "Jul",
                8 => "Aug",
                9 => "Sep",
                10 => "Oct",
                11 => "Nov",
                _ => "Dec",
            };
            header.push_str(name);
            header.push(' ');
            last_month = Some(m);
        } else {
            header.push_str("  ");
        }
    }
    lines.push(Line::from(Span::styled(header, secondary_style())));

    for (row, dn) in day_names.iter().enumerate() {
        let mut spans: Vec<Span> = Vec::with_capacity(weeks * 2 + 2);
        spans.push(Span::styled(format!("{:<4} ", dn), secondary_style()));
        for w in 0..weeks {
            let d = start + ChronoDuration::days((w * 7 + row) as i64);
            if d > today {
                spans.push(Span::raw("  "));
                continue;
            }
            let c = *counts.get(&d).unwrap_or(&0);
            let (glyph, color) = heatmap_cell(c);
            spans.push(Span::styled(glyph, Style::default().fg(color)));
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }

    // Legend.
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Legend  ", secondary_style()),
        Span::styled("·", secondary_style()),
        Span::raw(" 0   "),
        Span::styled("▒", Style::default().fg(Color::Green)),
        Span::raw(" 1–2   "),
        Span::styled("▓", Style::default().fg(Color::Green)),
        Span::raw(" 3–5   "),
        Span::styled("█", Style::default().fg(Color::LightGreen)),
        Span::raw(" 6+"),
    ]));

    let para = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(format!("Heatmap · last {} weeks", weeks)),
    );
    f.render_widget(para, area);
}

fn heatmap_cell(count: usize) -> (&'static str, Color) {
    match count {
        0 => ("·", Color::DarkGray),
        1..=2 => ("▒", Color::Green),
        3..=5 => ("▓", Color::Green),
        _ => ("█", Color::LightGreen),
    }
}

fn render_dashboard_bars(f: &mut Frame, app: &App, area: Rect) {
    // Aggregate by action and by user.
    let mut by_action: std::collections::HashMap<EventAction, usize> =
        std::collections::HashMap::new();
    let mut by_user: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for e in &app.events {
        *by_action.entry(e.action).or_insert(0) += 1;
        let u = e.user_id.clone().unwrap_or_else(|| "-".to_string());
        *by_user.entry(u).or_insert(0) += 1;
    }

    let max_action = by_action.values().copied().max().unwrap_or(1).max(1);
    let max_user = by_user.values().copied().max().unwrap_or(1).max(1);

    let bar_w: usize = 18;
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        "By action",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    let mut action_rows: Vec<(EventAction, usize)> = by_action.into_iter().collect();
    action_rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (a, n) in action_rows.into_iter().take(8) {
        let filled = ((n as f64 / max_action as f64) * bar_w as f64).round() as usize;
        let bar: String = "█".repeat(filled.min(bar_w));
        let empty: String = " ".repeat(bar_w.saturating_sub(filled));
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<8}", a.to_string()),
                Style::default().fg(action_color(a)),
            ),
            Span::raw(" "),
            Span::styled(bar, Style::default().fg(action_color(a))),
            Span::raw(empty),
            Span::raw(" "),
            Span::styled(n.to_string(), Style::default().fg(Color::Yellow)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "By user",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    let mut user_rows: Vec<(String, usize)> = by_user.into_iter().collect();
    user_rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (u, n) in user_rows.into_iter().take(6) {
        let filled = ((n as f64 / max_user as f64) * bar_w as f64).round() as usize;
        let bar: String = "█".repeat(filled.min(bar_w));
        let empty: String = " ".repeat(bar_w.saturating_sub(filled));
        let label = if u.len() > 12 {
            format!("{}…", &u[..11])
        } else {
            u.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<12}", label),
                Style::default().fg(Color::Magenta),
            ),
            Span::raw(" "),
            Span::styled(bar, Style::default().fg(Color::Magenta)),
            Span::raw(empty),
            Span::raw(" "),
            Span::styled(n.to_string(), Style::default().fg(Color::Yellow)),
        ]));
    }

    let para = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title("Breakdown"),
    );
    f.render_widget(para, area);
}

fn render_dashboard_sessions(f: &mut Frame, app: &App, area: Rect) {
    let total_events = app.events.len().max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "No sessions yet.",
            secondary_style(),
        )));
    } else {
        for s in app.sessions.iter().take(8) {
            let frac = s.event_count as f64 / total_events as f64;
            let bar_w = 12usize;
            let filled = (frac * bar_w as f64).round() as usize;
            let bar: String = "▰".repeat(filled.min(bar_w));
            let empty: String = "▱".repeat(bar_w.saturating_sub(filled));
            let mut row_spans = vec![
                Span::styled(
                    s.session_id.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(bar, Style::default().fg(Color::Green)),
                Span::styled(empty, secondary_style()),
                Span::raw("  "),
                Span::styled(
                    format!("{:>3}", s.event_count),
                    Style::default().fg(Color::Yellow),
                ),
            ];
            if let Some(name) = session_name_display(s) {
                row_spans.push(Span::raw("  "));
                row_spans.push(Span::styled(name, Style::default().fg(Color::Magenta)));
            }
            lines.push(Line::from(row_spans));
            let user = s.user_id.as_deref().unwrap_or("-");
            let harness = s
                .harness
                .as_deref()
                .or(s.platform.as_deref())
                .unwrap_or("-");
            let model = s.model.as_deref().unwrap_or("-");
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{} · {} · {}", user, harness, model),
                    secondary_style(),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!(
                        "{} → {}  ({} issues)",
                        s.first_at.format("%m-%d %H:%M"),
                        s.last_at.format("%H:%M"),
                        s.issue_ids.len()
                    ),
                    secondary_style(),
                ),
            ]));
            lines.push(Line::from(""));
        }
    }

    let para = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(format!("Top sessions ({} total)", app.sessions.len())),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn render_dashboard_tail(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let n = (area.height as usize).saturating_sub(2).max(1);
    let tail: Vec<&Event> = app.events.iter().rev().take(n).collect();
    if tail.is_empty() {
        lines.push(Line::from(Span::styled(
            "No events yet.",
            secondary_style(),
        )));
    }
    for e in tail.iter().rev() {
        let ts = e.timestamp.format("%m-%d %H:%M:%S").to_string();
        let action_str = e.action.to_string();
        let mut spans = vec![
            Span::styled(ts, secondary_style()),
            Span::raw("  "),
            Span::styled(
                format!("{:<9}", action_str),
                Style::default()
                    .fg(action_color(e.action))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(e.issue_id.clone(), Style::default().fg(Color::Cyan)),
        ];
        if let Some(c) = e.changes.first() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(format_field_change(c), secondary_style()));
        } else if let Some(note) = &e.note {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(note.clone(), Style::default().fg(Color::Gray)));
        }
        lines.push(Line::from(spans));
    }
    let title = if app.follow {
        "Live tail  ● follow"
    } else {
        "Live tail"
    };
    let para = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(title),
    );
    f.render_widget(para, area);
}
