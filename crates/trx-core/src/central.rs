//! Optional central per-user store for trx ledgers, modeled on mmry.
//!
//! Layout of a store root (default `$XDG_DATA_HOME/trx`):
//!
//! ```text
//! <store_root>/repos/<name>--<id>/issues.jsonl
//! <store_root>/repos/<name>--<id>/events.jsonl
//! <store_root>/repos/<name>--<id>/verifications.jsonl
//! <store_root>/repos/<name>--<id>/config.toml    # per-repo settings (prefix, ...)
//! <store_root>/repos/<name>--<id>/repo.json      # {identity, name}, written once
//! <store_root>/local/checkouts.json              # this machine's checkout paths (never synced)
//! ```
//!
//! A repository is identified by its git root commit (`git:<sha>`), so clones
//! and worktrees share one central ledger; repositories without git history
//! fall back to `path:<canonical root>`. Directory names are `<name>--<id>`
//! where `<id>` derives from the identity, so every machine computes the same
//! name for the same repository. If two machines registered the same
//! repository under different names, the duplicate directories are merged by
//! id on the next registration (append-only lines make this safe; issue
//! snapshots resolve by last-write-wins on load).
//!
//! A checkout selects central mode via a `.trx/central` marker file. Without
//! it, the repo-local `.trx/` ledger remains authoritative — exactly one
//! store per repository is ever read or written.

use crate::global_config::GlobalConfig;
use crate::store::{LOCK_FILE, StoreLock};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Name of the central-mode marker inside `.trx/`.
pub const CENTRAL_MARKER: &str = "central";
const REPOS_DIR: &str = "repos";
const LOCAL_DIR: &str = "local";
const CHECKOUTS_FILE: &str = "checkouts.json";
const REGISTRY_FILE: &str = "repo.json";
pub const ISSUES_FILE: &str = "issues.jsonl";
pub const EVENTS_FILE: &str = "events.jsonl";
pub const VERIFICATIONS_FILE: &str = "verifications.jsonl";
const CONFIG_FILE: &str = "config.toml";

/// A repository checkout on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// Canonical checkout root.
    pub root: PathBuf,
    /// Directory name of the checkout.
    pub name: String,
    /// `git:<root-commit>` for git repos with history, else `path:<root>`.
    pub identity: String,
}

impl Checkout {
    /// Treat `root` as a checkout: canonicalize, derive name and identity.
    pub fn at(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        let name = root
            .file_name()
            .map_or_else(|| "root".to_owned(), |n| n.to_string_lossy().into_owned());
        let identity = git_root_commit(&root).map_or_else(
            || format!("path:{}", root.display()),
            |sha| format!("git:{sha}"),
        );
        Ok(Self {
            root,
            name,
            identity,
        })
    }
}

/// Registry entry binding a central repo directory to a stable identity.
///
/// Immutable once written so it never conflicts when the store is synced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRecord {
    /// `git:<root-commit>` or `path:<canonical path>`.
    pub identity: String,
    /// Readable name (directory name of the first registered checkout).
    pub name: String,
}

/// A repository directory inside the central store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CentralRepo {
    pub dir: PathBuf,
    pub record: RepoRecord,
    /// Checkout paths known on this machine (machine-local, not synced).
    pub checkouts: Vec<PathBuf>,
}

impl CentralRepo {
    /// Directory name, unique within the store.
    pub fn dir_name(&self) -> String {
        self.dir.file_name().map_or_else(
            || "unknown".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    pub fn issues_path(&self) -> PathBuf {
        self.dir.join(ISSUES_FILE)
    }

    pub fn events_path(&self) -> PathBuf {
        self.dir.join(EVENTS_FILE)
    }

    pub fn verifications_path(&self) -> PathBuf {
        self.dir.join(VERIFICATIONS_FILE)
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join(CONFIG_FILE)
    }

    pub fn lock_path(&self) -> PathBuf {
        self.dir.join("issues.lock")
    }

    /// The store root this repo directory lives under.
    pub fn store_root(&self) -> Option<PathBuf> {
        self.dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }
}

/// Contents of a `.trx/central` marker.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CentralMarker {
    /// Named store to use; `None` selects the default store.
    pub store: Option<String>,
    /// Explicit store root recorded at `central init --store-root` time
    /// (machine-specific; named stores are the portable alternative).
    pub store_root: Option<String>,
}

/// Path of the central-mode marker for a checkout root.
pub fn marker_path(root: &Path) -> PathBuf {
    root.join(".trx").join(CENTRAL_MARKER)
}

/// Read the `.trx/central` marker, if present.
pub fn read_marker(root: &Path) -> Result<Option<CentralMarker>> {
    let binding = binding_path(root)?;
    if binding.is_file() {
        return Ok(Some(serde_json::from_str(&fs::read_to_string(binding)?)?));
    }
    let path = marker_path(root);
    if !path.is_file() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)?;
    let mut marker = CentralMarker::default();
    for line in content.lines() {
        for (key, target) in [
            ("store", &mut marker.store),
            ("store_root", &mut marker.store_root),
        ] {
            if let Some(value) = line.strip_prefix(key)
                && let Some(value) = value.trim().strip_prefix('=')
            {
                let value = value.trim().trim_matches('"');
                if !value.is_empty() {
                    *target = Some(value.to_string());
                }
            }
        }
    }
    // Older releases recorded both fields. A logical store name always wins
    // over a stale machine-specific path from another teammate's checkout.
    if marker.store.is_some() {
        marker.store_root = None;
    }
    Ok(Some(marker))
}

