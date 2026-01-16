//! World snapshot types for the functional core.
//!
//! A [`WorldSnapshot`] captures all cluster state needed for reconciliation
//! decisions at a specific point in time. This is the sole input to the
//! functional reconciliation core.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gateway_crds::{Gateway, GatewayClass, HTTPRoute, ReferenceGrant};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Node, Secret, Service};
use kube::ResourceExt;

/// A point-in-time snapshot of relevant cluster state.
///
/// This is the sole input to the functional reconciliation core.
/// All decisions are made based on this snapshot, making the
/// reconciliation logic pure and deterministic.
#[derive(Debug, Clone, Default)]
pub struct WorldSnapshot {
    /// Current timestamp (injected for deterministic testing)
    pub now: DateTime<Utc>,

    /// All GatewayClasses in the cluster
    /// Key: name (cluster-scoped)
    pub gateway_classes: BTreeMap<String, GatewayClass>,

    /// All Gateways we're watching
    /// Key: (namespace, name)
    pub gateways: BTreeMap<(String, String), Gateway>,

    /// All HTTPRoutes we're watching
    /// Key: (namespace, name)
    pub httproutes: BTreeMap<(String, String), HTTPRoute>,

    /// Services that backends reference
    /// Key: (namespace, name)
    pub services: BTreeMap<(String, String), Service>,

    /// ConfigMaps managed by our controller
    /// Key: (namespace, name)
    pub configmaps: BTreeMap<(String, String), ConfigMap>,

    /// Deployments managed by our controller
    /// Key: (namespace, name)
    pub deployments: BTreeMap<(String, String), Deployment>,

    /// Secrets for TLS certificates
    /// Key: (namespace, name)
    pub secrets: BTreeMap<(String, String), Secret>,

    /// ReferenceGrants for cross-namespace references
    /// Key: (namespace, name)
    pub reference_grants: BTreeMap<(String, String), ReferenceGrant>,

    /// Cluster nodes (for determining external addresses)
    /// Key: node name
    pub nodes: BTreeMap<String, Node>,
}

