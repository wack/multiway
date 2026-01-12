# Functional Core Control Plane Design

## Executive Summary

This document proposes a refactoring of the multiway control plane to follow a **"Functional Core, Imperative Shell"** architecture. The goal is to enable comprehensive unit testing of all Gateway API spec requirements without requiring a real Kubernetes cluster.

## Problem Statement

The current reconciler architecture tightly couples:
1. **Decision logic** (what should happen) with **execution** (making it happen)
2. **Pure computations** (validation, config building) with **effectful operations** (API calls)
3. **Time-dependent operations** (status timestamps) with business logic

This makes it impossible to unit test reconciliation decisions without either:
- Mocking the entire Kubernetes client (complex, brittle)
- Running integration tests against a real cluster (slow, ~10s+ per test)

### Current Architecture Pain Points

```
┌─────────────────────────────────────────────────────┐
│              reconcile_gateway()                     │
│  ┌───────────────────────────────────────────────┐  │
│  │  get_accepted_gateway_class()  ← API call     │  │
│  │  validate_listeners()          ← pure         │  │
│  │  build_gateway_config()        ← pure         │  │
│  │  ensure_configmap()            ← API call     │  │
│  │  ensure_service()              ← API call     │  │
│  │  ensure_deployment()           ← API call     │  │
│  │  update_gateway_status()       ← API call     │  │
│  └───────────────────────────────────────────────┘  │
│         Pure logic mixed with effects               │
└─────────────────────────────────────────────────────┘
```

## Proposed Architecture

### Core Principle: Effects as Data

Instead of performing effects directly, the reconciler computes a **ReconcileResult** that describes what should happen. The imperative shell then executes these effects.

```
┌─────────────────────────────────────────────────────┐
│                 Functional Core                      │
│           (Pure functions, no I/O)                   │
│  ┌───────────────────────────────────────────────┐  │
│  │  Input: WorldSnapshot                         │  │
│  │    - GatewayClasses                           │  │
│  │    - Gateways                                 │  │
│  │    - HTTPRoutes                               │  │
│  │    - Services                                 │  │
│  │    - ConfigMaps                               │  │
│  │    - ReferenceGrants                          │  │
│  │    - Current time                             │  │
│  │                                               │  │
│  │  Output: ReconcileResult                      │  │
│  │    - Resources to create/update/delete        │  │
│  │    - Status updates                           │  │
│  │    - Requeue decision                         │  │
│  └───────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────┐
│                 Imperative Shell                     │
│           (Executes effects)                         │
│  ┌───────────────────────────────────────────────┐  │
│  │  - Fetches resources from Kubernetes API      │  │
│  │  - Builds WorldSnapshot                       │  │
│  │  - Calls functional core                      │  │
│  │  - Applies ReconcileResult to cluster         │  │
│  └───────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
```

## Detailed Design

### 1. World Snapshot

The `WorldSnapshot` captures all cluster state needed for reconciliation decisions.

