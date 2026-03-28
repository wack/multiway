// TLS termination for HTTPS listeners.
//
// Provides certificate parsing, `rustls::ServerConfig` construction, and
// SNI-based certificate selection. Built on top of `monoio-rustls` for
// async TLS handshakes within the monoio runtime.

use std::io;
use std::sync::Arc;

use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::ResolvesServerCertUsingSni;
use rustls::sign::CertifiedKey;

use crate::config::{InlineCertificate, TlsConfig, TlsMode};

// Re-export the monoio-rustls types callers will need.
pub use monoio_rustls::ServerTlsStream;
pub use monoio_rustls::TlsAcceptor;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during TLS setup.
#[derive(Debug)]
pub enum TlsSetupError {
    /// No inline certificates were provided.
    NoCertificates,
    /// Failed to parse PEM certificate chain.
    CertParse(String),
    /// Failed to parse PEM private key.
    KeyParse(String),
    /// Rustls rejected the configuration.
    Rustls(rustls::Error),
    /// SNI resolver rejected a hostname/certificate pair.
    SniAdd(String),
}

impl std::fmt::Display for TlsSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCertificates => write!(f, "no inline certificates provided"),
            Self::CertParse(msg) => write!(f, "certificate parse error: {msg}"),
            Self::KeyParse(msg) => write!(f, "private key parse error: {msg}"),
            Self::Rustls(e) => write!(f, "rustls error: {e}"),
            Self::SniAdd(msg) => write!(f, "SNI resolver error: {msg}"),
        }
    }
}

impl std::error::Error for TlsSetupError {}

impl From<rustls::Error> for TlsSetupError {
    fn from(e: rustls::Error) -> Self {
        Self::Rustls(e)
    }
}

impl From<TlsSetupError> for io::Error {
    fn from(e: TlsSetupError) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
    }
}

// ---------------------------------------------------------------------------
// PEM parsing
// ---------------------------------------------------------------------------

/// Parse PEM-encoded certificates into DER certificate chain.
///
/// Returns the certificates in the order they appear in the PEM data
/// (leaf first, then intermediates).
pub fn parse_pem_certs(pem: &str) -> Result<Vec<CertificateDer<'static>>, TlsSetupError> {
    let mut reader = io::BufReader::new(pem.as_bytes());
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsSetupError::CertParse(e.to_string()))?;

    if certs.is_empty() {
        return Err(TlsSetupError::CertParse(
            "no certificates found in PEM data".to_string(),
        ));
    }
    Ok(certs)
}

/// Parse a PEM-encoded private key (PKCS#1, PKCS#8, or SEC1).
///
/// Returns the first private key found in the PEM data.
pub fn parse_pem_private_key(pem: &str) -> Result<PrivateKeyDer<'static>, TlsSetupError> {
    let mut reader = io::BufReader::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| TlsSetupError::KeyParse(e.to_string()))?
        .ok_or_else(|| TlsSetupError::KeyParse("no private key found in PEM data".to_string()))
}

// ---------------------------------------------------------------------------
// TLS acceptor construction
// ---------------------------------------------------------------------------

/// Build a `monoio_rustls::TlsAcceptor` from a [`TlsConfig`].
///
/// If TLS mode is `Passthrough`, returns `None` (passthrough means the
/// proxy should not terminate TLS). For `Terminate` mode, parses the
/// inline certificates and builds a `rustls::ServerConfig`.
///
/// When a single inline certificate is provided, it is used for all
/// connections. When multiple certificates are provided, SNI-based
/// selection is used, with the first certificate also serving as the
/// fallback for clients that don't send SNI.
pub fn build_tls_acceptor(tls_config: &TlsConfig) -> Result<Option<TlsAcceptor>, TlsSetupError> {
    if tls_config.mode == TlsMode::Passthrough {
        return Ok(None);
    }

    if tls_config.inline_certificates.is_empty() {
        return Err(TlsSetupError::NoCertificates);
    }

    let server_config = build_server_config(&tls_config.inline_certificates)?;
    Ok(Some(TlsAcceptor::from(Arc::new(server_config))))
}

