//! trx - Minimal git-backed issue tracker
//!
//! No daemon, no SQLite - just JSONL files in .trx/

use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser)]
#[command(name = "trx")]
#[command(about = "Minimal git-backed issue tracker")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new trx repository
    Init {
        /// Issue ID prefix
        #[arg(long, default_value = "trx")]
        prefix: String,
    },

    /// Create a new issue
    Create {
        /// Issue title
        title: String,

        /// Issue type (bug, feature, task, epic, chore)
        #[arg(short = 't', long, default_value = "task")]
        issue_type: String,

        /// Priority (0=critical, 1=high, 2=medium, 3=low, 4=backlog)
        #[arg(short, long, default_value = "2")]
        priority: u8,

        /// Description (use '-' to read from stdin)
        #[arg(short, long)]
        description: Option<String>,

        /// Parent issue ID (for child issues)
        #[arg(long)]
        parent: Option<String>,

        /// Custom ID prefix (e.g., 'mmry' generates mmry-xxxx)
        #[arg(long)]
        id: Option<String>,

        /// Open $EDITOR for description
        #[arg(long)]
        edit: bool,
    },

    /// List issues
    List {
        /// Filter by status (open, in_progress, blocked, closed)
        #[arg(short, long)]
        status: Option<String>,

        /// Filter by type (bug, feature, task, epic, chore)
        #[arg(short = 't', long)]
        issue_type: Option<String>,

        /// Filter by priority (0-4)
        #[arg(short = 'P', long)]
        priority: Option<u8>,

        /// Search title and description
        #[arg(long)]
        search: Option<String>,

        /// Show children/descendants of an epic
        #[arg(long)]
        epic: Option<String>,

        /// Show all including closed
        #[arg(short, long)]
        all: bool,

        /// Limit number of issues shown
        #[arg(short = 'l', long)]
        limit: Option<usize>,

        /// Filter by label (multiple --label flags for AND filtering)
        #[arg(long)]
        label: Vec<String>,

        /// Filter by assignee (use 'me' for current user)
        #[arg(long)]
        assignee: Option<String>,

        /// Show issues created after this date (ISO or relative: '1 week', '2 days')
        #[arg(long)]
        created_after: Option<String>,

        /// Show issues created before this date (ISO or relative: '1 week', '2 days')
        #[arg(long)]
        created_before: Option<String>,
    },

    /// Show issue details
    Show {
        /// Issue ID
        id: String,
    },

    /// Update an issue
    Update {
        /// Issue ID
        id: String,

        /// New status
        #[arg(long)]
        status: Option<String>,

        /// New priority
        #[arg(short, long)]
        priority: Option<u8>,

        /// New title
        #[arg(long)]
        title: Option<String>,

        /// New description (use '-' to read from stdin)
        #[arg(short, long)]
        description: Option<String>,

        /// Open $EDITOR for description
        #[arg(long)]
        edit: bool,

        /// Clear a field (description, parent, labels, assignee, notes, sessions)
        #[arg(long)]
        clear: Vec<String>,
    },

    /// Close an issue
    Close {
        /// Issue ID
        id: String,

        /// Reason for closing
        #[arg(short, long)]
        reason: Option<String>,
    },

    /// Show ready (unblocked) issues
    Ready,

    /// Manage dependencies
    Dep {
        #[command(subcommand)]
        command: DepCommands,
    },

    /// Batch-create issues from JSON (stdin or file)
    CreateMany {
        /// Path to JSON file (use "-" for stdin)
        #[arg(long = "json-input")]
        json_input: String,

        /// Preview without creating
        #[arg(long)]
        dry_run: bool,
    },

    /// Import a plan file to create an epic with children
    Plan {
        #[command(subcommand)]
        command: PlanCommands,
    },

    /// Git add and commit .trx/
    Sync {
        /// Commit message
        #[arg(short, long)]
        message: Option<String>,

        /// Preview changes without committing
        #[arg(long)]
        dry_run: bool,

        /// Stage changes without committing
        #[arg(long)]
        no_commit: bool,
    },

    /// Generate compact handover summary for agent handoff
    Handover,

    /// Search issues across all repos
    Search {
        /// Search query
        query: String,

        /// Search all sibling repos with .trx/
        #[arg(long)]
        all_repos: bool,
    },

    /// Migrate storage format
    Migrate {
        /// Preview migration without making changes
        #[arg(long)]
        dry_run: bool,

        /// Rollback from v2 to v1
        #[arg(long)]
        rollback: bool,

        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },

    /// Import from beads
    Import {
        /// Path to beads issues.jsonl
        path: String,

        /// New prefix for imported issues
        #[arg(long)]
        prefix: Option<String>,
    },

    /// Remove beads from repository
    PurgeBeads {
        /// Skip confirmation
        #[arg(long)]
        force: bool,
    },

    /// Output JSON schema for config file
    Schema,

    /// Resolve merge conflicts in ISSUES.md by regenerating from source files
    Resolve,

    /// Manage git merge driver for auto-resolving ISSUES.md conflicts
    MergeDriver {
        #[command(subcommand)]
        command: MergeDriverCommands,
    },

    /// Show or edit configuration
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommands>,
    },

    /// Manage trx-api service
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Show current configuration
    Show,
    /// Edit configuration file
    Edit,
    /// Reset to default configuration
    Reset,
    /// Get a specific config value
    Get {
        /// Config key (e.g., "prefix", "display.colors")
        key: String,
    },
    /// Set a config value
    Set {
        /// Config key
        key: String,
        /// New value
        value: String,
    },
}

