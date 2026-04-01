//! Gateway reconciler
//!
//! This module implements the reconciliation logic for Gateway resources.
//! According to the Gateway API specification:
//!
//! - Gateway is a namespaced resource that provisions infrastructure
//! - Each Gateway triggers the creation of data plane resources
//! - Gateway status reflects the state of the infrastructure
//!
//! For this implementation:
//! - We create a Deployment running the proxy-core data plane
//! - We create a Service to expose the data plane
//! - We create a ConfigMap with the gateway configuration
//! - We update the Gateway status with addresses and listener status

use std::sync::Arc;

use futures::StreamExt;
use gateway_crds::Gateway;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Service};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Api, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, error, info, instrument};

use super::config::{MANAGED_BY_LABEL, MANAGED_BY_VALUE};
use super::context::ControllerContext;
use super::error::{ControllerError, Result};
use crate::core;
use crate::shell::{ReconcileExecutor, SnapshotFetcher};

/// Run the Gateway controller
pub async fn run_gateway_controller(ctx: Arc<ControllerContext>) {
    info!("Starting Gateway controller");

    let client = ctx.client.clone();
    let gateways: Api<Gateway> = match &ctx.config.watch_namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };

    // Watch for related resources to trigger reconciliation
    let deployments: Api<Deployment> = Api::all(client.clone());
    let services: Api<Service> = Api::all(client.clone());
    let configmaps: Api<ConfigMap> = Api::all(client);

    let controller = Controller::new(gateways.clone(), WatcherConfig::default())
        .shutdown_on_signal()
        // Watch deployments managed by us
        .owns(
            deployments,
            WatcherConfig::default().labels(&format!("{}={}", MANAGED_BY_LABEL, MANAGED_BY_VALUE)),
        )
        // Watch services managed by us
        .owns(
            services,
            WatcherConfig::default().labels(&format!("{}={}", MANAGED_BY_LABEL, MANAGED_BY_VALUE)),
        )
        // Watch configmaps managed by us
        .owns(
            configmaps,
            WatcherConfig::default().labels(&format!("{}={}", MANAGED_BY_LABEL, MANAGED_BY_VALUE)),
        )
        .run(
            |obj, ctx| async move { reconcile_gateway(obj, ctx).await },
            error_policy,
            ctx,
        )
        .for_each(|result| async move {
            match result {
                Ok((obj, action)) => {
                    debug!(
                        namespace = %obj.namespace.as_deref().unwrap_or("unknown"),
                        name = %obj.name,
                        ?action,
                        "Gateway reconciled successfully"
                    );
                }
                Err(error) => {
                    error!(%error, "Gateway reconciliation error");
                }
            }
        });

    controller.await;
}

/// Reconcile a single Gateway resource using the functional core
#[instrument(skip_all, fields(
    namespace = %gateway.namespace().unwrap_or_default(),
    name = %gateway.name_any()
))]
async fn reconcile_gateway(gateway: Arc<Gateway>, ctx: Arc<ControllerContext>) -> Result<Action> {
    let name = gateway.name_any();
    let namespace = gateway.namespace().unwrap_or_default();
    info!("Reconciling Gateway");

    // Build snapshot from cluster state
    let fetcher = SnapshotFetcher::new(ctx.client.clone());
    let snapshot = fetcher.snapshot_for_gateway(&namespace, &name).await?;

    // Pure reconciliation - compute what needs to be done
    let result = core::reconcile_gateway(&snapshot, &ctx.config, &namespace, &name);

    // Execute the computed effects
    let executor = ReconcileExecutor::new(ctx.client.clone());
    executor.execute(result).await
}

