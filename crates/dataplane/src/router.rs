//! HTTP Router
//!
//! This module implements HTTP routing based on the Gateway API specification.
//! It matches incoming requests against configured routes and selects the
//! appropriate backend.

use regex::Regex;
use tracing::trace;

use crate::config::{
    BackendRef, GatewayConfig, HeaderMatch, HeaderMatchType, PathMatch, PathMatchType,
    QueryParamMatch, QueryParamMatchType, RouteConfig, RouteFilter, RouteMatch, RouteRule,
};

/// Information about an incoming HTTP request for routing
#[derive(Debug, Clone)]
pub struct RequestInfo<'a> {
    /// The hostname from the Host header
    pub host: Option<&'a str>,
    /// The request path
    pub path: &'a str,
    /// The HTTP method
    pub method: &'a str,
    /// Request headers
    pub headers: &'a [(String, String)],
    /// Query parameters
    pub query_params: &'a [(String, String)],
}

/// Result of routing a request
#[derive(Debug, Clone)]
pub struct RoutingResult {
    /// The matched route (if any)
    pub route: Option<MatchedRoute>,
    /// Whether to redirect
    pub redirect: Option<RedirectAction>,
}

/// A matched route with the backend to use
#[derive(Debug, Clone)]
pub struct MatchedRoute {
    /// Route namespace/name for logging
    pub route_id: String,
    /// Rule name (if set)
    #[allow(dead_code)]
    pub rule_name: Option<String>,
    /// Backends to forward to
    pub backends: Vec<BackendRef>,
    /// Filters to apply
    pub filters: Vec<RouteFilter>,
    /// Request timeout
    #[allow(dead_code)]
    pub timeout_secs: Option<f64>,
    /// Backend request timeout
    #[allow(dead_code)]
    pub backend_timeout_secs: Option<f64>,
    /// Hostname rewrite
    pub hostname_rewrite: Option<String>,
    /// Path rewrite
    pub path_rewrite: Option<PathRewrite>,
}

/// Path rewrite configuration
#[derive(Debug, Clone)]
pub enum PathRewrite {
    ReplaceFullPath(String),
    ReplacePrefixMatch {
        match_prefix: String,
        replacement: String,
    },
}

/// Redirect action
#[derive(Debug, Clone)]
pub struct RedirectAction {
    pub scheme: Option<String>,
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub path: Option<String>,
    pub status_code: u16,
}

/// Match specificity for sorting routes per Gateway API spec.
///
/// Precedence (from highest to lowest):
/// 1. Exact path match
/// 2. Prefix path match with largest number of characters
/// 3. Method match
/// 4. Largest number of header matches
/// 5. Largest number of query param matches
#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchSpecificity {
    /// Whether this is an exact path match (highest priority)
    is_exact_path: bool,
    /// Length of the path prefix (longer = higher priority)
    path_prefix_len: usize,
    /// Whether there's a method match
    has_method_match: bool,
    /// Number of header matches
    header_match_count: usize,
    /// Number of query param matches
    query_param_match_count: usize,
}

impl Ord for MatchSpecificity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 1. Exact path match takes priority
        match self.is_exact_path.cmp(&other.is_exact_path) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }

        // 2. Longer path prefix takes priority
        match self.path_prefix_len.cmp(&other.path_prefix_len) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }

        // 3. Method match takes priority
        match self.has_method_match.cmp(&other.has_method_match) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }

        // 4. More header matches takes priority
        match self.header_match_count.cmp(&other.header_match_count) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }

        // 5. More query param matches takes priority
        self.query_param_match_count
            .cmp(&other.query_param_match_count)
    }
}

impl PartialOrd for MatchSpecificity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// HTTP Router that matches requests to routes
pub struct Router {
    /// Compiled regex patterns for path matching
    #[allow(dead_code)]
    path_patterns: Vec<CompiledPattern>,
}

#[allow(dead_code)]
struct CompiledPattern {
    route_idx: usize,
    rule_idx: usize,
    match_idx: usize,
    regex: Option<Regex>,
}

impl Router {
    /// Create a new router from configuration
    pub fn new(config: &GatewayConfig) -> Self {
        let mut path_patterns = Vec::new();

        // Pre-compile regex patterns
        for (route_idx, route) in config.routes.iter().enumerate() {
            for (rule_idx, rule) in route.rules.iter().enumerate() {
                for (match_idx, route_match) in rule.matches.iter().enumerate() {
                    if let Some(path) = &route_match.path
                        && path.match_type == PathMatchType::RegularExpression
                    {
                        let regex = Regex::new(&path.value).ok();
                        path_patterns.push(CompiledPattern {
                            route_idx,
                            rule_idx,
                            match_idx,
                            regex,
                        });
                    }
                }
            }
        }

        Self { path_patterns }
    }

