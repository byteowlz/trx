//! Global per-user configuration: `~/.config/trx/config.toml`.
//!
//! This file configures the optional central store and its sync behavior.
//! It is deliberately global: a repository's own `.trx/config.toml` must
//! never be able to redirect the central store or force migration, because
//! cloned repositories are untrusted input.

use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Where a repository's ledger lives when a `.trx/central` marker exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MigratePolicy {
    /// Ask once on a terminal; otherwise warn and continue.
    #[default]
    Prompt,
    /// Migrate a leftover repo-local ledger automatically on first use.
    Auto,
    /// Never migrate automatically; only warn.
    Off,
}

/// Automatic git sync of the central store (opt-in via `trx sync init`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    /// Pull at first central-store access in a process.
    pub auto_pull: bool,
    /// Commit after every central-store write.
    pub auto_commit: bool,
    /// Push after an automatic commit.
    pub auto_push: bool,
    /// Timeout for each git operation in seconds.
    pub timeout_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_pull: true,
            auto_commit: true,
            auto_push: true,
            timeout_secs: 10,
        }
    }
}

/// An additional named central store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreDef {
    /// Absolute or `~`-relative path of the store root.
    pub root: String,
}

/// Root scan entry for discovery (`trx setup`, future `--all` operations).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootScan {
    /// Directory to scan for repo-local `.trx` ledgers.
    pub path: String,
    /// Maximum depth to descend.
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

fn default_max_depth() -> u32 {
    4
}

/// Default storage mode for checkouts that have no `.trx` ledger yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DefaultMode {
    /// Repo-local `.trx` (create via `trx init`); central strictly opt-in.
    #[default]
    RepoLocal,
    /// Central store automatically: a checkout without `.trx` reads/writes
    /// the central store without anything written into the checkout
    /// (mmry-style). `trx init` still forces an explicit repo-local ledger.
    Central,
}

/// Global per-user configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GlobalConfig {
    /// Override for the default central store root.
    pub store_root: Option<String>,
    /// What to do with leftover repo-local ledgers in central-marked repos.
    pub migrate: MigratePolicy,
    /// Mode for checkouts that have no `.trx` ledger yet.
    pub default_mode: DefaultMode,
    /// Where discovery looks for repo-local ledgers.
    pub roots: Vec<RootScan>,
    /// Automatic sync settings for central stores.
    pub sync: SyncConfig,
    /// Named additional stores (`[stores.<name>]`).
    pub stores: BTreeMap<String, StoreDef>,
}

impl GlobalConfig {
    /// Path of the global config file. `TRX_CONFIG` (or the `--config` flag,
    /// which maps to it) selects an alternate file, which must exist when
    /// given — parity with mmry's `--config`/`MMRY_CONFIG`.
    pub fn path() -> crate::Result<PathBuf> {
        if let Ok(path) = std::env::var("TRX_CONFIG") {
            return Ok(PathBuf::from(path));
        }
        Ok(paths::config_base()?.join("trx").join("config.toml"))
    }

    /// True when an alternate config file was explicitly selected.
    pub fn override_active() -> bool {
        std::env::var("TRX_CONFIG").is_ok()
    }

    /// Load the global config; a missing default file yields the default, a
    /// missing *overridden* file is an error.
    pub fn load() -> crate::Result<Self> {
        let path = Self::path()?;
        if !path.is_file() {
            if Self::override_active() {
                return Err(crate::Error::Other(format!(
                    "config override {} does not exist",
                    path.display()
                )));
            }
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        toml::from_str(&content).map_err(|error| {
            crate::Error::Other(format!("invalid global config {}: {error}", path.display()))
        })
    }

    /// Resolve the store root for `store` (None = default store).
    ///
    /// `store_root_override` (the `--store-root` flag) wins over everything.
    /// `TRX_STORE_ROOT` overrides the default store; `TRX_STORE` selects a
    /// named store. `~` expands to the home directory.
    pub fn resolve_store_root(
        &self,
        store: Option<&str>,
        store_root_override: Option<&str>,
    ) -> crate::Result<PathBuf> {
        if let Some(root) = store_root_override {
            return expand_home(root);
        }
        self.store_root(store)
    }

    /// Resolve the store root for `store` (None = default store).
    ///
    /// `TRX_STORE_ROOT` overrides the default store; `TRX_STORE` selects a
    /// named store. `~` expands to the home directory.
    pub fn store_root(&self, store: Option<&str>) -> crate::Result<PathBuf> {
        let selected: Option<String> = store
            .map(str::to_string)
            .or_else(|| std::env::var("TRX_STORE").ok());
        if let Some(name) = selected.as_deref() {
            let def = self.stores.get(name).ok_or_else(|| {
                crate::Error::Other(format!(
                    "unknown store '{name}': define [stores.{name}] with root = \"...\" in {}",
                    Self::path().unwrap_or_default().display()
                ))
            })?;
            return expand_home(&def.root);
        }
        if let Ok(root) = std::env::var("TRX_STORE_ROOT") {
            return expand_home(&root);
        }
        if let Some(root) = &self.store_root {
            return expand_home(root);
        }
        Ok(paths::data_base()?.join("trx"))
    }
}

/// Expand a leading `~/` to the home directory.
fn expand_home(path: &str) -> crate::Result<PathBuf> {
    crate::paths::expand_tilde(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_store(name: &str, root: &str) -> GlobalConfig {
        let mut stores = BTreeMap::new();
        stores.insert(
            name.to_string(),
            StoreDef {
                root: root.to_string(),
            },
        );
        GlobalConfig {
            stores,
            ..Default::default()
        }
    }

    #[test]
    fn test_named_store_resolution() {
        let config = config_with_store("work", "~/trx-work");
        let got = config.store_root(Some("work")).unwrap();
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        assert_eq!(got, home.join("trx-work"));
    }

    #[test]
    fn test_unknown_store_is_an_error() {
        let config = GlobalConfig::default();
        assert!(config.store_root(Some("nope")).is_err());
    }

    #[test]
    fn test_default_store_uses_data_base() {
        let config = GlobalConfig::default();
        let got = config.store_root(None).unwrap();
        assert!(got.ends_with("trx"), "unexpected default root: {got:?}");
    }

    #[test]
    fn test_config_round_trip() {
        let config = config_with_store("work", "/data/trx");
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: GlobalConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed, config);
        assert_eq!(parsed.sync.timeout_secs, 10);
        assert_eq!(parsed.migrate, MigratePolicy::Prompt);
    }

    #[test]
    fn test_default_mode_parsing() {
        let central: GlobalConfig = toml::from_str("default_mode = \"central\"").unwrap();
        assert_eq!(central.default_mode, DefaultMode::Central);
        let local: GlobalConfig = toml::from_str("default_mode = \"repo-local\"").unwrap();
        assert_eq!(local.default_mode, DefaultMode::RepoLocal);
        let empty: GlobalConfig = toml::from_str("").unwrap();
        assert_eq!(
            empty.default_mode,
            DefaultMode::RepoLocal,
            "repo-local stays the default"
        );
    }

    #[test]
    fn test_parse_store_root_override() {
        let parsed: GlobalConfig = toml::from_str("store_root = \"/data/my-trx\"").unwrap();
        assert_eq!(parsed.store_root.as_deref(), Some("/data/my-trx"));
    }
}
