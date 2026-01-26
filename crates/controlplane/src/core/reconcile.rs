//! Pure reconciliation functions.
//!
//! These functions take a [`WorldSnapshot`] and return a [`ReconcileResult`]
//! describing the effects to perform. No I/O is performed - all effects
//! are returned as data to be executed by the imperative shell.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::DateTime;
use gateway_crds::{
    Gateway, GatewayClassStatus, GatewayStatus, GatewayStatusAddresses, GatewayStatusListeners,
    GatewayStatusListenersSupportedKinds, HttpRouteStatus, HttpRouteStatusParents,
    HttpRouteStatusParentsParentRef,
};
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, ContainerPort, EnvVar, PodSpec, PodTemplateSpec, ResourceRequirements,
    Service, ServiceAccount, ServicePort, ServiceSpec,
};
use k8s_openapi::api::rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{
    Condition, LabelSelector, OwnerReference, Time,
};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use k8s_openapi::chrono::Utc;
use kube::{Resource, ResourceExt};

use super::result::ReconcileResult;
use super::snapshot::WorldSnapshot;
use super::validate::{
    get_accepted_gateway_class, is_our_gateway_class, validate_gateway_class, validate_listeners,
    validate_parent_ref,
};
use crate::controller::config::{
    CONFIG_KEY, DataPlaneNames, GatewayConfig, ListenerConfig, Protocol,
};
use crate::controller::context::ControllerConfig;

// ============================================================================
// GatewayClass Reconciliation
// ============================================================================

/// Reconcile a GatewayClass resource.
///
/// This function is pure - it computes what changes need to be made based
/// on the current snapshot and returns them as a [`ReconcileResult`].
pub fn reconcile_gateway_class(
    snapshot: &WorldSnapshot,
    config: &ControllerConfig,
    gateway_class_name: &str,
) -> ReconcileResult {
    let mut result = ReconcileResult::new();

    let gateway_class = match snapshot.get_gateway_class(gateway_class_name) {
        Some(gc) => gc,
        None => {
            // GatewayClass was deleted, nothing to do
            return result;
        }
    };

    // Check if this GatewayClass is for our controller
    if !is_our_gateway_class(gateway_class) {
        // Not our GatewayClass, requeue after a long interval
        return result.with_requeue(Duration::from_secs(3600));
    }

    // Validate the GatewayClass
    let validation = validate_gateway_class(gateway_class);

    // Build the status
    let status = build_gateway_class_status(
        snapshot.now,
        gateway_class.metadata.generation,
        validation.accepted,
        validation.reason,
        &validation.message,
    );

    result = result.update_gateway_class_status(gateway_class_name.to_string(), status);

    if validation.accepted {
        result = result.emit_normal("Accepted", "GatewayClass accepted");
    } else {
        result = result.emit_warning("NotAccepted", &validation.message);
    }

    result.with_requeue(Duration::from_secs(config.requeue_after_secs))
}

/// Build a GatewayClassStatus
fn build_gateway_class_status(
    now: DateTime<Utc>,
    generation: Option<i64>,
    accepted: bool,
    reason: &str,
    message: &str,
) -> GatewayClassStatus {
    let status_value = if accepted { "True" } else { "False" };

    let condition = Condition {
        type_: "Accepted".to_string(),
        status: status_value.to_string(),
        observed_generation: generation,
        last_transition_time: Time(now),
        reason: reason.to_string(),
        message: message.to_string(),
    };

    // Note: supported_features field not available in Gateway API v1.2.1
    GatewayClassStatus {
        conditions: Some(vec![condition]),
    }
}

// ============================================================================
// Gateway Reconciliation
// ============================================================================

/// Reconcile a Gateway resource.
///
/// This function is pure - it computes what resources need to be created/updated
/// and what status changes need to be made.
pub fn reconcile_gateway(
    snapshot: &WorldSnapshot,
    config: &ControllerConfig,
    gateway_ns: &str,
    gateway_name: &str,
) -> ReconcileResult {
    let mut result = ReconcileResult::new();

    let gateway = match snapshot.get_gateway(gateway_ns, gateway_name) {
        Some(gw) => gw,
        None => return result, // Gateway was deleted
    };

    // Step 1: Check if the GatewayClass is accepted
    if get_accepted_gateway_class(snapshot, &gateway.spec.gateway_class_name).is_none() {
        let status = build_gateway_status(
            snapshot.now,
            gateway,
            false,
            "Invalid",
            "GatewayClass not found or not accepted by this controller",
            None,
        );
        return result
            .update_gateway_status(gateway_ns.to_string(), gateway_name.to_string(), status)
            .emit_warning("InvalidGatewayClass", "GatewayClass not accepted")
            .with_requeue(Duration::from_secs(30));
    }

    // Step 2: Validate listeners
    let listener_validation = validate_listeners(&gateway.spec.listeners);
    if !listener_validation.all_valid {
        let status = build_gateway_status(
            snapshot.now,
            gateway,
            false,
            "ListenersNotValid",
            "One or more listeners have invalid configuration",
            None,
        );
        return result
            .update_gateway_status(gateway_ns.to_string(), gateway_name.to_string(), status)
            .emit_warning("InvalidListeners", "Listener validation failed")
            .with_requeue(Duration::from_secs(30));
    }

    // Step 3: Build data plane resources
    let names = DataPlaneNames::new(gateway_ns, gateway_name);

    // Build ConfigMap
    let gateway_config = build_gateway_config(gateway, gateway_ns, snapshot);
    let configmap = build_configmap(&names, &gateway_config, gateway);
    result = result.upsert_configmap(configmap);

    // Build RBAC resources for data plane to read ConfigMap via API
    let serviceaccount = build_serviceaccount(&names, gateway);
    let role = build_role(&names, gateway);
    let rolebinding = build_rolebinding(&names, gateway);
    result = result.upsert_serviceaccount(serviceaccount);
    result = result.upsert_role(role);
    result = result.upsert_rolebinding(rolebinding);

    // Build Service
    let service = build_service(&names, gateway);
    result = result.upsert_service(service);

    // Build Deployment
    let deployment = build_deployment(&names, gateway, config);
    result = result.upsert_deployment(deployment);

    // Step 4: Get addresses for the Gateway status
    //
    // For local development and conformance testing, we use 127.0.0.1 (localhost)
    // as the Gateway address. This allows external clients to reach the Gateway
    // via kubectl port-forward.
    //
    // For production environments with LoadBalancer support (e.g., MetalLB),
    // the address should come from the Service's external IP. This is a future
    // enhancement - for now, we optimize for local development.
    let addresses = Some(vec![GatewayStatusAddresses {
        r#type: Some("IPAddress".to_string()),
        value: "127.0.0.1".to_string(),
    }]);

    // Step 5: Count attached routes
    let attached_routes = snapshot.routes_for_gateway(gateway_ns, gateway_name).len() as i32;

    // Step 6: Build success status
    let status = build_gateway_status_with_routes(
        snapshot.now,
        gateway,
        true,
        "Accepted",
        "Gateway is accepted and data plane is provisioned",
        addresses,
        attached_routes,
    );

    result
        .update_gateway_status(gateway_ns.to_string(), gateway_name.to_string(), status)
        .emit_normal("Reconciled", "Gateway reconciled successfully")
        .with_requeue(Duration::from_secs(config.requeue_after_secs))
}