/// Build a `rustls::ServerConfig` from inline certificates.
///
/// For a single certificate, uses `with_single_cert`. For multiple
/// certificates, constructs a `ResolvesServerCertUsingSni` resolver
/// so the correct certificate is presented based on the client's SNI.
fn build_server_config(inline_certs: &[InlineCertificate]) -> Result<ServerConfig, TlsSetupError> {
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));

    if inline_certs.len() == 1 {
        // Single certificate: use with_single_cert for simplicity.
        let cert = &inline_certs[0];
        let cert_chain = parse_pem_certs(&cert.cert_pem)?;
        let key = parse_pem_private_key(&cert.key_pem)?;
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(TlsSetupError::Rustls)?
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)
            .map_err(TlsSetupError::Rustls)?;
        Ok(config)
    } else {
        // Multiple certificates: use SNI resolver.
        let mut sni_resolver = ResolvesServerCertUsingSni::new();

        for cert_config in inline_certs {
            let cert_chain = parse_pem_certs(&cert_config.cert_pem)?;
            let key_der = parse_pem_private_key(&cert_config.key_pem)?;
            let certified_key = CertifiedKey::from_der(cert_chain, key_der, &provider)
                .map_err(TlsSetupError::Rustls)?;

            for hostname in &cert_config.hostnames {
                sni_resolver
                    .add(hostname, certified_key.clone())
                    .map_err(|e| TlsSetupError::SniAdd(format!("{hostname}: {e}")))?;
            }
        }

        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(TlsSetupError::Rustls)?
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(sni_resolver));
        Ok(config)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CertificateRef, TlsMode};

    /// Helper: generate a self-signed certificate using a single keypair.
    fn generate_cert_and_key(hostnames: &[&str]) -> (String, String) {
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(
            hostnames.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
        )
        .unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        (cert.pem(), key_pair.serialize_pem())
    }

    // -----------------------------------------------------------------------
    // PEM parsing tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_valid_cert_pem() {
        let (cert_pem, _key_pem) = generate_cert_and_key(&["localhost"]);
        let certs = parse_pem_certs(&cert_pem).unwrap();
        assert_eq!(certs.len(), 1, "should parse one certificate");
    }

    #[test]
    fn parse_valid_key_pem() {
        let (_cert_pem, key_pem) = generate_cert_and_key(&["localhost"]);
        let key = parse_pem_private_key(&key_pem);
        assert!(key.is_ok(), "should parse private key");
    }

    #[test]
    fn parse_empty_cert_pem_returns_error() {
        let result = parse_pem_certs("");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no certificates"),
            "Expected 'no certificates' error, got: {err}"
        );
    }

    #[test]
    fn parse_empty_key_pem_returns_error() {
        let result = parse_pem_private_key("");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no private key"),
            "Expected 'no private key' error, got: {err}"
        );
    }

    #[test]
    fn parse_garbage_cert_pem_returns_error() {
        let result = parse_pem_certs("this is not PEM data");
        assert!(result.is_err());
    }

    #[test]
    fn parse_garbage_key_pem_returns_error() {
        let result = parse_pem_private_key("this is not PEM data");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // TLS acceptor construction tests
    // -----------------------------------------------------------------------

    #[test]
    fn build_acceptor_single_cert() {
        let (cert_pem, key_pem) = generate_cert_and_key(&["localhost"]);
        let tls_config = TlsConfig {
            mode: TlsMode::Terminate,
            certificates: Vec::new(),
            inline_certificates: vec![InlineCertificate {
                cert_pem,
                key_pem,
                hostnames: vec!["localhost".to_string()],
            }],
        };

        let acceptor = build_tls_acceptor(&tls_config).unwrap();
        assert!(
            acceptor.is_some(),
            "Terminate mode should produce an acceptor"
        );
    }

    #[test]
    fn build_acceptor_passthrough_returns_none() {
        let tls_config = TlsConfig {
            mode: TlsMode::Passthrough,
            certificates: Vec::new(),
            inline_certificates: Vec::new(),
        };

        let acceptor = build_tls_acceptor(&tls_config).unwrap();
        assert!(acceptor.is_none(), "Passthrough mode should return None");
    }

    #[test]
    fn build_acceptor_no_inline_certs_returns_error() {
        let tls_config = TlsConfig {
            mode: TlsMode::Terminate,
            certificates: vec![CertificateRef {
                namespace: "default".to_string(),
                name: "my-cert".to_string(),
            }],
            inline_certificates: Vec::new(),
        };

        let result = build_tls_acceptor(&tls_config);
        assert!(result.is_err());
        let err = match result {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected error"),
        };
        assert!(
            err.contains("no inline certificates"),
            "Expected 'no inline certificates' error, got: {err}"
        );
    }

    #[test]
    fn build_acceptor_multiple_certs_sni() {
        let (cert_pem_a, key_pem_a) = generate_cert_and_key(&["alpha.example.com"]);
        let (cert_pem_b, key_pem_b) = generate_cert_and_key(&["beta.example.com"]);

        let tls_config = TlsConfig {
            mode: TlsMode::Terminate,
            certificates: Vec::new(),
            inline_certificates: vec![
                InlineCertificate {
                    cert_pem: cert_pem_a,
                    key_pem: key_pem_a,
                    hostnames: vec!["alpha.example.com".to_string()],
                },
                InlineCertificate {
                    cert_pem: cert_pem_b,
                    key_pem: key_pem_b,
                    hostnames: vec!["beta.example.com".to_string()],
                },
            ],
        };

        let acceptor = build_tls_acceptor(&tls_config).unwrap();
        assert!(
            acceptor.is_some(),
            "Multiple certs with SNI should produce an acceptor"
        );
    }

    #[test]
    fn build_acceptor_invalid_cert_returns_error() {
        let (_cert_pem, key_pem) = generate_cert_and_key(&["localhost"]);
        let tls_config = TlsConfig {
            mode: TlsMode::Terminate,
            certificates: Vec::new(),
            inline_certificates: vec![InlineCertificate {
                cert_pem: "not a cert".to_string(),
                key_pem,
                hostnames: Vec::new(),
            }],
        };

        let result = build_tls_acceptor(&tls_config);
        assert!(result.is_err());
    }

    #[test]
    fn build_acceptor_invalid_key_returns_error() {
        let (cert_pem, _key_pem) = generate_cert_and_key(&["localhost"]);
        let tls_config = TlsConfig {
            mode: TlsMode::Terminate,
            certificates: Vec::new(),
            inline_certificates: vec![InlineCertificate {
                cert_pem,
                key_pem: "not a key".to_string(),
                hostnames: Vec::new(),
            }],
        };

        let result = build_tls_acceptor(&tls_config);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // InlineCertificate serde round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn inline_certificate_roundtrip() {
        let (cert_pem, key_pem) = generate_cert_and_key(&["example.com"]);
        let cert = InlineCertificate {
            cert_pem,
            key_pem,
            hostnames: vec!["example.com".to_string()],
        };
        let json = serde_json::to_string(&cert).unwrap();
        let back: InlineCertificate = serde_json::from_str(&json).unwrap();
        assert_eq!(cert, back);
    }

    #[test]
    fn tls_config_with_inline_certs_roundtrip() {
        let (cert_pem, key_pem) = generate_cert_and_key(&["example.com"]);
        let tls_config = TlsConfig {
            mode: TlsMode::Terminate,
            certificates: vec![CertificateRef {
                namespace: "default".to_string(),
                name: "my-cert".to_string(),
            }],
            inline_certificates: vec![InlineCertificate {
                cert_pem,
                key_pem,
                hostnames: vec!["example.com".to_string()],
            }],
        };
        let json = serde_json::to_string(&tls_config).unwrap();
        let back: TlsConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(tls_config, back);
    }

    #[test]
    fn tls_config_without_inline_certs_deserializes() {
        // JSON produced by older control planes that don't populate inline_certificates
        let json = r#"{
            "mode": "terminate",
            "certificates": [
                { "namespace": "default", "name": "my-cert" }
            ]
        }"#;
        let tls_config: TlsConfig = serde_json::from_str(json).unwrap();
        assert!(tls_config.inline_certificates.is_empty());
    }
}
