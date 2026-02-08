# Linear Tickets: `kube-sans-io` Crate Extraction

> Structured ticket content for tracking the extraction of the sans-I/O
> controller pattern into a standalone crate. See
> [sans-io-crate-extraction.md](./sans-io-crate-extraction.md) for the full
> design document.
>
> **Project:** Helm Support

---

## Parent Issue

**Title:** Extract `kube-sans-io` crate from controlplane

**Description:**

Extract the generalizable "Functional Core, Imperative Shell" controller
pattern from `crates/controlplane` into a standalone `crates/kube-sans-io`
crate that any Kubernetes controller can use to separate effectful operations
from domain logic.

The new crate provides:
- `ReconcileResult<U, S>` — generic effects-as-data struct parameterized over domain types
- `Snapshot` trait — injected `jiff::Timestamp` for deterministic testing
- `NamespacedStore<T>` / `ClusterStore<T>` — common resource storage patterns
- `Executor` trait (`#[async_trait]`, dyn-safe) — apply effects with pluggable backends
- `execute()` orchestration function — correct ordering (status → upserts → deletes)
- Built-in test fakes — `RecordingExecutor`, `FailingExecutor`

Design doc: `docs/design/sans-io-crate-extraction.md`

**Acceptance Criteria:**

- [ ] `crates/kube-sans-io` exists as a workspace member with its own `Cargo.toml`
- [ ] Generic `ReconcileResult<U, S>`, `Snapshot` trait, `Executor` trait, and built-in test fakes are implemented
- [ ] `crates/controlplane` depends on `kube-sans-io` and uses its types (no duplication)
- [ ] `ControllerContext` holds a `Box<dyn Executor<...>>` for dependency injection
- [ ] All existing tests pass: `cargo make dev-test-flow`
- [ ] `cargo make fmt` and `cargo make clippy-flow` pass with zero warnings
- [ ] Design doc at `docs/design/sans-io-crate-extraction.md` is kept up to date
- [ ] Crate has its own unit tests and doc examples

---

## Sub-issue 1: Create `crates/kube-sans-io` with generic types (Phase 1)

**Title:** Create `kube-sans-io` crate with generic `ReconcileResult`, `Snapshot` trait, and store types

**Description:**

Initialize the new crate and implement the generic types that form the
skeleton of the sans-I/O pattern. This phase is purely additive — no
existing code is modified.

Types to create:
- `ReconcileResult<U, S>` generic struct with `merge()`, `with_requeue()`, `emit_event()`, `has_upserts()`, `has_deletes()`, `has_status_updates()`, `delete_resource()` methods
- `RequeueDecision` enum (`After` / `OnError` / `Never`) with `min()` merging logic
- `ReconcileEvent` and `EventSeverity` types
- `ResourceDelete` type (already uses generic strings)
- `Snapshot` trait with `fn now(&self) -> jiff::Timestamp`
- `NamespacedStore<T>` / `ClusterStore<T>` type aliases with `insert_namespaced()`, `insert_cluster_scoped()`, `get_namespaced()` helpers

Reference: `docs/design/sans-io-crate-extraction.md` §1–3, Phase 1.

Estimated effort: ~200 new lines.

**Acceptance Criteria:**

- [ ] `cargo init crates/kube-sans-io --lib` with workspace membership in root `Cargo.toml`
- [ ] `ReconcileResult<U, S>` struct with all generic methods listed in the design doc
- [ ] `RequeueDecision` enum with `min()` merging — unit test: `min(After(5s), After(10s)) == After(5s)`, `min(Never, After(5s)) == After(5s)`, `min(OnError(_), After(_)) == After(_)`
- [ ] `ReconcileEvent`, `EventSeverity`, `ResourceDelete` types implemented
- [ ] `Snapshot` trait with `fn now(&self) -> jiff::Timestamp`
- [ ] `NamespacedStore<T>` / `ClusterStore<T>` type aliases and helpers (`insert_namespaced`, `insert_cluster_scoped`, `get_namespaced`)
- [ ] Unit tests for `ReconcileResult::merge()` (merges upserts, status updates, deletes, events; picks minimum requeue)
- [ ] Unit tests for snapshot store helpers (insert + retrieve, missing key returns `None`)
- [ ] `cargo make fmt` passes
- [ ] `cargo make clippy-flow` passes with zero warnings
- [ ] No changes to `crates/controlplane` in this phase

---

## Sub-issue 2: Adapt controlplane to import `kube-sans-io` generic types (Phase 2)

**Title:** Replace controlplane's `ReconcileResult` internals with `kube-sans-io` generic types

**Description:**

Wire `crates/controlplane` to depend on `kube-sans-io` and replace its own
`ReconcileResult`, `RequeueDecision`, `ReconcileEvent`, `EventSeverity`, and
`ResourceDelete` with imports from the new crate. Define a type alias and
extension trait for domain-specific convenience methods. Update
`WorldSnapshot` to use `NamespacedStore<T>` / `ClusterStore<T>` and impl
`Snapshot`.

This is a pure mechanical refactor — zero behavioral changes.

Key pattern:
```rust
use kube_sans_io::result::ReconcileResult as BaseResult;

pub type ReconcileResult = BaseResult<ResourceUpsert, StatusUpdate>;

pub trait ReconcileResultExt {
    fn upsert_deployment(self, deployment: Deployment) -> Self;
    fn upsert_service(self, service: Service) -> Self;
    // ...
}
```

Reference: `docs/design/sans-io-crate-extraction.md` §Phase 2.

Estimated effort: ~50 new lines, ~100 changed lines.

**Acceptance Criteria:**

