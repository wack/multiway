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

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use gateway_crds::{
    Gateway, HTTPRoute, HttpRouteParentRefs, HttpRouteRules, HttpRouteRulesBackendRefs,
    HttpRouteRulesFilters, HttpRouteRulesFiltersType, HttpRouteRulesMatches,
    HttpRouteRulesMatchesHeaders, HttpRouteRulesMatchesHeadersType, HttpRouteRulesMatchesMethod,
    HttpRouteRulesMatchesPath, HttpRouteRulesMatchesPathType, HttpRouteRulesMatchesQueryParams,
    HttpRouteRulesMatchesQueryParamsType, HttpRouteStatus, HttpRouteStatusParents,
    HttpRouteStatusParentsParentRef, ReferenceGrant,
};
use k8s_openapi::api::core::v1::{ConfigMap, Service};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::chrono::Utc;
use kube::api::{ListParams, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Api, Client, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, error, info, instrument, warn};

use super::config::{
    BackendRef, CONFIG_KEY, CONTROLLER_NAME, DataPlaneNames, GatewayConfig,
    HeaderMatch, HeaderMatchType, HeaderValue, PathMatch, PathMatchType, PathModifier,
    QueryParamMatch, QueryParamMatchType, RouteConfig, RouteFilter, RouteMatch, RouteRule,
    TimeoutConfig,
};
use super::context::ControllerContext;
use super::error::{ControllerError, Result};
use super::gateway::{AllowedNamespaces, get_allowed_route_namespaces};
use super::gateway_class::get_accepted_gateway_class;

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
            |obj, error, ctx| error_policy(obj, error, ctx),
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

/// Reconcile a single HTTPRoute resource
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

    let parent_refs = httproute
        .spec
        .parent_refs
        .as_ref()
        .cloned()
        .unwrap_or_default();

    if parent_refs.is_empty() {
        debug!("HTTPRoute has no parentRefs");
        return Ok(Action::requeue(Duration::from_secs(
            ctx.config.requeue_after_secs,
        )));
    }

    let mut parent_statuses = Vec::new();

    for parent_ref in &parent_refs {
        let status = process_parent_ref(&ctx, &httproute, parent_ref, &namespace).await;
        parent_statuses.push(status);
    }

    // Update HTTPRoute status
    update_httproute_status(&ctx.client, &namespace, &name, parent_statuses).await?;

    info!("HTTPRoute reconciled successfully");
    Ok(Action::requeue(Duration::from_secs(
        ctx.config.requeue_after_secs,
    )))
}

