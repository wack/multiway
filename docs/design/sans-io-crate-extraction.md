# Sans-I/O Crate Extraction Design

## Summary

This document analyzes what it would take to extract the generalizable "sans-I/O"
controller pattern from `crates/controlplane` into a standalone, reusable crate
(working name: `kube-sans-io`) that any Kubernetes controller could use to
separate effectful operations from domain logic.

## Motivation

The multiway control plane implements a clean "Functional Core, Imperative Shell"
architecture:

```
WorldSnapshot (all cluster state)
    ↓  pure functions
ReconcileResult (effects as data)
    ↓  execute
Kubernetes cluster
```

About 10% of the controlplane code (~420 lines) is structurally generic — it
doesn't mention Gateway API types at all. But that 10% is the *skeleton* that
the other 90% hangs on. Extracting it into a crate would let other controllers
adopt the same testability benefits without reinventing the architecture.

## What Would Move Into the New Crate

### 1. ReconcileResult core (`core/result.rs`)

The following types and logic are fully generic today:

```rust
// kube_sans_io::result

/// The output of a pure reconciliation function.
/// Effects are described as data, not executed.
#[derive(Debug, Clone, Default)]
pub struct ReconcileResult<U, S>
where
    U: std::fmt::Debug + Clone,
    S: std::fmt::Debug + Clone,
{
    pub upserts: Vec<U>,
    pub deletes: Vec<ResourceDelete>,
    pub status_updates: Vec<S>,
    pub requeue: Option<RequeueDecision>,
    pub events: Vec<ReconcileEvent>,
}
```

The key change: today `ResourceUpsert` and `StatusUpdate` are concrete enums
with Gateway API variants. The generic crate parameterizes over them.

**Types that move unchanged:**

| Type | Current location | Notes |
|------|-----------------|-------|
| `RequeueDecision` | `core/result.rs:346-382` | After/OnError/Never + `min()` logic |
| `ReconcileEvent` | `core/result.rs:384-393` | reason + message |
| `EventSeverity` | `core/result.rs:396-402` | Normal / Warning |
| `ResourceDelete` | `core/result.rs:275-312` | Already uses generic strings |

**Methods that move (become generic over `U`/`S`):**

| Method | Notes |
|--------|-------|
| `new()` | Trivial |
| `with_requeue()` / `with_requeue_on_error()` | No type params needed |
| `emit_event()` / `emit_normal()` / `emit_warning()` | No type params needed |
| `merge()` | Works on any `U`/`S` with `Extend` |
| `has_upserts()` / `has_deletes()` / `has_status_updates()` | Trivial |
| `delete_resource()` | Works on `ResourceDelete` directly |

**Methods that stay in controlplane (type-specific accessors):**

| Method | Reason |
|--------|--------|
| `upsert_deployment()` | Takes `Deployment`, pushes `ResourceUpsert::Deployment` |
| `upsert_service()` | Takes `Service`, pushes `ResourceUpsert::Service` |
| `update_gateway_status()` | Takes `GatewayStatus` |
| `deployment_upserts()` | Filters by `ResourceUpsert::Deployment` variant |
| `gateway_status_update()` | Filters by `StatusUpdate::Gateway` variant |
| etc. | All type-specific convenience methods |

### 2. WorldSnapshot trait

Today `WorldSnapshot` is a concrete struct with Gateway API fields. The generic
crate would provide a trait and utilities:

```rust
// kube_sans_io::snapshot

use jiff::Timestamp;

/// A point-in-time snapshot of cluster state.
///
/// Implementors populate this with whatever resource types their
/// controller needs. The only requirement is an injected timestamp
/// for deterministic testing.
pub trait Snapshot {
    /// The time this snapshot was taken.
    /// Inject a fixed value in tests for deterministic timestamps.
    fn now(&self) -> Timestamp;
}

// Note: k8s-openapi uses chrono for its `Time` type internally.
// The controlplane crate will need to convert at the boundary:
//   jiff::Timestamp::from_second(chrono_time.timestamp())
// This is a one-line conversion at the point where status conditions
// are built (core/reconcile.rs), not a pervasive change.
```

This is deliberately minimal. The multiway `WorldSnapshot` would impl this
trait, but the trait itself doesn't prescribe what resources to store. The
snapshot's fields are inherently domain-specific.

