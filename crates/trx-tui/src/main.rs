//! trx-tui - Terminal UI viewer for trx issues
//!
//! Replaces beads-viewer with a Rust-native TUI.

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
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
use std::io;
use std::time::{Duration, Instant};
use trx_core::{Issue, IssueGraph, Status, UnifiedStore};

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
    let store = UnifiedStore::open()?;
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

    let store = UnifiedStore::open()?;
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

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    let mut last_tick = Instant::now();
    const TICK_RATE: Duration = Duration::from_millis(250);

    loop {
        terminal.draw(|f| ui(f, app))?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
        {
            let action = parse_key_action(key);
            if app.handle_key_action(action)? {
                return Ok(());
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            app.on_tick();
            last_tick = Instant::now();
        }
    }
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

    store: UnifiedStore,

    status_message: Option<String>,
    status_message_time: Option<Instant>,

    issue_form: IssueForm,
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

        if !self.enabled_statuses.contains(&issue.status) {
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
    fn new(store: UnifiedStore) -> Result<Self> {
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
        };

        app.apply_filters()?;
        Ok(app)
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
        }
    }

    fn handle_normal_mode(&mut self, action: KeyAction) -> Result<bool> {
        match action {
            KeyAction::Quit => return Ok(true),
            KeyAction::Escape => {
                self.mode = AppMode::Normal;
                self.g_prefix = false;
            }
            KeyAction::Up => self.selection.previous(),
            KeyAction::Down => self.selection.next(self.filtered_issues.len(), 20),
            KeyAction::PageDown => {
                self.selection.page_down(self.filtered_issues.len(), 20);
            }
            KeyAction::PageUp => self.selection.page_up(),
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
                self.show_status("Refreshed".to_string());
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

    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Percentage(40),
            Constraint::Percentage(40),
        ])
        .split(main_chunks[0]);

    render_left_pane(f, app, content_chunks[0]);
    render_middle_pane(f, app, content_chunks[1]);
    render_right_pane(f, app, content_chunks[2]);
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

fn render_left_pane(f: &mut Frame, app: &App, area: Rect) {
    let mut items = vec![
        ListItem::new(Line::from(vec![Span::styled(
            "Filters",
            Style::default().add_modifier(Modifier::BOLD),
        )])),
        ListItem::new(""),
    ];

    items.push(ListItem::new(Line::from(vec![
        Span::styled("[f]", Style::default().fg(Color::Cyan)),
        Span::raw(" Status filters"),
    ])));

    for status in [Status::Open, Status::InProgress, Status::Blocked] {
        let enabled = app.filter_state.enabled_statuses.contains(&status);
        let prefix = if enabled { "[x]" } else { "[ ]" };
        items.push(ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(prefix, Style::default()),
            Span::raw(format!(" {}", status)),
        ])));
    }

    if app.filter_state.show_closed {
        items.push(ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::styled("[x]", Style::default()),
            Span::raw(" Closed"),
        ])));
    }

    items.push(ListItem::new(""));

    items.push(ListItem::new(Line::from(vec![
        Span::styled("[t]", Style::default().fg(Color::Cyan)),
        Span::raw(" Type filters"),
    ])));

    for itype in [
        trx_core::IssueType::Bug,
        trx_core::IssueType::Feature,
        trx_core::IssueType::Task,
        trx_core::IssueType::Epic,
        trx_core::IssueType::Chore,
    ] {
        let enabled = app.filter_state.enabled_types.contains(&itype);
        let prefix = if enabled { "[x]" } else { "[ ]" };
        items.push(ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(prefix, Style::default()),
            Span::raw(format!(" {}", itype)),
        ])));
    }

    if app.filter_state.ready_only {
        items.push(ListItem::new(""));
        items.push(ListItem::new(Line::from(vec![
            Span::styled("[r]", Style::default().fg(Color::Yellow)),
            Span::raw(" Ready only"),
        ])));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title("Navigation"),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));

    f.render_widget(list, area);
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
                Status::Closed => Style::default().fg(Color::DarkGray),
                Status::Tombstone => Style::default().fg(Color::DarkGray),
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
            Status::Closed => Style::default().fg(Color::DarkGray),
            Status::Tombstone => Style::default().fg(Color::DarkGray),
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
        Line::from(vec![
            Span::styled(mode_text, mode_style),
            Span::raw(" | "),
            Span::raw(format!("Selected: {} | ", selected_count)),
            Span::raw("[:a]dd [:e]dit [:c]lose [1-4]status [:s]ort [:?]help [/]search [:q]uit"),
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
        Line::from("  e          Edit issue"),
        Line::from("  c          Close issue"),
        Line::from("  1-4        Set status (Open/InProgress/Blocked/Closed)"),
        Line::from("  /          Search"),
        Line::from("  s          Sort menu"),
        Line::from("  f          Filter menu"),
        Line::from("  r          Refresh"),
        Line::from("  ?          Help"),
        Line::from("  q          Quit"),
        Line::from("  Esc        Return to normal mode"),
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