impl WorldSnapshot {
    /// Create an empty snapshot at the given time
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now,
            gateway_classes: BTreeMap::new(),
            gateways: BTreeMap::new(),
            httproutes: BTreeMap::new(),
            services: BTreeMap::new(),
            configmaps: BTreeMap::new(),
            deployments: BTreeMap::new(),
            secrets: BTreeMap::new(),
            reference_grants: BTreeMap::new(),
            nodes: BTreeMap::new(),
        }
    }

    /// Create an empty snapshot at the current time
    pub fn now() -> Self {
        Self::new(Utc::now())
    }

    /// Get a GatewayClass by name
    pub fn get_gateway_class(&self, name: &str) -> Option<&GatewayClass> {
        self.gateway_classes.get(name)
    }

    /// Get a Gateway by namespace and name
    pub fn get_gateway(&self, namespace: &str, name: &str) -> Option<&Gateway> {
        self.gateways
            .get(&(namespace.to_string(), name.to_string()))
    }

    /// Get an HTTPRoute by namespace and name
    pub fn get_httproute(&self, namespace: &str, name: &str) -> Option<&HTTPRoute> {
        self.httproutes
            .get(&(namespace.to_string(), name.to_string()))
    }

    /// Get a Service by namespace and name
    pub fn get_service(&self, namespace: &str, name: &str) -> Option<&Service> {
        self.services
            .get(&(namespace.to_string(), name.to_string()))
    }

    /// Get a ConfigMap by namespace and name
    pub fn get_configmap(&self, namespace: &str, name: &str) -> Option<&ConfigMap> {
        self.configmaps
            .get(&(namespace.to_string(), name.to_string()))
    }

    /// Get a Deployment by namespace and name
    pub fn get_deployment(&self, namespace: &str, name: &str) -> Option<&Deployment> {
        self.deployments
            .get(&(namespace.to_string(), name.to_string()))
    }

    /// Get a Secret by namespace and name
    pub fn get_secret(&self, namespace: &str, name: &str) -> Option<&Secret> {
        self.secrets.get(&(namespace.to_string(), name.to_string()))
    }

    /// Get all HTTPRoutes referencing a specific Gateway
    pub fn routes_for_gateway(&self, gateway_ns: &str, gateway_name: &str) -> Vec<&HTTPRoute> {
        self.httproutes
            .values()
            .filter(|route| {
                route.spec.parent_refs.as_ref().is_some_and(|refs| {
                    refs.iter().any(|r| {
                        let ref_ns = r
                            .namespace
                            .as_deref()
                            .unwrap_or(route.metadata.namespace.as_deref().unwrap_or_default());
                        ref_ns == gateway_ns && r.name == gateway_name
                    })
                })
            })
            .collect()
    }

    /// Get all Gateways using a specific GatewayClass
    pub fn gateways_for_class(&self, class_name: &str) -> Vec<&Gateway> {
        self.gateways
            .values()
            .filter(|gw| gw.spec.gateway_class_name == class_name)
            .collect()
    }

    /// Check if a service exists
    pub fn service_exists(&self, namespace: &str, name: &str) -> bool {
        self.services
            .contains_key(&(namespace.to_string(), name.to_string()))
    }

    /// Check if a secret exists
    pub fn secret_exists(&self, namespace: &str, name: &str) -> bool {
        self.secrets
            .contains_key(&(namespace.to_string(), name.to_string()))
    }

    /// Check if a ReferenceGrant allows a cross-namespace reference
    pub fn is_reference_allowed(
        &self,
        from_ns: &str,
        from_group: &str,
        from_kind: &str,
        to_ns: &str,
        to_group: &str,
        to_kind: &str,
    ) -> bool {
        self.reference_grants
            .values()
            .filter(|grant| grant.metadata.namespace.as_deref() == Some(to_ns))
            .any(|grant| {
                let allows_from = grant.spec.from.iter().any(|f| {
                    f.namespace == from_ns && f.group == from_group && f.kind == from_kind
                });
                let allows_to = grant
                    .spec
                    .to
                    .iter()
                    .any(|t| t.group == to_group && t.kind == to_kind);
                allows_from && allows_to
            })
    }

    /// Check if a cross-namespace Gateway reference is allowed
    pub fn is_gateway_reference_allowed(&self, from_ns: &str, to_ns: &str) -> bool {
        self.is_reference_allowed(
            from_ns,
            "gateway.networking.k8s.io",
            "HTTPRoute",
            to_ns,
            "gateway.networking.k8s.io",
            "Gateway",
        )
    }

    /// Check if a cross-namespace Service reference is allowed
    pub fn is_service_reference_allowed(&self, from_ns: &str, to_ns: &str) -> bool {
        self.is_reference_allowed(
            from_ns,
            "gateway.networking.k8s.io",
            "HTTPRoute",
            to_ns,
            "",
            "Service",
        )
    }

    /// Get the first available node internal IP address.
    ///
    /// This is used to determine the external address for Gateways when using
    /// NodePort services (e.g., in Kind clusters without LoadBalancer support).
    /// Returns the first InternalIP address found from any Ready node.
    pub fn get_node_internal_ip(&self) -> Option<String> {
        for node in self.nodes.values() {
            // Check if node is Ready
            let is_ready = node
                .status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .map(|conditions| {
                    conditions
                        .iter()
                        .any(|c| c.type_ == "Ready" && c.status == "True")
                })
                .unwrap_or(false);

            if !is_ready {
                continue;
            }

            // Get InternalIP address
            if let Some(addresses) = node.status.as_ref().and_then(|s| s.addresses.as_ref()) {
                for addr in addresses {
                    if addr.type_ == "InternalIP" {
                        return Some(addr.address.clone());
                    }
                }
            }
        }
        None
    }
}

