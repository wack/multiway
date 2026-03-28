//! Configuration types for the Gateway data plane.
//!
//! Re-exports from `proxy_core::config` since the data plane and proxy-core
//! share the same JSON configuration schema. The `GatewayConfig` type alias
//! provides backward compatibility with existing code that references the
//! control plane's configuration type name.

/// Type alias for backward compatibility.
///
/// The control plane writes a `GatewayConfig` JSON blob into a ConfigMap.
/// proxy-core calls the same schema `ProxyConfig`. This alias lets the
/// data plane binary continue using the `GatewayConfig` name while
/// actually deserializing into `proxy_core::config::ProxyConfig`.
pub type GatewayConfig = proxy_core::config::ProxyConfig;