/// Write the `.trx/central` marker selecting central mode for a checkout.
pub fn write_marker(root: &Path, store: Option<&str>, store_root: Option<&Path>) -> Result<()> {
    let path = binding_path(root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let marker = CentralMarker {
        store: store.map(str::to_owned),
        store_root: store_root
            .filter(|_| store.is_none())
            .map(|p| p.to_string_lossy().into_owned()),
    };
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary, serde_json::to_string_pretty(&marker)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn binding_path(root: &Path) -> Result<PathBuf> {
    let checkout = Checkout::at(root)?;
    #[cfg(test)]
    let checkout = Checkout {
        identity: format!(
            "{}:{}",
            std::thread::current().name().unwrap_or("test"),
            checkout.identity
        ),
        ..checkout
    };
    Ok(crate::paths::config_base()?
        .join("trx/bindings")
        .join(format!("{}.json", short_id(&checkout.identity))))
}

/// Handle for one central store root.
#[derive(Debug, Clone)]
pub struct CentralStore {
    pub root: PathBuf,
}

impl CentralStore {
    /// Open the central store `store` selects (None = default), resolving the
    /// root from the flag argument, `TRX_STORE`/`TRX_STORE_ROOT` env or the
    /// global config.
    pub fn open(store: Option<&str>) -> Result<Self> {
        let config = GlobalConfig::load()?;
        Ok(Self {
            root: config.store_root(store)?,
        })
    }

    /// Open a store at an explicit root (no config resolution).
    pub fn open_at(root: PathBuf) -> Self {
        Self { root }
    }

    fn repos_dir(&self) -> PathBuf {
        self.root.join(REPOS_DIR)
    }

    /// Every registered repository directory (by `repo.json`).
    pub fn repos(&self) -> Result<Vec<CentralRepo>> {
        let mut repos = Vec::new();
        let dir = self.repos_dir();
        if !dir.is_dir() {
            return Ok(repos);
        }
        let mut entries: Vec<_> = fs::read_dir(&dir)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let registry = entry.path().join(REGISTRY_FILE);
            if !registry.is_file() {
                continue;
            }
            let record: RepoRecord = serde_json::from_str(&fs::read_to_string(&registry)?)
                .map_err(|error| {
                    Error::Other(format!("invalid registry {}: {error}", registry.display()))
                })?;
            repos.push(CentralRepo {
                dir: entry.path(),
                record,
                checkouts: Vec::new(),
            });
        }
        Ok(repos)
    }

    /// The central repo for `checkout`, if registered (the first directory
    /// when several share its identity).
    pub fn find(&self, checkout: &Checkout) -> Result<Option<CentralRepo>> {
        Ok(self
            .repos()?
            .into_iter()
            .find(|repo| repo.record.identity == checkout.identity))
    }

    /// Where `checkout` is (or would be) stored, without writing anything.
    pub fn plan(&self, checkout: &Checkout) -> Result<CentralRepo> {
        if let Some(repo) = self.find(checkout)? {
            return Ok(repo);
        }
        let name = format!(
            "{}--{}",
            sanitize(&checkout.name),
            short_id(&checkout.identity)
        );
        Ok(CentralRepo {
            dir: self.repos_dir().join(name),
            record: RepoRecord {
                identity: checkout.identity.clone(),
                name: checkout.name.clone(),
            },
            checkouts: Vec::new(),
        })
    }

    /// Resolve and register `checkout`: create its directory, remember the
    /// checkout path locally, and merge duplicate directories of the same
    /// identity (from other machines) into the first one.
    pub fn register(&self, checkout: &Checkout) -> Result<CentralRepo> {
        let mut repo = self.plan(checkout)?;
        let registry = repo.dir.join(REGISTRY_FILE);
        if !registry.is_file() {
            fs::create_dir_all(&repo.dir)?;
            write_atomic(&registry, &serde_json::to_string_pretty(&repo.record)?)?;
        }
        if !repo.checkouts.contains(&checkout.root) {
            repo.checkouts.push(checkout.root.clone());
        }
        self.remember_checkout(&checkout.identity, &checkout.root)?;
        self.merge_duplicates(&repo)?;
        Ok(repo)
    }

    /// Merge other directories with `primary`'s identity into it, then remove
    /// them. All trx files are append-only JSONL resolved by id on load, so
    /// concatenation is a correct merge. A duplicate that is currently locked
    /// by another writer is left in place and merged on a later registration —
    /// never merged or deleted under an active writer.
    fn merge_duplicates(&self, primary: &CentralRepo) -> Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        for other in self.repos()? {
            if other.record.identity != primary.record.identity || other.dir == primary.dir {
                continue;
            }
            let lock_path = other.dir.join(LOCK_FILE);
            let Some(_lock) = StoreLock::try_acquire(lock_path)? else {
                continue;
            };
            merge_ledger_files(&other, primary)?;
            fs::remove_dir_all(&other.dir)?;
            removed.push(other.dir);
        }
        Ok(removed)
    }

    fn checkouts_file(&self) -> PathBuf {
        self.root.join(LOCAL_DIR).join(CHECKOUTS_FILE)
    }

    fn local_checkouts(&self) -> Result<BTreeMap<String, Vec<PathBuf>>> {
        let path = self.checkouts_file();
        if !path.is_file() {
            return Ok(BTreeMap::new());
        }
        serde_json::from_str(&fs::read_to_string(&path)?)
            .map_err(|error| Error::Other(format!("invalid {}: {error}", path.display())))
    }

    fn remember_checkout(&self, identity: &str, root: &Path) -> Result<()> {
        let mut all = self.local_checkouts()?;
        let paths = all.entry(identity.to_owned()).or_default();
        if !paths.iter().any(|path| path == root) {
            paths.push(root.to_path_buf());
        }
        let path = self.checkouts_file();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&path, &serde_json::to_string_pretty(&all)?)
    }
}

