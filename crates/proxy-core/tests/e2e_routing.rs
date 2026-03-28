// End-to-end routing tests for proxy-core.
//
// These tests exercise the full request lifecycle through ProxyServer,
// verifying that routing decisions (path matching, hostname matching,
// header matching, multi-listener routing) work correctly end-to-end.

#[allow(dead_code)]
mod common;

use common::{pick_free_port, send_request, start_backend, start_proxy, stop_proxy};
use proxy_core::config::{
    Backend, HeaderMatch, HeaderMatchType, PathMatch, PathMatchType, ProxyConfigBuilder,
    RouteConfigBuilder, RouteMatch, RouteRuleBuilder,
};

// ---------------------------------------------------------------------------
// Test 1: Exact path match routes to correct backend
// ---------------------------------------------------------------------------

#[test]
fn exact_path_match_routes_to_correct_backend() {
    let (backend_addr, backend_stop, _backend_handle) = start_backend("exact-hit");
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "exact-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_exact_path("/exact")
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "127.0.0.1", "/exact");
    assert!(
        resp.contains("200"),
        "Expected 200 for exact path match, got: {resp}"
    );
    assert!(
        resp.contains("exact-hit"),
        "Expected backend body, got: {resp}"
    );

    // A different path should not match.
    let resp_miss = send_request(proxy_addr, "127.0.0.1", "/exact/sub");
    assert!(
        resp_miss.contains("404"),
        "Expected 404 for non-matching path, got: {resp_miss}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 2: Prefix path match routes to correct backend
// ---------------------------------------------------------------------------

#[test]
fn prefix_path_match_routes_to_correct_backend() {
    let (backend_addr, backend_stop, _backend_handle) = start_backend("prefix-hit");
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "prefix-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/api")
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    // Should match /api and /api/anything.
    let resp = send_request(proxy_addr, "127.0.0.1", "/api/users");
    assert!(
        resp.contains("200"),
        "Expected 200 for prefix match, got: {resp}"
    );
    assert!(
        resp.contains("prefix-hit"),
        "Expected backend body, got: {resp}"
    );

    // /apiversion should NOT match (segment boundary).
    let resp_miss = send_request(proxy_addr, "127.0.0.1", "/apiversion");
    assert!(
        resp_miss.contains("404"),
        "Expected 404 for partial segment match, got: {resp_miss}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 3: Longer prefix wins over shorter prefix
// ---------------------------------------------------------------------------

#[test]
fn longer_prefix_wins_over_shorter_prefix() {
    let (short_addr, short_stop, _short_handle) = start_backend("short-prefix");
    let (long_addr, long_stop, _long_handle) = start_backend("long-prefix");
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "short-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/api")
                        .with_backend(Backend::new("default", "127.0.0.1", short_addr.port()))
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
                        .with_backend(Backend::new("default", "127.0.0.1", long_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    // /api/users/123 should route to the longer prefix backend.
    let resp = send_request(proxy_addr, "127.0.0.1", "/api/users/123");
    assert!(resp.contains("200"), "Expected 200, got: {resp}");
    assert!(
        resp.contains("long-prefix"),
        "Expected long-prefix backend, got: {resp}"
    );

    // /api/other should route to the shorter prefix backend.
    let resp2 = send_request(proxy_addr, "127.0.0.1", "/api/other");
    assert!(resp2.contains("200"), "Expected 200, got: {resp2}");
    assert!(
        resp2.contains("short-prefix"),
        "Expected short-prefix backend, got: {resp2}"
    );

    short_stop.store(true, std::sync::atomic::Ordering::Release);
    long_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 4: Exact match wins over prefix match
// ---------------------------------------------------------------------------

#[test]
fn exact_match_wins_over_prefix_match() {
    let (prefix_addr, prefix_stop, _prefix_handle) = start_backend("prefix-backend");
    let (exact_addr, exact_stop, _exact_handle) = start_backend("exact-backend");
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "prefix-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/api")
                        .with_backend(Backend::new("default", "127.0.0.1", prefix_addr.port()))
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
                        .with_backend(Backend::new("default", "127.0.0.1", exact_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    // Exactly /api should go to exact backend.
    let resp = send_request(proxy_addr, "127.0.0.1", "/api");
    assert!(resp.contains("200"), "Expected 200, got: {resp}");
    assert!(
        resp.contains("exact-backend"),
        "Expected exact-backend, got: {resp}"
    );

    // /api/sub should go to prefix backend.
    let resp2 = send_request(proxy_addr, "127.0.0.1", "/api/sub");
    assert!(resp2.contains("200"), "Expected 200, got: {resp2}");
    assert!(
        resp2.contains("prefix-backend"),
        "Expected prefix-backend, got: {resp2}"
    );

    prefix_stop.store(true, std::sync::atomic::Ordering::Release);
    exact_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 5: Hostname exact match routes to correct backend
// ---------------------------------------------------------------------------

#[test]
fn hostname_exact_match_routes_correctly() {
    let (backend_addr, backend_stop, _backend_handle) = start_backend("host-match");
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "host-route")
                .attached_to("http")
                .with_hostname("example.com")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    // Matching hostname.
    let resp = send_request(proxy_addr, "example.com", "/test");
    assert!(
        resp.contains("200"),
        "Expected 200 for matching hostname, got: {resp}"
    );
    assert!(
        resp.contains("host-match"),
        "Expected backend body, got: {resp}"
    );

    // Non-matching hostname.
    let resp_miss = send_request(proxy_addr, "other.com", "/test");
    assert!(
        resp_miss.contains("404"),
        "Expected 404 for non-matching hostname, got: {resp_miss}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 6: Hostname wildcard match (*.example.com)
// ---------------------------------------------------------------------------

#[test]
fn hostname_wildcard_match() {
    let (backend_addr, backend_stop, _backend_handle) = start_backend("wildcard-match");
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "wildcard-route")
                .attached_to("http")
                .with_hostname("*.example.com")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    // foo.example.com should match.
    let resp = send_request(proxy_addr, "foo.example.com", "/test");
    assert!(
        resp.contains("200"),
        "Expected 200 for wildcard match, got: {resp}"
    );

    // example.com (bare domain) should NOT match.
    let resp_bare = send_request(proxy_addr, "example.com", "/test");
    assert!(
        resp_bare.contains("404"),
        "Expected 404 for bare domain, got: {resp_bare}"
    );

    // foo.bar.example.com (nested subdomain) should NOT match.
    let resp_nested = send_request(proxy_addr, "foo.bar.example.com", "/test");
    assert!(
        resp_nested.contains("404"),
        "Expected 404 for nested subdomain, got: {resp_nested}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 7: Header-based routing (exact match)
// ---------------------------------------------------------------------------

#[test]
fn header_based_routing() {
    let (backend_addr, backend_stop, _backend_handle) = start_backend("header-route-hit");
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "header-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_match(RouteMatch {
                            path: Some(PathMatch {
                                match_type: PathMatchType::PathPrefix,
                                value: "/".to_string(),
                            }),
                            headers: vec![HeaderMatch {
                                name: "X-Version".to_string(),
                                match_type: HeaderMatchType::Exact,
                                value: "v2".to_string(),
                            }],
                            ..Default::default()
                        })
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    // Request with matching header.
    let resp = common::send_raw_request(
        proxy_addr,
        "GET /test HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Version: v2\r\nConnection: close\r\n\r\n",
    );
    assert!(
        resp.contains("200"),
        "Expected 200 for matching header, got: {resp}"
    );

    // Request without the header.
    let resp_miss = send_request(proxy_addr, "127.0.0.1", "/test");
    assert!(
        resp_miss.contains("404"),
        "Expected 404 for missing header, got: {resp_miss}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 8: Multiple routes, each going to a different backend
// ---------------------------------------------------------------------------

#[test]
fn multiple_routes_to_different_backends() {
    let (api_addr, api_stop, _api_handle) = start_backend("api-backend");
    let (web_addr, web_stop, _web_handle) = start_backend("web-backend");
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "api-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/api")
                        .with_backend(Backend::new("default", "127.0.0.1", api_addr.port()))
                        .build(),
                )
                .build(),
        )
        .with_route(
            RouteConfigBuilder::new("default", "web-route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/web")
                        .with_backend(Backend::new("default", "127.0.0.1", web_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp_api = send_request(proxy_addr, "127.0.0.1", "/api/data");
    assert!(
        resp_api.contains("api-backend"),
        "Expected api-backend, got: {resp_api}"
    );

    let resp_web = send_request(proxy_addr, "127.0.0.1", "/web/page");
    assert!(
        resp_web.contains("web-backend"),
        "Expected web-backend, got: {resp_web}"
    );

    api_stop.store(true, std::sync::atomic::Ordering::Release);
    web_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 9: No matching route returns 404
// ---------------------------------------------------------------------------

#[test]
fn no_matching_route_returns_404() {
    let port = pick_free_port();

    // Config with a listener but no routes.
    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "127.0.0.1", "/anything");
    assert!(
        resp.contains("404"),
        "Expected 404 with no routes, got: {resp}"
    );

    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 10: Multiple listeners on different ports route independently
// ---------------------------------------------------------------------------

#[test]
fn multiple_listeners_route_independently() {
    let (api_addr, api_stop, _api_handle) = start_backend("api-listener");
    let (web_addr, web_stop, _web_handle) = start_backend("web-listener");
    let port1 = pick_free_port();
    let port2 = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("api-listener", port1, None)
        .with_http_listener("web-listener", port2, None)
        .with_route(
            RouteConfigBuilder::new("default", "api-route")
                .attached_to("api-listener")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_backend(Backend::new("default", "127.0.0.1", api_addr.port()))
                        .build(),
                )
                .build(),
        )
        .with_route(
            RouteConfigBuilder::new("default", "web-route")
                .attached_to("web-listener")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_backend(Backend::new("default", "127.0.0.1", web_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);

    let resp1 = send_request(([127, 0, 0, 1], port1).into(), "127.0.0.1", "/test");
    assert!(
        resp1.contains("api-listener"),
        "Port 1 should serve api-listener backend, got: {resp1}"
    );

    let resp2 = send_request(([127, 0, 0, 1], port2).into(), "127.0.0.1", "/test");
    assert!(
        resp2.contains("web-listener"),
        "Port 2 should serve web-listener backend, got: {resp2}"
    );

    api_stop.store(true, std::sync::atomic::Ordering::Release);
    web_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}