/// Build a GatewayStatus
fn build_gateway_status(
    now: DateTime<Utc>,
    gateway: &Gateway,
    accepted: bool,
    reason: &str,
    message: &str,
    addresses: Option<Vec<GatewayStatusAddresses>>,
) -> GatewayStatus {
    build_gateway_status_with_routes(now, gateway, accepted, reason, message, addresses, 0)
}

/// Build a GatewayStatus with route count
fn build_gateway_status_with_routes(
    now: DateTime<Utc>,
    gateway: &Gateway,
    accepted: bool,
    reason: &str,
    message: &str,
    addresses: Option<Vec<GatewayStatusAddresses>>,
    attached_routes: i32,
) -> GatewayStatus {
    let status_value = if accepted { "True" } else { "False" };
    let time = Time(now);

    let conditions = vec![
        Condition {
            type_: "Accepted".to_string(),
            status: status_value.to_string(),
            observed_generation: gateway.metadata.generation,
            last_transition_time: time.clone(),
            reason: reason.to_string(),
            message: message.to_string(),
        },
        Condition {
            type_: "Programmed".to_string(),
            status: status_value.to_string(),
            observed_generation: gateway.metadata.generation,
            last_transition_time: time.clone(),
            reason: if accepted { "Programmed" } else { "Invalid" }.to_string(),
            message: if accepted {
                "Data plane is programmed".to_string()
            } else {
                message.to_string()
            },
        },
    ];

    let listener_statuses: Vec<GatewayStatusListeners> = gateway
        .spec
        .listeners
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
                attached_routes,
                supported_kinds,
                conditions: vec![
                    Condition {
                        type_: "Accepted".to_string(),
                        status: status_value.to_string(),
                        observed_generation: gateway.metadata.generation,
                        last_transition_time: time.clone(),
                        reason: reason.to_string(),
                        message: message.to_string(),
                    },
                    Condition {
                        type_: "ResolvedRefs".to_string(),
                        status: status_value.to_string(),
                        observed_generation: gateway.metadata.generation,
                        last_transition_time: time.clone(),
                        reason: if accepted {
                            "ResolvedRefs".to_string()
                        } else {
                            reason.to_string()
                        },
                        message: if accepted {
                            "All references resolved".to_string()
                        } else {
                            message.to_string()
                        },
                    },
                    Condition {
                        type_: "Programmed".to_string(),
                        status: status_value.to_string(),
                        observed_generation: gateway.metadata.generation,
                        last_transition_time: time.clone(),
                        reason: if accepted {
                            "Programmed".to_string()
                        } else {
                            "Invalid".to_string()
                        },
                        message: if accepted {
                            "Listener is programmed".to_string()
                        } else {
                            message.to_string()
                        },
                    },
                ],
            }
        })
        .collect();

    GatewayStatus {
        addresses,
        conditions: Some(conditions),
        listeners: Some(listener_statuses),
    }
}

/// Build GatewayConfig for the data plane
fn build_gateway_config(
    gateway: &Gateway,
    namespace: &str,
    snapshot: &WorldSnapshot,
) -> GatewayConfig {
    let mut config = GatewayConfig::new(namespace, gateway.name_any());

    // Add listeners
    for listener in &gateway.spec.listeners {
        let protocol = listener.protocol.parse().unwrap_or(Protocol::Http);
        let port = listener.port as u16;
        let listener_config = ListenerConfig {
            name: listener.name.clone(),
            port,
            container_port: crate::controller::config::compute_container_port(port),
            protocol,
            hostname: listener.hostname.clone(),
            tls: None, // TODO: Handle TLS configuration
        };
        config.add_listener(listener_config);
    }

    // Add routes
    let routes = snapshot.routes_for_gateway(namespace, &gateway.name_any());
    for route in routes {
        if let Ok(route_config) = crate::controller::httproute::convert_httproute_to_config(
            route,
            &gateway
                .spec
                .listeners
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>(),
        ) {
            config.add_route(route_config);
        }
    }

    config
}

