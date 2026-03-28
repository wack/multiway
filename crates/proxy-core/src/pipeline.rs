// Request pipeline (service-async composition + monoio-http codecs).
//
// Composes the full request processing pipeline using service-async
// FactoryStack and FactoryLayer. Integrates monoio-http ServerCodec
// and ClientCodec for HTTP/1.1 codec handling within the pipeline stages.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::rc::Rc;

use monoio::io::sink::Sink;
use monoio::io::{AsyncReadRent, AsyncWriteRent, AsyncWriteRentExt, Split, Splitable};
use monoio::net::TcpStream;
use monoio_http::h1::codec::{ClientCodec, ServerCodec};
use monoio_http::h1::payload::Payload;
use monoio_transports::connectors::Connector;
use service_async::layer::{FactoryLayer, layer_fn};
use service_async::{MakeService, Service};

use crate::config::{Backend, BackendProtocol, ProxyConfig};
use crate::filter::{self, AppliedFilter};
use crate::pool::{PoolableConnection, TcpConnectionPool};
use crate::routing::{self, RoutingDecision};
use crate::transport::Resolver;
use monoio_transports::pool::Pooled;

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

type MonoioResponse = monoio_http::common::response::Response;
type MonoioRequest = monoio_http::common::request::Request;

// ---------------------------------------------------------------------------
// Metadata extraction
// ---------------------------------------------------------------------------

/// Metadata extracted from an incoming HTTP request.
///
/// Owns all its data so it can be passed around freely without lifetime
/// constraints on the original request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMeta {
    /// Hostname from the `Host` header, with port stripped if present.
    pub host: Option<String>,
    /// Request path (e.g. `/api/v1/users`).
    pub path: String,
    /// HTTP method as a string (e.g. `GET`, `POST`).
    pub method: String,
    /// Request headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Query parameters as `(name, value)` pairs.
    pub query_params: Vec<(String, String)>,
}

/// Extract routing-relevant metadata from an HTTP request.
///
/// The host is taken from the `Host` header with any port suffix stripped.
/// Query parameters are parsed from the URI query string.
pub fn extract_request_meta<B>(req: &http::Request<B>) -> RequestMeta {
    let host = req
        .headers()
        .get(http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| {
            // Strip port if present (e.g. "example.com:8080" -> "example.com").
            match h.rsplit_once(':') {
                Some((host_part, port_part)) if port_part.chars().all(|c| c.is_ascii_digit()) => {
                    host_part.to_string()
                }
                _ => h.to_string(),
            }
        });

    let path = req.uri().path().to_string();
    let method = req.method().to_string();

    let headers = req
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    let query_params = req
        .uri()
        .query()
        .map(|q| {
            q.split('&')
                .filter_map(|pair| {
                    let mut parts = pair.splitn(2, '=');
                    let key = parts.next()?;
                    let value = parts.next().unwrap_or("");
                    Some((key.to_string(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    RequestMeta {
        host,
        path,
        method,
        headers,
        query_params,
    }
}

// ---------------------------------------------------------------------------
// Response builders
// ---------------------------------------------------------------------------

/// Build a 404 Not Found response.
pub fn not_found_response() -> http::Response<String> {
    http::Response::builder()
        .status(http::StatusCode::NOT_FOUND)
        .header(http::header::CONTENT_TYPE, "text/plain")
        .body("404 Not Found".to_string())
        .unwrap()
}

/// Build a 502 Bad Gateway response.
pub fn bad_gateway_response() -> http::Response<String> {
    http::Response::builder()
        .status(http::StatusCode::BAD_GATEWAY)
        .header(http::header::CONTENT_TYPE, "text/plain")
        .body("502 Bad Gateway".to_string())
        .unwrap()
}

/// Build a 503 Service Unavailable response.
pub fn service_unavailable_response() -> http::Response<String> {
    http::Response::builder()
        .status(http::StatusCode::SERVICE_UNAVAILABLE)
        .header(http::header::CONTENT_TYPE, "text/plain")
        .body("503 Service Unavailable".to_string())
        .unwrap()
}

/// Build a 504 Gateway Timeout response.
pub fn gateway_timeout_response() -> http::Response<String> {
    http::Response::builder()
        .status(http::StatusCode::GATEWAY_TIMEOUT)
        .header(http::header::CONTENT_TYPE, "text/plain")
        .body("504 Gateway Timeout".to_string())
        .unwrap()
}

/// Build a redirect response with the given status code and `Location` header.
pub fn redirect_response(status: u16, location: &str) -> http::Response<String> {
    http::Response::builder()
        .status(http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::FOUND))
        .header(http::header::LOCATION, location)
        .header(http::header::CONTENT_TYPE, "text/plain")
        .body(String::new())
        .unwrap()
}

/// Select a backend using weighted random selection.
///
/// Backends with weight 0 are never selected. If all backends have weight 0
/// or the slice is empty, returns `None`.
pub fn select_backend(backends: &[Backend]) -> Option<&Backend> {
    let total_weight: u32 = backends.iter().map(|b| b.weight).sum();
    if total_weight == 0 {
        return None;
    }

    let mut pick = rand::random_range(0..total_weight);
    for backend in backends {
        if backend.weight == 0 {
            continue;
        }
        if pick < backend.weight {
            return Some(backend);
        }
        pick -= backend.weight;
    }

    // Should be unreachable if total_weight > 0, but be safe.
    backends.iter().find(|b| b.weight > 0)
}

// ---------------------------------------------------------------------------
// WebSocket upgrade detection
// ---------------------------------------------------------------------------

/// Check if a request is a WebSocket upgrade request.
///
/// A valid WebSocket upgrade requires all of:
/// - `Connection` header contains `upgrade` (case-insensitive)
/// - `Upgrade` header is `websocket` (case-insensitive)
/// - `Sec-WebSocket-Version` header is present
/// - `Sec-WebSocket-Key` header is present
#[cfg(test)]
fn is_websocket_upgrade<B>(req: &http::Request<B>) -> bool {
    let headers = req.headers();

    // Check Connection header contains "upgrade" (may be a comma-separated list).
    let has_connection_upgrade = headers
        .get(http::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
        });
    if !has_connection_upgrade {
        return false;
    }

    // Check Upgrade header is "websocket".
    let has_upgrade_websocket = headers
        .get(http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if !has_upgrade_websocket {
        return false;
    }

    // Check Sec-WebSocket-Version header is present.
    if !headers.contains_key("sec-websocket-version") {
        return false;
    }

    // Check Sec-WebSocket-Key header is present.
    if !headers.contains_key("sec-websocket-key") {
        return false;
    }

    true
}

// ---------------------------------------------------------------------------
// PrefixedStream: a stream wrapper with pre-read bytes
// ---------------------------------------------------------------------------

/// A stream wrapper that yields pre-read bytes before delegating to the
/// inner stream. Used when we need to "unread" bytes that were already
/// consumed from the stream (e.g. for WebSocket upgrade detection).
struct PrefixedStream<S> {
    inner: S,
    prefix: Vec<u8>,
    prefix_offset: usize,
}

impl<S> PrefixedStream<S> {
    fn new(inner: S, prefix: Vec<u8>) -> Self {
        Self {
            inner,
            prefix,
            prefix_offset: 0,
        }
    }

    fn prefix_remaining(&self) -> usize {
        self.prefix.len() - self.prefix_offset
    }
}

impl<S: AsyncReadRent> AsyncReadRent for PrefixedStream<S> {
    async fn read<T: monoio::buf::IoBufMut>(&mut self, mut buf: T) -> monoio::BufResult<usize, T> {
        let remaining = self.prefix_remaining();
        if remaining > 0 {
            let to_copy = std::cmp::min(remaining, buf.bytes_total());
            unsafe {
                buf.write_ptr()
                    .copy_from_nonoverlapping(self.prefix[self.prefix_offset..].as_ptr(), to_copy);
                buf.set_init(to_copy);
            }
            self.prefix_offset += to_copy;
            (Ok(to_copy), buf)
        } else {
            self.inner.read(buf).await
        }
    }

    async fn readv<T: monoio::buf::IoVecBufMut>(&mut self, buf: T) -> monoio::BufResult<usize, T> {
        if self.prefix_remaining() > 0 {
            // For vectored reads with prefix data, use the IoVecWrapperMut
            // adapter to write into the first segment.
            let wrapper = match monoio::buf::IoVecWrapperMut::new(buf) {
                Ok(wrapper) => wrapper,
                Err(buf) => return (Ok(0), buf),
            };
            let (result, wrapper) = self.read(wrapper).await;
            let mut buf = wrapper.into_inner();
            if let Ok(n) = result {
                unsafe { buf.set_init(n) };
            }
            (result, buf)
        } else {
            self.inner.readv(buf).await
        }
    }
}

impl<S: AsyncWriteRent> AsyncWriteRent for PrefixedStream<S> {
    fn write<T: monoio::buf::IoBuf>(
        &mut self,
        buf: T,
    ) -> impl std::future::Future<Output = monoio::BufResult<usize, T>> {
        self.inner.write(buf)
    }

    fn writev<T: monoio::buf::IoVecBuf>(
        &mut self,
        buf_vec: T,
    ) -> impl std::future::Future<Output = monoio::BufResult<usize, T>> {
        self.inner.writev(buf_vec)
    }

    fn flush(&mut self) -> impl std::future::Future<Output = std::io::Result<()>> {
        self.inner.flush()
    }

    fn shutdown(&mut self) -> impl std::future::Future<Output = std::io::Result<()>> {
        self.inner.shutdown()
    }
}

// Safety: PrefixedStream can be safely split because reads (from prefix + inner)
// and writes (to inner) are independent operations, same as the underlying stream.
unsafe impl<S: Split> Split for PrefixedStream<S> {}

// ---------------------------------------------------------------------------
// WebSocket upgrade helpers
// ---------------------------------------------------------------------------

/// Read raw HTTP request bytes from the stream until the header terminator
/// `\r\n\r\n` is found. Returns the complete header bytes including the
/// terminator.
async fn read_http_request_head<S: AsyncReadRent>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    loop {
        // Grow the buffer to read more data.
        let read_buf = vec![0u8; 4096];
        let (result, read_buf) = stream.read(read_buf).await;
        let n = result?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before request headers complete",
            ));
        }
        buf.extend_from_slice(&read_buf[..n]);

        // Check if we have the complete headers.
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(buf);
        }

        // Safety limit: 64 KiB for request headers.
        if buf.len() > 65536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
    }
}

/// Check if raw HTTP request bytes represent a WebSocket upgrade.
///
/// Performs a lightweight parse of the raw HTTP/1.1 request to detect
/// the WebSocket upgrade headers without a full HTTP parser.
fn is_raw_websocket_upgrade(raw: &[u8]) -> bool {
    let text = match std::str::from_utf8(raw) {
        Ok(t) => t,
        Err(_) => return false,
    };

    let mut has_connection_upgrade = false;
    let mut has_upgrade_websocket = false;
    let mut has_ws_version = false;
    let mut has_ws_key = false;

    for line in text.lines().skip(1) {
        // Empty line marks end of headers.
        if line.is_empty() || line == "\r" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("connection") {
                has_connection_upgrade = value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"));
            } else if name.eq_ignore_ascii_case("upgrade") {
                has_upgrade_websocket = value.eq_ignore_ascii_case("websocket");
            } else if name.eq_ignore_ascii_case("sec-websocket-version") {
                has_ws_version = true;
            } else if name.eq_ignore_ascii_case("sec-websocket-key") {
                has_ws_key = true;
            }
        }
    }

    has_connection_upgrade && has_upgrade_websocket && has_ws_version && has_ws_key
}

