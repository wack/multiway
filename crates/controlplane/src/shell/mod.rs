//! Imperative shell for the control plane.
//!
//! This module handles all I/O operations for the control plane:
//!
//! - **Fetcher**: Builds [`WorldSnapshot`]s from the Kubernetes API
//! - **Executor**: Applies [`ReconcileResult`]s to the cluster
//!
//! The shell is the only part of the control plane that performs I/O.
//! The functional core (in `crate::core`) is pure and testable.

pub mod executor;
pub mod fetcher;

pub use executor::ReconcileExecutor;
pub use fetcher::SnapshotFetcher;
