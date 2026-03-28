// Configuration types with serde serialization.
//
// Defines the proxy's runtime configuration schema, including listener
// bindings, upstream backends, TLS settings, and route tables. All types
// derive Serialize/Deserialize for JSON-based hot-reload via ConfigMap.
//
// These types are designed to be wire-compatible with the control plane's
// `GatewayConfig` JSON schema so they can deserialize ConfigMap payloads
// produced by the multiway control plane.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

/// Top-level proxy configuration describing listeners and routes.
///
/// This is the data contract between the control plane (which serializes
/// `GatewayConfig` JSON into a ConfigMap) and the proxy-core library.
/// The field names and serde attributes are chosen to be wire-compatible
/// with the control plane schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProxyConfig {
    /// Schema version (currently `"v1"`).
    pub version: String,
    /// Reference to the originating Gateway resource.
    pub gateway: GatewayRef,
    /// Listeners that accept inbound connections.
    pub listeners: Vec<ListenerConfig>,
    /// Routes that describe how to match and forward requests.
    pub routes: Vec<RouteConfig>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            version: "v1".to_string(),
            gateway: GatewayRef::default(),
            listeners: Vec::new(),
            routes: Vec::new(),
        }
    }
}

impl ProxyConfig {
    /// Serialize to pretty-printed JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Return all routes attached to the given listener name.
    pub fn routes_for_listener(&self, listener_name: &str) -> Vec<&RouteConfig> {
        self.routes
            .iter()
            .filter(|r| r.attached_listeners.contains(&listener_name.to_string()))
            .collect()
    }
}

/// Reference to the Gateway resource that produced this configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GatewayRef {
    /// Namespace of the gateway.
    pub namespace: String,
    /// Name of the gateway.
    pub name: String,
}

// ---------------------------------------------------------------------------
// Listener
// ---------------------------------------------------------------------------

/// Configuration for a single listener that accepts inbound connections.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListenerConfig {
    /// Listener name (from the Gateway spec).
    pub name: String,
    /// Port to listen on.
    pub port: u16,
    /// Protocol for this listener.
    pub protocol: Protocol,
    /// Optional hostname restriction. `None` means match all hosts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// TLS configuration. Required when `protocol` is `Https`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
}

impl ListenerConfig {
    /// Create a plain HTTP listener.
    pub fn http(name: impl Into<String>, port: u16, hostname: Option<String>) -> Self {
        Self {
            name: name.into(),
            port,
            protocol: Protocol::Http,
            hostname,
            tls: None,
        }
    }

    /// Create an HTTPS listener with TLS configuration.
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

/// Protocol supported by a listener.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Protocol {
    Http,
    Https,
}

// ---------------------------------------------------------------------------
// TLS
// ---------------------------------------------------------------------------

/// TLS configuration for an HTTPS listener.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TlsConfig {
    /// Termination mode.
    pub mode: TlsMode,
    /// References to TLS certificate secrets (Kubernetes Secret references).
    pub certificates: Vec<CertificateRef>,
    /// Inline certificates with PEM data for direct use by the proxy.
    ///
    /// The control plane resolves `CertificateRef` secrets and populates
    /// these inline certificates before writing the config to the ConfigMap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_certificates: Vec<InlineCertificate>,
}

/// TLS termination mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    Terminate,
    Passthrough,
}

/// Reference to a Kubernetes Secret containing a TLS certificate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CertificateRef {
    /// Namespace of the secret.
    pub namespace: String,
    /// Name of the secret.
    pub name: String,
}

/// Inline TLS certificate with actual PEM data.
///
/// The control plane resolves `CertificateRef` Kubernetes Secret references
/// and populates these inline certificates so the proxy-core library can
/// build a `rustls::ServerConfig` directly without Kubernetes API access.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InlineCertificate {
    /// PEM-encoded certificate chain (leaf first, then intermediates).
    pub cert_pem: String,
    /// PEM-encoded private key (PKCS#1, PKCS#8, or SEC1).
    pub key_pem: String,
    /// Optional hostnames this certificate covers (for SNI routing).
    ///
    /// When multiple inline certificates are provided, these hostnames
    /// determine which certificate to present based on the client's SNI.
    /// If empty, the certificate is used as the default/fallback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hostnames: Vec<String>,
}

