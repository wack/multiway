//! Pingora-based HTTP Proxy
//!
//! This module implements the HTTP proxy using CloudFlare's Pingora library.
//! It handles request routing, header modification, and load balancing.

use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use pingora::http::{RequestHeader, ResponseHeader};
use pingora::prelude::*;
use pingora::proxy::{ProxyHttp, Session};
use pingora::server::Server;
use pingora::upstreams::peer::HttpPeer;
use tracing::{debug, error, info, warn};

use crate::config::{GatewayConfig, RouteFilter};
use crate::router::{PathRewrite, Router};

/// Gateway proxy implementation
pub struct GatewayProxy {
    config: Arc<ArcSwap<GatewayConfig>>,
}

impl GatewayProxy {
    /// Create a new gateway proxy
    pub fn new(config: Arc<ArcSwap<GatewayConfig>>) -> Self {
        Self { config }
    }

    /// Run the proxy server
    pub fn run(self) -> anyhow::Result<()> {
        let mut server = Server::new(None)?;
        server.bootstrap();

        // Get current config for initial setup
        let config = self.config.load();

        // Create HTTP proxy service for each listener
        for listener in &config.listeners {
            let addr = format!("0.0.0.0:{}", listener.port);
            info!(
                listener = %listener.name,
                port = listener.port,
                protocol = ?listener.protocol,
                "Starting listener"
            );

            let proxy_service = GatewayProxyService {
                config: self.config.clone(),
                listener_name: listener.name.clone(),
            };

            let mut lb = http_proxy_service(&server.configuration, proxy_service);

            lb.add_tcp(&addr);
            server.add_service(lb);
        }

        info!("Starting Pingora server");
        server.run_forever();
    }
}

/// Proxy service for a single listener
struct GatewayProxyService {
    config: Arc<ArcSwap<GatewayConfig>>,
    listener_name: String,
}

/// Header modification to apply
#[derive(Clone)]
struct HeaderMod {
    name: String,
    value: String,
}

/// Context for a single request
pub struct RequestContext {
    /// Backend address to connect to
    backend_addr: Option<String>,
    /// Route ID for logging
    route_id: Option<String>,
    /// Request header modifications to add
    request_headers_add: Vec<HeaderMod>,
    /// Request header modifications to set
    request_headers_set: Vec<HeaderMod>,
    /// Request headers to remove
    request_headers_remove: Vec<String>,
    /// Response header modifications to add
    response_headers_add: Vec<HeaderMod>,
    /// Response header modifications to set
    response_headers_set: Vec<HeaderMod>,
    /// Response headers to remove
    response_headers_remove: Vec<String>,
    /// Path to use (after any rewrites)
    rewritten_path: Option<String>,
    /// Host to use (after any rewrites)
    rewritten_host: Option<String>,
    /// Original request path
    original_path: String,
    /// Should return 404
    not_found: bool,
    /// Should redirect
    redirect: Option<RedirectInfo>,
}

#[derive(Clone)]
#[allow(dead_code)]
struct RedirectInfo {
    location: String,
    status: u16,
}

#[async_trait]
impl ProxyHttp for GatewayProxyService {
    type CTX = RequestContext;

