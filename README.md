# trx - Minimal Git-Backed Issue Tracker

## Overview

trx is a minimal, git-native issue tracker focused on simplicity and low overhead.

**Philosophy**: Pure data layer. No daemon. No SQLite. Just JSONL files in git.

## Design Goals

- Minimal footprint with ~20 fields per issue
- Git-native: all data tracked in version control
- Conflict-free merging with CRDT support (automerge)
- Easy to understand and extend

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/cloonix/trx/master/install.sh | bash
```

Or build from source with `cargo install --path crates/trx-cli`.

## Architecture

```
trx/
├── crates/
│   ├── trx-core/     # Core library: Issue model, Store, Graph
│   ├── trx-cli/      # CLI binary: trx command
│   └── trx-tui/      # TUI binary: trx-tui viewer
└── .trx/             # Per-repo issue storage
    ├── config.toml   # Repo configuration
    ├── ISSUES.md     # Human-readable issue summary (auto-generated)
    ├── issues.jsonl  # V1: All issues, one per line
    └── crdt/         # V2: One .automerge file per issue
```

## Storage Versions

trx supports two storage backends:

**V1 (JSONL)**: Simple, human-readable format. All issues stored in a single `issues.jsonl` file. Good for small projects or when human readability is prioritized.

**V2 (CRDT)**: Each issue stored as a separate `.automerge` file. Enables conflict-free merging when multiple users edit issues concurrently. Recommended for teams.

### Why CRDT?

With JSONL, concurrent edits to different issues can still cause merge conflicts because they modify the same file. CRDT storage solves this:

- Each issue is an independent automerge document
- Concurrent edits to the same issue merge automatically
- No manual conflict resolution needed
- Git treats `.automerge` files as binary, automerge handles the merge semantics

### Migration

```bash
# Check current storage version
trx migrate --status

# Upgrade v1 -> v2 (CRDT)
trx migrate

# Rollback v2 -> v1 (preserves crdt/ directory for safety)
trx migrate --rollback
```

## Data Model

```rust
struct Issue {
    id: String,           // trx-xxxx (hash-based, conflict-free)
    title: String,
    description: Option<String>,
    status: Status,       // open, in_progress, blocked, closed, tombstone
    priority: u8,         // 0-4
    issue_type: IssueType, // bug, feature, task, epic, chore
    labels: Vec<String>,
    created_at: DateTime,
    updated_at: DateTime,
    closed_at: Option<DateTime>,
    deleted_at: Option<DateTime>,
    dependencies: Vec<Dependency>,
    // ... a few more
}
```

## CLI Commands

```bash
trx init [--prefix PREFIX]     # Initialize .trx/ directory
trx create TITLE [-t TYPE] [-p PRIORITY] [-d DESC] [--parent ID]
trx list [--status S] [--type T] [--all]
trx show ID
trx update ID [--status S] [--priority P] [--title T]
trx close ID [-r REASON]
trx ready                      # Show unblocked work
trx dep add ID --blocks OTHER
trx dep rm ID --blocks OTHER
trx sync [-m MESSAGE]          # Git add + commit .trx/

# Migration
trx import .beads/issues.jsonl [--prefix PREFIX]
trx purge-beads [--force]
```

## TUI Viewer

```bash
trx-tui                        # Interactive TUI
trx-tui robot triage           # JSON: prioritized issues
trx-tui robot next             # JSON: next recommended issue
trx-tui robot insights         # JSON: graph analytics
trx-tui --workspace config.yaml # Multi-repo view
```

## beads Compatibility

trx supports importing from beads and uses a compatible JSONL format:

- Compatible field names: `id`, `title`, `status`, `priority`, `issue_type`
- Compatible dependency format: `[{issue_id, depends_on_id, type}]`
- Compatible workspace.yaml format for multi-repo

## Conflict Resolution

With V2 (CRDT) storage, conflicts are resolved automatically during `trx sync`:

```bash
# After a git pull with conflicts
trx sync  # Automatically merges any conflicting .automerge files
```

If git creates conflict marker files (`.BASE`, `.LOCAL`, `.REMOTE`), trx detects and merges them using automerge's built-in conflict resolution.

## Development

```bash
# Build all crates
cargo build

# Run CLI
cargo run -p trx-cli -- list

# Run TUI
cargo run -p trx-tui

# Run tests
cargo test
```

## Migration from beads

```bash
# 1. Initialize trx in repo
trx init --prefix myproject

# 2. Import beads issues
trx import .beads/issues.jsonl

# 3. Verify import
trx list --all

# 4. Remove beads (optional)
trx purge-beads

# 5. Commit
git add .trx/ && git commit -m "Add trx issue tracking"
```
