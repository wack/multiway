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
use gateway_crds::{GatewayClass, GatewayClassStatus, GatewayClassStatusSupportedFeatures};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::chrono::Utc;
use kube::api::{Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Api, Client, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, error, info, instrument, warn};

use super::config::CONTROLLER_NAME;
use super::context::ControllerContext;
use super::error::{ControllerError, Result};

/// Supported features for this Gateway implementation
const SUPPORTED_FEATURES: &[&str] = &[
    "Gateway",
    "HTTPRoute",
    "HTTPRouteDestinationPortMatching",
    "HTTPRouteHostRewrite",
    "HTTPRouteMethodMatching",
    "HTTPRoutePathRedirect",
    "HTTPRoutePathRewrite",
    "HTTPRoutePortRedirect",
    "HTTPRouteQueryParamMatching",
    "HTTPRouteRequestHeaderModifier",
    "HTTPRouteRequestMirror",
    "HTTPRouteRequestRedirect",
    "HTTPRouteResponseHeaderModifier",
    "HTTPRouteSchemeRedirect",
];

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

/// Reconcile a single GatewayClass resource
#[instrument(skip_all, fields(name = %gateway_class.name_any()))]
async fn reconcile_gateway_class(
    gateway_class: Arc<GatewayClass>,
    ctx: Arc<ControllerContext>,
) -> Result<Action> {
    let name = gateway_class.name_any();
    info!("Reconciling GatewayClass");

    // Check if this GatewayClass is for our controller
    if gateway_class.spec.controller_name != CONTROLLER_NAME {
        debug!(
            controller_name = %gateway_class.spec.controller_name,
            expected = %CONTROLLER_NAME,
            "Ignoring GatewayClass with different controller"
        );
        // Requeue after a long interval since we're not responsible for this class
        return Ok(Action::requeue(Duration::from_secs(3600)));
    }

    // Validate the GatewayClass configuration
    let (accepted, reason, message) = validate_gateway_class(&gateway_class);

    // Update the status
    update_gateway_class_status(
        &ctx.client,
        &name,
        gateway_class.metadata.generation,
        accepted,
        reason,
        &message,
    )
    .await?;

    if accepted {
        info!("GatewayClass accepted");
    } else {
        warn!(reason, %message, "GatewayClass not accepted");
    }

    // Requeue after the configured interval
    Ok(Action::requeue(Duration::from_secs(
        ctx.config.requeue_after_secs,
    )))
}

/// Validate the GatewayClass configuration
///
/// Returns (accepted, reason, message)
fn validate_gateway_class(gateway_class: &GatewayClass) -> (bool, &'static str, String) {
    // Check for parametersRef - we don't support custom parameters yet
    if let Some(params_ref) = &gateway_class.spec.parameters_ref {
        // Log that we're ignoring parameters
        debug!(
            group = %params_ref.group,
            kind = %params_ref.kind,
            name = %params_ref.name,
            "GatewayClass has parametersRef (not currently supported, will be ignored)"
        );
    }

    // The GatewayClass is valid and accepted
    (
        true,
        "Accepted",
        "GatewayClass is accepted by the multiway controller".to_string(),
    )
}

/// Update the status of a GatewayClass
async fn update_gateway_class_status(
    client: &Client,
    name: &str,
    generation: Option<i64>,
    accepted: bool,
    reason: &str,
    message: &str,
) -> Result<()> {
    let api: Api<GatewayClass> = Api::all(client.clone());

    let now = Time(Utc::now());
    let status_value = if accepted { "True" } else { "False" };

    let condition = Condition {
        type_: "Accepted".to_string(),
        status: status_value.to_string(),
        observed_generation: generation,
        last_transition_time: now,
        reason: reason.to_string(),
        message: message.to_string(),
    };

    // Build supported features list
    let supported_features: Vec<GatewayClassStatusSupportedFeatures> = SUPPORTED_FEATURES
        .iter()
        .map(|f| GatewayClassStatusSupportedFeatures {
            name: f.to_string(),
        })
        .collect();

    let status = GatewayClassStatus {
        conditions: Some(vec![condition]),
        supported_features: Some(supported_features),
    };

    let patch = serde_json::json!({
        "status": status
    });

    api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;

    Ok(())
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
        .map(|conditions| {
            conditions
                .iter()
                .any(|c| c.type_ == "Accepted" && c.status == "True")
        })
        .unwrap_or(false)
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
    use gateway_crds::GatewayClassParametersRef;

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

    /// Spec: GatewayClass.spec.controllerName determines which controller is responsible
    /// Controllers should only respond to GatewayClasses with matching controllerName
    #[test]
    fn test_controller_name_matching() {
        // Our controller name should be accepted
        let gc = create_test_gateway_class(CONTROLLER_NAME);
        let (accepted, reason, _) = validate_gateway_class(&gc);
        assert!(accepted);
        assert_eq!(reason, "Accepted");
    }

    /// Spec: Controller name format is recommended to be domain/path
    /// Our controller name follows this convention: io.multiway/gateway-controller
    #[test]
    fn test_controller_name_format() {
        // Verify our controller name follows domain/path convention
        assert!(CONTROLLER_NAME.contains('/'));
        assert!(CONTROLLER_NAME.starts_with("io.multiway"));
    }

    /// Spec: A new GatewayClass should start with Accepted=False until processed
    /// Once processed, condition should be set to True
    #[test]
    fn test_validate_gateway_class() {
        let gc = create_test_gateway_class(CONTROLLER_NAME);
        let (accepted, reason, _message) = validate_gateway_class(&gc);
        assert!(accepted);
        assert_eq!(reason, "Accepted");
    }

    /// Spec: parametersRef allows passing controller-specific parameters
    /// Our implementation logs but ignores parametersRef (still accepts the class)
    #[test]
    fn test_gateway_class_with_parameters_ref() {
        let mut gc = create_test_gateway_class(CONTROLLER_NAME);
        gc.spec.parameters_ref = Some(GatewayClassParametersRef {
            group: "example.net".to_string(),
            kind: "Config".to_string(),
            name: "my-config".to_string(),
            namespace: Some("default".to_string()),
        });

        // Should still be accepted even with parametersRef
        let (accepted, reason, _) = validate_gateway_class(&gc);
        assert!(accepted);
        assert_eq!(reason, "Accepted");
    }

    /// Spec: GatewayClass with optional description field
    #[test]
    fn test_gateway_class_with_description() {
        let mut gc = create_test_gateway_class(CONTROLLER_NAME);
        gc.spec.description = Some("A test gateway class for unit testing".to_string());

        let (accepted, reason, _) = validate_gateway_class(&gc);
        assert!(accepted);
        assert_eq!(reason, "Accepted");
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
