//! Multiway Gateway Data Plane
//!
//! This is the data plane component of the Multiway Gateway API implementation.
//! It uses CloudFlare's Pingora library to provide high-performance HTTP proxying.
//!
//! # Architecture
//!
//! The data plane reads its configuration from a file (typically mounted from a
//! ConfigMap in Kubernetes). It supports hot-reload when the configuration changes.
//!
//! # Configuration
//!
//! The configuration file is in JSON format and contains:
//! - Listeners: ports and protocols to listen on
//! - Routes: HTTP routing rules with matches, filters, and backends

mod config;
mod proxy;
mod router;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use config::GatewayConfig;
use proxy::GatewayProxy;

/// Multiway Gateway Data Plane
#[derive(Parser, Debug)]
#[command(name = "multiway-dataplane")]
#[command(about = "Pingora-based HTTP proxy for Kubernetes Gateway API")]
struct Args {
    /// Path to the configuration file
    #[arg(long, env = "CONFIG_PATH", default_value = "/config/config.json")]
    config_path: PathBuf,

    /// Gateway name (for logging)
    #[arg(long, env = "GATEWAY_NAME")]
    gateway_name: Option<String>,

    /// Gateway namespace (for logging)
    #[arg(long, env = "GATEWAY_NAMESPACE")]
    gateway_namespace: Option<String>,

    /// Log level
    #[arg(long, env = "LOG_LEVEL", default_value = "info")]
    log_level: String,

    /// Enable JSON logging
    #[arg(long, env = "LOG_JSON", default_value = "false")]
    log_json: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    init_logging(&args.log_level, args.log_json)?;

    info!(
        config_path = %args.config_path.display(),
        gateway_name = ?args.gateway_name,
        gateway_namespace = ?args.gateway_namespace,
        "Starting Multiway Gateway Data Plane"
    );

    // Load initial configuration
    let config = load_config(&args.config_path)?;
    let config_state = Arc::new(ArcSwap::from_pointee(config));

    info!(
        listeners = config_state.load().listeners.len(),
        routes = config_state.load().routes.len(),
        "Configuration loaded"
    );

    // Start configuration watcher
    let config_path = args.config_path.clone();
    let config_state_clone = config_state.clone();
    std::thread::spawn(move || {
        if let Err(e) = watch_config(config_path, config_state_clone) {
            warn!(error = %e, "Configuration watcher failed");
        }
    });

    // Create and run the proxy
    let proxy = GatewayProxy::new(config_state);
    proxy.run()?;

    Ok(())
}

fn init_logging(level: &str, json: bool) -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    if json {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }

    Ok(())
}

fn load_config(path: &PathBuf) -> Result<GatewayConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;

    let config: GatewayConfig = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

    Ok(config)
}

fn watch_config(path: PathBuf, state: Arc<ArcSwap<GatewayConfig>>) -> Result<()> {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;
    use std::time::Duration;

    let (tx, rx) = channel();

    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        Config::default().with_poll_interval(Duration::from_secs(2)),
    )?;

    watcher.watch(&path, RecursiveMode::NonRecursive)?;

    info!(path = %path.display(), "Watching configuration file for changes");

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                if event.kind.is_modify() || event.kind.is_create() {
                    info!("Configuration file changed, reloading");
                    match load_config(&path) {
                        Ok(config) => {
                            state.store(Arc::new(config));
                            info!("Configuration reloaded successfully");
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to reload configuration");
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Watch error");
            }
            Err(e) => {
                warn!(error = %e, "Channel error");
                break;
            }
        }
    }

    Ok(())
}
