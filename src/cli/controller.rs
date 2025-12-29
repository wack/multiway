//! The API Gateway Controller is a Kubernetes Controller, meaning
//! that it watches Kubernetes resource state and responds to
//! changes in certain resources by "reconciling" the desired
//! state with what's currently in the cluster. The API Gateway
//! Controller, in particular, responds to changes in GatewayClass
//! resources, along with Gateways.

use std::future::ready;
use std::sync::Arc;

use futures::StreamExt;
use gateway_crds::GatewayClass;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::{chrono::Utc, serde_json::json};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams},
    runtime::{
        controller::{Action, Controller as K8sController},
        watcher,
    },
};
use miette::{Diagnostic, IntoDiagnostic as _, Result, WrapErr};
use tokio::{runtime::Runtime, time::Duration};
use tracing::info;

/// Run the API Gateway Controller
pub struct Controller;

impl Controller {
    pub fn new() -> Self {
        Self
    }

    pub fn dispatch(self) -> Result<()> {
        // • First, we kick off a Tokio runtime.
        let rt = Runtime::new().into_diagnostic()?;
        let _guard = rt.enter();

        // • Now, we kick off the control loop, which runs
        //   in the async context.
        rt.block_on(async {
            info!("Starting the MultiTool API Gateway!");
            self.run_control_loop().await
        })
    }

    // `run_control_loop` will execute the control loop, which
    async fn run_control_loop(self) -> Result<()> {
        // • First, we create a new Kubernetes client.
        let client = Self::create_kubectl_client().await?;
        // • Build the API object for the Gateway Class resource type.
        let gateway_classes = Api::<GatewayClass>::all(client.clone());
        // • Create a new Controller for the GatewayClass resource type.
        let config = watcher::Config::default();
        let gateway_class_controller = K8sController::new(gateway_classes.clone(), config);
        // • Run the controller.
        Self::run_gateway_class_control_loop(gateway_class_controller).await;
        Ok(())

        // Watch for changes to GatewayClass resources.
        // • Create the Gateway Class controllr, which watches
        //   only for changes to the Gateway Class.
        // let gateway_class_controller = K8sController::new(gateway_classes.clone());
        // .run(reconcile_gateway_class, error_policy_gateway_class, Arc::new(()))
        // .for_each(|_| ready(()));
    }

    async fn run_gateway_class_control_loop(ctrl: K8sController<GatewayClass>) {
        // We don't share any state, so we pass the unit struct here.
        let shared_state = Arc::new(());
        ctrl.run(reconcile_gateway_class, error_policy, shared_state)
            .for_each(|_| ready(()))
            .await;
    }

    /// Attempt to create the client, converting it into a proper
    /// error if unable.
    async fn create_kubectl_client() -> Result<Client> {
        Client::try_default()
            .await
            .map_err(MultiwayError::from)
            .wrap_err("Could not create Kubernetes client")
    }
}

// TODO: I need to make sure I understand exactly what this is doing and why.
async fn reconcile_gateway_class(
    obj: Arc<GatewayClass>,
    _ctx: Arc<()>,
) -> Result<Action, MultiwayError> {
    info!("reconcile request: {}", obj.name_any());

    if obj.spec.controller_name != "multitool.run/multitool" {
        return Ok(Action::requeue(Duration::from_secs(3600)));
    }

    // Check if status is already set to Accepted
    if !is_accepted(&*obj) {
        update_gateway_class_status(&*obj)
            .await
            .map_err(MultiwayError::from)?;
        info!("Updated GatewayClass {} status to Accepted", obj.name_any());
    }

    Ok(Action::requeue(Duration::from_secs(3600)))
}

async fn update_gateway_class_status(
    gateway_class: &GatewayClass,
) -> Result<GatewayClass, kube::Error> {
    // Create a Kubernetes client to update the status
    let client = Client::try_default().await?;
    let api: Api<GatewayClass> = Api::all(client);

    let now = Time(Utc::now());
    let condition = Condition {
        type_: "Accepted".to_string(),
        status: "True".to_string(),
        observed_generation: gateway_class.metadata.generation,
        last_transition_time: now,
        reason: "Accepted".to_string(),
        message: "GatewayClass accepted by controller".to_string(),
    };

    let status = json!({
        "status": {
            "conditions": [condition]
        }
    });

    api.patch_status(
        &gateway_class.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&status),
    )
    .await
}

// Check if a gateway class has been accepted.
fn is_accepted(gateway_class: &GatewayClass) -> bool {
    gateway_class
        .status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .map(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Accepted" && condition.status == "True")
        })
        .unwrap_or(false)
}

// Not really sure what this does...
fn error_policy(_object: Arc<GatewayClass>, _err: &MultiwayError, _ctx: Arc<()>) -> Action {
    Action::requeue(Duration::from_secs(5))
}

/// This is a catch-all error type that encompasses any of the
/// structured errors we could encounter.
#[derive(thiserror::Error, Diagnostic, Debug)]
pub enum MultiwayError {
    // #[error("Could not create Kubernetes client: {0}")]
    #[error(transparent)]
    KubernetesError(#[from] kube::Error), // ClientCreationError(#[from] kube::Error),
                                          // #[error("Could not update GatewayClass: {0}")]
                                          // UpdateGatewayClassError(#[from] kube::Error),
}
