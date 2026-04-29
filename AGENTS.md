# trx - Minimal Git-Backed Issue Tracker

## Overview

trx is a minimal, git-native issue tracker focused on simplicity and low overhead.

**Philosophy**: Pure data layer. No daemon. No SQLite. Just JSONL files in git.

## Design Goals

- Minimal footprint with ~20 fields per issue
- Git-native: all data stored as JSONL, tracked in version control
- Simple merge semantics
- Easy to understand and extend

## Architecture

```
trx/
├── crates/
│   ├── trx-core/     # Core library: Issue model, Store, Graph
│   ├── trx-cli/      # CLI binary: trx command
│   └── trx-tui/      # TUI binary: trx-tui viewer
└── .trx/             # Per-repo issue storage
    ├── issues.jsonl  # All issues, one per line (git-tracked)
    └── config.toml   # Repo configuration
```

## Build/Test Commands

```bash
# Build
cargo build                    # Build all crates
cargo build -p trx-core        # Build specific crate
cargo build --release          # Release build

# Run
cargo run -p trx-cli -- list   # Run CLI with args
cargo run -p trx-tui           # Run TUI

# Test
cargo test                     # Run all tests
cargo test -p trx-core         # Test specific crate
cargo test test_ready_issues   # Run single test by name
cargo test -p trx-core -- --nocapture  # Show println output

# Lint and Format
cargo fmt                      # Format all code
cargo fmt -- --check           # Check formatting (CI)
cargo clippy                   # Run lints
cargo clippy --fix             # Auto-fix lints
cargo clippy -- -D warnings    # Fail on warnings (CI)

# Check
cargo check                    # Fast type checking (run after changes)
```

## Code Style Guidelines

### Rust Edition and Tooling

- Rust 2024 edition
- Uses workspace dependency management in root Cargo.toml
- No custom rustfmt.toml or clippy.toml - uses defaults

### Import Organization

Order imports: external crates, workspace crates, crate-local, std. Group items with braces:

```rust
use anyhow::{bail, Result};
use trx_core::{generate_id, DependencyType, Issue, Status, Store};
use crate::{Error, Result};
use std::path::PathBuf;
```

### Error Handling

**Library crate (trx-core)**: Use `thiserror` with custom Error enum:

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Issue not found: {0}")]
    NotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
```

**Binary crates (trx-cli, trx-tui)**: Use `anyhow` for flexible error handling:

```rust
use anyhow::{bail, Result};

fn show(id: &str) -> Result<()> {
    let issue = store.get(id)
        .ok_or_else(|| anyhow::anyhow!("Issue not found: {}", id))?;
    Ok(())
}
```

### Naming Conventions

| Element     | Style              | Example                          |
|-------------|--------------------|----------------------------------|
| Types       | PascalCase         | `Issue`, `IssueGraph`, `Status`  |
| Functions   | snake_case         | `generate_id`, `list_open`       |
| Variables   | snake_case         | `issue_type`, `created_at`       |
| Constants   | SCREAMING_SNAKE    | `TRX_DIR`, `ISSUES_FILE`         |
| Modules     | snake_case         | `issue`, `store`, `graph`        |

### Serde Patterns

Use snake_case serialization for enums and skip empty values:

```rust
#[serde(rename_all = "snake_case")]
pub enum Status { Open, InProgress, Blocked, Closed }

#[serde(skip_serializing_if = "Option::is_none")]
pub description: Option<String>,

#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub labels: Vec<String>,
```

### Testing

Write inline tests in `#[cfg(test)]` modules:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_id() {
        let id = generate_id("trx");
        assert!(id.starts_with("trx-"));
        assert_eq!(id.len(), 8);
    }
}
```

- Use descriptive `test_` prefixed names
- No separate test files - inline with source
- Use `.into()` for string conversion in tests

### CLI Structure (clap)

Use derive macros with subcommands:

```rust
#[derive(Parser)]
#[command(name = "trx", about = "Minimal git-backed issue tracker")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(long, global = true)]
    json: bool,
}
```

## Issue Tracking (trx)

**IMPORTANT**: `trx` is installed globally at `/home/wismut/.local/bin/trx`. Always use `trx` directly — never `cargo run -p trx-cli --`.

```bash
trx ready              # Show unblocked issues
trx create "Title" -t task -p 2   # Create issue (types: bug/feature/task/epic/chore, priority: 0-4)
trx update <id> --status in_progress
trx close <id> -r "Done"
trx sync               # Commit .trx/ changes
```

Priorities: 0=critical, 1=high, 2=medium, 3=low, 4=backlog

## Changelog

Keep `CHANGELOG.md` up to date. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) with `Added`,
`Changed`, `Fixed`, `Removed`, `Deprecated`, `Security` sections.

- For any user-visible change (CLI, TUI, public API in `trx-core`, file
  formats, behavior), add a one-line entry under `## Unreleased` in the
  same commit as the change.
- When bumping the workspace version in `Cargo.toml`, rename the
  `Unreleased` heading to `[<version>] - <YYYY-MM-DD>` and start a fresh
  empty `Unreleased` section above it. Do this in the same commit as the
  version bump so the release tag points at a complete CHANGELOG.
- Skip the CHANGELOG only for purely internal changes with no user-visible
  effect (refactors, test-only edits, doc tweaks).

