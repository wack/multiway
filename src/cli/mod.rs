use controller::Controller;
use gateway::Gateway;
use version::Version;

mod colors;
mod controller;
mod gateway;
mod version;

pub use colors::EnableColors;

use clap::{Parser, Subcommand};
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

#[derive(Debug, Subcommand, Clone)]
pub enum CliCommand {
    /// Print the CLI version and exit
    Version,
    /// Run the gateway
    Gateway,
    /// Run the controller
    Controller,
}

impl CliCommand {
    pub fn dispatch(self) -> miette::Result<()> {
        match self {
            Self::Version => Version::new().dispatch(),
            Self::Gateway => Gateway::new().dispatch(),
            Self::Controller => Controller::new().dispatch(),
        }
    }
}
