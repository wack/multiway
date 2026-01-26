//! HTTPRoute reconciler
//!
//! This module implements the reconciliation logic for HTTPRoute resources.
//! According to the Gateway API specification:
//!
//! - HTTPRoute specifies routing rules for HTTP traffic
//! - Routes attach to Gateways via parentRefs
//! - Multiple routes can attach to the same Gateway
//! - Route status reflects which Gateways have accepted the route
//!
//! For this implementation:
//! - We validate parent references to Gateways
//! - We resolve backend services
//! - We update the ConfigMap with combined routing configuration
//! - We update HTTPRoute status with parent statuses

use std::sync::Arc;

use futures::StreamExt;
use gateway_crds::{
    Gateway, HTTPRoute, HttpRouteRules, HttpRouteRulesBackendRefs, HttpRouteRulesFilters,
    HttpRouteRulesFiltersType, HttpRouteRulesMatches, HttpRouteRulesMatchesHeaders,
    HttpRouteRulesMatchesHeadersType, HttpRouteRulesMatchesMethod, HttpRouteRulesMatchesPath,
    HttpRouteRulesMatchesPathType, HttpRouteRulesMatchesQueryParams,
    HttpRouteRulesMatchesQueryParamsType,
};
use k8s_openapi::api::core::v1::Service;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Api, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, error, info, instrument};

use super::config::{
    BackendRef, HeaderMatch, HeaderMatchType, HeaderValue, PathMatch, PathMatchType, PathModifier,
    QueryParamMatch, QueryParamMatchType, RouteConfig, RouteFilter, RouteMatch, RouteRule,
    TimeoutConfig,
};
use super::context::ControllerContext;
use super::error::{ControllerError, Result};
use crate::core;
use crate::shell::{ReconcileExecutor, SnapshotFetcher};

/// Run the HTTPRoute controller
pub async fn run_httproute_controller(ctx: Arc<ControllerContext>) {
    info!("Starting HTTPRoute controller");

    let client = ctx.client.clone();
    let httproutes: Api<HTTPRoute> = match &ctx.config.watch_namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };

    // Watch for related resources
    let gateways: Api<Gateway> = Api::all(client.clone());
    let _services: Api<Service> = Api::all(client.clone());

    let controller = Controller::new(httproutes.clone(), WatcherConfig::default())
        .shutdown_on_signal()
        // Watch Gateways that routes may reference
        .watches(gateways, WatcherConfig::default(), |gateway| {
            // When a Gateway changes, reconcile all HTTPRoutes in its namespace
            // This is a simplified approach - in production, you'd want more precise filtering
            vec![
                kube::runtime::reflector::ObjectRef::new(&gateway.name_any())
                    .within(&gateway.namespace().unwrap_or_default()),
            ]
        })
        .run(
            |obj, ctx| async move { reconcile_httproute(obj, ctx).await },
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
                        "HTTPRoute reconciled successfully"
                    );
                }
                Err(error) => {
                    error!(%error, "HTTPRoute reconciliation error");
                }
            }
        });

    controller.await;
}

/// Reconcile a single HTTPRoute resource using the functional core
#[instrument(skip_all, fields(
    namespace = %httproute.namespace().unwrap_or_default(),
    name = %httproute.name_any()
))]
async fn reconcile_httproute(
    httproute: Arc<HTTPRoute>,
    ctx: Arc<ControllerContext>,
) -> Result<Action> {
    let name = httproute.name_any();
    let namespace = httproute.namespace().unwrap_or_default();
    info!("Reconciling HTTPRoute");

    // Build snapshot from cluster state
    let fetcher = SnapshotFetcher::new(ctx.client.clone());
    let snapshot = fetcher.snapshot_for_httproute(&namespace, &name).await?;

    // Pure reconciliation - compute what needs to be done
    let result = core::reconcile_httproute(&snapshot, &ctx.config, &namespace, &name);

    // Execute the computed effects
    let executor = ReconcileExecutor::new(ctx.client.clone());
    executor.execute(result).await
}

/// Convert an HTTPRoute to our internal RouteConfig format
pub fn convert_httproute_to_config(
    httproute: &HTTPRoute,
    attached_listeners: &[&str],
) -> Result<RouteConfig> {
    let route_namespace = httproute.namespace().unwrap_or_default();

    let mut route_config = RouteConfig::new(&route_namespace, httproute.name_any());
    route_config.hostnames = httproute.spec.hostnames.clone().unwrap_or_default();
    route_config.attached_listeners = attached_listeners.iter().map(|s| s.to_string()).collect();

    let rules = httproute.spec.rules.as_ref().cloned().unwrap_or_default();
    for rule in rules {
        let route_rule = convert_rule(&rule, &route_namespace)?;
        route_config.rules.push(route_rule);
    }

    Ok(route_config)
}

