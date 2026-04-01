//! Configuration types for Gateway data plane
//!
//! This module defines the configuration schema used to communicate between
//! the control plane and data plane via ConfigMaps. The data plane reads this
//! configuration to set up HTTP routing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The controller name that this implementation responds to
pub const CONTROLLER_NAME: &str = "io.multiway/gateway-controller";

/// Label used to identify resources managed by this controller
pub const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";

/// Value for the managed-by label
pub const MANAGED_BY_VALUE: &str = "multiway-gateway-controller";

/// Label for the gateway name
pub const GATEWAY_NAME_LABEL: &str = "gateway.networking.k8s.io/gateway-name";

/// Label for the gateway namespace
pub const GATEWAY_NAMESPACE_LABEL: &str = "gateway.networking.k8s.io/gateway-namespace";

/// ConfigMap key for the gateway configuration
pub const CONFIG_KEY: &str = "config.json";

/// The complete gateway configuration stored in a ConfigMap
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GatewayConfig {
    /// Version of the configuration schema
    pub version: String,
    /// The gateway this configuration belongs to
    pub gateway: GatewayRef,
    /// List of listeners configured on this gateway
    pub listeners: Vec<ListenerConfig>,
    /// List of routes attached to this gateway
    pub routes: Vec<RouteConfig>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            version: "v1".to_string(),
            gateway: GatewayRef::default(),
            listeners: Vec::new(),
            routes: Vec::new(),
        }
    }
}

impl GatewayConfig {
    /// Create a new gateway configuration
    pub fn new(gateway_namespace: impl Into<String>, gateway_name: impl Into<String>) -> Self {
        Self {
            version: "v1".to_string(),
            gateway: GatewayRef {
                namespace: gateway_namespace.into(),
                name: gateway_name.into(),
            },
            listeners: Vec::new(),
            routes: Vec::new(),
        }
    }

    /// Add a listener to the configuration
    pub fn add_listener(&mut self, listener: ListenerConfig) {
        self.listeners.push(listener);
    }

    /// Add a route to the configuration
    pub fn add_route(&mut self, route: RouteConfig) {
        self.routes.push(route);
    }

    /// Get all routes for a specific listener
    pub fn routes_for_listener(&self, listener_name: &str) -> Vec<&RouteConfig> {
        self.routes
            .iter()
            .filter(|r| r.attached_listeners.contains(&listener_name.to_string()))
            .collect()
    }

    /// Serialize to JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from JSON
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// Reference to a Gateway resource
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GatewayRef {
    /// Namespace of the gateway
    pub namespace: String,
    /// Name of the gateway
    pub name: String,
}

/// Configuration for a single listener
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListenerConfig {
    /// Name of the listener (from Gateway spec)
    pub name: String,
    /// Port to listen on
    pub port: u16,
    /// Protocol (HTTP or HTTPS)
    pub protocol: Protocol,
    /// Hostname to match (optional, None means all hosts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// TLS configuration (for HTTPS listeners)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
}

impl ListenerConfig {
    /// Create a new HTTP listener configuration
    pub fn http(name: impl Into<String>, port: u16, hostname: Option<String>) -> Self {
        Self {
            name: name.into(),
            port,
            protocol: Protocol::Http,
            hostname,
            tls: None,
        }
    }

    /// Create a new HTTPS listener configuration
    pub fn https(
        name: impl Into<String>,
        port: u16,
        hostname: Option<String>,
        tls: TlsConfig,
    ) -> Self {
        Self {
            name: name.into(),
            port,
            protocol: Protocol::Https,
            hostname,
            tls: Some(tls),
        }
    }
}

/// Protocol supported by listeners
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Protocol {
    Http,
    Https,
}

impl std::str::FromStr for Protocol {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "HTTP" => Ok(Protocol::Http),
            "HTTPS" => Ok(Protocol::Https),
            _ => Err(()),
        }
    }
}

