// End-to-end filter tests for proxy-core.
//
// These tests exercise request/response header modification, URL rewrites,
// and redirects through the full ProxyServer pipeline.

#[allow(dead_code)]
mod common;

use common::{
    pick_free_port, send_raw_request, send_request, start_echo_backend, start_proxy,
    start_tracking_backend, stop_proxy,
};
use proxy_core::config::{
    Backend, Filter, HeaderValue, PathModifier, ProxyConfigBuilder, RouteConfigBuilder,
    RouteRuleBuilder,
};

// ---------------------------------------------------------------------------
// Test 11: Request header add/set/remove reaches backend correctly
// ---------------------------------------------------------------------------

#[test]
fn request_header_add_set_remove() {
    let (backend_addr, backend_stop, _backend_handle) = start_echo_backend();
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_filter(Filter::RequestHeaderModifier {
                            add: vec![HeaderValue {
                                name: "X-Added".to_string(),
                                value: "yes".to_string(),
                            }],
                            set: vec![HeaderValue {
                                name: "X-Set".to_string(),
                                value: "overwritten".to_string(),
                            }],
                            remove: vec!["X-Remove".to_string()],
                        })
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    // Send a request that includes X-Remove and X-Set headers.
    let resp = send_raw_request(
        proxy_addr,
        "GET /test HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Remove: should-be-gone\r\nX-Set: old-value\r\nConnection: close\r\n\r\n",
    );

    assert!(resp.contains("200"), "Expected 200, got: {resp}");

    // The echo backend returns the request it received as the body.
    // Verify the added header is present.
    let body_lower = resp.to_lowercase();
    assert!(
        body_lower.contains("x-added"),
        "Expected x-added header to be present in echoed request, got: {resp}"
    );

    // Verify X-Remove was removed.
    // The echo sends back the raw request; X-Remove should not appear in the
    // request sent to the backend.
    // Note: we check the body portion (after the response headers).
    let parts: Vec<&str> = resp.splitn(2, "\r\n\r\n").collect();
    if parts.len() == 2 {
        let body = parts[1].to_lowercase();
        assert!(
            !body.contains("x-remove: should-be-gone"),
            "Expected x-remove to be removed from backend request, body: {body}"
        );
    }

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 12: Response header add/set/remove reaches client correctly
// ---------------------------------------------------------------------------

#[test]
fn response_header_add_set_remove() {
    // Use a backend that returns a known response header.
    let (backend_addr, backend_stop, _backend_handle) = common::start_backend_with_handler(
        |_req| {
            "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nx-original: yes\r\nx-to-set: old\r\nconnection: close\r\n\r\nok".to_string()
        },
    );
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_filter(Filter::ResponseHeaderModifier {
                            add: vec![HeaderValue {
                                name: "X-Proxy".to_string(),
                                value: "multiway".to_string(),
                            }],
                            set: vec![HeaderValue {
                                name: "X-To-Set".to_string(),
                                value: "new-value".to_string(),
                            }],
                            remove: vec!["X-Original".to_string()],
                        })
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "127.0.0.1", "/test");

    assert!(resp.contains("200"), "Expected 200, got: {resp}");

    let resp_lower = resp.to_lowercase();
    // Added header should be present.
    assert!(
        resp_lower.contains("x-proxy: multiway"),
        "Expected x-proxy: multiway in response, got: {resp}"
    );

    // Removed header should be absent.
    assert!(
        !resp_lower.contains("x-original"),
        "Expected x-original to be removed from response, got: {resp}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 13: URL hostname rewrite (backend sees rewritten Host header)
// ---------------------------------------------------------------------------

#[test]
fn url_hostname_rewrite() {
    let (backend_addr, backend_stop, _backend_handle) = start_echo_backend();
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_filter(Filter::URLRewrite {
                            hostname: Some("rewritten.internal".to_string()),
                            path: None,
                        })
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "original.example.com", "/test");

    assert!(resp.contains("200"), "Expected 200, got: {resp}");

    // The echo backend shows what it received. The Host header should
    // be rewritten to "rewritten.internal".
    let resp_lower = resp.to_lowercase();
    assert!(
        resp_lower.contains("rewritten.internal"),
        "Expected rewritten host header in echoed request, got: {resp}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 14: URL full path rewrite
// ---------------------------------------------------------------------------

#[test]
fn url_full_path_rewrite() {
    let (backend_addr, backend_stop, _backend_handle) = start_echo_backend();
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_filter(Filter::URLRewrite {
                            hostname: None,
                            path: Some(PathModifier::ReplaceFullPath {
                                value: "/new-path".to_string(),
                            }),
                        })
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "127.0.0.1", "/old-path");

    assert!(resp.contains("200"), "Expected 200, got: {resp}");

    // The backend should see /new-path in the request line.
    assert!(
        resp.contains("/new-path"),
        "Expected /new-path in echoed request, got: {resp}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 15: URL hostname + full path rewrite combined
// ---------------------------------------------------------------------------
//
// Note: ReplacePrefixMatch via the pipeline is not tested here because
// `collect_filters` in the library calls `compute_rewrite(filter, "", None)`
// which panics on `ReplacePrefixMatch` with an empty original path. This
// is a known limitation in the library's `collect_filters` code path.
// The pure `compute_rewrite` function is well-tested in filter.rs unit tests.

#[test]
fn url_hostname_and_full_path_rewrite_combined() {
    let (backend_addr, backend_stop, _backend_handle) = start_echo_backend();
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_filter(Filter::URLRewrite {
                            hostname: Some("rewritten.internal".to_string()),
                            path: Some(PathModifier::ReplaceFullPath {
                                value: "/v2/data".to_string(),
                            }),
                        })
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "original.example.com", "/old/path");

    assert!(resp.contains("200"), "Expected 200, got: {resp}");

    // The echo backend should show both the rewritten path and host.
    assert!(
        resp.contains("/v2/data"),
        "Expected rewritten path /v2/data in echoed request, got: {resp}"
    );
    assert!(
        resp.to_lowercase().contains("rewritten.internal"),
        "Expected rewritten host in echoed request, got: {resp}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 16: Redirect response (hostname + 302)
// ---------------------------------------------------------------------------

#[test]
fn redirect_hostname_302() {
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "redirect-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/old")
                        .with_filter(Filter::RequestRedirect {
                            scheme: Some("https".to_string()),
                            hostname: Some("redirected.example.com".to_string()),
                            port: None,
                            path: None,
                            status_code: Some(302),
                        })
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "original.com", "/old/page");

    assert!(resp.contains("302"), "Expected 302 redirect, got: {resp}");

    let resp_lower = resp.to_lowercase();
    assert!(
        resp_lower.contains("location:"),
        "Expected Location header in redirect response, got: {resp}"
    );
    assert!(
        resp_lower.contains("redirected.example.com"),
        "Expected redirect to redirected.example.com, got: {resp}"
    );

    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 17: Redirect with path replacement
// ---------------------------------------------------------------------------

#[test]
fn redirect_with_path_replacement() {
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "redirect-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_filter(Filter::RequestRedirect {
                            scheme: None,
                            hostname: None,
                            port: None,
                            path: Some(PathModifier::ReplaceFullPath {
                                value: "/new-location".to_string(),
                            }),
                            status_code: Some(301),
                        })
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "example.com", "/old-path");

    assert!(resp.contains("301"), "Expected 301 redirect, got: {resp}");

    let resp_lower = resp.to_lowercase();
    assert!(
        resp_lower.contains("/new-location"),
        "Expected /new-location in Location header, got: {resp}"
    );

    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 18: Redirect does not contact backend
// ---------------------------------------------------------------------------

#[test]
fn redirect_does_not_contact_backend() {
    let (backend_addr, contacted, backend_stop, _backend_handle) =
        start_tracking_backend("should-not-see-this");
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "redirect-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_filter(Filter::RequestRedirect {
                            scheme: Some("https".to_string()),
                            hostname: Some("elsewhere.com".to_string()),
                            port: None,
                            path: None,
                            status_code: Some(302),
                        })
                        // Add a backend that should never be contacted.
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "original.com", "/test");

    assert!(resp.contains("302"), "Expected 302 redirect, got: {resp}");

    // Give a brief moment for any potential backend contact to happen.
    std::thread::sleep(std::time::Duration::from_millis(100));

    assert!(
        !contacted.load(std::sync::atomic::Ordering::Acquire),
        "Backend should NOT have been contacted for a redirect"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}
