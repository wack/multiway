//! Gateway data plane command
//!
//! This module provides the CLI command for running the Gateway data plane.
//! The data plane is a proxy-core HTTP proxy that reads its configuration
//! from a Kubernetes ConfigMap via the API.

use miette::Result;
use tracing::info;

/// Run the API Gateway data plane
pub struct Gateway {
    /// Path to the configuration file
    pub config_path: String,
    /// Gateway name (for logging)
    pub gateway_name: Option<String>,
    /// Gateway namespace (for logging)
    pub gateway_namespace: Option<String>,
}

impl Gateway {
    pub fn new() -> Self {
        Self {
            config_path: "/config/config.json".to_string(),
            gateway_name: None,
            gateway_namespace: None,
        }
    }

    pub fn dispatch(self) -> Result<()> {
        info!(
            config_path = %self.config_path,
            gateway_name = ?self.gateway_name,
            gateway_namespace = ?self.gateway_namespace,
            "Starting Gateway data plane"
        );

        // TODO: This is a placeholder — the actual data plane is in the dataplane crate
        // (multiway-dataplane binary) which uses proxy-core for HTTP proxying.
        println!(
            "Gateway data plane starting with config: {}",
            self.config_path
        );
        println!(
            "This is a placeholder - the proxy-core data plane runs as a separate binary (multiway-dataplane)"
        );

        // For now, just exit successfully
        // In production, the data plane runs as multiway-dataplane
        Ok(())
    }
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new()
    }
}