```rust
//! crates/controlplane/src/core/snapshot.rs

use std::collections::BTreeMap;
use chrono::{DateTime, Utc};
use gateway_crds::{Gateway, GatewayClass, HTTPRoute, ReferenceGrant};
use k8s_openapi::api::core::v1::{ConfigMap, Service};

/// A point-in-time snapshot of relevant cluster state.
/// This is the sole input to the functional reconciliation core.
#[derive(Debug, Clone)]
pub struct WorldSnapshot {
    /// Current timestamp (injected for deterministic testing)
    pub now: DateTime<Utc>,

    /// All GatewayClasses in the cluster
    /// Key: name (cluster-scoped)
    pub gateway_classes: BTreeMap<String, GatewayClass>,

    /// All Gateways we're watching
    /// Key: (namespace, name)
    pub gateways: BTreeMap<(String, String), Gateway>,

    /// All HTTPRoutes we're watching
    /// Key: (namespace, name)
    pub httproutes: BTreeMap<(String, String), HTTPRoute>,

    /// Services that backends reference
    /// Key: (namespace, name)
    pub services: BTreeMap<(String, String), Service>,

    /// ConfigMaps managed by our controller
    /// Key: (namespace, name)
    pub configmaps: BTreeMap<(String, String), ConfigMap>,

    /// ReferenceGrants for cross-namespace references
    /// Key: (namespace, name)
    pub reference_grants: BTreeMap<(String, String), ReferenceGrant>,
}

impl WorldSnapshot {
    /// Create an empty snapshot at the given time
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now,
            gateway_classes: BTreeMap::new(),
            gateways: BTreeMap::new(),
            httproutes: BTreeMap::new(),
            services: BTreeMap::new(),
            configmaps: BTreeMap::new(),
            reference_grants: BTreeMap::new(),
        }
    }

    /// Get a GatewayClass by name
    pub fn get_gateway_class(&self, name: &str) -> Option<&GatewayClass> {
        self.gateway_classes.get(name)
    }

    /// Get a Gateway by namespace and name
    pub fn get_gateway(&self, namespace: &str, name: &str) -> Option<&Gateway> {
        self.gateways.get(&(namespace.to_string(), name.to_string()))
    }

    /// Get all HTTPRoutes referencing a specific Gateway
    pub fn routes_for_gateway(&self, gateway_ns: &str, gateway_name: &str) -> Vec<&HTTPRoute> {
        self.httproutes
            .values()
            .filter(|route| {
                route.spec.parent_refs.as_ref().map_or(false, |refs| {
                    refs.iter().any(|r| {
                        let ref_ns = r.namespace.as_deref().unwrap_or(
                            route.metadata.namespace.as_deref().unwrap_or_default()
                        );
                        ref_ns == gateway_ns && r.name == gateway_name
                    })
                })
            })
            .collect()
    }

    /// Check if a service exists
    pub fn service_exists(&self, namespace: &str, name: &str) -> bool {
        self.services.contains_key(&(namespace.to_string(), name.to_string()))
    }

    /// Check if a ReferenceGrant allows a cross-namespace reference
    pub fn is_reference_allowed(
        &self,
        from_ns: &str,
        from_kind: &str,
        to_ns: &str,
        to_kind: &str,
    ) -> bool {
        self.reference_grants
            .values()
            .filter(|grant| grant.metadata.namespace.as_deref() == Some(to_ns))
            .any(|grant| {
                let allows_from = grant.spec.from.iter().any(|f| {
                    f.namespace == from_ns && f.kind == from_kind
                });
                let allows_to = grant.spec.to.iter().any(|t| t.kind == to_kind);
                allows_from && allows_to
            })
    }
}

/// Builder for constructing test snapshots
#[derive(Default)]
pub struct WorldSnapshotBuilder {
    snapshot: WorldSnapshot,
}

impl WorldSnapshotBuilder {
    pub fn new() -> Self {
        Self {
            snapshot: WorldSnapshot::new(Utc::now()),
        }
    }

    pub fn at_time(mut self, time: DateTime<Utc>) -> Self {
        self.snapshot.now = time;
        self
    }

    pub fn with_gateway_class(mut self, gc: GatewayClass) -> Self {
        let name = gc.metadata.name.clone().unwrap_or_default();
        self.snapshot.gateway_classes.insert(name, gc);
        self
    }

    pub fn with_gateway(mut self, gw: Gateway) -> Self {
        let ns = gw.metadata.namespace.clone().unwrap_or_default();
        let name = gw.metadata.name.clone().unwrap_or_default();
        self.snapshot.gateways.insert((ns, name), gw);
        self
    }

    pub fn with_httproute(mut self, route: HTTPRoute) -> Self {
        let ns = route.metadata.namespace.clone().unwrap_or_default();
        let name = route.metadata.name.clone().unwrap_or_default();
        self.snapshot.httproutes.insert((ns, name), route);
        self
    }

    pub fn with_service(mut self, svc: Service) -> Self {
        let ns = svc.metadata.namespace.clone().unwrap_or_default();
        let name = svc.metadata.name.clone().unwrap_or_default();
        self.snapshot.services.insert((ns, name), svc);
        self
    }

    pub fn with_configmap(mut self, cm: ConfigMap) -> Self {
        let ns = cm.metadata.namespace.clone().unwrap_or_default();
        let name = cm.metadata.name.clone().unwrap_or_default();
        self.snapshot.configmaps.insert((ns, name), cm);
        self
    }

    pub fn with_reference_grant(mut self, grant: ReferenceGrant) -> Self {
        let ns = grant.metadata.namespace.clone().unwrap_or_default();
        let name = grant.metadata.name.clone().unwrap_or_default();
        self.snapshot.reference_grants.insert((ns, name), grant);
        self
    }

    pub fn build(self) -> WorldSnapshot {
        self.snapshot
    }
}
```

### 2. Reconcile Result (Effects as Data)

