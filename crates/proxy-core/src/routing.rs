// Pure route matching engine (Sans-I/O).
//
// Implements hostname, path, header, and query parameter matching against
// incoming requests. All functions are pure — no I/O, no async — taking
// a request and route table as input and returning the matched backend.

use regex::Regex;

use crate::config::{
    Backend, Filter, HeaderMatch, HeaderMatchType, PathMatchType, ProxyConfig, QueryParamMatch,
    QueryParamMatchType, RouteConfig, RouteMatch, RouteRule, TimeoutConfig,
};

// ---------------------------------------------------------------------------
// Request metadata
// ---------------------------------------------------------------------------

/// Extracted request metadata used for route matching.
///
/// Uses borrowed references to avoid copying request data during matching.
#[derive(Debug)]
pub struct RequestMeta<'a> {
    /// The Host header value (without port).
    pub host: Option<&'a str>,
    /// The request path (e.g. `/api/users`).
    pub path: &'a str,
    /// The HTTP method (e.g. `GET`, `POST`).
    pub method: &'a str,
    /// Request headers as `(name, value)` pairs.
    pub headers: &'a [(&'a str, &'a str)],
    /// Query parameters as `(name, value)` pairs.
    pub query_params: &'a [(&'a str, &'a str)],
}

// ---------------------------------------------------------------------------
// Routing decision
// ---------------------------------------------------------------------------

/// The outcome of route matching.
#[derive(Debug)]
pub enum RoutingDecision<'a> {
    /// Forward the request to the given backends, applying filters.
    Forward {
        backends: &'a [Backend],
        filters: &'a [Filter],
        timeout: Option<&'a TimeoutConfig>,
    },
    /// Redirect the client.
    Redirect(RedirectResponse),
    /// No matching route was found.
    NotFound,
}

/// A redirect response to send to the client.
#[derive(Debug, PartialEq)]
pub struct RedirectResponse {
    /// HTTP status code (e.g. 301, 302).
    pub status_code: u16,
    /// The Location URL to redirect to.
    pub location: String,
}

// ---------------------------------------------------------------------------
// Core routing function
// ---------------------------------------------------------------------------

/// Match a request against the proxy configuration for a specific listener.
///
/// This function is pure: no I/O, no async, no side effects.
///
/// # Matching algorithm
///
/// 1. Filter routes to those attached to the named listener.
/// 2. For each route, check hostname match.
/// 3. For each rule in a matching route, evaluate match conditions.
/// 4. Among all matching rules, select the one with the highest precedence
///    (exact path > longest prefix > others).
/// 5. If the winning rule has a `RequestRedirect` filter, return `Redirect`.
/// 6. Otherwise return `Forward` with the rule's backends, filters, and timeout.
pub fn route<'a>(
    config: &'a ProxyConfig,
    listener_name: &str,
    request: &RequestMeta<'_>,
) -> RoutingDecision<'a> {
    let mut best: Option<MatchCandidate<'a>> = None;

    for route in &config.routes {
        if !route
            .attached_listeners
            .contains(&listener_name.to_string())
        {
            continue;
        }

        if !hostname_matches(route, request.host) {
            continue;
        }

        for rule in &route.rules {
            if let Some(precedence) = rule_matches(rule, request) {
                let dominated = best
                    .as_ref()
                    .is_none_or(|b| precedence_beats(&precedence, &b.precedence));
                if dominated {
                    best = Some(MatchCandidate { rule, precedence });
                }
            }
        }
    }

    match best {
        Some(candidate) => build_decision(candidate.rule, request),
        None => RoutingDecision::NotFound,
    }
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

/// A candidate rule that matched the request, together with its precedence.
struct MatchCandidate<'a> {
    rule: &'a RouteRule,
    precedence: MatchPrecedence,
}