    /// Route a request
    ///
    /// Per the Gateway API specification, matches are prioritized based on:
    /// 1. Exact path match
    /// 2. Prefix path match with largest number of characters
    /// 3. Method match
    /// 4. Largest number of header matches
    /// 5. Largest number of query param matches
    pub fn route(
        &self,
        config: &GatewayConfig,
        listener_name: &str,
        request: RequestInfo<'_>,
    ) -> RoutingResult {
        trace!(
            listener = listener_name,
            host = ?request.host,
            path = request.path,
            method = request.method,
            "Routing request"
        );

        // Collect all matching rules with their specificity scores
        let mut matches: Vec<(MatchSpecificity, &RouteConfig, &RouteRule)> = Vec::new();

        // Find matching routes for this listener
        for route in &config.routes {
            // Check if route is attached to this listener
            if !route
                .attached_listeners
                .contains(&listener_name.to_string())
            {
                continue;
            }

            // Check hostname match
            if !self.matches_hostname(&route.hostnames, request.host) {
                continue;
            }

            // Check rules and collect matches with specificity
            for rule in &route.rules {
                if let Some(specificity) = self.match_rule_with_specificity(
                    rule,
                    request.path,
                    request.method,
                    request.headers,
                    request.query_params,
                ) {
                    matches.push((specificity, route, rule));
                }
            }
        }

        // Sort by specificity (highest priority first)
        matches.sort_by(|a, b| b.0.cmp(&a.0));

        // Return the highest priority match
        if let Some((_, route, rule)) = matches.first() {
            self.create_result(route, rule, request.path)
        } else {
            // No match found
            RoutingResult {
                route: None,
                redirect: None,
            }
        }
    }

    fn matches_hostname(&self, hostnames: &[String], request_host: Option<&str>) -> bool {
        // If no hostnames specified, match all
        if hostnames.is_empty() {
            return true;
        }

        let request_host = match request_host {
            Some(h) => h,
            None => return false,
        };

        // Remove port from host if present
        let request_host = request_host.split(':').next().unwrap_or(request_host);

        for hostname in hostnames {
            if hostname.starts_with("*.") {
                // Wildcard match
                let suffix = &hostname[1..]; // Keep the dot
                if request_host.ends_with(suffix) {
                    return true;
                }
            } else if hostname == request_host {
                return true;
            }
        }

        false
    }

    /// Match a rule and return its specificity if it matches.
    ///
    /// Per Gateway API spec, multiple matches within a rule have OR semantics,
    /// so we return the highest specificity among matching entries.
    fn match_rule_with_specificity(
        &self,
        rule: &RouteRule,
        path: &str,
        method: &str,
        headers: &[(String, String)],
        query_params: &[(String, String)],
    ) -> Option<MatchSpecificity> {
        // Per spec: "If no matches are specified, the default is a prefix
        // path match on '/', which has the effect of matching every HTTP request."
        if rule.matches.is_empty() {
            return Some(MatchSpecificity {
                is_exact_path: false,
                path_prefix_len: 1, // Default "/" prefix has length 1
                has_method_match: false,
                header_match_count: 0,
                query_param_match_count: 0,
            });
        }

        // OR semantics between matches - find the highest specificity match
        let mut best_specificity: Option<MatchSpecificity> = None;

        for route_match in &rule.matches {
            if self.matches(route_match, path, method, headers, query_params) {
                let specificity = self.calculate_specificity(route_match);
                if let Some(ref best) = best_specificity {
                    if specificity > *best {
                        best_specificity = Some(specificity);
                    }
                } else {
                    best_specificity = Some(specificity);
                }
            }
        }

        best_specificity
    }

    /// Calculate the specificity of a route match
    ///
    /// Per Gateway API spec: "If no matches are specified, the default is a prefix
    /// path match on '/', which has the effect of matching every HTTP request."
    /// This means a match without a path implicitly has path prefix "/" (length 1).
    fn calculate_specificity(&self, route_match: &RouteMatch) -> MatchSpecificity {
        let (is_exact_path, path_prefix_len) = match &route_match.path {
            Some(path_match) => match path_match.match_type {
                PathMatchType::Exact => (true, path_match.value.len()),
                PathMatchType::PathPrefix => (false, path_match.value.len()),
                PathMatchType::RegularExpression => {
                    // Regex matches have implementation-specific precedence
                    // We treat them as lower priority than exact/prefix
                    (false, 0)
                }
            },
            // Per spec, no path means implicit "/" prefix (length 1)
            None => (false, 1),
        };

        MatchSpecificity {
            is_exact_path,
            path_prefix_len,
            has_method_match: route_match.method.is_some(),
            header_match_count: route_match.headers.len(),
            query_param_match_count: route_match.query_params.len(),
        }
    }