/// Build a ConfigMap for the gateway configuration
fn build_configmap(names: &DataPlaneNames, config: &GatewayConfig, gateway: &Gateway) -> ConfigMap {
    let config_json = config.to_json().unwrap_or_default();

    let mut data = BTreeMap::new();
    data.insert(CONFIG_KEY.to_string(), config_json);

    ConfigMap {
        metadata: kube::core::ObjectMeta {
            name: Some(names.configmap_name()),
            namespace: Some(names.namespace().to_string()),
            labels: Some(names.labels()),
            owner_references: Some(vec![owner_reference(gateway)]),
            ..Default::default()
        },
        data: Some(data),
        // Explicitly set immutable to None to avoid SSA conflicts with unmanaged fields
        immutable: None,
        binary_data: None,
    }
}

/// Build a ServiceAccount for the data plane
fn build_serviceaccount(names: &DataPlaneNames, gateway: &Gateway) -> ServiceAccount {
    ServiceAccount {
        metadata: kube::core::ObjectMeta {
            name: Some(names.serviceaccount_name()),
            namespace: Some(names.namespace().to_string()),
            labels: Some(names.labels()),
            owner_references: Some(vec![owner_reference(gateway)]),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Build a Role for the data plane to read ConfigMaps
fn build_role(names: &DataPlaneNames, gateway: &Gateway) -> Role {
    Role {
        metadata: kube::core::ObjectMeta {
            name: Some(names.role_name()),
            namespace: Some(names.namespace().to_string()),
            labels: Some(names.labels()),
            owner_references: Some(vec![owner_reference(gateway)]),
            ..Default::default()
        },
        rules: Some(vec![PolicyRule {
            api_groups: Some(vec!["".to_string()]),
            resources: Some(vec!["configmaps".to_string()]),
            verbs: vec!["get".to_string(), "list".to_string(), "watch".to_string()],
            // Restrict to only the specific ConfigMap for this Gateway
            resource_names: Some(vec![names.configmap_name()]),
            ..Default::default()
        }]),
    }
}

/// Build a RoleBinding for the data plane
fn build_rolebinding(names: &DataPlaneNames, gateway: &Gateway) -> RoleBinding {
    RoleBinding {
        metadata: kube::core::ObjectMeta {
            name: Some(names.rolebinding_name()),
            namespace: Some(names.namespace().to_string()),
            labels: Some(names.labels()),
            owner_references: Some(vec![owner_reference(gateway)]),
            ..Default::default()
        },
        role_ref: RoleRef {
            api_group: "rbac.authorization.k8s.io".to_string(),
            kind: "Role".to_string(),
            name: names.role_name(),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".to_string(),
            name: names.serviceaccount_name(),
            namespace: Some(names.namespace().to_string()),
            ..Default::default()
        }]),
    }
}

/// Maximum length for Kubernetes port names (IANA service name format, RFC 6335).
const MAX_PORT_NAME_LENGTH: usize = 15;

/// Sanitize a listener name to be a valid Kubernetes port name.
///
/// Kubernetes port names must conform to IANA service name format (RFC 6335):
/// - Maximum 15 characters
/// - Alphanumeric with hyphens (no leading/trailing hyphens)
/// - Must contain at least one letter
///
/// Gateway API listener names can be up to 253 characters, so we must truncate
/// long names. To ensure uniqueness, we append a short hash when truncating.
fn sanitize_port_name(name: &str) -> String {
    if name.len() <= MAX_PORT_NAME_LENGTH {
        return name.to_string();
    }

    // Generate a simple hash from the full name for uniqueness
    let hash: u32 = name
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    let hash_suffix = format!("{:x}", hash & 0xFFFF); // 4 hex chars

    // Truncate name to leave room for hyphen and hash suffix
    // Format: <truncated>-<hash> where hash is 4 chars, so truncate to 10 chars
    let truncate_len = MAX_PORT_NAME_LENGTH - 1 - hash_suffix.len(); // 15 - 1 - 4 = 10
    let truncated: String = name.chars().take(truncate_len).collect();

    // Remove trailing hyphens from truncated part (invalid in port names)
    let truncated = truncated.trim_end_matches('-');

    format!("{}-{}", truncated, hash_suffix)
}

/// Build a Service for the data plane
fn build_service(names: &DataPlaneNames, gateway: &Gateway) -> Service {
    // Deduplicate ports: multiple Gateway listeners can share the same port
    // (e.g., for SNI-based routing), but Kubernetes Services require unique ports.
    //
    // The Service maps external listener ports to internal container ports.
    // For privileged ports (< 1024), the container binds to port + 8000 to avoid
    // requiring root privileges.
    let mut port_map: BTreeMap<i32, ServicePort> = BTreeMap::new();
    for listener in &gateway.spec.listeners {
        let container_port =
            crate::controller::config::compute_container_port(listener.port as u16) as i32;
        port_map
            .entry(listener.port)
            .or_insert_with(|| ServicePort {
                name: Some(sanitize_port_name(&listener.name)),
                port: listener.port,
                target_port: Some(IntOrString::Int(container_port)),
                protocol: Some("TCP".to_string()),
                ..Default::default()
            });
    }
    let ports: Vec<ServicePort> = port_map.into_values().collect();

    Service {
        metadata: kube::core::ObjectMeta {
            name: Some(names.service_name()),
            namespace: Some(names.namespace().to_string()),
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
    }
}

/// Build a Deployment for the data plane
fn build_deployment(
    names: &DataPlaneNames,
    gateway: &Gateway,
    config: &ControllerConfig,
) -> Deployment {
    // Deduplicate ports: multiple Gateway listeners can share the same port
    // (e.g., for SNI-based routing), but Kubernetes rejects duplicate container ports.
    //
    // The container binds to an internal port (container_port) which may differ from
    // the external listener port. For privileged ports (< 1024), we add an offset
    // to avoid requiring root privileges. The Service maps external port → container_port.
    let mut port_map: BTreeMap<i32, ContainerPort> = BTreeMap::new();
    for listener in &gateway.spec.listeners {
        let internal_port =
            crate::controller::config::compute_container_port(listener.port as u16) as i32;
        port_map
            .entry(internal_port)
            .or_insert_with(|| ContainerPort {
                name: Some(sanitize_port_name(&listener.name)),
                container_port: internal_port,
                protocol: Some("TCP".to_string()),
                ..Default::default()
            });
    }
    let ports: Vec<ContainerPort> = port_map.into_values().collect();

    let mut requests = BTreeMap::new();
    requests.insert(
        "cpu".to_string(),
        Quantity(config.resource_requests.cpu.clone()),
    );
    requests.insert(
        "memory".to_string(),
        Quantity(config.resource_requests.memory.clone()),
    );

    let mut limits = BTreeMap::new();
    limits.insert(
        "cpu".to_string(),
        Quantity(config.resource_limits.cpu.clone()),
    );
    limits.insert(
        "memory".to_string(),
        Quantity(config.resource_limits.memory.clone()),
    );

    // Data plane reads configuration from the ConfigMap via the Kubernetes API,
    // not from a mounted volume. This provides immediate notification of changes
    // (bypassing the ~60 second kubelet sync delay for mounted ConfigMaps).
    let container = Container {
        name: "dataplane".to_string(),
        image: Some(config.dataplane_image.clone()),
        image_pull_policy: Some(config.image_pull_policy.clone()),
        ports: Some(ports),
        env: Some(vec![
            EnvVar {
                name: "GATEWAY_NAME".to_string(),
                value: Some(gateway.name_any()),
                ..Default::default()
            },
            EnvVar {
                name: "GATEWAY_NAMESPACE".to_string(),
                value: Some(names.namespace().to_string()),
                ..Default::default()
            },
        ]),
        resources: Some(ResourceRequirements {
            requests: Some(requests),
            limits: Some(limits),
            ..Default::default()
        }),
        ..Default::default()
    };

    Deployment {
        metadata: kube::core::ObjectMeta {
            name: Some(names.deployment_name()),
            namespace: Some(names.namespace().to_string()),
            labels: Some(names.labels()),
            owner_references: Some(vec![owner_reference(gateway)]),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(config.default_replicas),
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
                    service_account_name: Some(names.serviceaccount_name()),
                    containers: vec![container],
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
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

// ============================================================================
// HTTPRoute Reconciliation
// ============================================================================

/// Reconcile an HTTPRoute resource.
///
/// This function is pure - it computes what status updates and ConfigMap
/// changes need to be made.
pub fn reconcile_httproute(
    snapshot: &WorldSnapshot,
    config: &ControllerConfig,
    route_ns: &str,
    route_name: &str,
) -> ReconcileResult {
    let mut result = ReconcileResult::new();

    let httproute = match snapshot.get_httproute(route_ns, route_name) {
        Some(r) => r,
        None => return result, // Route was deleted
    };

    let parent_refs = httproute
        .spec
        .parent_refs
        .as_ref()
        .cloned()
        .unwrap_or_default();

    if parent_refs.is_empty() {
        return result.with_requeue(Duration::from_secs(config.requeue_after_secs));
    }

    let mut parent_statuses = Vec::new();

    for parent_ref in &parent_refs {
        let validation = validate_parent_ref(snapshot, httproute, parent_ref, route_ns);

        let parent_status = build_parent_status(
            snapshot.now,
            &validation.parent_ref,
            validation.accepted,
            validation.reason,
            &validation.message,
            httproute.metadata.generation,
        );
        parent_statuses.push(parent_status);

        // If accepted, we need to update the parent Gateway's ConfigMap
        if validation.accepted {
            let parent_namespace = parent_ref.namespace.as_deref().unwrap_or(route_ns);
            let parent_name = &parent_ref.name;

            if let Some(gateway) = snapshot.get_gateway(parent_namespace, parent_name) {
                let names = DataPlaneNames::new(parent_namespace, parent_name);
                let gateway_config = build_gateway_config(gateway, parent_namespace, snapshot);
                let configmap = build_configmap(&names, &gateway_config, gateway);
                result = result.upsert_configmap(configmap);
            }
        }
    }

    let route_status = HttpRouteStatus {
        parents: parent_statuses,
    };

    result
        .update_httproute_status(route_ns.to_string(), route_name.to_string(), route_status)
        .with_requeue(Duration::from_secs(config.requeue_after_secs))
}

/// Build an HTTPRoute parent status
fn build_parent_status(
    now: DateTime<Utc>,
    parent_ref: &gateway_crds::HttpRouteParentRefs,
    accepted: bool,
    reason: &str,
    message: &str,
    generation: Option<i64>,
) -> HttpRouteStatusParents {
    let time = Time(now);
    let status_value = if accepted { "True" } else { "False" };

    HttpRouteStatusParents {
        controller_name: crate::controller::config::CONTROLLER_NAME.to_string(),
        parent_ref: HttpRouteStatusParentsParentRef {
            group: parent_ref.group.clone(),
            kind: parent_ref.kind.clone(),
            name: parent_ref.name.clone(),
            namespace: parent_ref.namespace.clone(),
            port: parent_ref.port,
            section_name: parent_ref.section_name.clone(),
        },
        conditions: Some(vec![
            Condition {
                type_: "Accepted".to_string(),
                status: status_value.to_string(),
                observed_generation: generation,
                last_transition_time: time.clone(),
                reason: reason.to_string(),
                message: message.to_string(),
            },
            Condition {
                type_: "ResolvedRefs".to_string(),
                status: status_value.to_string(),
                observed_generation: generation,
                last_transition_time: time,
                reason: if accepted { "ResolvedRefs" } else { reason }.to_string(),
                message: message.to_string(),
            },
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::snapshot::WorldSnapshotBuilder;
    use chrono::TimeZone;
    use gateway_crds::{GatewayClassSpec, GatewaySpec, HttpRouteParentRefs, HttpRouteSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    fn default_config() -> ControllerConfig {
        ControllerConfig::default()
    }

    fn create_gateway_class(name: &str, controller: &str) -> gateway_crds::GatewayClass {
        gateway_crds::GatewayClass {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: GatewayClassSpec {
                controller_name: controller.to_string(),
                description: None,
                parameters_ref: None,
            },
            status: None,
        }
    }

    fn create_accepted_gateway_class(name: &str) -> gateway_crds::GatewayClass {
        gateway_crds::GatewayClass {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: GatewayClassSpec {
                controller_name: crate::controller::config::CONTROLLER_NAME.to_string(),
                description: None,
                parameters_ref: None,
            },
            status: Some(GatewayClassStatus {
                conditions: Some(vec![Condition {
                    type_: "Accepted".to_string(),
                    status: "True".to_string(),
                    observed_generation: Some(1),
                    last_transition_time: Time(Utc::now()),
                    reason: "Accepted".to_string(),
                    message: "Accepted".to_string(),
                }]),
            }),
        }
    }

    fn create_gateway(ns: &str, name: &str, class: &str) -> Gateway {
        Gateway {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(ns.to_string()),
                uid: Some("test-uid".to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: class.to_string(),
                listeners: vec![gateway_crds::GatewayListeners {
                    name: "http".to_string(),
                    port: 80,
                    protocol: "HTTP".to_string(),
                    hostname: None,
                    allowed_routes: None,
                    tls: None,
                }],
                addresses: None,
                infrastructure: None,
            },
            status: None,
        }
    }

    fn create_httproute(
        ns: &str,
        name: &str,
        gw_ns: &str,
        gw_name: &str,
    ) -> gateway_crds::HTTPRoute {
        gateway_crds::HTTPRoute {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(ns.to_string()),
                ..Default::default()
            },
            spec: HttpRouteSpec {
                parent_refs: Some(vec![HttpRouteParentRefs {
                    group: Some("gateway.networking.k8s.io".to_string()),
                    kind: Some("Gateway".to_string()),
                    namespace: Some(gw_ns.to_string()),
                    name: gw_name.to_string(),
                    section_name: None,
                    port: None,
                }]),
                hostnames: None,
                rules: None,
            },
            status: None,
        }
    }

    // ========================================
    // GatewayClass Reconciliation Tests
    // ========================================

    #[test]
    fn test_reconcile_gateway_class_not_found() {
        let snapshot = WorldSnapshotBuilder::new().build();
        let config = default_config();

        let result = reconcile_gateway_class(&snapshot, &config, "nonexistent");

        assert!(!result.has_status_updates());
    }

    #[test]
    fn test_reconcile_gateway_class_not_ours() {
        let gc = create_gateway_class("other", "other.io/controller");
        let snapshot = WorldSnapshotBuilder::new().with_gateway_class(gc).build();
        let config = default_config();

        let result = reconcile_gateway_class(&snapshot, &config, "other");

        // Should not update status for other controllers
        assert!(!result.has_status_updates());
        // Should requeue after long interval
        assert!(matches!(
            result.requeue,
            Some(super::super::result::RequeueDecision::After(d)) if d.as_secs() == 3600
        ));
    }

    #[test]
    fn test_reconcile_gateway_class_accepted() {
        let fixed_time = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let gc = create_gateway_class("multiway", crate::controller::config::CONTROLLER_NAME);

        let snapshot = WorldSnapshotBuilder::new()
            .at_time(fixed_time)
            .with_gateway_class(gc)
            .build();
        let config = default_config();

        let result = reconcile_gateway_class(&snapshot, &config, "multiway");

        // Should update status
        assert!(result.has_status_updates());

        let status = result.gateway_class_status_update("multiway").unwrap();
        let conditions = status.conditions.as_ref().unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, "Accepted");
        assert_eq!(conditions[0].status, "True");

        // Note: supported_features field not available in Gateway API v1.2.1
    }

    // ========================================
    // Gateway Reconciliation Tests
    // ========================================

    #[test]
    fn test_reconcile_gateway_not_found() {
        let snapshot = WorldSnapshotBuilder::new().build();
        let config = default_config();

        let result = reconcile_gateway(&snapshot, &config, "default", "nonexistent");

        assert!(!result.has_upserts());
        assert!(!result.has_status_updates());
    }

    #[test]
    fn test_reconcile_gateway_class_not_accepted() {
        let gc = create_gateway_class("multiway", crate::controller::config::CONTROLLER_NAME);
        let gw = create_gateway("default", "my-gateway", "multiway");

        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(gc)
            .with_gateway(gw)
            .build();
        let config = default_config();

        let result = reconcile_gateway(&snapshot, &config, "default", "my-gateway");

        // Should update status with error
        let status = result
            .gateway_status_update("default", "my-gateway")
            .unwrap();
        let conditions = status.conditions.as_ref().unwrap();
        let accepted = conditions.iter().find(|c| c.type_ == "Accepted").unwrap();
        assert_eq!(accepted.status, "False");
        assert_eq!(accepted.reason, "Invalid");

        // Should not create resources
        assert!(!result.has_upserts());
    }

    #[test]
    fn test_reconcile_gateway_creates_resources() {
        let gc = create_accepted_gateway_class("multiway");
        let gw = create_gateway("default", "my-gateway", "multiway");

        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(gc)
            .with_gateway(gw)
            .build();
        let config = default_config();

        let result = reconcile_gateway(&snapshot, &config, "default", "my-gateway");

        // Should create Deployment, Service, ConfigMap
        assert_eq!(result.deployment_upserts().len(), 1);
        assert_eq!(result.service_upserts().len(), 1);
        assert_eq!(result.configmap_upserts().len(), 1);

        // Should update status with success
        let status = result
            .gateway_status_update("default", "my-gateway")
            .unwrap();
        let conditions = status.conditions.as_ref().unwrap();
        let accepted = conditions.iter().find(|c| c.type_ == "Accepted").unwrap();
        assert_eq!(accepted.status, "True");
    }

    #[test]
    fn test_reconcile_gateway_deployment_has_correct_config() {
        let gc = create_accepted_gateway_class("multiway");
        let gw = create_gateway("default", "my-gateway", "multiway");

        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(gc)
            .with_gateway(gw)
            .build();
        let config = default_config();

        let result = reconcile_gateway(&snapshot, &config, "default", "my-gateway");

        let deployment = &result.deployment_upserts()[0];
        assert_eq!(
            deployment.metadata.name,
            Some("multiway-dp-my-gateway".to_string())
        );
        assert_eq!(deployment.metadata.namespace, Some("default".to_string()));

        // Check owner reference
        let owner_refs = deployment.metadata.owner_references.as_ref().unwrap();
        assert_eq!(owner_refs.len(), 1);
        assert_eq!(owner_refs[0].name, "my-gateway");
        assert_eq!(owner_refs[0].kind, "Gateway");
    }

    // ========================================
    // HTTPRoute Reconciliation Tests
    // ========================================

    #[test]
    fn test_reconcile_httproute_not_found() {
        let snapshot = WorldSnapshotBuilder::new().build();
        let config = default_config();

        let result = reconcile_httproute(&snapshot, &config, "default", "nonexistent");

        assert!(!result.has_status_updates());
    }

    #[test]
    fn test_reconcile_httproute_no_parent_refs() {
        let route = gateway_crds::HTTPRoute {
            metadata: ObjectMeta {
                name: Some("my-route".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: HttpRouteSpec {
                parent_refs: None, // No parent refs
                hostnames: None,
                rules: None,
            },
            status: None,
        };

        let snapshot = WorldSnapshotBuilder::new().with_httproute(route).build();
        let config = default_config();

        let result = reconcile_httproute(&snapshot, &config, "default", "my-route");

        // Should just requeue, no status updates
        assert!(!result.has_status_updates());
    }

    #[test]
    fn test_reconcile_httproute_gateway_not_found() {
        let gc = create_accepted_gateway_class("multiway");
        let route = create_httproute("default", "my-route", "default", "nonexistent");

        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(gc)
            .with_httproute(route)
            .build();
        let config = default_config();

        let result = reconcile_httproute(&snapshot, &config, "default", "my-route");

        // Should update status with error
        let status = result
            .httproute_status_update("default", "my-route")
            .unwrap();
        assert_eq!(status.parents.len(), 1);

        let accepted = status.parents[0]
            .conditions
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.type_ == "Accepted")
            .unwrap();
        assert_eq!(accepted.status, "False");
        assert_eq!(accepted.reason, "NoMatchingParent");
    }

    #[test]
    fn test_reconcile_httproute_accepted() {
        let gc = create_accepted_gateway_class("multiway");
        let gw = create_gateway("default", "my-gateway", "multiway");
        let route = create_httproute("default", "my-route", "default", "my-gateway");

        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(gc)
            .with_gateway(gw)
            .with_httproute(route)
            .build();
        let config = default_config();

        let result = reconcile_httproute(&snapshot, &config, "default", "my-route");

        // Should update status with success
        let status = result
            .httproute_status_update("default", "my-route")
            .unwrap();
        assert_eq!(status.parents.len(), 1);

        let accepted = status.parents[0]
            .conditions
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.type_ == "Accepted")
            .unwrap();
        assert_eq!(accepted.status, "True");
        assert_eq!(accepted.reason, "Accepted");

        // Should update the Gateway's ConfigMap
        assert!(!result.configmap_upserts().is_empty());
    }

    // ========================================
    // Service Generation Tests
    // ========================================

    #[test]
    fn test_build_service_deduplicates_ports_for_multiple_listeners_on_same_port() {
        // Create a Gateway with two listeners on the same port (443)
        // This is a valid Gateway API configuration for SNI-based routing
        let gateway = Gateway {
            metadata: ObjectMeta {
                name: Some("multi-listener-gateway".to_string()),
                namespace: Some("default".to_string()),
                uid: Some("test-uid".to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: "multiway".to_string(),
                listeners: vec![
                    gateway_crds::GatewayListeners {
                        name: "https-wildcard".to_string(),
                        port: 443,
                        protocol: "HTTPS".to_string(),
                        hostname: Some("*.example.com".to_string()),
                        allowed_routes: None,
                        tls: None,
                    },
                    gateway_crds::GatewayListeners {
                        name: "https-specific".to_string(),
                        port: 443,
                        protocol: "HTTPS".to_string(),
                        hostname: Some("api.example.org".to_string()),
                        allowed_routes: None,
                        tls: None,
                    },
                ],
                addresses: None,
                infrastructure: None,
            },
            status: None,
        };

        let names = DataPlaneNames::new("default", "multi-listener-gateway");
        let service = build_service(&names, &gateway);

        // The Service should have only ONE port entry for port 443,
        // not two (which would be rejected by Kubernetes)
        let ports = service.spec.unwrap().ports.unwrap();

        // Count unique port numbers
        let unique_ports: std::collections::HashSet<i32> = ports.iter().map(|p| p.port).collect();
        let port_443_count = ports.iter().filter(|p| p.port == 443).count();

        assert_eq!(
            port_443_count, 1,
            "Expected exactly 1 ServicePort for port 443, but found {}. \
             Multiple Gateway listeners can share the same port, but the \
             generated Service must deduplicate ports.",
            port_443_count
        );

        assert_eq!(
            ports.len(),
            unique_ports.len(),
            "Service ports must be unique. Found {} ports but only {} unique port numbers.",
            ports.len(),
            unique_ports.len()
        );
    }

    #[test]
    fn test_build_deployment_deduplicates_ports_for_multiple_listeners_on_same_port() {
        // Create a Gateway with two listeners on the same port (443)
        // This is a valid Gateway API configuration for SNI-based routing.
        // Kubernetes rejects Deployments with duplicate container ports.
        let gateway = Gateway {
            metadata: ObjectMeta {
                name: Some("multi-listener-gateway".to_string()),
                namespace: Some("default".to_string()),
                uid: Some("test-uid".to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: "multiway".to_string(),
                listeners: vec![
                    gateway_crds::GatewayListeners {
                        name: "https-wildcard".to_string(),
                        port: 443,
                        protocol: "HTTPS".to_string(),
                        hostname: Some("*.example.com".to_string()),
                        allowed_routes: None,
                        tls: None,
                    },
                    gateway_crds::GatewayListeners {
                        name: "https-specific".to_string(),
                        port: 443,
                        protocol: "HTTPS".to_string(),
                        hostname: Some("api.example.org".to_string()),
                        allowed_routes: None,
                        tls: None,
                    },
                ],
                addresses: None,
                infrastructure: None,
            },
            status: None,
        };

        let names = DataPlaneNames::new("default", "multi-listener-gateway");
        let config = default_config();
        let deployment = build_deployment(&names, &gateway, &config);

        // The Deployment should have only ONE container port entry for port 8443
        // (the internal container port for listener port 443, which is < 1024 so gets +8000 offset).
        // Not two (which would be rejected by Kubernetes with:
        // "duplicate entries for key [containerPort=8443,protocol="TCP"]")
        let containers = deployment.spec.unwrap().template.spec.unwrap().containers;
        let ports = containers[0].ports.as_ref().unwrap();

        // Count container ports by port number (443 + 8000 = 8443)
        let container_port = crate::controller::config::compute_container_port(443);
        let port_8443_count = ports
            .iter()
            .filter(|p| p.container_port == container_port as i32)
            .count();

        assert_eq!(
            port_8443_count, 1,
            "Expected exactly 1 ContainerPort for port {} (internal port for listener 443), but found {}. \
             Multiple Gateway listeners can share the same port, but the \
             generated Deployment must deduplicate container ports to avoid \
             Kubernetes API rejection.",
            container_port, port_8443_count
        );

        // Verify all container ports are unique
        let unique_ports: std::collections::HashSet<i32> =
            ports.iter().map(|p| p.container_port).collect();
        assert_eq!(
            ports.len(),
            unique_ports.len(),
            "Container ports must be unique. Found {} ports but only {} unique port numbers.",
            ports.len(),
            unique_ports.len()
        );
    }

    #[test]
    fn test_deployment_port_names_must_not_exceed_15_characters() {
        // Gateway API allows listener names up to 253 characters (SectionName/RFC 1123),
        // but Kubernetes container port names must be <= 15 characters (IANA service name format).
        // This test verifies that port names are properly truncated or transformed.
        let gateway = Gateway {
            metadata: ObjectMeta {
                name: Some("test-gateway".to_string()),
                namespace: Some("default".to_string()),
                uid: Some("test-uid".to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: "multiway".to_string(),
                listeners: vec![gateway_crds::GatewayListeners {
                    name: "https-with-hostname".to_string(), // 18 characters - exceeds limit!
                    port: 443,
                    protocol: "HTTPS".to_string(),
                    hostname: Some("example.com".to_string()),
                    allowed_routes: None,
                    tls: None,
                }],
                addresses: None,
                infrastructure: None,
            },
            status: None,
        };

        let names = DataPlaneNames::new("default", "test-gateway");
        let config = default_config();
        let deployment = build_deployment(&names, &gateway, &config);

        let containers = deployment.spec.unwrap().template.spec.unwrap().containers;
        let ports = containers[0].ports.as_ref().unwrap();

        // Verify all port names are <= 15 characters (Kubernetes IANA service name limit)
        for port in ports {
            if let Some(name) = &port.name {
                assert!(
                    name.len() <= 15,
                    "Container port name '{}' is {} characters, but Kubernetes requires \
                     port names to be at most 15 characters (IANA service name format). \
                     Gateway API listener names can be up to 253 characters, so the \
                     controller must truncate or transform them.",
                    name,
                    name.len()
                );
            }
        }
    }

    #[test]
    fn test_service_port_names_must_not_exceed_15_characters() {
        // Same issue applies to Service port names
        let gateway = Gateway {
            metadata: ObjectMeta {
                name: Some("test-gateway".to_string()),
                namespace: Some("default".to_string()),
                uid: Some("test-uid".to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: "multiway".to_string(),
                listeners: vec![gateway_crds::GatewayListeners {
                    name: "https-with-hostname".to_string(), // 18 characters - exceeds limit!
                    port: 443,
                    protocol: "HTTPS".to_string(),
                    hostname: Some("example.com".to_string()),
                    allowed_routes: None,
                    tls: None,
                }],
                addresses: None,
                infrastructure: None,
            },
            status: None,
        };

        let names = DataPlaneNames::new("default", "test-gateway");
        let service = build_service(&names, &gateway);

        let ports = service.spec.unwrap().ports.unwrap();

        // Verify all port names are <= 15 characters (Kubernetes IANA service name limit)
        for port in ports {
            if let Some(name) = &port.name {
                assert!(
                    name.len() <= 15,
                    "Service port name '{}' is {} characters, but Kubernetes requires \
                     port names to be at most 15 characters (IANA service name format). \
                     Gateway API listener names can be up to 253 characters, so the \
                     controller must truncate or transform them.",
                    name,
                    name.len()
                );
            }
        }
    }

    // ========================================
    // observedGeneration Tests
    // ========================================

    #[test]
    fn test_gateway_class_status_has_correct_observed_generation() {
        // Create a GatewayClass with generation = 1 (simulating a live K8s resource)
        let mut gc = create_gateway_class("multiway", crate::controller::config::CONTROLLER_NAME);
        gc.metadata.generation = Some(1);

        let fixed_time = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let snapshot = WorldSnapshotBuilder::new()
            .at_time(fixed_time)
            .with_gateway_class(gc)
            .build();
        let config = default_config();

        let result = reconcile_gateway_class(&snapshot, &config, "multiway");

        // Should update status
        assert!(
            result.has_status_updates(),
            "Expected status update for GatewayClass"
        );

        let status = result.gateway_class_status_update("multiway").unwrap();
        let conditions = status.conditions.as_ref().unwrap();

        // The Accepted condition must have observedGeneration matching metadata.generation
        let accepted = conditions.iter().find(|c| c.type_ == "Accepted").unwrap();

        assert_eq!(
            accepted.observed_generation,
            Some(1),
            "GatewayClass Accepted condition should have observedGeneration=1 matching \
             metadata.generation, but got {:?}. The Gateway API conformance tests require \
             observedGeneration to match the resource's generation.",
            accepted.observed_generation
        );
    }

    #[test]
    fn test_gateway_status_has_correct_observed_generation() {
        // Create a Gateway with generation = 1 (simulating a live K8s resource)
        let gc = create_accepted_gateway_class("multiway");
        let mut gw = create_gateway("default", "my-gateway", "multiway");
        gw.metadata.generation = Some(1);

        let fixed_time = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let snapshot = WorldSnapshotBuilder::new()
            .at_time(fixed_time)
            .with_gateway_class(gc)
            .with_gateway(gw)
            .build();
        let config = default_config();

        let result = reconcile_gateway(&snapshot, &config, "default", "my-gateway");

        // Should update status
        let status = result
            .gateway_status_update("default", "my-gateway")
            .expect("Expected status update for Gateway");
        let conditions = status.conditions.as_ref().unwrap();

        // Both Accepted and Programmed conditions must have correct observedGeneration
        let accepted = conditions
            .iter()
            .find(|c| c.type_ == "Accepted")
            .expect("Expected Accepted condition");
        let programmed = conditions
            .iter()
            .find(|c| c.type_ == "Programmed")
            .expect("Expected Programmed condition");

        assert_eq!(
            accepted.observed_generation,
            Some(1),
            "Gateway Accepted condition should have observedGeneration=1 matching \
             metadata.generation, but got {:?}. The Gateway API conformance tests require \
             observedGeneration to match the resource's generation.",
            accepted.observed_generation
        );

        assert_eq!(
            programmed.observed_generation,
            Some(1),
            "Gateway Programmed condition should have observedGeneration=1 matching \
             metadata.generation, but got {:?}. The Gateway API conformance tests require \
             observedGeneration to match the resource's generation.",
            programmed.observed_generation
        );
    }

    #[test]
    fn test_gateway_listener_status_has_correct_observed_generation() {
        // Create a Gateway with generation = 1
        let gc = create_accepted_gateway_class("multiway");
        let mut gw = create_gateway("default", "my-gateway", "multiway");
        gw.metadata.generation = Some(1);

        let fixed_time = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let snapshot = WorldSnapshotBuilder::new()
            .at_time(fixed_time)
            .with_gateway_class(gc)
            .with_gateway(gw)
            .build();
        let config = default_config();

        let result = reconcile_gateway(&snapshot, &config, "default", "my-gateway");

        let status = result
            .gateway_status_update("default", "my-gateway")
            .expect("Expected status update for Gateway");
        let listeners = status.listeners.as_ref().unwrap();

        // Each listener status should also have correct observedGeneration
        for listener in listeners {
            for condition in &listener.conditions {
                assert_eq!(
                    condition.observed_generation,
                    Some(1),
                    "Gateway listener '{}' condition '{}' should have observedGeneration=1, \
                     but got {:?}. All conditions must track the resource generation.",
                    listener.name,
                    condition.type_,
                    condition.observed_generation
                );
            }
        }
    }

    #[test]
    fn test_gateway_class_status_requires_non_none_generation() {
        // BUG REPRODUCTION: When metadata.generation is None, the observedGeneration
        // will be None. When serialized to JSON and sent to Kubernetes, this may
        // result in observedGeneration: 0 or null, which fails conformance tests.
        //
        // While Kubernetes should always set generation for live resources,
        // this test documents the expected behavior and guards against regressions.
        let mut gc = create_gateway_class("multiway", crate::controller::config::CONTROLLER_NAME);
        gc.metadata.generation = None; // Simulate missing generation

        let fixed_time = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let snapshot = WorldSnapshotBuilder::new()
            .at_time(fixed_time)
            .with_gateway_class(gc)
            .build();
        let config = default_config();

        let result = reconcile_gateway_class(&snapshot, &config, "multiway");

        let status = result.gateway_class_status_update("multiway").unwrap();
        let conditions = status.conditions.as_ref().unwrap();
        let accepted = conditions.iter().find(|c| c.type_ == "Accepted").unwrap();

        // When generation is None, observedGeneration should also be None.
        // This is correct behavior - the issue is that Kubernetes may interpret
        // None as 0 during serialization/deserialization. This test documents
        // the current behavior. A fix would be to handle None explicitly and
        // either skip the status update or use a default value.
        assert!(
            accepted.observed_generation.is_none(),
            "When metadata.generation is None, observedGeneration should be None. \
             Got {:?}. If this becomes Some(0), there's a serialization issue.",
            accepted.observed_generation
        );
    }
}
