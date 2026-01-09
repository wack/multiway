//! Error types for the Gateway API controller
//!
//! This module provides comprehensive error handling using `thiserror` for
//! structured error types and `miette` for user-friendly error reports.

use miette::Diagnostic;
use thiserror::Error;

/// The main error type for the Gateway controller
#[derive(Error, Diagnostic, Debug)]
pub enum ControllerError {
    /// Error communicating with Kubernetes API
    #[error("Kubernetes API error: {0}")]
    #[diagnostic(code(multiway::kubernetes_error))]
    KubernetesError(#[from] kube::Error),

    /// Error serializing/deserializing JSON
    #[error("JSON serialization error: {0}")]
    #[diagnostic(code(multiway::json_error))]
    JsonError(#[from] serde_json::Error),

    /// GatewayClass not found
    #[error("GatewayClass '{name}' not found")]
    #[diagnostic(
        code(multiway::gateway_class_not_found),
        help("Ensure the GatewayClass resource exists and is spelled correctly")
    )]
    GatewayClassNotFound { name: String },

    /// GatewayClass not accepted
    #[error("GatewayClass '{name}' is not accepted by this controller")]
    #[diagnostic(
        code(multiway::gateway_class_not_accepted),
        help("Check that the GatewayClass has controllerName: io.multiway/gateway-controller")
    )]
    GatewayClassNotAccepted { name: String },

    /// Gateway not found
    #[error("Gateway '{namespace}/{name}' not found")]
    #[diagnostic(code(multiway::gateway_not_found))]
    GatewayNotFound { namespace: String, name: String },

    /// Invalid Gateway configuration
    #[error("Invalid Gateway configuration: {message}")]
    #[diagnostic(code(multiway::invalid_gateway_config))]
    InvalidGatewayConfig { message: String },

    /// Invalid HTTPRoute configuration
    #[error("Invalid HTTPRoute configuration: {message}")]
    #[diagnostic(code(multiway::invalid_httproute_config))]
    InvalidHttpRouteConfig { message: String },

    /// Backend service not found
    #[error("Backend service '{namespace}/{name}' not found")]
    #[diagnostic(
        code(multiway::backend_not_found),
        help("Ensure the backend Service exists in the specified namespace")
    )]
    BackendNotFound { namespace: String, name: String },

    /// ReferenceGrant required for cross-namespace reference
    #[error(
        "Cross-namespace reference from {from_namespace} to {to_namespace}/{to_name} requires a ReferenceGrant"
    )]
    #[diagnostic(
        code(multiway::reference_grant_required),
        help("Create a ReferenceGrant in the target namespace to allow this reference")
    )]
    ReferenceGrantRequired {
        from_namespace: String,
        to_namespace: String,
        to_name: String,
    },

    /// Listener protocol not supported
    #[error("Listener protocol '{protocol}' is not supported")]
    #[diagnostic(
        code(multiway::unsupported_protocol),
        help("Supported protocols: HTTP, HTTPS")
    )]
    UnsupportedProtocol { protocol: String },

    /// TLS configuration error
    #[error("TLS configuration error: {message}")]
    #[diagnostic(code(multiway::tls_config_error))]
    TlsConfigError { message: String },

    /// Secret not found
    #[error("Secret '{namespace}/{name}' not found")]
    #[diagnostic(code(multiway::secret_not_found))]
    SecretNotFound { namespace: String, name: String },

    /// Invalid secret format
    #[error("Secret '{namespace}/{name}' has invalid format: {message}")]
    #[diagnostic(code(multiway::invalid_secret_format))]
    InvalidSecretFormat {
        namespace: String,
        name: String,
        message: String,
    },

    /// Resource conflict
    #[error("Resource conflict: {message}")]
    #[diagnostic(code(multiway::resource_conflict))]
    ResourceConflict { message: String },

    /// Internal error
    #[error("Internal error: {message}")]
    #[diagnostic(code(multiway::internal_error))]
    InternalError { message: String },

    /// Configuration parsing error
    #[error("Configuration parsing error: {message}")]
    #[diagnostic(code(multiway::config_parse_error))]
    ConfigParseError { message: String },
}

impl ControllerError {
    /// Create a new GatewayClassNotFound error
    pub fn gateway_class_not_found(name: impl Into<String>) -> Self {
        Self::GatewayClassNotFound { name: name.into() }
    }

    /// Create a new GatewayClassNotAccepted error
    pub fn gateway_class_not_accepted(name: impl Into<String>) -> Self {
        Self::GatewayClassNotAccepted { name: name.into() }
    }

    /// Create a new GatewayNotFound error
    pub fn gateway_not_found(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self::GatewayNotFound {
            namespace: namespace.into(),
            name: name.into(),
        }
    }

    /// Create a new InvalidGatewayConfig error
    pub fn invalid_gateway_config(message: impl Into<String>) -> Self {
        Self::InvalidGatewayConfig {
            message: message.into(),
        }
    }

    /// Create a new InvalidHttpRouteConfig error
    pub fn invalid_httproute_config(message: impl Into<String>) -> Self {
        Self::InvalidHttpRouteConfig {
            message: message.into(),
        }
    }

    /// Create a new BackendNotFound error
    pub fn backend_not_found(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self::BackendNotFound {
            namespace: namespace.into(),
            name: name.into(),
        }
    }

    /// Create a new ReferenceGrantRequired error
    pub fn reference_grant_required(
        from_namespace: impl Into<String>,
        to_namespace: impl Into<String>,
        to_name: impl Into<String>,
    ) -> Self {
        Self::ReferenceGrantRequired {
            from_namespace: from_namespace.into(),
            to_namespace: to_namespace.into(),
            to_name: to_name.into(),
        }
    }

    /// Create a new UnsupportedProtocol error
    pub fn unsupported_protocol(protocol: impl Into<String>) -> Self {
        Self::UnsupportedProtocol {
            protocol: protocol.into(),
        }
    }

    /// Create a new TlsConfigError error
    pub fn tls_config_error(message: impl Into<String>) -> Self {
        Self::TlsConfigError {
            message: message.into(),
        }
    }

    /// Create a new SecretNotFound error
    pub fn secret_not_found(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self::SecretNotFound {
            namespace: namespace.into(),
            name: name.into(),
        }
    }

    /// Create a new InvalidSecretFormat error
    pub fn invalid_secret_format(
        namespace: impl Into<String>,
        name: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::InvalidSecretFormat {
            namespace: namespace.into(),
            name: name.into(),
            message: message.into(),
        }
    }

    /// Create a new ResourceConflict error
    pub fn resource_conflict(message: impl Into<String>) -> Self {
        Self::ResourceConflict {
            message: message.into(),
        }
    }

    /// Create a new InternalError
    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::InternalError {
            message: message.into(),
        }
    }
}

/// Result type alias for controller operations
pub type Result<T> = std::result::Result<T, ControllerError>;