/// Convert an HTTPRoute rule to our internal format
fn convert_rule(rule: &HttpRouteRules, route_namespace: &str) -> Result<RouteRule> {
    // Note: rule.name field not available in Gateway API v1.2.1
    let mut route_rule = RouteRule {
        name: None,
        matches: Vec::new(),
        filters: Vec::new(),
        backends: Vec::new(),
        timeout: None,
    };

    // Convert matches
    if let Some(matches) = &rule.matches {
        for m in matches {
            route_rule.matches.push(convert_match(m)?);
        }
    }

    // If no matches, default to matching all
    if route_rule.matches.is_empty() {
        route_rule.matches.push(RouteMatch::default());
    }

    // Convert filters
    if let Some(filters) = &rule.filters {
        for f in filters {
            if let Some(filter) = convert_filter(f)? {
                route_rule.filters.push(filter);
            }
        }
    }

    // Convert backend refs
    if let Some(backends) = &rule.backend_refs {
        for b in backends {
            route_rule
                .backends
                .push(convert_backend(b, route_namespace)?);
        }
    }

    // Convert timeouts
    if let Some(timeouts) = &rule.timeouts {
        route_rule.timeout = Some(TimeoutConfig {
            request: timeouts.request.as_ref().and_then(|t| parse_duration(t)),
            backend_request: timeouts
                .backend_request
                .as_ref()
                .and_then(|t| parse_duration(t)),
        });
    }

    Ok(route_rule)
}

/// Convert an HTTPRoute match to our internal format
fn convert_match(m: &HttpRouteRulesMatches) -> Result<RouteMatch> {
    let mut route_match = RouteMatch::default();

    // Convert path match
    if let Some(path) = &m.path {
        route_match.path = Some(convert_path_match(path)?);
    }

    // Convert header matches
    if let Some(headers) = &m.headers {
        for h in headers {
            route_match.headers.push(convert_header_match(h)?);
        }
    }

    // Convert query param matches
    if let Some(params) = &m.query_params {
        for p in params {
            route_match.query_params.push(convert_query_param_match(p)?);
        }
    }

    // Convert method
    if let Some(method) = &m.method {
        route_match.method = Some(convert_method(method));
    }

    Ok(route_match)
}

/// Convert a path match
fn convert_path_match(path: &HttpRouteRulesMatchesPath) -> Result<PathMatch> {
    let match_type = match &path.r#type {
        Some(HttpRouteRulesMatchesPathType::Exact) => PathMatchType::Exact,
        Some(HttpRouteRulesMatchesPathType::PathPrefix) | None => PathMatchType::PathPrefix,
        Some(HttpRouteRulesMatchesPathType::RegularExpression) => PathMatchType::RegularExpression,
    };

    Ok(PathMatch {
        match_type,
        value: path.value.clone().unwrap_or_else(|| "/".to_string()),
    })
}

/// Convert a header match
fn convert_header_match(header: &HttpRouteRulesMatchesHeaders) -> Result<HeaderMatch> {
    let match_type = match &header.r#type {
        Some(HttpRouteRulesMatchesHeadersType::Exact) | None => HeaderMatchType::Exact,
        Some(HttpRouteRulesMatchesHeadersType::RegularExpression) => {
            HeaderMatchType::RegularExpression
        }
    };

    Ok(HeaderMatch {
        name: header.name.clone(),
        match_type,
        value: header.value.clone(),
    })
}

/// Convert a query param match
fn convert_query_param_match(param: &HttpRouteRulesMatchesQueryParams) -> Result<QueryParamMatch> {
    let match_type = match &param.r#type {
        Some(HttpRouteRulesMatchesQueryParamsType::Exact) | None => QueryParamMatchType::Exact,
        Some(HttpRouteRulesMatchesQueryParamsType::RegularExpression) => {
            QueryParamMatchType::RegularExpression
        }
    };

    Ok(QueryParamMatch {
        name: param.name.clone(),
        match_type,
        value: param.value.clone(),
    })
}

