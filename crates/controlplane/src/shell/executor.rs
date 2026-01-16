//! Reconcile executor - applies ReconcileResults to the Kubernetes cluster.
//!
//! This module is responsible for executing the effects described in a
//! [`ReconcileResult`]. It performs all I/O operations to apply changes
//! to the cluster.

use std::time::Duration;

use gateway_crds::{Gateway, GatewayClass, HTTPRoute};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Service, ServiceAccount};
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use kube::api::{DeleteParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Api, Client};
use tracing::{debug, error, info, warn};

use crate::controller::error::Result;
use crate::core::result::{
    ReconcileResult, RequeueDecision, ResourceDelete, ResourceUpsert, StatusUpdate,
};

/// Executes ReconcileResults against a Kubernetes cluster.
pub struct ReconcileExecutor {
    client: Client,
}

impl ReconcileExecutor {
    /// Create a new ReconcileExecutor
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Execute a ReconcileResult against the cluster.
    ///
    /// This applies all the effects described in the result:
    /// - Creates/updates resources
    /// - Deletes resources
    /// - Updates status subresources
    ///
    /// Returns the appropriate requeue action.
    pub async fn execute(&self, result: ReconcileResult) -> Result<Action> {
        // Log events
        for event in &result.events {
            match event.severity {
                crate::core::result::EventSeverity::Normal => {
                    info!(reason = %event.reason, message = %event.message, "Event");
                }
                crate::core::result::EventSeverity::Warning => {
                    warn!(reason = %event.reason, message = %event.message, "Warning");
                }
            }
        }

        // Execute status updates FIRST to ensure observedGeneration is updated
        // even if resource creation fails. This is required for Gateway API conformance.
        for status_update in &result.status_updates {
            if let Err(e) = self.execute_status_update(status_update).await {
                error!(error = %e, "Failed to execute status update");
                return Ok(Action::requeue(Duration::from_secs(5)));
            }
        }

        // Execute upserts
        for upsert in &result.upserts {
            if let Err(e) = self.execute_upsert(upsert).await {
                error!(error = %e, "Failed to execute upsert");
                return Ok(Action::requeue(Duration::from_secs(5)));
            }
        }

        // Execute deletes
        for delete in &result.deletes {
            if let Err(e) = self.execute_delete(delete).await {
                error!(error = %e, "Failed to execute delete");
                // Continue with other deletes, don't fail the whole reconciliation
            }
        }

        // Convert requeue decision to Action
        let action = match result.requeue {
            Some(RequeueDecision::After(d)) => Action::requeue(d),
            Some(RequeueDecision::OnError(d)) => Action::requeue(d),
            Some(RequeueDecision::Never) => Action::requeue(Duration::from_secs(3600)), // Long requeue for "never"
            None => Action::requeue(Duration::from_secs(300)),
        };

        Ok(action)
    }

    /// Execute a single resource upsert
    async fn execute_upsert(&self, upsert: &ResourceUpsert) -> Result<()> {
        match upsert {
            ResourceUpsert::Deployment(deployment) => self.upsert_deployment(deployment).await,
            ResourceUpsert::Service(service) => self.upsert_service(service).await,
            ResourceUpsert::ConfigMap(configmap) => self.upsert_configmap(configmap).await,
            ResourceUpsert::ServiceAccount(sa) => self.upsert_serviceaccount(sa).await,
            ResourceUpsert::Role(role) => self.upsert_role(role).await,
            ResourceUpsert::RoleBinding(rb) => self.upsert_rolebinding(rb).await,
        }
    }

