# CLAUDE.md

The current year is 2026. This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Multiway is a Kubernetes [Gateway API](https://gateway-api.sigs.k8s.io/) implementation written in Rust (2024 edition). It implements a split-architecture controller: a **control plane** that reconciles Gateway API resources and a **data plane** (built on [Pingora](https://github.com/cloudflare/pingora)) that proxies HTTP traffic. The controller name is `io.multiway/gateway-controller`.

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

# Lint shell scripts with ShellCheck
cargo make shellcheck
```

## Before Completing a Task

Always validate your changes before considering a task complete:

- **At minimum**: Run `cargo make fmt` to ensure code is properly formatted
- **Preferred**: Run `cargo make` to run the full test suite (formatting, linting, build, and tests)
- **After editing shell scripts**: Run `cargo make shellcheck` to lint shell scripts

Do not commit or mark work as done until validation passes.

## Workspace Structure

```
multiway/
├── crates/
│   ├── controlplane/        # Core controller logic (binary: `multiway`)
│   ├── dataplane/           # Pingora-based HTTP proxy (binary: `multiway-dataplane`)
│   └── gateway-crds/        # Auto-generated Rust bindings for Gateway API CRDs
├── .crds/v1.2.1/            # Gateway API v1.2.1 CRD YAML definitions
├── conformance/             # Gateway API conformance test suite (Docker + K8s Job)
├── deploy/                  # Kubernetes deployment manifests (Kustomize)
├── docs/design/             # Architecture and design documents
├── Makefile.toml            # cargo-make task definitions
├── Dockerfile               # Multi-stage build for controlplane and dataplane
└── Dockerfile.dev           # Development container
```

## Architecture: Functional Core, Imperative Shell

The control plane follows a **"Functional Core, Imperative Shell"** (aka sans-I/O) architecture. Effects are described as data, not executed directly. This is the most important design principle in the codebase.

### Three-Phase Reconciliation Loop

Every reconciliation follows this pattern:

```
1. FETCH (effectful)    → SnapshotFetcher builds a WorldSnapshot from the K8s API
2. COMPUTE (pure)       → Pure function takes WorldSnapshot, returns ReconcileResult
3. EXECUTE (effectful)  → ReconcileExecutor applies ReconcileResult to the cluster
```

### Why This Matters

- **Unit tests run in milliseconds** — no cluster, no mocks, no async
- **Deterministic** — timestamps are injected via `WorldSnapshot.now`, never `Utc::now()`
- **Spec traceable** — each Gateway API requirement maps to a unit test on the pure core
- **Composable** — `ReconcileResult` values can be merged, filtered, and inspected

### Module Layout

```
crates/controlplane/src/
├── core/                  # PURE — no I/O, no .await, no side effects
│   ├── snapshot.rs        # WorldSnapshot, WorldSnapshotBuilder
│   ├── result.rs          # ReconcileResult, ResourceUpsert, StatusUpdate, RequeueDecision
│   ├── reconcile.rs       # reconcile_gateway_class(), reconcile_gateway(), reconcile_httproute()
│   └── validate.rs        # Pure validation: listeners, parent refs, namespaces
│
├── shell/                 # EFFECTFUL — all I/O lives here
│   ├── fetcher.rs         # SnapshotFetcher: K8s API → WorldSnapshot
│   └── executor.rs        # ReconcileExecutor: ReconcileResult → K8s API
│
├── controller/            # WIRING — connects core + shell via kube::Controller
│   ├── reconciler.rs      # GatewayController: spawns 3 concurrent reconcilers
│   ├── gateway_class.rs   # GatewayClass reconciler
│   ├── gateway.rs         # Gateway reconciler
│   ├── httproute.rs       # HTTPRoute reconciler
│   ├── config.rs          # GatewayConfig, DataPlaneNames, constants
│   ├── context.rs         # ControllerContext, ControllerConfig
│   └── error.rs           # ControllerError, Result type alias
│
├── cli/                   # CLI definitions (clap derive)
│   ├── mod.rs             # Cli struct, CliCommand enum
│   ├── controller.rs      # `multiway controller` subcommand
│   ├── gateway.rs         # `multiway gateway` subcommand (data plane)
│   ├── colors.rs          # Color output configuration
│   └── version.rs         # Version subcommand
│
├── bin/main.rs            # Entry point
└── lib.rs                 # Public re-exports
```

### Core Types

**WorldSnapshot** (`core/snapshot.rs`) — immutable snapshot of all cluster state needed for reconciliation:

```rust
pub struct WorldSnapshot {
    pub now: DateTime<Utc>,                                    // Injected, never Utc::now()
    pub gateway_classes: BTreeMap<String, GatewayClass>,       // Cluster-scoped
    pub gateways: BTreeMap<(String, String), Gateway>,         // (namespace, name)
    pub httproutes: BTreeMap<(String, String), HTTPRoute>,
    pub services: BTreeMap<(String, String), Service>,
    pub configmaps: BTreeMap<(String, String), ConfigMap>,
    pub deployments: BTreeMap<(String, String), Deployment>,
    pub secrets: BTreeMap<(String, String), Secret>,
    pub reference_grants: BTreeMap<(String, String), ReferenceGrant>,
    pub nodes: BTreeMap<String, Node>,
}
```

All stores use `BTreeMap` (not `HashMap`) for deterministic iteration order.

**ReconcileResult** (`core/result.rs`) — effects described as data:

```rust
pub struct ReconcileResult {
    pub upserts: Vec<ResourceUpsert>,       // Create/update resources
    pub deletes: Vec<ResourceDelete>,       // Delete resources
    pub status_updates: Vec<StatusUpdate>,  // Update .status subresources
    pub requeue: Option<RequeueDecision>,   // When to reconcile again
    pub events: Vec<ReconcileEvent>,        // Observability events
}
```

Both types have fluent builder APIs. Use `WorldSnapshotBuilder` and `ReconcileResult::new().upsert_deployment(d).emit_normal(...)` chains.

### Pure Reconciliation Functions

The three entry points in `core/reconcile.rs`:

```rust
pub fn reconcile_gateway_class(snapshot, config, gateway_class_name) -> ReconcileResult
pub fn reconcile_gateway(snapshot, config, gateway_ns, gateway_name) -> ReconcileResult
pub fn reconcile_httproute(snapshot, config, route_ns, route_name) -> ReconcileResult
```

These functions have **no `.await`**, **no I/O**, and get all time from `snapshot.now`.

### Execution Ordering

The executor (`shell/executor.rs`) applies effects in this order:
1. Log events
2. **Status updates FIRST** (Gateway API spec requires `observedGeneration` updates even when resource creation fails)
3. Upserts (server-side apply)
4. Deletes (best-effort, don't fail the reconciliation)

### Testing Pattern

Unit tests create a snapshot, call a pure function, and assert on the result:

```rust
#[test]
fn test_gateway_class_accepted() {
    let snapshot = WorldSnapshotBuilder::new()
        .at_time(fixed_time)
        .with_gateway_class(create_gateway_class("multiway", CONTROLLER_NAME))
        .build();

    let result = reconcile_gateway_class(&snapshot, &config, "multiway");

    assert_eq!(result.status_updates.len(), 1);
    // Inspect the status update...
}
```

No cluster, no mocks, no async runtime needed.

## Three Reconcilers

The `GatewayController` spawns three concurrent reconcilers via `tokio::spawn`:

| Reconciler | Watches | Produces |
|---|---|---|
| **GatewayClass** | GatewayClass resources | Status updates (Accepted/Rejected) |
| **Gateway** | Gateways + owned Deployments/Services/ConfigMaps | 6 resources: Deployment, Service, ConfigMap, ServiceAccount, Role, RoleBinding + status |
| **HTTPRoute** | HTTPRoutes + parent Gateways | ConfigMap updates + route status |

Each reconciler follows the same fetch → compute → execute pattern.

## Data Plane

The data plane (`crates/dataplane/`) is a Pingora-based HTTP proxy:

- **Config source**: Reads a ConfigMap (`multiway-config-{gateway_name}`) containing JSON `GatewayConfig`
- **Hot reload**: Watches ConfigMap via K8s API (not filesystem mounts) using `arc-swap` for atomic config swaps
- **Routing**: Hostname + path + header + query param matching with support for redirects, rewrites, header modification, and mirroring
- **Per-listener**: Each listener gets its own TCP listener bound to `0.0.0.0:{port}`

## Control-to-Data-Plane Communication

The control plane writes a `GatewayConfig` JSON blob into a ConfigMap. The data plane watches that ConfigMap and hot-reloads its routing table. The config schema is defined in both `crates/controlplane/src/controller/config.rs` and `crates/dataplane/src/config.rs`.

## Key Constants

| Constant | Value | Location |
|---|---|---|
| `CONTROLLER_NAME` | `io.multiway/gateway-controller` | `controller/config.rs` |
| `MANAGED_BY_LABEL` | `app.kubernetes.io/managed-by` | `controller/config.rs` |
| `MANAGED_BY_VALUE` | `multiway-gateway-controller` | `controller/config.rs` |
| `GATEWAY_NAME_LABEL` | `gateway.networking.k8s.io/gateway-name` | `controller/config.rs` |
| `CONFIG_KEY` | `config.json` | `controller/config.rs` |

Managed resource names follow the pattern `multiway-{gateway_name}`.

## CLI

```
multiway version                # Print version
multiway controller [OPTIONS]   # Run the control plane
multiway gateway [OPTIONS]      # Run the data plane (Pingora)
```

- Global options: `--log-level`, `--log-format` (text/json), `--enable-colors`
- Uses `clap` derive macros, `miette` for error reporting, `tracing` for structured logging

## Gateway API CRDs

CRD definitions are stored in `.crds/v1.2.1/` and Rust bindings are generated using [kopium](https://github.com/kube-rs/kopium) into `crates/gateway-crds/`.

**When to run `gateway-api-sync`:**
- After changing `GATEWAY_API_VERSION` in `Makefile.toml`
- When setting up a fresh clone of the repository
- When Gateway API releases a new version you want to adopt

**Related commands:**
- `cargo make gateway-api-refresh` — Download and split CRD YAML files only
- `cargo make gen-crds` — Regenerate Rust bindings from existing YAML files
- `cargo make gateway-api-install` — Install CRDs into the current Kubernetes cluster

## Conformance Tests

The project includes infrastructure for running the official [Gateway API conformance test suite](https://gateway-api.sigs.k8s.io/concepts/conformance/).

```bash
cargo make conformance          # Full workflow: build, push, run
cargo make conformance-build    # Build the conformance test Docker image
cargo make conformance-push     # Push image to registry
cargo make conformance-run      # Run the job and wait for completion
cargo make conformance-logs     # View logs from the last run
cargo make conformance-cleanup  # Delete the job and resources
```

Configuration is in `conformance/job.yaml` via environment variables: `GATEWAY_CLASS_NAME`, `SUPPORTED_FEATURES`, `CONFORMANCE_PROFILES`, `SHOW_DEBUG`.

## Docker Images

```bash
cargo make docker-build-controlplane   # Build control plane image
cargo make docker-build-dataplane      # Build data plane image
cargo make docker-build-all            # Build both
cargo make docker-build-all-cross      # Multi-platform (amd64 + arm64)
cargo make docker-build-all-push       # Build + push to $DOCKER_REGISTRY
```

## DigitalOcean Cluster Management

```bash
cargo make do-create    # Create DO K8s cluster
cargo make do-delete    # Delete DO K8s cluster
cargo make do-list      # List clusters
cargo make do-use       # Set kubectl context
```

## Coding Guidelines

- **Never call `Utc::now()` in core/**: All time must come from `WorldSnapshot.now`
- **No `.await` in `core/`**: The functional core must remain pure and synchronous
- **`core/` must not import from `shell/` or `controller/`**: Dependencies flow inward only
- **Use `BTreeMap` not `HashMap`**: For deterministic iteration in snapshots and configs
- **Effects as data**: Reconciliation functions return `ReconcileResult`, they don't call APIs
- **Status updates before resource creation**: The executor applies status first per Gateway API spec
- **Server-side apply**: All upserts use Kubernetes server-side apply for idempotency
- **Builder patterns**: Use `WorldSnapshotBuilder` in tests, `GatewayControllerBuilder` for controller setup
- **Label-based ownership**: Managed resources are identified by `app.kubernetes.io/managed-by=multiway-gateway-controller`

## Design Documents

Detailed architectural rationale lives in `docs/design/`:

- `functional-control-plane.md` — Original design for the sans-I/O architecture
- `sans-io-crate-extraction.md` — Plan to extract generic sans-I/O patterns into a reusable `kube-sans-io` crate
- `kube-sans-io-tickets.md` — Tracking issues for the extraction work
