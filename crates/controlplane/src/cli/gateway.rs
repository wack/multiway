//! Gateway data plane command
//!
//! This module provides the CLI command for running the Gateway data plane.
//! The data plane is a Pingora-based HTTP proxy that reads its configuration
//! from a file (typically a ConfigMap mounted in Kubernetes).

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

        // TODO: This is a placeholder for the Pingora-based data plane
        // The actual implementation will be in the dataplane crate
        println!(
            "Gateway data plane starting with config: {}",
            self.config_path
        );
        println!(
            "This is a placeholder - the Pingora data plane will be implemented in the dataplane crate"
        );

        // For now, just exit successfully
        // In production, this would start the Pingora server
        Ok(())
    }
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new()
    }
}