    /// Execute a single resource delete
    async fn execute_delete(&self, delete: &ResourceDelete) -> Result<()> {
        let namespace = delete.namespace.as_deref();

        match delete.kind.as_str() {
            "Deployment" => {
                if let Some(ns) = namespace {
                    let api: Api<Deployment> = Api::namespaced(self.client.clone(), ns);
                    api.delete(&delete.name, &DeleteParams::default()).await?;
                    debug!(namespace = ns, name = %delete.name, "Deleted Deployment");
                }
            }
            "Service" => {
                if let Some(ns) = namespace {
                    let api: Api<Service> = Api::namespaced(self.client.clone(), ns);
                    api.delete(&delete.name, &DeleteParams::default()).await?;
                    debug!(namespace = ns, name = %delete.name, "Deleted Service");
                }
            }
            "ConfigMap" => {
                if let Some(ns) = namespace {
                    let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
                    api.delete(&delete.name, &DeleteParams::default()).await?;
                    debug!(namespace = ns, name = %delete.name, "Deleted ConfigMap");
                }
            }
            "ServiceAccount" => {
                if let Some(ns) = namespace {
                    let api: Api<ServiceAccount> = Api::namespaced(self.client.clone(), ns);
                    api.delete(&delete.name, &DeleteParams::default()).await?;
                    debug!(namespace = ns, name = %delete.name, "Deleted ServiceAccount");
                }
            }
            "Role" => {
                if let Some(ns) = namespace {
                    let api: Api<Role> = Api::namespaced(self.client.clone(), ns);
                    api.delete(&delete.name, &DeleteParams::default()).await?;
                    debug!(namespace = ns, name = %delete.name, "Deleted Role");
                }
            }
            "RoleBinding" => {
                if let Some(ns) = namespace {
                    let api: Api<RoleBinding> = Api::namespaced(self.client.clone(), ns);
                    api.delete(&delete.name, &DeleteParams::default()).await?;
                    debug!(namespace = ns, name = %delete.name, "Deleted RoleBinding");
                }
            }
            kind => {
                warn!(kind, name = %delete.name, "Unknown resource kind for delete");
            }
        }

        Ok(())
    }

    /// Execute a single status update
    async fn execute_status_update(&self, update: &StatusUpdate) -> Result<()> {
        match update {
            StatusUpdate::GatewayClass { name, status } => {
                let api: Api<GatewayClass> = Api::all(self.client.clone());
                let patch = serde_json::json!({ "status": status });
                api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
                    .await?;
                debug!(name, "Updated GatewayClass status");
            }
            StatusUpdate::Gateway {
                namespace,
                name,
                status,
            } => {
                let api: Api<Gateway> = Api::namespaced(self.client.clone(), namespace);
                let patch = serde_json::json!({ "status": status });
                api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
                    .await?;
                debug!(namespace, name, "Updated Gateway status");
            }
            StatusUpdate::HTTPRoute {
                namespace,
                name,
                status,
            } => {
                let api: Api<HTTPRoute> = Api::namespaced(self.client.clone(), namespace);
                let patch = serde_json::json!({ "status": status });
                api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
                    .await?;
                debug!(namespace, name, "Updated HTTPRoute status");
            }
        }

        Ok(())
    }

    /// Upsert a Deployment using Server-Side Apply
    async fn upsert_deployment(&self, deployment: &Deployment) -> Result<()> {
        let namespace = deployment
            .metadata
            .namespace
            .as_deref()
            .unwrap_or("default");
        let name = deployment.metadata.name.as_deref().unwrap();
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);

        debug!(namespace, name, "Applying Deployment");
        api.patch(
            name,
            &PatchParams::apply("multiway-controller").force(),
            &Patch::Apply(deployment),
        )
        .await?;