    #[allow(dead_code)]
    fn try_match_rule(
        &self,
        route: &RouteConfig,
        rule: &RouteRule,
        path: &str,
        method: &str,
        headers: &[(String, String)],
        query_params: &[(String, String)],
    ) -> Option<RoutingResult> {
        // If no matches, default to matching all
        if rule.matches.is_empty() {
            return Some(self.create_result(route, rule, path));
        }

        // OR semantics between matches
        for route_match in &rule.matches {
            if self.matches(route_match, path, method, headers, query_params) {
                return Some(self.create_result(route, rule, path));
            }
        }

        None
    }

    fn matches(
        &self,
        route_match: &RouteMatch,
        path: &str,
        method: &str,
        headers: &[(String, String)],
        query_params: &[(String, String)],
    ) -> bool {
        // Check path match
        if let Some(path_match) = &route_match.path
            && !self.matches_path(path_match, path)
        {
            return false;
        }

        // Check method match
        if let Some(expected_method) = &route_match.method
            && !method.eq_ignore_ascii_case(expected_method)
        {
            return false;
        }

        // Check header matches (AND semantics)
        for header_match in &route_match.headers {
            if !self.matches_header(header_match, headers) {
                return false;
            }
        }

        // Check query param matches (AND semantics)
        for param_match in &route_match.query_params {
            if !self.matches_query_param(param_match, query_params) {
                return false;
            }
        }

        true
    }

    fn matches_path(&self, path_match: &PathMatch, path: &str) -> bool {
        match path_match.match_type {
            PathMatchType::Exact => path == path_match.value,
            PathMatchType::PathPrefix => {
                if path_match.value == "/" {
                    true
                } else {
                    path == path_match.value || path.starts_with(&format!("{}/", path_match.value))
                }
            }
            PathMatchType::RegularExpression => {
                // Use pre-compiled regex if available
                if let Ok(regex) = Regex::new(&path_match.value) {
                    regex.is_match(path)
                } else {
                    false
                }
            }
        }
    }

