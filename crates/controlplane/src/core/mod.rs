//! Functional core for the control plane.
//!
//! This module contains pure functions that implement the reconciliation logic
//! without any I/O operations. The core takes a [`WorldSnapshot`] as input and
//! produces a [`ReconcileResult`] describing the effects to perform.
//!
//! # Architecture
//!
//! The control plane follows a "Functional Core, Imperative Shell" pattern:
//!
//! 1. The **functional core** (this module) contains pure functions that:
//!    - Take a snapshot of cluster state as input
//!    - Compute what changes need to be made
//!    - Return effects as data (not executed)
//!
//! 2. The **imperative shell** (`crate::shell`) handles I/O:
//!    - Fetches resources from Kubernetes API
//!    - Builds WorldSnapshots
//!    - Executes ReconcileResults against the cluster
//!
//! # Benefits
//!
//! - **Fast tests**: Unit tests run in milliseconds without a cluster
//! - **Deterministic**: Injected time means reproducible tests
//! - **Spec traceable**: Every Gateway API requirement maps to a unit test

pub mod result;
pub mod snapshot;

mod reconcile;
mod validate;

pub use reconcile::{reconcile_gateway, reconcile_gateway_class, reconcile_httproute};
pub use result::{ReconcileResult, RequeueDecision, ResourceUpsert, StatusUpdate};
pub use snapshot::{WorldSnapshot, WorldSnapshotBuilder};
pub use validate::{
    ListenerValidation, ListenerValidationResult, validate_gateway_class, validate_listeners,
};