// ---------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------

/// Configuration for an HTTP route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RouteConfig {
    /// Name of the HTTPRoute resource.
    pub name: String,
    /// Namespace of the HTTPRoute resource.
    pub namespace: String,
    /// Hostnames this route matches.
    #[serde(default)]
    pub hostnames: Vec<String>,
    /// Listener names this route is attached to.
    #[serde(default)]
    pub attached_listeners: Vec<String>,
    /// Ordered list of routing rules.
    pub rules: Vec<RouteRule>,
}

impl RouteConfig {
    /// Create a new route with the given identity and no rules.
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

// ---------------------------------------------------------------------------
// Route rule
// ---------------------------------------------------------------------------

/// A single routing rule consisting of matches, filters, and backends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RouteRule {
    /// Optional rule name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Match conditions. Multiple matches have OR semantics.
    #[serde(default)]
    pub matches: Vec<RouteMatch>,
    /// Filters to apply to matched requests.
    #[serde(default)]
    pub filters: Vec<Filter>,
    /// Backend targets for forwarding.
    #[serde(default)]
    pub backends: Vec<Backend>,
    /// Timeout configuration for this rule.
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

// ---------------------------------------------------------------------------
// Route match
// ---------------------------------------------------------------------------

/// Match conditions for a route rule. Header and query-param matches use
/// AND semantics; multiple `RouteMatch` values within a rule use OR semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RouteMatch {
    /// Path match condition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathMatch>,
    /// Header match conditions (AND semantics).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HeaderMatch>,
    /// Query parameter match conditions (AND semantics).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_params: Vec<QueryParamMatch>,
    /// HTTP method to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

// ---------------------------------------------------------------------------
// Path matching
// ---------------------------------------------------------------------------

/// Path matching configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathMatch {
    /// Type of path match.
    #[serde(rename = "type")]
    pub match_type: PathMatchType,
    /// Value to match against.
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

/// Type of path matching.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PathMatchType {
    Exact,
    PathPrefix,
    RegularExpression,
}

// ---------------------------------------------------------------------------
// Header matching
// ---------------------------------------------------------------------------

/// Header matching configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeaderMatch {
    /// Header name.
    pub name: String,
    /// Type of match.
    #[serde(rename = "type")]
    pub match_type: HeaderMatchType,
    /// Value to match against.
    pub value: String,
}

/// Type of header matching.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HeaderMatchType {
    Exact,
    RegularExpression,
}

// ---------------------------------------------------------------------------
// Query parameter matching
// ---------------------------------------------------------------------------

/// Query parameter matching configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryParamMatch {
    /// Parameter name.
    pub name: String,
    /// Type of match.
    #[serde(rename = "type")]
    pub match_type: QueryParamMatchType,
    /// Value to match against.
    pub value: String,
}

/// Type of query parameter matching.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryParamMatchType {
    Exact,
    RegularExpression,
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// A filter applied to matched requests or responses.
///
/// The serde representation uses an internally-tagged enum (the `"type"`
/// field) so it is wire-compatible with the control plane's `RouteFilter`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Filter {
    /// Modify request headers before forwarding.
    RequestHeaderModifier {
        #[serde(default)]
        add: Vec<HeaderValue>,
        #[serde(default)]
        set: Vec<HeaderValue>,
        #[serde(default)]
        remove: Vec<String>,
    },
    /// Modify response headers before returning to the client.
    ResponseHeaderModifier {
        #[serde(default)]
        add: Vec<HeaderValue>,
        #[serde(default)]
        set: Vec<HeaderValue>,
        #[serde(default)]
        remove: Vec<String>,
    },
    /// Redirect the request instead of forwarding.
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
    /// Rewrite the request URL before forwarding.
    URLRewrite {
        #[serde(skip_serializing_if = "Option::is_none")]
        hostname: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<PathModifier>,
    },
    /// Mirror the request to another backend.
    RequestMirror {
        backend: Backend,
        #[serde(skip_serializing_if = "Option::is_none")]
        percent: Option<u32>,
    },
}

