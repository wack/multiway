# CLAUDE.md

The current year is 2026. This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build and Development Commands

This project uses [cargo-make](https://github.com/sagiegurari/cargo-make) for task orchestration.

```bash
# Check if code compiles (prefer this over `cargo build` - faster since it skips code generation)
cargo check

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

# Sync Gateway API CRDs and regenerate Rust bindings
cargo make gateway-api-sync
```

## Gateway API CRDs

This project uses Kubernetes Gateway API CRDs. The CRD definitions are stored in `.crds/v<version>/` and Rust bindings are generated using [kopium](https://github.com/kube-rs/kopium).

**When to run `gateway-api-sync`:**
- After changing `GATEWAY_API_VERSION` in `Makefile.toml`
- When setting up a fresh clone of the repository
- When Gateway API releases a new version you want to adopt

**Related commands:**
- `cargo make gateway-api-refresh` - Download and split CRD YAML files only
- `cargo make gen-crds` - Regenerate Rust bindings from existing YAML files
- `cargo make gateway-api-install` - Install CRDs into the current Kubernetes cluster

## Conformance Tests

The project includes infrastructure for running the official [Gateway API conformance test suite](https://gateway-api.sigs.k8s.io/concepts/conformance/) against the implementation.

**Running conformance tests (full workflow):**
```bash
cargo make conformance
```

This builds the Docker image, loads it into kind, runs the tests as a Kubernetes Job, and outputs the logs.

**Individual conformance commands:**
- `cargo make conformance-build` - Build the conformance test Docker image
- `cargo make conformance-load` - Load the image into kind cluster
- `cargo make conformance-run` - Run the job and wait for completion
- `cargo make conformance-logs` - View logs from the last run
- `cargo make conformance-cleanup` - Delete the job and resources

**Configuration:**
The conformance tests are configured via environment variables in `conformance/job.yaml`:
- `GATEWAY_CLASS_NAME` - Gateway class to test (default: `multiway`)
- `SUPPORTED_FEATURES` - Comma-separated list of features (default: `Gateway,HTTPRoute`)
- `CONFORMANCE_PROFILES` - Optional conformance profiles to run
- `SHOW_DEBUG` - Enable debug output (`true`/`false`)

## Before Completing a Task

Always validate your changes before considering a task complete:

- **At minimum**: Run `cargo make fmt` to ensure code is properly formatted
- **Preferred**: Run `cargo make` to run the full test suite (formatting, linting, build, and tests)

Do not commit or mark work as done until validation passes.

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
