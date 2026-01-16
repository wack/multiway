//! Multiway Gateway Data Plane
//!
//! This is the data plane component of the Multiway Gateway API implementation.
//! It uses CloudFlare's Pingora library to provide high-performance HTTP proxying.
//!
//! # Architecture
//!
//! The data plane reads its configuration from a Kubernetes ConfigMap via the API.
//! This provides immediate notification of ConfigMap updates, bypassing the ~60 second
//! kubelet sync delay that affects mounted ConfigMaps. It supports hot-reload when
//! the configuration changes.
//!
//! # Configuration
//!
//! The configuration is in JSON format and contains:
//! - Listeners: ports and protocols to listen on
//! - Routes: HTTP routing rules with matches, filters, and backends

mod config;
mod proxy;
mod router;

use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use clap::Parser;
use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client, runtime::watcher};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use config::GatewayConfig;
use proxy::GatewayProxy;

/// ConfigMap key for the gateway configuration (matches control plane)
const CONFIG_KEY: &str = "config.json";

/// Multiway Gateway Data Plane
#[derive(Parser, Debug)]
#[command(name = "multiway-dataplane")]
#[command(about = "Pingora-based HTTP proxy for Kubernetes Gateway API")]
struct Args {
    /// Gateway name (required for ConfigMap naming)
    #[arg(long, env = "GATEWAY_NAME")]
    gateway_name: String,

    /// Gateway namespace (required for ConfigMap lookup)
    #[arg(long, env = "GATEWAY_NAMESPACE")]
    gateway_namespace: String,

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

    let configmap_name = format!("multiway-config-{}", args.gateway_name);

    info!(
        gateway_name = %args.gateway_name,
        gateway_namespace = %args.gateway_namespace,
        configmap_name = %configmap_name,
        "Starting Multiway Gateway Data Plane"
    );

    // Create tokio runtime for K8s client
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create tokio runtime")?;

    // Load initial configuration from Kubernetes ConfigMap
    let config = rt.block_on(load_config_from_k8s(
        &args.gateway_namespace,
        &configmap_name,
    ))?;
    let config_state = Arc::new(ArcSwap::from_pointee(config));

    info!(
        listeners = config_state.load().listeners.len(),
        routes = config_state.load().routes.len(),
        "Configuration loaded from ConfigMap"
    );

    // Start configuration watcher (watches ConfigMap via K8s API)
    let namespace = args.gateway_namespace.clone();
    let cm_name = configmap_name.clone();
    let config_state_clone = config_state.clone();
    std::thread::spawn(move || {
        rt.block_on(async {
            if let Err(e) = watch_configmap(&namespace, &cm_name, config_state_clone).await {
                error!(error = %e, "ConfigMap watcher failed");
            }
        });
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

/// Load configuration from a Kubernetes ConfigMap via the API
async fn load_config_from_k8s(namespace: &str, configmap_name: &str) -> Result<GatewayConfig> {
    let client = Client::try_default()
        .await
        .context("Failed to create Kubernetes client")?;

    let configmaps: Api<ConfigMap> = Api::namespaced(client, namespace);

    let cm = configmaps
        .get(configmap_name)
        .await
        .with_context(|| format!("Failed to get ConfigMap {}/{}", namespace, configmap_name))?;

    parse_config_from_configmap(&cm)
}

/// Parse GatewayConfig from a ConfigMap
fn parse_config_from_configmap(cm: &ConfigMap) -> Result<GatewayConfig> {
    let data = cm.data.as_ref().context("ConfigMap has no data")?;

    let content = data
        .get(CONFIG_KEY)
        .with_context(|| format!("ConfigMap missing '{}' key", CONFIG_KEY))?;

    let config: GatewayConfig =
        serde_json::from_str(content).context("Failed to parse config JSON from ConfigMap")?;

    Ok(config)
}

/// Watch a ConfigMap via the Kubernetes API and update config state on changes
async fn watch_configmap(
    namespace: &str,
    configmap_name: &str,
    state: Arc<ArcSwap<GatewayConfig>>,
) -> Result<()> {
    let client = Client::try_default()
        .await
        .context("Failed to create Kubernetes client for watcher")?;

    let configmaps: Api<ConfigMap> = Api::namespaced(client, namespace);

    // Use field selector to watch only our specific ConfigMap
    let watcher_config =
        watcher::Config::default().fields(&format!("metadata.name={}", configmap_name));

    info!(
        namespace = %namespace,
        configmap = %configmap_name,
        "Watching ConfigMap for changes via Kubernetes API"
    );

    let mut stream = watcher(configmaps, watcher_config).boxed();

    while let Some(event) = stream.next().await {
        match event {
            Ok(watcher::Event::Apply(cm)) | Ok(watcher::Event::InitApply(cm)) => {
                debug!(
                    configmap = %configmap_name,
                    "ConfigMap changed, reloading configuration"
                );

                match parse_config_from_configmap(&cm) {
                    Ok(config) => {
                        let routes = config.routes.len();
                        let listeners = config.listeners.len();
                        state.store(Arc::new(config));
                        info!(
                            listeners = listeners,
                            routes = routes,
                            "Configuration reloaded from ConfigMap"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to parse ConfigMap data");
                    }
                }
            }
            Ok(watcher::Event::Delete(_)) => {
                warn!(
                    configmap = %configmap_name,
                    "ConfigMap was deleted"
                );
            }
            Ok(watcher::Event::Init) => {
                debug!("ConfigMap watcher initialized");
            }
            Ok(watcher::Event::InitDone) => {
                debug!("ConfigMap watcher initial list complete");
            }
            Err(e) => {
                warn!(error = %e, "ConfigMap watcher error");
            }
        }
    }

    warn!("ConfigMap watcher stream ended unexpectedly");
    Ok(())
}