/// Convenience constructors for common header-modifier filters.
impl Filter {
    /// Create a `RequestHeaderModifier` that adds headers.
    pub fn request_header_add(headers: Vec<HeaderValue>) -> Self {
        Self::RequestHeaderModifier {
            add: headers,
            set: Vec::new(),
            remove: Vec::new(),
        }
    }

    /// Create a `RequestHeaderModifier` that removes headers.
    pub fn request_header_remove(names: Vec<String>) -> Self {
        Self::RequestHeaderModifier {
            add: Vec::new(),
            set: Vec::new(),
            remove: names,
        }
    }

    /// Create a `RequestRedirect` with a status code.
    pub fn redirect(status_code: u16) -> Self {
        Self::RequestRedirect {
            scheme: None,
            hostname: None,
            port: None,
            path: None,
            status_code: Some(status_code),
        }
    }
}

/// A header name-value pair used in header modifier filters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeaderValue {
    pub name: String,
    pub value: String,
}

/// Path modification for redirects and rewrites.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum PathModifier {
    ReplaceFullPath { value: String },
    ReplacePrefixMatch { value: String },
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Protocol used to connect to a backend.
///
/// Defaults to `Http1` for backward compatibility. When set to `H2c`, the
/// proxy connects to the backend using HTTP/2 prior knowledge (cleartext h2c)
/// instead of HTTP/1.1.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackendProtocol {
    /// HTTP/1.1 (default).
    #[default]
    Http1,
    /// HTTP/2 cleartext (h2c) — prior knowledge mode.
    H2c,
}

/// A backend target that receives forwarded traffic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Backend {
    /// Namespace of the backend Service.
    pub namespace: String,
    /// Name of the backend Service.
    pub name: String,
    /// Port on the backend Service.
    pub port: u16,
    /// Relative weight for load balancing (default: 1).
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// Protocol to use when connecting to this backend.
    #[serde(default)]
    pub protocol: BackendProtocol,
}

fn default_weight() -> u32 {
    1
}

impl Backend {
    /// Create a new backend reference with weight 1 and HTTP/1.1 protocol.
    pub fn new(namespace: impl Into<String>, name: impl Into<String>, port: u16) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
            port,
            weight: 1,
            protocol: BackendProtocol::default(),
        }
    }

    /// Set the weight for this backend.
    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }

    /// Set the backend protocol.
    pub fn with_protocol(mut self, protocol: BackendProtocol) -> Self {
        self.protocol = protocol;
        self
    }
}

// ---------------------------------------------------------------------------
// Timeout
// ---------------------------------------------------------------------------

/// Timeout configuration for a route rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeoutConfig {
    /// Total request timeout in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<f64>,
    /// Backend request timeout in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_request: Option<f64>,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Fluent builder for constructing a [`ProxyConfig`] in tests.
pub struct ProxyConfigBuilder {
    config: ProxyConfig,
}

impl ProxyConfigBuilder {
    /// Start building a new `ProxyConfig`.
    pub fn new(gateway_namespace: impl Into<String>, gateway_name: impl Into<String>) -> Self {
        Self {
            config: ProxyConfig {
                version: "v1".to_string(),
                gateway: GatewayRef {
                    namespace: gateway_namespace.into(),
                    name: gateway_name.into(),
                },
                listeners: Vec::new(),
                routes: Vec::new(),
            },
        }
    }

    /// Add an HTTP listener.
    pub fn with_http_listener(
        mut self,
        name: impl Into<String>,
        port: u16,
        hostname: Option<String>,
    ) -> Self {
        self.config
            .listeners
            .push(ListenerConfig::http(name, port, hostname));
        self
    }

    /// Add an HTTPS listener.
    pub fn with_https_listener(
        mut self,
        name: impl Into<String>,
        port: u16,
        hostname: Option<String>,
        tls: TlsConfig,
    ) -> Self {
        self.config
            .listeners
            .push(ListenerConfig::https(name, port, hostname, tls));
        self
    }