**What the trait enables:** The generic executor and bridge code can accept
`impl Snapshot` without knowing about Gateway/HTTPRoute/etc.

**What does NOT belong in the trait:** Query methods like `routes_for_gateway()`,
`get_gateway_class()`, `service_exists()`. These are domain logic.

### 3. Snapshot builder utilities

The pattern of keying namespaced resources by `(namespace, name)` and
cluster-scoped resources by `name` is universal. The crate could provide
helper types:

```rust
// kube_sans_io::snapshot

use std::collections::BTreeMap;
use kube::ResourceExt;

/// A BTreeMap keyed by (namespace, name) for namespaced resources.
pub type NamespacedStore<T> = BTreeMap<(String, String), T>;

/// A BTreeMap keyed by name for cluster-scoped resources.
pub type ClusterStore<T> = BTreeMap<String, T>;

/// Insert a namespaced kube resource into a NamespacedStore.
pub fn insert_namespaced<T: ResourceExt>(store: &mut NamespacedStore<T>, resource: T) {
    let ns = resource.namespace().unwrap_or_default();
    let name = resource.name_any();
    store.insert((ns, name), resource);
}

/// Insert a cluster-scoped kube resource into a ClusterStore.
pub fn insert_cluster_scoped<T: ResourceExt>(store: &mut ClusterStore<T>, resource: T) {
    let name = resource.name_any();
    store.insert(name, resource);
}

/// Look up a namespaced resource.
pub fn get_namespaced<T>(
    store: &NamespacedStore<T>,
    namespace: &str,
    name: &str,
) -> Option<&T> {
    store.get(&(namespace.to_string(), name.to_string()))
}
```

### 4. Executor trait and orchestration (`shell/executor.rs`)

The execution *orchestration* is generic — the order of operations and error
handling — but the type dispatch is domain-specific. The crate provides a
trait and a generic `execute()` function.

**Critical constraint:** We require dynamic dispatch (`dyn Executor`) so that
controllers can inject fake/recording executors in tests without needing a
real Kubernetes cluster. This means async trait methods must be object-safe.

Native `async fn` in traits (stable since Rust 1.75) produces RPITIT return
types that are NOT dyn-compatible. So we use `#[async_trait]` which desugars
async methods into `-> Pin<Box<dyn Future<...> + Send + '_>>`, making the
trait object-safe. (`async-trait` is already a dependency.)

```rust
// kube_sans_io::executor

use std::time::Duration;
use async_trait::async_trait;
use kube::runtime::controller::Action;

/// The complete interface for applying reconciliation effects.
///
/// Object-safe for use as `dyn Executor<...>`, enabling fake/recording
/// implementations in tests.
///
/// # Example: Production
///
/// ```ignore
/// struct KubeExecutor { client: Client }
///
/// #[async_trait]
/// impl Executor for KubeExecutor {
///     type Upsert = ResourceUpsert;
///     type StatusUpdate = StatusUpdate;
///     type Error = ControllerError;
///
///     async fn execute_upsert(&self, u: &ResourceUpsert) -> Result<(), Self::Error> {
///         match u {
///             ResourceUpsert::Deployment(d) => { /* SSA patch */ }
///             ResourceUpsert::Service(s) => { /* SSA patch */ }
///             // ...
///         }
///         Ok(())
///     }
///     // ...
/// }
/// ```
///
/// # Example: Test fake
///
/// ```ignore
/// struct RecordingExecutor {
///     upserts: Mutex<Vec<ResourceUpsert>>,
///     statuses: Mutex<Vec<StatusUpdate>>,
///     deletes: Mutex<Vec<ResourceDelete>>,
/// }
///
/// #[async_trait]
/// impl Executor for RecordingExecutor {
///     type Upsert = ResourceUpsert;
///     type StatusUpdate = StatusUpdate;
///     type Error = Infallible;
///
///     async fn execute_upsert(&self, u: &ResourceUpsert) -> Result<(), Infallible> {
///         self.upserts.lock().unwrap().push(u.clone());
///         Ok(())
///     }
///     // ...
/// }
/// ```
#[async_trait]
pub trait Executor: Send + Sync {
    /// The domain-specific upsert type (e.g., an enum of resource kinds).
    type Upsert: std::fmt::Debug + Clone + Send + Sync;
    /// The domain-specific status update type.
    type StatusUpdate: std::fmt::Debug + Clone + Send + Sync;
    /// Error type for execution failures.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Apply a single resource upsert (create or update).
    async fn execute_upsert(&self, upsert: &Self::Upsert) -> Result<(), Self::Error>;

