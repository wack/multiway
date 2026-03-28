//! GatewayClass reconciler
//!
//! This module implements the reconciliation logic for GatewayClass resources.
//! According to the Gateway API specification:
//!
//! - GatewayClass is a cluster-scoped resource
//! - Controllers should only respond to GatewayClasses with matching controllerName
//! - The controller must set the "Accepted" condition to indicate it recognizes the class
//! - The "SupportedFeatures" status field indicates which features are supported

use std::sync::Arc;

use futures::StreamExt;
use gateway_crds::GatewayClass;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Api, Client, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, error, info, instrument};

use super::config::CONTROLLER_NAME;
use super::context::ControllerContext;
use super::error::{ControllerError, Result};
use crate::core;
use crate::shell::{ReconcileExecutor, SnapshotFetcher};

/// Run the GatewayClass controller
///
/// This function starts the reconciliation loop for GatewayClass resources.
/// It watches for changes and reconciles each GatewayClass that has the
/// matching controller name.
pub async fn run_gateway_class_controller(ctx: Arc<ControllerContext>) {
    info!("Starting GatewayClass controller");

    let client = ctx.client.clone();
    let gateway_classes: Api<GatewayClass> = Api::all(client);

    let controller = Controller::new(gateway_classes.clone(), WatcherConfig::default())
        .shutdown_on_signal()
        .run(
            |obj, ctx| async move { reconcile_gateway_class(obj, ctx).await },
            error_policy,
            ctx,
        )
        .for_each(|result| async move {
            match result {
                Ok((obj, action)) => {
                    debug!(
                        name = %obj.name,
                        ?action,
                        "GatewayClass reconciled successfully"
                    );
                }
                Err(error) => {
                    error!(%error, "GatewayClass reconciliation error");
                }
            }
        });

    controller.await;
}

/// Reconcile a single GatewayClass resource using the functional core
#[instrument(skip_all, fields(name = %gateway_class.name_any()))]
async fn reconcile_gateway_class(
    gateway_class: Arc<GatewayClass>,
    ctx: Arc<ControllerContext>,
) -> Result<Action> {
    let name = gateway_class.name_any();
    info!("Reconciling GatewayClass");

    // Build snapshot from cluster state
    let fetcher = SnapshotFetcher::new(ctx.client.clone());
    let snapshot = fetcher.snapshot_for_gateway_class(&name).await?;

    // Pure reconciliation - compute what needs to be done
    let result = core::reconcile_gateway_class(&snapshot, &ctx.config, &name);

    // Execute the computed effects
    let executor = ReconcileExecutor::new(ctx.client.clone());
    executor.execute(result).await
}

/// Error policy for GatewayClass reconciliation
fn error_policy(
    _obj: Arc<GatewayClass>,
    error: &ControllerError,
    ctx: Arc<ControllerContext>,
) -> Action {
    error!(%error, "GatewayClass reconciliation failed");
    Action::requeue(Duration::from_secs(ctx.config.error_requeue_secs))
}

/// Check if a GatewayClass is accepted by this controller
pub fn is_gateway_class_accepted(gateway_class: &GatewayClass) -> bool {
    // First check if it's our controller
    if gateway_class.spec.controller_name != CONTROLLER_NAME {
        return false;
    }

    // Then check if we've accepted it
    gateway_class
        .status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|c| c.type_ == "Accepted" && c.status == "True")
        })
}

