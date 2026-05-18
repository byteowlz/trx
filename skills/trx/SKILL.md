---
name: trx
description: Use trx, the minimal git-backed issue tracker, to create, query, and manage issues, including epic/child workflows and dependencies.
---

# trx (repo-shipped skill)

Use this skill when managing issues in this repository with `trx`.

## Core rules

- Use `trx` directly (global install), not `cargo run -p trx-cli -- ...`
- Run from repo root (where `.trx/` exists)

## Quick commands

```bash
trx ready
trx list
trx list --type epic
trx list --epic <epic-id>          # epic + all descendants/children
trx list --epic <epic-id> --all    # include closed
trx show <id>
trx create "Title" -t task -p 2
trx create "Child task" --parent <epic-id>
trx dep tree <id>
trx update <id> --status in_progress
trx close <id> -r "Done"
trx sync
```