    /// Apply a single status subresource update.
    async fn execute_status_update(&self, update: &Self::StatusUpdate) -> Result<(), Self::Error>;

    /// Delete a single resource.
    async fn execute_delete(&self, delete: &ResourceDelete) -> Result<(), Self::Error>;
}

/// Execute a ReconcileResult using the provided executor.
///
/// Orchestration order (important for correctness):
/// 1. Log events
/// 2. Execute status updates FIRST (for observedGeneration correctness)
/// 3. Execute upserts
/// 4. Execute deletes (continue on individual delete errors)
/// 5. Convert RequeueDecision to Action
///
/// This function accepts `&dyn Executor<...>` — both production and test
/// executor implementations work identically.
pub async fn execute<E: Executor + ?Sized>(
    result: ReconcileResult<E::Upsert, E::StatusUpdate>,
    executor: &E,
) -> Result<Action, E::Error> {
    // Log events
    for event in &result.events {
        match event.severity {
            EventSeverity::Normal => {
                tracing::info!(reason = %event.reason, message = %event.message, "Event");
            }
            EventSeverity::Warning => {
                tracing::warn!(reason = %event.reason, message = %event.message, "Warning");
            }
        }
    }

    // Status updates FIRST — ensures observedGeneration is set even if
    // resource creation fails. Required for Gateway API conformance and
    // good practice for any controller.
    for update in &result.status_updates {
        executor.execute_status_update(update).await?;
    }

    // Upserts
    for upsert in &result.upserts {
        executor.execute_upsert(upsert).await?;
    }

    // Deletes (continue on error — a failed delete shouldn't block others)
    for delete in &result.deletes {
        if let Err(e) = executor.execute_delete(delete).await {
            tracing::error!(error = %e, "Failed to execute delete");
        }
    }

    Ok(requeue_to_action(result.requeue))
}

/// Convert a RequeueDecision to a kube Action.
pub fn requeue_to_action(requeue: Option<RequeueDecision>) -> Action {
    match requeue {
        Some(RequeueDecision::After(d)) => Action::requeue(d),
        Some(RequeueDecision::OnError(d)) => Action::requeue(d),
        Some(RequeueDecision::Never) => Action::requeue(Duration::from_secs(3600)),
        None => Action::requeue(Duration::from_secs(300)),
    }
}
```

**Why `#[async_trait]` instead of native `async fn` in traits:**