```rust
//! crates/controlplane/src/core/result.rs

use std::time::Duration;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Service};
use gateway_crds::{GatewayClassStatus, GatewayStatus, HttpRouteStatus};

/// The result of a reconciliation - describes effects to perform.
#[derive(Debug, Clone, Default)]
pub struct ReconcileResult {
    /// Resources to create or update
    pub upserts: Vec<ResourceUpsert>,

    /// Resources to delete
    pub deletes: Vec<ResourceDelete>,

    /// Status updates to apply
    pub status_updates: Vec<StatusUpdate>,

    /// When to requeue this reconciliation
    pub requeue: Option<RequeueDecision>,

    /// Events to emit (for debugging/observability)
    pub events: Vec<ReconcileEvent>,
}

impl ReconcileResult {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_requeue(mut self, after: Duration) -> Self {
        self.requeue = Some(RequeueDecision::After(after));
        self
    }

    pub fn with_requeue_on_error(mut self, after: Duration) -> Self {
        self.requeue = Some(RequeueDecision::OnError(after));
        self
    }

    pub fn upsert_deployment(mut self, deployment: Deployment) -> Self {
        self.upserts.push(ResourceUpsert::Deployment(deployment));
        self
    }

    pub fn upsert_service(mut self, service: Service) -> Self {
        self.upserts.push(ResourceUpsert::Service(service));
        self
    }

    pub fn upsert_configmap(mut self, configmap: ConfigMap) -> Self {
        self.upserts.push(ResourceUpsert::ConfigMap(configmap));
        self
    }

    pub fn update_gateway_status(
        mut self,
        namespace: String,
        name: String,
        status: GatewayStatus,
    ) -> Self {
        self.status_updates.push(StatusUpdate::Gateway {
            namespace,
            name,
            status,
        });
        self
    }

    pub fn update_gateway_class_status(
        mut self,
        name: String,
        status: GatewayClassStatus,
    ) -> Self {
        self.status_updates.push(StatusUpdate::GatewayClass { name, status });
        self
    }

    pub fn update_httproute_status(
        mut self,
        namespace: String,
        name: String,
        status: HttpRouteStatus,
    ) -> Self {
        self.status_updates.push(StatusUpdate::HTTPRoute {
            namespace,
            name,
            status,
        });
        self
    }

    pub fn emit_event(mut self, event: ReconcileEvent) -> Self {
        self.events.push(event);
        self
    }

    /// Merge another result into this one
    pub fn merge(mut self, other: ReconcileResult) -> Self {
        self.upserts.extend(other.upserts);
        self.deletes.extend(other.deletes);
        self.status_updates.extend(other.status_updates);
        self.events.extend(other.events);
        // Keep the shortest requeue time
        self.requeue = match (self.requeue, other.requeue) {
            (None, r) | (r, None) => r,
            (Some(a), Some(b)) => Some(a.min(b)),
        };
        self
    }
}

/// A resource to create or update
#[derive(Debug, Clone)]
pub enum ResourceUpsert {
    Deployment(Deployment),
    Service(Service),
    ConfigMap(ConfigMap),
}

/// A resource to delete
#[derive(Debug, Clone)]
pub struct ResourceDelete {
    pub api_version: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
}

/// A status update to apply
#[derive(Debug, Clone)]
pub enum StatusUpdate {
    GatewayClass {
        name: String,
        status: GatewayClassStatus,
    },
    Gateway {
        namespace: String,
        name: String,
        status: GatewayStatus,
    },
    HTTPRoute {
        namespace: String,
        name: String,
        status: HttpRouteStatus,
    },
}

/// When to requeue the reconciliation
#[derive(Debug, Clone, Copy)]
pub enum RequeueDecision {
    /// Requeue after a duration (normal case)
    After(Duration),
    /// Requeue after a duration (error case)
    OnError(Duration),
    /// Don't requeue (terminal state)
    Never,
}

impl RequeueDecision {
    fn min(self, other: Self) -> Self {
        use RequeueDecision::*;
        match (self, other) {
            (Never, r) | (r, Never) => r,
            (After(a), After(b)) => After(a.min(b)),
            (OnError(a), OnError(b)) => OnError(a.min(b)),
            (After(a), OnError(b)) | (OnError(b), After(a)) => {
                // Error requeues take precedence
                OnError(a.min(b))
            }
        }
    }
}

/// Events emitted during reconciliation (for observability)
#[derive(Debug, Clone)]
pub struct ReconcileEvent {
    pub severity: EventSeverity,
    pub reason: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy)]
pub enum EventSeverity {
    Normal,
    Warning,
}
```

### 3. Functional Core - The Reconcilers

