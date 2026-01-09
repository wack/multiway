//! Shared context for the Gateway controller
//!
//! This module provides the shared state that is passed to all reconcilers.
//! It includes the Kubernetes client and configuration options.

use kube::Client;
use std::sync::Arc;

/// Shared context for all reconcilers
#[derive(Clone)]
pub struct ControllerContext {
    /// Kubernetes client
    pub client: Client,
    /// Controller configuration
    pub config: ControllerConfig,
}

impl ControllerContext {
    /// Create a new controller context
    pub fn new(client: Client, config: ControllerConfig) -> Self {
        Self { client, config }
    }

    /// Create a new controller context with default configuration
    pub fn with_defaults(client: Client) -> Self {
        Self {
            client,
            config: ControllerConfig::default(),
        }
    }

    /// Wrap in Arc for sharing between reconcilers
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

/// Configuration for the controller
#[derive(Clone, Debug)]
pub struct ControllerConfig {
    /// Data plane image to use
    pub dataplane_image: String,
    /// Image pull policy for data plane pods
    pub image_pull_policy: String,
    /// Default number of replicas for data plane deployments
    pub default_replicas: i32,
    /// Resource requests for data plane pods
    pub resource_requests: ResourceConfig,
    /// Resource limits for data plane pods
    pub resource_limits: ResourceConfig,
    /// Requeue duration for successful reconciliation (seconds)
    pub requeue_after_secs: u64,
    /// Requeue duration after errors (seconds)
    pub error_requeue_secs: u64,
    /// Enable leader election
    pub leader_election: bool,
    /// Namespace to watch (None = all namespaces)
    pub watch_namespace: Option<String>,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            dataplane_image: "ghcr.io/wack/multiway-dataplane:latest".to_string(),
            image_pull_policy: "IfNotPresent".to_string(),
            default_replicas: 1,
            resource_requests: ResourceConfig {
                cpu: "100m".to_string(),
                memory: "128Mi".to_string(),
            },
            resource_limits: ResourceConfig {
                cpu: "500m".to_string(),
                memory: "256Mi".to_string(),
            },
            requeue_after_secs: 300,
            error_requeue_secs: 5,
            leader_election: false,
            watch_namespace: None,
        }
    }
}

impl ControllerConfig {
    /// Create a new controller configuration builder
    pub fn builder() -> ControllerConfigBuilder {
        ControllerConfigBuilder::default()
    }
}

/// Builder for ControllerConfig
#[derive(Default)]
pub struct ControllerConfigBuilder {
    config: ControllerConfig,
}

impl ControllerConfigBuilder {
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

    /// Set resource requests
    pub fn resource_requests(mut self, cpu: impl Into<String>, memory: impl Into<String>) -> Self {
        self.config.resource_requests = ResourceConfig {
            cpu: cpu.into(),
            memory: memory.into(),
        };
        self
    }

    /// Set resource limits
    pub fn resource_limits(mut self, cpu: impl Into<String>, memory: impl Into<String>) -> Self {
        self.config.resource_limits = ResourceConfig {
            cpu: cpu.into(),
            memory: memory.into(),
        };
        self
    }

    /// Set requeue duration for successful reconciliation
    pub fn requeue_after_secs(mut self, secs: u64) -> Self {
        self.config.requeue_after_secs = secs;
        self
    }

    /// Set requeue duration after errors
    pub fn error_requeue_secs(mut self, secs: u64) -> Self {
        self.config.error_requeue_secs = secs;
        self
    }

    /// Enable leader election
    pub fn leader_election(mut self, enabled: bool) -> Self {
        self.config.leader_election = enabled;
        self
    }

    /// Set the namespace to watch
    pub fn watch_namespace(mut self, namespace: Option<String>) -> Self {
        self.config.watch_namespace = namespace;
        self
    }

    /// Build the configuration
    pub fn build(self) -> ControllerConfig {
        self.config
    }
}

/// Resource configuration for pods
#[derive(Clone, Debug)]
pub struct ResourceConfig {
    /// CPU request/limit
    pub cpu: String,
    /// Memory request/limit
    pub memory: String,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            cpu: "100m".to_string(),
            memory: "128Mi".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ControllerConfig::default();
        assert_eq!(config.default_replicas, 1);
        assert_eq!(config.requeue_after_secs, 300);
        assert!(!config.leader_election);
    }

    #[test]
    fn test_config_builder() {
        let config = ControllerConfig::builder()
            .dataplane_image("my-image:v1")
            .default_replicas(3)
            .leader_election(true)
            .build();

        assert_eq!(config.dataplane_image, "my-image:v1");
        assert_eq!(config.default_replicas, 3);
        assert!(config.leader_election);
    }
}
