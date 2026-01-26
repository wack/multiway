//! Configuration types for the Gateway data plane
//!
//! These types mirror the configuration schema used by the control plane
//! in the ConfigMap. They are used to configure the HTTP proxy.

use serde::{Deserialize, Serialize};

/// The complete gateway configuration stored in a ConfigMap
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GatewayConfig {
    /// Version of the configuration schema
    pub version: String,
    /// The gateway this configuration belongs to
    pub gateway: GatewayRef,
    /// List of listeners configured on this gateway
    #[serde(default)]
    pub listeners: Vec<ListenerConfig>,
    /// List of routes attached to this gateway
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
}

/// Reference to a Gateway resource
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GatewayRef {
    /// Namespace of the gateway
    pub namespace: String,
    /// Name of the gateway
    pub name: String,
}

/// Configuration for a single listener
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenerConfig {
    /// Name of the listener
    pub name: String,
    /// Port to listen on (the external port exposed by the Service)
    pub port: u16,
    /// Port the container binds to internally.
    /// For privileged ports (< 1024), this is offset by 8000 to avoid requiring root.
    /// If not specified, defaults to `port` (for backwards compatibility).
    #[serde(default)]
    pub container_port: u16,
    /// Protocol (HTTP or HTTPS)
    pub protocol: Protocol,
    /// Hostname to match (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// TLS configuration (for HTTPS listeners)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
}

/// Protocol supported by listeners
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Protocol {
    Http,
    Https,
}

/// TLS configuration for HTTPS listeners
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// TLS mode (Terminate or Passthrough)
    pub mode: TlsMode,
    /// Certificate references
    #[serde(default)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateRef {
    /// Namespace of the secret
    pub namespace: String,
    /// Name of the secret
    pub name: String,
}

/// Configuration for an HTTP route
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default)]
    pub rules: Vec<RouteRule>,
}

/// A single routing rule
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Match conditions for a route rule
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteMatch {
    /// Path match condition
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathMatch>,
    /// Header match conditions
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HeaderMatch>,
    /// Query parameter match conditions
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_params: Vec<QueryParamMatch>,
    /// HTTP method to match
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

/// Path matching configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderValue {
    pub name: String,
    pub value: String,
}

/// Path modification configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PathModifier {
    ReplaceFullPath { value: String },
    ReplacePrefixMatch { value: String },
}

/// Reference to a backend service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendRef {
    /// Namespace of the service
    pub namespace: String,
    /// Name of the service
    pub name: String,
    /// Port to connect to
    pub port: u16,
    /// Weight for load balancing
    #[serde(default = "default_weight")]
    pub weight: u32,
}

fn default_weight() -> u32 {
    1
}

/// Timeout configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutConfig {
    /// Total request timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<f64>,
    /// Backend request timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_request: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_config() {
        let json = r#"{
            "version": "v1",
            "gateway": {
                "namespace": "default",
                "name": "my-gateway"
            },
            "listeners": [
                {
                    "name": "http",
                    "port": 80,
                    "protocol": "HTTP"
                }
            ],
            "routes": [
                {
                    "name": "my-route",
                    "namespace": "default",
                    "hostnames": ["example.com"],
                    "attached_listeners": ["http"],
                    "rules": [
                        {
                            "matches": [
                                {
                                    "path": {
                                        "type": "PathPrefix",
                                        "value": "/api"
                                    }
                                }
                            ],
                            "backends": [
                                {
                                    "namespace": "default",
                                    "name": "my-service",
                                    "port": 8080,
                                    "weight": 1
                                }
                            ]
                        }
                    ]
                }
            ]
        }"#;

        let config: GatewayConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.version, "v1");
        assert_eq!(config.gateway.name, "my-gateway");
        assert_eq!(config.listeners.len(), 1);
        assert_eq!(config.routes.len(), 1);
    }
}
