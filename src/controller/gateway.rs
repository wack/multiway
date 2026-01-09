//! Gateway reconciler
//!
//! This module implements the reconciliation logic for Gateway resources.
//! According to the Gateway API specification:
//!
//! - Gateway is a namespaced resource that provisions infrastructure
//! - Each Gateway triggers the creation of data plane resources
//! - Gateway status reflects the state of the infrastructure
//!
//! For this implementation:
//! - We create a Deployment running the Pingora-based data plane
//! - We create a Service to expose the data plane
//! - We create a ConfigMap with the gateway configuration
//! - We update the Gateway status with addresses and listener status

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use gateway_crds::{
    Gateway, GatewayListeners, GatewayListenersAllowedRoutesNamespacesFrom, GatewayStatus,
    GatewayStatusAddresses, GatewayStatusListeners, GatewayStatusListenersSupportedKinds,
};
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, ContainerPort, EnvVar, PodSpec, PodTemplateSpec,
    ResourceRequirements, Service, ServicePort, ServiceSpec, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{
    Condition, LabelSelector, OwnerReference, Time,
};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use k8s_openapi::chrono::Utc;
use kube::api::{Patch, PatchParams, PostParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Api, Client, Resource, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, error, info, instrument, warn};

use super::config::{
    CONFIG_KEY, DataPlaneNames, GATEWAY_NAME_LABEL,
    GatewayConfig, ListenerConfig, MANAGED_BY_LABEL, MANAGED_BY_VALUE, Protocol,
};
use super::context::ControllerContext;
use super::error::{ControllerError, Result};
use super::gateway_class::get_accepted_gateway_class;

/// Run the Gateway controller
pub async fn run_gateway_controller(ctx: Arc<ControllerContext>) {
    info!("Starting Gateway controller");

    let client = ctx.client.clone();
    let gateways: Api<Gateway> = match &ctx.config.watch_namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };

    // Watch for related resources to trigger reconciliation
    let deployments: Api<Deployment> = Api::all(client.clone());
    let services: Api<Service> = Api::all(client.clone());
    let configmaps: Api<ConfigMap> = Api::all(client);

    let controller = Controller::new(gateways.clone(), WatcherConfig::default())
        .shutdown_on_signal()
        // Watch deployments managed by us
        .owns(
            deployments,
            WatcherConfig::default().labels(&format!("{}={}", MANAGED_BY_LABEL, MANAGED_BY_VALUE)),
        )
        // Watch services managed by us
        .owns(
            services,
            WatcherConfig::default().labels(&format!("{}={}", MANAGED_BY_LABEL, MANAGED_BY_VALUE)),
        )
        // Watch configmaps managed by us
        .owns(
            configmaps,
            WatcherConfig::default().labels(&format!("{}={}", MANAGED_BY_LABEL, MANAGED_BY_VALUE)),
        )
        .run(
            |obj, ctx| async move { reconcile_gateway(obj, ctx).await },
            |obj, error, ctx| error_policy(obj, error, ctx),
            ctx,
        )
        .for_each(|result| async move {
            match result {
                Ok((obj, action)) => {
                    debug!(
                        namespace = %obj.namespace.as_deref().unwrap_or("unknown"),
                        name = %obj.name,
                        ?action,
                        "Gateway reconciled successfully"
                    );
                }
                Err(error) => {
                    error!(%error, "Gateway reconciliation error");
                }
            }
        });

    controller.await;
}

