//! Reconciliation result types.
//!
//! The [`ReconcileResult`] represents effects as data. Instead of performing
//! I/O operations directly, the functional core returns a result describing
//! what should happen. The imperative shell then executes these effects.

use std::time::Duration;

use gateway_crds::{GatewayClassStatus, GatewayStatus, HttpRouteStatus};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Service, ServiceAccount};

/// The result of a reconciliation - describes effects to perform.
///
/// This struct is the output of the functional reconciliation core.
/// It describes all the effects that should be performed without
/// actually performing them, enabling easy testing.
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
    /// Create a new empty result
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the requeue decision
    pub fn with_requeue(mut self, after: Duration) -> Self {
        self.requeue = Some(RequeueDecision::After(after));
        self
    }

    /// Set the requeue decision for error cases
    pub fn with_requeue_on_error(mut self, after: Duration) -> Self {
        self.requeue = Some(RequeueDecision::OnError(after));
        self
    }

    /// Add a Deployment to create or update
    pub fn upsert_deployment(mut self, deployment: Deployment) -> Self {
        self.upserts.push(ResourceUpsert::Deployment(deployment));
        self
    }

    /// Add a Service to create or update
    pub fn upsert_service(mut self, service: Service) -> Self {
        self.upserts.push(ResourceUpsert::Service(service));
        self
    }

    /// Add a ConfigMap to create or update
    pub fn upsert_configmap(mut self, configmap: ConfigMap) -> Self {
        self.upserts.push(ResourceUpsert::ConfigMap(configmap));
        self
    }

    /// Add a ServiceAccount to create or update
    pub fn upsert_serviceaccount(mut self, sa: ServiceAccount) -> Self {
        self.upserts.push(ResourceUpsert::ServiceAccount(sa));
        self
    }

    /// Add a resource to delete
    pub fn delete_resource(mut self, delete: ResourceDelete) -> Self {
        self.deletes.push(delete);
        self
    }

    /// Add a GatewayClass status update
    pub fn update_gateway_class_status(mut self, name: String, status: GatewayClassStatus) -> Self {
        self.status_updates
            .push(StatusUpdate::GatewayClass { name, status });
        self
    }

    /// Add a Gateway status update
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

    /// Add an HTTPRoute status update
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

    /// Emit an event
    pub fn emit_event(mut self, event: ReconcileEvent) -> Self {
        self.events.push(event);
        self
    }

    /// Emit a normal event
    pub fn emit_normal(self, reason: impl Into<String>, message: impl Into<String>) -> Self {
        self.emit_event(ReconcileEvent {
            severity: EventSeverity::Normal,
            reason: reason.into(),
            message: message.into(),
        })
    }

    /// Emit a warning event
    pub fn emit_warning(self, reason: impl Into<String>, message: impl Into<String>) -> Self {
        self.emit_event(ReconcileEvent {
            severity: EventSeverity::Warning,
            reason: reason.into(),
            message: message.into(),
        })
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

    /// Check if this result has any upserts
    pub fn has_upserts(&self) -> bool {
        !self.upserts.is_empty()
    }

    /// Check if this result has any deletes
    pub fn has_deletes(&self) -> bool {
        !self.deletes.is_empty()
    }

    /// Check if this result has any status updates
    pub fn has_status_updates(&self) -> bool {
        !self.status_updates.is_empty()
    }

    /// Get all Deployment upserts
    pub fn deployment_upserts(&self) -> Vec<&Deployment> {
        self.upserts
            .iter()
            .filter_map(|u| match u {
                ResourceUpsert::Deployment(d) => Some(d),
                _ => None,
            })
            .collect()
    }

    /// Get all Service upserts
    pub fn service_upserts(&self) -> Vec<&Service> {
        self.upserts
            .iter()
            .filter_map(|u| match u {
                ResourceUpsert::Service(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    /// Get all ConfigMap upserts
    pub fn configmap_upserts(&self) -> Vec<&ConfigMap> {
        self.upserts
            .iter()
            .filter_map(|u| match u {
                ResourceUpsert::ConfigMap(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    /// Get the Gateway status update if present
    pub fn gateway_status_update(&self, namespace: &str, name: &str) -> Option<&GatewayStatus> {
        self.status_updates.iter().find_map(|u| match u {
            StatusUpdate::Gateway {
                namespace: ns,
                name: n,
                status,
            } if ns == namespace && n == name => Some(status),
            _ => None,
        })
    }

    /// Get the GatewayClass status update if present
    pub fn gateway_class_status_update(&self, name: &str) -> Option<&GatewayClassStatus> {
        self.status_updates.iter().find_map(|u| match u {
            StatusUpdate::GatewayClass {
                name: n, status, ..
            } if n == name => Some(status),
            _ => None,
        })
    }

    /// Get the HTTPRoute status update if present
    pub fn httproute_status_update(&self, namespace: &str, name: &str) -> Option<&HttpRouteStatus> {
        self.status_updates.iter().find_map(|u| match u {
            StatusUpdate::HTTPRoute {
                namespace: ns,
                name: n,
                status,
            } if ns == namespace && n == name => Some(status),
            _ => None,
        })
    }
}

/// A resource to create or update
#[derive(Debug, Clone)]
pub enum ResourceUpsert {
    /// Create or update a Deployment
    Deployment(Deployment),
    /// Create or update a Service
    Service(Service),
    /// Create or update a ConfigMap
    ConfigMap(ConfigMap),
    /// Create or update a ServiceAccount
    ServiceAccount(ServiceAccount),
}

/// A resource to delete
#[derive(Debug, Clone)]
pub struct ResourceDelete {
    /// API group and version (e.g., "apps/v1")
    pub api_version: String,
    /// Resource kind (e.g., "Deployment")
    pub kind: String,
    /// Namespace (None for cluster-scoped resources)
    pub namespace: Option<String>,
    /// Resource name
    pub name: String,
}

impl ResourceDelete {
    /// Create a new resource delete
    pub fn new(
        api_version: impl Into<String>,
        kind: impl Into<String>,
        namespace: Option<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            api_version: api_version.into(),
            kind: kind.into(),
            namespace,
            name: name.into(),
        }
    }

    /// Create a delete for a namespaced resource
    pub fn namespaced(
        api_version: impl Into<String>,
        kind: impl Into<String>,
        namespace: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self::new(api_version, kind, Some(namespace.into()), name)
    }
}

/// A status update to apply
#[derive(Debug, Clone)]
pub enum StatusUpdate {
    /// Update GatewayClass status
    GatewayClass {
        /// Name of the GatewayClass
        name: String,
        /// New status
        status: GatewayClassStatus,
    },
    /// Update Gateway status
    Gateway {
        /// Namespace of the Gateway
        namespace: String,
        /// Name of the Gateway
        name: String,
        /// New status
        status: GatewayStatus,
    },
    /// Update HTTPRoute status
    HTTPRoute {
        /// Namespace of the HTTPRoute
        namespace: String,
        /// Name of the HTTPRoute
        name: String,
        /// New status
        status: HttpRouteStatus,
    },
}

/// When to requeue the reconciliation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequeueDecision {
    /// Requeue after a duration (normal case)
    After(Duration),
    /// Requeue after a duration (error case)
    OnError(Duration),
    /// Don't requeue (terminal state or watch will trigger)
    Never,
}

impl RequeueDecision {
    /// Get the minimum of two requeue decisions
    pub fn min(self, other: Self) -> Self {
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

    /// Get the duration to requeue after
    pub fn duration(&self) -> Option<Duration> {
        match self {
            RequeueDecision::After(d) | RequeueDecision::OnError(d) => Some(*d),
            RequeueDecision::Never => None,
        }
    }

    /// Check if this is an error requeue
    pub fn is_error(&self) -> bool {
        matches!(self, RequeueDecision::OnError(_))
    }
}

/// Events emitted during reconciliation (for observability)
#[derive(Debug, Clone)]
pub struct ReconcileEvent {
    /// Event severity
    pub severity: EventSeverity,
    /// Short reason for the event
    pub reason: String,
    /// Detailed message
    pub message: String,
}

/// Event severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSeverity {
    /// Normal event (informational)
    Normal,
    /// Warning event (something unexpected but not fatal)
    Warning,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_result_merge() {
        let result1 = ReconcileResult::new()
            .with_requeue(Duration::from_secs(60))
            .emit_normal("Created", "Resource created");

        let result2 = ReconcileResult::new()
            .with_requeue(Duration::from_secs(30))
            .emit_warning("Updated", "Resource updated");

        let merged = result1.merge(result2);

        // Should have both events
        assert_eq!(merged.events.len(), 2);

        // Should have the shorter requeue time
        assert_eq!(
            merged.requeue,
            Some(RequeueDecision::After(Duration::from_secs(30)))
        );
    }

    #[test]
    fn test_requeue_decision_min() {
        use RequeueDecision::*;

        assert_eq!(
            After(Duration::from_secs(60)).min(After(Duration::from_secs(30))),
            After(Duration::from_secs(30))
        );

        assert_eq!(
            Never.min(After(Duration::from_secs(30))),
            After(Duration::from_secs(30))
        );

        // Error takes precedence
        assert_eq!(
            After(Duration::from_secs(60)).min(OnError(Duration::from_secs(30))),
            OnError(Duration::from_secs(30))
        );
    }

    #[test]
    fn test_result_accessors() {
        let deployment = Deployment {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some("test-deployment".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let result = ReconcileResult::new().upsert_deployment(deployment);

        assert!(result.has_upserts());
        assert!(!result.has_deletes());
        assert_eq!(result.deployment_upserts().len(), 1);
        assert_eq!(result.service_upserts().len(), 0);
    }

    #[test]
    fn test_resource_delete() {
        let delete = ResourceDelete::namespaced("apps/v1", "Deployment", "default", "my-deploy");
        assert_eq!(delete.api_version, "apps/v1");
        assert_eq!(delete.kind, "Deployment");
        assert_eq!(delete.namespace, Some("default".to_string()));
        assert_eq!(delete.name, "my-deploy");
    }
}