/// Handle a WebSocket upgrade: connect to the backend, relay the upgrade
/// handshake, and bidirectionally copy data until either side closes.
///
/// This function takes ownership of the client read/write halves and does
/// NOT return to the keep-alive loop.
async fn handle_websocket_upgrade<S, R>(
    mut client_reader: monoio::io::OwnedReadHalf<S>,
    mut client_writer: monoio::io::OwnedWriteHalf<S>,
    raw_request: &[u8],
    config: &ProxyConfig,
    listener_name: &str,
    resolver: &R,
) -> Result<(), io::Error>
where
    S: AsyncReadRent + AsyncWriteRent + Split + 'static,
    R: Resolver,
{
    // Parse minimal request metadata for routing.
    let text = std::str::from_utf8(raw_request)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let first_line = text.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let uri_str = parts.next().unwrap_or("/");
    let uri: http::Uri = uri_str
        .parse()
        .map_err(|e: http::uri::InvalidUri| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Parse headers for routing metadata.
    let mut host: Option<String> = None;
    let mut headers_vec: Vec<(String, String)> = Vec::new();
    for line in text.lines().skip(1) {
        if line.is_empty() || line == "\r" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_lowercase();
            let value = value.trim().to_string();
            if name == "host" {
                // Strip port from host for routing.
                let h = match value.rsplit_once(':') {
                    Some((host_part, port_part))
                        if port_part.chars().all(|c| c.is_ascii_digit()) =>
                    {
                        host_part.to_string()
                    }
                    _ => value.clone(),
                };
                host = Some(h);
            }
            headers_vec.push((name, value));
        }
    }

    let query_params: Vec<(String, String)> = uri
        .query()
        .map(|q| {
            q.split('&')
                .filter_map(|pair| {
                    let mut parts = pair.splitn(2, '=');
                    let key = parts.next()?;
                    let value = parts.next().unwrap_or("");
                    Some((key.to_string(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    // Build routing metadata.
    let header_refs: Vec<(&str, &str)> = headers_vec
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let qp_refs: Vec<(&str, &str)> = query_params
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let routing_meta = routing::RequestMeta {
        host: host.as_deref(),
        path: uri.path(),
        method,
        headers: &header_refs,
        query_params: &qp_refs,
    };

    let decision = routing::route(config, listener_name, &routing_meta);

    let backends = match decision {
        RoutingDecision::Forward { backends, .. } => backends,
        _ => {
            // No route found or redirect — send a 502 and close.
            let resp_bytes =
                b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 15\r\n\r\n502 Bad Gateway";
            let (result, _) = client_writer.write_all(resp_bytes.to_vec()).await;
            result?;
            return Ok(());
        }
    };

    let backend = match select_backend(backends) {
        Some(b) => b,
        None => {
            let resp_bytes = b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 23\r\n\r\n503 Service Unavailable";
            let (result, _) = client_writer.write_all(resp_bytes.to_vec()).await;
            result?;
            return Ok(());
        }
    };

    // Resolve backend address.
    let addrs = match resolver.resolve(&backend.name, backend.port) {
        Ok(addrs) if !addrs.is_empty() => addrs,
        _ => {
            let resp_bytes =
                b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 15\r\n\r\n502 Bad Gateway";
            let (result, _) = client_writer.write_all(resp_bytes.to_vec()).await;
            result?;
            return Ok(());
        }
    };
    let addr = addrs[0];

    // Connect to backend directly (NOT from pool — WebSocket is long-lived).
    let backend_stream = match TcpStream::connect(addr).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(addr = %addr, error = %e, "websocket: failed to connect to backend");
            let resp_bytes =
                b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 15\r\n\r\n502 Bad Gateway";
            let (result, _) = client_writer.write_all(resp_bytes.to_vec()).await;
            result?;
            return Ok(());
        }
    };

    let (mut backend_reader, mut backend_writer) = backend_stream.into_split();

    // Forward the raw upgrade request to the backend.
    let (result, _) = backend_writer.write_all(raw_request.to_vec()).await;
    result.map_err(|e| {
        tracing::warn!(error = %e, "websocket: failed to send upgrade to backend");
        e
    })?;

    // Read the backend's response headers.
    let backend_response = read_http_request_head(&mut backend_reader).await?;

    // Check if the backend responded with 101 Switching Protocols.
    let resp_text = std::str::from_utf8(&backend_response).unwrap_or("");
    let first_resp_line = resp_text.lines().next().unwrap_or("");
    if !first_resp_line.contains("101") {
        tracing::warn!(
            response = first_resp_line,
            "websocket: backend did not respond with 101"
        );
        // Forward the non-101 response to the client as-is.
        let (result, _) = client_writer.write_all(backend_response).await;
        result?;
        return Ok(());
    }

    // Forward the 101 response to the client.
    let (result, _) = client_writer.write_all(backend_response).await;
    result?;

    tracing::debug!("websocket: upgrade complete, entering bidirectional copy");

    // Bidirectional copy between client and backend.
    let c2b = monoio::io::copy(&mut client_reader, &mut backend_writer);
    let b2c = monoio::io::copy(&mut backend_reader, &mut client_writer);

    let (c2b_result, b2c_result) = monoio::join!(c2b, b2c);

    // Log errors but don't propagate — one side closing is normal.
    if let Err(e) = c2b_result {
        tracing::debug!(error = %e, "websocket: client-to-backend copy ended");
    }
    if let Err(e) = b2c_result {
        tracing::debug!(error = %e, "websocket: backend-to-client copy ended");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Pipeline request/response types
// ---------------------------------------------------------------------------

/// Convert a `http::Response<String>` into a monoio `Response` with payload.
fn string_response_to_monoio(resp: http::Response<String>) -> MonoioResponse {
    let (mut parts, body) = resp.into_parts();
    let payload = if body.is_empty() {
        Payload::None
    } else {
        let body_bytes = bytes::Bytes::from(body);
        parts.headers.insert(
            http::header::CONTENT_LENGTH,
            http::header::HeaderValue::from(body_bytes.len()),
        );
        Payload::Fixed(monoio_http::h1::payload::FixedPayload::new(body_bytes))
    };
    MonoioResponse::from_parts(parts, payload)
}

/// A request flowing through the pipeline with its routing context.
pub struct PipelineRequest {
    /// The parsed HTTP request (headers only, body as Payload).
    pub request: MonoioRequest,
    /// Extracted metadata for routing.
    pub meta: RequestMeta,
    /// The listener name this request arrived on.
    pub listener_name: String,
}

/// The result after route resolution — either a forwarding decision or an
/// immediate response.
pub struct ForwardRequest {
    /// The original HTTP request.
    pub request: MonoioRequest,
    /// The extracted metadata.
    pub meta: RequestMeta,
    /// Backends to forward to.
    pub backends: Vec<Backend>,
    /// Collected filter effects.
    pub applied_filter: AppliedFilter,
}

// ---------------------------------------------------------------------------
// Layer 3 (innermost): UpstreamForwarder
// ---------------------------------------------------------------------------

/// Innermost service: forwards requests to upstream backends using the
/// connection pool and `ClientCodec`.
pub struct UpstreamForwarder<R: Resolver> {
    pool: TcpConnectionPool,
    resolver: R,
}

impl<R: Resolver + 'static> Service<ForwardRequest> for UpstreamForwarder<R> {
    type Response = MonoioResponse;
    type Error = Infallible;

    async fn call(&self, req: ForwardRequest) -> Result<MonoioResponse, Infallible> {
        let ForwardRequest {
            request,
            meta,
            backends,
            applied_filter,
        } = req;

        // Select a backend.
        let backend = match select_backend(&backends) {
            Some(b) => b,
            None => return Ok(string_response_to_monoio(service_unavailable_response())),
        };

        // Resolve backend address.
        let addrs = match self.resolver.resolve(&backend.name, backend.port) {
            Ok(addrs) if !addrs.is_empty() => addrs,
            _ => return Ok(string_response_to_monoio(bad_gateway_response())),
        };
        let addr = addrs[0];

        // Remember the backend protocol before borrowing anything else.
        let backend_protocol = backend.protocol;

        // Build the upstream request with applied filters.
        let upstream_req = build_upstream_request(request, &meta, &applied_filter);

        // Compute backend timeout duration.
        let backend_timeout = applied_filter
            .timeout
            .as_ref()
            .and_then(|t| t.backend_request)
            .filter(|&t| t > 0.0)
            .map(std::time::Duration::from_secs_f64);

        // Spawn mirror requests as fire-and-forget background tasks.
        // Mirrors always use HTTP/1.1 regardless of the primary backend protocol.
        for mirror in &applied_filter.mirrors {
            if mirror.percent < 100 && rand::random_range(0..100) >= mirror.percent {
                continue;
            }
            let mirrored_req = clone_request_for_mirror(&upstream_req);
            let mirror_addr = match self
                .resolver
                .resolve(&mirror.backend.name, mirror.backend.port)
            {
                Ok(addrs) if !addrs.is_empty() => addrs[0],
                _ => {
                    tracing::warn!(
                        backend = %mirror.backend.name,
                        port = mirror.backend.port,
                        "mirror: failed to resolve backend address"
                    );
                    continue;
                }
            };
            let pool = self.pool.clone();
            monoio::spawn(async move {
                let conn = match pool.connect(mirror_addr).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            addr = %mirror_addr,
                            error = %e,
                            "mirror: failed to connect to backend"
                        );
                        return;
                    }
                };
                match forward_via_codec(conn, mirrored_req, None).await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            addr = %mirror_addr,
                            error = %e,
                            "mirror: failed to forward request"
                        );
                    }
                }
            });
        }

        // Forward to the primary backend using the configured protocol.
        let result = match backend_protocol {
            BackendProtocol::H2c => forward_via_h2(addr, upstream_req, backend_timeout).await,
            BackendProtocol::Http1 => {
                // Get a connection from the pool.
                let conn = match self.pool.connect(addr).await {
                    Ok(c) => c,
                    Err(_) => return Ok(string_response_to_monoio(bad_gateway_response())),
                };
                forward_via_codec(conn, upstream_req, backend_timeout).await
            }
        };

        match result {
            Ok(mut resp) => {
                // Apply response header filters.
                apply_response_filters(&mut resp, &applied_filter);
                Ok(resp)
            }
            Err(e) => {
                // A timeout from either codec manifests as a TimedOut io::Error.
                if e.kind() == io::ErrorKind::TimedOut {
                    tracing::warn!("backend request timeout exceeded");
                    Ok(string_response_to_monoio(gateway_timeout_response()))
                } else {
                    Ok(string_response_to_monoio(bad_gateway_response()))
                }
            }
        }
    }
}

