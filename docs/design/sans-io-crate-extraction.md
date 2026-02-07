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

use chrono::{DateTime, Utc};

/// A point-in-time snapshot of cluster state.
///
/// Implementors populate this with whatever resource types their
/// controller needs. The only requirement is an injected timestamp
/// for deterministic testing.
pub trait Snapshot {
    /// The time this snapshot was taken.
    /// Inject a fixed value in tests for deterministic timestamps.
    fn now(&self) -> DateTime<Utc>;
}
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

### 4. Executor scaffolding (`shell/executor.rs`)

The execution *orchestration* is generic — the order of operations and error
handling — but the type dispatch is domain-specific. The crate would provide:

```rust
// kube_sans_io::executor

use std::future::Future;
use std::time::Duration;
use kube::runtime::controller::Action;

/// Trait for executing a domain-specific upsert.
pub trait UpsertExecutor {
    type Upsert: std::fmt::Debug + Clone;
    type Error: std::error::Error;

    fn execute_upsert(
        &self,
        upsert: &Self::Upsert,
    ) -> impl Future<Output = Result<(), Self::Error>>;
}

/// Trait for executing a domain-specific status update.
pub trait StatusExecutor {
    type StatusUpdate: std::fmt::Debug + Clone;
    type Error: std::error::Error;

    fn execute_status_update(
        &self,
        update: &Self::StatusUpdate,
    ) -> impl Future<Output = Result<(), Self::Error>>;
}

/// Trait for executing a resource delete.
pub trait DeleteExecutor {
    type Error: std::error::Error;

    fn execute_delete(
        &self,
        delete: &ResourceDelete,
    ) -> impl Future<Output = Result<(), Self::Error>>;
}

/// Execute a ReconcileResult with the standard orchestration:
/// 1. Log events
/// 2. Execute status updates FIRST (for observedGeneration correctness)
/// 3. Execute upserts
/// 4. Execute deletes
/// 5. Convert RequeueDecision to Action
pub async fn execute<U, S, E>(
    result: ReconcileResult<U, S>,
    executor: &(impl UpsertExecutor<Upsert = U, Error = E>
              + StatusExecutor<StatusUpdate = S, Error = E>
              + DeleteExecutor<Error = E>),
) -> Result<Action, E>
where
    U: std::fmt::Debug + Clone,
    S: std::fmt::Debug + Clone,
    E: std::error::Error,
{
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

    // Status updates FIRST
    for update in &result.status_updates {
        executor.execute_status_update(update).await?;
    }

    // Upserts
    for upsert in &result.upserts {
        executor.execute_upsert(upsert).await?;
    }

    // Deletes (continue on error)
    for delete in &result.deletes {
        if let Err(e) = executor.execute_delete(delete).await {
            tracing::error!(error = %e, "Failed to execute delete");
        }
    }

    // Convert requeue
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

### 5. The bridge pattern

The three-line pattern (fetch → compute → execute) could be a provided function:

```rust
// kube_sans_io::reconcile

use std::future::Future;