```rust
//! crates/controlplane/src/core/reconcile.rs

use super::snapshot::WorldSnapshot;
use super::result::ReconcileResult;
use crate::controller::config::{ControllerConfig, CONTROLLER_NAME};

/// Pure reconciliation logic for GatewayClass resources.
pub fn reconcile_gateway_class(
    snapshot: &WorldSnapshot,
    config: &ControllerConfig,
    gateway_class_name: &str,
) -> ReconcileResult {
    let mut result = ReconcileResult::new();

    let gateway_class = match snapshot.get_gateway_class(gateway_class_name) {
        Some(gc) => gc,
        None => {
            // GatewayClass was deleted, nothing to do
            return result;
        }
    };

    // Check if this GatewayClass is for our controller
    if gateway_class.spec.controller_name != CONTROLLER_NAME {
        // Not our GatewayClass, requeue after a long interval
        return result.with_requeue(std::time::Duration::from_secs(3600));
    }

    // Validate the GatewayClass
    let (accepted, reason, message) = validate_gateway_class(gateway_class);

    // Build the status update
    let status = build_gateway_class_status(
        snapshot.now,
        gateway_class.metadata.generation,
        accepted,
        reason,
        &message,
    );

    result = result.update_gateway_class_status(
        gateway_class_name.to_string(),
        status,
    );

    result.with_requeue(std::time::Duration::from_secs(config.requeue_after_secs))
}

/// Pure reconciliation logic for Gateway resources.
pub fn reconcile_gateway(
    snapshot: &WorldSnapshot,
    config: &ControllerConfig,
    gateway_ns: &str,
    gateway_name: &str,
) -> ReconcileResult {
    let mut result = ReconcileResult::new();

    let gateway = match snapshot.get_gateway(gateway_ns, gateway_name) {
        Some(gw) => gw,
        None => return result, // Gateway was deleted
    };

    // Step 1: Check if the GatewayClass is accepted
    let gateway_class = match get_accepted_gateway_class(snapshot, &gateway.spec.gateway_class_name) {
        Some(gc) => gc,
        None => {
            // GatewayClass not found or not accepted
            let status = build_gateway_status_not_accepted(
                snapshot.now,
                gateway,
                "Invalid",
                "GatewayClass not found or not accepted by this controller",
            );
            return result
                .update_gateway_status(gateway_ns.to_string(), gateway_name.to_string(), status)
                .with_requeue(std::time::Duration::from_secs(30));
        }
    };

    // Step 2: Validate listeners
    let validation = validate_gateway_listeners(&gateway.spec.listeners);
    if !validation.all_valid {
        let status = build_gateway_status_not_accepted(
            snapshot.now,
            gateway,
            "ListenersNotValid",
            "One or more listeners have invalid configuration",
        );
        return result
            .update_gateway_status(gateway_ns.to_string(), gateway_name.to_string(), status)
            .with_requeue(std::time::Duration::from_secs(30));
    }

    // Step 3: Build data plane resources
    let names = DataPlaneNames::new(gateway_ns, gateway_name);

    // Build ConfigMap
    let gateway_config = build_gateway_config(gateway, gateway_ns);
    let configmap = build_configmap(&names, &gateway_config, gateway);
    result = result.upsert_configmap(configmap);

    // Build Service
    let service = build_service(&names, gateway);
    result = result.upsert_service(service);

    // Build Deployment
    let deployment = build_deployment(&names, gateway, config);
    result = result.upsert_deployment(deployment);

    // Step 4: Calculate routes attached to this Gateway
    let routes = snapshot.routes_for_gateway(gateway_ns, gateway_name);
    let attached_route_count = routes.len() as i32;

    // Step 5: Build success status
    // Note: In real implementation, we'd need the Service ClusterIP from the snapshot
    // For now, we'll leave addresses empty (the shell will fill it in after creation)
    let status = build_gateway_status_accepted(
        snapshot.now,
        gateway,
        attached_route_count,
        None, // addresses filled by shell after Service is created
    );

    result
        .update_gateway_status(gateway_ns.to_string(), gateway_name.to_string(), status)
        .with_requeue(std::time::Duration::from_secs(config.requeue_after_secs))
}

/// Pure reconciliation logic for HTTPRoute resources.
pub fn reconcile_httproute(
    snapshot: &WorldSnapshot,
    config: &ControllerConfig,
    route_ns: &str,
    route_name: &str,
) -> ReconcileResult {
    let mut result = ReconcileResult::new();

    let httproute = match snapshot.httproutes.get(&(route_ns.to_string(), route_name.to_string())) {
        Some(r) => r,
        None => return result, // Route was deleted
    };

    let parent_refs = httproute.spec.parent_refs.as_ref().cloned().unwrap_or_default();
    if parent_refs.is_empty() {
        return result.with_requeue(std::time::Duration::from_secs(config.requeue_after_secs));
    }

    let mut parent_statuses = Vec::new();

    for parent_ref in &parent_refs {
        let status = evaluate_parent_ref(snapshot, httproute, parent_ref, route_ns);
        parent_statuses.push(status);

        // If accepted, update the parent Gateway's ConfigMap
        if is_parent_accepted(&status) {
            if let Some(configmap_update) = build_route_configmap_update(
                snapshot,
                httproute,
                parent_ref,
                route_ns,
            ) {
                result = result.upsert_configmap(configmap_update);
            }
        }
    }

    let route_status = HttpRouteStatus { parents: parent_statuses };
    result
        .update_httproute_status(route_ns.to_string(), route_name.to_string(), route_status)
        .with_requeue(std::time::Duration::from_secs(config.requeue_after_secs))
}

// ============================================================================
// Helper functions (all pure)
// ============================================================================

fn get_accepted_gateway_class<'a>(
    snapshot: &'a WorldSnapshot,
    class_name: &str,
) -> Option<&'a GatewayClass> {
    snapshot.get_gateway_class(class_name).filter(|gc| {
        gc.spec.controller_name == CONTROLLER_NAME
            && gc.status.as_ref().map_or(false, |s| {
                s.conditions.as_ref().map_or(false, |conds| {
                    conds.iter().any(|c| c.type_ == "Accepted" && c.status == "True")
                })
            })
    })
}

fn validate_gateway_class(gc: &GatewayClass) -> (bool, &'static str, String) {
    // Check for unsupported parametersRef
    if let Some(params_ref) = &gc.spec.parameters_ref {
        // We accept but ignore parametersRef for now
        tracing::debug!(
            group = %params_ref.group,
            kind = %params_ref.kind,
            name = %params_ref.name,
            "GatewayClass has parametersRef (ignored)"
        );
    }

    (true, "Accepted", "GatewayClass is accepted by the multiway controller".to_string())
}

/// Validate all backend references in an HTTPRoute rule.
/// Returns validation errors for any invalid backends.
fn validate_route_backends(
    snapshot: &WorldSnapshot,
    route_ns: &str,
    rules: &[HttpRouteRules],
) -> Vec<BackendValidationError> {
    let mut errors = Vec::new();

    for rule in rules {
        if let Some(backends) = &rule.backend_refs {
            for backend in backends {
                let backend_ns = backend.namespace.as_deref().unwrap_or(route_ns);
                let backend_kind = backend.kind.as_deref().unwrap_or("Service");
                let backend_group = backend.group.as_deref().unwrap_or("");

                // Only Service backends are supported
                if !backend_group.is_empty() || backend_kind != "Service" {
                    errors.push(BackendValidationError::UnsupportedType {
                        group: backend_group.to_string(),
                        kind: backend_kind.to_string(),
                    });
                    continue;
                }

                // Check if service exists
                if !snapshot.service_exists(backend_ns, &backend.name) {
                    errors.push(BackendValidationError::NotFound {
                        namespace: backend_ns.to_string(),
                        name: backend.name.clone(),
                    });
                    continue;
                }

                // Check cross-namespace reference
                if backend_ns != route_ns {
                    if !snapshot.is_reference_allowed(
                        route_ns,
                        "HTTPRoute",
                        backend_ns,
                        "Service",
                    ) {
                        errors.push(BackendValidationError::CrossNamespaceNotAllowed {
                            from_ns: route_ns.to_string(),
                            to_ns: backend_ns.to_string(),
                            name: backend.name.clone(),
                        });
                    }
                }
            }
        }
    }

    errors
}

#[derive(Debug, Clone)]
enum BackendValidationError {
    UnsupportedType { group: String, kind: String },
    NotFound { namespace: String, name: String },
    CrossNamespaceNotAllowed { from_ns: String, to_ns: String, name: String },
}

// ... more helper functions for building resources, status, etc.
```