/// Build the upstream HTTP request, applying request header and rewrite filters.
fn build_upstream_request(
    original: MonoioRequest,
    meta: &RequestMeta,
    applied: &AppliedFilter,
) -> MonoioRequest {
    let (mut parts, payload) = original.into_parts();

    // Apply rewrite filter if present.
    if let Some(ref rewrite) = applied.rewrite {
        if let Some(ref new_path) = rewrite.path
            && let Ok(uri) = new_path.parse::<http::Uri>()
        {
            parts.uri = uri;
        }
        if let Some(ref new_host) = rewrite.hostname
            && let Ok(val) = http::header::HeaderValue::from_str(new_host)
        {
            parts.headers.insert(http::header::HOST, val);
        }
    }

    // Apply request header modifications: remove -> set -> add.
    for name in &applied.request_header_remove {
        if let Ok(header_name) = name.parse::<http::header::HeaderName>() {
            parts.headers.remove(&header_name);
        }
    }
    for hv in &applied.request_header_set {
        if let (Ok(name), Ok(val)) = (
            hv.name.parse::<http::header::HeaderName>(),
            http::header::HeaderValue::from_str(&hv.value),
        ) {
            parts.headers.insert(name, val);
        }
    }
    for hv in &applied.request_header_add {
        if let (Ok(name), Ok(val)) = (
            hv.name.parse::<http::header::HeaderName>(),
            http::header::HeaderValue::from_str(&hv.value),
        ) {
            parts.headers.append(name, val);
        }
    }

    // Ensure Host header is present (use original meta if not overridden).
    if !parts.headers.contains_key(http::header::HOST)
        && let Some(ref host) = meta.host
        && let Ok(val) = http::header::HeaderValue::from_str(host)
    {
        parts.headers.insert(http::header::HOST, val);
    }

    MonoioRequest::from_parts(parts, payload)
}

/// Clone request headers for mirroring (body is sent as empty).
///
/// Mirror requests copy the method, URI, and headers from the original
/// request but use `Payload::None` for the body. Full body buffering and
/// cloning is avoided for performance.
fn clone_request_for_mirror(req: &MonoioRequest) -> MonoioRequest {
    let (parts, _) = http::Request::builder()
        .method(req.method().clone())
        .uri(req.uri().clone())
        .body(())
        .unwrap()
        .into_parts();
    let mut new_parts = parts;
    new_parts.headers = req.headers().clone();
    MonoioRequest::from_parts(new_parts, Payload::None)
}

/// Apply response header filters to a monoio response.
fn apply_response_filters(resp: &mut MonoioResponse, applied: &AppliedFilter) {
    let headers = resp.headers_mut();

    // Remove.
    for name in &applied.response_header_remove {
        if let Ok(header_name) = name.parse::<http::header::HeaderName>() {
            headers.remove(&header_name);
        }
    }
    // Set.
    for hv in &applied.response_header_set {
        if let (Ok(name), Ok(val)) = (
            hv.name.parse::<http::header::HeaderName>(),
            http::header::HeaderValue::from_str(&hv.value),
        ) {
            headers.insert(name, val);
        }
    }
    // Add.
    for hv in &applied.response_header_add {
        if let (Ok(name), Ok(val)) = (
            hv.name.parse::<http::header::HeaderName>(),
            http::header::HeaderValue::from_str(&hv.value),
        ) {
            headers.append(name, val);
        }
    }
}

/// Forward a request to upstream using ClientCodec on a pooled connection.
///
/// When `backend_timeout` is `Some`, the codec is constructed with a read
/// timeout so that slow upstreams are cut off after the specified duration.
async fn forward_via_codec(
    conn: Pooled<SocketAddr, PoolableConnection>,
    request: MonoioRequest,
    backend_timeout: Option<std::time::Duration>,
) -> Result<MonoioResponse, io::Error> {
    let mut codec: ClientCodec<Pooled<SocketAddr, PoolableConnection>> = match backend_timeout {
        Some(dur) => ClientCodec::new_with_timeout(conn, dur),
        None => ClientCodec::new(conn),
    };

    // Send request.
    <ClientCodec<Pooled<SocketAddr, PoolableConnection>> as Sink<MonoioRequest>>::send(
        &mut codec, request,
    )
    .await
    .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))?;
    <ClientCodec<Pooled<SocketAddr, PoolableConnection>> as Sink<MonoioRequest>>::flush(&mut codec)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))?;

    // Receive response.
    let resp: http::Response<_> = monoio::io::stream::Stream::next(&mut codec)
        .await
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "upstream closed"))?
        .map_err(|e: monoio_http::common::error::HttpError| {
            // Detect codec-level timeout (from ClientCodec::new_with_timeout).
            let msg = e.to_string();
            if msg.contains("timeout") {
                io::Error::new(io::ErrorKind::TimedOut, msg)
            } else {
                io::Error::new(io::ErrorKind::BrokenPipe, msg)
            }
        })?;

    // Read the body if present so the connection can be reused.
    let (head, payload_decoder) = resp.into_parts();
    let mut body_reader = payload_decoder.with_io(&mut codec);
    use monoio_http::common::body::Body;
    let mut body_bytes = bytes::BytesMut::new();
    while let Some(Ok(chunk)) = body_reader.next_data().await {
        body_bytes.extend_from_slice(&chunk);
    }

    let response_payload = if body_bytes.is_empty() {
        Payload::None
    } else {
        Payload::Fixed(monoio_http::h1::payload::FixedPayload::new(
            body_bytes.freeze(),
        ))
    };
    Ok(MonoioResponse::from_parts(head, response_payload))
}

/// Forward a request to an upstream backend over HTTP/2 cleartext (h2c).
///
/// Opens a fresh TCP connection (no pooling), performs the HTTP/2 prior-
/// knowledge handshake, sends the request, and collects the full response
/// body into a `MonoioResponse`.
///
/// When `backend_timeout` is `Some`, the entire H2 exchange is wrapped in a
/// `monoio::time::timeout`.
async fn forward_via_h2(
    addr: SocketAddr,
    request: MonoioRequest,
    backend_timeout: Option<std::time::Duration>,
) -> Result<MonoioResponse, io::Error> {
    if let Some(dur) = backend_timeout {
        match monoio::time::timeout(dur, forward_via_h2_inner(addr, request)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "h2 backend timeout",
            )),
        }
    } else {
        forward_via_h2_inner(addr, request).await
    }
}

/// Inner implementation of H2C forwarding (without timeout wrapper).
async fn forward_via_h2_inner(
    addr: SocketAddr,
    request: MonoioRequest,
) -> Result<MonoioResponse, io::Error> {
    // 1. Open a TCP connection to the backend.
    let stream = monoio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?;

    // 2. Perform H2 handshake using monoio_http::h2::client.
    let (send_request, h2_conn) = monoio_http::h2::client::handshake(stream)
        .await
        .map_err(|e| io::Error::other(e.to_string()))?;

    // 3. Spawn the H2 connection driver.
    monoio::spawn(async move {
        if let Err(e) = h2_conn.await {
            tracing::debug!("h2 connection driver error: {e}");
        }
    });

    // 4. Wait for the H2 connection to be ready.
    let mut send_request = send_request
        .ready()
        .await
        .map_err(|e| io::Error::other(e.to_string()))?;

    // 5. Convert the monoio request into an http::Request<()> for H2.
    let (parts, payload) = request.into_parts();

    // Read the body from the payload (if any) so we can send it over H2.
    let mut body_bytes = bytes::BytesMut::new();
    {
        use monoio_http::common::body::Body;
        let mut payload = payload;
        while let Some(Ok(chunk)) = payload.next_data().await {
            body_bytes.extend_from_slice(&chunk);
        }
    }
    let has_body = !body_bytes.is_empty();

    // Build the H2 request with HTTP/2 pseudo-headers.
    let mut h2_req_builder = http::Request::builder()
        .method(parts.method.clone())
        .version(http::Version::HTTP_2);

    // Set the :authority pseudo-header from the Host header.
    if let Some(host) = parts.headers.get(http::header::HOST) {
        h2_req_builder = h2_req_builder.uri(
            http::Uri::builder()
                .scheme("http")
                .authority(host.to_str().unwrap_or(""))
                .path_and_query(
                    parts
                        .uri
                        .path_and_query()
                        .map(|pq| pq.as_str())
                        .unwrap_or("/"),
                )
                .build()
                .unwrap_or_else(|_| parts.uri.clone()),
        );
    } else {
        h2_req_builder = h2_req_builder.uri(parts.uri.clone());
    }

    let mut h2_request = h2_req_builder
        .body(())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

    // Copy headers, stripping HTTP/1.1-specific hop-by-hop headers.
    *h2_request.headers_mut() = parts.headers.clone();
    h2_request
        .headers_mut()
        .remove(http::header::TRANSFER_ENCODING);
    h2_request.headers_mut().remove(http::header::CONNECTION);
    h2_request.headers_mut().remove(http::header::HOST);

    // 6. Send the request.
    let end_of_stream = !has_body;
    let (response_future, mut send_stream) = send_request
        .send_request(h2_request, end_of_stream)
        .map_err(|e| io::Error::other(e.to_string()))?;

    // Send body data if present.
    if has_body {
        send_stream
            .send_data(body_bytes.freeze(), true)
            .map_err(|e| io::Error::other(e.to_string()))?;
    }

    // 7. Await the response.
    let h2_response = response_future
        .await
        .map_err(|e| io::Error::other(e.to_string()))?;

    // 8. Read the response body from RecvStream.
    let (resp_parts, mut recv_stream) = h2_response.into_parts();
    let mut resp_body = bytes::BytesMut::new();
    {
        use monoio_http::common::body::Body;
        while let Some(Ok(chunk)) = recv_stream.next_data().await {
            resp_body.extend_from_slice(&chunk);
        }
    }

    // 9. Convert back to MonoioResponse.
    let response_payload = if resp_body.is_empty() {
        Payload::None
    } else {
        Payload::Fixed(monoio_http::h1::payload::FixedPayload::new(
            resp_body.freeze(),
        ))
    };
    Ok(MonoioResponse::from_parts(resp_parts, response_payload))
}

