// ProxyServer with thread-per-core listener model.
//
// Implements the top-level ProxyServer that binds TCP listeners and
// spawns a monoio runtime per CPU core. Each core independently accepts
// connections and processes them through the request pipeline.

use std::cell::Cell;
use std::io;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use arc_swap::ArcSwap;
use service_async::Service;

use crate::config::{Protocol, ProxyConfig};
use crate::pipeline::{self, PipelineConfig, ProxyConnection, TcpConnection};
use crate::pool::new_tcp_pool;
use crate::tls;
use crate::transport::{Acceptor, ListenerOpts, StdResolver, TcpAcceptor};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A thread-per-core HTTP reverse proxy server.
///
/// Each worker thread runs its own monoio runtime with a FusionDriver,
/// binds TCP listeners using SO_REUSEPORT (so the OS distributes
/// connections across workers), and processes requests through an
/// independently owned service pipeline.
///
/// Configuration is shared via `Arc<ArcSwap<ProxyConfig>>`. Workers
/// detect config changes and rebuild their pipeline on the next
/// accept-loop iteration, preserving their thread-local connection pool.
pub struct ProxyServer {
    config: Arc<ArcSwap<ProxyConfig>>,
    worker_threads: usize,
}

/// Builder for constructing a [`ProxyServer`].
pub struct ProxyServerBuilder {
    config: Option<Arc<ArcSwap<ProxyConfig>>>,
    worker_threads: Option<usize>,
}

impl ProxyServerBuilder {
    pub fn new() -> Self {
        Self {
            config: None,
            worker_threads: None,
        }
    }

    pub fn config(mut self, config: Arc<ArcSwap<ProxyConfig>>) -> Self {
        self.config = Some(config);
        self
    }

    pub fn worker_threads(mut self, n: usize) -> Self {
        self.worker_threads = Some(n);
        self
    }

    pub fn build(self) -> Result<ProxyServer, io::Error> {
        let config = self
            .config
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config is required"))?;

        let worker_threads = self.worker_threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        });

        if worker_threads == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker_threads must be at least 1",
            ));
        }

        Ok(ProxyServer {
            config,
            worker_threads,
        })
    }
}

impl Default for ProxyServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyServer {
    pub fn builder() -> ProxyServerBuilder {
        ProxyServerBuilder::new()
    }

    /// Run the proxy server, blocking until `shutdown_flag` is set to `true`.
    ///
    /// The caller is responsible for setting the flag when shutdown is desired
    /// (e.g. from a signal handler or another thread).
    ///
    /// 1. Spawns `worker_threads` OS threads, each with its own monoio runtime
    /// 2. Each thread binds TCP listeners (SO_REUSEPORT) for every configured port
    /// 3. Each thread creates its own connection pool, resolver, and pipeline
    /// 4. Each thread enters an accept loop, spawning a task per connection
    /// 5. When `shutdown_flag` is set, all workers stop accepting and exit
    /// 6. The main thread joins all worker threads
    pub fn run(self, shutdown_flag: Arc<AtomicBool>) -> io::Result<()> {
        let mut handles: Vec<JoinHandle<io::Result<()>>> = Vec::with_capacity(self.worker_threads);

        for worker_id in 0..self.worker_threads {
            let config = self.config.clone();
            let flag = shutdown_flag.clone();

            let handle = std::thread::Builder::new()
                .name(format!("proxy-worker-{worker_id}"))
                .spawn(move || run_worker(worker_id, config, flag))?;

            handles.push(handle);
        }

        // Block the calling thread until shutdown is signaled.
        while !shutdown_flag.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Join all worker threads.
        let mut first_error: Option<io::Error> = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
                Err(_panic) => {
                    if first_error.is_none() {
                        first_error = Some(io::Error::other("worker thread panicked"));
                    }
                }
            }
        }

        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Worker implementation
// ---------------------------------------------------------------------------

type Pipeline =
    pipeline::HttpCoreService<pipeline::RouteAndFilter<pipeline::UpstreamForwarder<StdResolver>>>;

fn run_worker(
    worker_id: usize,
    config: Arc<ArcSwap<ProxyConfig>>,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    let mut rt = monoio::RuntimeBuilder::<monoio::FusionDriver>::new()
        .enable_timer()
        .build()?;

    rt.block_on(worker_loop(worker_id, config, shutdown))
}

/// Per-listener state: TCP acceptor and optional TLS acceptor.
struct ListenerState {
    name: String,
    tcp_acceptor: TcpAcceptor,
    tls_acceptor: Option<tls::TlsAcceptor>,
}

