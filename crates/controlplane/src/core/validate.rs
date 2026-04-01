//! Pure validation functions for Gateway API resources.
//!
//! All functions in this module are pure - they take input data and return
//! validation results without performing any I/O operations.

use std::collections::BTreeMap;

use gateway_crds::{
    Gateway, GatewayClass, GatewayListeners, GatewayListenersAllowedRoutesNamespacesFrom,
    HTTPRoute, HttpRouteParentRefs, HttpRouteRulesBackendRefs,
};

use super::snapshot::WorldSnapshot;
use crate::controller::config::CONTROLLER_NAME;

/// Supported features for this Gateway implementation
pub const SUPPORTED_FEATURES: &[&str] = &[
    "Gateway",
    "HTTPRoute",
    "HTTPRouteDestinationPortMatching",
    "HTTPRouteHostRewrite",
    "HTTPRouteMethodMatching",
    "HTTPRoutePathRedirect",
    "HTTPRoutePathRewrite",
    "HTTPRoutePortRedirect",
    "HTTPRouteQueryParamMatching",
    "HTTPRouteRequestHeaderModifier",
    "HTTPRouteRequestMirror",
    "HTTPRouteRequestRedirect",
    "HTTPRouteResponseHeaderModifier",
    "HTTPRouteSchemeRedirect",
];

// ============================================================================
// GatewayClass Validation
// ============================================================================

/// Result of validating a GatewayClass
#[derive(Debug, Clone)]
pub struct GatewayClassValidation {
    /// Whether the GatewayClass is accepted
    pub accepted: bool,
    /// Reason for the acceptance decision
    pub reason: &'static str,
    /// Detailed message
    pub message: String,
}

/// Validate a GatewayClass.
///
/// Returns validation result including whether it's accepted and why.
pub fn validate_gateway_class(gateway_class: &GatewayClass) -> GatewayClassValidation {
    // Check for parametersRef - we accept but don't use custom parameters
    if let Some(params_ref) = &gateway_class.spec.parameters_ref {
        tracing::debug!(
            group = %params_ref.group,
            kind = %params_ref.kind,
            name = %params_ref.name,
            "GatewayClass has parametersRef (not currently supported, will be ignored)"
        );
    }

    GatewayClassValidation {
        accepted: true,
        reason: "Accepted",
        message: "GatewayClass is accepted by the multiway controller".to_string(),
    }
}

/// Check if a GatewayClass is managed by this controller
pub fn is_our_gateway_class(gateway_class: &GatewayClass) -> bool {
    gateway_class.spec.controller_name == CONTROLLER_NAME
}

/// Check if a GatewayClass has been accepted (has Accepted=True condition)
pub fn is_gateway_class_accepted(gateway_class: &GatewayClass) -> bool {
    if !is_our_gateway_class(gateway_class) {
        return false;
    }

    gateway_class
        .status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|c| c.type_ == "Accepted" && c.status == "True")
        })
}

/// Get an accepted GatewayClass from the snapshot by name
pub fn get_accepted_gateway_class<'a>(
    snapshot: &'a WorldSnapshot,
    class_name: &str,
) -> Option<&'a GatewayClass> {
    snapshot
        .get_gateway_class(class_name)
        .filter(|gc| is_gateway_class_accepted(gc))
}

// ============================================================================
// Gateway Listener Validation
// ============================================================================

/// Result of validating a single listener
#[derive(Debug, Clone)]
pub struct ListenerValidationResult {
    /// Name of the listener
    pub name: String,
    /// Whether the listener is valid
    pub valid: bool,
    /// Reason for the validation result
    pub reason: &'static str,
    /// Detailed message (empty if valid)
    pub message: String,
}

/// Result of validating all listeners on a Gateway
#[derive(Debug, Clone)]
pub struct ListenerValidation {
    /// Whether all listeners are valid
    pub all_valid: bool,
    /// Validation results for each listener
    pub listeners: Vec<ListenerValidationResult>,
}

/// Validate all listeners on a Gateway.
///
/// Returns validation results for each listener.
pub fn validate_listeners(listeners: &[GatewayListeners]) -> ListenerValidation {
    let mut all_valid = true;
    let mut results = Vec::new();

    for listener in listeners {
        let result = validate_single_listener(listener);
        if !result.valid {
            all_valid = false;
        }
        results.push(result);
    }

    ListenerValidation {
        all_valid,
        listeners: results,
    }
}