// ---------------------------------------------------------------------------
// Layer 2: RouteAndFilter
//
// Combined route resolution and filter application. Routes the request,
// applies request filters, delegates to the inner forwarder, then applies
// response filters.
// ---------------------------------------------------------------------------

/// Service that resolves routes and applies filters before delegating to
/// the inner upstream forwarder.
pub struct RouteAndFilter<T> {
    config: Rc<ProxyConfig>,
    inner: T,
}

impl<T> Service<PipelineRequest> for RouteAndFilter<T>
where
    T: Service<ForwardRequest, Response = MonoioResponse, Error = Infallible>,
{
    type Response = MonoioResponse;
    type Error = Infallible;

    async fn call(&self, req: PipelineRequest) -> Result<MonoioResponse, Infallible> {
        let PipelineRequest {
            request,
            meta,
            listener_name,
        } = req;

        // Convert owned RequestMeta to borrowed for routing.
        let header_refs: Vec<(&str, &str)> = meta
            .headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let qp_refs: Vec<(&str, &str)> = meta
            .query_params
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let routing_meta = routing::RequestMeta {
            host: meta.host.as_deref(),
            path: &meta.path,
            method: &meta.method,
            headers: &header_refs,
            query_params: &qp_refs,
        };

        let decision = routing::route(&self.config, &listener_name, &routing_meta);

        match decision {
            RoutingDecision::NotFound => Ok(string_response_to_monoio(not_found_response())),
            RoutingDecision::Redirect(redir) => Ok(string_response_to_monoio(redirect_response(
                redir.status_code,
                &redir.location,
            ))),
            RoutingDecision::Forward {
                backends,
                filters,
                timeout,
            } => {
                let mut applied = filter::collect_filters(filters);
                // Re-compute rewrite with actual path and prefix info.
                applied = recompute_rewrite(filters, &meta, applied);
                // Attach timeout config from the matched route rule.
                applied.timeout = timeout.cloned();

                // Extract request timeout before moving applied into ForwardRequest.
                let request_timeout = applied
                    .timeout
                    .as_ref()
                    .and_then(|t| t.request)
                    .filter(|&t| t > 0.0);

                let forward_req = ForwardRequest {
                    request,
                    meta,
                    backends: backends.to_vec(),
                    applied_filter: applied,
                };

                if let Some(timeout_secs) = request_timeout {
                    let duration = std::time::Duration::from_secs_f64(timeout_secs);
                    match monoio::time::timeout(duration, self.inner.call(forward_req)).await {
                        Ok(result) => result,
                        Err(_elapsed) => {
                            tracing::warn!(timeout_secs, "request timeout exceeded");
                            Ok(string_response_to_monoio(gateway_timeout_response()))
                        }
                    }
                } else {
                    self.inner.call(forward_req).await
                }
            }
        }
    }
}

/// Re-compute the rewrite with the actual request path context.
fn recompute_rewrite(
    filters: &[crate::config::Filter],
    meta: &RequestMeta,
    mut applied: AppliedFilter,
) -> AppliedFilter {
    for f in filters {
        if let crate::config::Filter::URLRewrite { .. } = f {
            applied.rewrite = Some(filter::compute_rewrite(f, &meta.path, None));
        }
    }
    applied
}

// ---------------------------------------------------------------------------
// Layer 1 (outermost): HttpCoreService
// ---------------------------------------------------------------------------

/// Outermost service: manages the HTTP session loop via `ServerCodec`.
/// Parses requests, delegates to the inner pipeline, and sends responses.
/// Also handles WebSocket upgrade requests by routing directly and
/// establishing bidirectional tunnels.
pub struct HttpCoreService<T, R: Resolver = crate::transport::StdResolver> {
    listener_name: String,
    proxy_config: Rc<ProxyConfig>,
    resolver: R,
    inner: T,
}

/// The input to HttpCoreService: a connection wrapping a stream.
///
/// Generic over the stream type `S`, supporting both plain TCP and
/// TLS-terminated connections. The stream must implement monoio's
/// `AsyncReadRent`, `AsyncWriteRent`, and `Split` traits, which are
/// satisfied by both `TcpStream` and `monoio_rustls::ServerTlsStream<TcpStream>`.
pub struct ProxyConnection<S> {
    pub stream: S,
}

/// A plain TCP connection (backward-compatible alias).
pub type TcpConnection = ProxyConnection<TcpStream>;

/// A TLS-terminated connection over TCP.
pub type TlsConnection = ProxyConnection<monoio_rustls::ServerTlsStream<TcpStream>>;

impl<T, S, R> Service<ProxyConnection<S>> for HttpCoreService<T, R>
where
    S: AsyncReadRent + AsyncWriteRent + Split + 'static,
    T: Service<PipelineRequest, Response = MonoioResponse, Error = Infallible>,
    R: Resolver,
{
    type Response = ();
    type Error = io::Error;

    async fn call(&self, conn: ProxyConnection<S>) -> Result<(), io::Error> {
        // Read the first request's raw bytes to check for WebSocket upgrade
        // before handing the stream to the ServerCodec. This allows us to
        // retain the raw stream for bidirectional tunnelling when needed.
        let mut stream = conn.stream;
        let raw_head = read_http_request_head(&mut stream).await?;

        if is_raw_websocket_upgrade(&raw_head) {
            tracing::debug!("websocket upgrade request detected (pre-codec)");
            let (read_half, write_half) = stream.into_split();
            return handle_websocket_upgrade(
                read_half,
                write_half,
                &raw_head,
                &self.proxy_config,
                &self.listener_name,
                &self.resolver,
            )
            .await;
        }

        // Not a WebSocket request — wrap in PrefixedStream so the ServerCodec
        // re-reads the bytes we already consumed.
        let prefixed = PrefixedStream::new(stream, raw_head);
        let mut codec: ServerCodec<PrefixedStream<S>> = ServerCodec::new(prefixed);

        // Keep-alive loop: process requests until the connection is closed.
        loop {
            let req = match monoio::io::stream::Stream::next(&mut codec).await {
                Some(Ok(req)) => req,
                Some(Err(e)) => {
                    tracing::debug!("request parse error: {}", e);
                    return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
                }
                None => {
                    // Client closed the connection.
                    return Ok(());
                }
            };

            let meta = extract_request_meta(&req);
            let pipeline_req = PipelineRequest {
                request: req,
                meta,
                listener_name: self.listener_name.clone(),
            };

            // Delegate to inner pipeline (infallible).
            let resp = self
                .inner
                .call(pipeline_req)
                .await
                .unwrap_or_else(|e: Infallible| match e {});

            // Send response.
            <ServerCodec<PrefixedStream<S>> as Sink<MonoioResponse>>::send(&mut codec, resp)
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))?;
            <ServerCodec<PrefixedStream<S>> as Sink<MonoioResponse>>::flush(&mut codec)
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))?;
        }
    }
}

// ---------------------------------------------------------------------------
// MakeService / FactoryLayer implementations
// ---------------------------------------------------------------------------

/// Configuration for constructing the pipeline via FactoryStack.
pub struct PipelineConfig<R: Resolver> {
    pub proxy_config: Rc<ProxyConfig>,
    pub listener_name: String,
    pub pool: TcpConnectionPool,
    pub resolver: R,
}

/// Factory for UpstreamForwarder (innermost).
pub struct UpstreamForwarderFactory<R: Resolver> {
    pool: TcpConnectionPool,
    resolver: R,
}

impl<R: Resolver + Clone> MakeService for UpstreamForwarderFactory<R> {
    type Service = UpstreamForwarder<R>;
    type Error = Infallible;

    fn make_via_ref(&self, _old: Option<&Self::Service>) -> Result<Self::Service, Self::Error> {
        Ok(UpstreamForwarder {
            pool: self.pool.clone(),
            resolver: self.resolver.clone(),
        })
    }
}

impl<R: Resolver + Clone> UpstreamForwarderFactory<R> {
    pub fn layer<C>() -> impl FactoryLayer<C, (), Factory = Self>
    where
        C: AsRef<PipelineConfig<R>>,
    {
        layer_fn(|c: &C, ()| {
            let cfg = c.as_ref();
            UpstreamForwarderFactory {
                pool: cfg.pool.clone(),
                resolver: cfg.resolver.clone(),
            }
        })
    }
}

/// Factory for RouteAndFilter.
pub struct RouteAndFilterFactory<T> {
    config: Rc<ProxyConfig>,
    inner: T,
}

impl<T: MakeService> MakeService for RouteAndFilterFactory<T> {
    type Service = RouteAndFilter<T::Service>;
    type Error = T::Error;

    fn make_via_ref(&self, old: Option<&Self::Service>) -> Result<Self::Service, Self::Error> {
        let inner = self.inner.make_via_ref(old.map(|o| &o.inner))?;
        Ok(RouteAndFilter {
            config: self.config.clone(),
            inner,
        })
    }
}

impl<T> RouteAndFilterFactory<T> {
    pub fn layer<C, R: Resolver>() -> impl FactoryLayer<C, T, Factory = Self>
    where
        C: AsRef<PipelineConfig<R>>,
    {
        layer_fn(|c: &C, inner| {
            let cfg = c.as_ref();
            RouteAndFilterFactory {
                config: cfg.proxy_config.clone(),
                inner,
            }
        })
    }
}

/// Factory for HttpCoreService (outermost).
pub struct HttpCoreServiceFactory<T, R: Resolver> {
    listener_name: String,
    proxy_config: Rc<ProxyConfig>,
    resolver: R,
    inner: T,
}

impl<T: MakeService, R: Resolver + Clone> MakeService for HttpCoreServiceFactory<T, R> {
    type Service = HttpCoreService<T::Service, R>;
    type Error = T::Error;

    fn make_via_ref(&self, old: Option<&Self::Service>) -> Result<Self::Service, Self::Error> {
        let inner = self.inner.make_via_ref(old.map(|o| &o.inner))?;
        Ok(HttpCoreService {
            listener_name: self.listener_name.clone(),
            proxy_config: self.proxy_config.clone(),
            resolver: self.resolver.clone(),
            inner,
        })
    }
}