/// Builder for constructing test snapshots.
///
/// # Example
///
/// ```ignore
/// use controlplane::core::WorldSnapshotBuilder;
///
/// let snapshot = WorldSnapshotBuilder::new()
///     .at_time(fixed_time)
///     .with_gateway_class(my_gateway_class)
///     .with_gateway(my_gateway)
///     .with_httproute(my_route)
///     .build();
/// ```
#[derive(Debug, Default)]
pub struct WorldSnapshotBuilder {
    snapshot: WorldSnapshot,
}

impl WorldSnapshotBuilder {
    /// Create a new builder with the current time
    pub fn new() -> Self {
        Self {
            snapshot: WorldSnapshot::now(),
        }
    }

    /// Set the snapshot time
    pub fn at_time(mut self, time: DateTime<Utc>) -> Self {
        self.snapshot.now = time;
        self
    }

    /// Add a GatewayClass to the snapshot
    pub fn with_gateway_class(mut self, gc: GatewayClass) -> Self {
        let name = gc.name_any();
        self.snapshot.gateway_classes.insert(name, gc);
        self
    }

    /// Add a Gateway to the snapshot
    pub fn with_gateway(mut self, gw: Gateway) -> Self {
        let ns = gw.namespace().unwrap_or_default();
        let name = gw.name_any();
        self.snapshot.gateways.insert((ns, name), gw);
        self
    }

    /// Add an HTTPRoute to the snapshot
    pub fn with_httproute(mut self, route: HTTPRoute) -> Self {
        let ns = route.namespace().unwrap_or_default();
        let name = route.name_any();
        self.snapshot.httproutes.insert((ns, name), route);
        self
    }

    /// Add a Service to the snapshot
    pub fn with_service(mut self, svc: Service) -> Self {
        let ns = svc.namespace().unwrap_or_default();
        let name = svc.name_any();
        self.snapshot.services.insert((ns, name), svc);
        self
    }

    /// Add a ConfigMap to the snapshot
    pub fn with_configmap(mut self, cm: ConfigMap) -> Self {
        let ns = cm.namespace().unwrap_or_default();
        let name = cm.name_any();
        self.snapshot.configmaps.insert((ns, name), cm);
        self
    }

    /// Add a Deployment to the snapshot
    pub fn with_deployment(mut self, dep: Deployment) -> Self {
        let ns = dep.namespace().unwrap_or_default();
        let name = dep.name_any();
        self.snapshot.deployments.insert((ns, name), dep);
        self
    }

    /// Add a Secret to the snapshot
    pub fn with_secret(mut self, secret: Secret) -> Self {
        let ns = secret.namespace().unwrap_or_default();
        let name = secret.name_any();
        self.snapshot.secrets.insert((ns, name), secret);
        self
    }

    /// Add a ReferenceGrant to the snapshot
    pub fn with_reference_grant(mut self, grant: ReferenceGrant) -> Self {
        let ns = grant.namespace().unwrap_or_default();
        let name = grant.name_any();
        self.snapshot.reference_grants.insert((ns, name), grant);
        self
    }

    /// Add a Node to the snapshot
    pub fn with_node(mut self, node: Node) -> Self {
        let name = node.name_any();
        self.snapshot.nodes.insert(name, node);
        self
    }