/// Error policy for Gateway reconciliation
fn error_policy(
    _obj: Arc<Gateway>,
    error: &ControllerError,
    ctx: Arc<ControllerContext>,
) -> Action {
    error!(%error, "Gateway reconciliation failed");
    Action::requeue(Duration::from_secs(ctx.config.error_requeue_secs))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gateway_crds::{
        GatewayListeners, GatewayListenersAllowedRoutes, GatewayListenersAllowedRoutesNamespaces,
        GatewayListenersAllowedRoutesNamespacesFrom, GatewayListenersTls, GatewayListenersTlsMode,
    };

    use crate::controller::config::{
        DataPlaneNames, GATEWAY_NAME_LABEL, MANAGED_BY_LABEL, MANAGED_BY_VALUE,
    };
    use crate::core::validate::{
        AllowedNamespaces, get_allowed_route_namespaces, validate_listeners,
    };

    fn create_http_listener(name: &str, port: i32) -> GatewayListeners {
        GatewayListeners {
            name: name.to_string(),
            port,
            protocol: "HTTP".to_string(),
            hostname: None,
            allowed_routes: None,
            tls: None,
        }
    }

    fn create_https_listener(name: &str, port: i32) -> GatewayListeners {
        GatewayListeners {
            name: name.to_string(),
            port,
            protocol: "HTTPS".to_string(),
            hostname: Some("example.com".to_string()),
            allowed_routes: None,
            tls: Some(GatewayListenersTls {
                mode: Some(GatewayListenersTlsMode::Terminate),
                certificate_refs: Some(vec![]),
                options: None,
            }),
        }
    }

    // ==========================================
    // Listener Validation Tests (Gateway Spec)
    // ==========================================

    /// Spec: HTTP protocol is valid for listeners
    #[test]
    fn test_validate_listeners_http() {
        let listeners = vec![create_http_listener("http", 80)];
        let result = validate_listeners(&listeners);
        assert!(result.all_valid);
        assert_eq!(result.listeners.len(), 1);
        assert!(result.listeners[0].valid);
    }

    /// Spec: HTTPS protocol requires TLS configuration
    #[test]
    fn test_validate_listeners_https_with_tls() {
        let listeners = vec![create_https_listener("https", 443)];
        let result = validate_listeners(&listeners);
        assert!(result.all_valid);
        assert!(result.listeners[0].valid);
    }

    /// Spec: HTTPS without TLS config should be invalid
    #[test]
    fn test_validate_listeners_https_without_tls() {
        let listeners = vec![GatewayListeners {
            name: "https".to_string(),
            port: 443,
            protocol: "HTTPS".to_string(),
            hostname: None,
            allowed_routes: None,
            tls: None, // Missing TLS config
        }];

        let result = validate_listeners(&listeners);
        assert!(!result.all_valid);
        assert!(!result.listeners[0].valid);
        assert_eq!(result.listeners[0].reason, "InvalidTLS");
    }

    /// Spec: HTTP with TLS config should be invalid
    #[test]
    fn test_validate_listeners_http_with_tls() {
        let listeners = vec![GatewayListeners {
            name: "http".to_string(),
            port: 80,
            protocol: "HTTP".to_string(),
            hostname: None,
            allowed_routes: None,
            tls: Some(GatewayListenersTls {
                mode: Some(GatewayListenersTlsMode::Terminate),
                certificate_refs: Some(vec![]),
                options: None,
            }),
        }];

        let result = validate_listeners(&listeners);
        assert!(!result.all_valid);
        assert!(!result.listeners[0].valid);
        assert_eq!(result.listeners[0].reason, "InvalidTLS");
    }

    /// Spec: Unsupported protocol should be rejected
    #[test]
    fn test_validate_listeners_invalid_protocol() {
        let listeners = vec![GatewayListeners {
            name: "invalid".to_string(),
            port: 80,
            protocol: "INVALID".to_string(),
            hostname: None,
            allowed_routes: None,
            tls: None,
        }];

        let result = validate_listeners(&listeners);
        assert!(!result.all_valid);
        assert!(!result.listeners[0].valid);
        assert_eq!(result.listeners[0].reason, "UnsupportedProtocol");
    }

    /// Spec: Protocol matching should be case-insensitive
    #[test]
    fn test_validate_listeners_protocol_case_insensitive() {
        let listeners = vec![
            GatewayListeners {
                name: "http1".to_string(),
                port: 80,
                protocol: "http".to_string(), // lowercase
                hostname: None,
                allowed_routes: None,
                tls: None,
            },
            GatewayListeners {
                name: "http2".to_string(),
                port: 8080,
                protocol: "Http".to_string(), // mixed case
                hostname: None,
                allowed_routes: None,
                tls: None,
            },
        ];

        let result = validate_listeners(&listeners);
        assert!(result.all_valid);
        assert!(result.listeners[0].valid);
        assert!(result.listeners[1].valid);
    }

    /// Spec: Port must be in valid range (1-65535)
    #[test]
    fn test_validate_listeners_port_range() {
        // Valid port
        let listeners = vec![create_http_listener("http", 8080)];
        let result = validate_listeners(&listeners);
        assert!(result.all_valid);

        // Port 0 is invalid
        let listeners = vec![create_http_listener("http", 0)];
        let result = validate_listeners(&listeners);
        assert!(!result.all_valid);
        assert_eq!(result.listeners[0].reason, "InvalidPort");

        // Negative port is invalid
        let listeners = vec![create_http_listener("http", -1)];
        let result = validate_listeners(&listeners);
        assert!(!result.all_valid);
        assert_eq!(result.listeners[0].reason, "InvalidPort");
    }

    /// Spec: Multiple listeners can be defined on a Gateway
    #[test]
    fn test_validate_multiple_listeners() {
        let listeners = vec![
            create_http_listener("http", 80),
            create_https_listener("https", 443),
            create_http_listener("http-alt", 8080),
        ];

        let result = validate_listeners(&listeners);
        assert!(result.all_valid);
        assert_eq!(result.listeners.len(), 3);
        assert!(result.listeners.iter().all(|s| s.valid));
    }

    /// Spec: One invalid listener should mark the whole Gateway invalid
    #[test]
    fn test_validate_listeners_partial_invalid() {
        let listeners = vec![
            create_http_listener("http", 80),
            GatewayListeners {
                name: "invalid".to_string(),
                port: 80,
                protocol: "GRPC".to_string(), // Not supported
                hostname: None,
                allowed_routes: None,
                tls: None,
            },
        ];

        let result = validate_listeners(&listeners);
        assert!(!result.all_valid);
        assert!(result.listeners[0].valid); // First listener is valid
        assert!(!result.listeners[1].valid); // Second listener is invalid
    }

    // ==========================================
    // AllowedNamespaces Tests (Gateway Spec)
    // ==========================================

    /// Spec: Default allowed namespaces is "Same" (same namespace as Gateway)
    #[test]
    fn test_allowed_namespaces_default() {
        let listener = create_http_listener("http", 80);
        let allowed = get_allowed_route_namespaces(&listener, "default");

        match allowed {
            AllowedNamespaces::Same(ns) => assert_eq!(ns, "default"),
            _ => panic!("Expected Same namespace"),
        }
    }

    /// Spec: "All" allows routes from any namespace
    #[test]
    fn test_allowed_namespaces_all() {
        let mut listener = create_http_listener("http", 80);
        listener.allowed_routes = Some(GatewayListenersAllowedRoutes {
            kinds: None,
            namespaces: Some(GatewayListenersAllowedRoutesNamespaces {
                from: Some(GatewayListenersAllowedRoutesNamespacesFrom::All),
                selector: None,
            }),
        });

        let allowed = get_allowed_route_namespaces(&listener, "default");
        assert!(matches!(allowed, AllowedNamespaces::All));
    }

    /// Spec: "Same" only allows routes from Gateway's namespace
    #[test]
    fn test_allowed_namespaces_same() {
        let mut listener = create_http_listener("http", 80);
        listener.allowed_routes = Some(GatewayListenersAllowedRoutes {
            kinds: None,
            namespaces: Some(GatewayListenersAllowedRoutesNamespaces {
                from: Some(GatewayListenersAllowedRoutesNamespacesFrom::Same),
                selector: None,
            }),
        });

        let allowed = get_allowed_route_namespaces(&listener, "my-namespace");
        match allowed {
            AllowedNamespaces::Same(ns) => assert_eq!(ns, "my-namespace"),
            _ => panic!("Expected Same namespace"),
        }
    }

    /// Spec: "Selector" allows routes from namespaces matching labels
    #[test]
    fn test_allowed_namespaces_selector() {
        use gateway_crds::GatewayListenersAllowedRoutesNamespacesSelector;

        let mut selector_labels = BTreeMap::new();
        selector_labels.insert("env".to_string(), "production".to_string());

        let mut listener = create_http_listener("http", 80);
        listener.allowed_routes = Some(GatewayListenersAllowedRoutes {
            kinds: None,
            namespaces: Some(GatewayListenersAllowedRoutesNamespaces {
                from: Some(GatewayListenersAllowedRoutesNamespacesFrom::Selector),
                selector: Some(GatewayListenersAllowedRoutesNamespacesSelector {
                    match_labels: Some(selector_labels.clone()),
                    match_expressions: None,
                }),
            }),
        });

        let allowed = get_allowed_route_namespaces(&listener, "default");
        match allowed {
            AllowedNamespaces::Selector { match_labels } => {
                assert_eq!(match_labels.get("env"), Some(&"production".to_string()));
            }
            _ => panic!("Expected Selector namespace"),
        }
    }

    // ==========================================
    // Data Plane Names Tests
    // ==========================================

    /// Spec: Data plane resources should have consistent naming
    #[test]
    fn test_data_plane_names() {
        let names = DataPlaneNames::new("default", "my-gateway");
        assert_eq!(names.deployment_name(), "multiway-dp-my-gateway");
        assert_eq!(names.configmap_name(), "multiway-config-my-gateway");
        assert_eq!(names.service_name(), "multiway-dp-my-gateway");
        assert_eq!(names.namespace(), "default");
    }

    /// Spec: Labels should include managed-by and gateway references
    #[test]
    fn test_data_plane_labels() {
        let names = DataPlaneNames::new("default", "my-gateway");
        let labels = names.labels();

        assert_eq!(
            labels.get(MANAGED_BY_LABEL),
            Some(&MANAGED_BY_VALUE.to_string())
        );
        assert_eq!(
            labels.get(GATEWAY_NAME_LABEL),
            Some(&"my-gateway".to_string())
        );
        assert_eq!(
            labels.get("gateway.networking.k8s.io/gateway-namespace"),
            Some(&"default".to_string())
        );
    }

    /// Spec: Selector labels should uniquely identify pods
    #[test]
    fn test_data_plane_selector_labels() {
        let names = DataPlaneNames::new("default", "my-gateway");
        let selector = names.selector_labels();

        // Selector should have gateway name
        assert_eq!(
            selector.get(GATEWAY_NAME_LABEL),
            Some(&"my-gateway".to_string())
        );

        // Selector should have component
        assert_eq!(
            selector.get("app.kubernetes.io/component"),
            Some(&"dataplane".to_string())
        );
    }
}