#[derive(Subcommand)]
enum ServiceCommands {
    /// Start the API service in background
    Start,

    /// Run the API service in foreground (for debugging)
    Run,

    /// Stop the API service
    Stop,

    /// Restart the API service
    Restart,

    /// Show service status
    Status,

    /// Show instructions for enabling auto-start
    Enable,
}

#[derive(Subcommand)]
enum MergeDriverCommands {
    /// Install git merge driver for auto-resolving ISSUES.md conflicts
    Install,

    /// Remove the git merge driver configuration
    Uninstall,

    /// Show merge driver status
    Status,
}

#[derive(Subcommand)]
enum DepCommands {
    /// Add a dependency
    Add {
        /// Issue ID
        id: String,

        /// Issue this blocks
        #[arg(long)]
        blocks: String,
    },

    /// Remove a dependency
    Rm {
        /// Issue ID
        id: String,

        /// Issue to unblock
        #[arg(long)]
        blocks: String,
    },

    /// Mark an issue as blocked by one or more blockers
    Block {
        /// The issue that is blocked
        id: String,

        /// Blocker issue ID(s), comma-separated
        #[arg(long)]
        by: String,
    },

    /// Remove blockers from an issue
    Unblock {
        /// The issue to unblock
        id: String,

        /// Blocker issue ID(s) to remove, comma-separated
        #[arg(long)]
        by: String,
    },

    /// Show dependency tree
    Tree {
        /// Issue ID
        id: String,
    },
}

#[derive(Subcommand)]
enum PlanCommands {
    /// Import a plan file to create an epic with children
    Import {
        /// Path to plan file (Markdown or JSON)
        path: String,

        /// Epic title (required for Markdown input)
        #[arg(long)]
        epic: Option<String>,

        /// Epic priority
        #[arg(long, default_value = "2")]
        priority: u8,

        /// Preview without creating
        #[arg(long)]
        dry_run: bool,
    },