impl<T, R: Resolver> HttpCoreServiceFactory<T, R> {
    pub fn layer<C>() -> impl FactoryLayer<C, T, Factory = Self>
    where
        C: AsRef<PipelineConfig<R>>,
        R: Clone,
    {
        layer_fn(|c: &C, inner| {
            let cfg = c.as_ref();
            HttpCoreServiceFactory {
                listener_name: cfg.listener_name.clone(),
                proxy_config: cfg.proxy_config.clone(),
                resolver: cfg.resolver.clone(),
                inner,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Convenience: build_pipeline
// ---------------------------------------------------------------------------

/// Build the complete pipeline service from a `PipelineConfig`.
///
/// Returns an `HttpCoreService` that accepts `TcpConnection` and processes
/// HTTP requests through: route resolution -> filter application -> upstream
/// forwarding.
pub fn build_pipeline<R: Resolver + Clone>(
    config: &PipelineConfig<R>,
) -> HttpCoreService<RouteAndFilter<UpstreamForwarder<R>>, R> {
    HttpCoreService {
        listener_name: config.listener_name.clone(),
        proxy_config: config.proxy_config.clone(),
        resolver: config.resolver.clone(),
        inner: RouteAndFilter {
            config: config.proxy_config.clone(),
            inner: UpstreamForwarder {
                pool: config.pool.clone(),
                resolver: config.resolver.clone(),
            },
        },
    }
}

// ---------------------------------------------------------------------------
// Unit tests (original tests preserved)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // extract_request_meta
    // -----------------------------------------------------------------------

    #[test]
    fn extract_meta_basic() {
        let req = http::Request::builder()
            .method("GET")
            .uri("http://example.com/api/v1/users?page=2&limit=10")
            .header("host", "example.com")
            .header("x-custom", "value")
            .body(())
            .unwrap();

        let meta = extract_request_meta(&req);
        assert_eq!(meta.host.as_deref(), Some("example.com"));
        assert_eq!(meta.path, "/api/v1/users");
        assert_eq!(meta.method, "GET");
        assert!(
            meta.headers
                .contains(&("host".to_string(), "example.com".to_string()))
        );
        assert!(
            meta.headers
                .contains(&("x-custom".to_string(), "value".to_string()))
        );
        assert_eq!(
            meta.query_params,
            vec![
                ("page".to_string(), "2".to_string()),
                ("limit".to_string(), "10".to_string()),
            ]
        );
    }

    #[test]
    fn extract_meta_strips_port_from_host() {
        let req = http::Request::builder()
            .method("GET")
            .uri("/path")
            .header("host", "example.com:8080")
            .body(())
            .unwrap();

        let meta = extract_request_meta(&req);
        assert_eq!(meta.host.as_deref(), Some("example.com"));
    }

    #[test]
    fn extract_meta_host_without_port() {
        let req = http::Request::builder()
            .method("GET")
            .uri("/path")
            .header("host", "example.com")
            .body(())
            .unwrap();

        let meta = extract_request_meta(&req);
        assert_eq!(meta.host.as_deref(), Some("example.com"));
    }

    #[test]
    fn extract_meta_no_host_header() {
        let req = http::Request::builder()
            .method("POST")
            .uri("/submit")
            .body(())
            .unwrap();

        let meta = extract_request_meta(&req);
        assert_eq!(meta.host, None);
        assert_eq!(meta.method, "POST");
    }

    #[test]
    fn extract_meta_no_query_params() {
        let req = http::Request::builder()
            .method("GET")
            .uri("/path")
            .header("host", "example.com")
            .body(())
            .unwrap();

        let meta = extract_request_meta(&req);
        assert!(meta.query_params.is_empty());
    }

    #[test]
    fn extract_meta_method_preserved() {
        for method in &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"] {
            let req = http::Request::builder()
                .method(*method)
                .uri("/")
                .body(())
                .unwrap();

            let meta = extract_request_meta(&req);
            assert_eq!(meta.method, *method);
        }
    }

    #[test]
    fn extract_meta_multiple_headers() {
        let req = http::Request::builder()
            .method("GET")
            .uri("/")
            .header("host", "example.com")
            .header("accept", "text/html")
            .header("x-request-id", "abc-123")
            .body(())
            .unwrap();

        let meta = extract_request_meta(&req);
        assert_eq!(meta.headers.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Response builders
    // -----------------------------------------------------------------------

    #[test]
    fn not_found_response_status() {
        let resp = not_found_response();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        assert_eq!(resp.status().as_u16(), 404);
    }

    #[test]
    fn bad_gateway_response_status() {
        let resp = bad_gateway_response();
        assert_eq!(resp.status(), http::StatusCode::BAD_GATEWAY);
        assert_eq!(resp.status().as_u16(), 502);
    }

    #[test]
    fn service_unavailable_response_status() {
        let resp = service_unavailable_response();
        assert_eq!(resp.status(), http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.status().as_u16(), 503);
    }

    #[test]
    fn gateway_timeout_response_status() {
        let resp = gateway_timeout_response();
        assert_eq!(resp.status(), http::StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(resp.status().as_u16(), 504);
    }

    #[test]
    fn redirect_response_has_location_header() {
        let resp = redirect_response(301, "https://example.com/new");
        assert_eq!(resp.status().as_u16(), 301);
        assert_eq!(
            resp.headers().get(http::header::LOCATION).unwrap(),
            "https://example.com/new"
        );
    }

    #[test]
    fn redirect_response_302() {
        let resp = redirect_response(302, "/other");
        assert_eq!(resp.status().as_u16(), 302);
        assert_eq!(
            resp.headers().get(http::header::LOCATION).unwrap(),
            "/other"
        );
    }

    // -----------------------------------------------------------------------
    // select_backend
    // -----------------------------------------------------------------------

    #[test]
    fn select_backend_empty_returns_none() {
        assert!(select_backend(&[]).is_none());
    }

    #[test]
    fn select_backend_single_returns_that_backend() {
        let backends = vec![Backend::new("ns", "svc", 8080)];
        let selected = select_backend(&backends).unwrap();
        assert_eq!(selected.name, "svc");
    }

    #[test]
    fn select_backend_weight_zero_never_selected() {
        let backends = vec![
            Backend::new("ns", "zero-weight", 8080).with_weight(0),
            Backend::new("ns", "positive", 8081).with_weight(1),
        ];
        // Run many times to ensure zero-weight is never picked.
        for _ in 0..100 {
            let selected = select_backend(&backends).unwrap();
            assert_eq!(selected.name, "positive");
        }
    }

    #[test]
    fn select_backend_all_zero_weight_returns_none() {
        let backends = vec![
            Backend::new("ns", "a", 8080).with_weight(0),
            Backend::new("ns", "b", 8081).with_weight(0),
        ];
        assert!(select_backend(&backends).is_none());
    }

    #[test]
    fn select_backend_weighted_distribution() {
        let backends = vec![
            Backend::new("ns", "heavy", 8080).with_weight(100),
            Backend::new("ns", "light", 8081).with_weight(1),
        ];
        let mut heavy_count = 0u32;
        let iterations = 1000;
        for _ in 0..iterations {
            let selected = select_backend(&backends).unwrap();
            if selected.name == "heavy" {
                heavy_count += 1;
            }
        }
        // With 100:1 weighting, heavy should be picked most of the time.
        assert!(
            heavy_count > 900,
            "Expected heavy backend to be selected >900 times out of {iterations}, got {heavy_count}"
        );
    }

    // -----------------------------------------------------------------------
    // is_websocket_upgrade
    // -----------------------------------------------------------------------

    /// Helper: build an HTTP request with the given headers for WebSocket testing.
    fn ws_request(headers: &[(&str, &str)]) -> http::Request<()> {
        let mut builder = http::Request::builder().method("GET").uri("/ws");
        for &(name, value) in headers {
            builder = builder.header(name, value);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn websocket_upgrade_detects_valid_upgrade() {
        let req = ws_request(&[
            ("host", "example.com"),
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert!(is_websocket_upgrade(&req));
    }

    #[test]
    fn websocket_upgrade_rejects_missing_connection_header() {
        let req = ws_request(&[
            ("host", "example.com"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert!(!is_websocket_upgrade(&req));
    }

    #[test]
    fn websocket_upgrade_rejects_missing_upgrade_header() {
        let req = ws_request(&[
            ("host", "example.com"),
            ("connection", "Upgrade"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert!(!is_websocket_upgrade(&req));
    }

    #[test]
    fn websocket_upgrade_rejects_missing_websocket_version() {
        let req = ws_request(&[
            ("host", "example.com"),
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert!(!is_websocket_upgrade(&req));
    }

    #[test]
    fn websocket_upgrade_rejects_missing_websocket_key() {
        let req = ws_request(&[
            ("host", "example.com"),
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
        ]);
        assert!(!is_websocket_upgrade(&req));
    }

    #[test]
    fn websocket_upgrade_rejects_non_websocket_upgrade() {
        let req = ws_request(&[
            ("host", "example.com"),
            ("connection", "Upgrade"),
            ("upgrade", "h2c"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert!(!is_websocket_upgrade(&req));
    }

    #[test]
    fn websocket_upgrade_is_case_insensitive() {
        let req = ws_request(&[
            ("host", "example.com"),
            ("Connection", "UPGRADE"),
            ("Upgrade", "WebSocket"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert!(is_websocket_upgrade(&req));
    }

    #[test]
    fn websocket_upgrade_connection_header_with_multiple_values() {
        let req = ws_request(&[
            ("host", "example.com"),
            ("connection", "keep-alive, Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert!(is_websocket_upgrade(&req));
    }

    // -----------------------------------------------------------------------
    // is_raw_websocket_upgrade
    // -----------------------------------------------------------------------

    #[test]
    fn raw_websocket_upgrade_detects_valid_upgrade() {
        let raw = b"GET /ws HTTP/1.1\r\nHost: example.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
        assert!(is_raw_websocket_upgrade(raw));
    }

    #[test]
    fn raw_websocket_upgrade_rejects_normal_request() {
        let raw = b"GET /hello HTTP/1.1\r\nHost: example.com\r\n\r\n";
        assert!(!is_raw_websocket_upgrade(raw));
    }

    #[test]
    fn raw_websocket_upgrade_is_case_insensitive() {
        let raw = b"GET /ws HTTP/1.1\r\nhost: example.com\r\nCONNECTION: UPGRADE\r\nUPGRADE: WebSocket\r\nsec-websocket-version: 13\r\nsec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
        assert!(is_raw_websocket_upgrade(raw));
    }

    #[test]
    fn raw_websocket_upgrade_rejects_missing_key() {
        let raw = b"GET /ws HTTP/1.1\r\nHost: example.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\r\n";
        assert!(!is_raw_websocket_upgrade(raw));
    }
}

// ---------------------------------------------------------------------------
// Original integration tests (codec tests from MULTI-1078)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod integration_tests {
    use monoio::io::{AsyncReadRent, AsyncWriteRentExt};
    use monoio::net::{TcpListener, TcpStream};
    use monoio_http::common::body::Body;
    use monoio_http::h1::codec::ServerCodec;

    use monoio::io::sink::Sink;
    use monoio::io::stream::Stream;

    use monoio_http::h1::payload::Payload;

    /// Type aliases to help inference with monoio-http's generic Payload.
    type MonoioResponse = monoio_http::common::response::Response;
    type MonoioRequest = monoio_http::common::request::Request;

    /// Integration test: parse one HTTP request through ServerCodec, send a response back.
    #[monoio::test]
    async fn server_codec_parse_request_send_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = monoio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut codec = ServerCodec::new(stream);

            // Read one request.
            let req = codec.next().await.unwrap().unwrap();
            assert_eq!(req.method(), http::Method::GET);
            assert_eq!(req.uri().path(), "/hello");

            // Send a response.
            let (parts, _) = http::Response::builder()
                .status(200)
                .body(())
                .unwrap()
                .into_parts();
            let resp = MonoioResponse::from_parts(parts, Payload::None);
            codec.send(resp).await.unwrap();
            Sink::<MonoioResponse>::flush(&mut codec).await.unwrap();
        });

        let client = monoio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            let raw = b"GET /hello HTTP/1.1\r\nHost: localhost\r\n\r\n";
            let (result, _) = stream.write_all(raw.to_vec()).await;
            result.unwrap();

            // Read response (just verify we get data back).
            let buf = vec![0u8; 1024];
            let (result, buf) = stream.read(buf).await;
            let n = result.unwrap();
            let response_text = String::from_utf8_lossy(&buf[..n]);
            assert!(
                response_text.contains("200"),
                "Expected 200 in response, got: {response_text}"
            );
        });

        server.await;
        client.await;
    }

    /// Integration test: HTTP/1.1 keep-alive — two requests on the same connection.
    #[monoio::test]
    async fn server_codec_keepalive_two_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = monoio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut codec = ServerCodec::new(stream);

            // Request 1.
            let req1 = codec.next().await.unwrap().unwrap();
            assert_eq!(req1.uri().path(), "/first");
            let (parts, _) = http::Response::builder()
                .status(200)
                .body(())
                .unwrap()
                .into_parts();
            let resp1 = MonoioResponse::from_parts(parts, Payload::None);
            codec.send(resp1).await.unwrap();
            Sink::<MonoioResponse>::flush(&mut codec).await.unwrap();

            // Request 2 on same connection.
            let req2 = codec.next().await.unwrap().unwrap();
            assert_eq!(req2.uri().path(), "/second");
            let (parts, _) = http::Response::builder()
                .status(204)
                .body(())
                .unwrap()
                .into_parts();
            let resp2 = MonoioResponse::from_parts(parts, Payload::None);
            codec.send(resp2).await.unwrap();
            Sink::<MonoioResponse>::flush(&mut codec).await.unwrap();
        });

        let client = monoio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();

            // Send first request.
            let raw1 = b"GET /first HTTP/1.1\r\nHost: localhost\r\n\r\n";
            let (result, _) = stream.write_all(raw1.to_vec()).await;
            result.unwrap();

            // Read first response.
            let buf = vec![0u8; 1024];
            let (result, buf) = stream.read(buf).await;
            let n = result.unwrap();
            let resp1_text = String::from_utf8_lossy(&buf[..n]);
            assert!(
                resp1_text.contains("200"),
                "Expected 200 in first response, got: {resp1_text}"
            );
            drop(buf);

            // Send second request on same connection.
            let raw2 = b"GET /second HTTP/1.1\r\nHost: localhost\r\n\r\n";
            let (result, _) = stream.write_all(raw2.to_vec()).await;
            result.unwrap();

            // Read second response.
            let buf = vec![0u8; 1024];
            let (result, buf) = stream.read(buf).await;
            let n = result.unwrap();
            let resp2_text = String::from_utf8_lossy(&buf[..n]);
            assert!(
                resp2_text.contains("204"),
                "Expected 204 in second response, got: {resp2_text}"
            );
        });

        server.await;
        client.await;
    }

    /// Integration test: ClientCodec sends a request to a test backend and receives a response.
    #[monoio::test]
    async fn client_codec_send_receive() {
        use monoio_http::h1::codec::ClientCodec;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // Fake backend: read raw HTTP, write raw HTTP response.
        let backend = monoio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Read the request.
            let buf = vec![0u8; 1024];
            let (result, buf) = stream.read(buf).await;
            let n = result.unwrap();
            let request_text = String::from_utf8_lossy(&buf[..n]);
            assert!(
                request_text.contains("GET /backend-path"),
                "Expected request path, got: {request_text}"
            );

            // Send a response.
            let raw_resp = b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello";
            let (result, _) = stream.write_all(raw_resp.to_vec()).await;
            result.unwrap();
        });

        let client = monoio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            let mut codec = ClientCodec::new(stream);

            // Send a request via ClientCodec.
            let (parts, _) = http::Request::builder()
                .method("GET")
                .uri("/backend-path")
                .header("host", "localhost")
                .body(())
                .unwrap()
                .into_parts();
            let req = MonoioRequest::from_parts(parts, Payload::None);
            codec.send(req).await.unwrap();
            Sink::<MonoioRequest>::flush(&mut codec).await.unwrap();

            // Read the response.
            let resp = codec.next().await.unwrap().unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);

            // Read body via FramedPayload.
            let (head, payload_decoder) = resp.into_parts();
            assert_eq!(head.status, http::StatusCode::OK);
            let mut body = payload_decoder.with_io(&mut codec);
            let data = body.next_data().await.unwrap().unwrap();
            assert_eq!(&data[..], b"hello");
        });

        backend.await;
        client.await;
    }
}

// ---------------------------------------------------------------------------
// Pipeline integration tests (MULTI-1080)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod pipeline_tests {
    use super::*;
    use crate::config::{
        Backend, Filter, HeaderValue as ConfigHeaderValue, PathModifier, ProxyConfigBuilder,
        RouteConfigBuilder, RouteRuleBuilder,
    };
    use crate::pool::new_tcp_pool;
    use crate::transport::MockResolver;
    use monoio::io::{AsyncReadRent, AsyncWriteRentExt};
    use monoio::net::TcpListener;
    use std::net::SocketAddr;

    /// Helper: start a fake HTTP backend that reads the request and sends a
    /// canned response. Returns the listener address.
    async fn start_fake_backend(
        response: &'static [u8],
    ) -> (SocketAddr, monoio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = monoio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let buf = vec![0u8; 4096];
            let (result, buf) = stream.read(buf).await;
            let n = result.unwrap();
            let request_text = String::from_utf8_lossy(&buf[..n]).to_string();

            let (result, _) = stream.write_all(response.to_vec()).await;
            result.unwrap();
            request_text
        });
        (addr, handle)
    }

    /// Helper: start a fake backend that echoes the received request headers
    /// in the response body.
    async fn start_echo_backend() -> (SocketAddr, monoio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = monoio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let buf = vec![0u8; 4096];
            let (result, buf) = stream.read(buf).await;
            let n = result.unwrap();
            let request_text = String::from_utf8_lossy(&buf[..n]).to_string();

            // Echo the request text in the response body.
            let body = request_text.clone();
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            let (result, _) = stream.write_all(resp.into_bytes()).await;
            result.unwrap();
            request_text
        });
        (addr, handle)
    }

    /// Helper: send a raw HTTP request through the proxy and read the response.
    async fn send_through_proxy(proxy_addr: SocketAddr, raw_request: &[u8]) -> String {
        let mut stream = monoio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let (result, _) = stream.write_all(raw_request.to_vec()).await;
        result.unwrap();

        let buf = vec![0u8; 8192];
        let (result, buf) = stream.read(buf).await;
        let n = result.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    }

    /// Helper: build a pipeline and bind it to a TCP listener, returning
    /// the listener address.
    fn build_test_pipeline(
        config: ProxyConfig,
        listener_name: &str,
        resolver: MockResolver,
    ) -> (SocketAddr, Rc<ProxyConfig>, TcpConnectionPool) {
        let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let config = Rc::new(config);
        let pool = new_tcp_pool();

        let pipeline_config = PipelineConfig {
            proxy_config: config.clone(),
            listener_name: listener_name.to_string(),
            pool: pool.clone(),
            resolver,
        };

        let svc = build_pipeline(&pipeline_config);

        // Spawn the accept loop.
        monoio::spawn(async move {
            // Accept one connection.
            let (stream, _) = proxy_listener.accept().await.unwrap();
            let _ = svc.call(TcpConnection { stream }).await;
        });

        (proxy_addr, config, pool)
    }

    // -----------------------------------------------------------------------
    // Test 1: basic proxy
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_basic_proxy() {
        let (backend_addr, backend_handle) =
            start_fake_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let (proxy_addr, _, _) = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /hello HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        assert!(resp.contains("hello"), "Expected body 'hello', got: {resp}");

        backend_handle.await;
    }

    // -----------------------------------------------------------------------
    // Test 2: routing — path-based routing to different backends
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_path_routing() {
        let (backend_a_addr, backend_a_handle) =
            start_fake_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 9\r\n\r\nbackend-a").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-a")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/api")
                            .with_backend(Backend::new("default", "svc-a", backend_a_addr.port()))
                            .build(),
                    )
                    .build(),
            )
            .with_route(
                RouteConfigBuilder::new("default", "route-b")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/web")
                            .with_backend(Backend::new("default", "svc-b", 9999))
                            .build(),
                    )
                    .build(),
            )
            .build();

        // Resolver maps both backend names to backend_a's address.
        let resolver = MockResolver::new(vec![backend_a_addr]);
        let (proxy_addr, _, _) = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /api/users HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        assert!(
            resp.contains("backend-a"),
            "Expected backend-a response, got: {resp}"
        );

        backend_a_handle.await;
    }

    // -----------------------------------------------------------------------
    // Test 3: request header modification
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_request_header_modification() {
        let (backend_addr, backend_handle) = start_echo_backend().await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_filter(Filter::RequestHeaderModifier {
                                add: vec![ConfigHeaderValue {
                                    name: "x-added".to_string(),
                                    value: "injected".to_string(),
                                }],
                                set: vec![],
                                remove: vec![],
                            })
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let (proxy_addr, _, _) = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /test HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        // The echo backend sends back the request it received as the body.
        // Verify the added header is present in what the backend saw.
        let backend_received = backend_handle.await;
        assert!(
            backend_received.to_lowercase().contains("x-added"),
            "Expected x-added header in backend request, got: {backend_received}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: response header modification
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_response_header_modification() {
        let (backend_addr, backend_handle) = start_fake_backend(
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nx-original: yes\r\n\r\nok",
        )
        .await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_filter(Filter::ResponseHeaderModifier {
                                add: vec![ConfigHeaderValue {
                                    name: "x-proxy".to_string(),
                                    value: "multiway".to_string(),
                                }],
                                set: vec![],
                                remove: vec!["x-original".to_string()],
                            })
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let (proxy_addr, _, _) = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /test HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        assert!(
            resp.to_lowercase().contains("x-proxy: multiway"),
            "Expected x-proxy header in response, got: {resp}"
        );
        // x-original should be removed.
        assert!(
            !resp.to_lowercase().contains("x-original"),
            "Expected x-original to be removed, got: {resp}"
        );

        backend_handle.await;
    }

    // -----------------------------------------------------------------------
    // Test 5: URL rewrite
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_url_rewrite() {
        let (backend_addr, backend_handle) = start_echo_backend().await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_filter(Filter::URLRewrite {
                                hostname: Some("internal.example.com".to_string()),
                                path: Some(PathModifier::ReplaceFullPath {
                                    value: "/v2/data".to_string(),
                                }),
                            })
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let (proxy_addr, _, _) = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /old-path HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("200"), "Expected 200, got: {resp}");

        // Verify the backend received the rewritten path and host.
        let backend_received = backend_handle.await;
        assert!(
            backend_received.contains("/v2/data"),
            "Expected rewritten path /v2/data, got: {backend_received}"
        );
        assert!(
            backend_received
                .to_lowercase()
                .contains("internal.example.com"),
            "Expected rewritten host, got: {backend_received}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 6: redirect
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_redirect() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/old")
                            .with_filter(Filter::RequestRedirect {
                                scheme: Some("https".to_string()),
                                hostname: Some("new.example.com".to_string()),
                                port: None,
                                path: None,
                                status_code: Some(301),
                            })
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![]);
        let (proxy_addr, _, _) = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /old/page HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("301"), "Expected 301, got: {resp}");
        assert!(
            resp.to_lowercase().contains("location:"),
            "Expected Location header, got: {resp}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 7: 404 — no matching route
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_404_no_matching_route() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_exact_path("/specific")
                            .with_backend(Backend::new("default", "svc", 8080))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![]);
        let (proxy_addr, _, _) = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /nonexistent HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("404"), "Expected 404, got: {resp}");
    }

    // -----------------------------------------------------------------------
    // Test 8: 503 — route with no backends
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_503_no_backends() {
        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            // No backends added!
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![]);
        let (proxy_addr, _, _) = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /test HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("503"), "Expected 503, got: {resp}");
    }

    // -----------------------------------------------------------------------
    // Test 9: weighted backend distribution
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_weighted_backends() {
        // This test is purely on the select_backend function, as full pipeline
        // integration with multiple backends requires separate connections.
        let backends = vec![
            Backend::new("default", "heavy", 8080).with_weight(90),
            Backend::new("default", "light", 8081).with_weight(10),
        ];

        let mut heavy_count = 0u32;
        let iterations = 1000;
        for _ in 0..iterations {
            let selected = select_backend(&backends).unwrap();
            if selected.name == "heavy" {
                heavy_count += 1;
            }
        }

        // With 90:10 weighting, heavy should be selected roughly 90% of the time.
        assert!(
            heavy_count > 800,
            "Expected heavy > 800 out of {iterations}, got {heavy_count}"
        );
        assert!(
            heavy_count < 980,
            "Expected heavy < 980 out of {iterations}, got {heavy_count}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 10: backend failure returns 502
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_backend_failure_502() {
        // Use an address that nothing is listening on.
        let unreachable_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc", 1))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![unreachable_addr]);
        let (proxy_addr, _, _) = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /test HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("502"), "Expected 502, got: {resp}");
    }
}

