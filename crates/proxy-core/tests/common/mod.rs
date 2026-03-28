// Shared test harness for proxy-core E2E integration tests.
//
// Provides helpers to start backends, start the ProxyServer, send HTTP
// requests, and pick free ports. All helpers use std::net (blocking TCP)
// because ProxyServer manages its own monoio runtime internally.
//
// Backend design: each backend spawns a thread that calls blocking
// `listener.accept()` in a loop. This mirrors the pattern used in the
// existing server.rs unit tests. Backends are stopped by dropping the
// listener (which causes `accept()` to fail and the loop to exit).

use std::io::{self, Read as StdRead, Write as StdWrite};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use arc_swap::ArcSwap;
use proxy_core::config::ProxyConfig;
use proxy_core::server::ProxyServer;

// ---------------------------------------------------------------------------
// Port allocation
// ---------------------------------------------------------------------------

/// Pick an available TCP port on localhost by binding to port 0.
pub fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

// ---------------------------------------------------------------------------
// Backend helpers
// ---------------------------------------------------------------------------

/// Start a std::net backend that accepts connections in a blocking loop
/// and invokes the handler for each one.
///
/// The handler receives the raw request text and returns a full HTTP
/// response string (including status line, headers, and body).
///
/// The backend thread exits when `accept()` fails (i.e., the caller has
/// no way to stop it gracefully other than letting the listener be dropped).
/// However, we also provide a stop flag for compatibility.
pub fn start_backend_with_handler<F>(handler: F) -> (SocketAddr, Arc<AtomicBool>, JoinHandle<()>)
where
    F: Fn(&str) -> String + Send + Sync + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let stop = Arc::new(AtomicBool::new(false));

    let handler = Arc::new(handler);

    let handle = std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let handler = handler.clone();
            // Handle each connection in a separate thread to avoid blocking
            // the accept loop.
            std::thread::spawn(move || {
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap_or_default();
                stream
                    .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap_or_default();

                let mut buf = [0u8; 8192];
                let n = match stream.read(&mut buf) {
                    Ok(n) => n,
                    Err(_) => return,
                };
                let request_text = String::from_utf8_lossy(&buf[..n]).to_string();
                let response = handler(&request_text);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            });
        }
    });

    // Give the backend a moment to start accepting.
    std::thread::sleep(std::time::Duration::from_millis(50));
    (addr, stop, handle)
}

/// Start a backend that responds with the given static body and 200 OK.
/// Includes `Connection: close` in the response.
pub fn start_backend(body: &'static str) -> (SocketAddr, Arc<AtomicBool>, JoinHandle<()>) {
    start_backend_with_handler(move |_request| {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    })
}

/// Start an echo backend that includes the received request text in the
/// response body so tests can inspect what the backend actually received.
/// Useful for verifying header modifications and rewrites.
pub fn start_echo_backend() -> (SocketAddr, Arc<AtomicBool>, JoinHandle<()>) {
    start_backend_with_handler(|request| {
        let body = request.to_string();
        format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    })
}

/// Start a backend that tracks whether it was contacted.
/// Returns the socket addr, a flag indicating if the backend was contacted,
/// a stop flag, and the join handle.
pub fn start_tracking_backend(
    body: &'static str,
) -> (SocketAddr, Arc<AtomicBool>, Arc<AtomicBool>, JoinHandle<()>) {
    let contacted = Arc::new(AtomicBool::new(false));
    let contacted_clone = contacted.clone();

    let (addr, stop, handle) = start_backend_with_handler(move |_request| {
        contacted_clone.store(true, Ordering::Release);
        format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    });

    (addr, contacted, stop, handle)
}

// ---------------------------------------------------------------------------
// Proxy server helpers
// ---------------------------------------------------------------------------

/// Start a ProxyServer in a background thread. Returns the config store
/// (for hot reload), the shutdown flag, and the join handle.
/// The proxy uses 1 worker thread for test determinism.
pub fn start_proxy(
    config: ProxyConfig,
) -> (
    Arc<ArcSwap<ProxyConfig>>,
    Arc<AtomicBool>,
    JoinHandle<io::Result<()>>,
) {
    let config_store = Arc::new(ArcSwap::from_pointee(config));
    let config_clone = config_store.clone();
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    let server = ProxyServer::builder()
        .config(config_clone)
        .worker_threads(1)
        .build()
        .unwrap();

    let handle = std::thread::spawn(move || server.run(shutdown_clone));

    // Give workers time to bind listeners.
    std::thread::sleep(std::time::Duration::from_millis(300));

    (config_store, shutdown, handle)
}

/// Stop a running ProxyServer by signaling shutdown and joining the thread.
pub fn stop_proxy(shutdown: Arc<AtomicBool>, handle: JoinHandle<io::Result<()>>) {
    shutdown.store(true, Ordering::Release);
    let _ = handle.join();
}

// ---------------------------------------------------------------------------
// HTTP client helpers
// ---------------------------------------------------------------------------

/// Send a GET request to the given address and return the full response as
/// a String. Includes `Connection: close` in the request.
pub fn send_request(addr: SocketAddr, host: &str, path: &str) -> String {
    send_raw_request(
        addr,
        &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    )
}

/// Send a raw HTTP request (caller controls the full request text) and
/// return the response. Reads until the connection is closed.
pub fn send_raw_request(addr: SocketAddr, request: &str) -> String {
    let mut stream =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response
}

/// Send two requests on the same TCP connection (keep-alive). Returns both
/// responses as separate strings. The first request omits `Connection: close`
/// to keep the connection alive; the second includes it.
pub fn send_keepalive_requests(
    addr: SocketAddr,
    host: &str,
    path1: &str,
    path2: &str,
) -> (String, String) {
    let mut stream =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();

    // First request (keep-alive, no Connection: close).
    let req1 = format!("GET {path1} HTTP/1.1\r\nHost: {host}\r\n\r\n");
    stream.write_all(req1.as_bytes()).unwrap();
    stream.flush().unwrap();

    // Read first response by parsing headers + Content-Length body.
    let resp1 = read_one_response(&mut stream);

    // Second request with Connection: close.
    let req2 = format!("GET {path2} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(req2.as_bytes()).unwrap();
    stream.flush().unwrap();

    let resp2 = read_one_response(&mut stream);

    (resp1, resp2)
}

/// Read a single HTTP response from a stream by parsing headers to find
/// Content-Length, then reading exactly that many body bytes.
fn read_one_response(stream: &mut std::net::TcpStream) -> String {
    let mut all_bytes = Vec::new();
    let mut buf = [0u8; 1];

    // Read headers byte by byte until we find \r\n\r\n.
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                all_bytes.push(buf[0]);
                if all_bytes.len() >= 4 && all_bytes[all_bytes.len() - 4..] == *b"\r\n\r\n" {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let header_text = String::from_utf8_lossy(&all_bytes).to_string();

    // Parse Content-Length from headers.
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let lower = line.to_lowercase();
            if lower.starts_with("content-length:") {
                lower
                    .split(':')
                    .nth(1)
                    .and_then(|v| v.trim().parse::<usize>().ok())
            } else {
                None
            }
        })
        .unwrap_or(0);

    // Read the body.
    if content_length > 0 {
        let mut body_buf = vec![0u8; content_length];
        let mut read_so_far = 0;
        while read_so_far < content_length {
            match stream.read(&mut body_buf[read_so_far..]) {
                Ok(0) => break,
                Ok(n) => read_so_far += n,
                Err(_) => break,
            }
        }
        let body = String::from_utf8_lossy(&body_buf[..read_so_far]).to_string();
        format!("{header_text}{body}")
    } else {
        header_text
    }
}
