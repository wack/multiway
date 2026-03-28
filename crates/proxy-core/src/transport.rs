// Transport layer (Acceptor trait + monoio-transports connectors).
//
// Defines the Acceptor trait for inbound connections and integrates
// monoio-transports TcpConnector for outbound upstream connections.
// Includes a Resolver trait for DNS resolution and ListenerOpts for
// configuring TCP listeners.

use std::{
    io,
    net::{SocketAddr, ToSocketAddrs},
};

use monoio::net::{TcpListener, TcpStream};

// Re-export monoio-transports TcpConnector for outbound connections.
pub use monoio_transports::connectors::{Connector, TcpConnector};

/// Trait for accepting inbound connections on a server-side listener.
pub trait Acceptor {
    /// The stream type returned by this acceptor.
    type Stream;

    /// Accept a single inbound connection, returning the stream and the
    /// remote peer address.
    fn accept(&self) -> impl std::future::Future<Output = io::Result<(Self::Stream, SocketAddr)>>;
}

/// Options for configuring a [`TcpAcceptor`].
#[derive(Debug, Clone)]
pub struct ListenerOpts {
    pub reuse_port: bool,
    pub reuse_addr: bool,
    pub backlog: i32,
    pub tcp_nodelay: bool,
    pub recv_buf_size: Option<usize>,
    pub send_buf_size: Option<usize>,
}

impl Default for ListenerOpts {
    fn default() -> Self {
        Self {
            reuse_port: true,
            reuse_addr: true,
            backlog: 1024,
            tcp_nodelay: true,
            recv_buf_size: None,
            send_buf_size: None,
        }
    }
}

/// An [`Acceptor`] backed by a monoio [`TcpListener`].
pub struct TcpAcceptor {
    listener: TcpListener,
    tcp_nodelay: bool,
}

impl TcpAcceptor {
    /// Bind to `addr` using the given options.
    pub fn bind(addr: SocketAddr, opts: &ListenerOpts) -> io::Result<Self> {
        let mut monoio_opts = monoio::net::ListenerOpts::new()
            .reuse_port(opts.reuse_port)
            .reuse_addr(opts.reuse_addr)
            .backlog(opts.backlog);
        if let Some(size) = opts.send_buf_size {
            monoio_opts = monoio_opts.send_buf_size(size);
        }
        if let Some(size) = opts.recv_buf_size {
            monoio_opts = monoio_opts.recv_buf_size(size);
        }
        let listener = TcpListener::bind_with_config(addr, &monoio_opts)?;
        Ok(Self {
            listener,
            tcp_nodelay: opts.tcp_nodelay,
        })
    }

    /// Returns the local address this acceptor is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

impl Acceptor for TcpAcceptor {
    type Stream = TcpStream;

    async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let (stream, addr) = self.listener.accept().await?;
        if self.tcp_nodelay {
            let _ = stream.set_nodelay(true);
        }
        Ok((stream, addr))
    }
}

/// Trait for resolving a hostname and port into socket addresses.
pub trait Resolver {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
}

/// A [`Resolver`] backed by the standard library's [`ToSocketAddrs`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StdResolver;

impl Resolver for StdResolver {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        (host, port).to_socket_addrs().map(|iter| iter.collect())
    }
}

/// A [`Resolver`] that returns pre-configured addresses, useful for testing.
#[derive(Debug, Clone, Default)]
pub struct MockResolver {
    addrs: Vec<SocketAddr>,
}

impl MockResolver {
    pub fn new(addrs: Vec<SocketAddr>) -> Self {
        Self { addrs }
    }
}

impl Resolver for MockResolver {
    fn resolve(&self, _host: &str, _port: u16) -> io::Result<Vec<SocketAddr>> {
        if self.addrs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "mock resolver has no addresses",
            ));
        }
        Ok(self.addrs.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_resolver_resolves_localhost() {
        let resolver = StdResolver;
        let addrs = resolver.resolve("localhost", 80).unwrap();
        assert!(!addrs.is_empty());
        for addr in &addrs {
            assert_eq!(addr.port(), 80);
        }
    }

    #[test]
    fn mock_resolver_returns_configured_addresses() {
        let expected: Vec<SocketAddr> = vec![
            "127.0.0.1:8080".parse().unwrap(),
            "127.0.0.2:8080".parse().unwrap(),
        ];
        let resolver = MockResolver::new(expected.clone());
        let addrs = resolver.resolve("anything", 9999).unwrap();
        assert_eq!(addrs, expected);
    }

    #[test]
    fn mock_resolver_empty_returns_error() {
        let resolver = MockResolver::default();
        let result = resolver.resolve("host", 80);
        assert!(result.is_err());
    }

    #[monoio::test]
    async fn tcp_acceptor_bind_accept() {
        let opts = ListenerOpts::default();
        let acceptor = TcpAcceptor::bind("127.0.0.1:0".parse().unwrap(), &opts).unwrap();
        let local_addr = acceptor.local_addr().unwrap();

        // Spawn a client connection.
        let client_handle =
            monoio::spawn(async move { TcpStream::connect(local_addr).await.unwrap() });

        let (server_stream, peer_addr) = acceptor.accept().await.unwrap();
        assert_eq!(
            peer_addr.ip(),
            "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
        );
        // Nodelay should be set on the accepted stream.
        assert!(server_stream.nodelay().unwrap());

        let _client_stream = client_handle.await;
    }

    #[monoio::test]
    async fn tcp_connector_connects_to_listener() {
        // Set up a raw monoio listener as the test server.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let connector = TcpConnector::default();

        let connect_handle = monoio::spawn(async move { connector.connect(addr).await.unwrap() });

        let (server_stream, _peer_addr) = listener.accept().await.unwrap();
        let _client_stream = connect_handle.await;

        // Both sides connected successfully — drop confirms clean shutdown.
        drop(server_stream);
    }

    #[monoio::test]
    async fn round_trip_through_accepted_connection() {
        use monoio::io::{AsyncReadRentExt, AsyncWriteRentExt};

        let opts = ListenerOpts::default();
        let acceptor = TcpAcceptor::bind("127.0.0.1:0".parse().unwrap(), &opts).unwrap();
        let local_addr = acceptor.local_addr().unwrap();

        let client_handle = monoio::spawn(async move {
            let mut stream = TcpStream::connect(local_addr).await.unwrap();
            // Send data to the server.
            let (result, _buf) = stream.write_all(b"hello".to_vec()).await;
            result.unwrap();

            // Read the echo back.
            let buf = vec![0u8; 5];
            let (result, buf) = stream.read_exact(buf).await;
            result.unwrap();
            buf
        });

        let (mut server_stream, _addr) = acceptor.accept().await.unwrap();

        // Read the client's message.
        let buf = vec![0u8; 5];
        let (result, buf) = server_stream.read_exact(buf).await;
        result.unwrap();
        assert_eq!(&buf, b"hello");

        // Echo it back.
        let (result, _buf) = server_stream.write_all(b"hello".to_vec()).await;
        result.unwrap();

        let client_buf = client_handle.await;
        assert_eq!(&client_buf, b"hello");
    }
}