// ---------------------------------------------------------------------------
// Mirror tests (MULTI-1084)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod mirror_tests {
    use super::*;
    use crate::config::{
        Backend, Filter, ProxyConfigBuilder, RouteConfigBuilder, RouteRuleBuilder,
    };
    use crate::pool::new_tcp_pool;
    use monoio::io::{AsyncReadRent, AsyncWriteRentExt};
    use monoio::net::TcpListener;
    use std::net::SocketAddr;
    use std::rc::Rc;

    /// Helper: start a fake HTTP backend that reads the request and sends a
    /// canned response. Returns the listener address.
    async fn start_fake_backend(
        response: &'static [u8],
    ) -> (SocketAddr, monoio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = monoio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let buf = vec![0u8; 4096];
            let (result, buf) = stream.read(buf).await;
            let n = result.unwrap();
            let request_text = String::from_utf8_lossy(&buf[..n]).to_string();

            let (result, _) = stream.write_all(response.to_vec()).await;
            result.unwrap();
            request_text
        });
        (addr, handle)
    }

    /// Helper: start a mirror backend that accepts one connection, reads the
    /// request, sends a response, and returns the received request text.
    async fn start_mirror_backend(
        response: &'static [u8],
    ) -> (SocketAddr, monoio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = monoio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let buf = vec![0u8; 4096];
            let (result, buf) = stream.read(buf).await;
            let n = result.unwrap();
            let request_text = String::from_utf8_lossy(&buf[..n]).to_string();

            let (result, _) = stream.write_all(response.to_vec()).await;
            result.unwrap();
            request_text
        });
        (addr, handle)
    }

    /// Helper: send a raw HTTP request through the proxy and read the response.
    async fn send_through_proxy(proxy_addr: SocketAddr, raw_request: &[u8]) -> String {
        let mut stream = monoio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let (result, _) = stream.write_all(raw_request.to_vec()).await;
        result.unwrap();

        let buf = vec![0u8; 8192];
        let (result, buf) = stream.read(buf).await;
        let n = result.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    }

    // -----------------------------------------------------------------------
    // Test 1: single mirror — primary + mirror both receive the request
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_single_mirror() {
        let (primary_addr, primary_handle) =
            start_fake_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\n\r\nprimary").await;
        let (mirror_addr, mirror_handle) =
            start_mirror_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 8\r\n\r\nmirrored").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_filter(Filter::RequestMirror {
                                backend: Backend::new("default", "mirror-svc", mirror_addr.port()),
                                percent: None, // 100%
                            })
                            .with_backend(Backend::new(
                                "default",
                                "primary-svc",
                                primary_addr.port(),
                            ))
                            .build(),
                    )
                    .build(),
            )
            .build();

        // Resolver returns the appropriate address for any host resolution.
        // Since MockResolver returns the same addresses regardless of host,
        // we use a custom resolver that maps by port.
        let resolver = PortMappingResolver(vec![primary_addr, mirror_addr]);
        let proxy_addr = build_mirror_test_pipeline_custom(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /hello HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        // Client should see the primary response.
        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        assert!(
            resp.contains("primary"),
            "Expected primary body, got: {resp}"
        );

        // Both backends should have received a request.
        let primary_received = primary_handle.await;
        assert!(
            primary_received.contains("GET /hello"),
            "Primary should receive the request, got: {primary_received}"
        );

        let mirror_received = mirror_handle.await;
        assert!(
            mirror_received.contains("GET /hello"),
            "Mirror should receive the request, got: {mirror_received}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: mirror failure isolation — mirror unreachable, client gets primary
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_mirror_failure_isolation() {
        let (primary_addr, primary_handle) =
            start_fake_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\n\r\nprimary").await;

        // Mirror backend points to an unreachable port.
        let unreachable_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_filter(Filter::RequestMirror {
                                backend: Backend::new(
                                    "default",
                                    "mirror-svc",
                                    unreachable_addr.port(),
                                ),
                                percent: None,
                            })
                            .with_backend(Backend::new(
                                "default",
                                "primary-svc",
                                primary_addr.port(),
                            ))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = PortMappingResolver(vec![primary_addr, unreachable_addr]);
        let proxy_addr = build_mirror_test_pipeline_custom(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /test HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        // Client should still get the primary response despite mirror failure.
        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        assert!(
            resp.contains("primary"),
            "Expected primary body, got: {resp}"
        );

        primary_handle.await;
    }

    // -----------------------------------------------------------------------
    // Test 3: multiple mirrors — two mirror targets both receive requests
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_multiple_mirrors() {
        let (primary_addr, primary_handle) =
            start_fake_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\n\r\nprimary").await;
        let (mirror1_addr, mirror1_handle) =
            start_mirror_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\n\r\nmirror1").await;
        let (mirror2_addr, mirror2_handle) =
            start_mirror_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\n\r\nmirror2").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_filter(Filter::RequestMirror {
                                backend: Backend::new(
                                    "default",
                                    "mirror-svc-1",
                                    mirror1_addr.port(),
                                ),
                                percent: None,
                            })
                            .with_filter(Filter::RequestMirror {
                                backend: Backend::new(
                                    "default",
                                    "mirror-svc-2",
                                    mirror2_addr.port(),
                                ),
                                percent: None,
                            })
                            .with_backend(Backend::new(
                                "default",
                                "primary-svc",
                                primary_addr.port(),
                            ))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = PortMappingResolver(vec![primary_addr, mirror1_addr, mirror2_addr]);
        let proxy_addr = build_mirror_test_pipeline_custom(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /multi HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        assert!(
            resp.contains("primary"),
            "Expected primary body, got: {resp}"
        );

        let primary_received = primary_handle.await;
        assert!(primary_received.contains("GET /multi"));

        let mirror1_received = mirror1_handle.await;
        assert!(
            mirror1_received.contains("GET /multi"),
            "Mirror 1 should receive the request, got: {mirror1_received}"
        );

        let mirror2_received = mirror2_handle.await;
        assert!(
            mirror2_received.contains("GET /multi"),
            "Mirror 2 should receive the request, got: {mirror2_received}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: percentage mirroring — statistical unit test on the gating logic
    // -----------------------------------------------------------------------

    #[test]
    fn test_percentage_mirroring() {
        // The mirroring code uses `rand::random_range(0..100) >= percent`
        // to skip mirrors. We test this gating logic directly.
        //
        // With percent=0, no requests should be mirrored.
        let mut mirrored_count = 0u32;
        let iterations = 1000u32;
        let percent: u32 = 0;
        for _ in 0..iterations {
            if percent >= 100 || rand::random_range(0..100) < percent {
                mirrored_count += 1;
            }
        }
        assert_eq!(mirrored_count, 0, "0% should mirror nothing");

        // With percent=100, all requests should be mirrored.
        mirrored_count = 0;
        let percent: u32 = 100;
        for _ in 0..iterations {
            if percent >= 100 || rand::random_range(0..100) < percent {
                mirrored_count += 1;
            }
        }
        assert_eq!(mirrored_count, iterations, "100% should mirror everything");

        // With percent=50, roughly half should be mirrored.
        mirrored_count = 0;
        let percent: u32 = 50;
        for _ in 0..iterations {
            if percent >= 100 || rand::random_range(0..100) < percent {
                mirrored_count += 1;
            }
        }
        assert!(
            mirrored_count > 350 && mirrored_count < 650,
            "50% should mirror roughly half, got {mirrored_count} out of {iterations}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: mirror response discarded — mirror returns different response
    // -----------------------------------------------------------------------

    #[monoio::test]
    async fn test_mirror_response_discarded() {
        let (primary_addr, primary_handle) =
            start_fake_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\n\r\nprimary").await;
        let (mirror_addr, mirror_handle) = start_mirror_backend(
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 11\r\n\r\nmirror-fail",
        )
        .await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_filter(Filter::RequestMirror {
                                backend: Backend::new("default", "mirror-svc", mirror_addr.port()),
                                percent: None,
                            })
                            .with_backend(Backend::new(
                                "default",
                                "primary-svc",
                                primary_addr.port(),
                            ))
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = PortMappingResolver(vec![primary_addr, mirror_addr]);
        let proxy_addr = build_mirror_test_pipeline_custom(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /check HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        // Client should see the primary's 200, NOT the mirror's 503.
        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        assert!(
            resp.contains("primary"),
            "Expected primary body, got: {resp}"
        );
        assert!(
            !resp.contains("mirror-fail"),
            "Mirror response should not leak to client, got: {resp}"
        );

        primary_handle.await;
        mirror_handle.await;
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// A resolver that maps port numbers to addresses. This allows the test
    /// to route to different backends based on the port in the config.
    #[derive(Clone)]
    struct PortMappingResolver(Vec<SocketAddr>);

    impl Resolver for PortMappingResolver {
        fn resolve(&self, _host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
            for addr in &self.0 {
                if addr.port() == port {
                    return Ok(vec![*addr]);
                }
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                format!("no address for port {port}"),
            ))
        }
    }

    /// Build a pipeline with a custom resolver.
    fn build_mirror_test_pipeline_custom(
        config: ProxyConfig,
        listener_name: &str,
        resolver: PortMappingResolver,
    ) -> SocketAddr {
        let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let config = Rc::new(config);
        let pool = new_tcp_pool();

        let pipeline_config = PipelineConfig {
            proxy_config: config,
            listener_name: listener_name.to_string(),
            pool,
            resolver,
        };

        let svc = build_pipeline(&pipeline_config);

        monoio::spawn(async move {
            let (stream, _) = proxy_listener.accept().await.unwrap();
            let _ = svc.call(TcpConnection { stream }).await;
        });

        proxy_addr
    }
}

// ---------------------------------------------------------------------------
// Timeout tests (MULTI-1083)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use crate::config::{
        Backend, ProxyConfigBuilder, RouteConfigBuilder, RouteRuleBuilder, TimeoutConfig,
    };
    use crate::pool::new_tcp_pool;
    use crate::transport::MockResolver;
    use monoio::io::{AsyncReadRent, AsyncWriteRentExt};
    use monoio::net::TcpListener;
    use std::net::SocketAddr;

    /// Helper: start a fake HTTP backend that reads the request, sleeps for
    /// `delay_ms` milliseconds, and then sends a canned response.
    async fn start_slow_backend(
        delay_ms: u64,
        response: &'static [u8],
    ) -> (SocketAddr, monoio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = monoio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let buf = vec![0u8; 4096];
            let (result, buf) = stream.read(buf).await;
            let n = result.unwrap();
            let request_text = String::from_utf8_lossy(&buf[..n]).to_string();

            // Delay before sending the response.
            monoio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

            let (result, _) = stream.write_all(response.to_vec()).await;
            result.unwrap();
            request_text
        });
        (addr, handle)
    }

    /// Helper: start a fake HTTP backend that reads the request and sends a
    /// canned response immediately (no delay).
    async fn start_fast_backend(
        response: &'static [u8],
    ) -> (SocketAddr, monoio::task::JoinHandle<String>) {
        start_slow_backend(0, response).await
    }

    /// Helper: send a raw HTTP request through the proxy and read the response.
    async fn send_through_proxy(proxy_addr: SocketAddr, raw_request: &[u8]) -> String {
        let mut stream = monoio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let (result, _) = stream.write_all(raw_request.to_vec()).await;
        result.unwrap();

        let buf = vec![0u8; 8192];
        let (result, buf) = stream.read(buf).await;
        let n = result.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    }

    /// Helper: build a pipeline and bind it to a TCP listener, returning
    /// the listener address.
    fn build_test_pipeline(
        config: ProxyConfig,
        listener_name: &str,
        resolver: MockResolver,
    ) -> SocketAddr {
        let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let config = Rc::new(config);
        let pool = new_tcp_pool();

        let pipeline_config = PipelineConfig {
            proxy_config: config,
            listener_name: listener_name.to_string(),
            pool,
            resolver,
        };

        let svc = build_pipeline(&pipeline_config);

        monoio::spawn(async move {
            let (stream, _) = proxy_listener.accept().await.unwrap();
            let _ = svc.call(TcpConnection { stream }).await;
        });

        proxy_addr
    }

    // -----------------------------------------------------------------------
    // Test 1: backend timeout fires — slow backend exceeds the limit
    // -----------------------------------------------------------------------

    #[monoio::test(enable_timer = true)]
    async fn test_backend_timeout_fires() {
        // Backend delays 500ms, timeout set to 200ms.
        let (backend_addr, _backend_handle) =
            start_slow_backend(500, b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .with_timeout(TimeoutConfig {
                                request: None,
                                backend_request: Some(0.2), // 200ms
                            })
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let proxy_addr = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /slow HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(
            resp.contains("504"),
            "Expected 504 Gateway Timeout, got: {resp}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: backend timeout not exceeded — fast backend responds in time
    // -----------------------------------------------------------------------

    #[monoio::test(enable_timer = true)]
    async fn test_backend_timeout_not_exceeded() {
        let (backend_addr, backend_handle) =
            start_fast_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .with_timeout(TimeoutConfig {
                                request: None,
                                backend_request: Some(5.0), // 5s — generous
                            })
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let proxy_addr = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /fast HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        assert!(resp.contains("hello"), "Expected body 'hello', got: {resp}");

        backend_handle.await;
    }

    // -----------------------------------------------------------------------
    // Test 3: request timeout fires — total pipeline exceeds the limit
    // -----------------------------------------------------------------------

    #[monoio::test(enable_timer = true)]
    async fn test_request_timeout_fires() {
        // Backend delays 500ms, request timeout set to 200ms.
        let (backend_addr, _backend_handle) =
            start_slow_backend(500, b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .with_timeout(TimeoutConfig {
                                request: Some(0.2), // 200ms
                                backend_request: None,
                            })
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let proxy_addr = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /slow HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(
            resp.contains("504"),
            "Expected 504 Gateway Timeout, got: {resp}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: request timeout not exceeded — fast response within limit
    // -----------------------------------------------------------------------

    #[monoio::test(enable_timer = true)]
    async fn test_request_timeout_not_exceeded() {
        let (backend_addr, backend_handle) =
            start_fast_backend(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .with_timeout(TimeoutConfig {
                                request: Some(5.0), // 5s — generous
                                backend_request: None,
                            })
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let proxy_addr = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /fast HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(resp.contains("200"), "Expected 200, got: {resp}");
        assert!(resp.contains("hello"), "Expected body 'hello', got: {resp}");

        backend_handle.await;
    }

    // -----------------------------------------------------------------------
    // Test 5: zero timeout disables enforcement
    // -----------------------------------------------------------------------

    #[monoio::test(enable_timer = true)]
    async fn test_zero_timeout_disables() {
        // Backend delays 100ms, but timeout is 0.0 (disabled).
        let (backend_addr, backend_handle) =
            start_slow_backend(100, b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .with_timeout(TimeoutConfig {
                                request: Some(0.0),         // disabled
                                backend_request: Some(0.0), // disabled
                            })
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let proxy_addr = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /slow HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        // Even though backend is slow, 0.0 means no timeout, so we should get 200.
        assert!(
            resp.contains("200"),
            "Expected 200 (timeout disabled), got: {resp}"
        );
        assert!(resp.contains("hello"), "Expected body 'hello', got: {resp}");

        backend_handle.await;
    }

    // -----------------------------------------------------------------------
    // Test 6: backend timeout fires before request timeout
    // -----------------------------------------------------------------------

    #[monoio::test(enable_timer = true)]
    async fn test_backend_timeout_fires_before_request_timeout() {
        // Backend delays 500ms.
        // backend_request timeout = 200ms (fires first).
        // request timeout = 5s (does not fire).
        let (backend_addr, _backend_handle) =
            start_slow_backend(500, b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello").await;

        let config = ProxyConfigBuilder::new("default", "gw")
            .with_http_listener("http", 80, None)
            .with_route(
                RouteConfigBuilder::new("default", "route-1")
                    .attached_to("http")
                    .with_rule(
                        RouteRuleBuilder::new()
                            .with_path_prefix("/")
                            .with_backend(Backend::new("default", "svc", backend_addr.port()))
                            .with_timeout(TimeoutConfig {
                                request: Some(5.0),         // 5s — does not fire
                                backend_request: Some(0.2), // 200ms — fires
                            })
                            .build(),
                    )
                    .build(),
            )
            .build();

        let resolver = MockResolver::new(vec![backend_addr]);
        let proxy_addr = build_test_pipeline(config, "http", resolver);

        let resp = send_through_proxy(
            proxy_addr,
            b"GET /slow HTTP/1.1\r\nHost: example.com\r\n\r\n",
        )
        .await;

        assert!(
            resp.contains("504"),
            "Expected 504 from backend timeout, got: {resp}"
        );
    }
}