    /// Add a route.
    pub fn with_route(mut self, route: RouteConfig) -> Self {
        self.config.routes.push(route);
        self
    }

    /// Finish building and return the config.
    pub fn build(self) -> ProxyConfig {
        self.config
    }
}

/// Fluent builder for constructing a [`RouteConfig`] in tests.
pub struct RouteConfigBuilder {
    route: RouteConfig,
}

impl RouteConfigBuilder {
    /// Start building a new `RouteConfig`.
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            route: RouteConfig::new(namespace, name),
        }
    }

    /// Add a hostname.
    pub fn with_hostname(mut self, hostname: impl Into<String>) -> Self {
        self.route.hostnames.push(hostname.into());
        self
    }

    /// Attach to a listener by name.
    pub fn attached_to(mut self, listener: impl Into<String>) -> Self {
        self.route.attached_listeners.push(listener.into());
        self
    }

    /// Add a rule.
    pub fn with_rule(mut self, rule: RouteRule) -> Self {
        self.route.rules.push(rule);
        self
    }

    /// Finish building and return the route.
    pub fn build(self) -> RouteConfig {
        self.route
    }
}

/// Fluent builder for constructing a [`RouteRule`] in tests.
pub struct RouteRuleBuilder {
    rule: RouteRule,
}

impl RouteRuleBuilder {
    /// Start building a new `RouteRule`.
    pub fn new() -> Self {
        Self {
            rule: RouteRule {
                name: None,
                matches: Vec::new(),
                filters: Vec::new(),
                backends: Vec::new(),
                timeout: None,
            },
        }
    }

    /// Set the rule name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.rule.name = Some(name.into());
        self
    }

    /// Add a path-prefix match.
    pub fn with_path_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.rule.matches.push(RouteMatch {
            path: Some(PathMatch {
                match_type: PathMatchType::PathPrefix,
                value: prefix.into(),
            }),
            ..Default::default()
        });
        self
    }

    /// Add an exact-path match.
    pub fn with_exact_path(mut self, path: impl Into<String>) -> Self {
        self.rule.matches.push(RouteMatch {
            path: Some(PathMatch {
                match_type: PathMatchType::Exact,
                value: path.into(),
            }),
            ..Default::default()
        });
        self
    }

    /// Add a full `RouteMatch`.
    pub fn with_match(mut self, route_match: RouteMatch) -> Self {
        self.rule.matches.push(route_match);
        self
    }

    /// Add a filter.
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.rule.filters.push(filter);
        self
    }

    /// Add a backend.
    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.rule.backends.push(backend);
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, timeout: TimeoutConfig) -> Self {
        self.rule.timeout = Some(timeout);
        self
    }

    /// Finish building and return the rule.
    pub fn build(self) -> RouteRule {
        self.rule
    }
}