/// Validate a single listener
fn validate_single_listener(listener: &GatewayListeners) -> ListenerValidationResult {
    let name = listener.name.clone();

    // Validate protocol
    match listener.protocol.to_uppercase().as_str() {
        "HTTP" => {
            if listener.tls.is_some() {
                return ListenerValidationResult {
                    name,
                    valid: false,
                    reason: "InvalidTLS",
                    message: "HTTP listeners should not have TLS configuration".to_string(),
                };
            }
        }
        "HTTPS" => {
            if listener.tls.is_none() {
                return ListenerValidationResult {
                    name,
                    valid: false,
                    reason: "InvalidTLS",
                    message: "HTTPS listeners require TLS configuration".to_string(),
                };
            }
        }
        protocol => {
            return ListenerValidationResult {
                name,
                valid: false,
                reason: "UnsupportedProtocol",
                message: format!("Protocol '{}' is not supported", protocol),
            };
        }
    }

    // Validate port range
    if listener.port <= 0 || listener.port > 65535 {
        return ListenerValidationResult {
            name,
            valid: false,
            reason: "InvalidPort",
            message: format!("Port {} is out of valid range (1-65535)", listener.port),
        };
    }

    ListenerValidationResult {
        name,
        valid: true,
        reason: "Accepted",
        message: String::new(),
    }
}

// ============================================================================
// HTTPRoute Validation
// ============================================================================

/// Represents which namespaces routes can come from
#[derive(Debug, Clone)]
pub enum AllowedNamespaces {
    /// All namespaces
    All,
    /// Same namespace as the Gateway
    Same(String),
    /// Namespaces matching a label selector
    Selector {
        match_labels: BTreeMap<String, String>,
    },
}

/// Get the allowed namespaces for routes on a listener
pub fn get_allowed_route_namespaces(
    listener: &GatewayListeners,
    gateway_namespace: &str,
) -> AllowedNamespaces {
    match &listener.allowed_routes {
        None => AllowedNamespaces::Same(gateway_namespace.to_string()),
        Some(allowed) => match &allowed.namespaces {
            None => AllowedNamespaces::Same(gateway_namespace.to_string()),
            Some(ns_config) => match &ns_config.from {
                None => AllowedNamespaces::Same(gateway_namespace.to_string()),
                Some(GatewayListenersAllowedRoutesNamespacesFrom::All) => AllowedNamespaces::All,
                Some(GatewayListenersAllowedRoutesNamespacesFrom::Same) => {
                    AllowedNamespaces::Same(gateway_namespace.to_string())
                }
                Some(GatewayListenersAllowedRoutesNamespacesFrom::Selector) => {
                    match &ns_config.selector {
                        Some(selector) => AllowedNamespaces::Selector {
                            match_labels: selector.match_labels.clone().unwrap_or_default(),
                        },
                        None => AllowedNamespaces::Same(gateway_namespace.to_string()),
                    }
                }
            },
        },
    }
}

/// Check if a namespace is allowed by the listener policy
pub fn is_namespace_allowed(allowed: &AllowedNamespaces, namespace: &str) -> bool {
    match allowed {
        AllowedNamespaces::All => true,
        AllowedNamespaces::Same(gw_ns) => namespace == gw_ns,
        AllowedNamespaces::Selector { match_labels } => {
            // Note: In a real implementation, we'd check namespace labels
            // For now, we're permissive if there's a selector with labels
            !match_labels.is_empty()
        }
    }
}

/// Result of validating an HTTPRoute parent reference
#[derive(Debug, Clone)]
pub struct ParentRefValidation {
    /// The parent reference being validated
    pub parent_ref: HttpRouteParentRefs,
    /// Whether the route is accepted by this parent
    pub accepted: bool,
    /// Reason for the acceptance decision
    pub reason: &'static str,
    /// Detailed message
    pub message: String,
    /// Names of listeners the route is attached to (if accepted)
    pub attached_listeners: Vec<String>,
}