    /// Build the snapshot
    pub fn build(self) -> WorldSnapshot {
        self.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use gateway_crds::{GatewayClassSpec, GatewaySpec, HttpRouteSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    fn create_gateway_class(name: &str, controller: &str) -> GatewayClass {
        GatewayClass {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
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

    fn create_gateway(namespace: &str, name: &str, class_name: &str) -> Gateway {
        Gateway {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: class_name.to_string(),
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
        namespace: &str,
        name: &str,
        gateway_ns: &str,
        gateway_name: &str,
    ) -> HTTPRoute {
        HTTPRoute {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                ..Default::default()
            },
            spec: HttpRouteSpec {
                parent_refs: Some(vec![gateway_crds::HttpRouteParentRefs {
                    group: Some("gateway.networking.k8s.io".to_string()),
                    kind: Some("Gateway".to_string()),
                    namespace: Some(gateway_ns.to_string()),
                    name: gateway_name.to_string(),
                    section_name: None,
                    port: None,
                }]),
                hostnames: None,
                rules: None,
            },
            status: None,
        }
    }

    #[test]
    fn test_snapshot_builder() {
        let fixed_time = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let gc = create_gateway_class("multiway", "io.multiway/gateway-controller");
        let gw = create_gateway("default", "my-gateway", "multiway");

        let snapshot = WorldSnapshotBuilder::new()
            .at_time(fixed_time)
            .with_gateway_class(gc)
            .with_gateway(gw)
            .build();

        assert_eq!(snapshot.now, fixed_time);
        assert!(snapshot.get_gateway_class("multiway").is_some());
        assert!(snapshot.get_gateway("default", "my-gateway").is_some());
    }

    #[test]
    fn test_routes_for_gateway() {
        let gc = create_gateway_class("multiway", "io.multiway/gateway-controller");
        let gw = create_gateway("default", "my-gateway", "multiway");
        let route1 = create_httproute("default", "route1", "default", "my-gateway");
        let route2 = create_httproute("default", "route2", "default", "my-gateway");
        let route3 = create_httproute("default", "route3", "default", "other-gateway");

        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(gc)
            .with_gateway(gw)
            .with_httproute(route1)
            .with_httproute(route2)
            .with_httproute(route3)
            .build();

        let routes = snapshot.routes_for_gateway("default", "my-gateway");
        assert_eq!(routes.len(), 2);
    }

    #[test]
    fn test_gateways_for_class() {
        let gc = create_gateway_class("multiway", "io.multiway/gateway-controller");
        let gw1 = create_gateway("default", "gateway1", "multiway");
        let gw2 = create_gateway("default", "gateway2", "multiway");
        let gw3 = create_gateway("default", "gateway3", "other-class");

        let snapshot = WorldSnapshotBuilder::new()
            .with_gateway_class(gc)
            .with_gateway(gw1)
            .with_gateway(gw2)
            .with_gateway(gw3)
            .build();

        let gateways = snapshot.gateways_for_class("multiway");
        assert_eq!(gateways.len(), 2);
    }

    #[test]
    fn test_service_exists() {
        let svc = Service {
            metadata: ObjectMeta {
                name: Some("my-service".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let snapshot = WorldSnapshotBuilder::new().with_service(svc).build();

        assert!(snapshot.service_exists("default", "my-service"));
        assert!(!snapshot.service_exists("default", "nonexistent"));
        assert!(!snapshot.service_exists("other-ns", "my-service"));
    }

    #[test]
    fn test_reference_grant_checking() {
        let grant = ReferenceGrant {
            metadata: ObjectMeta {
                name: Some("allow-routes".to_string()),
                namespace: Some("backend-ns".to_string()),
                ..Default::default()
            },
            spec: gateway_crds::ReferenceGrantSpec {
                from: vec![gateway_crds::ReferenceGrantFrom {
                    group: "gateway.networking.k8s.io".to_string(),
                    kind: "HTTPRoute".to_string(),
                    namespace: "route-ns".to_string(),
                }],
                to: vec![gateway_crds::ReferenceGrantTo {
                    group: "".to_string(),
                    kind: "Service".to_string(),
                    name: None,
                }],
            },
        };

        let snapshot = WorldSnapshotBuilder::new()
            .with_reference_grant(grant)
            .build();

        // Should allow route-ns -> backend-ns Service reference
        assert!(snapshot.is_service_reference_allowed("route-ns", "backend-ns"));

        // Should not allow other-ns -> backend-ns
        assert!(!snapshot.is_service_reference_allowed("other-ns", "backend-ns"));

        // Should not allow route-ns -> other-ns
        assert!(!snapshot.is_service_reference_allowed("route-ns", "other-ns"));
    }
}