/// Reconcile a single Gateway resource
#[instrument(skip_all, fields(
    namespace = %gateway.namespace().unwrap_or_default(),
    name = %gateway.name_any()
))]
async fn reconcile_gateway(gateway: Arc<Gateway>, ctx: Arc<ControllerContext>) -> Result<Action> {
    let name = gateway.name_any();
    let namespace = gateway.namespace().unwrap_or_default();
    info!("Reconciling Gateway");

    // Check if the GatewayClass is accepted
    let gateway_class =
        match get_accepted_gateway_class(&ctx.client, &gateway.spec.gateway_class_name).await? {
            Some(gc) => gc,
            None => {
                warn!(
                    gateway_class = %gateway.spec.gateway_class_name,
                    "GatewayClass not found or not accepted"
                );
                // Update status to indicate the class is not accepted
                update_gateway_status(
                    &ctx.client,
                    &namespace,
                    &name,
                    gateway.metadata.generation,
                    &gateway.spec.listeners,
                    false,
                    "Invalid",
                    "GatewayClass not found or not accepted by this controller",
                    None,
                )
                .await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        };

    debug!(gateway_class = %gateway_class.name_any(), "Found accepted GatewayClass");

    // Validate listeners
    let (listeners_valid, _listener_statuses) = validate_listeners(&gateway.spec.listeners);

    if !listeners_valid {
        warn!("Gateway has invalid listeners");
        update_gateway_status(
            &ctx.client,
            &namespace,
            &name,
            gateway.metadata.generation,
            &gateway.spec.listeners,
            false,
            "ListenersNotValid",
            "One or more listeners have invalid configuration",
            None,
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(30)));
    }

    // Create or update data plane resources
    let names = DataPlaneNames::new(&namespace, &name);

    // Create ConfigMap with gateway configuration
    let config = build_gateway_config(&gateway, &namespace);
    ensure_configmap(&ctx.client, &gateway, &names, &config).await?;

    // Create Service for the data plane
    let service_ip = ensure_service(&ctx.client, &gateway, &names).await?;

    // Create Deployment for the data plane
    ensure_deployment(&ctx, &gateway, &names).await?;

    // Update Gateway status
    let addresses = service_ip.map(|ip| {
        vec![GatewayStatusAddresses {
            r#type: Some("IPAddress".to_string()),
            value: ip,
        }]
    });

    update_gateway_status(
        &ctx.client,
        &namespace,
        &name,
        gateway.metadata.generation,
        &gateway.spec.listeners,
        true,
        "Accepted",
        "Gateway is accepted and data plane is provisioned",
        addresses,
    )
    .await?;

    info!("Gateway reconciled successfully");
    Ok(Action::requeue(Duration::from_secs(
        ctx.config.requeue_after_secs,
    )))
}

/// Validate Gateway listeners
fn validate_listeners(listeners: &[GatewayListeners]) -> (bool, Vec<ListenerValidation>) {
    let mut all_valid = true;
    let mut statuses = Vec::new();

    for listener in listeners {
        let mut valid = true;
        let mut reason = "Accepted";
        let mut message = String::new();

        // Validate protocol
        match listener.protocol.to_uppercase().as_str() {
            "HTTP" => {
                // HTTP is valid without TLS
                if listener.tls.is_some() {
                    valid = false;
                    reason = "InvalidTLS";
                    message = "HTTP listeners should not have TLS configuration".to_string();
                }
            }
            "HTTPS" => {
                // HTTPS requires TLS configuration
                if listener.tls.is_none() {
                    valid = false;
                    reason = "InvalidTLS";
                    message = "HTTPS listeners require TLS configuration".to_string();
                }
            }
            _ => {
                valid = false;
                reason = "UnsupportedProtocol";
                message = format!("Protocol '{}' is not supported", listener.protocol);
            }
        }

        // Validate port range
        if listener.port <= 0 || listener.port > 65535 {
            valid = false;
            reason = "InvalidPort";
            message = format!("Port {} is out of valid range (1-65535)", listener.port);
        }

        if !valid {
            all_valid = false;
        }

        statuses.push(ListenerValidation {
            name: listener.name.clone(),
            valid,
            reason: reason.to_string(),
            message,
        });
    }

    (all_valid, statuses)
}

#[allow(dead_code)]
struct ListenerValidation {
    name: String,
    valid: bool,
    reason: String,
    message: String,
}

/// Build the GatewayConfig for the data plane
fn build_gateway_config(gateway: &Gateway, namespace: &str) -> GatewayConfig {
    let mut config = GatewayConfig::new(namespace, gateway.name_any());

    for listener in &gateway.spec.listeners {
        let protocol = Protocol::from_str(&listener.protocol).unwrap_or(Protocol::Http);
        let listener_config = ListenerConfig {
            name: listener.name.clone(),
            port: listener.port as u16,
            protocol,
            hostname: listener.hostname.clone(),
            tls: None, // TODO: Handle TLS configuration
        };
        config.add_listener(listener_config);
    }

    config
}

