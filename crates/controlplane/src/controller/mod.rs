//! Gateway API Controller
//!
//! This module implements a production-ready Kubernetes Gateway API controller.
//! It manages GatewayClass, Gateway, and HTTPRoute resources according to the
//! Gateway API specification.
//!
//! # Architecture
//!
//! The controller consists of three reconcilers:
//! - **GatewayClass Reconciler**: Validates and accepts GatewayClass resources
//! - **Gateway Reconciler**: Provisions data plane deployments for each Gateway
//! - **HTTPRoute Reconciler**: Updates ConfigMaps with routing configuration
//!
//! The data plane is communicated with via ConfigMaps that contain the routing
//! configuration in a JSON format.

pub mod config;
pub mod context;
pub mod error;
pub mod gateway;
pub mod gateway_class;
pub mod httproute;
pub mod reconciler;

pub use config::*;
pub use context::*;
pub use error::*;
pub use reconciler::*;
