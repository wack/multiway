// End-to-end error handling tests for proxy-core.
//
// These tests verify that the proxy returns correct error responses when
// backends are unreachable or routes have no backends.

#[allow(dead_code)]
mod common;

use common::{pick_free_port, send_request, start_proxy, stop_proxy};
use proxy_core::config::{Backend, ProxyConfigBuilder, RouteConfigBuilder, RouteRuleBuilder};

// ---------------------------------------------------------------------------
// Test 23: Backend unreachable returns 502 Bad Gateway
// ---------------------------------------------------------------------------

#[test]
fn backend_unreachable_returns_502() {
    let port = pick_free_port();

    // Use a port that nothing is listening on. Port 1 is typically unused
    // and not accessible to non-root users on most systems.
    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        .with_backend(Backend::new("default", "127.0.0.1", 1))
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "127.0.0.1", "/test");

    assert!(
        resp.contains("502"),
        "Expected 502 Bad Gateway for unreachable backend, got: {resp}"
    );

    stop_proxy(shutdown, handle);
}

// ---------------------------------------------------------------------------
// Test 24: Route with no backends returns 503 Service Unavailable
// ---------------------------------------------------------------------------

#[test]
fn route_with_no_backends_returns_503() {
    let port = pick_free_port();

    let config = ProxyConfigBuilder::new("default", "gw")
        .with_http_listener("http", port, None)
        .with_route(
            RouteConfigBuilder::new("default", "route")
                .attached_to("http")
                .with_rule(
                    RouteRuleBuilder::new()
                        .with_path_prefix("/")
                        // No backends added.
                        .build(),
                )
                .build(),
        )
        .build();

    let (_config_store, shutdown, handle) = start_proxy(config);
    let proxy_addr = ([127, 0, 0, 1], port).into();

    let resp = send_request(proxy_addr, "127.0.0.1", "/test");

    assert!(
        resp.contains("503"),
        "Expected 503 Service Unavailable for route with no backends, got: {resp}"
    );

    stop_proxy(shutdown, handle);
}
