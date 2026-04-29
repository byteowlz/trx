# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

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
