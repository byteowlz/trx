# Issues

## Open

### [trx-p7y6] trx ready should show all unblocked issues, not just epics - leaf feature/task issues with no blockers should appear (P1, bug)

### [trx-f4ck] trx list --epic <id> to show all children/descendants of an epic (P2, feature)

### [trx-tkmz] trx list --json should include resolved parent, blocked_by, blocks fields - currently these are null even when dependencies exist (P2, bug)

### [trx-pa3y] Custom ID prefix on create - 'trx create --id mmry-lrn' to control the generated issue ID prefix instead of random (P2, feature)

### [trx-nst7] Add list filters (status/type/priority/search) (P2, feature)
Improve trx list ergonomics with flags like --status open,closed --type bug,task --priority 0..4 and --search text. Support combined filters.

### [trx-jtmw] Show reverse dependencies in trx show (P2, feature)
Add optional output (e.g., --reverse or default section) that lists issues blocked by this one and parent/child relationships, so epics can show their children.

### [trx-hyv9] Implement dependency tree rendering (P2, feature)
trx dep tree currently prints '(not yet implemented)'. Add traversal to display blockers/children with type labels and detect cycles.

### [trx-s0t.4] TUI: Multi-repo workspace support (P2, task)
Load workspace.yaml with multiple repos. Aggregate view across repos. Support beads-viewer workspace format for compatibility.

### [trx-s0t.3] TUI: Dependency graph visualization (P2, task)
ASCII/Unicode graph view showing dependency relationships. Highlight cycles, critical path, blocked chains.

### [trx-cgs7] trx update should accept --description from stdin when value is '-', for adding long descriptions to existing issues (P3, feature)

### [trx-te7r] trx handover - generate compact one-line summary of all issues with dependency order for agent handoff (P3, feature)

### [trx-bqnp] Add trx sync options for message and dry-run (P3, feature)
Support --message to set commit message and --dry-run or --no-commit to inspect staged changes without committing.

### [trx-mb7y] Expose blockers in trx ready output (P3, feature)
When an issue is blocked, show which dependencies are open to explain why it isn't ready.

### [trx-gt71] Add --clear for update fields (P3, feature)
Allow trx update to clear optional fields (description, parent, labels) explicitly, e.g., --clear description.

### [trx-vtsw] Support description input from file or stdin (P3, feature)
For long issue text, allow trx create/update to read description from --description-file or --description - (stdin).

## Closed

- [trx-twga] Auto-resolve ISSUES.md merge conflicts (closed 2026-02-19)
- [trx-gexn] print status (open/closed/etc) when running trx list (closed 2026-01-05)
- [trx-7eq.3] Migration docs and byt integration (closed 2026-01-05)
- [trx-7eq] Beads migration: Import and purge (closed 2026-01-05)
- [trx-7eq.2] Purge: trx purge-beads command (closed 2026-01-05)
- [trx-7eq.1] Import: trx import command for beads JSONL (closed 2026-01-05)
- [trx-s0t] trx-tui: Terminal UI viewer (closed 2026-01-05)
- [trx-s0t.5] TUI: Robot mode JSON output for automation (closed 2026-01-05)
- [trx-s0t.2] TUI: Issue detail panel (closed 2026-01-05)
- [trx-s0t.1] TUI: Issue list view with filtering and sorting (closed 2026-01-05)
- [trx-ned] trx-cli: Command-line interface (closed 2026-01-05)
- [trx-ned.9] CLI: sync command - git add/commit .trx (closed 2026-01-05)
- [trx-ned.8] CLI: dep command - manage dependencies (closed 2026-01-05)
- [trx-ned.7] CLI: ready command - show unblocked work (closed 2026-01-05)
- [trx-ned.6] CLI: close command - close issues (closed 2026-01-05)
- [trx-ned.5] CLI: update command - modify issues (closed 2026-01-05)
- [trx-ned.4] CLI: show command - issue details (closed 2026-01-05)
- [trx-ned.3] CLI: list command - show issues (closed 2026-01-05)
- [trx-ned.2] CLI: create command - new issues (closed 2026-01-05)
- [trx-ned.1] CLI: init command - create .trx directory (closed 2026-01-05)
- [trx-af3] trx-core: Core library implementation (closed 2026-01-05)
- [trx-af3.4] Hash-based ID generation for conflict-free merges (closed 2026-01-05)
- [trx-af3.3] Dependency graph with cycle detection and ready-work analysis (closed 2026-01-05)
- [trx-af3.2] JSONL store - read/write issues without SQLite (closed 2026-01-05)
- [trx-af3.1] Issue data model with beads-compatible JSONL format (closed 2026-01-05)
