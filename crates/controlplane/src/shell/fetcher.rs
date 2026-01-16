//! Snapshot fetcher - builds WorldSnapshots from the Kubernetes API.
//!
//! This module is responsible for fetching all relevant resources from the
//! Kubernetes API and building a coherent [`WorldSnapshot`] for reconciliation.

use gateway_crds::{Gateway, GatewayClass, HTTPRoute, ReferenceGrant};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Node, Service};
use k8s_openapi::chrono::Utc;
use kube::api::ListParams;
use kube::{Api, Client, ResourceExt};
use tracing::{debug, warn};

use crate::controller::config::DataPlaneNames;
use crate::controller::error::Result;
use crate::core::snapshot::{WorldSnapshot, WorldSnapshotBuilder};

/// Fetches resources from Kubernetes to build WorldSnapshots.
pub struct SnapshotFetcher {
    client: Client,
}

impl SnapshotFetcher {
    /// Create a new SnapshotFetcher
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Build a WorldSnapshot for reconciling a specific GatewayClass
    pub async fn snapshot_for_gateway_class(&self, name: &str) -> Result<WorldSnapshot> {
        let mut builder = WorldSnapshotBuilder::new().at_time(Utc::now());

        // Fetch the specific GatewayClass
        let gc_api: Api<GatewayClass> = Api::all(self.client.clone());
        match gc_api.get(name).await {
            Ok(gc) => {
                builder = builder.with_gateway_class(gc);
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                debug!(name, "GatewayClass not found");
            }
            Err(e) => return Err(e.into()),
        }

        Ok(builder.build())
    }

    /// Build a WorldSnapshot for reconciling a specific Gateway
    pub async fn snapshot_for_gateway(&self, namespace: &str, name: &str) -> Result<WorldSnapshot> {
        let mut builder = WorldSnapshotBuilder::new().at_time(Utc::now());

        // Fetch the Gateway
        let gw_api: Api<Gateway> = Api::namespaced(self.client.clone(), namespace);
        let gateway = match gw_api.get(name).await {
            Ok(gw) => {
                builder = builder.with_gateway(gw.clone());
                Some(gw)
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                debug!(namespace, name, "Gateway not found");
                None
            }
            Err(e) => return Err(e.into()),
        };

        // If we have the gateway, fetch its GatewayClass
        if let Some(ref gw) = gateway {
            let gc_api: Api<GatewayClass> = Api::all(self.client.clone());
            match gc_api.get(&gw.spec.gateway_class_name).await {
                Ok(gc) => {
                    builder = builder.with_gateway_class(gc);
                }
                Err(kube::Error::Api(err)) if err.code == 404 => {
                    debug!(class = %gw.spec.gateway_class_name, "GatewayClass not found");
                }
                Err(e) => return Err(e.into()),
            }
        }

        // Fetch HTTPRoutes that might reference this Gateway
        builder = self
            .fetch_routes_for_gateway(&mut builder, namespace, name)
            .await?;

        // Fetch existing managed resources
        let names = DataPlaneNames::new(namespace, name);
        builder = self.fetch_managed_resources(&mut builder, &names).await?;

        // Fetch nodes (for determining external address when using NodePort/hostPort)
        builder = self.fetch_nodes(&mut builder).await?;

        Ok(builder.build())
    }