/// Precedence ranking for tie-breaking among matched rules.
///
/// Higher-priority matches sort first. The Gateway API spec defines:
/// 1. Exact path matches beat prefix matches
/// 2. Longer prefix matches beat shorter ones
/// 3. Method match (present vs absent) is a secondary tiebreaker
/// 4. More header matches beat fewer
/// 5. More query param matches beat fewer
#[derive(Debug, PartialEq, Eq)]
struct MatchPrecedence {
    /// 2 = Exact path, 1 = Prefix path, 0 = no path / regex
    path_kind: u8,
    /// Length of the path match value (used for prefix tiebreaking).
    path_length: usize,
    /// Whether a method match was specified.
    has_method: bool,
    /// Number of header match conditions.
    header_count: usize,
    /// Number of query param match conditions.
    query_param_count: usize,
}

/// Returns true if `a` should beat `b` in precedence.
fn precedence_beats(a: &MatchPrecedence, b: &MatchPrecedence) -> bool {
    let a_tuple = (
        a.path_kind,
        a.path_length,
        a.has_method,
        a.header_count,
        a.query_param_count,
    );
    let b_tuple = (
        b.path_kind,
        b.path_length,
        b.has_method,
        b.header_count,
        b.query_param_count,
    );
    a_tuple > b_tuple
}

// ---------------------------------------------------------------------------
// Hostname matching
// ---------------------------------------------------------------------------

/// Check if the request's host matches any of the route's hostnames.
///
/// - An empty hostname list matches all hosts.
/// - Wildcard hostnames like `*.example.com` match `foo.example.com`
///   but not `example.com` or `foo.bar.example.com`.
/// - Comparison is case-insensitive.
fn hostname_matches(route: &RouteConfig, host: Option<&str>) -> bool {
    if route.hostnames.is_empty() {
        return true;
    }

    let host = match host {
        Some(h) => h,
        None => return false,
    };

    // Strip port from host header if present.
    let host = strip_port(host);

    route.hostnames.iter().any(|pattern| {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            // Wildcard match: must have exactly one label before the suffix.
            let host_lower = host.to_ascii_lowercase();
            let suffix_lower = suffix.to_ascii_lowercase();
            if let Some(prefix) = host_lower.strip_suffix(&format!(".{suffix_lower}")) {
                // The prefix must be a single label (no dots).
                !prefix.is_empty() && !prefix.contains('.')
            } else {
                false
            }
        } else {
            host.eq_ignore_ascii_case(pattern)
        }
    })
}

/// Strip port from a host header value (e.g. `example.com:8080` -> `example.com`).
fn strip_port(host: &str) -> &str {
    // Handle IPv6 addresses like [::1]:8080
    if host.starts_with('[') {
        return host;
    }
    match host.rfind(':') {
        Some(pos) => &host[..pos],
        None => host,
    }
}

// ---------------------------------------------------------------------------
// Rule matching
// ---------------------------------------------------------------------------

/// Check if a rule matches the request. Returns `Some(precedence)` on match.
///
/// A rule with empty matches matches all requests.
/// Multiple `RouteMatch` values within a rule use OR semantics.
fn rule_matches(rule: &RouteRule, request: &RequestMeta<'_>) -> Option<MatchPrecedence> {
    if rule.matches.is_empty() {
        // A rule with no matches matches everything (like a default backend).
        return Some(MatchPrecedence {
            path_kind: 0,
            path_length: 0,
            has_method: false,
            header_count: 0,
            query_param_count: 0,
        });
    }

    // OR semantics: return the best precedence among all matches that pass.
    let mut best: Option<MatchPrecedence> = None;
    for route_match in &rule.matches {
        if let Some(prec) = single_match_applies(route_match, request) {
            let dominated = best.as_ref().is_none_or(|b| precedence_beats(&prec, b));
            if dominated {
                best = Some(prec);
            }
        }
    }
    best
}