    fn matches_header(&self, header_match: &HeaderMatch, headers: &[(String, String)]) -> bool {
        for (name, value) in headers {
            if name.eq_ignore_ascii_case(&header_match.name) {
                match header_match.match_type {
                    HeaderMatchType::Exact => {
                        if value == &header_match.value {
                            return true;
                        }
                    }
                    HeaderMatchType::RegularExpression => {
                        if let Ok(regex) = Regex::new(&header_match.value)
                            && regex.is_match(value)
                        {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn matches_query_param(
        &self,
        param_match: &QueryParamMatch,
        params: &[(String, String)],
    ) -> bool {
        for (name, value) in params {
            if name == &param_match.name {
                match param_match.match_type {
                    QueryParamMatchType::Exact => {
                        if value == &param_match.value {
                            return true;
                        }
                    }
                    QueryParamMatchType::RegularExpression => {
                        if let Ok(regex) = Regex::new(&param_match.value)
                            && regex.is_match(value)
                        {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn create_result(
        &self,
        route: &RouteConfig,
        rule: &RouteRule,
        request_path: &str,
    ) -> RoutingResult {
        let route_id = format!("{}/{}", route.namespace, route.name);

        // Check for redirect filter
        for filter in &rule.filters {
            if let RouteFilter::RequestRedirect {
                scheme,
                hostname,
                port,
                path,
                status_code,
            } = filter
            {
                let redirect_path = path.as_ref().map(|p| match p {
                    crate::config::PathModifier::ReplaceFullPath { value } => value.clone(),
                    crate::config::PathModifier::ReplacePrefixMatch { value } => {
                        // Find the matching prefix and replace it
                        if let Some(path_match) = rule.matches.first().and_then(|m| m.path.as_ref())
                        {
                            request_path.replacen(&path_match.value, value, 1)
                        } else {
                            value.clone()
                        }
                    }
                });

                return RoutingResult {
                    route: None,
                    redirect: Some(RedirectAction {
                        scheme: scheme.clone(),
                        hostname: hostname.clone(),
                        port: *port,
                        path: redirect_path,
                        status_code: status_code.unwrap_or(302),
                    }),
                };
            }
        }

        // Extract URL rewrite if present
        let (hostname_rewrite, path_rewrite) =
            self.extract_rewrites(&rule.filters, rule, request_path);

        RoutingResult {
            route: Some(MatchedRoute {
                route_id,
                rule_name: rule.name.clone(),
                backends: rule.backends.clone(),
                filters: rule.filters.clone(),
                timeout_secs: rule.timeout.as_ref().and_then(|t| t.request),
                backend_timeout_secs: rule.timeout.as_ref().and_then(|t| t.backend_request),
                hostname_rewrite,
                path_rewrite,
            }),
            redirect: None,
        }
    }

    fn extract_rewrites(
        &self,
        filters: &[RouteFilter],
        rule: &RouteRule,
        _request_path: &str,
    ) -> (Option<String>, Option<PathRewrite>) {
        let mut hostname_rewrite = None;
        let mut path_rewrite = None;

        for filter in filters {
            if let RouteFilter::URLRewrite { hostname, path } = filter {
                if hostname.is_some() {
                    hostname_rewrite = hostname.clone();
                }
                if let Some(p) = path {
                    path_rewrite = Some(match p {
                        crate::config::PathModifier::ReplaceFullPath { value } => {
                            PathRewrite::ReplaceFullPath(value.clone())
                        }
                        crate::config::PathModifier::ReplacePrefixMatch { value } => {
                            let match_prefix = rule
                                .matches
                                .first()
                                .and_then(|m| m.path.as_ref())
                                .map(|p| p.value.clone())
                                .unwrap_or_else(|| "/".to_string());
                            PathRewrite::ReplacePrefixMatch {
                                match_prefix,
                                replacement: value.clone(),
                            }
                        }
                    });
                }
            }
        }

        (hostname_rewrite, path_rewrite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        BackendRef, HeaderMatch, HeaderMatchType, PathMatch, PathMatchType, PathModifier,
        QueryParamMatch, QueryParamMatchType, RouteConfig, RouteMatch, RouteRule,
    };

    /// Helper trait to make test routing calls more concise
    trait RouterTestExt {
        fn route_test(
            &self,
            config: &GatewayConfig,
            listener_name: &str,
            host: Option<&str>,
            path: &str,
            method: &str,
            headers: &[(String, String)],
            query_params: &[(String, String)],
        ) -> RoutingResult;
    }

    impl RouterTestExt for Router {
        fn route_test(
            &self,
            config: &GatewayConfig,
            listener_name: &str,
            host: Option<&str>,
            path: &str,
            method: &str,
            headers: &[(String, String)],
            query_params: &[(String, String)],
        ) -> RoutingResult {
            self.route(
                config,
                listener_name,
                RequestInfo {
                    host,
                    path,
                    method,
                    headers,
                    query_params,
                },
            )
        }
    }

    fn create_test_config() -> GatewayConfig {
        GatewayConfig {
            version: "v1".to_string(),
            gateway: crate::config::GatewayRef {
                namespace: "default".to_string(),
                name: "test-gateway".to_string(),
            },
            listeners: vec![crate::config::ListenerConfig {
                name: "http".to_string(),
                port: 80,
                container_port: 8080,
                protocol: crate::config::Protocol::Http,
                hostname: None,
                tls: None,
            }],
            routes: vec![RouteConfig {
                name: "test-route".to_string(),
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
                        headers: vec![],
                        query_params: vec![],
                        method: None,
                    }],
                    filters: vec![],
                    backends: vec![BackendRef {
                        namespace: "default".to_string(),
                        name: "backend-service".to_string(),
                        port: 8080,
                        weight: 1,
                    }],
                    timeout: None,
                }],
            }],
        }
    }

    // ==========================================
    // Basic Route Matching Tests
    // ==========================================

    #[test]
    fn test_route_matching() {
        let config = create_test_config();
        let router = Router::new(&config);

        // Should match
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should not match - wrong host
        let result = router.route_test(
            &config,
            "http",
            Some("other.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());

        // Should not match - wrong path
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/other",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());
    }

    // ==========================================
    // Hostname Matching Tests (Gateway API Spec)
    // ==========================================

    /// Spec: Wildcard hostnames (*.example.com) match subdomains
    #[test]
    fn test_wildcard_hostname() {
        let mut config = create_test_config();
        config.routes[0].hostnames = vec!["*.example.com".to_string()];
        let router = Router::new(&config);

        // Should match wildcard
        let result = router.route_test(
            &config,
            "http",
            Some("api.example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should not match base domain for wildcard
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());
    }

    /// Spec: Multiple levels of subdomains should match wildcard
    #[test]
    fn test_wildcard_hostname_multi_level() {
        let mut config = create_test_config();
        config.routes[0].hostnames = vec!["*.example.com".to_string()];
        let router = Router::new(&config);

        // Should match multi-level subdomain
        let result = router.route_test(
            &config,
            "http",
            Some("api.v2.example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());
    }

    /// Spec: Empty hostnames means match all hosts
    #[test]
    fn test_empty_hostnames_matches_all() {
        let mut config = create_test_config();
        config.routes[0].hostnames = vec![];
        let router = Router::new(&config);

        // Should match any host
        let result = router.route_test(
            &config,
            "http",
            Some("anything.example.org"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should also match with no host
        let result = router.route_test(&config, "http", None, "/api/users", "GET", &[], &[]);
        assert!(result.route.is_some());
    }

    /// Spec: Host header with port should strip port for matching
    #[test]
    fn test_hostname_with_port() {
        let config = create_test_config();
        let router = Router::new(&config);

        // Should match even with port in Host header
        let result = router.route_test(
            &config,
            "http",
            Some("example.com:8080"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());
    }

    // ==========================================
    // Path Matching Tests (Gateway API Spec)
    // ==========================================

    /// Spec: PathPrefix "/" matches all paths
    #[test]
    fn test_path_prefix_root() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].path = Some(PathMatch {
            match_type: PathMatchType::PathPrefix,
            value: "/".to_string(),
        });
        let router = Router::new(&config);

        // Should match any path
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/anything",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        let result = router.route_test(&config, "http", Some("example.com"), "/", "GET", &[], &[]);
        assert!(result.route.is_some());
    }

    /// Spec: PathPrefix match requires exact prefix OR prefix followed by /
    #[test]
    fn test_path_prefix_boundary() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].path = Some(PathMatch {
            match_type: PathMatchType::PathPrefix,
            value: "/api".to_string(),
        });
        let router = Router::new(&config);

        // Should match exact prefix
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should match prefix followed by /
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should NOT match /apiversion (no / boundary)
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/apiversion",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());
    }

    /// Spec: Exact path match requires exact string equality
    #[test]
    fn test_exact_path_match() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].path = Some(PathMatch {
            match_type: PathMatchType::Exact,
            value: "/api/v1/users".to_string(),
        });
        let router = Router::new(&config);

        // Should match exact path
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/v1/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should NOT match with trailing slash
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/v1/users/",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());

        // Should NOT match sub-path
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/v1/users/123",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());
    }

    /// Spec: RegularExpression path match uses RE2 compatible regex
    #[test]
    fn test_regex_path_match() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].path = Some(PathMatch {
            match_type: PathMatchType::RegularExpression,
            value: "^/api/v[0-9]+/users$".to_string(),
        });
        let router = Router::new(&config);

        // Should match v1
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/v1/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should match v2
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/v2/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should NOT match vX
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/vX/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());
    }

    // ==========================================
    // Header Matching Tests (Gateway API Spec)
    // ==========================================

    /// Spec: Exact header match is case-insensitive on name
    #[test]
    fn test_header_match_exact() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].headers = vec![HeaderMatch {
            name: "X-Custom-Header".to_string(),
            match_type: HeaderMatchType::Exact,
            value: "expected-value".to_string(),
        }];
        let router = Router::new(&config);

        // Should match with exact value
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[("X-Custom-Header".to_string(), "expected-value".to_string())],
            &[],
        );
        assert!(result.route.is_some());

        // Should match with different case header name
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[("x-custom-header".to_string(), "expected-value".to_string())],
            &[],
        );
        assert!(result.route.is_some());

        // Should NOT match with wrong value
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[("X-Custom-Header".to_string(), "wrong-value".to_string())],
            &[],
        );
        assert!(result.route.is_none());
    }

    /// Spec: Multiple header matches have AND semantics
    #[test]
    fn test_multiple_header_matches_and_semantics() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].headers = vec![
            HeaderMatch {
                name: "X-Header-One".to_string(),
                match_type: HeaderMatchType::Exact,
                value: "value1".to_string(),
            },
            HeaderMatch {
                name: "X-Header-Two".to_string(),
                match_type: HeaderMatchType::Exact,
                value: "value2".to_string(),
            },
        ];
        let router = Router::new(&config);

        // Should match when ALL headers present
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[
                ("X-Header-One".to_string(), "value1".to_string()),
                ("X-Header-Two".to_string(), "value2".to_string()),
            ],
            &[],
        );
        assert!(result.route.is_some());

        // Should NOT match when only one header present
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[("X-Header-One".to_string(), "value1".to_string())],
            &[],
        );
        assert!(result.route.is_none());
    }

    /// Spec: Regex header match
    #[test]
    fn test_header_match_regex() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].headers = vec![HeaderMatch {
            name: "X-Request-ID".to_string(),
            match_type: HeaderMatchType::RegularExpression,
            value: "^[a-f0-9]{8}-[a-f0-9]{4}".to_string(),
        }];
        let router = Router::new(&config);

        // Should match UUID-like pattern
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[("X-Request-ID".to_string(), "12345678-abcd-1234".to_string())],
            &[],
        );
        assert!(result.route.is_some());

        // Should NOT match non-matching pattern
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[("X-Request-ID".to_string(), "not-a-uuid".to_string())],
            &[],
        );
        assert!(result.route.is_none());
    }

    // ==========================================
    // Query Parameter Matching Tests (Gateway API Spec)
    // ==========================================

    /// Spec: Exact query param match
    #[test]
    fn test_query_param_match_exact() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].query_params = vec![QueryParamMatch {
            name: "version".to_string(),
            match_type: QueryParamMatchType::Exact,
            value: "v2".to_string(),
        }];
        let router = Router::new(&config);

        // Should match exact value
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[("version".to_string(), "v2".to_string())],
        );
        assert!(result.route.is_some());

        // Should NOT match wrong value
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[("version".to_string(), "v1".to_string())],
        );
        assert!(result.route.is_none());
    }

    /// Spec: Regex query param match
    #[test]
    fn test_query_param_match_regex() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].query_params = vec![QueryParamMatch {
            name: "id".to_string(),
            match_type: QueryParamMatchType::RegularExpression,
            value: "^[0-9]+$".to_string(),
        }];
        let router = Router::new(&config);

        // Should match numeric ID
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[("id".to_string(), "12345".to_string())],
        );
        assert!(result.route.is_some());

        // Should NOT match non-numeric
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[("id".to_string(), "abc".to_string())],
        );
        assert!(result.route.is_none());
    }

    // ==========================================
    // Method Matching Tests (Gateway API Spec)
    // ==========================================

    /// Spec: Method match is case-insensitive
    #[test]
    fn test_method_match() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches[0].method = Some("POST".to_string());
        let router = Router::new(&config);

        // Should match POST
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "POST",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should match lowercase post
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "post",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should NOT match GET
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());
    }

    // ==========================================
    // Multiple Match OR Semantics Tests (Gateway API Spec)
    // ==========================================

    /// Spec: Multiple matches within a rule have OR semantics
    #[test]
    fn test_multiple_matches_or_semantics() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches = vec![
            RouteMatch {
                path: Some(PathMatch {
                    match_type: PathMatchType::PathPrefix,
                    value: "/api".to_string(),
                }),
                headers: vec![],
                query_params: vec![],
                method: None,
            },
            RouteMatch {
                path: Some(PathMatch {
                    match_type: PathMatchType::PathPrefix,
                    value: "/admin".to_string(),
                }),
                headers: vec![],
                query_params: vec![],
                method: None,
            },
        ];
        let router = Router::new(&config);

        // Should match /api
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should match /admin
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/admin/dashboard",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should NOT match /other
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/other",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());
    }

    // ==========================================
    // Redirect Filter Tests (Gateway API Spec)
    // ==========================================

    /// Spec: RequestRedirect filter returns redirect action
    #[test]
    fn test_redirect_filter() {
        let mut config = create_test_config();
        config.routes[0].rules[0].filters = vec![RouteFilter::RequestRedirect {
            scheme: Some("https".to_string()),
            hostname: Some("secure.example.com".to_string()),
            port: Some(443),
            path: Some(PathModifier::ReplaceFullPath {
                value: "/secure".to_string(),
            }),
            status_code: Some(301),
        }];
        let router = Router::new(&config);

        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );

        // Should have redirect, not route
        assert!(result.route.is_none());
        assert!(result.redirect.is_some());

        let redirect = result.redirect.unwrap();
        assert_eq!(redirect.scheme, Some("https".to_string()));
        assert_eq!(redirect.hostname, Some("secure.example.com".to_string()));
        assert_eq!(redirect.port, Some(443));
        assert_eq!(redirect.path, Some("/secure".to_string()));
        assert_eq!(redirect.status_code, 301);
    }

    /// Spec: Redirect with default status code (302)
    #[test]
    fn test_redirect_filter_default_status() {
        let mut config = create_test_config();
        config.routes[0].rules[0].filters = vec![RouteFilter::RequestRedirect {
            scheme: None,
            hostname: Some("new.example.com".to_string()),
            port: None,
            path: None,
            status_code: None, // Default
        }];
        let router = Router::new(&config);

        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );

        let redirect = result.redirect.unwrap();
        assert_eq!(redirect.status_code, 302); // Default status
    }

    // ==========================================
    // URL Rewrite Filter Tests (Gateway API Spec)
    // ==========================================

    /// Spec: URLRewrite filter modifies hostname
    #[test]
    fn test_url_rewrite_hostname() {
        let mut config = create_test_config();
        config.routes[0].rules[0].filters = vec![RouteFilter::URLRewrite {
            hostname: Some("backend.internal".to_string()),
            path: None,
        }];
        let router = Router::new(&config);

        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );

        let route = result.route.unwrap();
        assert_eq!(route.hostname_rewrite, Some("backend.internal".to_string()));
    }

    /// Spec: URLRewrite filter with ReplaceFullPath
    #[test]
    fn test_url_rewrite_replace_full_path() {
        let mut config = create_test_config();
        config.routes[0].rules[0].filters = vec![RouteFilter::URLRewrite {
            hostname: None,
            path: Some(PathModifier::ReplaceFullPath {
                value: "/new-path".to_string(),
            }),
        }];
        let router = Router::new(&config);

        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );

        let route = result.route.unwrap();
        assert!(matches!(
            route.path_rewrite,
            Some(PathRewrite::ReplaceFullPath(path)) if path == "/new-path"
        ));
    }

    /// Spec: URLRewrite filter with ReplacePrefixMatch
    #[test]
    fn test_url_rewrite_replace_prefix_match() {
        let mut config = create_test_config();
        config.routes[0].rules[0].filters = vec![RouteFilter::URLRewrite {
            hostname: None,
            path: Some(PathModifier::ReplacePrefixMatch {
                value: "/v2".to_string(),
            }),
        }];
        let router = Router::new(&config);

        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );

        let route = result.route.unwrap();
        assert!(matches!(
            route.path_rewrite,
            Some(PathRewrite::ReplacePrefixMatch { match_prefix, replacement })
                if match_prefix == "/api" && replacement == "/v2"
        ));
    }

    // ==========================================
    // Listener Attachment Tests (Gateway API Spec)
    // ==========================================

    /// Spec: Routes only match for attached listeners
    #[test]
    fn test_listener_attachment() {
        let mut config = create_test_config();
        config.listeners.push(crate::config::ListenerConfig {
            name: "https".to_string(),
            port: 443,
            container_port: 8443,
            protocol: crate::config::Protocol::Https,
            hostname: None,
            tls: None,
        });
        // Route is only attached to "http" listener
        let router = Router::new(&config);

        // Should match on http listener
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());

        // Should NOT match on https listener (not attached)
        let result = router.route_test(
            &config,
            "https",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_none());
    }

    // ==========================================
    // Empty/Default Match Tests (Gateway API Spec)
    // ==========================================

    /// Spec: Rule with no matches defaults to matching everything
    #[test]
    fn test_empty_matches_match_all() {
        let mut config = create_test_config();
        config.routes[0].rules[0].matches = vec![];
        let router = Router::new(&config);

        // Should match any path
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/anything/here",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());
    }

    // ==========================================
    // Match Precedence Tests (Gateway API Spec)
    // ==========================================

    /// Spec: Longer path prefix takes precedence over shorter path prefix
    ///
    /// This test mirrors the HTTPRouteMatching conformance test which validates
    /// that `/v2` prefix matches before `/` prefix for requests to `/v2/*`.
    #[test]
    fn test_path_prefix_precedence_longer_wins() {
        let mut config = create_test_config();
        config.routes[0].hostnames = vec![]; // Match all hosts

        // Rule 1: PathPrefix "/" -> backend-v1
        // Rule 2: PathPrefix "/v2" -> backend-v2
        // Per spec, longer prefix should win
        config.routes[0].rules = vec![
            RouteRule {
                name: None,
                matches: vec![RouteMatch {
                    path: Some(PathMatch {
                        match_type: PathMatchType::PathPrefix,
                        value: "/".to_string(),
                    }),
                    headers: vec![],
                    query_params: vec![],
                    method: None,
                }],
                filters: vec![],
                backends: vec![BackendRef {
                    namespace: "default".to_string(),
                    name: "backend-v1".to_string(),
                    port: 8080,
                    weight: 1,
                }],
                timeout: None,
            },
            RouteRule {
                name: None,
                matches: vec![RouteMatch {
                    path: Some(PathMatch {
                        match_type: PathMatchType::PathPrefix,
                        value: "/v2".to_string(),
                    }),
                    headers: vec![],
                    query_params: vec![],
                    method: None,
                }],
                filters: vec![],
                backends: vec![BackendRef {
                    namespace: "default".to_string(),
                    name: "backend-v2".to_string(),
                    port: 8080,
                    weight: 1,
                }],
                timeout: None,
            },
        ];
        let router = Router::new(&config);

        // Request to "/" should go to backend-v1
        let result = router.route_test(&config, "http", Some("example.com"), "/", "GET", &[], &[]);
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-v1");

        // Request to "/example" should go to backend-v1 (matches "/" prefix)
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/example",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-v1");

        // Request to "/v2" should go to backend-v2 (longer prefix wins)
        let result =
            router.route_test(&config, "http", Some("example.com"), "/v2", "GET", &[], &[]);
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-v2");

        // Request to "/v2/example" should go to backend-v2 (longer prefix wins)
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/v2/example",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-v2");

        // Request to "/v2example" should go to backend-v1 (no "/" boundary after /v2)
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/v2example",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-v1");
    }

    /// Spec: Header matches affect precedence
    ///
    /// This test validates that rules with more header matches take precedence
    /// when path matches are equal.
    #[test]
    fn test_header_match_precedence() {
        let mut config = create_test_config();
        config.routes[0].hostnames = vec![]; // Match all hosts

        // Rule 1: PathPrefix "/" with header Version=one -> backend-v1
        // Rule 2: PathPrefix "/" with header Version=two -> backend-v2
        config.routes[0].rules = vec![
            RouteRule {
                name: None,
                matches: vec![
                    // Match 1: "/" prefix (fallback)
                    RouteMatch {
                        path: Some(PathMatch {
                            match_type: PathMatchType::PathPrefix,
                            value: "/".to_string(),
                        }),
                        headers: vec![],
                        query_params: vec![],
                        method: None,
                    },
                    // Match 2: header Version=one
                    RouteMatch {
                        path: None,
                        headers: vec![HeaderMatch {
                            name: "version".to_string(),
                            match_type: HeaderMatchType::Exact,
                            value: "one".to_string(),
                        }],
                        query_params: vec![],
                        method: None,
                    },
                ],
                filters: vec![],
                backends: vec![BackendRef {
                    namespace: "default".to_string(),
                    name: "backend-v1".to_string(),
                    port: 8080,
                    weight: 1,
                }],
                timeout: None,
            },
            RouteRule {
                name: None,
                matches: vec![
                    // Match 1: "/v2" prefix
                    RouteMatch {
                        path: Some(PathMatch {
                            match_type: PathMatchType::PathPrefix,
                            value: "/v2".to_string(),
                        }),
                        headers: vec![],
                        query_params: vec![],
                        method: None,
                    },
                    // Match 2: header Version=two
                    RouteMatch {
                        path: None,
                        headers: vec![HeaderMatch {
                            name: "version".to_string(),
                            match_type: HeaderMatchType::Exact,
                            value: "two".to_string(),
                        }],
                        query_params: vec![],
                        method: None,
                    },
                ],
                filters: vec![],
                backends: vec![BackendRef {
                    namespace: "default".to_string(),
                    name: "backend-v2".to_string(),
                    port: 8080,
                    weight: 1,
                }],
                timeout: None,
            },
        ];
        let router = Router::new(&config);

        // Request with header Version=one should go to backend-v1
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/",
            "GET",
            &[("version".to_string(), "one".to_string())],
            &[],
        );
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-v1");

        // Request with header Version=two should go to backend-v2
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/",
            "GET",
            &[("version".to_string(), "two".to_string())],
            &[],
        );
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-v2");

        // Request to /v2 without headers should go to backend-v2 (longer path prefix)
        let result =
            router.route_test(&config, "http", Some("example.com"), "/v2", "GET", &[], &[]);
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-v2");
    }

    /// Spec: Exact path match takes precedence over prefix match
    #[test]
    fn test_exact_path_precedence_over_prefix() {
        let mut config = create_test_config();
        config.routes[0].hostnames = vec![]; // Match all hosts

        // Rule 1: PathPrefix "/api" -> backend-prefix
        // Rule 2: Exact "/api/users" -> backend-exact
        config.routes[0].rules = vec![
            RouteRule {
                name: None,
                matches: vec![RouteMatch {
                    path: Some(PathMatch {
                        match_type: PathMatchType::PathPrefix,
                        value: "/api".to_string(),
                    }),
                    headers: vec![],
                    query_params: vec![],
                    method: None,
                }],
                filters: vec![],
                backends: vec![BackendRef {
                    namespace: "default".to_string(),
                    name: "backend-prefix".to_string(),
                    port: 8080,
                    weight: 1,
                }],
                timeout: None,
            },
            RouteRule {
                name: None,
                matches: vec![RouteMatch {
                    path: Some(PathMatch {
                        match_type: PathMatchType::Exact,
                        value: "/api/users".to_string(),
                    }),
                    headers: vec![],
                    query_params: vec![],
                    method: None,
                }],
                filters: vec![],
                backends: vec![BackendRef {
                    namespace: "default".to_string(),
                    name: "backend-exact".to_string(),
                    port: 8080,
                    weight: 1,
                }],
                timeout: None,
            },
        ];
        let router = Router::new(&config);

        // Request to "/api/users" should go to backend-exact (exact match wins)
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/users",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-exact");

        // Request to "/api/other" should go to backend-prefix (prefix match)
        let result = router.route_test(
            &config,
            "http",
            Some("example.com"),
            "/api/other",
            "GET",
            &[],
            &[],
        );
        assert!(result.route.is_some());
        let route = result.route.unwrap();
        assert_eq!(route.backends[0].name, "backend-prefix");
    }
}
