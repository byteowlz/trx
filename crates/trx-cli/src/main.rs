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

        /// Description
        #[arg(short, long)]
        description: Option<String>,

        /// Parent issue ID (for child issues)
        #[arg(long)]
        parent: Option<String>,
    },

    /// List issues
    List {
        /// Filter by status
        #[arg(short, long)]
        status: Option<String>,

        /// Filter by type
        #[arg(short = 't', long)]
        issue_type: Option<String>,

        /// Show all including closed
        #[arg(short, long)]
        all: bool,

        /// Limit number of issues shown
        #[arg(short = 'l', long)]
        limit: Option<usize>,
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

        /// New description
        #[arg(short, long)]
        description: Option<String>,
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

    /// Git add and commit .trx/
    Sync {
        /// Commit message
        #[arg(short, long)]
        message: Option<String>,
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

    /// Show dependency tree
    Tree {
        /// Issue ID
        id: String,
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
        } => commands::create(&title, &issue_type, priority, description, parent, cli.json),
        Commands::List {
            status,
            issue_type,
            all,
            limit,
        } => commands::list(status, issue_type, all, limit, cli.json),
        Commands::Show { id } => commands::show(&id, cli.json),
        Commands::Update {
            id,
            status,
            priority,
            title,
            description,
        } => commands::update(&id, status, priority, title, description, cli.json),
        Commands::Close { id, reason } => commands::close(&id, reason, cli.json),
        Commands::Ready => commands::ready(cli.json),
        Commands::Dep { command } => match command {
            DepCommands::Add { id, blocks } => commands::dep_add(&id, &blocks, cli.json),
            DepCommands::Rm { id, blocks } => commands::dep_rm(&id, &blocks, cli.json),
            DepCommands::Tree { id } => commands::dep_tree(&id, cli.json),
        },
        Commands::Sync { message } => commands::sync(message),
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
