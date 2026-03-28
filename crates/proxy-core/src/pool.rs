// Connection pool integration (monoio-transports PooledConnector).
//
// Wraps monoio-transports PooledConnector and ConnectionPool to manage
// persistent upstream connections. Implements the Poolable trait for
// connection lifecycle management and health checking.

use std::{
    io,
    net::SocketAddr,
    ops::{Deref, DerefMut},
};

use monoio::io::{AsyncReadRent, AsyncWriteRent, Split};
use monoio::net::TcpStream;

// Re-export pool primitives from monoio-transports.
pub use monoio_transports::pool::{ConnectionPool, Poolable, Pooled, PooledConnector};

use monoio_transports::connectors::Connector;

/// Default maximum number of idle connections per address.
const DEFAULT_MAX_IDLE_PER_ADDR: usize = 16;

/// A TCP connection wrapper that implements [`Poolable`] with liveness
/// checking via `peer_addr()`.
///
/// When the underlying file descriptor is closed, `peer_addr()` returns
/// an error, which [`Poolable::is_open`] maps to `false`. This lets the
/// pool discard stale connections before handing them to callers.
pub struct PoolableConnection {
    stream: TcpStream,
}

impl PoolableConnection {
    /// Wrap a [`TcpStream`] for pool management.
    pub fn new(stream: TcpStream) -> Self {
        Self { stream }
    }

    /// Return a reference to the underlying [`TcpStream`].
    pub fn inner(&self) -> &TcpStream {
        &self.stream
    }
}

impl Poolable for PoolableConnection {
    /// Best-effort liveness check via `peer_addr()`.
    ///
    /// Note: monoio caches the peer address, so this will not detect a
    /// remotely-closed connection until the next I/O attempt. The pool
    /// may occasionally hand out a stale connection; callers should
    /// handle the resulting I/O error by retrying with a fresh one.
    fn is_open(&self) -> bool {
        self.stream.peer_addr().is_ok()
    }
}

impl Deref for PoolableConnection {
    type Target = TcpStream;

    fn deref(&self) -> &TcpStream {
        &self.stream
    }
}

impl DerefMut for PoolableConnection {
    fn deref_mut(&mut self) -> &mut TcpStream {
        &mut self.stream
    }
}

// SAFETY: TcpStream is splittable in monoio.
unsafe impl Split for PoolableConnection {}

impl AsyncReadRent for PoolableConnection {
    fn read<T: monoio::buf::IoBufMut>(
        &mut self,
        buf: T,
    ) -> impl std::future::Future<Output = monoio::BufResult<usize, T>> {
        self.stream.read(buf)
    }

    fn readv<T: monoio::buf::IoVecBufMut>(
        &mut self,
        buf: T,
    ) -> impl std::future::Future<Output = monoio::BufResult<usize, T>> {
        self.stream.readv(buf)
    }
}

impl AsyncWriteRent for PoolableConnection {
    fn write<T: monoio::buf::IoBuf>(
        &mut self,
        buf: T,
    ) -> impl std::future::Future<Output = monoio::BufResult<usize, T>> {
        self.stream.write(buf)
    }

    fn writev<T: monoio::buf::IoVecBuf>(
        &mut self,
        buf_vec: T,
    ) -> impl std::future::Future<Output = monoio::BufResult<usize, T>> {
        self.stream.writev(buf_vec)
    }

    fn flush(&mut self) -> impl std::future::Future<Output = io::Result<()>> {
        self.stream.flush()
    }

    fn shutdown(&mut self) -> impl std::future::Future<Output = io::Result<()>> {
        self.stream.shutdown()
    }
}

/// A [`Connector`] that wraps [`TcpStream`] connections in
/// [`PoolableConnection`] so they satisfy the pool's [`Poolable`] bound.
///
/// Compose this with [`PooledConnector`] to get pooled TCP connections:
///
/// ```ignore
/// let connector = PoolableTcpConnector::default();
/// let pool = ConnectionPool::new(Some(16));
/// let pooled = PooledConnector::new(connector, pool);
/// ```
#[derive(Debug, Clone, Default)]
pub struct PoolableTcpConnector {
    inner: monoio_transports::connectors::TcpConnector,
}

impl Connector<SocketAddr> for PoolableTcpConnector {
    type Connection = PoolableConnection;
    type Error = io::Error;

    async fn connect(&self, key: SocketAddr) -> Result<PoolableConnection, io::Error> {
        let stream = self.inner.connect(key).await?;
        Ok(PoolableConnection::new(stream))
    }
}

