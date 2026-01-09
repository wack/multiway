//! Controller CLI command
//!
//! This module provides the CLI command for running the Gateway API controller.

use miette::Result;
use tokio::runtime::Runtime;
use tracing::info;

use crate::controller::GatewayControllerBuilder;

/// Controller command configuration
pub struct Controller {
    /// Data plane image to use
    pub dataplane_image: Option<String>,
    /// Number of replicas for data plane deployments
    pub replicas: Option<i32>,
    /// Namespace to watch (None = all namespaces)
    pub namespace: Option<String>,
}

impl Controller {
    pub fn new() -> Self {
        Self {
            dataplane_image: None,
            replicas: None,
            namespace: None,
        }
    }

    pub fn dispatch(self) -> Result<()> {
        // Create the Tokio runtime
        let rt = Runtime::new().map_err(|e| miette::miette!("Failed to create runtime: {}", e))?;

        rt.block_on(async {
            info!("Starting Multiway Gateway API Controller");
            self.run_controller().await
        })
    }

    async fn run_controller(self) -> Result<()> {
        // Build the controller with configuration
        let mut builder = GatewayControllerBuilder::new();

        if let Some(image) = self.dataplane_image {
            builder = builder.dataplane_image(image);
        }

        if let Some(replicas) = self.replicas {
            builder = builder.default_replicas(replicas);
        }

        if let Some(ns) = self.namespace {
            builder = builder.watch_namespace(Some(ns));
        }

        let controller = builder
            .build()
            .await
            .map_err(|e| miette::miette!("Failed to create controller: {}", e))?;

        // Run the controller (blocks until shutdown)
        controller.run().await;

        Ok(())
    }
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}