/// TLS configuration for HTTPS listeners
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TlsConfig {
    /// TLS mode (Terminate or Passthrough)
    pub mode: TlsMode,
    /// Certificate references
    pub certificates: Vec<CertificateRef>,
    /// Inline certificates with PEM data for direct use by the proxy.
    ///
    /// The control plane resolves `CertificateRef` secrets and populates
    /// these inline certificates before writing the config to the ConfigMap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_certificates: Vec<InlineCertificate>,
}

/// TLS termination mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    Terminate,
    Passthrough,
}

/// Reference to a TLS certificate
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CertificateRef {
    /// Namespace of the secret
    pub namespace: String,
    /// Name of the secret
    pub name: String,
}

/// Inline TLS certificate with actual PEM data.
///
/// The control plane resolves `CertificateRef` Kubernetes Secret references
/// and populates these inline certificates so the data plane can build a
/// TLS configuration directly without Kubernetes API access.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InlineCertificate {
    /// PEM-encoded certificate chain (leaf first, then intermediates).
    pub cert_pem: String,
    /// PEM-encoded private key (PKCS#1, PKCS#8, or SEC1).
    pub key_pem: String,
    /// Optional hostnames this certificate covers (for SNI routing).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hostnames: Vec<String>,
}

/// Configuration for an HTTP route
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RouteConfig {
    /// Name of the route resource
    pub name: String,
    /// Namespace of the route resource
    pub namespace: String,
    /// Hostnames this route matches
    #[serde(default)]
    pub hostnames: Vec<String>,
    /// Listeners this route is attached to
    #[serde(default)]
    pub attached_listeners: Vec<String>,
    /// Routing rules
    pub rules: Vec<RouteRule>,
}

impl RouteConfig {
    /// Create a new route configuration
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
            hostnames: Vec::new(),
            attached_listeners: Vec::new(),
            rules: Vec::new(),
        }
    }
}

/// A single routing rule
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RouteRule {
    /// Optional rule name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Match conditions (OR semantics between matches)
    #[serde(default)]
    pub matches: Vec<RouteMatch>,
    /// Filters to apply
    #[serde(default)]
    pub filters: Vec<RouteFilter>,
    /// Backends to forward to
    #[serde(default)]
    pub backends: Vec<BackendRef>,
    /// Request timeout
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<TimeoutConfig>,
}

impl Default for RouteRule {
    fn default() -> Self {
        Self {
            name: None,
            matches: vec![RouteMatch::default()],
            filters: Vec::new(),
            backends: Vec::new(),
            timeout: None,
        }
    }
}

/// Match conditions for a route rule
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RouteMatch {
    /// Path match condition
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathMatch>,
    /// Header match conditions (AND semantics)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HeaderMatch>,
    /// Query parameter match conditions (AND semantics)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_params: Vec<QueryParamMatch>,
    /// HTTP method to match
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

/// Path matching configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathMatch {
    /// Type of path match
    #[serde(rename = "type")]
    pub match_type: PathMatchType,
    /// Value to match against
    pub value: String,
}

impl Default for PathMatch {
    fn default() -> Self {
        Self {
            match_type: PathMatchType::PathPrefix,
            value: "/".to_string(),
        }
    }
}

/// Type of path matching
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PathMatchType {
    Exact,
    PathPrefix,
    RegularExpression,
}

/// Header matching configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeaderMatch {
    /// Header name
    pub name: String,
    /// Type of match
    #[serde(rename = "type")]
    pub match_type: HeaderMatchType,
    /// Value to match against
    pub value: String,
}

/// Type of header matching
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HeaderMatchType {
    Exact,
    RegularExpression,
}

/// Query parameter matching configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryParamMatch {
    /// Parameter name
    pub name: String,
    /// Type of match
    #[serde(rename = "type")]
    pub match_type: QueryParamMatchType,
    /// Value to match against
    pub value: String,
}

/// Type of query parameter matching
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryParamMatchType {
    Exact,
    RegularExpression,
}

