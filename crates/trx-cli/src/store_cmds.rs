//! CLI commands for the optional central store and its git sync.
//!
//! - `trx central init` — opt the current repository into central mode
//! - `trx central status` — mode, identity, ledger location, sync summary
//! - `trx store sync init --remote URL` — make the store a synced git repository
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
pub fn central_init(
    dry_run: bool,
    store: Option<&str>,
    store_root: Option<&str>,
    json: bool,
) -> Result<()> {
    let config = GlobalConfig::load()?;
    let resolved = resolve_store_root(&config, store, store_root)?;

    // Dry run: plan only — no .trx/ creation, no registration, no marker.
    // A missing .trx is part of the plan ("would create") instead of an error.
    let repo_root = match Store::current_root() {
        Ok(root) => root,
        Err(_) if dry_run => std::env::current_dir()?,
        Err(error) => return Err(error.into()),
    };
    let checkout = Checkout::at(&repo_root)?;
    let cs = CentralStore::open_at(resolved.clone());
    let planned = cs.plan(&checkout)?;
    let marker = central::read_marker(&repo_root)?;
    let local_issue_count = local_ledger_issue_count(&repo_root)?;

    if dry_run {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "dry_run": true,
                    "store_root": resolved.display().to_string(),
                    "identity": checkout.identity,
                    "planned_ledger": planned.dir.display().to_string(),
                    "already_central": marker.is_some(),
                    "local_issues_would_shadow": local_issue_count,
                    "would_create_trx_dir": !repo_root.join(".trx").exists(),
                })
            );
        } else {
            println!("dry run — nothing written:");
            println!("  store:         {}", resolved.display());
            println!("  identity:      {}", checkout.identity);
            println!("  ledger:        {}", planned.dir.display());
            println!("  central now:   {}", marker.is_some());
            println!("  local issues:  {local_issue_count}");
            if local_issue_count > 0 {
                println!("  (init refuses while a non-empty repo-local ledger exists)");
            }
        }
        return Ok(());
    }

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
    if local_issue_count > 0 {
        bail!(
            "The repo-local ledger has {local_issue_count} issues. Enable central mode only after migrating them (`trx migrate`, coming in epic trx-a1s8.5) — refusing to shadow them now."
        );
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
        println!(
            "  Cross-machine sync: trx store sync init --remote <URL>  (in the central store)"
        );
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
            println!("sync:     not initialized (trx store sync init --remote <URL>)");
        }
    }
    Ok(())
}

/// Non-empty line count of the checkout's repo-local issues.jsonl (0 when absent).
fn local_ledger_issue_count(repo_root: &std::path::Path) -> Result<usize> {
    let local_ledger = repo_root.join(".trx").join("issues.jsonl");
    if !local_ledger.is_file() {
        return Ok(0);
    }
    Ok(std::fs::read_to_string(&local_ledger)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count())
}

/// Manual sync: init (opt-in git), full (commit→pull→push) or a single action.
pub fn store_sync(
    action: Option<crate::StoreSyncAction>,
    store: Option<&str>,
    store_root: Option<&str>,
    json: bool,
) -> Result<()> {
    let config = GlobalConfig::load()?;
    let resolved = resolve_store_root(&config, store, store_root)?;
    if !matches!(action, Some(crate::StoreSyncAction::Init { .. }))
        && !resolved.join(".git").exists()
    {
        bail!(
            "Store {} is not a git repository. Run: trx store sync init --remote <URL>",
            resolved.display()
        );
    }
    let outcome = match action {
        None => sync::full_sync(&resolved, &config.sync)?,
        Some(crate::StoreSyncAction::Init { remote }) => {
            sync::init_remote(&resolved, &remote, &config.sync)?
        }
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
