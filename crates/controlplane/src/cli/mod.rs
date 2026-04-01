use controller::Controller;
use gateway::Gateway;
use version::Version;

mod colors;
mod controller;
mod gateway;
mod version;

pub use colors::EnableColors;

use clap::{Args, Parser, Subcommand};
use tracing::level_filters::LevelFilter;

#[derive(Debug, Default, Clone, clap::ValueEnum)]
pub enum LogFormat {
    #[default]
    Text,
    JSON,
}

#[derive(Debug, Parser, Clone)]
pub struct Cli {
    #[arg(long, env = "LOG_LEVEL", default_value = "info", global = true)]
    pub log_level: LevelFilter,

    #[arg(long, env = "LOG_FORMAT", default_value = "text", global = true)]
    pub log_format: LogFormat,

    #[arg(long, default_value = "auto", global = true)]
    pub enable_colors: EnableColors,

    #[command(subcommand)]
    pub cmd: Option<CliCommand>,
}

/// Arguments for the controller command
#[derive(Debug, Args, Clone)]
pub struct ControllerArgs {
    /// Data plane image to use for Gateway deployments
    #[arg(
        long,
        env = "DATAPLANE_IMAGE",
        default_value = "ghcr.io/wack/multiway-dataplane:latest"
    )]
    pub dataplane_image: Option<String>,

    /// Number of replicas for data plane deployments
    #[arg(long, env = "DEFAULT_REPLICAS", default_value = "1")]
    pub replicas: Option<i32>,

    /// Namespace to watch (default: all namespaces)
    #[arg(long, env = "WATCH_NAMESPACE")]
    pub namespace: Option<String>,
}

/// Arguments for the gateway (data plane) command
#[derive(Debug, Args, Clone)]
pub struct GatewayArgs {
    /// Path to the configuration file
    #[arg(long, env = "CONFIG_PATH", default_value = "/config/config.json")]
    pub config_path: String,

    /// Gateway name (for logging)
    #[arg(long, env = "GATEWAY_NAME")]
    pub gateway_name: Option<String>,

    /// Gateway namespace (for logging)
    #[arg(long, env = "GATEWAY_NAMESPACE")]
    pub gateway_namespace: Option<String>,
}

#[derive(Debug, Subcommand, Clone)]
pub enum CliCommand {
    /// Print the CLI version and exit
    Version,
    /// Run the gateway data plane (proxy-core HTTP proxy)
    Gateway(GatewayArgs),
    /// Run the Gateway API controller (watches GatewayClass, Gateway, HTTPRoute)
    Controller(ControllerArgs),
}

impl CliCommand {
    pub fn dispatch(self) -> miette::Result<()> {
        match self {
            Self::Version => Version::new().dispatch(),
            Self::Gateway(args) => {
                let mut gw = Gateway::new();
                gw.config_path = args.config_path;
                gw.gateway_name = args.gateway_name;
                gw.gateway_namespace = args.gateway_namespace;
                gw.dispatch()
            }
            Self::Controller(args) => {
                let mut ctrl = Controller::new();
                ctrl.dataplane_image = args.dataplane_image;
                ctrl.replicas = args.replicas;
                ctrl.namespace = args.namespace;
                ctrl.dispatch()
            }
        }
    }
}