    /// Print example plan files (Markdown and JSON)
    Example {
        /// Format to show: "md", "json", or "all" (default)
        #[arg(default_value = "all")]
        format: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { prefix } => commands::init(&prefix),
        Commands::Create {
            title,
            issue_type,
            priority,
            description,
            parent,
            id,
            edit,
        } => commands::create(
            &title,
            &issue_type,
            priority,
            description,
            parent,
            id,
            edit,
            cli.json,
        ),
        Commands::List {
            status,
            issue_type,
            priority,
            search,
            epic,
            all,
            limit,
            label,
            assignee,
            created_after,
            created_before,
        } => commands::list(
            status,
            issue_type,
            priority,
            search,
            epic,
            all,
            limit,
            label,
            assignee,
            created_after,
            created_before,
            cli.json,
        ),
        Commands::Show { id } => commands::show(&id, cli.json),
        Commands::Update {
            id,
            status,
            priority,
            title,
            description,
            edit,
            clear,
        } => commands::update(
            &id,
            status,
            priority,
            title,
            description,
            edit,
            clear,
            cli.json,
        ),
        Commands::Close { id, reason } => commands::close(&id, reason, cli.json),
        Commands::Ready => commands::ready(cli.json),
        Commands::Dep { command } => match command {
            DepCommands::Add { id, blocks } => commands::dep_add(&id, &blocks, cli.json),
            DepCommands::Rm { id, blocks } => commands::dep_rm(&id, &blocks, cli.json),
            DepCommands::Block { id, by } => commands::dep_block(&id, &by, cli.json),
            DepCommands::Unblock { id, by } => commands::dep_unblock(&id, &by, cli.json),
            DepCommands::Tree { id } => commands::dep_tree(&id, cli.json),
        },
        Commands::CreateMany {
            json_input,
            dry_run,
        } => commands::create_many(&json_input, dry_run, cli.json),
        Commands::Plan { command } => match command {
            PlanCommands::Import {
                path,
                epic,
                priority,
                dry_run,
            } => commands::plan_import(&path, epic, priority, dry_run, cli.json),
            PlanCommands::Example { format } => commands::plan_example(&format),
        },
        Commands::Sync {
            message,
            dry_run,
            no_commit,
        } => commands::sync(message, dry_run, no_commit),
        Commands::Handover => commands::handover(cli.json),
        Commands::Search { query, all_repos } => commands::search(&query, all_repos, cli.json),
        Commands::Migrate {
            dry_run,
            rollback,
            yes,
        } => commands::migrate(dry_run, rollback, yes),
        Commands::Import { path, prefix } => commands::import(&path, prefix, cli.json),
        Commands::PurgeBeads { force } => commands::purge_beads(force),
        Commands::Schema => commands::schema(),
        Commands::Resolve => commands::resolve(cli.json),
        Commands::MergeDriver { command } => match command {
            MergeDriverCommands::Install => commands::merge_driver_install(),
            MergeDriverCommands::Uninstall => commands::merge_driver_uninstall(),
            MergeDriverCommands::Status => commands::merge_driver_status(),
        },
        Commands::Config { command } => match command {
            Some(ConfigCommands::Show) => commands::config_show(cli.json),
            Some(ConfigCommands::Edit) => commands::config_edit(),
            Some(ConfigCommands::Reset) => commands::config_reset(),
            Some(ConfigCommands::Get { key }) => commands::config_get(&key, cli.json),
            Some(ConfigCommands::Set { key, value }) => commands::config_set(&key, &value),
            None => commands::config_show(cli.json),
        },
        Commands::Service { command } => commands::service(command),
    }
}

impl commands::ServiceCommand for ServiceCommands {
    fn is_start(&self) -> bool {
        matches!(self, ServiceCommands::Start)
    }
    fn is_run(&self) -> bool {
        matches!(self, ServiceCommands::Run)
    }
    fn is_stop(&self) -> bool {
        matches!(self, ServiceCommands::Stop)
    }
    fn is_restart(&self) -> bool {
        matches!(self, ServiceCommands::Restart)
    }
    fn is_status(&self) -> bool {
        matches!(self, ServiceCommands::Status)
    }
    fn is_enable(&self) -> bool {
        matches!(self, ServiceCommands::Enable)
    }
}