- [ ] `kube-sans-io` added as dependency in `crates/controlplane/Cargo.toml`
- [ ] `ReconcileResult` is a type alias: `type ReconcileResult = kube_sans_io::ReconcileResult<ResourceUpsert, StatusUpdate>`
- [ ] `ReconcileResultExt` extension trait provides domain-specific convenience methods (`upsert_deployment`, `upsert_service`, `update_gateway_status`, etc.)
- [ ] `RequeueDecision`, `ReconcileEvent`, `EventSeverity`, `ResourceDelete` imported from `kube-sans-io` (originals deleted from controlplane)
- [ ] `WorldSnapshot` fields use `NamespacedStore<T>` / `ClusterStore<T>` from `kube-sans-io`
- [ ] `WorldSnapshot` implements `kube_sans_io::Snapshot` trait
- [ ] All existing tests pass unchanged: `cargo nextest run` in `crates/controlplane` — same test cases, same assertions, zero modifications to test files
- [ ] `cargo make fmt` passes
- [ ] `cargo make clippy-flow` passes with zero warnings
- [ ] Zero behavioral changes — pure mechanical refactor verified by identical test results

---

## Sub-issue 3: Implement `Executor` trait with dyn dispatch (Phase 3)

**Title:** Implement `Executor` trait, inject `Box<dyn Executor>` into `ControllerContext`

**Description:**

Define the `kube_sans_io::Executor` trait (using `#[async_trait]` for dyn
safety) and the generic `execute()` orchestration function. Implement the
trait for the existing `ReconcileExecutor`. Refactor `ControllerContext` to
hold `Box<dyn Executor<...>>` for dependency injection. Replace each
controller's manual execute call with `kube_sans_io::execute(result,
ctx.executor.as_ref()).await`.

This is the highest-risk phase because it changes `ControllerContext` from
owning a concrete `ReconcileExecutor` to holding a `Box<dyn Executor<...>>`,
touching every controller file. However the behavioral contract is identical.

Reference: `docs/design/sans-io-crate-extraction.md` §4, Phase 3.

Estimated effort: ~200 new lines, ~80 changed lines.

**Acceptance Criteria:**

- [ ] `Executor` trait defined in `kube-sans-io` with `#[async_trait]`:
  - Associated types: `Upsert`, `StatusUpdate`, `Error`
  - Methods: `execute_upsert()`, `execute_status_update()`, `execute_delete()`
- [ ] Generic `kube_sans_io::execute()` function with correct orchestration order: events → status updates → upserts → deletes
- [ ] `requeue_to_action()` helper converts `RequeueDecision` to `kube::runtime::controller::Action`
- [ ] `ReconcileExecutor` implements `Executor` trait (match on `ResourceUpsert` / `StatusUpdate` variants)
- [ ] `DynExecutor` type alias: `dyn Executor<Upsert=ResourceUpsert, StatusUpdate=StatusUpdate, Error=ControllerError>`
- [ ] `ControllerContext` holds `executor: Box<DynExecutor>` field
- [ ] `reconcile_gateway()`, `reconcile_gateway_class()`, `reconcile_httproute()` all call `kube_sans_io::execute(result, ctx.executor.as_ref()).await`
- [ ] Status-before-upserts ordering verified with a test using `RecordingExecutor` (from Phase 4, or a temporary inline fake)
- [ ] All existing tests pass: `cargo nextest run`
- [ ] `cargo make fmt` passes
- [ ] `cargo make clippy-flow` passes with zero warnings

---

## Sub-issue 4: Built-in test fakes, documentation, and crate tests (Phase 4)

**Title:** Add `RecordingExecutor`, `FailingExecutor`, doc examples, and comprehensive tests

**Description:**

Add `RecordingExecutor` and `FailingExecutor` to `kube_sans_io::testing`.
Add comprehensive unit tests for the new crate. Add doc examples showing
usage by a hypothetical non-Gateway controller. Optionally migrate at least
one existing controlplane test to use `RecordingExecutor` as proof of
concept.

Reference: `docs/design/sans-io-crate-extraction.md` §5, Phase 4.

Estimated effort: ~200 new lines.

**Acceptance Criteria:**

- [ ] `RecordingExecutor<U, S>` in `kube_sans_io::testing`:
  - Records all upserts, status updates, and deletes via `Mutex<Vec<_>>`
  - Accessor methods: `upserts()`, `status_updates()`, `deletes()`
  - Implements `Executor` with `#[async_trait]`; error type is `std::convert::Infallible`
- [ ] `FailingExecutor<U, S>` in `kube_sans_io::testing`:
  - Wraps `RecordingExecutor` and fails after N operations
  - Configurable via `FailingExecutor::new(fail_after: usize)`
  - Implements `Executor` with `#[async_trait]`
- [ ] Unit tests for `RecordingExecutor`: captures upserts, status updates, deletes correctly
- [ ] Unit tests for `FailingExecutor`: succeeds for first N operations, fails on N+1
- [ ] Unit tests for `execute()` orchestration:
  - Status updates execute before upserts (use `RecordingExecutor` to verify order)
  - Delete errors are logged but don't abort remaining deletes
  - `RequeueDecision` correctly maps to `Action`
- [ ] Doc examples on `Executor` trait, `ReconcileResult<U, S>`, `Snapshot` trait
- [ ] At least one integration-style test demonstrating fetch→compute→execute with `RecordingExecutor`
- [ ] At least one existing controlplane test migrated to use `RecordingExecutor` as proof of concept (optional but recommended)
- [ ] `cargo make dev-test-flow` passes (full suite: format, lint, build, test)
