# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build and Development Commands

This project uses [cargo-make](https://github.com/sagiegurari/cargo-make) for task orchestration.

```bash
# Build the project
cargo build

# Run all checks (format, lint, build, test)
cargo make dev-test-flow

# Run tests (uses cargo-nextest)
cargo make test

# Run a single test
cargo nextest run <test_name>

# Format code
cargo make fmt

# Check formatting without modifying
cargo make check-format

# Run clippy
cargo make clippy-flow

# Watch tests and rerun on file change
cargo make bacon

# Run the CLI
cargo run -- --help

# Check for outdated dependencies
cargo make outdated
```

## Architecture

This is a Rust CLI template using Rust 2024 edition. The binary entry point is at `src/bin/main.rs`.

**CLI structure:**
- `src/bin/main.rs` - Entry point, parses CLI args and dispatches commands
- `src/cli/mod.rs` - CLI definition using clap with derive macros. Contains `Cli` struct with global options and `CliCommand` enum for subcommands
- `src/cli/colors.rs` - Color output configuration

**Key patterns:**
- Commands are dispatched via `CliCommand::dispatch()` method
- Global CLI options: `--log-level`, `--log-format`, `--enable-colors`
- Uses `miette` for error handling with fancy error reports
- Uses `tracing` for structured logging
