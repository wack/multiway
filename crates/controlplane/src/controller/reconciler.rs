//! Main reconciler orchestrator
//!
//! This module provides the main entry point for running all Gateway API
//! controllers together. It handles startup, graceful shutdown, and
//! coordination between the different reconcilers.

use std::sync::Arc;

use futures::future::join_all;
use kube::Client;
use miette::Result;
use tracing::{error, info};

use super::context::{ControllerConfig, ControllerContext};
use super::gateway::run_gateway_controller;
use super::gateway_class::run_gateway_class_controller;
use super::httproute::run_httproute_controller;

/// Main controller that runs all reconcilers
pub struct GatewayController {
    context: Arc<ControllerContext>,
}

impl GatewayController {
    /// Create a new GatewayController with default configuration
    pub async fn new() -> Result<Self, kube::Error> {
        let client = Client::try_default().await?;
        let config = ControllerConfig::default();
        let context = ControllerContext::new(client, config).into_arc();
        Ok(Self { context })
    }

    /// Create a new GatewayController with custom configuration
    pub async fn with_config(config: ControllerConfig) -> Result<Self, kube::Error> {
        let client = Client::try_default().await?;
        let context = ControllerContext::new(client, config).into_arc();
        Ok(Self { context })
    }

    /// Create a new GatewayController with an existing client
    pub fn with_client(client: Client, config: ControllerConfig) -> Self {
        let context = ControllerContext::new(client, config).into_arc();
        Self { context }
    }

    /// Run all controllers concurrently
    ///
    /// This method runs the GatewayClass, Gateway, and HTTPRoute controllers
    /// concurrently using tokio. It returns when all controllers have finished
    /// (which typically happens on shutdown signal).
    pub async fn run(self) {
        info!(
            dataplane_image = %self.context.config.dataplane_image,
            "Starting Gateway API controller"
        );

        let ctx = self.context.clone();

        // Run all controllers concurrently
        let gateway_class_handle = tokio::spawn({
            let ctx = ctx.clone();
            async move {
                run_gateway_class_controller(ctx).await;
            }
        });

        let gateway_handle = tokio::spawn({
            let ctx = ctx.clone();
            async move {
                run_gateway_controller(ctx).await;
            }
        });

        let httproute_handle = tokio::spawn({
            let ctx = ctx.clone();
            async move {
                run_httproute_controller(ctx).await;
            }
        });

        // Wait for all controllers to complete
        let results = join_all(vec![gateway_class_handle, gateway_handle, httproute_handle]).await;

        for (i, result) in results.into_iter().enumerate() {
            let controller_name = match i {
                0 => "GatewayClass",
                1 => "Gateway",
                2 => "HTTPRoute",
                _ => "Unknown",
            };

            if let Err(e) = result {
                error!(
                    controller = controller_name,
                    error = %e,
                    "Controller task failed"
                );
            }
        }

        info!("Gateway API controller stopped");
    }

    /// Get the controller context
    pub fn context(&self) -> &Arc<ControllerContext> {
        &self.context
    }
}

/// Builder for GatewayController
pub struct GatewayControllerBuilder {
    config: ControllerConfig,
    client: Option<Client>,
}

impl Default for GatewayControllerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GatewayControllerBuilder {
    /// Create a new builder with default configuration
    pub fn new() -> Self {
        Self {
            config: ControllerConfig::default(),
            client: None,
        }
    }

    /// Set the data plane image
    pub fn dataplane_image(mut self, image: impl Into<String>) -> Self {
        self.config.dataplane_image = image.into();
        self
    }

    /// Set the image pull policy
    pub fn image_pull_policy(mut self, policy: impl Into<String>) -> Self {
        self.config.image_pull_policy = policy.into();
        self
    }

    /// Set the default number of replicas
    pub fn default_replicas(mut self, replicas: i32) -> Self {
        self.config.default_replicas = replicas;
        self
    }

    /// Set the requeue interval for successful reconciliation
    pub fn requeue_after_secs(mut self, secs: u64) -> Self {
        self.config.requeue_after_secs = secs;
        self
    }

    /// Set the requeue interval after errors
    pub fn error_requeue_secs(mut self, secs: u64) -> Self {
        self.config.error_requeue_secs = secs;
        self
    }

    /// Set the namespace to watch (None = all namespaces)
    pub fn watch_namespace(mut self, namespace: Option<String>) -> Self {
        self.config.watch_namespace = namespace;
        self
    }

    /// Use an existing Kubernetes client
    pub fn with_client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Build the GatewayController
    ///
    /// If no client was provided, this will create one using the default
    /// configuration (in-cluster or from kubeconfig).
    pub async fn build(self) -> Result<GatewayController, kube::Error> {
        let client = match self.client {
            Some(c) => c,
            None => Client::try_default().await?,
        };

        Ok(GatewayController::with_client(client, self.config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_defaults() {
        let builder = GatewayControllerBuilder::new();
        assert_eq!(
            builder.config.dataplane_image,
            "ghcr.io/wack/multiway-dataplane:latest"
        );
        assert_eq!(builder.config.default_replicas, 1);
    }

    #[test]
    fn test_builder_custom_config() {
        let builder = GatewayControllerBuilder::new()
            .dataplane_image("custom-image:v1")
            .default_replicas(3)
            .requeue_after_secs(600);

        assert_eq!(builder.config.dataplane_image, "custom-image:v1");
        assert_eq!(builder.config.default_replicas, 3);
        assert_eq!(builder.config.requeue_after_secs, 600);
    }
}