/// Filter to apply to requests/responses
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum RouteFilter {
    /// Modify request headers
    RequestHeaderModifier {
        #[serde(default)]
        add: Vec<HeaderValue>,
        #[serde(default)]
        set: Vec<HeaderValue>,
        #[serde(default)]
        remove: Vec<String>,
    },
    /// Modify response headers
    ResponseHeaderModifier {
        #[serde(default)]
        add: Vec<HeaderValue>,
        #[serde(default)]
        set: Vec<HeaderValue>,
        #[serde(default)]
        remove: Vec<String>,
    },
    /// Redirect the request
    RequestRedirect {
        #[serde(skip_serializing_if = "Option::is_none")]
        scheme: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hostname: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<PathModifier>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status_code: Option<u16>,
    },
    /// Rewrite the URL
    URLRewrite {
        #[serde(skip_serializing_if = "Option::is_none")]
        hostname: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<PathModifier>,
    },
    /// Mirror request to another backend
    RequestMirror {
        backend: BackendRef,
        #[serde(skip_serializing_if = "Option::is_none")]
        percent: Option<u32>,
    },
}

/// Header name-value pair
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeaderValue {
    pub name: String,
    pub value: String,
}

/// Path modification configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum PathModifier {
    ReplaceFullPath { value: String },
    ReplacePrefixMatch { value: String },
}

/// Protocol used to connect to a backend.
///
/// Defaults to `Http1` for backward compatibility. When set to `H2c`, the
/// proxy connects to the backend using HTTP/2 prior knowledge (cleartext h2c).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackendProtocol {
    /// HTTP/1.1 (default).
    #[default]
    Http1,
    /// HTTP/2 cleartext (h2c) — prior knowledge mode.
    H2c,
}

/// Reference to a backend service
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackendRef {
    /// Namespace of the service
    pub namespace: String,
    /// Name of the service
    pub name: String,
    /// Port to connect to
    pub port: u16,
    /// Weight for load balancing (default: 1)
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// Protocol to use when connecting to this backend
    #[serde(default)]
    pub protocol: BackendProtocol,
}

fn default_weight() -> u32 {
    1
}

impl BackendRef {
    /// Create a new backend reference
    pub fn new(namespace: impl Into<String>, name: impl Into<String>, port: u16) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
            port,
            weight: 1,
            protocol: BackendProtocol::default(),
        }
    }

    /// Set the weight for this backend
    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }
}

/// Timeout configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeoutConfig {
    /// Total request timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<f64>,
    /// Backend request timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_request: Option<f64>,
}

/// Resource names for data plane components
pub struct DataPlaneNames {
    gateway_namespace: String,
    gateway_name: String,
}

impl DataPlaneNames {
    /// Create a new DataPlaneNames instance
    pub fn new(gateway_namespace: impl Into<String>, gateway_name: impl Into<String>) -> Self {
        Self {
            gateway_namespace: gateway_namespace.into(),
            gateway_name: gateway_name.into(),
        }
    }

    /// Get the deployment name for this gateway's data plane
    pub fn deployment_name(&self) -> String {
        format!("multiway-dp-{}", self.gateway_name)
    }

    /// Get the service name for this gateway's data plane
    pub fn service_name(&self) -> String {
        format!("multiway-dp-{}", self.gateway_name)
    }

    /// Get the ConfigMap name for this gateway's configuration
    pub fn configmap_name(&self) -> String {
        format!("multiway-config-{}", self.gateway_name)
    }

    /// Get the ServiceAccount name for this gateway's data plane
    pub fn serviceaccount_name(&self) -> String {
        format!("multiway-dp-{}", self.gateway_name)
    }

    /// Get the Role name for this gateway's data plane
    pub fn role_name(&self) -> String {
        format!("multiway-dp-{}", self.gateway_name)
    }

    /// Get the RoleBinding name for this gateway's data plane
    pub fn rolebinding_name(&self) -> String {
        format!("multiway-dp-{}", self.gateway_name)
    }