/// Run the standard sans-I/O reconciliation loop:
/// 1. Build a snapshot (effectful)
/// 2. Run pure reconciliation logic (no I/O)
/// 3. Execute the result (effectful)
pub async fn reconcile<Snap, U, S, E, Fut1, Fut2>(
    fetch_snapshot: impl FnOnce() -> Fut1,
    reconcile_fn: impl FnOnce(&Snap) -> ReconcileResult<U, S>,
    execute_fn: impl FnOnce(ReconcileResult<U, S>) -> Fut2,
) -> Result<Action, E>
where
    Snap: Snapshot,
    U: std::fmt::Debug + Clone,
    S: std::fmt::Debug + Clone,
    E: std::error::Error,
    Fut1: Future<Output = Result<Snap, E>>,
    Fut2: Future<Output = Result<Action, E>>,
{
    let snapshot = fetch_snapshot().await?;
    let result = reconcile_fn(&snapshot);
    execute_fn(result).await
}
```

This is arguably over-abstracted for three lines of code — the real value is
in documenting the pattern, not saving keystrokes. It might be better as a
documented example than a function.

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
├── chrono          (for DateTime<Utc> in Snapshot trait)
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

The new crate would be lightweight: ~5 dependencies, no Gateway API types.

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

### Phase 3: Add executor traits and generic execute function

1. Add `UpsertExecutor`, `StatusExecutor`, `DeleteExecutor` traits
2. Add the generic `execute()` function
3. Impl the traits for controlplane's `ReconcileExecutor`
4. Replace the body of `ReconcileExecutor::execute()` with a call to the
   generic function

**Estimated effort:** ~150 lines of trait definitions + impls.

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
| Phase 3: Executor traits | ~150 | ~50 | Medium — async trait boundaries |
| Phase 4: Tests + docs | ~100 | 0 | Low |
| **Total** | **~500** | **~150** | |

The migration is low-risk because each phase is independently shippable and
testable. Phase 1 doesn't touch existing code at all. Phases 2-3 are pure
refactors — every test should pass identically before and after.

## Design Decisions to Make

### 1. How generic should the executor be?

**Option A: Trait-based (proposed above).** Controllers impl `UpsertExecutor`
for their types. The generic `execute()` function orchestrates.

**Option B: Callback-based.** The generic `execute()` takes closures:
```rust
pub async fn execute<U, S>(
    result: ReconcileResult<U, S>,
    on_upsert: impl AsyncFn(&U) -> Result<()>,
    on_status: impl AsyncFn(&S) -> Result<()>,
    on_delete: impl AsyncFn(&ResourceDelete) -> Result<()>,
) -> Result<Action>
```

Option B is simpler but doesn't compose as well. Recommendation: **Option A**
(traits), because it lets controllers build a single `Executor` struct that
holds the `Client` and implements all three traits.

### 2. Should the crate re-export `kube::runtime::controller::Action`?

Probably yes, since `execute()` returns it. But this creates a coupling to the
specific `kube` version. Alternatively, return a custom `RequeueAction` and let
the caller convert.

Recommendation: **Return `Action` directly.** The crate already depends on
`kube` for `ResourceExt` in snapshot helpers. Fighting the dependency isn't
worth it — controllers using this crate will already depend on `kube`.

### 3. Should `Snapshot` be a trait or a convention?

The trait is very thin (just `fn now()`). An alternative is to not have a trait
at all and just document "your snapshot struct should have a `now` field."

Recommendation: **Keep the trait.** It's cheap, enables generic functions that
accept any snapshot, and makes the contract explicit. A `#[derive(Snapshot)]`
macro could auto-impl it for structs with a `pub now: DateTime<Utc>` field.

### 4. Crate name

Options:
- `kube-sans-io` — descriptive, references the sans-I/O pattern
- `kube-functional-core` — references the "Functional Core, Imperative Shell" pattern
- `kube-reconcile-core` — more conservative naming
- `kube-effects` — focuses on the "effects as data" pattern

Recommendation: **`kube-sans-io`** — it's distinctive and directly references
the architectural pattern that makes this approach valuable.

## What This Does NOT Solve

1. **Watch topology.** Which resources to watch and how changes propagate between
   controllers is inherently domain-specific. The `Controller::new().owns().watches()`
   setup stays in each controller.

2. **Snapshot fetching.** What to fetch from the API server for each reconciliation
   trigger is domain-specific. The `SnapshotFetcher` stays in each controller.

3. **Domain validation.** All validation logic (listener rules, parent ref checks,
   backend resolution) is domain-specific.

4. **Resource construction.** Building the specific child resources (Deployments,
   Services, ConfigMaps with specific content) is domain-specific.

The crate provides the *skeleton* — the types and orchestration — not the
*muscles*. And that's the right boundary: the skeleton is what's hard to design
correctly (status-before-resources ordering, requeue merging, time injection),
while the muscles are straightforward once you have the skeleton.