/// Process a single parent reference
async fn process_parent_ref(
    ctx: &ControllerContext,
    httproute: &HTTPRoute,
    parent_ref: &HttpRouteParentRefs,
    route_namespace: &str,
) -> HttpRouteStatusParents {
    let parent_namespace = parent_ref.namespace.as_deref().unwrap_or(route_namespace);
    let parent_name = &parent_ref.name;
    let _now = Time(Utc::now());

    // Verify the parent is a Gateway (or default to Gateway)
    let parent_kind = parent_ref.kind.as_deref().unwrap_or("Gateway");
    let parent_group = parent_ref
        .group
        .as_deref()
        .unwrap_or("gateway.networking.k8s.io");

    if parent_group != "gateway.networking.k8s.io" || parent_kind != "Gateway" {
        return create_parent_status(
            parent_ref,
            route_namespace,
            false,
            "InvalidParentRef",
            format!("Unsupported parent type: {}/{}", parent_group, parent_kind),
        );
    }

    // Get the Gateway
    let gateway_api: Api<Gateway> = Api::namespaced(ctx.client.clone(), parent_namespace);
    let gateway = match gateway_api.get(parent_name).await {
        Ok(gw) => gw,
        Err(kube::Error::Api(err)) if err.code == 404 => {
            return create_parent_status(
                parent_ref,
                route_namespace,
                false,
                "NoMatchingParent",
                format!("Gateway {}/{} not found", parent_namespace, parent_name),
            );
        }
        Err(e) => {
            error!(%e, "Error fetching Gateway");
            return create_parent_status(
                parent_ref,
                route_namespace,
                false,
                "InternalError",
                format!("Error fetching Gateway: {}", e),
            );
        }
    };

    // Check if the GatewayClass is ours
    match get_accepted_gateway_class(&ctx.client, &gateway.spec.gateway_class_name).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return create_parent_status(
                parent_ref,
                route_namespace,
                false,
                "NotAllowedByGateway",
                "Gateway's GatewayClass is not managed by this controller".to_string(),
            );
        }
        Err(e) => {
            error!(%e, "Error checking GatewayClass");
            return create_parent_status(
                parent_ref,
                route_namespace,
                false,
                "InternalError",
                format!("Error checking GatewayClass: {}", e),
            );
        }
    }

    // Check if the route is allowed by the Gateway's listeners
    let section_name = parent_ref.section_name.as_deref();
    let matching_listeners = find_matching_listeners(&gateway, section_name, parent_ref.port);

    if matching_listeners.is_empty() {
        return create_parent_status(
            parent_ref,
            route_namespace,
            false,
            "NoMatchingListenerHostname",
            "No matching listener found on Gateway".to_string(),
        );
    }

    // Check namespace permissions for each listener
    let gateway_namespace = gateway.namespace().unwrap_or_default();
    let mut allowed = false;
    for listener in &matching_listeners {
        let allowed_ns = get_allowed_route_namespaces(listener, &gateway_namespace);
        if is_namespace_allowed(&allowed_ns, route_namespace) {
            allowed = true;
            break;
        }
    }

    if !allowed && route_namespace != gateway_namespace {
        // Check for ReferenceGrant
        let has_grant = check_reference_grant(
            &ctx.client,
            route_namespace,
            &gateway_namespace,
            parent_name,
        )
        .await
        .unwrap_or(false);

        if !has_grant {
            return create_parent_status(
                parent_ref,
                route_namespace,
                false,
                "RefNotPermitted",
                format!(
                    "Route namespace {} is not allowed by Gateway listener policies",
                    route_namespace
                ),
            );
        }
    }

    // Validate backends
    let rules = httproute.spec.rules.as_ref().cloned().unwrap_or_default();
    for rule in &rules {
        if let Some(backends) = &rule.backend_refs {
            for backend in backends {
                if let Err(e) = validate_backend(&ctx.client, route_namespace, backend).await {
                    warn!(%e, "Invalid backend reference");
                    return create_parent_status(
                        parent_ref,
                        route_namespace,
                        false,
                        "BackendNotFound",
                        format!("{}", e),
                    );
                }
            }
        }
    }

    // Update the Gateway's ConfigMap with this route
    let names = DataPlaneNames::new(&gateway_namespace, gateway.name_any());
    if let Err(e) = update_gateway_configmap(
        &ctx.client,
        &names,
        httproute,
        &gateway,
        &matching_listeners
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>(),
    )
    .await
    {
        error!(%e, "Failed to update Gateway ConfigMap");
        return create_parent_status(
            parent_ref,
            route_namespace,
            false,
            "InternalError",
            format!("Failed to update Gateway configuration: {}", e),
        );
    }

    // Route is accepted
    create_parent_status(
        parent_ref,
        route_namespace,
        true,
        "Accepted",
        "Route is accepted by Gateway".to_string(),
    )
}

/// Find listeners that match the parent reference
fn find_matching_listeners<'a>(
    gateway: &'a Gateway,
    section_name: Option<&str>,
    port: Option<i32>,
) -> Vec<&'a gateway_crds::GatewayListeners> {
    gateway
        .spec
        .listeners
        .iter()
        .filter(|l| {
            // Match by section name if specified
            if let Some(name) = section_name {
                if l.name != name {
                    return false;
                }
            }
            // Match by port if specified
            if let Some(p) = port {
                if l.port != p {
                    return false;
                }
            }
            // Must be HTTP or HTTPS protocol
            let protocol = l.protocol.to_uppercase();
            protocol == "HTTP" || protocol == "HTTPS"
        })
        .collect()
}