    /// Build a WorldSnapshot for reconciling a specific HTTPRoute
    pub async fn snapshot_for_httproute(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<WorldSnapshot> {
        let mut builder = WorldSnapshotBuilder::new().at_time(Utc::now());

        // Fetch the HTTPRoute
        let route_api: Api<HTTPRoute> = Api::namespaced(self.client.clone(), namespace);
        let route = match route_api.get(name).await {
            Ok(r) => {
                builder = builder.with_httproute(r.clone());
                Some(r)
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                debug!(namespace, name, "HTTPRoute not found");
                None
            }
            Err(e) => return Err(e.into()),
        };

        // Fetch parent Gateways and their GatewayClasses
        if let Some(ref r) = route
            && let Some(parent_refs) = &r.spec.parent_refs
        {
            for parent_ref in parent_refs {
                let parent_ns = parent_ref.namespace.as_deref().unwrap_or(namespace);
                let parent_name = &parent_ref.name;

                // Only process Gateway parents
                let parent_kind = parent_ref.kind.as_deref().unwrap_or("Gateway");
                let parent_group = parent_ref
                    .group
                    .as_deref()
                    .unwrap_or("gateway.networking.k8s.io");

                if parent_group != "gateway.networking.k8s.io" || parent_kind != "Gateway" {
                    continue;
                }

                // Fetch the Gateway
                let gw_api: Api<Gateway> = Api::namespaced(self.client.clone(), parent_ns);
                match gw_api.get(parent_name).await {
                    Ok(gw) => {
                        // Fetch the GatewayClass
                        let gc_api: Api<GatewayClass> = Api::all(self.client.clone());
                        if let Ok(gc) = gc_api.get(&gw.spec.gateway_class_name).await {
                            builder = builder.with_gateway_class(gc);
                        }

                        // Fetch managed resources for this Gateway
                        let names = DataPlaneNames::new(parent_ns, parent_name);
                        builder = self.fetch_managed_resources(&mut builder, &names).await?;

                        builder = builder.with_gateway(gw);
                    }
                    Err(kube::Error::Api(err)) if err.code == 404 => {
                        debug!(
                            namespace = parent_ns,
                            name = parent_name,
                            "Parent Gateway not found"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, "Error fetching parent Gateway");
                    }
                }
            }
        }

        // Fetch backend Services referenced by the route
        if let Some(ref r) = route {
            builder = self
                .fetch_backend_services(&mut builder, r, namespace)
                .await?;
        }

        // Fetch ReferenceGrants that might be relevant
        builder = self.fetch_reference_grants(&mut builder, namespace).await?;

        Ok(builder.build())
    }

    /// Build a comprehensive WorldSnapshot for a full reconciliation
    ///
    /// This fetches all GatewayClasses, Gateways, and HTTPRoutes that we manage.
    pub async fn snapshot_all(&self, watch_namespace: Option<&str>) -> Result<WorldSnapshot> {
        let mut builder = WorldSnapshotBuilder::new().at_time(Utc::now());

        // Fetch all GatewayClasses
        let gc_api: Api<GatewayClass> = Api::all(self.client.clone());
        let gateway_classes = gc_api.list(&ListParams::default()).await?;
        for gc in gateway_classes {
            builder = builder.with_gateway_class(gc);
        }

        // Fetch Gateways (optionally filtered by namespace)
        let gateways: Vec<Gateway> = match watch_namespace {
            Some(ns) => {
                let api: Api<Gateway> = Api::namespaced(self.client.clone(), ns);
                api.list(&ListParams::default()).await?.items
            }
            None => {
                let api: Api<Gateway> = Api::all(self.client.clone());
                api.list(&ListParams::default()).await?.items
            }
        };

        for gw in gateways {
            builder = builder.with_gateway(gw);
        }

        // Fetch HTTPRoutes
        let routes: Vec<HTTPRoute> = match watch_namespace {
            Some(ns) => {
                let api: Api<HTTPRoute> = Api::namespaced(self.client.clone(), ns);
                api.list(&ListParams::default()).await?.items
            }
            None => {
                let api: Api<HTTPRoute> = Api::all(self.client.clone());
                api.list(&ListParams::default()).await?.items
            }
        };

        for route in routes {
            builder = builder.with_httproute(route);
        }

        // Fetch ReferenceGrants
        let grants: Vec<ReferenceGrant> = match watch_namespace {
            Some(ns) => {
                let api: Api<ReferenceGrant> = Api::namespaced(self.client.clone(), ns);
                api.list(&ListParams::default()).await?.items
            }
            None => {
                let api: Api<ReferenceGrant> = Api::all(self.client.clone());
                api.list(&ListParams::default()).await?.items
            }
        };

        for grant in grants {
            builder = builder.with_reference_grant(grant);
        }

        Ok(builder.build())
    }

    /// Fetch HTTPRoutes that reference a specific Gateway
    async fn fetch_routes_for_gateway(
        &self,
        builder: &mut WorldSnapshotBuilder,
        gateway_ns: &str,
        gateway_name: &str,
    ) -> Result<WorldSnapshotBuilder> {
        // Fetch all HTTPRoutes and filter by parent ref
        // In production, you'd want more efficient filtering
        let route_api: Api<HTTPRoute> = Api::all(self.client.clone());
        let routes = route_api.list(&ListParams::default()).await?;

        let mut result = std::mem::take(builder);

        for route in routes {
            let references_gateway = route.spec.parent_refs.as_ref().is_some_and(|refs| {
                refs.iter().any(|r| {
                    let ref_ns = r
                        .namespace
                        .as_deref()
                        .unwrap_or(route.metadata.namespace.as_deref().unwrap_or_default());
                    ref_ns == gateway_ns && r.name == gateway_name
                })
            });

            if references_gateway {
                // Also fetch backend services for this route
                let route_ns = route.namespace().unwrap_or_default();
                result = self
                    .fetch_backend_services(&mut result, &route, &route_ns)
                    .await?;
                result = result.with_httproute(route);
            }
        }

        Ok(result)
    }

    /// Fetch managed resources (ConfigMap, Service, Deployment) for a Gateway
    async fn fetch_managed_resources(
        &self,
        builder: &mut WorldSnapshotBuilder,
        names: &DataPlaneNames,
    ) -> Result<WorldSnapshotBuilder> {
        let namespace = names.namespace();
        let mut result = std::mem::take(builder);

        // Fetch ConfigMap
        let cm_api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(cm) = cm_api.get(&names.configmap_name()).await {
            result = result.with_configmap(cm);
        }

        // Fetch Service
        let svc_api: Api<Service> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(svc) = svc_api.get(&names.service_name()).await {
            result = result.with_service(svc);
        }

        // Fetch Deployment
        let dep_api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(dep) = dep_api.get(&names.deployment_name()).await {
            result = result.with_deployment(dep);
        }

        Ok(result)
    }

    /// Fetch backend Services referenced by an HTTPRoute
    async fn fetch_backend_services(
        &self,
        builder: &mut WorldSnapshotBuilder,
        route: &HTTPRoute,
        route_namespace: &str,
    ) -> Result<WorldSnapshotBuilder> {
        let mut result = std::mem::take(builder);

        if let Some(rules) = &route.spec.rules {
            for rule in rules {
                if let Some(backends) = &rule.backend_refs {
                    for backend in backends {
                        let backend_ns = backend.namespace.as_deref().unwrap_or(route_namespace);
                        let backend_kind = backend.kind.as_deref().unwrap_or("Service");

                        // Only fetch Services
                        if backend_kind != "Service" {
                            continue;
                        }

                        let svc_api: Api<Service> =
                            Api::namespaced(self.client.clone(), backend_ns);
                        match svc_api.get(&backend.name).await {
                            Ok(svc) => {
                                result = result.with_service(svc);
                            }
                            Err(kube::Error::Api(err)) if err.code == 404 => {
                                debug!(
                                    namespace = backend_ns,
                                    name = %backend.name,
                                    "Backend Service not found"
                                );
                            }
                            Err(e) => {
                                warn!(error = %e, "Error fetching backend Service");
                            }
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    /// Fetch ReferenceGrants that might affect cross-namespace references
    async fn fetch_reference_grants(
        &self,
        builder: &mut WorldSnapshotBuilder,
        namespace: &str,
    ) -> Result<WorldSnapshotBuilder> {
        let mut result = std::mem::take(builder);

        // Fetch ReferenceGrants in the route's namespace and any referenced namespaces
        let grant_api: Api<ReferenceGrant> = Api::all(self.client.clone());
        let grants = grant_api.list(&ListParams::default()).await?;

        for grant in grants {
            // Include grants that might be relevant to cross-namespace references
            let grant_ns = grant.namespace().unwrap_or_default();

            // Include if it's in a namespace we care about
            let is_relevant =
                grant_ns == namespace || grant.spec.from.iter().any(|f| f.namespace == namespace);

            if is_relevant {
                result = result.with_reference_grant(grant);
            }
        }

        Ok(result)
    }

    /// Fetch cluster nodes (for determining external addresses)
    ///
    /// This is used to get the node IP address for Gateway status when using
    /// NodePort services or hostPort (e.g., in Kind clusters without LoadBalancer support).
    async fn fetch_nodes(
        &self,
        builder: &mut WorldSnapshotBuilder,
    ) -> Result<WorldSnapshotBuilder> {
        let mut result = std::mem::take(builder);

        let node_api: Api<Node> = Api::all(self.client.clone());
        match node_api.list(&ListParams::default()).await {
            Ok(nodes) => {
                for node in nodes {
                    result = result.with_node(node);
                }
            }
            Err(e) => {
                // Node access might be restricted by RBAC, log but don't fail
                warn!(error = %e, "Unable to fetch nodes - Gateway addresses may use ClusterIP instead of node IP");
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    // Integration tests for the fetcher would require a real or mock Kubernetes client
    // The fetcher is primarily tested through integration tests
}