/// Evaluate a single `RouteMatch` against the request. All conditions within
/// a match use AND semantics.
fn single_match_applies(
    route_match: &RouteMatch,
    request: &RequestMeta<'_>,
) -> Option<MatchPrecedence> {
    // Path matching.
    let (path_kind, path_length) = match &route_match.path {
        Some(pm) => match path_matches(&pm.match_type, &pm.value, request.path) {
            true => match pm.match_type {
                PathMatchType::Exact => (2u8, pm.value.len()),
                PathMatchType::PathPrefix => (1u8, pm.value.len()),
                PathMatchType::RegularExpression => (0u8, pm.value.len()),
            },
            false => return None,
        },
        None => (0u8, 0),
    };

    // Method matching (case-insensitive).
    let has_method = route_match.method.is_some();
    if let Some(method) = &route_match.method
        && !request.method.eq_ignore_ascii_case(method)
    {
        return None;
    }

    // Header matching (AND semantics).
    for header_match in &route_match.headers {
        if !header_matches(header_match, request.headers) {
            return None;
        }
    }

    // Query param matching (AND semantics).
    for qp_match in &route_match.query_params {
        if !query_param_matches(qp_match, request.query_params) {
            return None;
        }
    }

    Some(MatchPrecedence {
        path_kind,
        path_length,
        has_method,
        header_count: route_match.headers.len(),
        query_param_count: route_match.query_params.len(),
    })
}

// ---------------------------------------------------------------------------
// Path matching
// ---------------------------------------------------------------------------

/// Check if the request path matches the given path match condition.
fn path_matches(match_type: &PathMatchType, value: &str, path: &str) -> bool {
    match match_type {
        PathMatchType::Exact => path == value,
        PathMatchType::PathPrefix => prefix_path_matches(value, path),
        PathMatchType::RegularExpression => Regex::new(value).is_ok_and(|re| re.is_match(path)),
    }
}

/// Segment-boundary-aware prefix matching.
///
/// `/api` matches `/api` and `/api/users` but NOT `/apiversion`.
/// `/` matches all paths.
fn prefix_path_matches(prefix: &str, path: &str) -> bool {
    if prefix == "/" {
        return true;
    }

    if path == prefix {
        return true;
    }

    // The path must start with the prefix and the next character must be `/`.
    path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/')
}

// ---------------------------------------------------------------------------
// Header matching
// ---------------------------------------------------------------------------

