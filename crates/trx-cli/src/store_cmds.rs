//! CLI commands for the optional central store and its git sync.
//!
//! - `trx central init` — opt the current repository into central mode
//! - `trx central status` — mode, identity, ledger location, sync summary
//! - `trx store sync init --remote URL` — make the store a synced git repository
//! - `trx store sync [status|pull|push]` — manual sync (commit→pull→push)

use anyhow::{Result, bail};
use colored::Colorize;
use std::fs;
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

/// Resolve the store root for `store`-level operations, checkout-aware when
/// no explicit selection is made: standing in a central-mode checkout (e.g. a
/// project bound to its own named store) targets THAT store instead of the
/// default one.
///
/// Precedence: --store-root/--store flags > TRX_STORE_ROOT/TRX_STORE env >
/// the checkout's `.trx/central` marker > global config default.
fn resolve_store_root_checkout_aware(
    config: &GlobalConfig,
    store: Option<&str>,
    store_root: Option<&str>,
) -> Result<std::path::PathBuf> {
    if store_root.is_some() || store.is_some() {
        return Ok(config.resolve_store_root(store, store_root)?);
    }
    if std::env::var_os("TRX_STORE_ROOT").is_some() || std::env::var_os("TRX_STORE").is_some() {
        return Ok(config.store_root(None)?);
    }
    if let Ok(root) = Store::current_root()
        && let Some(marker) = central::read_marker(&root)?
    {
        if let Some(recorded) = &marker.store_root {
            return Ok(trx_core::paths::expand_tilde(recorded)?);
        }
        return Ok(config.store_root(marker.store.as_deref())?);
    }
    Ok(config.store_root(None)?)
}

/// Opt the current repository into central mode.
pub fn central_init(
    dry_run: bool,
    store: Option<&str>,
    store_root: Option<&str>,
    json: bool,
) -> Result<()> {
    central_init_impl(dry_run, store, store_root, json, None)
}

pub(crate) fn central_init_impl(
    dry_run: bool,
    store: Option<&str>,
    store_root: Option<&str>,
    json: bool,
    prefix: Option<&str>,
) -> Result<()> {
    let config = GlobalConfig::load()?;
    let resolved = resolve_store_root(&config, store, store_root)?;

    // Dry run: plan only — no .trx/ creation, no registration, no marker.
    // A missing .trx is part of the plan ("would create") instead of an error.
    let repo_root = match Store::current_root() {
        Ok(root) => root,
        Err(_) if dry_run => std::env::current_dir()?,
        Err(_) if !std::path::Path::new(".trx").exists() => {
            // Fresh clones/worktrees have no .trx yet: create the minimal
            // checkout state so central mode works with a single command.
            let trx_dir = std::path::Path::new(".trx");
            std::fs::create_dir_all(trx_dir)?;
            let prefix = prefix.unwrap_or("trx");
            if !trx_dir.join("config.toml").exists() {
                std::fs::write(
                    trx_dir.join("config.toml"),
                    format!("# trx configuration\nprefix = \"{prefix}\"\n"),
                )?;
            }
            Store::current_root()?
        }
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
            let prefix = prefix.unwrap_or("trx");
            std::fs::write(
                trx_dir.join("config.toml"),
                format!("# trx configuration\nprefix = \"{prefix}\"\n"),
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
    let resolved = resolve_store_root_checkout_aware(&config, store, store_root)?;
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

/// Migrate the current repo's `.trx` ledger into the central store, or with
/// `--all`, every repo-local ledger found under the configured scan roots.
pub fn migrate(
    all: bool,
    dry_run: bool,
    untrack: bool,
    scan: &[String],
    exclude: &[String],
    json: bool,
) -> Result<()> {
    let config = GlobalConfig::load()?;
    let resolved = resolve_store_root(&config, None, None)?;
    let opts = trx_core::migrate::MigrateOptions { dry_run, untrack };

    let targets: Vec<std::path::PathBuf> = if all {
        let mut roots: Vec<std::path::PathBuf> =
            scan.iter().map(std::path::PathBuf::from).collect();
        if roots.is_empty() {
            roots = config
                .roots
                .iter()
                .map(|root| std::path::PathBuf::from(&root.path))
                .collect();
        }
        if roots.is_empty() {
            bail!(
                "--all needs scan roots: pass --scan PATH or configure [[roots]] in {}",
                GlobalConfig::path()?.display()
            );
        }
        filter_excluded(trx_core::migrate::scan_for_ledgers(&roots, 8)?, exclude)
    } else {
        vec![Store::current_root()?]
    };
    if targets.is_empty() {
        if json {
            println!("{}", serde_json::json!({ "migrated": [] }));
        } else {
            println!("No repo-local ledgers found.");
        }
        return Ok(());
    }

    let mut reports = Vec::new();
    let mut failures = Vec::new();
    for target in &targets {
        match trx_core::migrate::migrate_repo(target, &resolved, None, opts) {
            Ok(report) => reports.push(report),
            Err(error) => failures.push(format!("{}: {error}", target.display())),
        }
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "dry_run": dry_run,
                "migrated": reports,
                "failures": failures,
            })
        );
    } else {
        for report in &reports {
            if report.already_central {
                println!(
                    "{} {} already central — nothing to do",
                    "=".dimmed(),
                    report.repo_root
                );
                continue;
            }
            if dry_run {
                println!("{} {} (dry run)", "⊘".yellow(), report.repo_root);
            } else {
                println!("{} {}", "✓".green(), report.repo_root);
            }
            println!(
                "    {} issues ({} snapshots, {} deps), {} events, {} verifications → {}",
                report.issues,
                report.issue_snapshots,
                report.dependencies,
                report.events,
                report.verifications,
                report.ledger
            );
            for backup in &report.backups {
                println!("    backup: {backup}");
            }
            for file in &report.untracked {
                println!("    untracked: {file}");
            }
        }
        for failure in &failures {
            println!("{} {failure}", "✗".red());
        }
        if !dry_run && !reports.is_empty() {
            println!("Central mode is now active in migrated checkouts (`.trx/central`).");
        }
    }
    if !failures.is_empty() {
        bail!("{} migration(s) failed", failures.len());
    }
    Ok(())
}