/// Convert a method enum to string
fn convert_method(method: &HttpRouteRulesMatchesMethod) -> String {
    match method {
        HttpRouteRulesMatchesMethod::Get => "GET",
        HttpRouteRulesMatchesMethod::Head => "HEAD",
        HttpRouteRulesMatchesMethod::Post => "POST",
        HttpRouteRulesMatchesMethod::Put => "PUT",
        HttpRouteRulesMatchesMethod::Delete => "DELETE",
        HttpRouteRulesMatchesMethod::Connect => "CONNECT",
        HttpRouteRulesMatchesMethod::Options => "OPTIONS",
        HttpRouteRulesMatchesMethod::Trace => "TRACE",
        HttpRouteRulesMatchesMethod::Patch => "PATCH",
    }
    .to_string()
}

/// Convert a filter
fn convert_filter(filter: &HttpRouteRulesFilters) -> Result<Option<RouteFilter>> {
    match &filter.r#type {
        HttpRouteRulesFiltersType::RequestHeaderModifier => {
            let modifier = filter.request_header_modifier.as_ref();
            Ok(Some(RouteFilter::RequestHeaderModifier {
                add: modifier
                    .and_then(|m| m.add.as_ref())
                    .map(|headers| {
                        headers
                            .iter()
                            .map(|h| HeaderValue {
                                name: h.name.clone(),
                                value: h.value.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                set: modifier
                    .and_then(|m| m.set.as_ref())
                    .map(|headers| {
                        headers
                            .iter()
                            .map(|h| HeaderValue {
                                name: h.name.clone(),
                                value: h.value.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                remove: modifier.and_then(|m| m.remove.clone()).unwrap_or_default(),
            }))
        }
        HttpRouteRulesFiltersType::ResponseHeaderModifier => {
            let modifier = filter.response_header_modifier.as_ref();
            Ok(Some(RouteFilter::ResponseHeaderModifier {
                add: modifier
                    .and_then(|m| m.add.as_ref())
                    .map(|headers| {
                        headers
                            .iter()
                            .map(|h| HeaderValue {
                                name: h.name.clone(),
                                value: h.value.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                set: modifier
                    .and_then(|m| m.set.as_ref())
                    .map(|headers| {
                        headers
                            .iter()
                            .map(|h| HeaderValue {
                                name: h.name.clone(),
                                value: h.value.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                remove: modifier.and_then(|m| m.remove.clone()).unwrap_or_default(),
            }))
        }
        HttpRouteRulesFiltersType::RequestRedirect => {
            if let Some(redirect) = &filter.request_redirect {
                Ok(Some(RouteFilter::RequestRedirect {
                    scheme: redirect.scheme.as_ref().map(|s| match s {
                        gateway_crds::HttpRouteRulesFiltersRequestRedirectScheme::Http => "http",
                        gateway_crds::HttpRouteRulesFiltersRequestRedirectScheme::Https => "https",
                    }.to_string()),
                    hostname: redirect.hostname.clone(),
                    port: redirect.port.map(|p| p as u16),
                    path: redirect.path.as_ref().map(|p| {
                        match &p.r#type {
                            gateway_crds::HttpRouteRulesFiltersRequestRedirectPathType::ReplaceFullPath => {
                                PathModifier::ReplaceFullPath {
                                    value: p.replace_full_path.clone().unwrap_or_default(),
                                }
                            }
                            gateway_crds::HttpRouteRulesFiltersRequestRedirectPathType::ReplacePrefixMatch => {
                                PathModifier::ReplacePrefixMatch {
                                    value: p.replace_prefix_match.clone().unwrap_or_default(),
                                }
                            }
                        }
                    }),
                    status_code: redirect.status_code.map(|c| c as u16),
                }))
            } else {
                Ok(None)
            }
        }
        HttpRouteRulesFiltersType::UrlRewrite => {
            if let Some(rewrite) = &filter.url_rewrite {
                Ok(Some(RouteFilter::URLRewrite {
                    hostname: rewrite.hostname.clone(),
                    path: rewrite.path.as_ref().map(|p| {
                        match &p.r#type {
                            gateway_crds::HttpRouteRulesFiltersUrlRewritePathType::ReplaceFullPath => {
                                PathModifier::ReplaceFullPath {
                                    value: p.replace_full_path.clone().unwrap_or_default(),
                                }
                            }
                            gateway_crds::HttpRouteRulesFiltersUrlRewritePathType::ReplacePrefixMatch => {
                                PathModifier::ReplacePrefixMatch {
                                    value: p.replace_prefix_match.clone().unwrap_or_default(),
                                }
                            }
                        }
                    }),
                }))
            } else {
                Ok(None)
            }
        }
        HttpRouteRulesFiltersType::RequestMirror => {
            if let Some(mirror) = &filter.request_mirror {
                let backend_ref = &mirror.backend_ref;
                let namespace = backend_ref.namespace.as_deref().unwrap_or("");
                Ok(Some(RouteFilter::RequestMirror {
                    backend: BackendRef::new(
                        namespace,
                        &backend_ref.name,
                        backend_ref.port.unwrap_or(80) as u16,
                    ),
                    // Note: percent field not available in Gateway API v1.2.1
                    percent: None,
                }))
            } else {
                Ok(None)
            }
        }
        HttpRouteRulesFiltersType::ExtensionRef => {
            // Extension refs are not supported
            debug!("ExtensionRef filter not supported");
            Ok(None)
        }
    }
}

/// Convert a backend reference
fn convert_backend(
    backend: &HttpRouteRulesBackendRefs,
    route_namespace: &str,
) -> Result<BackendRef> {
    let namespace = backend.namespace.as_deref().unwrap_or(route_namespace);
    let port = backend.port.unwrap_or(80) as u16;
    let weight = backend.weight.unwrap_or(1) as u32;

    Ok(BackendRef::new(namespace, &backend.name, port).with_weight(weight))
}

/// Parse a duration string (e.g., "10s", "1m")
fn parse_duration(duration: &str) -> Option<f64> {
    if let Some(s) = duration.strip_suffix('s') {
        s.parse().ok()
    } else if let Some(m) = duration.strip_suffix('m') {
        m.parse::<f64>().ok().map(|m| m * 60.0)
    } else if let Some(h) = duration.strip_suffix('h') {
        h.parse::<f64>().ok().map(|h| h * 3600.0)
    } else {
        duration.parse().ok()
    }
}

/// Error policy for HTTPRoute reconciliation
fn error_policy(
    _obj: Arc<HTTPRoute>,
    error: &ControllerError,
    ctx: Arc<ControllerContext>,
) -> Action {
    error!(%error, "HTTPRoute reconciliation failed");
    Action::requeue(Duration::from_secs(ctx.config.error_requeue_secs))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::core::validate::{AllowedNamespaces, find_matching_listeners, is_namespace_allowed};
    use gateway_crds::{
        HttpRouteRulesFiltersRequestHeaderModifier, HttpRouteRulesFiltersRequestRedirect,
        HttpRouteRulesFiltersRequestRedirectPath, HttpRouteRulesFiltersRequestRedirectPathType,
        HttpRouteRulesFiltersRequestRedirectScheme, HttpRouteRulesFiltersUrlRewrite,
        HttpRouteRulesFiltersUrlRewritePath, HttpRouteRulesFiltersUrlRewritePathType,
        HttpRouteRulesMatchesHeaders, HttpRouteRulesMatchesPath, HttpRouteRulesMatchesQueryParams,
        HttpRouteRulesTimeouts,
    };

    // ==========================================
    // Duration Parsing Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: Durations can be specified with suffixes (s, m, h)
    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("10s"), Some(10.0));
        assert_eq!(parse_duration("1m"), Some(60.0));
        assert_eq!(parse_duration("1h"), Some(3600.0));
        assert_eq!(parse_duration("30"), Some(30.0));
        assert_eq!(parse_duration("invalid"), None);
    }

    /// Spec: Fractional durations should be supported
    #[test]
    fn test_parse_duration_fractional() {
        assert_eq!(parse_duration("0.5s"), Some(0.5));
        assert_eq!(parse_duration("1.5m"), Some(90.0));
        assert_eq!(parse_duration("2.5h"), Some(9000.0));
    }

    /// Spec: Zero duration should be valid
    #[test]
    fn test_parse_duration_zero() {
        assert_eq!(parse_duration("0s"), Some(0.0));
        assert_eq!(parse_duration("0m"), Some(0.0));
    }

    // ==========================================
    // HTTP Method Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: All HTTP methods should be converted correctly
    #[test]
    fn test_convert_method() {
        assert_eq!(convert_method(&HttpRouteRulesMatchesMethod::Get), "GET");
        assert_eq!(convert_method(&HttpRouteRulesMatchesMethod::Post), "POST");
        assert_eq!(convert_method(&HttpRouteRulesMatchesMethod::Put), "PUT");
        assert_eq!(
            convert_method(&HttpRouteRulesMatchesMethod::Delete),
            "DELETE"
        );
        assert_eq!(convert_method(&HttpRouteRulesMatchesMethod::Patch), "PATCH");
        assert_eq!(convert_method(&HttpRouteRulesMatchesMethod::Head), "HEAD");
        assert_eq!(
            convert_method(&HttpRouteRulesMatchesMethod::Options),
            "OPTIONS"
        );
        assert_eq!(
            convert_method(&HttpRouteRulesMatchesMethod::Connect),
            "CONNECT"
        );
        assert_eq!(convert_method(&HttpRouteRulesMatchesMethod::Trace), "TRACE");
    }

    // ==========================================
    // Namespace Allowed Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: AllowedNamespaces controls cross-namespace route attachment
    #[test]
    fn test_is_namespace_allowed() {
        // All - allows any namespace
        assert!(is_namespace_allowed(&AllowedNamespaces::All, "any-ns"));
        assert!(is_namespace_allowed(&AllowedNamespaces::All, "default"));
        assert!(is_namespace_allowed(&AllowedNamespaces::All, "production"));

        // Same - only allows same namespace
        assert!(is_namespace_allowed(
            &AllowedNamespaces::Same("default".to_string()),
            "default"
        ));
        assert!(!is_namespace_allowed(
            &AllowedNamespaces::Same("default".to_string()),
            "other"
        ));

        // Selector - allows namespaces with matching labels
        let mut labels = BTreeMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        assert!(is_namespace_allowed(
            &AllowedNamespaces::Selector {
                match_labels: labels
            },
            "some-namespace" // selector is permissive if labels exist
        ));
    }

    // ==========================================
    // Path Match Conversion Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: PathPrefix is the default path match type
    #[test]
    fn test_convert_path_match_prefix_default() {
        let path = HttpRouteRulesMatchesPath {
            r#type: None, // No type means PathPrefix
            value: Some("/api".to_string()),
        };

        let result = convert_path_match(&path).unwrap();
        assert_eq!(result.match_type, PathMatchType::PathPrefix);
        assert_eq!(result.value, "/api");
    }

    /// Spec: Exact path match requires exact string equality
    #[test]
    fn test_convert_path_match_exact() {
        let path = HttpRouteRulesMatchesPath {
            r#type: Some(HttpRouteRulesMatchesPathType::Exact),
            value: Some("/api/v1/users".to_string()),
        };

        let result = convert_path_match(&path).unwrap();
        assert_eq!(result.match_type, PathMatchType::Exact);
        assert_eq!(result.value, "/api/v1/users");
    }

    /// Spec: PathPrefix match type
    #[test]
    fn test_convert_path_match_path_prefix() {
        let path = HttpRouteRulesMatchesPath {
            r#type: Some(HttpRouteRulesMatchesPathType::PathPrefix),
            value: Some("/api".to_string()),
        };

        let result = convert_path_match(&path).unwrap();
        assert_eq!(result.match_type, PathMatchType::PathPrefix);
        assert_eq!(result.value, "/api");
    }

    /// Spec: RegularExpression path match type
    #[test]
    fn test_convert_path_match_regex() {
        let path = HttpRouteRulesMatchesPath {
            r#type: Some(HttpRouteRulesMatchesPathType::RegularExpression),
            value: Some("^/api/v[0-9]+/.*".to_string()),
        };

        let result = convert_path_match(&path).unwrap();
        assert_eq!(result.match_type, PathMatchType::RegularExpression);
        assert_eq!(result.value, "^/api/v[0-9]+/.*");
    }

    /// Spec: Default path value is "/" when not specified
    #[test]
    fn test_convert_path_match_default_value() {
        let path = HttpRouteRulesMatchesPath {
            r#type: Some(HttpRouteRulesMatchesPathType::PathPrefix),
            value: None,
        };

        let result = convert_path_match(&path).unwrap();
        assert_eq!(result.value, "/");
    }

    // ==========================================
    // Header Match Conversion Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: Exact header match is the default
    #[test]
    fn test_convert_header_match_exact_default() {
        let header = HttpRouteRulesMatchesHeaders {
            r#type: None, // Default is Exact
            name: "X-Custom-Header".to_string(),
            value: "expected-value".to_string(),
        };

        let result = convert_header_match(&header).unwrap();
        assert_eq!(result.match_type, HeaderMatchType::Exact);
        assert_eq!(result.name, "X-Custom-Header");
        assert_eq!(result.value, "expected-value");
    }

    /// Spec: RegularExpression header match
    #[test]
    fn test_convert_header_match_regex() {
        let header = HttpRouteRulesMatchesHeaders {
            r#type: Some(HttpRouteRulesMatchesHeadersType::RegularExpression),
            name: "X-Request-ID".to_string(),
            value: "^[a-f0-9]{8}-.*".to_string(),
        };

        let result = convert_header_match(&header).unwrap();
        assert_eq!(result.match_type, HeaderMatchType::RegularExpression);
        assert_eq!(result.name, "X-Request-ID");
    }

    // ==========================================
    // Query Param Match Conversion Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: Exact query param match is the default
    #[test]
    fn test_convert_query_param_match_exact_default() {
        let param = HttpRouteRulesMatchesQueryParams {
            r#type: None, // Default is Exact
            name: "version".to_string(),
            value: "v2".to_string(),
        };

        let result = convert_query_param_match(&param).unwrap();
        assert_eq!(result.match_type, QueryParamMatchType::Exact);
        assert_eq!(result.name, "version");
        assert_eq!(result.value, "v2");
    }

    /// Spec: RegularExpression query param match
    #[test]
    fn test_convert_query_param_match_regex() {
        let param = HttpRouteRulesMatchesQueryParams {
            r#type: Some(HttpRouteRulesMatchesQueryParamsType::RegularExpression),
            name: "id".to_string(),
            value: "^[0-9]+$".to_string(),
        };

        let result = convert_query_param_match(&param).unwrap();
        assert_eq!(result.match_type, QueryParamMatchType::RegularExpression);
        assert_eq!(result.name, "id");
    }

    // ==========================================
    // Filter Conversion Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: RequestHeaderModifier filter adds/sets/removes headers
    #[test]
    fn test_convert_filter_request_header_modifier() {
        let filter = HttpRouteRulesFilters {
            r#type: HttpRouteRulesFiltersType::RequestHeaderModifier,
            request_header_modifier: Some(HttpRouteRulesFiltersRequestHeaderModifier {
                add: Some(vec![
                    gateway_crds::HttpRouteRulesFiltersRequestHeaderModifierAdd {
                        name: "X-Added".to_string(),
                        value: "added-value".to_string(),
                    },
                ]),
                set: Some(vec![
                    gateway_crds::HttpRouteRulesFiltersRequestHeaderModifierSet {
                        name: "X-Set".to_string(),
                        value: "set-value".to_string(),
                    },
                ]),
                remove: Some(vec!["X-Remove".to_string()]),
            }),
            response_header_modifier: None,
            request_redirect: None,
            url_rewrite: None,
            request_mirror: None,
            extension_ref: None,
        };

        let result = convert_filter(&filter).unwrap().unwrap();
        match result {
            RouteFilter::RequestHeaderModifier { add, set, remove } => {
                assert_eq!(add.len(), 1);
                assert_eq!(add[0].name, "X-Added");
                assert_eq!(add[0].value, "added-value");
                assert_eq!(set.len(), 1);
                assert_eq!(set[0].name, "X-Set");
                assert_eq!(remove.len(), 1);
                assert_eq!(remove[0], "X-Remove");
            }
            _ => panic!("Expected RequestHeaderModifier filter"),
        }
    }

    /// Spec: RequestRedirect filter redirects requests
    #[test]
    fn test_convert_filter_request_redirect() {
        let filter = HttpRouteRulesFilters {
            r#type: HttpRouteRulesFiltersType::RequestRedirect,
            request_header_modifier: None,
            response_header_modifier: None,
            request_redirect: Some(HttpRouteRulesFiltersRequestRedirect {
                scheme: Some(HttpRouteRulesFiltersRequestRedirectScheme::Https),
                hostname: Some("new-host.example.com".to_string()),
                port: Some(8443),
                path: Some(HttpRouteRulesFiltersRequestRedirectPath {
                    r#type: HttpRouteRulesFiltersRequestRedirectPathType::ReplaceFullPath,
                    replace_full_path: Some("/new-path".to_string()),
                    replace_prefix_match: None,
                }),
                status_code: Some(301),
            }),
            url_rewrite: None,
            request_mirror: None,
            extension_ref: None,
        };

        let result = convert_filter(&filter).unwrap().unwrap();
        match result {
            RouteFilter::RequestRedirect {
                scheme,
                hostname,
                port,
                path,
                status_code,
            } => {
                assert_eq!(scheme, Some("https".to_string()));
                assert_eq!(hostname, Some("new-host.example.com".to_string()));
                assert_eq!(port, Some(8443));
                assert!(
                    matches!(path, Some(PathModifier::ReplaceFullPath { value }) if value == "/new-path")
                );
                assert_eq!(status_code, Some(301));
            }
            _ => panic!("Expected RequestRedirect filter"),
        }
    }

    /// Spec: URLRewrite filter rewrites URL path and hostname
    #[test]
    fn test_convert_filter_url_rewrite() {
        let filter = HttpRouteRulesFilters {
            r#type: HttpRouteRulesFiltersType::UrlRewrite,
            request_header_modifier: None,
            response_header_modifier: None,
            request_redirect: None,
            url_rewrite: Some(HttpRouteRulesFiltersUrlRewrite {
                hostname: Some("backend.internal".to_string()),
                path: Some(HttpRouteRulesFiltersUrlRewritePath {
                    r#type: HttpRouteRulesFiltersUrlRewritePathType::ReplacePrefixMatch,
                    replace_full_path: None,
                    replace_prefix_match: Some("/internal-api".to_string()),
                }),
            }),
            request_mirror: None,
            extension_ref: None,
        };

        let result = convert_filter(&filter).unwrap().unwrap();
        match result {
            RouteFilter::URLRewrite { hostname, path } => {
                assert_eq!(hostname, Some("backend.internal".to_string()));
                assert!(
                    matches!(path, Some(PathModifier::ReplacePrefixMatch { value }) if value == "/internal-api")
                );
            }
            _ => panic!("Expected URLRewrite filter"),
        }
    }

    /// Spec: ExtensionRef filter is not supported and returns None
    #[test]
    fn test_convert_filter_extension_ref_unsupported() {
        let filter = HttpRouteRulesFilters {
            r#type: HttpRouteRulesFiltersType::ExtensionRef,
            request_header_modifier: None,
            response_header_modifier: None,
            request_redirect: None,
            url_rewrite: None,
            request_mirror: None,
            extension_ref: Some(gateway_crds::HttpRouteRulesFiltersExtensionRef {
                group: "custom.example.com".to_string(),
                kind: "CustomFilter".to_string(),
                name: "my-filter".to_string(),
            }),
        };

        let result = convert_filter(&filter).unwrap();
        assert!(result.is_none());
    }

    // ==========================================
    // Backend Reference Conversion Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: Backend defaults to same namespace and weight=1
    #[test]
    fn test_convert_backend_defaults() {
        let backend = HttpRouteRulesBackendRefs {
            group: None,
            kind: None,
            name: "my-service".to_string(),
            namespace: None,
            port: None,
            weight: None,
            filters: None,
        };

        let result = convert_backend(&backend, "default").unwrap();
        assert_eq!(result.namespace, "default");
        assert_eq!(result.name, "my-service");
        assert_eq!(result.port, 80); // Default port
        assert_eq!(result.weight, 1); // Default weight
    }

    /// Spec: Backend with explicit namespace and weight
    #[test]
    fn test_convert_backend_explicit() {
        let backend = HttpRouteRulesBackendRefs {
            group: None,
            kind: None,
            name: "backend-service".to_string(),
            namespace: Some("other-namespace".to_string()),
            port: Some(8080),
            weight: Some(10),
            filters: None,
        };

        let result = convert_backend(&backend, "default").unwrap();
        assert_eq!(result.namespace, "other-namespace");
        assert_eq!(result.name, "backend-service");
        assert_eq!(result.port, 8080);
        assert_eq!(result.weight, 10);
    }

    // ==========================================
    // Rule Conversion Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: Rule with no matches should match all requests
    #[test]
    fn test_convert_rule_no_matches() {
        // Note: name field not available in Gateway API v1.2.1
        let rule = HttpRouteRules {
            matches: None,
            filters: None,
            backend_refs: Some(vec![HttpRouteRulesBackendRefs {
                group: None,
                kind: None,
                name: "default-backend".to_string(),
                namespace: None,
                port: Some(80),
                weight: None,
                filters: None,
            }]),
            timeouts: None,
        };

        let result = convert_rule(&rule, "default").unwrap();
        // Note: name field not available in Gateway API v1.2.1
        assert_eq!(result.name, None);
        // Should have a default match
        assert_eq!(result.matches.len(), 1);
        assert!(result.matches[0].path.is_none());
    }

    /// Spec: Rule with timeouts
    #[test]
    fn test_convert_rule_with_timeouts() {
        // Note: name field not available in Gateway API v1.2.1
        let rule = HttpRouteRules {
            matches: None,
            filters: None,
            backend_refs: Some(vec![HttpRouteRulesBackendRefs {
                group: None,
                kind: None,
                name: "backend".to_string(),
                namespace: None,
                port: Some(80),
                weight: None,
                filters: None,
            }]),
            timeouts: Some(HttpRouteRulesTimeouts {
                request: Some("30s".to_string()),
                backend_request: Some("10s".to_string()),
            }),
        };

        let result = convert_rule(&rule, "default").unwrap();
        let timeout = result.timeout.unwrap();
        assert_eq!(timeout.request, Some(30.0));
        assert_eq!(timeout.backend_request, Some(10.0));
    }

    // ==========================================
    // Match Conversion Tests (HTTPRoute Spec)
    // ==========================================

    /// Spec: Match with path, headers, and query params (AND semantics within match)
    #[test]
    fn test_convert_match_complex() {
        let m = HttpRouteRulesMatches {
            path: Some(HttpRouteRulesMatchesPath {
                r#type: Some(HttpRouteRulesMatchesPathType::PathPrefix),
                value: Some("/api/v1".to_string()),
            }),
            headers: Some(vec![HttpRouteRulesMatchesHeaders {
                r#type: Some(HttpRouteRulesMatchesHeadersType::Exact),
                name: "X-API-Key".to_string(),
                value: "secret".to_string(),
            }]),
            query_params: Some(vec![HttpRouteRulesMatchesQueryParams {
                r#type: Some(HttpRouteRulesMatchesQueryParamsType::Exact),
                name: "format".to_string(),
                value: "json".to_string(),
            }]),
            method: Some(HttpRouteRulesMatchesMethod::Post),
        };

        let result = convert_match(&m).unwrap();

        // Path
        let path = result.path.unwrap();
        assert_eq!(path.match_type, PathMatchType::PathPrefix);
        assert_eq!(path.value, "/api/v1");

        // Headers
        assert_eq!(result.headers.len(), 1);
        assert_eq!(result.headers[0].name, "X-API-Key");

        // Query params
        assert_eq!(result.query_params.len(), 1);
        assert_eq!(result.query_params[0].name, "format");

        // Method
        assert_eq!(result.method, Some("POST".to_string()));
    }

    // ==========================================
    // Gateway Listener Matching Tests
    // ==========================================

    /// Spec: find_matching_listeners by section name
    #[test]
    fn test_find_matching_listeners_by_section_name() {
        let gateway = Gateway {
            metadata: kube::core::ObjectMeta {
                name: Some("test-gateway".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: gateway_crds::GatewaySpec {
                gateway_class_name: "multiway".to_string(),
                listeners: vec![
                    gateway_crds::GatewayListeners {
                        name: "http".to_string(),
                        port: 80,
                        protocol: "HTTP".to_string(),
                        hostname: None,
                        allowed_routes: None,
                        tls: None,
                    },
                    gateway_crds::GatewayListeners {
                        name: "https".to_string(),
                        port: 443,
                        protocol: "HTTPS".to_string(),
                        hostname: None,
                        allowed_routes: None,
                        tls: None,
                    },
                ],
                addresses: None,
                infrastructure: None,
            },
            status: None,
        };

        // Find by specific section name
        let matches = find_matching_listeners(&gateway, Some("http"), None);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "http");

        // Find by port
        let matches = find_matching_listeners(&gateway, None, Some(443));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "https");

        // Find all HTTP/HTTPS listeners
        let matches = find_matching_listeners(&gateway, None, None);
        assert_eq!(matches.len(), 2);
    }

    /// Spec: Non-HTTP protocols should not be matched for HTTPRoute
    #[test]
    fn test_find_matching_listeners_filters_non_http() {
        let gateway = Gateway {
            metadata: kube::core::ObjectMeta {
                name: Some("test-gateway".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: gateway_crds::GatewaySpec {
                gateway_class_name: "multiway".to_string(),
                listeners: vec![
                    gateway_crds::GatewayListeners {
                        name: "http".to_string(),
                        port: 80,
                        protocol: "HTTP".to_string(),
                        hostname: None,
                        allowed_routes: None,
                        tls: None,
                    },
                    gateway_crds::GatewayListeners {
                        name: "tcp".to_string(),
                        port: 9000,
                        protocol: "TCP".to_string(), // Non-HTTP
                        hostname: None,
                        allowed_routes: None,
                        tls: None,
                    },
                ],
                addresses: None,
                infrastructure: None,
            },
            status: None,
        };

        // Should only find HTTP listener, not TCP
        let matches = find_matching_listeners(&gateway, None, None);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "http");
    }
}
