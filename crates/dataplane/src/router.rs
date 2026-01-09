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
                    if let Some(path) = &route_match.path {
                        if path.match_type == PathMatchType::RegularExpression {
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
        }

        Self { path_patterns }
    }

    /// Route a request
    pub fn route(
        &self,
        config: &GatewayConfig,
        listener_name: &str,
        host: Option<&str>,
        path: &str,
        method: &str,
        headers: &[(String, String)],
        query_params: &[(String, String)],
    ) -> RoutingResult {
        trace!(
            listener = listener_name,
            host = ?host,
            path = path,
            method = method,
            "Routing request"
        );

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
            if !self.matches_hostname(&route.hostnames, host) {
                continue;
            }

            // Check rules
            for rule in &route.rules {
                if let Some(result) =
                    self.try_match_rule(route, rule, path, method, headers, query_params)
                {
                    return result;
                }
            }
        }

        // No match found
        RoutingResult {
            route: None,
            redirect: None,
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
        if let Some(path_match) = &route_match.path {
            if !self.matches_path(path_match, path) {
                return false;
            }
        }

        // Check method match
        if let Some(expected_method) = &route_match.method {
            if !method.eq_ignore_ascii_case(expected_method) {
                return false;
            }
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
                        if let Ok(regex) = Regex::new(&header_match.value) {
                            if regex.is_match(value) {
                                return true;
                            }
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
                        if let Ok(regex) = Regex::new(&param_match.value) {
                            if regex.is_match(value) {
                                return true;
                            }
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
    use crate::config::{BackendRef, PathMatch, PathMatchType, RouteConfig, RouteMatch, RouteRule};

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

    #[test]
    fn test_route_matching() {
        let config = create_test_config();
        let router = Router::new(&config);

        // Should match
        let result = router.route(
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
        let result = router.route(
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
        let result = router.route(
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

    #[test]
    fn test_wildcard_hostname() {
        let mut config = create_test_config();
        config.routes[0].hostnames = vec!["*.example.com".to_string()];
        let router = Router::new(&config);

        // Should match wildcard
        let result = router.route(
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
        let result = router.route(
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
}