/// Append every ledger line of `source` into `target` (all three files).
fn merge_ledger_files(source: &CentralRepo, target: &CentralRepo) -> Result<()> {
    for (from, to) in [
        (source.issues_path(), target.issues_path()),
        (source.events_path(), target.events_path()),
        (source.verifications_path(), target.verifications_path()),
    ] {
        if !from.is_file() {
            continue;
        }
        let content = fs::read_to_string(&from)?;
        use std::io::Write;
        let mut file = fs::OpenOptions::new().create(true).append(true).open(&to)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    Ok(())
}

/// Root commit of the git repository at `root` (worktrees resolve to their
/// common history). `None` without git, without commits, or on git errors.
fn git_root_commit(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .min()
        .map(str::to_owned)
}

/// Atomic small-file write via temp file + rename.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&tmp, content)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn sanitize(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_start_matches('.');
    if clean.is_empty() {
        "repo".to_owned()
    } else {
        clean.to_owned()
    }
}

/// Short, stable id suffix of every central repository directory.
fn short_id(identity: &str) -> String {
    if let Some(sha) = identity.strip_prefix("git:") {
        return sha.chars().take(12).collect();
    }
    // FNV-1a: stable across runs and platforms, unlike `DefaultHasher`.
    let hash = identity
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{hash:016x}").chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git should be available");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join("file.txt"), "hello\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "initial"]);
    }

    #[test]
    fn test_identity_uses_git_root_commit() {
        let temp = tempfile::tempdir().unwrap();
        init_git_repo(temp.path());
        let checkout = Checkout::at(temp.path()).unwrap();
        let sha = checkout.identity.strip_prefix("git:").unwrap();
        assert_eq!(sha.len(), 40);
    }

    #[test]
    fn test_worktree_shares_identity_with_main_checkout() {
        let main = tempfile::tempdir().unwrap();
        init_git_repo(main.path());
        let worktree_parent = tempfile::tempdir().unwrap();
        let worktree = worktree_parent.path().join("wt");
        git(
            main.path(),
            &[
                "worktree",
                "add",
                "-q",
                worktree.to_str().unwrap(),
                "-b",
                "feature",
            ],
        );

        let a = Checkout::at(main.path()).unwrap();
        let b = Checkout::at(&worktree).unwrap();
        assert_eq!(a.identity, b.identity);
        assert_ne!(a.root, b.root);
    }

    #[test]
    fn test_identity_falls_back_to_path_without_git() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path()).unwrap();
        let checkout = Checkout::at(temp.path()).unwrap();
        assert!(checkout.identity.starts_with("path:"));
    }

    #[test]
    fn test_empty_git_repo_falls_back_to_path_identity() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path()).unwrap();
        git(temp.path(), &["init", "-q"]);
        let checkout = Checkout::at(temp.path()).unwrap();
        assert!(checkout.identity.starts_with("path:"));
    }

    #[test]
    fn test_plan_dir_name_is_stable_and_sanitized() {
        let temp = tempfile::tempdir().unwrap();
        init_git_repo(temp.path());
        let checkout = Checkout::at(temp.path()).unwrap();
        let store = CentralStore::open_at(temp.path().join("store"));
        let planned = store.plan(&checkout).unwrap();
        let sha: String = checkout.identity.chars().skip(4).take(12).collect();
        let sanitized_name: String = temp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches('.')
            .to_string();
        assert_eq!(planned.dir_name(), format!("{sanitized_name}--{sha}"));

        // Odd checkout names are sanitized for the directory name; the
        // identity keeps the canonical path untouched.
        let store2 = CentralStore::open_at(temp.path().join("store2"));
        let weird_root = temp.path().join("my repo!");
        fs::create_dir_all(&weird_root).unwrap();
        let identity = format!("path:{}", weird_root.display());
        let planned2 = store2
            .plan(&Checkout {
                root: weird_root.clone(),
                name: "my repo!".into(),
                identity: identity.clone(),
            })
            .unwrap();
        assert!(planned2.dir_name().starts_with("my_repo_--"));
        assert_eq!(planned2.record.identity, identity);
    }

    #[test]
    fn test_register_is_idempotent_and_records_checkouts() {
        let temp = tempfile::tempdir().unwrap();
        init_git_repo(temp.path());
        let store = CentralStore::open_at(temp.path().join("store"));
        let checkout = Checkout::at(temp.path()).unwrap();

        let first = store.register(&checkout).unwrap();
        let second = store.register(&checkout).unwrap();
        assert_eq!(first.dir, second.dir);
        assert_eq!(store.repos().unwrap().len(), 1);

        let checkouts: BTreeMap<String, Vec<PathBuf>> = serde_json::from_str(
            &fs::read_to_string(store.root.join(LOCAL_DIR).join(CHECKOUTS_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            checkouts.get(&checkout.identity).unwrap(),
            &vec![checkout.root.clone()]
        );
    }

    #[test]
    fn test_busy_duplicate_dir_is_skipped_not_merged_or_deleted() {
        let source = tempfile::tempdir().unwrap();
        init_git_repo(source.path());
        let clone_parent = tempfile::tempdir().unwrap();
        git(
            clone_parent.path(),
            &[
                "clone",
                "-q",
                source.path().join(".").to_str().unwrap(),
                "alpha",
            ],
        );
        let store = CentralStore::open_at(source.path().join("central-store"));
        let checkout = Checkout::at(&clone_parent.path().join("alpha")).unwrap();
        let repo = store.register(&checkout).unwrap();

        // A second, differently-named dir with the same identity, currently
        // locked by another writer.
        let dup = store.root.join("repos").join("renamed--zzzz");
        fs::create_dir_all(&dup).unwrap();
        fs::write(
            dup.join(REGISTRY_FILE),
            serde_json::to_string_pretty(&RepoRecord {
                identity: checkout.identity.clone(),
                name: "renamed".into(),
            })
            .unwrap(),
        )
        .unwrap();
        fs::write(dup.join(ISSUES_FILE), "{\"id\":\"x-1\"}\n").unwrap();
        let lock = StoreLock::try_acquire(dup.join(LOCK_FILE))
            .unwrap()
            .unwrap();

        let again = store.register(&checkout).unwrap();
        assert_eq!(again.dir, repo.dir, "primary registration unaffected");
        assert!(dup.is_dir(), "locked duplicate must be left in place");
        assert!(
            !repo.issues_path().exists(),
            "nothing merged under an active writer"
        );
        drop(lock);

        // Once the writer is gone, the next registration merges and removes it.
        store.register(&checkout).unwrap();
        assert!(!dup.exists(), "unlocked duplicate merged and removed");
        let content = fs::read_to_string(repo.issues_path()).unwrap();
        assert!(content.contains("\"x-1\""));
    }

    #[test]
    fn test_two_checkouts_of_one_repo_merge_into_first_dir() {
        let source = tempfile::tempdir().unwrap();
        init_git_repo(source.path());
        // Two clones of the same repository => same root commit => same identity.
        let clone1_parent = tempfile::tempdir().unwrap();
        let clone2_parent = tempfile::tempdir().unwrap();
        git(
            clone1_parent.path(),
            &[
                "clone",
                "-q",
                source.path().join(".").to_str().unwrap(),
                "alpha",
            ],
        );
        git(
            clone2_parent.path(),
            &[
                "clone",
                "-q",
                source.path().join(".").to_str().unwrap(),
                "beta",
            ],
        );

        let store = CentralStore::open_at(source.path().join("central-store"));
        let a = Checkout::at(&clone1_parent.path().join("alpha")).unwrap();
        let b = Checkout::at(&clone2_parent.path().join("beta")).unwrap();
        assert_eq!(a.identity, b.identity);

        let repo_a = store.register(&a).unwrap();
        // Different dir name (different checkout name), same identity.
        let repo_b = store.register(&b).unwrap();
        assert_eq!(
            repo_a.dir, repo_b.dir,
            "second registration must merge into the first dir"
        );
        assert_eq!(store.repos().unwrap().len(), 1);
    }

    #[test]
    fn test_distinct_repos_get_distinct_dirs_even_with_same_name() {
        let temp = tempfile::tempdir().unwrap();
        let store_dir = temp.path().join("store");
        let repo1 = temp.path().join("same-name-1");
        let repo2 = temp.path().join("same-name-2");
        for repo in [&repo1, &repo2] {
            fs::create_dir_all(repo).unwrap();
            fs::write(repo.join("f"), "x\n").unwrap();
        }
        git(&repo1, &["init", "-q", "-b", "main"]);
        git(&repo2, &["init", "-q", "-b", "main"]);
        for repo in [&repo1, &repo2] {
            fs::write(repo.join("file.txt"), "hello\n").unwrap();
            git(repo, &["add", "."]);
            git(repo, &["commit", "-q", "-m", "initial"]);
        }
        // Give the repos DIFFERENT root commits by amending one.
        fs::write(repo2.join("file.txt"), "changed\n").unwrap();
        git(&repo2, &["commit", "-q", "--amend", "-m", "different root"]);

        let store = CentralStore::open_at(store_dir);
        let a = store.register(&Checkout::at(&repo1).unwrap()).unwrap();
        let b = store.register(&Checkout::at(&repo2).unwrap()).unwrap();
        assert_ne!(a.record.identity, b.record.identity);
        assert_ne!(a.dir, b.dir);
        assert_eq!(store.repos().unwrap().len(), 2);
    }

    #[test]
    fn test_marker_round_trip_with_and_without_store() {
        let temp = tempfile::tempdir().unwrap();
        assert!(read_marker(temp.path()).unwrap().is_none());
        write_marker(temp.path(), None, None).unwrap();
        assert_eq!(
            read_marker(temp.path()).unwrap(),
            Some(CentralMarker {
                store: None,
                store_root: None
            })
        );
        write_marker(temp.path(), Some("work"), Some(Path::new("/data/trx-work"))).unwrap();
        assert!(!temp.path().join(".trx").exists());
        fs::remove_file(binding_path(temp.path()).unwrap()).unwrap();
        fs::create_dir_all(temp.path().join(".trx")).unwrap();
        // Legacy markers from a teammate must not pin this machine's root.
        fs::write(
            marker_path(temp.path()),
            "store = \"work\"\nstore_root = \"/Users/other/trx\"\n",
        )
        .unwrap();
        assert_eq!(
            read_marker(temp.path()).unwrap(),
            Some(CentralMarker {
                store: Some("work".into()),
                store_root: None,
            })
        );
    }

    #[test]
    fn test_short_id_is_stable_for_path_identities() {
        assert_eq!(short_id("path:/a/b"), short_id("path:/a/b"));
        assert_ne!(short_id("path:/a/b"), short_id("path:/a/c"));
        assert_eq!(short_id("git:abcdef1234567890"), "abcdef123456");
    }
}