        Ok(())
    }

    /// Upsert a Service using Server-Side Apply
    async fn upsert_service(&self, service: &Service) -> Result<()> {
        let namespace = service.metadata.namespace.as_deref().unwrap_or("default");
        let name = service.metadata.name.as_deref().unwrap();
        let api: Api<Service> = Api::namespaced(self.client.clone(), namespace);

        debug!(namespace, name, "Applying Service");
        api.patch(
            name,
            &PatchParams::apply("multiway-controller").force(),
            &Patch::Apply(service),
        )
        .await?;

        Ok(())
    }

    /// Upsert a ConfigMap using create-or-replace pattern
    ///
    /// We use a create-or-replace pattern instead of Server-Side Apply because
    /// SSA field manager conflicts with "unknown" persist even when using
    /// `.force()`. This appears to be a kube-rs or Kubernetes API server issue.
    /// The create-or-replace pattern avoids these conflicts entirely.
    async fn upsert_configmap(&self, configmap: &ConfigMap) -> Result<()> {
        use kube::api::PostParams;

        let namespace = configmap.metadata.namespace.as_deref().unwrap_or("default");
        let name = configmap.metadata.name.as_deref().unwrap();
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);

        debug!(namespace, name, "Upserting ConfigMap");

        // Build a clean ConfigMap with required fields for creation
        let mut cm = ConfigMap {
            metadata: kube::core::ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                labels: configmap.metadata.labels.clone(),
                owner_references: configmap.metadata.owner_references.clone(),
                ..Default::default()
            },
            data: configmap.data.clone(),
            binary_data: None,
            immutable: None,
        };

        // Try to get the existing ConfigMap
        match api.get(name).await {
            Ok(existing) => {
                // ConfigMap exists - update it by setting resource_version
                cm.metadata.resource_version = existing.metadata.resource_version;
                api.replace(name, &PostParams::default(), &cm).await?;
                debug!(namespace, name, "Replaced ConfigMap");
            }
            Err(kube::Error::Api(api_err)) if api_err.code == 404 => {
                // ConfigMap doesn't exist - create it
                api.create(&PostParams::default(), &cm).await?;
                debug!(namespace, name, "Created ConfigMap");
            }
            Err(e) => {
                return Err(e.into());
            }
        }

        Ok(())
    }

    /// Upsert a ServiceAccount using Server-Side Apply
    async fn upsert_serviceaccount(&self, sa: &ServiceAccount) -> Result<()> {
        let namespace = sa.metadata.namespace.as_deref().unwrap_or("default");
        let name = sa.metadata.name.as_deref().unwrap();
        let api: Api<ServiceAccount> = Api::namespaced(self.client.clone(), namespace);

        debug!(namespace, name, "Applying ServiceAccount");
        api.patch(
            name,
            &PatchParams::apply("multiway-controller").force(),
            &Patch::Apply(sa),
        )
        .await?;

        Ok(())
    }

    /// Upsert a Role using Server-Side Apply
    async fn upsert_role(&self, role: &Role) -> Result<()> {
        let namespace = role.metadata.namespace.as_deref().unwrap_or("default");
        let name = role.metadata.name.as_deref().unwrap();
        let api: Api<Role> = Api::namespaced(self.client.clone(), namespace);

        debug!(namespace, name, "Applying Role");
        api.patch(
            name,
            &PatchParams::apply("multiway-controller").force(),
            &Patch::Apply(role),
        )
        .await?;

        Ok(())
    }

    /// Upsert a RoleBinding using Server-Side Apply
    async fn upsert_rolebinding(&self, rb: &RoleBinding) -> Result<()> {
        let namespace = rb.metadata.namespace.as_deref().unwrap_or("default");
        let name = rb.metadata.name.as_deref().unwrap();
        let api: Api<RoleBinding> = Api::namespaced(self.client.clone(), namespace);

        debug!(namespace, name, "Applying RoleBinding");
        api.patch(
            name,
            &PatchParams::apply("multiway-controller").force(),
            &Patch::Apply(rb),
        )
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::result::EventSeverity;

    #[test]
    fn test_result_to_action_conversion() {
        // Test After requeue
        let result = ReconcileResult::new().with_requeue(Duration::from_secs(60));
        assert!(matches!(result.requeue, Some(RequeueDecision::After(d)) if d.as_secs() == 60));

        // Test OnError requeue
        let result = ReconcileResult::new().with_requeue_on_error(Duration::from_secs(5));
        assert!(matches!(result.requeue, Some(RequeueDecision::OnError(d)) if d.as_secs() == 5));
    }

    #[test]
    fn test_result_events() {
        let result = ReconcileResult::new()
            .emit_normal("Created", "Resource created successfully")
            .emit_warning("Warning", "Something might be wrong");

        assert_eq!(result.events.len(), 2);
        assert!(matches!(result.events[0].severity, EventSeverity::Normal));
        assert!(matches!(result.events[1].severity, EventSeverity::Warning));
    }
}
