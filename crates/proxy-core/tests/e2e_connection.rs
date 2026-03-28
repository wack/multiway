// End-to-end connection management tests for proxy-core.
//
// These tests verify HTTP/1.1 keep-alive behavior and hot config reload
// via ArcSwap through the full ProxyServer pipeline.

#[allow(dead_code)]
mod common;

use std::sync::Arc;

use common::{
    pick_free_port, send_keepalive_requests, send_request, start_backend, start_proxy, stop_proxy,
};
use proxy_core::config::{Backend, ProxyConfigBuilder, RouteConfigBuilder, RouteRuleBuilder};

// ---------------------------------------------------------------------------
// Test 25: HTTP/1.1 keep-alive: two sequential requests on same connection
// ---------------------------------------------------------------------------

#[test]
fn keepalive_two_sequential_requests() {
    // Use a backend that supports keep-alive (does NOT send Connection: close).
    let (backend_addr, backend_stop, _backend_handle) =
        common::start_backend_with_handler(|_req| {
            let body = "keepalive-ok";
            format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
        });
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "route")
                .attached_to("http")
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

    let (resp1, resp2) = send_keepalive_requests(proxy_addr, "127.0.0.1", "/first", "/second");

    assert!(
        resp1.contains("200"),
        "Expected 200 for first request, got: {resp1}"
    );
    assert!(
        resp1.contains("keepalive-ok"),
        "Expected body in first response, got: {resp1}"
    );

    assert!(
        resp2.contains("200"),
        "Expected 200 for second request, got: {resp2}"
    );
    assert!(
        resp2.contains("keepalive-ok"),
        "Expected body in second response, got: {resp2}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 26: Config update via ArcSwap is reflected in subsequent requests
// ---------------------------------------------------------------------------

#[test]
fn config_update_via_arcswap() {
    let (backend_addr, backend_stop, _backend_handle) = start_backend("after-update");
    let port = pick_free_port();

    // Start with a config that has no routes, so requests get 404.
    let initial_config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .build();

    let (config_store, shutdown, handle) = start_proxy(initial_config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    // First request: should get 404 (no routes).
    let resp1 = send_request(proxy_addr, "127.0.0.1", "/test");
    assert!(
        resp1.contains("404"),
        "Expected 404 with no routes, got: {resp1}"
    );

    // Update config to include a route to the backend.
    let updated_config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                        .build(),
                )
                .build(),
        )
        .build();

    config_store.store(Arc::new(updated_config));

    // Give the worker time to detect the config change.
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Second request: should now succeed.
    let resp2 = send_request(proxy_addr, "127.0.0.1", "/test");
    assert!(
        resp2.contains("200"),
        "Expected 200 after config update, got: {resp2}"
    );
    assert!(
        resp2.contains("after-update"),
        "Expected backend body after config update, got: {resp2}"
    );

    backend_stop.store(true, std::sync::atomic::Ordering::Release);
    stop_proxy(shutdown, handle);
}
