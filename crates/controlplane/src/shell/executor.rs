//! Reconcile executor - applies ReconcileResults to the Kubernetes cluster.
//!
//! This module is responsible for executing the effects described in a
//! [`ReconcileResult`]. It performs all I/O operations to apply changes
//! to the cluster.

use std::time::Duration;

use gateway_crds::{Gateway, GatewayClass, HTTPRoute};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Service, ServiceAccount};
use kube::api::{DeleteParams, Patch, PatchParams, PostParams};
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

        // Execute status updates
        for status_update in &result.status_updates {
            if let Err(e) = self.execute_status_update(status_update).await {
                error!(error = %e, "Failed to execute status update");
                return Ok(Action::requeue(Duration::from_secs(5)));
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

    /// Upsert a Deployment
    async fn upsert_deployment(&self, deployment: &Deployment) -> Result<()> {
        let namespace = deployment
            .metadata
            .namespace
            .as_deref()
            .unwrap_or("default");
        let name = deployment.metadata.name.as_deref().unwrap();
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);

        match api.get(name).await {
            Ok(_) => {
                debug!(namespace, name, "Updating Deployment");
                api.patch(
                    name,
                    &PatchParams::apply("multiway-controller"),
                    &Patch::Apply(deployment),
                )
                .await?;
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                info!(namespace, name, "Creating Deployment");
                api.create(&PostParams::default(), deployment).await?;
            }
            Err(e) => return Err(e.into()),
        }

        Ok(())
    }

    /// Upsert a Service
    async fn upsert_service(&self, service: &Service) -> Result<()> {
        let namespace = service.metadata.namespace.as_deref().unwrap_or("default");
        let name = service.metadata.name.as_deref().unwrap();
        let api: Api<Service> = Api::namespaced(self.client.clone(), namespace);

        match api.get(name).await {
            Ok(_) => {
                debug!(namespace, name, "Updating Service");
                api.patch(
                    name,
                    &PatchParams::apply("multiway-controller"),
                    &Patch::Apply(service),
                )
                .await?;
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                info!(namespace, name, "Creating Service");
                api.create(&PostParams::default(), service).await?;
            }
            Err(e) => return Err(e.into()),
        }

        Ok(())
    }

    /// Upsert a ConfigMap
    async fn upsert_configmap(&self, configmap: &ConfigMap) -> Result<()> {
        let namespace = configmap.metadata.namespace.as_deref().unwrap_or("default");
        let name = configmap.metadata.name.as_deref().unwrap();
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);

        match api.get(name).await {
            Ok(_) => {
                debug!(namespace, name, "Updating ConfigMap");
                api.patch(
                    name,
                    &PatchParams::apply("multiway-controller"),
                    &Patch::Apply(configmap),
                )
                .await?;
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                info!(namespace, name, "Creating ConfigMap");
                api.create(&PostParams::default(), configmap).await?;
            }
            Err(e) => return Err(e.into()),
        }

        Ok(())
    }

    /// Upsert a ServiceAccount
    async fn upsert_serviceaccount(&self, sa: &ServiceAccount) -> Result<()> {
        let namespace = sa.metadata.namespace.as_deref().unwrap_or("default");
        let name = sa.metadata.name.as_deref().unwrap();
        let api: Api<ServiceAccount> = Api::namespaced(self.client.clone(), namespace);

        match api.get(name).await {
            Ok(_) => {
                debug!(namespace, name, "Updating ServiceAccount");
                api.patch(
                    name,
                    &PatchParams::apply("multiway-controller"),
                    &Patch::Apply(sa),
                )
                .await?;
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                info!(namespace, name, "Creating ServiceAccount");
                api.create(&PostParams::default(), sa).await?;
            }
            Err(e) => return Err(e.into()),
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::result::{EventSeverity, ReconcileEvent};

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