/// Check if a namespace is allowed by the listener policy
fn is_namespace_allowed(allowed: &AllowedNamespaces, namespace: &str) -> bool {
    match allowed {
        AllowedNamespaces::All => true,
        AllowedNamespaces::Same(gw_ns) => namespace == gw_ns,
        AllowedNamespaces::Selector { match_labels } => {
            // In a real implementation, you'd check the namespace labels
            // For now, we'll be permissive if there's a selector
            !match_labels.is_empty()
        }
    }
}

/// Check for a ReferenceGrant allowing the cross-namespace reference
async fn check_reference_grant(
    client: &Client,
    from_namespace: &str,
    to_namespace: &str,
    _to_name: &str,
) -> Result<bool> {
    let api: Api<ReferenceGrant> = Api::namespaced(client.clone(), to_namespace);
    let grants = api.list(&ListParams::default()).await?;

    for grant in grants {
        // Check if the grant allows references from our namespace
        let allows_from = grant.spec.from.iter().any(|from| {
            from.group == "gateway.networking.k8s.io"
                && from.kind == "HTTPRoute"
                && from.namespace == from_namespace
        });

        // Check if the grant allows references to Gateways
        let allows_to = grant
            .spec
            .to
            .iter()
            .any(|to| to.group == "gateway.networking.k8s.io" && to.kind == "Gateway");

        if allows_from && allows_to {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Validate a backend reference
async fn validate_backend(
    client: &Client,
    route_namespace: &str,
    backend: &HttpRouteRulesBackendRefs,
) -> Result<()> {
    let backend_namespace = backend.namespace.as_deref().unwrap_or(route_namespace);
    let backend_kind = backend.kind.as_deref().unwrap_or("Service");
    let backend_group = backend.group.as_deref().unwrap_or("");

    // We only support Service backends
    if backend_group != "" || backend_kind != "Service" {
        return Err(ControllerError::invalid_httproute_config(format!(
            "Unsupported backend type: {}/{}",
            backend_group, backend_kind
        )));
    }

    // Check cross-namespace reference
    if backend_namespace != route_namespace {
        // Would need to check ReferenceGrant here
        // For now, we'll allow it and let the data plane handle the error
        debug!(
            from = route_namespace,
            to = backend_namespace,
            "Cross-namespace backend reference"
        );
    }

    // Check if the service exists
    let svc_api: Api<Service> = Api::namespaced(client.clone(), backend_namespace);
    match svc_api.get(&backend.name).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(err)) if err.code == 404 => Err(ControllerError::backend_not_found(
            backend_namespace,
            &backend.name,
        )),
        Err(e) => Err(e.into()),
    }
}

/// Update the Gateway's ConfigMap with route configuration
async fn update_gateway_configmap(
    client: &Client,
    names: &DataPlaneNames,
    httproute: &HTTPRoute,
    gateway: &Gateway,
    attached_listeners: &[&str],
) -> Result<()> {
    let namespace = names.namespace();
    let configmap_name = names.configmap_name();
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);

    // Get existing ConfigMap
    let configmap = match api.get(&configmap_name).await {
        Ok(cm) => cm,
        Err(kube::Error::Api(err)) if err.code == 404 => {
            // ConfigMap doesn't exist yet - Gateway hasn't been reconciled
            debug!("ConfigMap not found, skipping route update");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    // Parse existing configuration
    let config_data = configmap
        .data
        .as_ref()
        .and_then(|d| d.get(CONFIG_KEY))
        .map(|s| s.as_str())
        .unwrap_or("{}");

    let mut config: GatewayConfig = GatewayConfig::from_json(config_data).unwrap_or_else(|_| {
        GatewayConfig::new(gateway.namespace().unwrap_or_default(), gateway.name_any())
    });

    // Remove existing route config for this HTTPRoute
    let route_key = format!(
        "{}/{}",
        httproute.namespace().unwrap_or_default(),
        httproute.name_any()
    );
    config.routes.retain(|r| {
        let existing_key = format!("{}/{}", r.namespace, r.name);
        existing_key != route_key
    });

    // Convert HTTPRoute to RouteConfig
    let route_config = convert_httproute_to_config(httproute, attached_listeners)?;
    config.routes.push(route_config);

    // Update the ConfigMap
    let config_json = config.to_json()?;
    let mut data = BTreeMap::new();
    data.insert(CONFIG_KEY.to_string(), config_json);

    let patch = serde_json::json!({
        "data": data
    });

    api.patch(
        &configmap_name,
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;

    debug!("Updated ConfigMap with route configuration");
    Ok(())
}

/// Convert an HTTPRoute to our internal RouteConfig format
fn convert_httproute_to_config(
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
    let mut route_rule = RouteRule {
        name: rule.name.clone(),
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
                    percent: mirror.percent.map(|p| p as u32),
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
    if duration.ends_with('s') {
        duration[..duration.len() - 1].parse().ok()
    } else if duration.ends_with('m') {
        duration[..duration.len() - 1]
            .parse::<f64>()
            .ok()
            .map(|m| m * 60.0)
    } else if duration.ends_with('h') {
        duration[..duration.len() - 1]
            .parse::<f64>()
            .ok()
            .map(|h| h * 3600.0)
    } else {
        duration.parse().ok()
    }
}

/// Create a parent status entry
fn create_parent_status(
    parent_ref: &HttpRouteParentRefs,
    _route_namespace: &str,
    accepted: bool,
    reason: &str,
    message: String,
) -> HttpRouteStatusParents {
    let now = Time(Utc::now());
    let status_value = if accepted { "True" } else { "False" };

    HttpRouteStatusParents {
        controller_name: CONTROLLER_NAME.to_string(),
        parent_ref: HttpRouteStatusParentsParentRef {
            group: parent_ref.group.clone(),
            kind: parent_ref.kind.clone(),
            name: parent_ref.name.clone(),
            namespace: parent_ref.namespace.clone(),
            port: parent_ref.port,
            section_name: parent_ref.section_name.clone(),
        },
        conditions: vec![
            Condition {
                type_: "Accepted".to_string(),
                status: status_value.to_string(),
                observed_generation: None,
                last_transition_time: now.clone(),
                reason: reason.to_string(),
                message: message.clone(),
            },
            Condition {
                type_: "ResolvedRefs".to_string(),
                status: status_value.to_string(),
                observed_generation: None,
                last_transition_time: now,
                reason: if accepted { "ResolvedRefs" } else { reason }.to_string(),
                message,
            },
        ],
    }
}

/// Update the status of an HTTPRoute
async fn update_httproute_status(
    client: &Client,
    namespace: &str,
    name: &str,
    parents: Vec<HttpRouteStatusParents>,
) -> Result<()> {
    let api: Api<HTTPRoute> = Api::namespaced(client.clone(), namespace);

    let status = HttpRouteStatus { parents };

    let patch = serde_json::json!({
        "status": status
    });

    api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;

    Ok(())
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
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("10s"), Some(10.0));
        assert_eq!(parse_duration("1m"), Some(60.0));
        assert_eq!(parse_duration("1h"), Some(3600.0));
        assert_eq!(parse_duration("30"), Some(30.0));
        assert_eq!(parse_duration("invalid"), None);
    }

    #[test]
    fn test_convert_method() {
        assert_eq!(convert_method(&HttpRouteRulesMatchesMethod::Get), "GET");
        assert_eq!(convert_method(&HttpRouteRulesMatchesMethod::Post), "POST");
    }

    #[test]
    fn test_is_namespace_allowed() {
        assert!(is_namespace_allowed(&AllowedNamespaces::All, "any-ns"));
        assert!(is_namespace_allowed(
            &AllowedNamespaces::Same("default".to_string()),
            "default"
        ));
        assert!(!is_namespace_allowed(
            &AllowedNamespaces::Same("default".to_string()),
            "other"
        ));
    }
}