impl Default for RouteRuleBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- JSON round-trip tests -----------------------------------------------

    #[test]
    fn proxy_config_roundtrip() {
        let config = ProxyConfigBuilder::new("default", "my-gateway")
            .with_http_listener("http", 80, Some("example.com".to_string()))
            .with_route(
                RouteConfigBuilder::new("default", "my-route")
                    .with_hostname("example.com")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/api")
                            .with_backend(Backend::new("default", "my-service", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let json = config.to_json().unwrap();
        let deserialized = ProxyConfig::from_json(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn listener_config_roundtrip() {
        let http = ListenerConfig::http("http", 80, None);
        let json = serde_json::to_string(&http).unwrap();
        let back: ListenerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(http, back);

        let https = ListenerConfig::https(
            "https",
            443,
            Some("secure.example.com".to_string()),
            TlsConfig {
                mode: TlsMode::Terminate,
                certificates: vec![CertificateRef {
                    namespace: "default".to_string(),
                    name: "my-cert".to_string(),
                }],
                inline_certificates: Vec::new(),
            },
        );
        let json = serde_json::to_string(&https).unwrap();
        let back: ListenerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(https, back);
    }

    #[test]
    fn route_config_roundtrip() {
        let route = RouteConfigBuilder::new("ns", "route-1")
            .with_hostname("example.com")
            .attached_to("http")
            .with_rule(RouteRule::default())
            .build();

        let json = serde_json::to_string(&route).unwrap();
        let back: RouteConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(route, back);
    }

    #[test]
    fn route_rule_roundtrip() {
        let rule = RouteRuleBuilder::new()
            .with_name("test-rule")
            .with_path_prefix("/v1")
            .with_backend(Backend::new("default", "svc", 8080).with_weight(5))
            .with_timeout(TimeoutConfig {
                request: Some(30.0),
                backend_request: Some(10.0),
            })
            .build();

        let json = serde_json::to_string(&rule).unwrap();
        let back: RouteRule = serde_json::from_str(&json).unwrap();
        assert_eq!(rule, back);
    }

    #[test]
    fn path_match_roundtrip() {
        for mt in [
            PathMatchType::Exact,
            PathMatchType::PathPrefix,
            PathMatchType::RegularExpression,
        ] {
            let pm = PathMatch {
                match_type: mt,
                value: "/test".to_string(),
            };
            let json = serde_json::to_string(&pm).unwrap();
            let back: PathMatch = serde_json::from_str(&json).unwrap();
            assert_eq!(pm, back);
        }
    }

    #[test]
    fn header_match_roundtrip() {
        for mt in [HeaderMatchType::Exact, HeaderMatchType::RegularExpression] {
            let hm = HeaderMatch {
                name: "X-Test".to_string(),
                match_type: mt,
                value: "value".to_string(),
            };
            let json = serde_json::to_string(&hm).unwrap();
            let back: HeaderMatch = serde_json::from_str(&json).unwrap();
            assert_eq!(hm, back);
        }
    }

    #[test]
    fn query_param_match_roundtrip() {
        for mt in [
            QueryParamMatchType::Exact,
            QueryParamMatchType::RegularExpression,
        ] {
            let qp = QueryParamMatch {
                name: "page".to_string(),
                match_type: mt,
                value: "1".to_string(),
            };
            let json = serde_json::to_string(&qp).unwrap();
            let back: QueryParamMatch = serde_json::from_str(&json).unwrap();
            assert_eq!(qp, back);
        }
    }

    #[test]
    fn filter_request_header_modifier_roundtrip() {
        let filter = Filter::RequestHeaderModifier {
            add: vec![HeaderValue {
                name: "X-Added".to_string(),
                value: "yes".to_string(),
            }],
            set: vec![HeaderValue {
                name: "X-Set".to_string(),
                value: "val".to_string(),
            }],
            remove: vec!["X-Remove".to_string()],
        };
        let json = serde_json::to_string(&filter).unwrap();
        let back: Filter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, back);
    }

    #[test]
    fn filter_response_header_modifier_roundtrip() {
        let filter = Filter::ResponseHeaderModifier {
            add: vec![],
            set: vec![HeaderValue {
                name: "Cache-Control".to_string(),
                value: "no-cache".to_string(),
            }],
            remove: vec![],
        };
        let json = serde_json::to_string(&filter).unwrap();
        let back: Filter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, back);
    }

    #[test]
    fn filter_redirect_roundtrip() {
        let filter = Filter::RequestRedirect {
            scheme: Some("https".to_string()),
            hostname: Some("example.com".to_string()),
            port: Some(443),
            path: Some(PathModifier::ReplaceFullPath {
                value: "/new-path".to_string(),
            }),
            status_code: Some(301),
        };
        let json = serde_json::to_string(&filter).unwrap();
        let back: Filter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, back);
    }

    #[test]
    fn filter_url_rewrite_roundtrip() {
        let filter = Filter::URLRewrite {
            hostname: Some("internal.example.com".to_string()),
            path: Some(PathModifier::ReplacePrefixMatch {
                value: "/v2".to_string(),
            }),
        };
        let json = serde_json::to_string(&filter).unwrap();
        let back: Filter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, back);
    }

    #[test]
    fn filter_request_mirror_roundtrip() {
        let filter = Filter::RequestMirror {
            backend: Backend::new("default", "mirror-svc", 9090),
            percent: Some(50),
        };
        let json = serde_json::to_string(&filter).unwrap();
        let back: Filter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, back);
    }

    #[test]
    fn path_modifier_roundtrip() {
        let full = PathModifier::ReplaceFullPath {
            value: "/new".to_string(),
        };
        let json = serde_json::to_string(&full).unwrap();
        let back: PathModifier = serde_json::from_str(&json).unwrap();
        assert_eq!(full, back);

        let prefix = PathModifier::ReplacePrefixMatch {
            value: "/v2".to_string(),
        };
        let json = serde_json::to_string(&prefix).unwrap();
        let back: PathModifier = serde_json::from_str(&json).unwrap();
        assert_eq!(prefix, back);
    }

    #[test]
    fn backend_roundtrip() {
        let b = Backend::new("ns", "svc", 8080).with_weight(3);
        let json = serde_json::to_string(&b).unwrap();
        let back: Backend = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn backend_protocol_roundtrip() {
        for p in [BackendProtocol::Http1, BackendProtocol::H2c] {
            let json = serde_json::to_string(&p).unwrap();
            let back: BackendProtocol = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn backend_protocol_serde_values() {
        assert_eq!(
            serde_json::to_string(&BackendProtocol::Http1).unwrap(),
            "\"http1\""
        );
        assert_eq!(
            serde_json::to_string(&BackendProtocol::H2c).unwrap(),
            "\"h2c\""
        );
    }

    #[test]
    fn backend_with_h2c_protocol_roundtrip() {
        let b = Backend::new("ns", "svc", 8080).with_protocol(BackendProtocol::H2c);
        let json = serde_json::to_string(&b).unwrap();
        let back: Backend = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
        assert_eq!(back.protocol, BackendProtocol::H2c);
    }

    #[test]
    fn backend_without_protocol_defaults_to_http1() {
        // Omitting the protocol field should default to Http1.
        let json = r#"{ "namespace": "ns", "name": "svc", "port": 80 }"#;
        let b: Backend = serde_json::from_str(json).unwrap();
        assert_eq!(b.protocol, BackendProtocol::Http1);
    }

    #[test]
    fn backend_default_protocol_is_http1() {
        let b = Backend::new("ns", "svc", 80);
        assert_eq!(b.protocol, BackendProtocol::Http1);
    }

    #[test]
    fn timeout_config_roundtrip() {
        let tc = TimeoutConfig {
            request: Some(30.0),
            backend_request: None,
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: TimeoutConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(tc, back);
    }

    #[test]
    fn protocol_roundtrip() {
        for p in [Protocol::Http, Protocol::Https] {
            let json = serde_json::to_string(&p).unwrap();
            let back: Protocol = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn tls_mode_roundtrip() {
        for m in [TlsMode::Terminate, TlsMode::Passthrough] {
            let json = serde_json::to_string(&m).unwrap();
            let back: TlsMode = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    // -- Control plane compatibility -----------------------------------------

    #[test]
    fn deserialize_controlplane_compatible_json() {
        // This JSON matches the format the control plane's GatewayConfig produces.
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
                    "protocol": "HTTP",
                    "hostname": "example.com"
                },
                {
                    "name": "https",
                    "port": 443,
                    "protocol": "HTTPS",
                    "hostname": "secure.example.com",
                    "tls": {
                        "mode": "terminate",
                        "certificates": [
                            { "namespace": "default", "name": "tls-secret" }
                        ]
                    }
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
                                    "path": { "type": "PathPrefix", "value": "/api" },
                                    "headers": [
                                        { "name": "X-Version", "type": "Exact", "value": "v2" }
                                    ],
                                    "query_params": [
                                        { "name": "debug", "type": "Exact", "value": "true" }
                                    ],
                                    "method": "GET"
                                }
                            ],
                            "filters": [
                                {
                                    "type": "RequestHeaderModifier",
                                    "add": [{ "name": "X-Proxy", "value": "multiway" }],
                                    "set": [],
                                    "remove": []
                                }
                            ],
                            "backends": [
                                { "namespace": "default", "name": "my-service", "port": 8080, "weight": 1 }
                            ]
                        }
                    ]
                }
            ]
        }"#;

        let config = ProxyConfig::from_json(json).unwrap();
        assert_eq!(config.version, "v1");
        assert_eq!(config.gateway.name, "my-gateway");
        assert_eq!(config.listeners.len(), 2);
        assert_eq!(config.listeners[0].protocol, Protocol::Http);
        assert_eq!(config.listeners[1].protocol, Protocol::Https);
        assert_eq!(
            config.listeners[1].tls.as_ref().unwrap().mode,
            TlsMode::Terminate
        );
        assert_eq!(config.routes.len(), 1);
        assert_eq!(config.routes[0].rules[0].matches.len(), 1);
        assert_eq!(
            config.routes[0].rules[0].matches[0]
                .path
                .as_ref()
                .unwrap()
                .match_type,
            PathMatchType::PathPrefix
        );
        assert_eq!(config.routes[0].rules[0].backends[0].name, "my-service");

        // Re-serialize and re-deserialize to confirm roundtrip stability.
        let re_json = config.to_json().unwrap();
        let re_config = ProxyConfig::from_json(&re_json).unwrap();
        assert_eq!(config, re_config);
    }

    // -- Error handling ------------------------------------------------------

    #[test]
    fn invalid_json_produces_clear_error() {
        let result = ProxyConfig::from_json("not json at all");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("expected"),
            "Error should describe what was expected, got: {err}"
        );
    }

    #[test]
    fn missing_required_field_produces_clear_error() {
        // Missing `version` field.
        let json =
            r#"{ "gateway": { "namespace": "a", "name": "b" }, "listeners": [], "routes": [] }"#;
        let result = ProxyConfig::from_json(json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("version"),
            "Error should mention the missing field, got: {err}"
        );
    }

    #[test]
    fn unknown_filter_type_produces_clear_error() {
        let json = r#"{ "type": "UnknownFilter" }"#;
        let result: Result<Filter, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // -- Builder API tests ---------------------------------------------------

    #[test]
    fn builder_constructs_complete_config() {
        let config = ProxyConfigBuilder::new("prod", "main-gw")
            .with_http_listener("http", 80, None)
            .with_https_listener(
                "https",
                443,
                Some("*.example.com".to_string()),
                TlsConfig {
                    mode: TlsMode::Terminate,
                    certificates: vec![CertificateRef {
                        namespace: "prod".to_string(),
                        name: "wildcard-cert".to_string(),
                    }],
                    inline_certificates: Vec::new(),
                },
            )
            .with_route(
                RouteConfigBuilder::new("prod", "api-route")
                    .with_hostname("api.example.com")
                    .attached_to("http")
                    .attached_to("https")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_name("api-v1")
                            .with_path_prefix("/v1")
                            .with_filter(Filter::request_header_add(vec![HeaderValue {
                                name: "X-Version".to_string(),
                                value: "1".to_string(),
                            }]))
                            .with_backend(Backend::new("prod", "api-svc", 8080))
                            .with_timeout(TimeoutConfig {
                                request: Some(60.0),
                                backend_request: Some(30.0),
                            })
                            .build(),
                    )
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_exact_path("/healthz")
                            .with_backend(Backend::new("prod", "health-svc", 8081))
                            .build(),
                    )
                    .build(),
            )
            .build();

        assert_eq!(config.version, "v1");
        assert_eq!(config.gateway.namespace, "prod");
        assert_eq!(config.gateway.name, "main-gw");
        assert_eq!(config.listeners.len(), 2);
        assert_eq!(config.routes.len(), 1);

        let route = &config.routes[0];
        assert_eq!(route.attached_listeners.len(), 2);
        assert_eq!(route.rules.len(), 2);
        assert_eq!(route.rules[0].name.as_deref(), Some("api-v1"));
        assert_eq!(route.rules[0].backends[0].name, "api-svc");
        assert!(route.rules[0].timeout.is_some());
        assert_eq!(
            route.rules[1].matches[0].path.as_ref().unwrap().match_type,
            PathMatchType::Exact
        );
    }

    #[test]
    fn routes_for_listener_filters_correctly() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_http_listener("internal", 8080, None)
            .with_route(
                RouteConfigBuilder::new("default", "public")
                    .attached_to("http")
                    .with_rule(RouteRule::default())
                    .build(),
            )
            .with_route(
                RouteConfigBuilder::new("default", "internal")
                    .attached_to("internal")
                    .with_rule(RouteRule::default())
                    .build(),
            )
            .with_route(
                RouteConfigBuilder::new("default", "both")
                    .attached_to("http")
                    .attached_to("internal")
                    .with_rule(RouteRule::default())
                    .build(),
            )
            .build();

        let http_routes = config.routes_for_listener("http");
        assert_eq!(http_routes.len(), 2);
        assert_eq!(http_routes[0].name, "public");
        assert_eq!(http_routes[1].name, "both");

        let internal_routes = config.routes_for_listener("internal");
        assert_eq!(internal_routes.len(), 2);
        assert_eq!(internal_routes[0].name, "internal");
        assert_eq!(internal_routes[1].name, "both");

        let none_routes = config.routes_for_listener("nonexistent");
        assert!(none_routes.is_empty());
    }

    #[test]
    fn default_backend_weight_is_one() {
        let b = Backend::new("ns", "svc", 80);
        assert_eq!(b.weight, 1);
    }

    #[test]
    fn backend_weight_from_json_default() {
        // Omitting the weight field should default to 1.
        let json = r#"{ "namespace": "ns", "name": "svc", "port": 80 }"#;
        let b: Backend = serde_json::from_str(json).unwrap();
        assert_eq!(b.weight, 1);
    }

    #[test]
    fn filter_convenience_constructors() {
        let add = Filter::request_header_add(vec![HeaderValue {
            name: "X-Foo".to_string(),
            value: "bar".to_string(),
        }]);
        match &add {
            Filter::RequestHeaderModifier { add, set, remove } => {
                assert_eq!(add.len(), 1);
                assert!(set.is_empty());
                assert!(remove.is_empty());
            }
            _ => panic!("Expected RequestHeaderModifier"),
        }

        let rm = Filter::request_header_remove(vec!["X-Bad".to_string()]);
        match &rm {
            Filter::RequestHeaderModifier { add, set, remove } => {
                assert!(add.is_empty());
                assert!(set.is_empty());
                assert_eq!(remove.len(), 1);
            }
            _ => panic!("Expected RequestHeaderModifier"),
        }

        let redir = Filter::redirect(302);
        match &redir {
            Filter::RequestRedirect { status_code, .. } => {
                assert_eq!(*status_code, Some(302));
            }
            _ => panic!("Expected RequestRedirect"),
        }
    }

    #[test]
    fn complex_route_match_roundtrip() {
        let route_match = RouteMatch {
            path: Some(PathMatch {
                match_type: PathMatchType::RegularExpression,
                value: r"^/api/v\d+/.*".to_string(),
            }),
            headers: vec![
                HeaderMatch {
                    name: "Authorization".to_string(),
                    match_type: HeaderMatchType::RegularExpression,
                    value: r"^Bearer .+".to_string(),
                },
                HeaderMatch {
                    name: "Content-Type".to_string(),
                    match_type: HeaderMatchType::Exact,
                    value: "application/json".to_string(),
                },
            ],
            query_params: vec![QueryParamMatch {
                name: "format".to_string(),
                match_type: QueryParamMatchType::Exact,
                value: "json".to_string(),
            }],
            method: Some("POST".to_string()),
        };

        let json = serde_json::to_string(&route_match).unwrap();
        let back: RouteMatch = serde_json::from_str(&json).unwrap();
        assert_eq!(route_match, back);
    }
}
