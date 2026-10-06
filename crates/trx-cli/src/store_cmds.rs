//! CLI commands for the optional central store and its git sync.
//!
//! - `trx central init` — opt the current repository into central mode
//! - `trx central status` — mode, identity, ledger location, sync summary
//! - `trx store init --remote URL` — make the store a synced git repository
//! - `trx store sync [status|pull|push]` — manual sync (commit→pull→push)

use anyhow::{Result, bail};
use colored::Colorize;
use trx_core::central::{self, CentralStore, Checkout};
use trx_core::global_config::GlobalConfig;
use trx_core::{Store, sync};

fn resolve_store_root(
    config: &GlobalConfig,
    store: Option<&str>,
    store_root: Option<&str>,
) -> Result<std::path::PathBuf> {
    Ok(config.resolve_store_root(store, store_root)?)
}

/// Opt the current repository into central mode.
pub fn central_init(store: Option<&str>, store_root: Option<&str>, json: bool) -> Result<()> {
    let config = GlobalConfig::load()?;
    let resolved = resolve_store_root(&config, store, store_root)?;
    // Fresh clones/worktrees have no .trx/ yet: create the minimal checkout
    // state so central mode works with a single command.
    let trx_dir = std::path::Path::new(".trx");
    if !trx_dir.exists() {
        std::fs::create_dir_all(trx_dir)?;
        if !trx_dir.join("config.toml").exists() {
            std::fs::write(
                trx_dir.join("config.toml"),
                "# trx configuration\nprefix = \"trx\"\n",
            )?;
        }
    }
    let repo_root = Store::current_root()?;

    // Refuse to shadow a non-empty repo-local ledger: migration lands with
    // `trx migrate` (epic trx-a1s8.5). Until then, enabling central mode on a
    // populated repo would hide its issues.
    let local_issues = repo_root.join(".trx").join("issues.jsonl");
    if local_issues.is_file() {
        let count = std::fs::read_to_string(&local_issues)?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
        if count > 0 {
            bail!(
                "The repo-local ledger has {count} issues. Enable central mode only after migrating them (`trx migrate`, coming in epic trx-a1s8.5) — refusing to shadow them now."
            );
        }
    }

    let existing = central::read_marker(&repo_root)?;
    if let Some(marker) = &existing {
        let same_store = marker.store.as_deref() == store;
        let same_root = store_root.is_none()
            || marker.store_root.as_deref() == store_root.map(str::to_string).as_deref();
        if same_store && same_root {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "already-central",
                        "identity": Checkout::at(&repo_root)?.identity,
                    })
                );
            } else {
                println!("Already in central mode — nothing to do.");
            }
            return Ok(());
        }
    }

    let cs = CentralStore::open_at(resolved.clone());
    let checkout = Checkout::at(&repo_root)?;
    let repo = cs.register(&checkout)?;
    central::write_marker(&repo_root, store, Some(resolved.as_path()))?;

    // Verify routing end-to-end.
    let verified = Store::open_at(repo_root.clone())?;
    if !verified.is_central() {
        bail!("internal error: marker written but the store did not switch to central mode");
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "central",
                "store_root": resolved.display().to_string(),
                "identity": repo.record.identity,
                "ledger": repo.dir.display().to_string(),
            })
        );
    } else {
        println!("{} Central mode enabled.", "✓".green());
        println!("  store:   {}", resolved.display());
        println!("  ledger:  {}", repo.dir.display());
        println!("  identity: {}", repo.record.identity);
        println!(
            "  This checkout now reads/writes the central store; the repo-local .trx/ is not touched."
        );
        println!("  Cross-machine sync: trx store init --remote <URL>  (in the central store)");
    }
    Ok(())
}