/// Get the GatewayClass for a Gateway, if it's accepted
pub async fn get_accepted_gateway_class(
    client: &Client,
    class_name: &str,
) -> Result<Option<GatewayClass>> {
    let api: Api<GatewayClass> = Api::all(client.clone());

    match api.get(class_name).await {
        Ok(gateway_class) => {
            if is_gateway_class_accepted(&gateway_class) {
                Ok(Some(gateway_class))
            } else {
                Ok(None)
            }
        }
        Err(kube::Error::Api(err)) if err.code == 404 => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::validate::SUPPORTED_FEATURES;
    use gateway_crds::GatewayClassStatus;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
    use k8s_openapi::chrono::Utc;

    fn create_test_gateway_class(controller_name: &str) -> GatewayClass {
        GatewayClass {
            metadata: kube::core::ObjectMeta {
                name: Some("test-class".to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: gateway_crds::GatewayClassSpec {
                controller_name: controller_name.to_string(),
                description: None,
                parameters_ref: None,
            },
            status: None,
        }
    }

    // ==========================================
    // GatewayClass Spec Compliance Tests
    // ==========================================

    /// Spec: Controller name format is recommended to be domain/path
    /// Our controller name follows this convention: io.multiway/gateway-controller
    #[test]
    fn test_controller_name_format() {
        // Verify our controller name follows domain/path convention
        assert!(CONTROLLER_NAME.contains('/'));
        assert!(CONTROLLER_NAME.starts_with("io.multiway"));
    }

    // ==========================================
    // GatewayClass Status Tests
    // ==========================================

    /// Spec: is_gateway_class_accepted should check both controllerName AND status
    #[test]
    fn test_is_gateway_class_accepted() {
        // Not our controller - should return false regardless of status
        let gc = create_test_gateway_class("other-controller");
        assert!(!is_gateway_class_accepted(&gc));

        // Our controller but no status - should return false
        let gc = create_test_gateway_class(CONTROLLER_NAME);
        assert!(!is_gateway_class_accepted(&gc));

        // Our controller with accepted status - should return true
        let mut gc = create_test_gateway_class(CONTROLLER_NAME);
        gc.status = Some(GatewayClassStatus {
            conditions: Some(vec![Condition {
                type_: "Accepted".to_string(),
                status: "True".to_string(),
                observed_generation: Some(1),
                last_transition_time: Time(Utc::now()),
                reason: "Accepted".to_string(),
                message: "Accepted".to_string(),
            }]),
            supported_features: None,
        });
        assert!(is_gateway_class_accepted(&gc));
    }

    /// Spec: Accepted condition with status=False means class is not usable
    #[test]
    fn test_is_gateway_class_not_accepted_with_false_status() {
        let mut gc = create_test_gateway_class(CONTROLLER_NAME);
        gc.status = Some(GatewayClassStatus {
            conditions: Some(vec![Condition {
                type_: "Accepted".to_string(),
                status: "False".to_string(),
                observed_generation: Some(1),
                last_transition_time: Time(Utc::now()),
                reason: "InvalidConfiguration".to_string(),
                message: "Configuration is invalid".to_string(),
            }]),
            supported_features: None,
        });
        assert!(!is_gateway_class_accepted(&gc));
    }

    /// Spec: observedGeneration should track which generation was processed
    #[test]
    fn test_observed_generation_tracking() {
        let mut gc = create_test_gateway_class(CONTROLLER_NAME);
        gc.metadata.generation = Some(5);
        gc.status = Some(GatewayClassStatus {
            conditions: Some(vec![Condition {
                type_: "Accepted".to_string(),
                status: "True".to_string(),
                observed_generation: Some(5),
                last_transition_time: Time(Utc::now()),
                reason: "Accepted".to_string(),
                message: "Accepted".to_string(),
            }]),
            supported_features: None,
        });

        // Should be accepted when observedGeneration matches resource generation
        assert!(is_gateway_class_accepted(&gc));
    }

    /// Spec: Empty conditions list should be treated as not accepted
    #[test]
    fn test_empty_conditions_not_accepted() {
        let mut gc = create_test_gateway_class(CONTROLLER_NAME);
        gc.status = Some(GatewayClassStatus {
            conditions: Some(vec![]),
            supported_features: None,
        });
        assert!(!is_gateway_class_accepted(&gc));
    }

    /// Spec: Missing Accepted condition type should be treated as not accepted
    #[test]
    fn test_missing_accepted_condition() {
        let mut gc = create_test_gateway_class(CONTROLLER_NAME);
        gc.status = Some(GatewayClassStatus {
            conditions: Some(vec![Condition {
                type_: "SomeOtherCondition".to_string(),
                status: "True".to_string(),
                observed_generation: Some(1),
                last_transition_time: Time(Utc::now()),
                reason: "Something".to_string(),
                message: "Something".to_string(),
            }]),
            supported_features: None,
        });
        assert!(!is_gateway_class_accepted(&gc));
    }

    // ==========================================
    // Supported Features Tests (Core Gateway API features)
    // ==========================================

    /// Spec: Core features must be supported by implementations
    #[test]
    fn test_supported_features() {
        // Core features
        assert!(SUPPORTED_FEATURES.contains(&"Gateway"));
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRoute"));
    }

    /// Spec: Extended features for HTTPRoute matching
    #[test]
    fn test_extended_httproute_matching_features() {
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRouteMethodMatching"));
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRouteQueryParamMatching"));
    }

    /// Spec: Extended features for HTTPRoute filters
    #[test]
    fn test_extended_httproute_filter_features() {
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRouteRequestHeaderModifier"));
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRouteResponseHeaderModifier"));
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRouteRequestRedirect"));
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRouteRequestMirror"));
    }

    /// Spec: Extended features for HTTPRoute rewrites
    #[test]
    fn test_extended_httproute_rewrite_features() {
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRouteHostRewrite"));
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRoutePathRewrite"));
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRoutePathRedirect"));
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRouteSchemeRedirect"));
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRoutePortRedirect"));
    }

    /// Spec: Backend destination port matching feature
    #[test]
    fn test_backend_destination_port_feature() {
        assert!(SUPPORTED_FEATURES.contains(&"HTTPRouteDestinationPortMatching"));
    }
}
