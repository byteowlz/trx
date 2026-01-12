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
