# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Fixed

- Serialize `.trx/issues.jsonl` mutations with a lock file and reload under the lock so concurrent `trx create`/update/delete commands cannot corrupt or overwrite the JSONL store.
- Make `trx-core` AGENT_CTX environment tests isolate all known context variables so they pass when run inside an agent harness that sets AGENT_CTX values.

## [0.6.0] - 2026-05-16

### Added

- `trx list --sort <field>` and `--reverse` flags to control output order.
  Accepted fields: `priority` (default), `created`, `updated`, `closed`,
  `id`, `status`. Date sorts default to newest-first and prepend the
  matching date column to each line.
- `trx log`: chronological activity feed across all events, grouped by
  session (platform/harness session id), with action-colored bullets,
  indented field diffs, and a compact `[harness · session · model]`
  AGENT_CTX line. Flags: `--issue`, `--session`, `--user`, `--action`,
  `--since`, `--until`, `--limit`, `--no-group` (flat output),
  `-v/--verbose` (expanded AGENT_CTX block per event), `--json`.
  (trx-3e6q.1)
- `trx sessions`: one row per distinct session id with user, harness,
  model, event count, time range, and the issues touched. Pass a session
  id to drill into that session's full trace. Flags: `--user`, `--since`,
  `--until`, `--limit`, `-v/--verbose`, `--json`. (trx-3e6q.2)
- `trx stats`: aggregate activity over a window (default last 30 days),
  with a Unicode-block sparkline by day or hour and ranked bar charts by
  action / user / harness. Flags: `--since`, `--until`, `--by day|hour`,
  `--json`. (trx-3e6q.3)
- `trx-core` now exposes `SessionSummary` and `summarize_sessions()` for
  embedders that want the same grouping logic.
- TUI: per-issue **Activity pane** (`T` toggles right pane between Issue
  details and Activity). Shows event timeline with action-colored
  actions, notes, field diffs, and a compact AGENT_CTX footer. Press `v`
  to expand the AGENT_CTX block (user, platform, harness, session,
  workspace, model, request, correlation). (trx-3e6q.4)
- TUI: **Sessions view** (`S` toggles middle pane between Issues and
  Sessions). Each row shows session id, event count, user / harness /
  model, and time range. Drilling into a session populates the right
  pane with its full event trace. (trx-3e6q.5)
- TUI: **Follow mode** (`F` toggles). When on, the TUI re-reads
  `.trx/events.jsonl` every ~2s so newly emitted events appear live;
  pane titles and the status bar show a `● follow` indicator.
  (trx-3e6q.6)
- `trx heatmap`: GitHub-style calendar heatmap of event activity
  (Mon-anchored rows × week columns) with Unicode block intensity
  cells. Flags: `--weeks` (default 13), `--since`, `--until`, `--json`.
  (trx-bhrt)
- `trx swimlane`: issue × time grid showing each issue's event timeline
  as a row of action glyphs (`+` created, `*` updated, `↑` reopened,
  `■` closed, `✕` deleted, `◆` dep, `·` other). Auto-sized to the
  terminal; `--limit` caps the number of issues, `--cols` overrides
  width. Flags: `--since`, `--until`, `--limit`, `--cols`. (trx-bhrt)
- TUI: **Dashboard mode** (`D` toggles). Full-screen visual summary
  combining a 13-week activity heatmap, ranked action/user bar charts,
  a top-sessions panel, and a live event tail. Honors `F` for follow
  mode; `Esc` / `D` / `q` exits back to normal mode. (trx-bhrt)

## [0.5.1] - 2026-05-08

### Added

- `trx export`: one-shot Markdown rendering of all issues, grouped by
  status (Open / In Progress / Blocked / Closed) and sorted by
  (priority, id). `-o PATH` writes to a file; default is stdout.
  `--all` includes closed issues; `-t/--type` and `--label` filter the
  output. Not auto-regenerated — call it whenever you want a snapshot.

## [0.5.0] - 2026-05-07

### Added

- Append-only event log at `.trx/events.jsonl`. Every create / update /
  close / dep_added / dep_removed mutation records an `Event` tagged with
  the active `AGENT_CTX_*` identity (user, platform/harness session,
  workspace, model, request id). On `update`, the event carries a field-
  level diff. Open-append-flush-fsync per write — no merge logic needed
  since two writers always produce distinct event ids. (trx-40mg.4)
