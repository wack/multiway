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
    /// Port to listen on (the external port exposed by the Service)
    pub port: u16,
    /// Port the container binds to internally.
    /// For privileged ports (< 1024), this is offset by 8000 to avoid requiring root.
    /// The Service maps the external `port` to this internal `container_port`.
    #[serde(default)]
    pub container_port: u16,
    /// Protocol (HTTP or HTTPS)
    pub protocol: Protocol,
    /// Hostname to match (optional, None means all hosts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// TLS configuration (for HTTPS listeners)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
}

/// Offset added to privileged ports to compute the container port.
/// Ports below 1024 require root privileges to bind, so we add this offset
/// to allow the data plane to run as non-root.
const PRIVILEGED_PORT_OFFSET: u16 = 8000;

/// Compute the internal container port from an external listener port.
/// Privileged ports (< 1024) are offset by 8000 to avoid requiring root.
pub fn compute_container_port(port: u16) -> u16 {
    if port < 1024 {
        port + PRIVILEGED_PORT_OFFSET
    } else {
        port
    }
}

impl ListenerConfig {
    /// Create a new HTTP listener configuration
    pub fn http(name: impl Into<String>, port: u16, hostname: Option<String>) -> Self {
        Self {
            name: name.into(),
            port,
            container_port: compute_container_port(port),
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
            container_port: compute_container_port(port),
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
}