/// Type alias for a pooled TCP connector keyed by [`SocketAddr`].
///
/// Connections obtained from this connector are returned to the pool on drop
/// (if still open), avoiding the cost of re-establishing TCP handshakes.
pub type TcpConnectionPool = PooledConnector<PoolableTcpConnector, SocketAddr, PoolableConnection>;

/// Create a [`TcpConnectionPool`] with sensible defaults.
///
/// Defaults:
/// - Max idle connections: 16 per address
pub fn new_tcp_pool() -> TcpConnectionPool {
    let connector = PoolableTcpConnector::default();
    let pool = ConnectionPool::new(Some(DEFAULT_MAX_IDLE_PER_ADDR));
    PooledConnector::new(connector, pool)
}

/// Create a [`TcpConnectionPool`] with a custom max-idle-per-address limit.
pub fn new_tcp_pool_with_max_idle(max_idle: usize) -> TcpConnectionPool {
    let connector = PoolableTcpConnector::default();
    let pool = ConnectionPool::new(Some(max_idle));
    PooledConnector::new(connector, pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use monoio::io::{AsyncReadRentExt, AsyncWriteRentExt};
    use monoio::net::TcpListener;

    #[test]
    fn open_connection_reports_open() {
        // Use a real runtime to create a real fd, then check liveness
        // synchronously via Poolable::is_open.
        let mut rt = monoio::RuntimeBuilder::<monoio::FusionDriver>::new()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let client_handle =
                monoio::spawn(async move { TcpStream::connect(addr).await.unwrap() });

            let (server_stream, _) = listener.accept().await.unwrap();
            let client_stream = client_handle.await;

            let conn = PoolableConnection::new(client_stream);
            assert!(conn.is_open(), "connected socket should report open");

            drop(server_stream);
        });
    }

    #[monoio::test]
    async fn pool_creates_new_connection_when_empty() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let pool = new_tcp_pool();

        let connect_handle = monoio::spawn(async move { pool.connect(addr).await.unwrap() });

        let (_server_stream, _) = listener.accept().await.unwrap();
        let conn = connect_handle.await;

        // First connection should not be reused.
        assert!(!conn.is_reused(), "first connection should not be reused");
    }

    #[monoio::test]
    async fn pool_reuses_returned_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let pool = new_tcp_pool();

        // --- First connection ---
        let pool_clone = pool.clone();
        let connect_handle = monoio::spawn(async move { pool_clone.connect(addr).await.unwrap() });

        let (server_stream_1, _) = listener.accept().await.unwrap();
        let conn1 = connect_handle.await;
        assert!(!conn1.is_reused());

        // Return the connection to the pool by dropping it.
        drop(conn1);

        // --- Second connection (same address) ---
        let conn2 = pool.connect(addr).await.unwrap();

        // This should reuse the connection from the pool.
        assert!(conn2.is_reused(), "second connection should be reused");

        // Verify the connection is still functional by doing a round-trip.
        let mut conn2 = conn2;
        let write_handle = monoio::spawn(async move {
            let (result, _) = conn2.write_all(b"ping".to_vec()).await;
            result.unwrap();
            conn2
        });

        let mut server_stream_1 = server_stream_1;
        let buf = vec![0u8; 4];
        let (result, buf) = server_stream_1.read_exact(buf).await;
        result.unwrap();
        assert_eq!(&buf, b"ping");

        let _conn2 = write_handle.await;
    }

    #[monoio::test]
    async fn connection_reuse_across_multiple_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let pool = new_tcp_pool();

        // Accept one connection from the server side.
        let pool_clone = pool.clone();
        let connect_handle = monoio::spawn(async move { pool_clone.connect(addr).await.unwrap() });

        let (mut server_stream, _) = listener.accept().await.unwrap();
        let conn = connect_handle.await;
        assert!(!conn.is_reused());
        drop(conn);

        // Re-acquire from pool three times and verify reuse + data.
        for i in 0u8..3 {
            let mut conn = pool.connect(addr).await.unwrap();
            assert!(conn.is_reused(), "iteration {i} should reuse");

            let msg = vec![i; 1];
            let write_handle = monoio::spawn(async move {
                let (result, _) = conn.write_all(msg).await;
                result.unwrap();
                conn
            });

            let buf = vec![0u8; 1];
            let (result, buf) = server_stream.read_exact(buf).await;
            result.unwrap();
            assert_eq!(buf[0], i);

            let conn = write_handle.await;
            // Return to pool.
            drop(conn);
        }
    }
}