/// Show mode, identity, ledger location and sync summary.
pub fn central_status(json: bool) -> Result<()> {
    let store = Store::open()?;
    let config = GlobalConfig::load()?;

    if !store.is_central() {
        if json {
            println!(
                "{}",
                serde_json::json!({ "mode": "repo-local", "path": store.trx_dir().display().to_string() })
            );
        } else {
            println!("mode:    repo-local");
            println!("ledger:  {}", store.trx_dir().display());
            println!("Enable central mode with: trx central init");
        }
        return Ok(());
    }

    let repo = store
        .central_repo()
        .ok_or_else(|| anyhow::anyhow!("central mode without central repo"))?;
    let sync_status = repo
        .store_root()
        .map(|root| sync::status(&root, &config.sync))
        .transpose()?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "mode": "central",
                "identity": repo.record.identity,
                "name": repo.record.name,
                "store_root": repo.store_root().map(|p| p.display().to_string()),
                "ledger": repo.dir.display().to_string(),
                "issues": store.list(false).len(),
                "sync": sync_status,
            })
        );
        return Ok(());
    }

    println!("mode:     central");
    println!("identity: {}", repo.record.identity);
    println!(
        "store:    {}",
        repo.store_root()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    println!("ledger:   {}", repo.dir.display());
    if let Some(status) = sync_status {
        if status.initialized {
            println!(
                "sync:     remote={} pending_push={} last_pull={} last_push={}",
                status.remote.as_deref().unwrap_or("(none)"),
                status.pending_push,
                status.state.last_pull.as_deref().unwrap_or("never"),
                status.state.last_push.as_deref().unwrap_or("never"),
            );
            if let Some(error) = &status.state.last_error {
                println!("  last error: {error}");
            }
        } else {
            println!("sync:     not initialized (trx store init --remote <URL>)");
        }
    }
    Ok(())
}

/// Make the central store a synced git repository.
pub fn store_init(
    remote: &str,
    store: Option<&str>,
    store_root: Option<&str>,
    json: bool,
) -> Result<()> {
    let config = GlobalConfig::load()?;
    let resolved = resolve_store_root(&config, store, store_root)?;
    let outcome = sync::init_remote(&resolved, remote, &config.sync)?;
    report_outcome(&resolved, &outcome, json)
}

/// Manual sync: full (commit→pull→push) or a single action.
pub fn store_sync(
    action: Option<crate::StoreSyncAction>,
    store: Option<&str>,
    store_root: Option<&str>,
    json: bool,
) -> Result<()> {
    let config = GlobalConfig::load()?;
    let resolved = resolve_store_root(&config, store, store_root)?;
    if !resolved.join(".git").exists() {
        bail!(
            "Store {} is not a git repository. Run: trx store init --remote <URL>",
            resolved.display()
        );
    }
    let outcome = match action {
        None => sync::full_sync(&resolved, &config.sync)?,
        Some(crate::StoreSyncAction::Pull) => sync::pull(&resolved, &config.sync)?,
        Some(crate::StoreSyncAction::Push) => sync::push_only(&resolved, &config.sync)?,
        Some(crate::StoreSyncAction::Status) => {
            let status = sync::status(&resolved, &config.sync)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("store:         {}", status.store_root);
                println!("initialized:   {}", yes_no(status.initialized));
                println!(
                    "remote:        {}",
                    status.remote.as_deref().unwrap_or("(none)")
                );
                println!(
                    "branch:        {}",
                    status.branch.as_deref().unwrap_or("(none)")
                );
                println!("pending push:  {}", status.pending_push);
                println!(
                    "last pull:     {}",
                    status.state.last_pull.as_deref().unwrap_or("never")
                );
                println!(
                    "last push:     {}",
                    status.state.last_push.as_deref().unwrap_or("never")
                );
                if let Some(error) = &status.state.last_error {
                    println!("last error:    {error}");
                }
            }
            return Ok(());
        }
    };
    report_outcome(&resolved, &outcome, json)
}

fn report_outcome(
    store_root: &std::path::Path,
    outcome: &sync::SyncOutcome,
    json: bool,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(outcome)?);
        return Ok(());
    }
    if outcome.committed {
        println!("{} committed local changes", "✓".green());
    }
    if outcome.pulled {
        println!("{} pulled new commits", "↓".green());
    }
    if outcome.pushed {
        println!("{} pushed to remote", "↑".green());
    }
    if let Some(detail) = &outcome.detail {
        println!("{} {detail}", "!".yellow());
    }
    println!(
        "{} pending commits: {}",
        if outcome.pending_commits > 0 {
            "!".yellow()
        } else {
            "✓".green()
        },
        outcome.pending_commits
    );
    let _ = store_root;
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yes_no() {
        assert_eq!(yes_no(true), "yes");
        assert_eq!(yes_no(false), "no");
    }
}