/// Check if the request headers satisfy a single header match condition.
///
/// Header name comparison is case-insensitive.
/// Value comparison depends on the match type.
fn header_matches(hm: &HeaderMatch, headers: &[(&str, &str)]) -> bool {
    headers.iter().any(|(name, value)| {
        if !name.eq_ignore_ascii_case(&hm.name) {
            return false;
        }
        match hm.match_type {
            HeaderMatchType::Exact => *value == hm.value,
            HeaderMatchType::RegularExpression => {
                Regex::new(&hm.value).is_ok_and(|re| re.is_match(value))
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Query parameter matching
// ---------------------------------------------------------------------------

/// Check if the request query params satisfy a single query param match condition.
///
/// Name and value comparison is case-sensitive per the Gateway API spec.
fn query_param_matches(qpm: &QueryParamMatch, query_params: &[(&str, &str)]) -> bool {
    query_params.iter().any(|(name, value)| {
        if *name != qpm.name {
            return false;
        }
        match qpm.match_type {
            QueryParamMatchType::Exact => *value == qpm.value,
            QueryParamMatchType::RegularExpression => {
                Regex::new(&qpm.value).is_ok_and(|re| re.is_match(value))
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Decision building
// ---------------------------------------------------------------------------

/// Build the final routing decision from a matched rule.
///
/// If the rule contains a `RequestRedirect` filter, return a `Redirect`.
/// Otherwise return `Forward`.
fn build_decision<'a>(rule: &'a RouteRule, request: &RequestMeta<'_>) -> RoutingDecision<'a> {
    // Check for a redirect filter.
    for filter in &rule.filters {
        if let Filter::RequestRedirect {
            scheme,
            hostname,
            port,
            path,
            status_code,
        } = filter
        {
            let location = build_redirect_location(
                scheme.as_deref(),
                hostname.as_deref(),
                port.as_ref().copied(),
                path.as_ref(),
                request,
            );
            return RoutingDecision::Redirect(RedirectResponse {
                status_code: status_code.unwrap_or(302),
                location,
            });
        }
    }

    RoutingDecision::Forward {
        backends: &rule.backends,
        filters: &rule.filters,
        timeout: rule.timeout.as_ref(),
    }
}

/// Construct the redirect Location URL from redirect filter fields + the
/// original request.
fn build_redirect_location(
    scheme: Option<&str>,
    hostname: Option<&str>,
    port: Option<u16>,
    path: Option<&crate::config::PathModifier>,
    request: &RequestMeta<'_>,
) -> String {
    let scheme = scheme.unwrap_or("http");
    let host = hostname.or(request.host).unwrap_or("localhost");
    let path = match path {
        Some(crate::config::PathModifier::ReplaceFullPath { value }) => value.as_str(),
        Some(crate::config::PathModifier::ReplacePrefixMatch { value }) => value.as_str(),
        None => request.path,
    };

    match port {
        Some(p) => format!("{scheme}://{host}:{p}{path}"),
        None => format!("{scheme}://{host}{path}"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Backend, Filter, HeaderMatch, HeaderMatchType, PathMatch, PathMatchType,
        ProxyConfigBuilder, QueryParamMatch, QueryParamMatchType, RouteConfigBuilder, RouteMatch,
        RouteRule, RouteRuleBuilder, TimeoutConfig,
    };

    // Helper to create a basic request.
    fn request<'a>(host: Option<&'a str>, path: &'a str) -> RequestMeta<'a> {
        RequestMeta {
            host,
            path,
            method: "GET",
            headers: &[],
            query_params: &[],
        }
    }

    // Helper to create a basic config with one HTTP listener and one route.
    fn simple_config(
        path_type: PathMatchType,
        path_value: &str,
        hostnames: &[&str],
    ) -> ProxyConfig {
        let mut builder = RouteConfigBuilder::new("default", "route-1").attached_to("http");
        for h in hostnames {
            builder = builder.with_hostname(*h);
        }
        builder = builder.with_rule(
            RouteRuleBuilder::new()
                .with_match(RouteMatch {
                    path: Some(PathMatch {
                        match_type: path_type,
                        value: path_value.to_string(),
                    }),
                    ..Default::default()
                })
                .with_backend(Backend::new("default", "svc", 8080))
                .build(),
        );

        ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(builder.build())
            .build()
    }

    // -- Exact path matching -------------------------------------------------

    #[test]
    fn exact_path_matches_identical() {
        let config = simple_config(PathMatchType::Exact, "/one", &[]);
        let req = request(None, "/one");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn exact_path_does_not_match_trailing_slash() {
        let config = simple_config(PathMatchType::Exact, "/one", &[]);
        let req = request(None, "/one/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    #[test]
    fn exact_path_is_case_sensitive() {
        let config = simple_config(PathMatchType::Exact, "/one", &[]);
        let req = request(None, "/One");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    #[test]
    fn exact_path_does_not_match_subpath() {
        let config = simple_config(PathMatchType::Exact, "/one", &[]);
        let req = request(None, "/one/example");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    // -- PathPrefix matching -------------------------------------------------

    #[test]
    fn path_prefix_matches_exact() {
        let config = simple_config(PathMatchType::PathPrefix, "/api", &[]);
        let req = request(None, "/api");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn path_prefix_matches_subpath() {
        let config = simple_config(PathMatchType::PathPrefix, "/api", &[]);
        let req = request(None, "/api/users");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn path_prefix_segment_boundary_no_partial_match() {
        let config = simple_config(PathMatchType::PathPrefix, "/api", &[]);
        let req = request(None, "/apiversion");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    #[test]
    fn path_prefix_root_matches_everything() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &[]);
        let req = request(None, "/anything/at/all");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    // -- Regex path matching -------------------------------------------------

    #[test]
    fn regex_path_matches() {
        let config = simple_config(PathMatchType::RegularExpression, r"^/api/v\d+/.*", &[]);
        let req = request(None, "/api/v2/users");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn regex_path_does_not_match() {
        let config = simple_config(PathMatchType::RegularExpression, r"^/api/v\d+/.*", &[]);
        let req = request(None, "/api/latest/users");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    // -- Path precedence: exact > prefix; longer prefix > shorter ------------

    #[test]
    fn exact_path_beats_prefix() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "prefix-route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/api")
                            .with_backend(Backend::new("default", "prefix-svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .with_route(
                RouteConfigBuilder::new("default", "exact-route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_exact_path("/api")
                            .with_backend(Backend::new("default", "exact-svc", 9090))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let req = request(None, "/api");
        match route(&config, "http", &req) {
            RoutingDecision::Forward { backends, .. } => {
                assert_eq!(backends[0].name, "exact-svc");
            }
            other => panic!("Expected Forward, got {other:?}"),
        }
    }

    #[test]
    fn longer_prefix_beats_shorter() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "short-route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/api")
                            .with_backend(Backend::new("default", "short-svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .with_route(
                RouteConfigBuilder::new("default", "long-route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/api/users")
                            .with_backend(Backend::new("default", "long-svc", 9090))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let req = request(None, "/api/users/123");
        match route(&config, "http", &req) {
            RoutingDecision::Forward { backends, .. } => {
                assert_eq!(backends[0].name, "long-svc");
            }
            other => panic!("Expected Forward, got {other:?}"),
        }
    }

    // -- Hostname matching ---------------------------------------------------

    #[test]
    fn exact_hostname_matches() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &["example.com"]);
        let req = request(Some("example.com"), "/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn exact_hostname_case_insensitive() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &["Example.COM"]);
        let req = request(Some("example.com"), "/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn exact_hostname_no_match() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &["other.com"]);
        let req = request(Some("example.com"), "/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    #[test]
    fn wildcard_hostname_matches_subdomain() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &["*.example.com"]);
        let req = request(Some("foo.example.com"), "/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn wildcard_hostname_does_not_match_bare_domain() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &["*.example.com"]);
        let req = request(Some("example.com"), "/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    #[test]
    fn wildcard_hostname_does_not_match_nested_subdomain() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &["*.example.com"]);
        let req = request(Some("foo.bar.example.com"), "/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    #[test]
    fn hostname_port_stripping() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &["example.com"]);
        let req = request(Some("example.com:8080"), "/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn empty_hostname_list_matches_all() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &[]);
        let req = request(Some("anything.example.com"), "/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn no_host_header_with_hostnames_is_not_found() {
        let config = simple_config(PathMatchType::PathPrefix, "/", &["example.com"]);
        let req = request(None, "/");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    // -- Header matching -----------------------------------------------------

    #[test]
    fn header_exact_match() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                headers: vec![HeaderMatch {
                                    name: "X-Version".to_string(),
                                    match_type: HeaderMatchType::Exact,
                                    value: "v2".to_string(),
                                }],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let headers = [("X-Version", "v2")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn header_name_case_insensitive() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                headers: vec![HeaderMatch {
                                    name: "X-Version".to_string(),
                                    match_type: HeaderMatchType::Exact,
                                    value: "v2".to_string(),
                                }],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let headers = [("x-version", "v2")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn header_value_is_case_sensitive() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                headers: vec![HeaderMatch {
                                    name: "X-Version".to_string(),
                                    match_type: HeaderMatchType::Exact,
                                    value: "v2".to_string(),
                                }],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let headers = [("X-Version", "V2")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    #[test]
    fn header_and_semantics_all_must_match() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                headers: vec![
                                    HeaderMatch {
                                        name: "X-A".to_string(),
                                        match_type: HeaderMatchType::Exact,
                                        value: "1".to_string(),
                                    },
                                    HeaderMatch {
                                        name: "X-B".to_string(),
                                        match_type: HeaderMatchType::Exact,
                                        value: "2".to_string(),
                                    },
                                ],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        // Only one of two required headers is present.
        let headers = [("X-A", "1")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));

        // Both headers present.
        let headers = [("X-A", "1"), ("X-B", "2")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    // -- Query parameter matching --------------------------------------------

    #[test]
    fn query_param_exact_match() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                query_params: vec![QueryParamMatch {
                                    name: "page".to_string(),
                                    match_type: QueryParamMatchType::Exact,
                                    value: "1".to_string(),
                                }],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let qp = [("page", "1")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &[],
            query_params: &qp,
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    #[test]
    fn query_param_case_sensitive() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                query_params: vec![QueryParamMatch {
                                    name: "Page".to_string(),
                                    match_type: QueryParamMatchType::Exact,
                                    value: "1".to_string(),
                                }],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        // Name is case-sensitive: "page" != "Page".
        let qp = [("page", "1")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &[],
            query_params: &qp,
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    #[test]
    fn query_param_and_semantics() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                query_params: vec![
                                    QueryParamMatch {
                                        name: "a".to_string(),
                                        match_type: QueryParamMatchType::Exact,
                                        value: "1".to_string(),
                                    },
                                    QueryParamMatch {
                                        name: "b".to_string(),
                                        match_type: QueryParamMatchType::Exact,
                                        value: "2".to_string(),
                                    },
                                ],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        // Only one of two required params.
        let qp = [("a", "1")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &[],
            query_params: &qp,
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));

        // Both present.
        let qp = [("a", "1"), ("b", "2")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &[],
            query_params: &qp,
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    // -- Method matching -----------------------------------------------------

    #[test]
    fn method_matching_case_insensitive() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                method: Some("POST".to_string()),
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let req = RequestMeta {
            host: None,
            path: "/",
            method: "post",
            headers: &[],
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));

        // Wrong method.
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &[],
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    // -- Listener attachment -------------------------------------------------

    #[test]
    fn unattached_route_is_not_found() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_http_listener("internal", 8080, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("internal")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let req = request(None, "/anything");
        // Route is attached to "internal", not "http".
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
        // But it matches on "internal".
        assert!(matches!(
            route(&config, "internal", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    // -- Multiple matches in a rule (OR semantics) ---------------------------

    #[test]
    fn multiple_matches_or_semantics() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch {
                                    match_type: PathMatchType::Exact,
                                    value: "/a".to_string(),
                                }),
                                ..Default::default()
                            })
                            .with_match(RouteMatch {
                                path: Some(PathMatch {
                                    match_type: PathMatchType::Exact,
                                    value: "/b".to_string(),
                                }),
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        assert!(matches!(
            route(&config, "http", &request(None, "/a")),
            RoutingDecision::Forward { .. }
        ));
        assert!(matches!(
            route(&config, "http", &request(None, "/b")),
            RoutingDecision::Forward { .. }
        ));
        assert!(matches!(
            route(&config, "http", &request(None, "/c")),
            RoutingDecision::NotFound
        ));
    }

    // -- Multiple conditions in a match (AND semantics) ----------------------

    #[test]
    fn multiple_conditions_and_semantics() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch {
                                    match_type: PathMatchType::PathPrefix,
                                    value: "/api".to_string(),
                                }),
                                method: Some("POST".to_string()),
                                headers: vec![HeaderMatch {
                                    name: "Content-Type".to_string(),
                                    match_type: HeaderMatchType::Exact,
                                    value: "application/json".to_string(),
                                }],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        // All conditions met.
        let headers = [("Content-Type", "application/json")];
        let req = RequestMeta {
            host: None,
            path: "/api/data",
            method: "POST",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));

        // Wrong method.
        let req = RequestMeta {
            host: None,
            path: "/api/data",
            method: "GET",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));

        // Wrong path.
        let req = RequestMeta {
            host: None,
            path: "/other",
            method: "POST",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));

        // Missing header.
        let req = RequestMeta {
            host: None,
            path: "/api/data",
            method: "POST",
            headers: &[],
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    // -- No routes → NotFound ------------------------------------------------

    #[test]
    fn no_routes_returns_not_found() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .build();

        let req = request(None, "/anything");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    // -- Empty matches → matches all -----------------------------------------

    #[test]
    fn empty_matches_matches_all() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(RouteRule {
                        name: None,
                        matches: Vec::new(),
                        filters: Vec::new(),
                        backends: vec![Backend::new("default", "svc", 8080)],
                        timeout: None,
                    })
                    .build(),
            )
            .build();

        let req = request(Some("any.host"), "/any/path");
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));
    }

    // -- Redirect filter -----------------------------------------------------

    #[test]
    fn redirect_filter_returns_redirect() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/old")
                            .with_filter(Filter::RequestRedirect {
                                scheme: Some("https".to_string()),
                                hostname: Some("new.example.com".to_string()),
                                port: Some(443),
                                path: None,
                                status_code: Some(301),
                            })
                            .build(),
                    )
                    .build(),
            )
            .build();

        let req = request(Some("old.example.com"), "/old/page");
        match route(&config, "http", &req) {
            RoutingDecision::Redirect(redir) => {
                assert_eq!(redir.status_code, 301);
                assert_eq!(redir.location, "https://new.example.com:443/old/page");
            }
            other => panic!("Expected Redirect, got {other:?}"),
        }
    }

    // -- Forward returns correct data ----------------------------------------

    #[test]
    fn forward_returns_backends_filters_timeout() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc-a", 8080))
                            .with_backend(Backend::new("default", "svc-b", 9090).with_weight(3))
                            .with_filter(Filter::request_header_add(vec![
                                crate::config::HeaderValue {
                                    name: "X-Proxy".to_string(),
                                    value: "multiway".to_string(),
                                },
                            ]))
                            .with_timeout(TimeoutConfig {
                                request: Some(30.0),
                                backend_request: Some(10.0),
                            })
                            .build(),
                    )
                    .build(),
            )
            .build();

        let req = request(None, "/anything");
        match route(&config, "http", &req) {
            RoutingDecision::Forward {
                backends,
                filters,
                timeout,
            } => {
                assert_eq!(backends.len(), 2);
                assert_eq!(backends[0].name, "svc-a");
                assert_eq!(backends[1].name, "svc-b");
                assert_eq!(backends[1].weight, 3);
                assert_eq!(filters.len(), 1);
                let t = timeout.unwrap();
                assert_eq!(t.request, Some(30.0));
                assert_eq!(t.backend_request, Some(10.0));
            }
            other => panic!("Expected Forward, got {other:?}"),
        }
    }

    // -- Header regex matching -----------------------------------------------

    #[test]
    fn header_regex_match() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                headers: vec![HeaderMatch {
                                    name: "Authorization".to_string(),
                                    match_type: HeaderMatchType::RegularExpression,
                                    value: r"^Bearer .+".to_string(),
                                }],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let headers = [("Authorization", "Bearer abc123")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));

        let headers = [("Authorization", "Basic abc123")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &headers,
            query_params: &[],
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    // -- Query param regex matching ------------------------------------------

    #[test]
    fn query_param_regex_match() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_match(RouteMatch {
                                path: Some(PathMatch::default()),
                                query_params: vec![QueryParamMatch {
                                    name: "id".to_string(),
                                    match_type: QueryParamMatchType::RegularExpression,
                                    value: r"^\d+$".to_string(),
                                }],
                                ..Default::default()
                            })
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let qp = [("id", "42")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &[],
            query_params: &qp,
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::Forward { .. }
        ));

        let qp = [("id", "abc")];
        let req = RequestMeta {
            host: None,
            path: "/",
            method: "GET",
            headers: &[],
            query_params: &qp,
        };
        assert!(matches!(
            route(&config, "http", &req),
            RoutingDecision::NotFound
        ));
    }

    // -- prefix_path_matches unit tests --------------------------------------

    #[test]
    fn prefix_path_unit_tests() {
        // Exact match
        assert!(prefix_path_matches("/api", "/api"));
        // Subpath
        assert!(prefix_path_matches("/api", "/api/users"));
        // No partial segment match
        assert!(!prefix_path_matches("/api", "/apiversion"));
        // Root matches everything
        assert!(prefix_path_matches("/", "/"));
        assert!(prefix_path_matches("/", "/anything"));
        // Deeper paths
        assert!(prefix_path_matches("/a/b", "/a/b/c"));
        assert!(!prefix_path_matches("/a/b", "/a/bc"));
    }

    // -- strip_port unit tests -----------------------------------------------

    #[test]
    fn strip_port_unit_tests() {
        assert_eq!(strip_port("example.com:8080"), "example.com");
        assert_eq!(strip_port("example.com"), "example.com");
        assert_eq!(strip_port("[::1]:8080"), "[::1]:8080"); // IPv6 not stripped
    }
}