Rust 2024 edition supports native `async fn` in traits, but these produce
RPITIT return types (`impl Future<...>`) that make the trait NOT object-safe.
You cannot write `dyn Executor` if the trait uses native async methods. The
`#[async_trait]` macro transforms each `async fn` into a method returning
`Pin<Box<dyn Future<...> + Send + '_>>`, which IS object-safe. This is the
standard approach in the Rust ecosystem (used by `tower`, `axum`, `tonic`,
and kube's own `runtime` internals).

The boxing cost is negligible — executor methods make Kubernetes API calls
that take milliseconds; the nanosecond overhead of a heap-allocated future
is irrelevant. And in tests, the fake executor methods are trivial, so the
boxing cost is equally irrelevant there.

**Why a single `Executor` trait instead of three separate traits:**

The earlier draft had `UpsertExecutor`, `StatusExecutor`, `DeleteExecutor` as
separate traits. This is over-decomposed: in practice, every executor needs
all three capabilities, and they always share the same `Client`/state. A
single trait with associated types means:
- One `impl` block instead of three
- One `dyn Executor<...>` instead of `dyn UpsertExecutor<...> + StatusExecutor<...> + DeleteExecutor<...>` (which isn't even valid — Rust doesn't support multiple trait bounds on trait objects with conflicting associated types)
- Simpler type signatures everywhere

**Why associated types instead of type parameters:**

Associated types (`type Upsert`) are "output types" determined by the
implementor, while type parameters would be "input types" chosen by the
caller. Each executor implementation works with exactly one set of resource
types, so associated types are the correct Rust idiom. For dyn dispatch,
you specify them: `dyn Executor<Upsert = ResourceUpsert, StatusUpdate = StatusUpdate, Error = ControllerError>`.

A type alias in the controller crate keeps this ergonomic:
```rust
pub type DynExecutor = dyn Executor<
    Upsert = ResourceUpsert,
    StatusUpdate = StatusUpdate,
    Error = ControllerError,
>;
```

### 5. Built-in test fakes

The crate should ship with a `RecordingExecutor` that controllers can use
out of the box for testing the full reconciliation bridge:

```rust
// kube_sans_io::testing

use std::sync::Mutex;

/// An executor that records all effects without applying them.
///
/// Use this to test the full fetch→compute→execute pipeline
/// without a Kubernetes cluster.
///
/// ```ignore
/// let executor = RecordingExecutor::<ResourceUpsert, StatusUpdate>::new();
/// kube_sans_io::execute(result, &executor).await.unwrap();
///
/// // Assert on what was recorded
/// assert_eq!(executor.upserts().len(), 3);
/// assert_eq!(executor.status_updates().len(), 1);
/// assert!(executor.deletes().is_empty());
/// ```
pub struct RecordingExecutor<U, S> {
    upserts: Mutex<Vec<U>>,
    status_updates: Mutex<Vec<S>>,
    deletes: Mutex<Vec<ResourceDelete>>,
}

/// An executor that fails on the Nth operation, for testing error paths.
pub struct FailingExecutor<U, S> {
    inner: RecordingExecutor<U, S>,
    fail_after: usize,
    call_count: Mutex<usize>,
}
```

These are parameterized over `U` and `S`, so any controller can use them
with its own domain types. The `RecordingExecutor` also doubles as a
snapshot of "what would have happened", complementing the pure-core
`ReconcileResult` inspection that works without any executor at all.

**Testing levels enabled by this design:**

| Level | How | Needs executor? |
|-------|-----|-----------------|
| Pure domain logic | Inspect `ReconcileResult` directly | No |
| Orchestration order | `RecordingExecutor` — verify status updates came before upserts | Yes (fake) |
| Error handling | `FailingExecutor` — verify requeue on Nth failure | Yes (fake) |
| Integration test | Real `ReconcileExecutor` against kind cluster | Yes (real) |

### 6. The bridge pattern

The three-line bridge pattern (fetch → compute → execute) is the heart of
the architecture. With the `dyn Executor` trait, it becomes natural to accept
the executor as an injected dependency:

```rust
// In each controller's reconcile function:

async fn reconcile_gateway(
    gateway: Arc<Gateway>,
    ctx: Arc<ControllerContext>,
) -> Result<Action> {
    let namespace = gateway.namespace().unwrap_or_default();
    let name = gateway.name_any();

    // 1. Build snapshot (effectful)
    let snapshot = ctx.fetcher.snapshot_for_gateway(&namespace, &name).await?;

    // 2. Pure reconciliation (no I/O)
    let result = core::reconcile_gateway(&snapshot, &ctx.config, &namespace, &name);

    // 3. Execute effects via injected executor (dyn dispatch)
    kube_sans_io::execute(result, ctx.executor.as_ref()).await
}
```

The ControllerContext now holds a `Box<dyn Executor<...>>`:

```rust
pub struct ControllerContext {
    pub client: Client,
    pub config: ControllerConfig,
    pub executor: Box<DynExecutor>,  // injected — real or fake
}
```

In production, this is the real `ReconcileExecutor`. In tests, it can be a
`RecordingExecutor` or `FailingExecutor`.

Note: we do NOT provide a generic `reconcile()` function that wraps the
three-step pattern. It's only three lines, and abstracting over the fetcher
signature (which varies per resource type) would add complexity without value.
The pattern is better expressed as documentation and convention than as a
generic function.

## What Stays in Controlplane

Everything domain-specific remains in `crates/controlplane`:

| Component | Why it stays |
|-----------|-------------|
| `WorldSnapshot` struct (fields + query methods) | Gateway API resource graph |
| `WorldSnapshotBuilder` (with_gateway, with_httproute, etc.) | Domain resource types |
| `ResourceUpsert` enum | What child resources this controller creates |
| `StatusUpdate` enum | Which CRD statuses this controller writes |
| `ReconcileResult` convenience methods | Type-specific upsert/query methods |
| `reconcile_gateway_class/gateway/httproute()` | Pure domain logic |
| `validate_*()` functions | Gateway API spec rules |
| `build_*()` resource constructors | Data plane resource generation |
| `SnapshotFetcher` | Which resources to fetch for each trigger |
| `ReconcileExecutor` impl | How to apply each resource type |
| `GatewayConfig` and route config types | Data plane configuration schema |
| `DataPlaneNames` | Naming conventions |
| Controller wiring (watches, owned resources) | Gateway API resource graph |
| Error types | Domain error variants |

## Dependency Graph

```
kube-sans-io (new crate)
├── async-trait     (for dyn-safe Executor trait)
├── jiff            (for Timestamp in Snapshot trait)
├── kube            (for ResourceExt in snapshot helpers, Action in executor)
├── k8s-openapi     (only if snapshot helpers are included)
├── tracing         (for event logging in executor)
└── serde           (optional, for ResourceDelete serialization)

controlplane (existing)
├── kube-sans-io    (new dependency)
├── gateway-crds    (unchanged)
├── kube            (unchanged)
├── k8s-openapi     (unchanged)
└── ...             (unchanged)
```

The new crate would be lightweight: ~6 dependencies, no Gateway API types.

## Concrete Migration Steps

### Phase 1: Create `crates/kube-sans-io` with types only

1. `cargo init crates/kube-sans-io --lib`
2. Add to workspace members
3. Move these types (copy, then delete originals):
   - `RequeueDecision` → `kube_sans_io::result::RequeueDecision`
   - `ReconcileEvent` → `kube_sans_io::result::ReconcileEvent`
   - `EventSeverity` → `kube_sans_io::result::EventSeverity`
   - `ResourceDelete` → `kube_sans_io::result::ResourceDelete`
4. Create `ReconcileResult<U, S>` generic struct
5. Implement generic methods (merge, requeue, events, has_*)
6. Add `Snapshot` trait
7. Add `NamespacedStore<T>` / `ClusterStore<T>` type aliases and helpers

**Estimated effort:** ~200 lines of new code, mostly reshuffled from existing.

### Phase 2: Adapt controlplane to use generic types

1. Add `kube-sans-io` dependency to `crates/controlplane/Cargo.toml`
2. Define domain types:
   ```rust
   // crates/controlplane/src/core/result.rs
   use kube_sans_io::result::ReconcileResult as GenericResult;

   pub type ReconcileResult = GenericResult<ResourceUpsert, StatusUpdate>;
   ```
3. Keep `ResourceUpsert` and `StatusUpdate` enums in controlplane
4. Add convenience methods as extension trait or inherent methods on the type alias
5. Replace `WorldSnapshot` internals to use `NamespacedStore<T>` / `ClusterStore<T>`
6. Impl `Snapshot` for `WorldSnapshot`

**Key decision:** Type alias vs. newtype wrapper for `ReconcileResult`.

- **Type alias** (`type ReconcileResult = GenericResult<ResourceUpsert, StatusUpdate>`):
  Simpler, but you can't add inherent methods to a type alias. You'd need
  extension traits for `upsert_deployment()` etc.
- **Newtype** (`struct ReconcileResult(GenericResult<ResourceUpsert, StatusUpdate>)`):
  Allows inherent methods, but requires forwarding generic methods or Deref.

Recommendation: **Type alias + extension trait.** The extension trait pattern
is idiomatic Rust and keeps the generic/specific boundary clear:

```rust
// controlplane/src/core/result.rs

use kube_sans_io::result::ReconcileResult as BaseResult;

pub type ReconcileResult = BaseResult<ResourceUpsert, StatusUpdate>;

pub trait ReconcileResultExt {
    fn upsert_deployment(self, deployment: Deployment) -> Self;
    fn upsert_service(self, service: Service) -> Self;
    fn update_gateway_status(self, ns: String, name: String, status: GatewayStatus) -> Self;
    fn deployment_upserts(&self) -> Vec<&Deployment>;
    // ...
}

impl ReconcileResultExt for ReconcileResult {
    fn upsert_deployment(mut self, deployment: Deployment) -> Self {
        self.upserts.push(ResourceUpsert::Deployment(Box::new(deployment)));
        self
    }
    // ...
}
```

**Estimated effort:** ~100 lines changed across existing files, mostly
mechanical import path changes. Zero behavioral changes.

### Phase 3: Impl `Executor` for `ReconcileExecutor`, inject via `dyn`

1. Impl `kube_sans_io::Executor` for the existing `ReconcileExecutor`:
   ```rust
   #[async_trait]
   impl Executor for ReconcileExecutor {
       type Upsert = ResourceUpsert;
       type StatusUpdate = StatusUpdate;
       type Error = ControllerError;

       async fn execute_upsert(&self, upsert: &ResourceUpsert) -> Result<(), ControllerError> {
           match upsert {
               ResourceUpsert::Deployment(d) => self.upsert_deployment(d).await,
               ResourceUpsert::Service(s) => self.upsert_service(s).await,
               ResourceUpsert::ConfigMap(cm) => self.upsert_configmap(cm).await,
               ResourceUpsert::ServiceAccount(sa) => self.upsert_serviceaccount(sa).await,
               ResourceUpsert::Role(r) => self.upsert_role(r).await,
               ResourceUpsert::RoleBinding(rb) => self.upsert_rolebinding(rb).await,
           }
       }
       // ... similarly for execute_status_update and execute_delete
   }
   ```

2. Add a type alias for the trait object:
   ```rust
   pub type DynExecutor = dyn Executor<
       Upsert = ResourceUpsert,
       StatusUpdate = StatusUpdate,
       Error = ControllerError,
   >;
   ```

3. Change `ControllerContext` to hold `Box<DynExecutor>`:
   ```rust
   pub struct ControllerContext {
       pub client: Client,
       pub config: ControllerConfig,
       pub executor: Box<DynExecutor>,
   }
   ```

4. Replace the body of each controller's reconcile function to call
   `kube_sans_io::execute(result, ctx.executor.as_ref()).await`

5. In production startup:
   ```rust
   let executor = Box::new(ReconcileExecutor::new(client.clone()));
   let context = ControllerContext { client, config, executor };
   ```

6. In tests:
   ```rust
   use kube_sans_io::testing::RecordingExecutor;

   let executor = Box::new(RecordingExecutor::new());
   let context = ControllerContext { client, config, executor };

   // ... run reconcile ...

   // Assert on what was recorded
   assert_eq!(executor.upserts().len(), 3);
   ```

**Estimated effort:** ~200 lines of trait impls + context changes.

### Phase 4: Tests and documentation

1. Verify all existing tests still pass (`cargo make dev-test-flow`)
2. Add unit tests for generic types in the new crate
3. Add doc examples showing how a hypothetical non-Gateway controller would use it

**Estimated effort:** ~100 lines of tests and docs.

## Total Estimated Effort

| Phase | New lines | Changed lines | Risk |
|-------|-----------|---------------|------|
| Phase 1: Create crate with types | ~200 | 0 | Low — additive only |
| Phase 2: Adapt controlplane | ~50 | ~100 | Low — mechanical imports |
| Phase 3: Executor trait + dyn injection | ~200 | ~80 | Medium — async trait boundaries, ControllerContext refactor |
| Phase 4: Tests + docs + built-in fakes | ~200 | 0 | Low |
| **Total** | **~650** | **~180** | |

The migration is low-risk because each phase is independently shippable and
testable. Phase 1 doesn't touch existing code at all. Phases 2-3 are pure
refactors — every test should pass identically before and after.

Phase 3 is the riskiest step because it changes `ControllerContext` from
owning a concrete `ReconcileExecutor` to holding a `Box<dyn Executor<...>>`.
This is a structural change that touches every controller file. But the
behavioral contract is identical — the `dyn` dispatch just adds an indirection
layer. All existing tests exercise the same code paths.

## Design Decisions

### 1. Executor: `#[async_trait]` for dyn dispatch (decided)

We require `dyn Executor` for test fakes. Native `async fn` in traits (Rust
2024 edition) produces RPITIT types that are not object-safe. `#[async_trait]`
desugars to `-> Pin<Box<dyn Future + Send + '_>>`, making the trait dyn-safe.

The boxing overhead is irrelevant — executor methods call Kubernetes APIs that
take milliseconds.

An alternative: `trait_variant::make(SendExecutor: Send)` from the Rust
project, which generates a separate dyn-safe variant. This avoids boxing in
the static-dispatch case but adds a second trait to understand. Not worth the
complexity — `#[async_trait]` is universally understood in the Rust ecosystem.

### 2. One `Executor` trait, not three (decided)

Earlier drafts split into `UpsertExecutor` + `StatusExecutor` + `DeleteExecutor`.
This fails in practice: you cannot write `dyn UpsertExecutor<...> + StatusExecutor<...>`
when the traits have different associated types. A single `Executor` trait with
associated types for `Upsert`, `StatusUpdate`, and `Error` is both simpler and
dyn-compatible.

### 3. Associated types, not type parameters (decided)

Each executor implementation works with exactly one set of resource types, so
associated types (output types) are correct. Type parameters (input types)
would force callers to specify them at every call site. For dyn dispatch, the
associated types are specified once in a type alias:
```rust
type DynExecutor = dyn Executor<
    Upsert = ResourceUpsert,
    StatusUpdate = StatusUpdate,
    Error = ControllerError,
>;
```

### 4. Ship built-in test fakes (decided)

`RecordingExecutor` and `FailingExecutor` ship in a `kube_sans_io::testing`
module. This makes the "happy path" for testing zero-effort: controllers don't
need to write their own fakes to test the orchestration layer.

### 5. Should the crate re-export `kube::runtime::controller::Action`?

Yes. The crate depends on `kube` for `ResourceExt` and `Action`. Controllers
using this crate will already depend on `kube`. Fighting the dependency adds
indirection without value.

### 6. Should `Snapshot` be a trait or a convention?

**Keep the trait.** It's cheap (just `fn now()`), enables generic functions,
and makes the contract explicit. A `#[derive(Snapshot)]` proc macro could
auto-impl it for structs with `pub now: Timestamp`, but that's a nice-to-have.

### 7. Crate name

Options:
- `kube-sans-io` — descriptive, references the sans-I/O pattern
- `kube-functional-core` — references the "Functional Core, Imperative Shell" pattern
- `kube-reconcile-core` — more conservative naming

Recommendation: **`kube-sans-io`** — it's distinctive and directly references
the pattern.

## What This Does NOT Solve

1. **Watch topology.** Which resources to watch and how changes propagate between
   controllers is inherently domain-specific. The `Controller::new().owns().watches()`
   setup stays in each controller.

2. **Snapshot fetching.** What to fetch from the API server for each reconciliation
   trigger is domain-specific. The `SnapshotFetcher` stays in each controller.
   Unlike the executor, the fetcher's method signatures vary per resource type
   (`snapshot_for_gateway(ns, name)` vs. `snapshot_for_gateway_class(name)`),
   making a useful generic trait difficult. For testing the fetcher layer, use
   kube's built-in `Client::try_from()` with a mock Tower service, or bypass
   the fetcher entirely and construct snapshots directly (which is what the
   pure-core tests already do).

3. **Domain validation.** All validation logic (listener rules, parent ref checks,
   backend resolution) is domain-specific.

4. **Resource construction.** Building the specific child resources (Deployments,
   Services, ConfigMaps with specific content) is domain-specific.

The crate provides the *skeleton* — the types and orchestration — not the
*muscles*. And that's the right boundary: the skeleton is what's hard to design
correctly (status-before-resources ordering, requeue merging, time injection),
while the muscles are straightforward once you have the skeleton.

### Testing summary: what's fakeable at each layer

```
Layer               Fake mechanism                  Provided by
─────────────────── ─────────────────────────────── ──────────────
Pure core           No fake needed — inspect         (inherent to
                    ReconcileResult directly          the pattern)

Executor            dyn Executor with                kube-sans-io
                    RecordingExecutor /               ::testing
                    FailingExecutor

Fetcher             Construct WorldSnapshot          Domain crate
                    directly via builder             (already works)

K8s API client      kube::Client with mock           kube crate
                    Tower service layer
```