/// Discover repo-local ledgers under scan roots and show/migrate them all;
/// sets `migrate = "auto"` in the global config on a real run (mmry setup parity).
pub fn setup(
    dry_run: bool,
    scans: &[String],
    depth: u32,
    yes: bool,
    exclude: &[String],
    json: bool,
) -> Result<()> {
    let config = GlobalConfig::load()?;
    let mut roots: Vec<std::path::PathBuf> = scans.iter().map(std::path::PathBuf::from).collect();
    if roots.is_empty() {
        roots = config
            .roots
            .iter()
            .map(|root| std::path::PathBuf::from(&root.path))
            .collect();
    }
    if roots.is_empty() {
        bail!(
            "Nothing to scan: pass --scan PATH or configure [[roots]] in {}",
            GlobalConfig::path()?.display()
        );
    }
    let targets = trx_core::migrate::scan_for_ledgers(&roots, depth)?;
    let targets = filter_excluded(targets, exclude);

    if dry_run || targets.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::json!({ "dry_run": dry_run, "found": targets.iter().map(|p| p.display().to_string()).collect::<Vec<_>>() })
            );
        } else if targets.is_empty() {
            println!("No repo-local ledgers found under the configured roots.");
        } else {
            println!("Found {} repo-local ledger(s):", targets.len());
            for target in &targets {
                println!("  {}", target.display());
            }
            if dry_run {
                println!("dry run — nothing written. Run `trx setup` to migrate them all.");
            }
        }
        return Ok(());
    }

    if !yes && !is_interactive() {
        bail!(
            "Refusing to migrate {} repo-local ledger(s) non-interactively without --yes.",
            targets.len()
        );
    }
    if !yes {
        println!("Found {} repo-local ledger(s):", targets.len());
        for target in &targets {
            println!("  {}", target.display());
        }
        print!("Migrate them all into the central store? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("Aborted");
            return Ok(());
        }
    }

    // Real run: migrate everything, then flip the policy to auto.
    migrate(true, false, false, scans, exclude, json)?;
    set_migrate_auto()?;
    if !json {
        println!(
            "{} migrate = \"auto\" set in {}",
            "✓".green(),
            GlobalConfig::path()?.display()
        );
    }
    Ok(())
}