/// Validate a parent reference for an HTTPRoute
pub fn validate_parent_ref(
    snapshot: &WorldSnapshot,
    route: &HTTPRoute,
    parent_ref: &HttpRouteParentRefs,
    route_namespace: &str,
) -> ParentRefValidation {
    let parent_namespace = parent_ref.namespace.as_deref().unwrap_or(route_namespace);
    let parent_name = &parent_ref.name;

    // Verify the parent is a Gateway
    let parent_kind = parent_ref.kind.as_deref().unwrap_or("Gateway");
    let parent_group = parent_ref
        .group
        .as_deref()
        .unwrap_or("gateway.networking.k8s.io");

    if parent_group != "gateway.networking.k8s.io" || parent_kind != "Gateway" {
        return ParentRefValidation {
            parent_ref: parent_ref.clone(),
            accepted: false,
            reason: "InvalidParentRef",
            message: format!("Unsupported parent type: {}/{}", parent_group, parent_kind),
            attached_listeners: vec![],
        };
    }

    // Get the Gateway from snapshot
    let gateway = match snapshot.get_gateway(parent_namespace, parent_name) {
        Some(gw) => gw,
        None => {
            return ParentRefValidation {
                parent_ref: parent_ref.clone(),
                accepted: false,
                reason: "NoMatchingParent",
                message: format!("Gateway {}/{} not found", parent_namespace, parent_name),
                attached_listeners: vec![],
            };
        }
    };

    // Check if the GatewayClass is ours and accepted
    if get_accepted_gateway_class(snapshot, &gateway.spec.gateway_class_name).is_none() {
        return ParentRefValidation {
            parent_ref: parent_ref.clone(),
            accepted: false,
            reason: "NotAllowedByGateway",
            message: "Gateway's GatewayClass is not managed by this controller".to_string(),
            attached_listeners: vec![],
        };
    }

    // Find matching listeners
    let section_name = parent_ref.section_name.as_deref();
    let matching_listeners = find_matching_listeners(gateway, section_name, parent_ref.port);

    if matching_listeners.is_empty() {
        return ParentRefValidation {
            parent_ref: parent_ref.clone(),
            accepted: false,
            reason: "NoMatchingListenerHostname",
            message: "No matching listener found on Gateway".to_string(),
            attached_listeners: vec![],
        };
    }

    // Check namespace permissions
    let gateway_namespace = gateway.metadata.namespace.as_deref().unwrap_or_default();
    let mut allowed = false;

    for listener in &matching_listeners {
        let allowed_ns = get_allowed_route_namespaces(listener, gateway_namespace);
        if is_namespace_allowed(&allowed_ns, route_namespace) {
            allowed = true;
            break;
        }
    }

    if !allowed && route_namespace != gateway_namespace {
        // Check for ReferenceGrant
        if !snapshot.is_gateway_reference_allowed(route_namespace, gateway_namespace) {
            return ParentRefValidation {
                parent_ref: parent_ref.clone(),
                accepted: false,
                reason: "RefNotPermitted",
                message: format!(
                    "Route namespace {} is not allowed by Gateway listener policies",
                    route_namespace
                ),
                attached_listeners: vec![],
            };
        }
    }

    // Validate backends
    let rules = route.spec.rules.as_ref().cloned().unwrap_or_default();
    for rule in &rules {
        if let Some(backends) = &rule.backend_refs {
            for backend in backends {
                if let Some(error) = validate_backend(snapshot, route_namespace, backend) {
                    return ParentRefValidation {
                        parent_ref: parent_ref.clone(),
                        accepted: false,
                        reason: error.reason,
                        message: error.message,
                        attached_listeners: vec![],
                    };
                }
            }
        }
    }

    // Route is accepted
    let attached_listener_names = matching_listeners.iter().map(|l| l.name.clone()).collect();

    ParentRefValidation {
        parent_ref: parent_ref.clone(),
        accepted: true,
        reason: "Accepted",
        message: "Route is accepted by Gateway".to_string(),
        attached_listeners: attached_listener_names,
    }
}

