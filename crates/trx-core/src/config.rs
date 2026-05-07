//! Configuration for trx
//!
//! Stored in .trx/config.toml

use serde::{Deserialize, Serialize};
use std::path::Path;

/// trx configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Issue ID prefix (e.g., "trx", "myproject")
    pub prefix: String,

    /// Default priority for new issues (0-4)
    pub default_priority: u8,

    /// Default issue type for new issues
    pub default_type: String,

    /// Auto-sync after mutations (git add + commit)
    pub auto_sync: bool,

    /// Sync commit message template
    /// Available variables: {action}, {id}, {title}
    pub sync_message_template: String,

    /// Show closed issues in list by default
    pub show_closed: bool,

    /// Editor command for editing descriptions
    pub editor: Option<String>,

    /// Git settings
    #[serde(default)]
    pub git: GitConfig,

    /// Display settings
    #[serde(default)]
    pub display: DisplayConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: "trx".to_string(),
            default_priority: 2,
            default_type: "task".to_string(),
            auto_sync: false,
            sync_message_template: "trx: {action} {id}".to_string(),
            show_closed: false,
            editor: None,
            git: GitConfig::default(),
            display: DisplayConfig::default(),
        }
    }
}

/// Git-related configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GitConfig {
    /// Automatically stage .trx/ after changes
    pub auto_stage: bool,

    /// Branch to sync to (if different from current)
    pub sync_branch: Option<String>,
}

/// Display configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// Use colors in output
    pub colors: bool,

    /// Date format for display
    pub date_format: String,

    /// Show issue count in list header
    pub show_count: bool,

    /// Maximum title length before truncation
    pub max_title_length: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            colors: true,
            date_format: "%Y-%m-%d %H:%M".to_string(),
            show_count: true,
            max_title_length: 80,
        }
    }
}

impl Config {
    /// Load config from a TOML file
    pub fn load(path: &Path) -> crate::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| crate::Error::Other(format!("Invalid config: {}", e)))?;
        Ok(config)
    }

    /// Save config to a TOML file
    pub fn save(&self, path: &Path) -> crate::Result<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::Error::Other(format!("Failed to serialize config: {}", e)))?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Generate a default config file with comments
    pub fn default_with_comments() -> String {
        r#"# trx configuration
# See schema at: https://raw.githubusercontent.com/byteowlz/schemas/main/trx/trx.config.schema.json

# Issue ID prefix (e.g., "trx", "myproject")
prefix = "trx"

# Default priority for new issues (0=critical, 1=high, 2=medium, 3=low, 4=backlog)
default_priority = 2

# Default issue type (bug, feature, task, epic, chore)
default_type = "task"

# Auto-sync after mutations (git add + commit)
auto_sync = false

# Sync commit message template
# Variables: {action}, {id}, {title}
sync_message_template = "trx: {action} {id}"

# Show closed issues in list by default
show_closed = false

# Editor command for editing descriptions (uses $EDITOR if not set)
# editor = "vim"

[git]
# Automatically stage .trx/ after changes
auto_stage = false

# Branch to sync to (if different from current)
# sync_branch = "main"

[display]
# Use colors in output
colors = true

# Date format for display (strftime format)
date_format = "%Y-%m-%d %H:%M"

# Show issue count in list header
show_count = true

# Maximum title length before truncation
max_title_length = 80
"#
        .to_string()
    }
}