- `trx history <id>`: timeline of events for one issue (most recent first,
  `--limit`, `--json`). (trx-40mg.5)
- `trx events`: cross-issue event query with `--issue`, `--session`
  (matches platform_session_id or harness_session_id), `--user`,
  `--action`, `--since`, `--until`, `--limit`, and `--json`. (trx-40mg.5)
- `trx show <id>` now prints a "Recent activity" footer with the last 5
  events for that issue. (trx-40mg.6)
- `trx ready` accepts `--type`, `-P/--priority`, `--label`, and `--limit`
  filters. Filtered-out issues stay out of the "Blocked" section. (trx-40mg.6)
- `trx close <id> [<id>…]` accepts multiple issue ids and closes them in
  one shot with a shared reason. (trx-40mg.6)
- `trx info` now reports the event count alongside the issue counts.
- `Issue.created_by` and `Issue.sessions` are auto-populated from
  `AGENT_CTX_USER_ID` / session ids on first creation. Existing values are
  never overwritten. (trx-40mg.4)
- `trx info` command: prints effective AGENT_CTX context, store summary
  (path, format, issue counts, pending migration flag), and trx version.
  Supports `--json`. (trx-40mg.3)
- `trx-core::agent_ctx` module: defensive reader for the `AGENT_CTX_*`
  environment contract (v1). Returns an `AgentCtx` of `Option<String>` fields;
  missing or whitespace-only variables are treated as absent. See
  `schemas/agent-context-env/agent-context-env.md`. (trx-40mg.3)
- Transparent legacy v2 (Automerge) → JSONL migration on `Store::open_at`.
  `.trx/crdt/` is loaded into memory; the next mutation writes canonical
  JSONL (atomic temp+rename+fsync) and removes the legacy directory plus
  `ISSUES.md`. Reads never mutate disk. (trx-40mg.1, trx-40mg.2)

### Changed

- JSONL is now the only on-disk format. There is no version flag; legacy
  layouts are detected structurally. (trx-40mg.1)
- `Store` is the single canonical store implementation. `UnifiedStore` and
  the v1/v2 dispatcher have been removed.
- `issues.jsonl` lines are now written in stable id order, making git diffs
  reviewable.
- `--type` is now the canonical long form for the issue-type filter on
  `trx create`, `trx list`, and `trx ready` (previously kebab-cased to
  `--issue-type`). The `-t` short form is unchanged. (trx-40mg.6)

### Removed

- `StorageVersion` enum and the `storage_version` config key. The field is
  ignored if present in existing `config.toml`.
- `trx-core::CrdtStore`, `trx-core::UnifiedStore`, `migrate_v1_to_v2`,
  `rollback_v2_to_v1`, and `MigrationResult`.
- `trx migrate` command — no longer needed. Open any repo and the next
  mutation migrates it.
- `trx resolve` and `trx merge-driver` commands — these existed only to
  regenerate / merge `ISSUES.md`, which is no longer produced.
- `trx dep add` and `trx dep rm` — the `--blocks` flag had inverted
  semantics from its name. Use `trx dep block --by` and
  `trx dep unblock --by` instead, which are correctly named. (trx-40mg.6)
- `.trx/ISSUES.md` is no longer generated. JSONL is itself human-readable.

## [0.4.1] - 2026-04-29

### Added

- `Store::open_at(root)`, `CrdtStore::open_at(root)`, and `UnifiedStore::open_at(root)`
  constructors that take an explicit repo root instead of probing the current
  working directory. Lets embedders (e.g. servers handling multiple workspaces)
  use `trx-core` directly without `chdir` or shelling out to the CLI. Existing
  `open()` constructors delegate to `open_at` after resolving the root, so CLI
  behavior is unchanged. (trx-bwt9)

### Fixed

- CLI `list --status closed` now includes closed issues.
- TUI now uses `UnifiedStore` so it works with both V1 (JSONL) and V2 (CRDT)
  storage backends.

## [0.4.0] - earlier

- CLI ergonomics, linting, and `sessions` field on `Issue`. See git history for
  details (commit `53120ac`).