### 4. Imperative Shell

```rust
//! crates/controlplane/src/shell/executor.rs

use kube::{Api, Client};
use kube::api::{Patch, PatchParams, PostParams};
use std::sync::Arc;
use tokio::time::Duration;

use crate::core::result::{ReconcileResult, ResourceUpsert, StatusUpdate, RequeueDecision};
use crate::core::snapshot::{WorldSnapshot, WorldSnapshotBuilder};
use crate::controller::config::ControllerConfig;

/// The imperative shell that fetches state and executes effects.
pub struct ReconcileExecutor {
    client: Client,
    config: ControllerConfig,
}

impl ReconcileExecutor {
    pub fn new(client: Client, config: ControllerConfig) -> Self {
        Self { client, config }
    }

    /// Build a WorldSnapshot for a specific Gateway reconciliation.
    pub async fn snapshot_for_gateway(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<WorldSnapshot, kube::Error> {
        let mut builder = WorldSnapshotBuilder::new();

        // Fetch the Gateway
        let gw_api: Api<Gateway> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(gateway) = gw_api.get(name).await {
            let class_name = gateway.spec.gateway_class_name.clone();
            builder = builder.with_gateway(gateway);

            // Fetch the GatewayClass
            let gc_api: Api<GatewayClass> = Api::all(self.client.clone());
            if let Ok(gc) = gc_api.get(&class_name).await {
                builder = builder.with_gateway_class(gc);
            }
        }

        // Fetch related HTTPRoutes
        let route_api: Api<HTTPRoute> = Api::all(self.client.clone());
        let routes = route_api.list(&Default::default()).await?;
        for route in routes {
            if route_references_gateway(&route, namespace, name) {
                builder = builder.with_httproute(route);
            }
        }

        // Fetch existing managed ConfigMap
        let cm_api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let cm_name = format!("multiway-config-{}", name);
        if let Ok(cm) = cm_api.get(&cm_name).await {
            builder = builder.with_configmap(cm);
        }

        Ok(builder.build())
    }

    /// Execute a ReconcileResult against the cluster.
    pub async fn execute(&self, result: ReconcileResult) -> Result<kube::runtime::controller::Action, kube::Error> {
        // Execute upserts
        for upsert in result.upserts {
            match upsert {
                ResourceUpsert::Deployment(dep) => {
                    self.upsert_deployment(dep).await?;
                }
                ResourceUpsert::Service(svc) => {
                    self.upsert_service(svc).await?;
                }
                ResourceUpsert::ConfigMap(cm) => {
                    self.upsert_configmap(cm).await?;
                }
            }
        }

        // Execute deletes
        for delete in result.deletes {
            self.delete_resource(&delete).await?;
        }

        // Execute status updates
        for status_update in result.status_updates {
            match status_update {
                StatusUpdate::GatewayClass { name, status } => {
                    self.update_gateway_class_status(&name, status).await?;
                }
                StatusUpdate::Gateway { namespace, name, status } => {
                    self.update_gateway_status(&namespace, &name, status).await?;
                }
                StatusUpdate::HTTPRoute { namespace, name, status } => {
                    self.update_httproute_status(&namespace, &name, status).await?;
                }
            }
        }

        // Convert requeue decision to Action
        let action = match result.requeue {
            Some(RequeueDecision::After(d)) => kube::runtime::controller::Action::requeue(d),
            Some(RequeueDecision::OnError(d)) => kube::runtime::controller::Action::requeue(d),
            Some(RequeueDecision::Never) | None => {
                kube::runtime::controller::Action::requeue(Duration::from_secs(300))
            }
        };

        Ok(action)
    }

    async fn upsert_deployment(&self, deployment: Deployment) -> Result<(), kube::Error> {
        let ns = deployment.metadata.namespace.as_deref().unwrap_or("default");
        let name = deployment.metadata.name.as_deref().unwrap();
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), ns);

        api.patch(
            name,
            &PatchParams::apply("multiway-controller"),
            &Patch::Apply(&deployment),
        ).await?;

        Ok(())
    }

    async fn upsert_service(&self, service: Service) -> Result<(), kube::Error> {
        let ns = service.metadata.namespace.as_deref().unwrap_or("default");
        let name = service.metadata.name.as_deref().unwrap();
        let api: Api<Service> = Api::namespaced(self.client.clone(), ns);

        api.patch(
            name,
            &PatchParams::apply("multiway-controller"),
            &Patch::Apply(&service),
        ).await?;

        Ok(())
    }

    async fn upsert_configmap(&self, configmap: ConfigMap) -> Result<(), kube::Error> {
        let ns = configmap.metadata.namespace.as_deref().unwrap_or("default");
        let name = configmap.metadata.name.as_deref().unwrap();
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);

        api.patch(
            name,
            &PatchParams::apply("multiway-controller"),
            &Patch::Apply(&configmap),
        ).await?;

        Ok(())
    }

    async fn update_gateway_status(
        &self,
        namespace: &str,
        name: &str,
        status: GatewayStatus,
    ) -> Result<(), kube::Error> {
        let api: Api<Gateway> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({ "status": status });
        api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch)).await?;
        Ok(())
    }

    // ... other helper methods
}
```

