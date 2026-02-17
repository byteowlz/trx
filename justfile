# trx - Minimal git-backed issue tracker

default:
    @just --list

# Build all crates
build:
    cargo build --release

# Run tests
test:
    cargo test

# Run clippy lints
lint:
    cargo clippy --all-targets -- -D warnings

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt -- --check

# Build and install locally (CLI + TUI)
install:
    cargo install --path crates/trx-cli
    cargo install --path crates/trx-tui

# Install all binaries (CLI, TUI, API, MCP)
install-all:
    cargo install --path crates/trx-cli
    cargo install --path crates/trx-tui
    cargo install --path crates/trx-api
    cargo install --path crates/trx-mcp

# Run trx CLI
run *args:
    cargo run -p trx-cli -- {{args}}

# Run trx TUI
tui *args:
    cargo run -p trx-tui -- {{args}}

# Clean build artifacts
clean:
    cargo clean

# Generate JSON schema for config
schema:
    cargo run -p trx-cli -- schema > examples/config.schema.json

# Check all (test + lint + fmt)
check: test lint fmt-check

# Watch for changes and rebuild
watch:
    cargo watch -x check

# Show open issues
issues:
    @bd list --status open 2>/dev/null || cargo run -p trx-cli -- list

# Show ready (unblocked) issues
ready:
    @bd ready 2>/dev/null || cargo run -p trx-cli -- ready

# === Release ===

# Release: bump version, commit, tag, and push
release-bump version:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{version}}"
    if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "Error: Version must be in format X.Y.Z"
        exit 1
    fi
    echo "Bumping workspace version to $VERSION"
    sed -i "s/^version = .*/version = \"$VERSION\"/" Cargo.toml
    git add Cargo.toml
    git commit -m "chore: bump version to $VERSION"
    git tag "v$VERSION"
    git push origin main
    git push origin "v$VERSION"
    echo "Release v$VERSION pushed! Workflow will start automatically."

# Check release readiness
release-check:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Checking release readiness..."
    cargo test --quiet
    cargo clippy --all-targets --quiet -- -D warnings
    cargo fmt -- --check
    echo "All checks passed!"