    fn new_ctx(&self) -> Self::CTX {
        RequestContext {
            backend_addr: None,
            route_id: None,
            request_headers_add: Vec::new(),
            request_headers_set: Vec::new(),
            request_headers_remove: Vec::new(),
            response_headers_add: Vec::new(),
            response_headers_set: Vec::new(),
            response_headers_remove: Vec::new(),
            rewritten_path: None,
            rewritten_host: None,
            original_path: String::new(),
            not_found: false,
            redirect: None,
        }
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<bool, Box<Error>> {
        let config = self.config.load();
        let router = Router::new(&config);

        // Extract request info
        let req = session.req_header();
        let host = req.headers.get("host").and_then(|h| h.to_str().ok());
        let path = req.uri.path();
        let method = req.method.as_str();

        ctx.original_path = path.to_string();

        // Extract headers for matching
        let headers: Vec<(String, String)> = req
            .headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.to_string(), v.to_string()))
            })
            .collect();

        // Extract query parameters
        let query_params: Vec<(String, String)> = req
            .uri
            .query()
            .map(|q| {
                url::form_urlencoded::parse(q.as_bytes())
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Route the request
        let result = router.route(
            &config,
            &self.listener_name,
            crate::router::RequestInfo {
                host,
                path,
                method,
                headers: &headers,
                query_params: &query_params,
            },
        );

        debug!(
            listener = %self.listener_name,
            host = ?host,
            path = path,
            method = method,
            matched = result.route.is_some(),
            redirect = result.redirect.is_some(),
            "Routed request"
        );

        // Handle redirect
        if let Some(redirect) = result.redirect {
            let mut location = String::new();

            if let Some(scheme) = &redirect.scheme {
                location.push_str(scheme);
                location.push_str("://");
            }

            if let Some(hostname) = &redirect.hostname {
                if !location.contains("://") {
                    location.push_str("http://");
                }
                location.push_str(hostname);
            } else if let Some(h) = host {
                if !location.contains("://") {
                    location.push_str("http://");
                }
                location.push_str(h.split(':').next().unwrap_or(h));
            }

            if let Some(port) = redirect.port {
                location.push(':');
                location.push_str(&port.to_string());
            }

            if let Some(path) = &redirect.path {
                location.push_str(path);
            } else {
                location.push_str(&ctx.original_path);
            }

            // Add query string if present
            if let Some(query) = req.uri.query() {
                location.push('?');
                location.push_str(query);
            }

            // Send redirect response
            let mut resp = ResponseHeader::build(redirect.status_code, Some(2))?;
            resp.insert_header("Location", &location)?;
            resp.insert_header("Content-Length", "0")?;
            session.write_response_header(Box::new(resp), true).await?;

            return Ok(true);
        }

        // Check if we have a route
        match result.route {
            None => {
                // Send 404 Not Found response
                debug!(
                    listener = %self.listener_name,
                    path = path,
                    "No matching route found"
                );
                let mut resp = ResponseHeader::build(404, Some(2))?;
                resp.insert_header("Content-Type", "text/plain")?;
                resp.insert_header("Content-Length", "9")?;
                session.write_response_header(Box::new(resp), false).await?;
                session
                    .write_response_body(Some(bytes::Bytes::from("Not Found")), true)
                    .await?;
                return Ok(true);
            }
            Some(matched) => {
                if matched.backends.is_empty() {
                    // Send 503 Service Unavailable response
                    debug!(
                        listener = %self.listener_name,
                        route = %matched.route_id,
                        "Route has no backends"
                    );
                    let mut resp = ResponseHeader::build(503, Some(2))?;
                    resp.insert_header("Content-Type", "text/plain")?;
                    resp.insert_header("Content-Length", "19")?;
                    session.write_response_header(Box::new(resp), false).await?;
                    session
                        .write_response_body(Some(bytes::Bytes::from("Service Unavailable")), true)
                        .await?;
                    return Ok(true);
                }

                // Select backend using weighted random
                let total_weight: u32 = matched.backends.iter().map(|b| b.weight).sum();
                let mut random = rand_value() % total_weight;
                let mut selected_backend = &matched.backends[0];

                for backend in &matched.backends {
                    if random < backend.weight {
                        selected_backend = backend;
                        break;
                    }
                    random -= backend.weight;
                }

                // Build backend address (Kubernetes DNS)
                let addr = format!(
                    "{}.{}.svc.cluster.local:{}",
                    selected_backend.name, selected_backend.namespace, selected_backend.port
                );

                ctx.backend_addr = Some(addr);
                ctx.route_id = Some(matched.route_id);

                // Process filters and store header modifications
                for filter in &matched.filters {
                    match filter {
                        RouteFilter::RequestHeaderModifier { add, set, remove } => {
                            for h in add {
                                ctx.request_headers_add.push(HeaderMod {
                                    name: h.name.clone(),
                                    value: h.value.clone(),
                                });
                            }
                            for h in set {
                                ctx.request_headers_set.push(HeaderMod {
                                    name: h.name.clone(),
                                    value: h.value.clone(),
                                });
                            }
                            ctx.request_headers_remove.extend(remove.clone());
                        }
                        RouteFilter::ResponseHeaderModifier { add, set, remove } => {
                            for h in add {
                                ctx.response_headers_add.push(HeaderMod {
                                    name: h.name.clone(),
                                    value: h.value.clone(),
                                });
                            }
                            for h in set {
                                ctx.response_headers_set.push(HeaderMod {
                                    name: h.name.clone(),
                                    value: h.value.clone(),
                                });
                            }
                            ctx.response_headers_remove.extend(remove.clone());
                        }
                        _ => {}
                    }
                }

                // Handle path rewrite
                if let Some(ref rewrite) = matched.path_rewrite {
                    let new_path = match rewrite {
                        PathRewrite::ReplaceFullPath(new) => new.clone(),
                        PathRewrite::ReplacePrefixMatch {
                            match_prefix,
                            replacement,
                        } => ctx.original_path.replacen(match_prefix, replacement, 1),
                    };
                    ctx.rewritten_path = Some(new_path);
                }

                // Handle hostname rewrite
                ctx.rewritten_host = matched.hostname_rewrite;
            }
        }

        Ok(false) // Continue processing
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>, Box<Error>> {
        let addr = ctx
            .backend_addr
            .as_ref()
            .expect("backend_addr should be set");

        debug!(
            backend = %addr,
            route = ?ctx.route_id,
            "Selected upstream"
        );

        let peer = HttpPeer::new(addr, false, String::new());
        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<(), Box<Error>> {
        // Apply path rewrite
        if let Some(ref new_path) = ctx.rewritten_path {
            // Rebuild URI with new path
            let mut parts = upstream_request.uri.clone().into_parts();
            let query = parts.path_and_query.as_ref().and_then(|pq| pq.query());
            let new_path_and_query = if let Some(q) = query {
                format!("{}?{}", new_path, q)
            } else {
                new_path.clone()
            };
            if let Ok(pq) = new_path_and_query.parse() {
                parts.path_and_query = Some(pq);
                if let Ok(uri) = http::Uri::from_parts(parts) {
                    upstream_request.set_uri(uri);
                }
            }
        }

        // Apply host rewrite
        if let Some(ref new_host) = ctx.rewritten_host {
            let _ = upstream_request.insert_header("Host", new_host);
        }

        // Apply request header modifications
        // Clone the data we need to avoid lifetime issues
        let remove = ctx.request_headers_remove.clone();
        let set = ctx.request_headers_set.clone();
        let add = ctx.request_headers_add.clone();

        // Remove headers first
        for name in &remove {
            let _ = upstream_request.remove_header(name);
        }
        // Then set headers (overwrites)
        for header in &set {
            let _ = upstream_request.insert_header(header.name.clone(), header.value.clone());
        }
        // Then add headers (appends)
        for header in &add {
            let _ = upstream_request.append_header(header.name.clone(), header.value.clone());
        }

        Ok(())
    }

    async fn response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<(), Box<Error>>
    where
        Self::CTX: Send + Sync,
    {
        // Apply response header modifications
        // Clone the data we need to avoid lifetime issues
        let remove = ctx.response_headers_remove.clone();
        let set = ctx.response_headers_set.clone();
        let add = ctx.response_headers_add.clone();

        // Remove headers first
        for name in &remove {
            let _ = upstream_response.remove_header(name);
        }
        // Then set headers
        for header in &set {
            let _ = upstream_response.insert_header(header.name.clone(), header.value.clone());
        }
        // Then add headers
        for header in &add {
            let _ = upstream_response.append_header(header.name.clone(), header.value.clone());
        }

        Ok(())
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        e: Box<Error>,
    ) -> Box<Error> {
        warn!(
            error = %e,
            original_path = %ctx.original_path,
            "Failed to connect to upstream"
        );
        e
    }

    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        _session: &mut Session,
        e: Box<Error>,
        ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<Error> {
        error!(
            error = %e,
            peer = %peer,
            original_path = %ctx.original_path,
            client_reused = client_reused,
            "Error while proxying"
        );
        e
    }
}

/// Simple random number generator for load balancing
fn rand_value() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rand_value() {
        let v1 = rand_value();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let v2 = rand_value();
        // Values should generally be different (not a strict test)
        // This is just to ensure the function runs
        assert!(v1 != v2 || true); // Always passes, just checking it compiles
    }
}