/// One-command machine bootstrap for the central store:
///
/// 1. point the default store at `--remote` (creating/merging it — safe on a
///    fresh machine: an empty local store merges the remote down)
/// 2. opt into `migrate = "auto"` (and optionally `default_mode = "central"`)
/// 3. with `--scan`: bulk-migrate this machine's repo-local ledgers
///
/// Idempotent: re-running is a no-op for everything already done.
pub fn onboard(
    remote: &str,
    scans: &[String],
    depth: u32,
    exclude: &[String],
    default_central: bool,
    yes: bool,
    json: bool,
) -> Result<()> {
    let config = GlobalConfig::load()?;
    let store_root = config.store_root(None)?;

    // 1. Store bootstrap / remote reconciliation (union-merge, never resets).
    let sync_outcome = if store_root.join(".git").exists() {
        sync::full_sync(&store_root, &config.sync)?
    } else {
        sync::init_remote(&store_root, remote, &config.sync)?
    };

    // 2. Global policy.
    set_migrate_auto()?;
    if default_central {
        set_config_key("default_mode", "central")?;
    }

    // 3. Bulk migration of this machine's ledgers (skips already-central).
    let migration_summary = if scans.is_empty() {
        None
    } else {
        if !yes && !is_interactive() {
            bail!(
                "Refusing to migrate repo-local ledgers non-interactively without --yes. Everything else is done; re-run with --yes to migrate."
            );
        }
        let roots: Vec<std::path::PathBuf> = scans.iter().map(std::path::PathBuf::from).collect();
        let targets = filter_excluded(trx_core::migrate::scan_for_ledgers(&roots, depth)?, exclude);
        let mut migrated = 0usize;
        let mut failed = 0usize;
        for target in &targets {
            match trx_core::migrate::migrate_repo(
                target,
                &store_root,
                None,
                trx_core::migrate::MigrateOptions::default(),
            ) {
                Ok(report) => {
                    if !report.already_central {
                        migrated += 1;
                    }
                }
                Err(error) => {
                    failed += 1;
                    if json {
                        eprintln!("trx: warning: {}: {error}", target.display());
                    } else {
                        println!("{} {}: {error}", "✗".red(), target.display());
                    }
                }
            }
        }
        Some((targets.len(), migrated, failed))
    };

    if json {
        println!(
            "{}",
            serde_json::json!({
                "store_root": store_root.display().to_string(),
                "remote": remote,
                "pushed": sync_outcome.pushed,
                "pending_commits": sync_outcome.pending_commits,
                "migrate_policy": "auto",
                "default_mode": if default_central { "central" } else { "repo-local" },
                "found": migration_summary.as_ref().map(|(found, _, _)| *found),
                "migrated": migration_summary.as_ref().map(|(_, migrated, _)| *migrated),
                "failed": migration_summary.as_ref().map(|(_, _, failed)| *failed),
            })
        );
        return Ok(());
    }

    println!(
        "{} Store: {} (remote {})",
        "✓".green(),
        store_root.display(),
        remote
    );
    if sync_outcome.pushed {
        println!("  pushed to remote");
    } else if sync_outcome.pending_commits > 0 {
        println!(
            "  {} pending commits — next online sync drains them",
            "!".yellow()
        );
    }
    println!("  migrate policy: auto");
    if default_central {
        println!("  default mode:   central (new checkouts without .trx go central automatically)");
    }
    if let Some((found, migrated, failed)) = migration_summary {
        println!(
            "{} migrated {} of {} repo-local ledger(s){}",
            "✓".green(),
            migrated,
            found,
            if failed > 0 {
                format!(", {failed} failed (see above)")
            } else {
                String::new()
            }
        );
    }
    Ok(())
}

fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Drop scan targets under any `--exclude PATH` (repeatable; matched as a
/// canonicalized path prefix, so excluding `~/byteowlz/trx` keeps the
/// team-shared tracker repo-local).
fn filter_excluded(
    targets: Vec<std::path::PathBuf>,
    exclude: &[String],
) -> Vec<std::path::PathBuf> {
    if exclude.is_empty() {
        return targets;
    }
    let excluded: Vec<std::path::PathBuf> = exclude
        .iter()
        .filter_map(|path| {
            trx_core::paths::expand_tilde(path)
                .ok()
                .map(|expanded| fs::canonicalize(&expanded).unwrap_or(expanded))
        })
        .collect();
    targets
        .into_iter()
        .filter(|target| {
            let canonical = fs::canonicalize(target).unwrap_or_else(|_| target.clone());
            !excluded
                .iter()
                .any(|excluded| canonical.starts_with(excluded))
        })
        .collect()
}

/// Set `migrate = "auto"` in the global config, preserving comments:
/// replace an existing `migrate = ...` line in place, else append.
fn set_migrate_auto() -> Result<()> {
    set_config_key("migrate", "auto")
}

/// Set a top-level TOML key in the global config, preserving comments:
/// replace an existing `key = ...` line in place, else append.
fn set_config_key(key: &str, value: &str) -> Result<()> {
    let path = GlobalConfig::path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut lines: Vec<String> = if path.is_file() {
        std::fs::read_to_string(&path)?
            .lines()
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };
    let mut replaced = false;
    for line in &mut lines {
        if line.trim_start().starts_with(key) {
            *line = format!("{key} = \"{value}\"");
            replaced = true;
        }
    }
    if !replaced {
        // Top-level keys must come before the first [table] header, else TOML
        // silently scopes them into that table.
        let first_table = lines
            .iter()
            .position(|line| line.trim_start().starts_with('['))
            .unwrap_or(lines.len());
        lines.insert(first_table, format!("{key} = \"{value}\""));
    }
    let mut content = lines.join("\n");
    if !content.ends_with('\n') {
        content.push('\n');
    }
    std::fs::write(&path, content)?;
    Ok(())
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