### 5. Wiring It Together

```rust
//! crates/controlplane/src/controller/gateway.rs (refactored)

use std::sync::Arc;
use kube::runtime::controller::{Action, Controller};
use crate::core::{reconcile, snapshot::WorldSnapshot};
use crate::shell::executor::ReconcileExecutor;

pub async fn run_gateway_controller(ctx: Arc<ControllerContext>) {
    let executor = ReconcileExecutor::new(ctx.client.clone(), ctx.config.clone());

    let controller = Controller::new(gateways, WatcherConfig::default())
        .shutdown_on_signal()
        .run(
            |gateway, ctx| async move {
                let namespace = gateway.namespace().unwrap_or_default();
                let name = gateway.name_any();

                // Build snapshot (effectful)
                let snapshot = executor.snapshot_for_gateway(&namespace, &name).await?;

                // Compute result (pure)
                let result = reconcile::reconcile_gateway(&snapshot, &ctx.config, &namespace, &name);

                // Execute result (effectful)
                executor.execute(result).await
            },
            error_policy,
            ctx,
        );

    controller.await;
}
```

## Testing Strategy

### Unit Tests (Fast, Pure)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::snapshot::WorldSnapshotBuilder;
    use chrono::TimeZone;

    /// Spec: GatewayClass with matching controllerName should be accepted
    #[test]
    fn test_gateway_class_accepted_when_controller_matches() {
        let fixed_time = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let snapshot = WorldSnapshotBuilder::new()
            .at_time(fixed_time)
            .with_gateway_class(create_gateway_class("multiway", CONTROLLER_NAME))
            .build();

        let config = ControllerConfig::default();
        let result = reconcile_gateway_class(&snapshot, &config, "multiway");

        // Verify the status update
        assert_eq!(result.status_updates.len(), 1);
        match &result.status_updates[0] {
            StatusUpdate::GatewayClass { name, status } => {
                assert_eq!(name, "multiway");
                let conditions = status.conditions.as_ref().unwrap();
                let accepted = conditions.iter().find(|c| c.type_ == "Accepted").unwrap();
                assert_eq!(accepted.status, "True");
            }
            _ => panic!("Expected GatewayClass status update"),
        }
    }

    /// Spec: GatewayClass with non-matching controllerName should be ignored
    #[test]
    fn test_gateway_class_ignored_when_controller_differs() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(create_gateway_class("other", "other.controller/name"))
            .build();

        let config = ControllerConfig::default();
        let result = reconcile_gateway_class(&snapshot, &config, "other");

        // Should not update status (not our class)
        assert!(result.status_updates.is_empty());
        // Should requeue after long interval
        assert!(matches!(result.requeue, Some(RequeueDecision::After(d)) if d.as_secs() == 3600));
    }

    /// Spec: Gateway should create Deployment, Service, and ConfigMap
    #[test]
    fn test_gateway_creates_data_plane_resources() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(accepted_gateway_class("multiway"))
            .with_gateway(create_gateway("default", "my-gateway", "multiway"))
            .build();

        let config = ControllerConfig::default();
        let result = reconcile_gateway(&snapshot, &config, "default", "my-gateway");

        // Should create 3 resources
        assert_eq!(result.upserts.len(), 3);

        // Verify Deployment
        let has_deployment = result.upserts.iter().any(|u| matches!(u, ResourceUpsert::Deployment(_)));
        assert!(has_deployment, "Should create Deployment");

        // Verify Service
        let has_service = result.upserts.iter().any(|u| matches!(u, ResourceUpsert::Service(_)));
        assert!(has_service, "Should create Service");

        // Verify ConfigMap
        let has_configmap = result.upserts.iter().any(|u| matches!(u, ResourceUpsert::ConfigMap(_)));
        assert!(has_configmap, "Should create ConfigMap");
    }

    /// Spec: HTTPRoute backend not found should set BackendNotFound condition
    #[test]
    fn test_httproute_backend_not_found() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(accepted_gateway_class("multiway"))
            .with_gateway(create_gateway("default", "my-gateway", "multiway"))
            .with_httproute(create_httproute_with_backend(
                "default", "my-route", "my-gateway",
                "nonexistent-service", 80,
            ))
            // Note: service is NOT added to snapshot
            .build();

        let config = ControllerConfig::default();
        let result = reconcile_httproute(&snapshot, &config, "default", "my-route");

        // Should update route status with error
        assert_eq!(result.status_updates.len(), 1);
        match &result.status_updates[0] {
            StatusUpdate::HTTPRoute { status, .. } => {
                let parent = &status.parents[0];
                let accepted = parent.conditions.iter().find(|c| c.type_ == "Accepted").unwrap();
                assert_eq!(accepted.status, "False");
                assert_eq!(accepted.reason, "BackendNotFound");
            }
            _ => panic!("Expected HTTPRoute status update"),
        }
    }

    /// Spec: Cross-namespace reference requires ReferenceGrant
    #[test]
    fn test_cross_namespace_requires_reference_grant() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(accepted_gateway_class("multiway"))
            .with_gateway(create_gateway("gateway-ns", "my-gateway", "multiway"))
            .with_httproute(create_httproute(
                "route-ns", "my-route", "gateway-ns", "my-gateway",
            ))
            // No ReferenceGrant
            .build();

        let config = ControllerConfig::default();
        let result = reconcile_httproute(&snapshot, &config, "route-ns", "my-route");

        match &result.status_updates[0] {
            StatusUpdate::HTTPRoute { status, .. } => {
                let parent = &status.parents[0];
                let accepted = parent.conditions.iter().find(|c| c.type_ == "Accepted").unwrap();
                assert_eq!(accepted.status, "False");
                assert_eq!(accepted.reason, "RefNotPermitted");
            }
            _ => panic!("Expected HTTPRoute status update"),
        }
    }

    /// Spec: Cross-namespace reference allowed with ReferenceGrant
    #[test]
    fn test_cross_namespace_allowed_with_grant() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(accepted_gateway_class("multiway"))
            .with_gateway(create_gateway("gateway-ns", "my-gateway", "multiway"))
            .with_httproute(create_httproute(
                "route-ns", "my-route", "gateway-ns", "my-gateway",
            ))
            .with_reference_grant(create_reference_grant(
                "gateway-ns",  // Grant is in the target namespace
                "route-ns",    // Allows references from route-ns
                "HTTPRoute",
                "Gateway",
            ))
            .build();

        let config = ControllerConfig::default();
        let result = reconcile_httproute(&snapshot, &config, "route-ns", "my-route");

        match &result.status_updates[0] {
            StatusUpdate::HTTPRoute { status, .. } => {
                let parent = &status.parents[0];
                let accepted = parent.conditions.iter().find(|c| c.type_ == "Accepted").unwrap();
                assert_eq!(accepted.status, "True");
                assert_eq!(accepted.reason, "Accepted");
            }
            _ => panic!("Expected HTTPRoute status update"),
        }
    }

    // Helper functions for creating test resources
    fn create_gateway_class(name: &str, controller: &str) -> GatewayClass { /* ... */ }
    fn accepted_gateway_class(name: &str) -> GatewayClass { /* ... */ }
    fn create_gateway(ns: &str, name: &str, class: &str) -> Gateway { /* ... */ }
    fn create_httproute(ns: &str, name: &str, gw_ns: &str, gw_name: &str) -> HTTPRoute { /* ... */ }
    fn create_reference_grant(ns: &str, from_ns: &str, from_kind: &str, to_kind: &str) -> ReferenceGrant { /* ... */ }
}
```

## Gateway API Spec Traceability

With this architecture, each Gateway API spec requirement can be traced to a unit test:

| Spec Section | Requirement | Test |
|-------------|-------------|------|
| GatewayClass | controllerName matching | `test_gateway_class_accepted_when_controller_matches` |
| GatewayClass | Non-matching controller ignored | `test_gateway_class_ignored_when_controller_differs` |
| Gateway | Creates data plane resources | `test_gateway_creates_data_plane_resources` |
| Gateway | Invalid listener rejected | `test_gateway_invalid_listener_rejected` |
| HTTPRoute | Backend not found | `test_httproute_backend_not_found` |
| HTTPRoute | Cross-namespace requires ReferenceGrant | `test_cross_namespace_requires_reference_grant` |
| HTTPRoute | Cross-namespace allowed with grant | `test_cross_namespace_allowed_with_grant` |
| ... | ... | ... |

## Module Structure

```
crates/controlplane/src/
├── core/                    # Functional core (pure, no I/O)
│   ├── mod.rs
│   ├── snapshot.rs          # WorldSnapshot and builder
│   ├── result.rs            # ReconcileResult (effects as data)
│   ├── reconcile.rs         # Pure reconciliation functions
│   ├── validate.rs          # Pure validation functions
│   └── build.rs             # Pure resource building functions
├── shell/                   # Imperative shell (I/O)
│   ├── mod.rs
│   ├── executor.rs          # Executes ReconcileResults
│   └── fetcher.rs           # Builds WorldSnapshots from cluster
├── controller/              # Controller wiring
│   ├── mod.rs
│   ├── gateway_class.rs     # GatewayClass controller entry
│   ├── gateway.rs           # Gateway controller entry
│   └── httproute.rs         # HTTPRoute controller entry
├── config.rs                # Configuration types
└── lib.rs
```

## Migration Strategy

1. **Phase 1: Add core module with snapshot and result types**
   - Add `WorldSnapshot`, `WorldSnapshotBuilder`
   - Add `ReconcileResult` and effect types
   - No changes to existing code

2. **Phase 2: Extract pure functions**
   - Move `validate_listeners()` to `core/validate.rs`
   - Move `build_gateway_config()` to `core/build.rs`
   - Move conversion functions to `core/`
   - Add unit tests for each extracted function

3. **Phase 3: Implement functional reconcilers**
   - Create `core/reconcile.rs` with pure reconciliation functions
   - Create comprehensive unit tests
   - Validate against Gateway API spec

4. **Phase 4: Implement imperative shell**
   - Create `shell/executor.rs`
   - Create `shell/fetcher.rs`
   - Wire up controllers to use functional core

5. **Phase 5: Clean up**
   - Remove old effectful reconciler code
   - Update documentation
   - Add conformance test mapping

## Benefits

1. **Fast tests**: Unit tests run in milliseconds, not seconds
2. **Complete coverage**: Every spec requirement can have a unit test
3. **Deterministic**: Injected time means reproducible tests
4. **Debuggable**: Can inspect `ReconcileResult` to see exactly what would happen
5. **Composable**: Results can be merged, filtered, or transformed
6. **Maintainable**: Pure functions are easier to understand and modify

## Trade-offs

1. **More code**: Snapshot and result types add boilerplate
2. **Two-phase execution**: Effects happen after computation
3. **Learning curve**: Different pattern from typical controller code
4. **Snapshot staleness**: Snapshot may become stale during execution