    /// Get the namespace for the data plane
    pub fn namespace(&self) -> &str {
        &self.gateway_namespace
    }

    /// Get standard labels for data plane resources
    pub fn labels(&self) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::new();
        labels.insert(MANAGED_BY_LABEL.to_string(), MANAGED_BY_VALUE.to_string());
        labels.insert(GATEWAY_NAME_LABEL.to_string(), self.gateway_name.clone());
        labels.insert(
            GATEWAY_NAMESPACE_LABEL.to_string(),
            self.gateway_namespace.clone(),
        );
        labels.insert(
            "app.kubernetes.io/component".to_string(),
            "dataplane".to_string(),
        );
        labels.insert("app.kubernetes.io/name".to_string(), "multiway".to_string());
        labels
    }

    /// Get selector labels for the deployment
    pub fn selector_labels(&self) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::new();
        labels.insert(GATEWAY_NAME_LABEL.to_string(), self.gateway_name.clone());
        labels.insert(
            GATEWAY_NAMESPACE_LABEL.to_string(),
            self.gateway_namespace.clone(),
        );
        labels.insert(
            "app.kubernetes.io/component".to_string(),
            "dataplane".to_string(),
        );
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_config_serialization() {
        let mut config = GatewayConfig::new("default", "my-gateway");
        config.add_listener(ListenerConfig::http(
            "http",
            80,
            Some("example.com".to_string()),
        ));
        config.add_route(RouteConfig {
            name: "my-route".to_string(),
            namespace: "default".to_string(),
            hostnames: vec!["example.com".to_string()],
            attached_listeners: vec!["http".to_string()],
            rules: vec![RouteRule {
                name: None,
                matches: vec![RouteMatch {
                    path: Some(PathMatch {
                        match_type: PathMatchType::PathPrefix,
                        value: "/api".to_string(),
                    }),
                    ..Default::default()
                }],
                filters: vec![],
                backends: vec![BackendRef::new("default", "my-service", 8080)],
                timeout: None,
            }],
        });

        let json = config.to_json().unwrap();
        let deserialized = GatewayConfig::from_json(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_data_plane_names() {
        let names = DataPlaneNames::new("my-namespace", "my-gateway");
        assert_eq!(names.deployment_name(), "multiway-dp-my-gateway");
        assert_eq!(names.service_name(), "multiway-dp-my-gateway");
        assert_eq!(names.configmap_name(), "multiway-config-my-gateway");
        assert_eq!(names.namespace(), "my-namespace");
    }

    #[test]
    fn test_route_filter_serialization() {
        let filter = RouteFilter::RequestHeaderModifier {
            add: vec![HeaderValue {
                name: "X-Custom".to_string(),
                value: "value".to_string(),
            }],
            set: vec![],
            remove: vec!["X-Remove".to_string()],
        };

        let json = serde_json::to_string(&filter).unwrap();
        let deserialized: RouteFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, deserialized);
    }

    // -- Cross-deserialization tests (control plane ↔ proxy-core) -------------

    /// Serialize from control plane GatewayConfig, deserialize into proxy-core ProxyConfig.
    #[test]
    fn controlplane_to_proxy_core_basic() {
        let mut config = GatewayConfig::new("default", "my-gateway");
        config.add_listener(ListenerConfig::http(
            "http",
            80,
            Some("example.com".to_string()),
        ));
        config.add_route(RouteConfig {
            name: "my-route".to_string(),
            namespace: "default".to_string(),
            hostnames: vec!["example.com".to_string()],
            attached_listeners: vec!["http".to_string()],
            rules: vec![RouteRule {
                name: None,
                matches: vec![RouteMatch {
                    path: Some(PathMatch {
                        match_type: PathMatchType::PathPrefix,
                        value: "/api".to_string(),
                    }),
                    ..Default::default()
                }],
                filters: vec![],
                backends: vec![BackendRef::new("default", "my-service", 8080)],
                timeout: None,
            }],
        });

        let json = config.to_json().unwrap();
        let proxy_config: proxy_core::config::ProxyConfig =
            serde_json::from_str(&json).expect("proxy-core should deserialize control plane JSON");

        assert_eq!(proxy_config.version, "v1");
        assert_eq!(proxy_config.gateway.name, "my-gateway");
        assert_eq!(proxy_config.listeners.len(), 1);
        assert_eq!(proxy_config.routes.len(), 1);
        assert_eq!(
            proxy_config.routes[0].rules[0].backends[0].name,
            "my-service"
        );
        // Protocol defaults to Http1 when not specified
        assert_eq!(
            proxy_config.routes[0].rules[0].backends[0].protocol,
            proxy_core::config::BackendProtocol::Http1
        );
    }

    /// Serialize from proxy-core ProxyConfig, deserialize into control plane GatewayConfig.
    #[test]
    fn proxy_core_to_controlplane_basic() {
        let proxy_config = proxy_core::config::ProxyConfig {
            version: "v1".to_string(),
            gateway: proxy_core::config::GatewayRef {
                namespace: "default".to_string(),
                name: "my-gateway".to_string(),
            },
            listeners: vec![proxy_core::config::ListenerConfig::http(
                "http",
                80,
                Some("example.com".to_string()),
            )],
            routes: vec![proxy_core::config::RouteConfig {
                name: "my-route".to_string(),
                namespace: "default".to_string(),
                hostnames: vec!["example.com".to_string()],
                attached_listeners: vec!["http".to_string()],
                rules: vec![proxy_core::config::RouteRule {
                    name: None,
                    matches: vec![proxy_core::config::RouteMatch {
                        path: Some(proxy_core::config::PathMatch {
                            match_type: proxy_core::config::PathMatchType::PathPrefix,
                            value: "/api".to_string(),
                        }),
                        ..Default::default()
                    }],
                    filters: vec![],
                    backends: vec![proxy_core::config::Backend::new(
                        "default",
                        "my-service",
                        8080,
                    )],
                    timeout: None,
                }],
            }],
        };

        let json = proxy_config.to_json().unwrap();
        let gw_config: GatewayConfig =
            serde_json::from_str(&json).expect("control plane should deserialize proxy-core JSON");

        assert_eq!(gw_config.version, "v1");
        assert_eq!(gw_config.gateway.name, "my-gateway");
        assert_eq!(gw_config.listeners.len(), 1);
        assert_eq!(gw_config.routes.len(), 1);
        assert_eq!(gw_config.routes[0].rules[0].backends[0].name, "my-service");
        assert_eq!(
            gw_config.routes[0].rules[0].backends[0].protocol,
            BackendProtocol::Http1
        );
    }

    /// Cross-deserialization with TLS and inline certificates.
    #[test]
    fn controlplane_to_proxy_core_tls_with_inline_certs() {
        let mut config = GatewayConfig::new("default", "secure-gw");
        config.add_listener(ListenerConfig::https(
            "https",
            443,
            Some("secure.example.com".to_string()),
            TlsConfig {
                mode: TlsMode::Terminate,
                certificates: vec![CertificateRef {
                    namespace: "default".to_string(),
                    name: "tls-secret".to_string(),
                }],
                inline_certificates: vec![InlineCertificate {
                    cert_pem: "-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----"
                        .to_string(),
                    key_pem: "-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----"
                        .to_string(),
                    hostnames: vec!["secure.example.com".to_string()],
                }],
            },
        ));

        let json = config.to_json().unwrap();
        let proxy_config: proxy_core::config::ProxyConfig =
            serde_json::from_str(&json).expect("proxy-core should deserialize TLS config");

        let tls = proxy_config.listeners[0].tls.as_ref().unwrap();
        assert_eq!(tls.mode, proxy_core::config::TlsMode::Terminate);
        assert_eq!(tls.certificates.len(), 1);
        assert_eq!(tls.inline_certificates.len(), 1);
        assert_eq!(
            tls.inline_certificates[0].hostnames,
            vec!["secure.example.com"]
        );
    }

    /// Cross-deserialization with TLS but no inline certificates (field omitted).
    #[test]
    fn controlplane_to_proxy_core_tls_without_inline_certs() {
        let mut config = GatewayConfig::new("default", "secure-gw");
        config.add_listener(ListenerConfig::https(
            "https",
            443,
            None,
            TlsConfig {
                mode: TlsMode::Terminate,
                certificates: vec![CertificateRef {
                    namespace: "default".to_string(),
                    name: "tls-secret".to_string(),
                }],
                inline_certificates: vec![],
            },
        ));

        let json = config.to_json().unwrap();
        // inline_certificates should be omitted from JSON (skip_serializing_if)
        assert!(
            !json.contains("inline_certificates"),
            "empty inline_certificates should be omitted from JSON"
        );

        let proxy_config: proxy_core::config::ProxyConfig = serde_json::from_str(&json)
            .expect("proxy-core should handle missing inline_certificates");

        let tls = proxy_config.listeners[0].tls.as_ref().unwrap();
        assert!(tls.inline_certificates.is_empty());
    }

    /// Cross-deserialization with backend protocol field.
    #[test]
    fn controlplane_to_proxy_core_backend_protocol() {
        let mut config = GatewayConfig::new("default", "gw");
        config.add_listener(ListenerConfig::http("http", 80, None));
        config.add_route(RouteConfig {
            name: "route".to_string(),
            namespace: "default".to_string(),
            hostnames: vec![],
            attached_listeners: vec!["http".to_string()],
            rules: vec![RouteRule {
                name: None,
                matches: vec![RouteMatch::default()],
                filters: vec![],
                backends: vec![BackendRef::new("default", "http1-svc", 8080), {
                    let mut b = BackendRef::new("default", "h2c-svc", 9090);
                    b.protocol = BackendProtocol::H2c;
                    b
                }],
                timeout: None,
            }],
        });

        let json = config.to_json().unwrap();
        let proxy_config: proxy_core::config::ProxyConfig =
            serde_json::from_str(&json).expect("proxy-core should deserialize backend protocol");

        let backends = &proxy_config.routes[0].rules[0].backends;
        assert_eq!(
            backends[0].protocol,
            proxy_core::config::BackendProtocol::Http1
        );
        assert_eq!(
            backends[1].protocol,
            proxy_core::config::BackendProtocol::H2c
        );
    }

    /// Cross-deserialization of all filter types.
    #[test]
    fn controlplane_to_proxy_core_all_filters() {
        let filters = vec![
            RouteFilter::RequestHeaderModifier {
                add: vec![HeaderValue {
                    name: "X-Added".to_string(),
                    value: "yes".to_string(),
                }],
                set: vec![],
                remove: vec!["X-Remove".to_string()],
            },
            RouteFilter::ResponseHeaderModifier {
                add: vec![],
                set: vec![HeaderValue {
                    name: "Cache-Control".to_string(),
                    value: "no-cache".to_string(),
                }],
                remove: vec![],
            },
            RouteFilter::RequestRedirect {
                scheme: Some("https".to_string()),
                hostname: Some("example.com".to_string()),
                port: Some(443),
                path: Some(PathModifier::ReplaceFullPath {
                    value: "/new".to_string(),
                }),
                status_code: Some(301),
            },
            RouteFilter::URLRewrite {
                hostname: Some("internal.example.com".to_string()),
                path: Some(PathModifier::ReplacePrefixMatch {
                    value: "/v2".to_string(),
                }),
            },
            RouteFilter::RequestMirror {
                backend: BackendRef::new("default", "mirror-svc", 9090),
                percent: Some(50),
            },
        ];

        for filter in &filters {
            let json = serde_json::to_string(filter).unwrap();
            let proxy_filter: proxy_core::config::Filter = serde_json::from_str(&json)
                .unwrap_or_else(|e| {
                    panic!("proxy-core should deserialize filter: {json}\nerror: {e}")
                });

            // Re-serialize from proxy-core and deserialize back into control plane
            let re_json = serde_json::to_string(&proxy_filter).unwrap();
            let _roundtrip: RouteFilter = serde_json::from_str(&re_json).unwrap_or_else(|e| {
                panic!("control plane should deserialize proxy-core filter: {re_json}\nerror: {e}")
            });
        }
    }

    /// Full roundtrip: control plane → JSON → proxy-core → JSON → control plane.
    #[test]
    fn full_roundtrip_controlplane_proxy_core_controlplane() {
        let mut config = GatewayConfig::new("production", "main-gw");
        config.add_listener(ListenerConfig::http(
            "http",
            80,
            Some("*.example.com".to_string()),
        ));
        config.add_listener(ListenerConfig::https(
            "https",
            443,
            Some("secure.example.com".to_string()),
            TlsConfig {
                mode: TlsMode::Terminate,
                certificates: vec![CertificateRef {
                    namespace: "production".to_string(),
                    name: "wildcard-cert".to_string(),
                }],
                inline_certificates: vec![InlineCertificate {
                    cert_pem: "CERT_PEM_DATA".to_string(),
                    key_pem: "KEY_PEM_DATA".to_string(),
                    hostnames: vec!["secure.example.com".to_string()],
                }],
            },
        ));
        config.add_route(RouteConfig {
            name: "api-route".to_string(),
            namespace: "production".to_string(),
            hostnames: vec!["api.example.com".to_string()],
            attached_listeners: vec!["http".to_string(), "https".to_string()],
            rules: vec![
                RouteRule {
                    name: Some("api-v1".to_string()),
                    matches: vec![RouteMatch {
                        path: Some(PathMatch {
                            match_type: PathMatchType::PathPrefix,
                            value: "/v1".to_string(),
                        }),
                        headers: vec![HeaderMatch {
                            name: "X-Version".to_string(),
                            match_type: HeaderMatchType::Exact,
                            value: "1".to_string(),
                        }],
                        query_params: vec![QueryParamMatch {
                            name: "debug".to_string(),
                            match_type: QueryParamMatchType::Exact,
                            value: "true".to_string(),
                        }],
                        method: Some("GET".to_string()),
                    }],
                    filters: vec![RouteFilter::RequestHeaderModifier {
                        add: vec![HeaderValue {
                            name: "X-Proxy".to_string(),
                            value: "multiway".to_string(),
                        }],
                        set: vec![],
                        remove: vec![],
                    }],
                    backends: vec![
                        BackendRef::new("production", "api-svc", 8080).with_weight(3),
                        {
                            let mut b =
                                BackendRef::new("production", "api-svc-h2", 9090).with_weight(1);
                            b.protocol = BackendProtocol::H2c;
                            b
                        },
                    ],
                    timeout: Some(TimeoutConfig {
                        request: Some(60.0),
                        backend_request: Some(30.0),
                    }),
                },
                RouteRule {
                    name: None,
                    matches: vec![RouteMatch {
                        path: Some(PathMatch {
                            match_type: PathMatchType::Exact,
                            value: "/healthz".to_string(),
                        }),
                        ..Default::default()
                    }],
                    filters: vec![],
                    backends: vec![BackendRef::new("production", "health-svc", 8081)],
                    timeout: None,
                },
            ],
        });

        // Control plane → JSON
        let cp_json = config.to_json().unwrap();

        // JSON → proxy-core
        let proxy_config: proxy_core::config::ProxyConfig =
            serde_json::from_str(&cp_json).expect("proxy-core should deserialize full config");

        // proxy-core → JSON
        let pc_json = proxy_config.to_json().unwrap();

        // JSON → control plane
        let roundtrip: GatewayConfig =
            serde_json::from_str(&pc_json).expect("control plane should deserialize roundtrip");

        assert_eq!(config, roundtrip);
    }
}