/// Ensure the ConfigMap exists with the current configuration
async fn ensure_configmap(
    client: &Client,
    gateway: &Gateway,
    names: &DataPlaneNames,
    config: &GatewayConfig,
) -> Result<()> {
    let namespace = names.namespace();
    let configmap_name = names.configmap_name();
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);

    let config_json = config.to_json()?;

    let mut data = BTreeMap::new();
    data.insert(CONFIG_KEY.to_string(), config_json);

    let configmap = ConfigMap {
        metadata: kube::core::ObjectMeta {
            name: Some(configmap_name.clone()),
            namespace: Some(namespace.to_string()),
            labels: Some(names.labels()),
            owner_references: Some(vec![owner_reference(gateway)]),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };

    match api.get(&configmap_name).await {
        Ok(_existing) => {
            debug!("Updating ConfigMap");
            api.patch(
                &configmap_name,
                &PatchParams::apply("multiway-controller"),
                &Patch::Apply(&configmap),
            )
            .await?;
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            info!("Creating ConfigMap");
            api.create(&PostParams::default(), &configmap).await?;
        }
        Err(e) => return Err(e.into()),
    }

    Ok(())
}

/// Ensure the Service exists for the data plane
async fn ensure_service(
    client: &Client,
    gateway: &Gateway,
    names: &DataPlaneNames,
) -> Result<Option<String>> {
    let namespace = names.namespace();
    let service_name = names.service_name();
    let api: Api<Service> = Api::namespaced(client.clone(), namespace);

    // Build ports from listeners
    let ports: Vec<ServicePort> = gateway
        .spec
        .listeners
        .iter()
        .map(|l| ServicePort {
            name: Some(l.name.clone()),
            port: l.port,
            target_port: Some(IntOrString::Int(l.port)),
            protocol: Some("TCP".to_string()),
            ..Default::default()
        })
        .collect();

    let service = Service {
        metadata: kube::core::ObjectMeta {
            name: Some(service_name.clone()),
            namespace: Some(namespace.to_string()),
            labels: Some(names.labels()),
            owner_references: Some(vec![owner_reference(gateway)]),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            selector: Some(names.selector_labels()),
            ports: Some(ports),
            type_: Some("ClusterIP".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let cluster_ip = match api.get(&service_name).await {
        Ok(_existing) => {
            debug!("Updating Service");
            let result = api
                .patch(
                    &service_name,
                    &PatchParams::apply("multiway-controller"),
                    &Patch::Apply(&service),
                )
                .await?;
            result.spec.and_then(|s| s.cluster_ip)
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            info!("Creating Service");
            let result = api.create(&PostParams::default(), &service).await?;
            result.spec.and_then(|s| s.cluster_ip)
        }
        Err(e) => return Err(e.into()),
    };

    Ok(cluster_ip)
}

/// Ensure the Deployment exists for the data plane
async fn ensure_deployment(
    ctx: &ControllerContext,
    gateway: &Gateway,
    names: &DataPlaneNames,
) -> Result<()> {
    let namespace = names.namespace();
    let deployment_name = names.deployment_name();
    let api: Api<Deployment> = Api::namespaced(ctx.client.clone(), namespace);

    // Build container ports from listeners
    let ports: Vec<ContainerPort> = gateway
        .spec
        .listeners
        .iter()
        .map(|l| ContainerPort {
            name: Some(l.name.clone()),
            container_port: l.port,
            protocol: Some("TCP".to_string()),
            ..Default::default()
        })
        .collect();

    // Build resource requirements
    let mut requests = BTreeMap::new();
    requests.insert(
        "cpu".to_string(),
        Quantity(ctx.config.resource_requests.cpu.clone()),
    );
    requests.insert(
        "memory".to_string(),
        Quantity(ctx.config.resource_requests.memory.clone()),
    );

    let mut limits = BTreeMap::new();
    limits.insert(
        "cpu".to_string(),
        Quantity(ctx.config.resource_limits.cpu.clone()),
    );
    limits.insert(
        "memory".to_string(),
        Quantity(ctx.config.resource_limits.memory.clone()),
    );

    let container = Container {
        name: "dataplane".to_string(),
        image: Some(ctx.config.dataplane_image.clone()),
        image_pull_policy: Some(ctx.config.image_pull_policy.clone()),
        ports: Some(ports),
        env: Some(vec![
            EnvVar {
                name: "GATEWAY_NAME".to_string(),
                value: Some(gateway.name_any()),
                ..Default::default()
            },
            EnvVar {
                name: "GATEWAY_NAMESPACE".to_string(),
                value: Some(namespace.to_string()),
                ..Default::default()
            },
            EnvVar {
                name: "CONFIG_PATH".to_string(),
                value: Some(format!("/config/{}", CONFIG_KEY)),
                ..Default::default()
            },
        ]),
        volume_mounts: Some(vec![VolumeMount {
            name: "config".to_string(),
            mount_path: "/config".to_string(),
            read_only: Some(true),
            ..Default::default()
        }]),
        resources: Some(ResourceRequirements {
            requests: Some(requests),
            limits: Some(limits),
            ..Default::default()
        }),
        ..Default::default()
    };

    let deployment = Deployment {
        metadata: kube::core::ObjectMeta {
            name: Some(deployment_name.clone()),
            namespace: Some(namespace.to_string()),
            labels: Some(names.labels()),
            owner_references: Some(vec![owner_reference(gateway)]),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(ctx.config.default_replicas),
            selector: LabelSelector {
                match_labels: Some(names.selector_labels()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(kube::core::ObjectMeta {
                    labels: Some(names.labels()),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![container],
                    volumes: Some(vec![Volume {
                        name: "config".to_string(),
                        config_map: Some(ConfigMapVolumeSource {
                            name: names.configmap_name(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    };

    match api.get(&deployment_name).await {
        Ok(_existing) => {
            debug!("Updating Deployment");
            api.patch(
                &deployment_name,
                &PatchParams::apply("multiway-controller"),
                &Patch::Apply(&deployment),
            )
            .await?;
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            info!("Creating Deployment");
            api.create(&PostParams::default(), &deployment).await?;
        }
        Err(e) => return Err(e.into()),
    }

    Ok(())
}

/// Create an owner reference for a Gateway
fn owner_reference(gateway: &Gateway) -> OwnerReference {
    OwnerReference {
        api_version: Gateway::api_version(&()).to_string(),
        kind: Gateway::kind(&()).to_string(),
        name: gateway.name_any(),
        uid: gateway.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

/// Update the status of a Gateway
async fn update_gateway_status(
    client: &Client,
    namespace: &str,
    name: &str,
    generation: Option<i64>,
    listeners: &[GatewayListeners],
    accepted: bool,
    reason: &str,
    message: &str,
    addresses: Option<Vec<GatewayStatusAddresses>>,
) -> Result<()> {
    let api: Api<Gateway> = Api::namespaced(client.clone(), namespace);

    let now = Time(Utc::now());
    let status_value = if accepted { "True" } else { "False" };

    // Build conditions
    let conditions = vec![
        Condition {
            type_: "Accepted".to_string(),
            status: status_value.to_string(),
            observed_generation: generation,
            last_transition_time: now.clone(),
            reason: reason.to_string(),
            message: message.to_string(),
        },
        Condition {
            type_: "Programmed".to_string(),
            status: status_value.to_string(),
            observed_generation: generation,
            last_transition_time: now.clone(),
            reason: if accepted { "Programmed" } else { "Invalid" }.to_string(),
            message: if accepted {
                "Data plane is programmed".to_string()
            } else {
                message.to_string()
            },
        },
    ];

    // Build listener statuses
    let listener_statuses: Vec<GatewayStatusListeners> = listeners
        .iter()
        .map(|l| {
            let supported_kinds = match l.protocol.to_uppercase().as_str() {
                "HTTP" | "HTTPS" => vec![GatewayStatusListenersSupportedKinds {
                    group: Some("gateway.networking.k8s.io".to_string()),
                    kind: "HTTPRoute".to_string(),
                }],
                _ => vec![],
            };

            GatewayStatusListeners {
                name: l.name.clone(),
                attached_routes: 0, // Will be updated by HTTPRoute reconciler
                supported_kinds,
                conditions: vec![Condition {
                    type_: "Accepted".to_string(),
                    status: status_value.to_string(),
                    observed_generation: generation,
                    last_transition_time: now.clone(),
                    reason: reason.to_string(),
                    message: message.to_string(),
                }],
            }
        })
        .collect();

    let status = GatewayStatus {
        addresses,
        conditions: Some(conditions),
        listeners: Some(listener_statuses),
    };

    let patch = serde_json::json!({
        "status": status
    });

    api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;

    Ok(())
}

/// Error policy for Gateway reconciliation
fn error_policy(
    _obj: Arc<Gateway>,
    error: &ControllerError,
    ctx: Arc<ControllerContext>,
) -> Action {
    error!(%error, "Gateway reconciliation failed");
    Action::requeue(Duration::from_secs(ctx.config.error_requeue_secs))
}

/// Get allowed namespaces for routes on a listener
pub fn get_allowed_route_namespaces(
    listener: &GatewayListeners,
    gateway_namespace: &str,
) -> AllowedNamespaces {
    match &listener.allowed_routes {
        None => AllowedNamespaces::Same(gateway_namespace.to_string()),
        Some(allowed) => match &allowed.namespaces {
            None => AllowedNamespaces::Same(gateway_namespace.to_string()),
            Some(ns_config) => match &ns_config.from {
                None => AllowedNamespaces::Same(gateway_namespace.to_string()),
                Some(GatewayListenersAllowedRoutesNamespacesFrom::All) => AllowedNamespaces::All,
                Some(GatewayListenersAllowedRoutesNamespacesFrom::Same) => {
                    AllowedNamespaces::Same(gateway_namespace.to_string())
                }
                Some(GatewayListenersAllowedRoutesNamespacesFrom::Selector) => {
                    match &ns_config.selector {
                        Some(selector) => AllowedNamespaces::Selector {
                            match_labels: selector.match_labels.clone().unwrap_or_default(),
                        },
                        None => AllowedNamespaces::Same(gateway_namespace.to_string()),
                    }
                }
            },
        },
    }
}

/// Represents which namespaces routes can come from
pub enum AllowedNamespaces {
    All,
    Same(String),
    Selector {
        match_labels: BTreeMap<String, String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_listeners_http() {
        let listeners = vec![GatewayListeners {
            name: "http".to_string(),
            port: 80,
            protocol: "HTTP".to_string(),
            hostname: None,
            allowed_routes: None,
            tls: None,
        }];

        let (valid, statuses) = validate_listeners(&listeners);
        assert!(valid);
        assert_eq!(statuses.len(), 1);
        assert!(statuses[0].valid);
    }

    #[test]
    fn test_validate_listeners_invalid_protocol() {
        let listeners = vec![GatewayListeners {
            name: "invalid".to_string(),
            port: 80,
            protocol: "INVALID".to_string(),
            hostname: None,
            allowed_routes: None,
            tls: None,
        }];

        let (valid, statuses) = validate_listeners(&listeners);
        assert!(!valid);
        assert!(!statuses[0].valid);
        assert_eq!(statuses[0].reason, "UnsupportedProtocol");
    }

    #[test]
    fn test_data_plane_names() {
        let names = DataPlaneNames::new("default", "my-gateway");
        assert_eq!(names.deployment_name(), "multiway-dp-my-gateway");
        assert_eq!(names.configmap_name(), "multiway-config-my-gateway");

        let labels = names.labels();
        assert_eq!(
            labels.get(MANAGED_BY_LABEL),
            Some(&MANAGED_BY_VALUE.to_string())
        );
        assert_eq!(
            labels.get(GATEWAY_NAME_LABEL),
            Some(&"my-gateway".to_string())
        );
    }
}