/// Find listeners that match the parent reference criteria
pub fn find_matching_listeners<'a>(
    gateway: &'a Gateway,
    section_name: Option<&str>,
    port: Option<i32>,
) -> Vec<&'a GatewayListeners> {
    gateway
        .spec
        .listeners
        .iter()
        .filter(|l| {
            // Match by section name if specified
            if let Some(name) = section_name
                && l.name != name
            {
                return false;
            }
            // Match by port if specified
            if let Some(p) = port
                && l.port != p
            {
                return false;
            }
            // Must be HTTP or HTTPS protocol
            let protocol = l.protocol.to_uppercase();
            protocol == "HTTP" || protocol == "HTTPS"
        })
        .collect()
}

/// Backend validation error
#[derive(Debug, Clone)]
pub struct BackendValidationError {
    /// Reason for the error
    pub reason: &'static str,
    /// Detailed message
    pub message: String,
}

/// Validate a backend reference
pub fn validate_backend(
    snapshot: &WorldSnapshot,
    route_namespace: &str,
    backend: &HttpRouteRulesBackendRefs,
) -> Option<BackendValidationError> {
    let backend_namespace = backend.namespace.as_deref().unwrap_or(route_namespace);
    let backend_kind = backend.kind.as_deref().unwrap_or("Service");
    let backend_group = backend.group.as_deref().unwrap_or("");

    // Only Service backends are supported
    if !backend_group.is_empty() || backend_kind != "Service" {
        return Some(BackendValidationError {
            reason: "InvalidBackendRef",
            message: format!(
                "Unsupported backend type: {}/{}",
                backend_group, backend_kind
            ),
        });
    }

    // Check if the service exists
    if !snapshot.service_exists(backend_namespace, &backend.name) {
        return Some(BackendValidationError {
            reason: "BackendNotFound",
            message: format!("Service {}/{} not found", backend_namespace, backend.name),
        });
    }

    // Check cross-namespace reference
    if backend_namespace != route_namespace
        && !snapshot.is_service_reference_allowed(route_namespace, backend_namespace)
    {
        return Some(BackendValidationError {
            reason: "RefNotPermitted",
            message: format!(
                "Cross-namespace reference to Service {}/{} requires ReferenceGrant",
                backend_namespace, backend.name
            ),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::snapshot::WorldSnapshotBuilder;
    use gateway_crds::{
        GatewayClassSpec, GatewayListenersTls, GatewayListenersTlsMode, GatewaySpec, HttpRouteSpec,
        ReferenceGrantFrom, ReferenceGrantSpec, ReferenceGrantTo,
    };
    use k8s_openapi::api::core::v1::Service;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    // ========================================
    // GatewayClass Validation Tests
    // ========================================

    fn create_gateway_class(name: &str, controller: &str) -> GatewayClass {
        GatewayClass {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec: GatewayClassSpec {
                controller_name: controller.to_string(),
                description: None,
                parameters_ref: None,
            },
            status: None,
        }
    }

    #[test]
    fn test_validate_gateway_class_accepted() {
        let gc = create_gateway_class("multiway", CONTROLLER_NAME);
        let result = validate_gateway_class(&gc);
        assert!(result.accepted);
        assert_eq!(result.reason, "Accepted");
    }

    #[test]
    fn test_is_our_gateway_class() {
        let ours = create_gateway_class("multiway", CONTROLLER_NAME);
        assert!(is_our_gateway_class(&ours));

        let other = create_gateway_class("other", "other.io/controller");
        assert!(!is_our_gateway_class(&other));
    }

    // ========================================
    // Listener Validation Tests
    // ========================================

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

    #[test]
    fn test_validate_http_listener() {
        let listeners = vec![create_http_listener("http", 80)];
        let result = validate_listeners(&listeners);
        assert!(result.all_valid);
        assert!(result.listeners[0].valid);
    }

    #[test]
    fn test_validate_https_listener() {
        let listeners = vec![create_https_listener("https", 443)];
        let result = validate_listeners(&listeners);
        assert!(result.all_valid);
    }

    #[test]
    fn test_validate_https_without_tls() {
        let listeners = vec![GatewayListeners {
            name: "https".to_string(),
            port: 443,
            protocol: "HTTPS".to_string(),
            hostname: None,
            allowed_routes: None,
            tls: None,
        }];
        let result = validate_listeners(&listeners);
        assert!(!result.all_valid);
        assert_eq!(result.listeners[0].reason, "InvalidTLS");
    }

    #[test]
    fn test_validate_http_with_tls() {
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
        assert_eq!(result.listeners[0].reason, "InvalidTLS");
    }

    #[test]
    fn test_validate_unsupported_protocol() {
        let listeners = vec![GatewayListeners {
            name: "grpc".to_string(),
            port: 9090,
            protocol: "GRPC".to_string(),
            hostname: None,
            allowed_routes: None,
            tls: None,
        }];
        let result = validate_listeners(&listeners);
        assert!(!result.all_valid);
        assert_eq!(result.listeners[0].reason, "UnsupportedProtocol");
    }

    #[test]
    fn test_validate_invalid_port() {
        let listeners = vec![create_http_listener("http", 0)];
        let result = validate_listeners(&listeners);
        assert!(!result.all_valid);
        assert_eq!(result.listeners[0].reason, "InvalidPort");
    }

    // ========================================
    // Parent Reference Validation Tests
    // ========================================

    fn create_accepted_gateway_class(name: &str) -> GatewayClass {
        GatewayClass {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec: GatewayClassSpec {
                controller_name: CONTROLLER_NAME.to_string(),
                description: None,
                parameters_ref: None,
            },
            status: Some(gateway_crds::GatewayClassStatus {
                conditions: Some(vec![
                    k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition {
                        type_: "Accepted".to_string(),
                        status: "True".to_string(),
                        observed_generation: Some(1),
                        last_transition_time: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                            chrono::Utc::now(),
                        ),
                        reason: "Accepted".to_string(),
                        message: "Accepted".to_string(),
                    },
                ]),
            }),
        }
    }

    fn create_gateway(ns: &str, name: &str, class_name: &str) -> Gateway {
        Gateway {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(ns.to_string()),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: class_name.to_string(),
                listeners: vec![create_http_listener("http", 80)],
                addresses: None,
                infrastructure: None,
            },
            status: None,
        }
    }

    fn create_httproute(ns: &str, name: &str, gateway_ns: &str, gateway_name: &str) -> HTTPRoute {
        HTTPRoute {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(ns.to_string()),
                ..Default::default()
            },
            spec: HttpRouteSpec {
                parent_refs: Some(vec![HttpRouteParentRefs {
                    group: Some("gateway.networking.k8s.io".to_string()),
                    kind: Some("Gateway".to_string()),
                    namespace: Some(gateway_ns.to_string()),
                    name: gateway_name.to_string(),
                    section_name: None,
                    port: None,
                }]),
                hostnames: None,
                rules: None,
            },
            status: None,
        }
    }

    fn create_service(ns: &str, name: &str) -> Service {
        Service {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(ns.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_parent_ref_gateway_not_found() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(create_accepted_gateway_class("multiway"))
            .build();

        let route = create_httproute("default", "my-route", "default", "nonexistent");
        let parent_ref = route.spec.parent_refs.as_ref().unwrap()[0].clone();

        let result = validate_parent_ref(&snapshot, &route, &parent_ref, "default");
        assert!(!result.accepted);
        assert_eq!(result.reason, "NoMatchingParent");
    }

    #[test]
    fn test_parent_ref_gateway_class_not_accepted() {
        // Gateway class not marked as accepted
        let gc = create_gateway_class("multiway", CONTROLLER_NAME);
        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(gc)
            .with_gateway(create_gateway("default", "my-gateway", "multiway"))
            .build();

        let route = create_httproute("default", "my-route", "default", "my-gateway");
        let parent_ref = route.spec.parent_refs.as_ref().unwrap()[0].clone();

        let result = validate_parent_ref(&snapshot, &route, &parent_ref, "default");
        assert!(!result.accepted);
        assert_eq!(result.reason, "NotAllowedByGateway");
    }

    #[test]
    fn test_parent_ref_accepted() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(create_accepted_gateway_class("multiway"))
            .with_gateway(create_gateway("default", "my-gateway", "multiway"))
            .build();

        let route = create_httproute("default", "my-route", "default", "my-gateway");
        let parent_ref = route.spec.parent_refs.as_ref().unwrap()[0].clone();

        let result = validate_parent_ref(&snapshot, &route, &parent_ref, "default");
        assert!(result.accepted);
        assert_eq!(result.reason, "Accepted");
        assert!(!result.attached_listeners.is_empty());
    }

    #[test]
    fn test_cross_namespace_without_grant() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(create_accepted_gateway_class("multiway"))
            .with_gateway(create_gateway("gateway-ns", "my-gateway", "multiway"))
            .build();

        let route = create_httproute("route-ns", "my-route", "gateway-ns", "my-gateway");
        let parent_ref = route.spec.parent_refs.as_ref().unwrap()[0].clone();

        let result = validate_parent_ref(&snapshot, &route, &parent_ref, "route-ns");
        assert!(!result.accepted);
        assert_eq!(result.reason, "RefNotPermitted");
    }

    #[test]
    fn test_cross_namespace_with_grant() {
        let grant = gateway_crds::ReferenceGrant {
            metadata: ObjectMeta {
                name: Some("allow-routes".to_string()),
                namespace: Some("gateway-ns".to_string()),
                ..Default::default()
            },
            spec: ReferenceGrantSpec {
                from: vec![ReferenceGrantFrom {
                    group: "gateway.networking.k8s.io".to_string(),
                    kind: "HTTPRoute".to_string(),
                    namespace: "route-ns".to_string(),
                }],
                to: vec![ReferenceGrantTo {
                    group: "gateway.networking.k8s.io".to_string(),
                    kind: "Gateway".to_string(),
                    name: None,
                }],
            },
        };

        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(create_accepted_gateway_class("multiway"))
            .with_gateway(create_gateway("gateway-ns", "my-gateway", "multiway"))
            .with_reference_grant(grant)
            .build();

        let route = create_httproute("route-ns", "my-route", "gateway-ns", "my-gateway");
        let parent_ref = route.spec.parent_refs.as_ref().unwrap()[0].clone();

        let result = validate_parent_ref(&snapshot, &route, &parent_ref, "route-ns");
        assert!(result.accepted);
    }

    // ========================================
    // Backend Validation Tests
    // ========================================

    #[test]
    fn test_validate_backend_not_found() {
        let snapshot = WorldSnapshotBuilder::new().build();
        let backend = HttpRouteRulesBackendRefs {
            group: None,
            kind: None,
            name: "nonexistent".to_string(),
            namespace: None,
            port: Some(80),
            weight: None,
            filters: None,
        };

        let error = validate_backend(&snapshot, "default", &backend);
        assert!(error.is_some());
        assert_eq!(error.unwrap().reason, "BackendNotFound");
    }

    #[test]
    fn test_validate_backend_exists() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_service(create_service("default", "my-service"))
            .build();

        let backend = HttpRouteRulesBackendRefs {
            group: None,
            kind: None,
            name: "my-service".to_string(),
            namespace: None,
            port: Some(80),
            weight: None,
            filters: None,
        };

        let error = validate_backend(&snapshot, "default", &backend);
        assert!(error.is_none());
    }

    #[test]
    fn test_validate_backend_unsupported_type() {
        let snapshot = WorldSnapshotBuilder::new().build();
        let backend = HttpRouteRulesBackendRefs {
            group: Some("custom.io".to_string()),
            kind: Some("CustomBackend".to_string()),
            name: "my-backend".to_string(),
            namespace: None,
            port: Some(80),
            weight: None,
            filters: None,
        };

        let error = validate_backend(&snapshot, "default", &backend);
        assert!(error.is_some());
        assert_eq!(error.unwrap().reason, "InvalidBackendRef");
    }

    #[test]
    fn test_validate_backend_cross_namespace_without_grant() {
        let snapshot = WorldSnapshotBuilder::new()
            .with_service(create_service("backend-ns", "my-service"))
            .build();

        let backend = HttpRouteRulesBackendRefs {
            group: None,
            kind: None,
            name: "my-service".to_string(),
            namespace: Some("backend-ns".to_string()),
            port: Some(80),
            weight: None,
            filters: None,
        };

        let error = validate_backend(&snapshot, "route-ns", &backend);
        assert!(error.is_some());
        assert_eq!(error.unwrap().reason, "RefNotPermitted");
    }
}