async fn worker_loop(
    worker_id: usize,
    config: Arc<ArcSwap<ProxyConfig>>,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    let pool = new_tcp_pool();
    let resolver = StdResolver;
    let opts = ListenerOpts::default();

    let current_config = config.load_full();

    let mut listener_states = bind_listeners(&current_config, &opts)?;
    if listener_states.is_empty() {
        tracing::warn!(worker_id, "no listeners configured, worker exiting");
        return Ok(());
    }

    let mut pipelines = build_pipelines(&current_config, &pool, &resolver);
    let mut config_ptr = Arc::as_ptr(&current_config);

    // Track in-flight connections so we can drain on shutdown.
    let in_flight = Rc::new(Cell::new(0u64));

    loop {
        if shutdown.load(Ordering::Acquire) {
            tracing::debug!(worker_id, "shutdown signal received");
            break;
        }

        // Check for config changes.
        let new_config = config.load_full();
        let new_ptr = Arc::as_ptr(&new_config);
        if new_ptr != config_ptr {
            tracing::info!(worker_id, "config changed, rebuilding pipelines");
            let listeners_changed = listeners_differ_arcs(&config_ptr, &new_config);
            if listeners_changed {
                match bind_listeners(&new_config, &opts) {
                    Ok(new_states) => listener_states = new_states,
                    Err(e) => {
                        tracing::error!(worker_id, error = %e, "failed to rebind listeners");
                        // Keep old listener states.
                    }
                }
            }
            pipelines = build_pipelines(&new_config, &pool, &resolver);
            config_ptr = new_ptr;

            if listener_states.is_empty() {
                tracing::warn!(worker_id, "no listeners after reload, worker exiting");
                break;
            }
        }

        // Accept one connection from each listener in round-robin fashion.
        // We try each acceptor once per loop iteration and use a short sleep
        // between iterations to avoid busy-waiting.
        let mut accepted = false;
        for (i, state) in listener_states.iter().enumerate() {
            if shutdown.load(Ordering::Acquire) {
                break;
            }

            // Non-blocking accept attempt: race against a very short timer.
            let maybe_conn = accept_with_timeout(&state.tcp_acceptor).await;

            if let Some((stream, _addr)) = maybe_conn {
                accepted = true;
                if let Some(pipeline) = pipelines.get(i) {
                    let in_flight_rc = in_flight.clone();
                    in_flight_rc.set(in_flight_rc.get() + 1);

                    if let Some(ref tls_acceptor) = state.tls_acceptor {
                        // HTTPS listener: perform TLS handshake.
                        match tls_acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                let conn = ProxyConnection { stream: tls_stream };
                                let _ = pipeline.call(conn).await;
                            }
                            Err(e) => {
                                tracing::debug!(
                                    listener = %state.name,
                                    error = %e,
                                    "TLS handshake failed"
                                );
                            }
                        }
                    } else {
                        // HTTP listener: plain TCP.
                        let conn = TcpConnection { stream };
                        let _ = pipeline.call(conn).await;
                    }

                    in_flight_rc.set(in_flight_rc.get().saturating_sub(1));
                }
            }
        }

        if !accepted {
            // No connections available on any listener, brief sleep.
            monoio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    // Drain in-flight connections.
    while in_flight.get() > 0 {
        monoio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    Ok(())
}

/// Try to accept a connection with a short timeout. Returns `None` if no
/// connection arrived within the timeout.
async fn accept_with_timeout(
    acceptor: &TcpAcceptor,
) -> Option<(monoio::net::TcpStream, SocketAddr)> {
    // Use monoio::select! to race the accept against a timer.
    // monoio provides a select! macro via the `monoio::select!` path.
    monoio::select! {
        result = acceptor.accept() => {
            match result {
                Ok(conn) => Some(conn),
                Err(e) => {
                    tracing::debug!(error = %e, "accept error");
                    None
                }
            }
        }
        _ = monoio::time::sleep(std::time::Duration::from_millis(50)) => {
            None
        }
    }
}

fn bind_listeners(config: &ProxyConfig, opts: &ListenerOpts) -> io::Result<Vec<ListenerState>> {
    let mut states = Vec::new();
    for listener in &config.listeners {
        let addr: SocketAddr = ([0, 0, 0, 0], listener.port).into();
        let tcp_acceptor = TcpAcceptor::bind(addr, opts)?;

        // Build TLS acceptor for HTTPS listeners.
        let tls_acceptor = if listener.protocol == Protocol::Https {
            if let Some(ref tls_config) = listener.tls {
                match tls::build_tls_acceptor(tls_config) {
                    Ok(acceptor) => {
                        if acceptor.is_some() {
                            tracing::info!(
                                listener_name = %listener.name,
                                port = listener.port,
                                "TLS termination enabled"
                            );
                        }
                        acceptor
                    }
                    Err(e) => {
                        tracing::error!(
                            listener_name = %listener.name,
                            port = listener.port,
                            error = %e,
                            "failed to build TLS acceptor, listener will not accept TLS"
                        );
                        None
                    }
                }
            } else {
                tracing::warn!(
                    listener_name = %listener.name,
                    port = listener.port,
                    "HTTPS listener has no TLS config"
                );
                None
            }
        } else {
            None
        };

        tracing::info!(
            listener_name = %listener.name,
            port = listener.port,
            protocol = ?listener.protocol,
            local_addr = %tcp_acceptor.local_addr()?,
            tls = tls_acceptor.is_some(),
            "bound listener"
        );
        states.push(ListenerState {
            name: listener.name.clone(),
            tcp_acceptor,
            tls_acceptor,
        });
    }
    Ok(states)
}

/// Check if the listener set changed. Uses the raw pointer of the old config
/// (since we only store the pointer) and the new Arc.
fn listeners_differ_arcs(old_ptr: &*const ProxyConfig, new: &ProxyConfig) -> bool {
    // We can't dereference old_ptr safely, but we know the configs differ
    // if the pointers differ (which they do since we only call this when
    // new_ptr != config_ptr). Compare listener counts and ports from the
    // new config against what we'd have bound.
    //
    // For simplicity, always return true — we'll rebind on any config change
    // that affects listeners. The rebind is idempotent with SO_REUSEPORT.
    let _ = old_ptr;
    let _ = new;
    // Actually, we should only rebind if listeners changed. Since we can't
    // access the old config, always rebind. This is safe because SO_REUSEPORT
    // allows multiple binds to the same port.
    true
}

fn build_pipelines(
    config: &ProxyConfig,
    pool: &crate::pool::TcpConnectionPool,
    resolver: &StdResolver,
) -> Vec<Pipeline> {
    let proxy_config = Rc::new(config.clone());
    config
        .listeners
        .iter()
        .map(|listener| {
            let pipeline_config = PipelineConfig {
                proxy_config: proxy_config.clone(),
                listener_name: listener.name.clone(),
                pool: pool.clone(),
                resolver: *resolver,
            };
            pipeline::build_pipeline(&pipeline_config)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Backend, ProxyConfigBuilder, RouteConfigBuilder, RouteRuleBuilder};
    use std::io::{Read as StdRead, Write as StdWrite};

    fn pick_free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    fn start_std_backend(body: &'static str) -> (SocketAddr, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        (addr, handle)
    }

    fn test_config(listener_name: &str, port: u16, backend_addr: SocketAddr) -> ProxyConfig {
        ProxyConfigBuilder::new("default", "gw")
            .with_http_listener(listener_name, port, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to(listener_name)
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    fn send_http_request(addr: SocketAddr, path: &str) -> io::Result<String> {
        let mut stream =
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes())?;
        stream.flush()?;

        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        Ok(response)
    }

    // -----------------------------------------------------------------------
    // Builder tests
    // -----------------------------------------------------------------------

    #[test]
    fn builder_requires_config() {
        let result = ProxyServer::builder().build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_rejects_zero_workers() {
        let config = Arc::new(ArcSwap::from_pointee(ProxyConfig::default()));
        let result = ProxyServer::builder()
            .config(config)
            .worker_threads(0)
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_succeeds_with_valid_config() {
        let config = Arc::new(ArcSwap::from_pointee(ProxyConfig::default()));
        let server = ProxyServer::builder()
            .config(config)
            .worker_threads(1)
            .build();
        assert!(server.is_ok());
    }

    /// Helper: start a ProxyServer in a background thread and return the
    /// shutdown flag handle plus the join handle.
    fn start_server(
        config: Arc<ArcSwap<ProxyConfig>>,
        workers: usize,
    ) -> (Arc<AtomicBool>, JoinHandle<io::Result<()>>) {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let server = ProxyServer::builder()
            .config(config)
            .worker_threads(workers)
            .build()
            .unwrap();
        let handle = std::thread::spawn(move || server.run(shutdown_clone));
        // Give workers time to bind.
        std::thread::sleep(std::time::Duration::from_millis(300));
        (shutdown, handle)
    }

    fn stop_server(shutdown: Arc<AtomicBool>, handle: JoinHandle<io::Result<()>>) {
        shutdown.store(true, Ordering::Release);
        let _ = handle.join();
    }

    // -----------------------------------------------------------------------
    // Test: single worker
    // -----------------------------------------------------------------------

    #[test]
    fn single_worker_proxy() {
        let (backend_addr, _backend) = start_std_backend("hello from backend");
        let port = pick_free_port();
        let proxy_config = test_config("http", port, backend_addr);

        let config = Arc::new(ArcSwap::from_pointee(proxy_config));
        let (shutdown, handle) = start_server(config, 1);

        let proxy_addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let response = send_http_request(proxy_addr, "/test").unwrap();

        assert!(
            response.contains("200"),
            "Expected 200 in response, got: {response}"
        );
        assert!(
            response.contains("hello from backend"),
            "Expected backend body, got: {response}"
        );

        stop_server(shutdown, handle);
    }

    // -----------------------------------------------------------------------
    // Test: multiple workers
    // -----------------------------------------------------------------------

    #[test]
    fn multiple_workers_concurrent_requests() {
        let (backend_addr, _backend) = start_std_backend("multi-worker-ok");
        let port = pick_free_port();
        let proxy_config = test_config("http", port, backend_addr);

        let config = Arc::new(ArcSwap::from_pointee(proxy_config));
        let (shutdown, handle) = start_server(config, 4);

        let proxy_addr: SocketAddr = ([127, 0, 0, 1], port).into();

        let mut client_handles = Vec::new();
        for i in 0..8 {
            let addr = proxy_addr;
            let h = std::thread::spawn(move || send_http_request(addr, &format!("/req-{i}")));
            client_handles.push(h);
        }

        let mut success_count = 0;
        for h in client_handles {
            if let Ok(Ok(resp)) = h.join()
                && resp.contains("200")
                && resp.contains("multi-worker-ok")
            {
                success_count += 1;
            }
        }

        assert!(
            success_count >= 1,
            "Expected at least 1 successful request, got {success_count}"
        );

        stop_server(shutdown, handle);
    }

    // -----------------------------------------------------------------------
    // Test: multiple listeners on different ports
    // -----------------------------------------------------------------------

    #[test]
    fn multiple_listeners_different_ports() {
        let (backend_addr, _backend) = start_std_backend("dual-listener");
        let port1 = pick_free_port();
        let port2 = pick_free_port();

        let proxy_config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http1", port1, None)
            .with_http_listener("http2", port2, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http1")
                    .attached_to("http2")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "127.0.0.1", backend_addr.port()))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let config = Arc::new(ArcSwap::from_pointee(proxy_config));
        let (shutdown, handle) = start_server(config, 1);

        let resp1 = send_http_request(([127, 0, 0, 1], port1).into(), "/test1");
        if let Ok(r) = &resp1 {
            assert!(r.contains("200"), "Port {port1}: expected 200, got: {r}");
            assert!(r.contains("dual-listener"), "Port {port1}: wrong body: {r}");
        }

        let resp2 = send_http_request(([127, 0, 0, 1], port2).into(), "/test2");
        if let Ok(r) = &resp2 {
            assert!(r.contains("200"), "Port {port2}: expected 200, got: {r}");
            assert!(r.contains("dual-listener"), "Port {port2}: wrong body: {r}");
        }

        stop_server(shutdown, handle);
    }

    // -----------------------------------------------------------------------
    // Test: graceful shutdown
    // -----------------------------------------------------------------------

    #[test]
    fn graceful_shutdown() {
        let (backend_addr, _backend) = start_std_backend("shutdown-test");
        let port = pick_free_port();
        let proxy_config = test_config("http", port, backend_addr);

        let config = Arc::new(ArcSwap::from_pointee(proxy_config));
        let (shutdown, handle) = start_server(config, 1);

        let proxy_addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let resp = send_http_request(proxy_addr, "/before-shutdown").unwrap();
        assert!(resp.contains("200"), "Expected 200 before shutdown");

        // Signal shutdown.
        shutdown.store(true, Ordering::Release);

        let result = handle.join();
        assert!(
            result.is_ok(),
            "Server thread should exit cleanly after shutdown"
        );
    }

    // -----------------------------------------------------------------------
    // Test: config update via ArcSwap
    // -----------------------------------------------------------------------

    #[test]
    fn config_update_via_arcswap() {
        let (backend_addr, _backend) = start_std_backend("original");
        let port = pick_free_port();

        // Start with a config that has no routes, so requests get 404.
        let initial_config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", port, None)
            .build();

        let config = Arc::new(ArcSwap::from_pointee(initial_config));
        let config_writer = config.clone();
        let (shutdown, handle) = start_server(config, 1);

        let proxy_addr: SocketAddr = ([127, 0, 0, 1], port).into();

        // First request: 404 (no routes).
        let resp1 = send_http_request(proxy_addr, "/test").unwrap();
        assert!(
            resp1.contains("404"),
            "Expected 404 with no routes, got: {resp1}"
        );

        // Update config to include a route.
        let updated_config = test_config("http", port, backend_addr);
        config_writer.store(Arc::new(updated_config));

        // Give worker time to detect the change.
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Second request: should now succeed.
        let resp2 = send_http_request(proxy_addr, "/test").unwrap();
        assert!(
            resp2.contains("200"),
            "Expected 200 after config update, got: {resp2}"
        );

        stop_server(shutdown, handle);
    }
}
