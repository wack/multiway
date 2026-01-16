#!/usr/bin/env bash
#
# run-conformance-local.sh
#
# Runs the Gateway API conformance test suite locally against a Kind cluster.
# This script handles environment setup, cluster creation, image building,
# deployment, and test execution with automatic recovery where possible.
#
# Usage:
#   ./scripts/run-conformance-local.sh [OPTIONS]
#
# Options:
#   --skip-build      Skip the Rust compilation and Docker image build steps
#   --skip-deploy     Skip the gateway controller deployment step
#   --cluster-name    Name of the Kind cluster (default: multiway-local)
#   --dry-run         Print commands without executing them
#   --help            Show this help message
#

set -euo pipefail

# =============================================================================
# CONFIGURATION
# =============================================================================

# Default configuration values (can be overridden via command-line arguments)
readonly DEFAULT_CLUSTER_NAME="multiway-local"
readonly DEFAULT_NAMESPACE="multiway-system"
readonly DEFAULT_POD_LABEL="app.kubernetes.io/name=multiway"
readonly DEFAULT_POD_READY_TIMEOUT="120s"
readonly DEFAULT_EXTENDED_POD_READY_TIMEOUT="300s"

# Color codes for output formatting
readonly COLOR_RED='\033[0;31m'
readonly COLOR_GREEN='\033[0;32m'
readonly COLOR_YELLOW='\033[0;33m'
readonly COLOR_BLUE='\033[0;34m'
readonly COLOR_RESET='\033[0m'

# =============================================================================
# GLOBAL STATE
# =============================================================================

# These variables are set by parse_arguments() and used throughout the script
CLUSTER_NAME=""
SKIP_BUILD=false
SKIP_DEPLOY=false
DRY_RUN=false
USE_DEV_BUILD=false

# =============================================================================
# UTILITY FUNCTIONS
# =============================================================================

#######################################
# Prints an informational message in blue.
# Arguments:
#   $1 - The message to print
#######################################
info() {
    local readonly message="$1"
    echo -e "${COLOR_BLUE}[INFO]${COLOR_RESET} ${message}"
}

#######################################
# Prints a success message in green.
# Arguments:
#   $1 - The message to print
#######################################
success() {
    local readonly message="$1"
    echo -e "${COLOR_GREEN}[OK]${COLOR_RESET} ${message}"
}

#######################################
# Prints a warning message in yellow.
# Arguments:
#   $1 - The message to print
#######################################
warn() {
    local readonly message="$1"
    echo -e "${COLOR_YELLOW}[WARN]${COLOR_RESET} ${message}"
}

#######################################
# Prints an error message in red and exits with code 1.
# Arguments:
#   $1 - The error message to print
#######################################
error_exit() {
    local readonly message="$1"
    echo -e "${COLOR_RED}[ERROR]${COLOR_RESET} ${message}" >&2
    exit 1
}

#######################################
# Executes a command, or prints it if in dry-run mode.
# Arguments:
#   $@ - The command and its arguments to execute
# Returns:
#   The exit code of the command (0 in dry-run mode)
#######################################
run_cmd() {
    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} $*"
        return 0
    else
        "$@"
    fi
}

#######################################
# Prints the help message and exits.
#######################################
show_help() {
    cat << EOF
Usage: $(basename "$0") [OPTIONS]

Runs the Gateway API conformance test suite locally against a Kind cluster.

Options:
  --skip-build      Skip the Rust compilation and Docker image build steps
  --skip-deploy     Skip the gateway controller deployment step
  --cluster-name    Name of the Kind cluster (default: ${DEFAULT_CLUSTER_NAME})
  --dev             Use development (debug) builds for faster compilation
  --dry-run         Print commands without executing them
  --help            Show this help message

Environment Variables:
  GATEWAY_CONFORMANCE_SUITE   Path to the Gateway API repository root (required)

Examples:
  # Run full conformance test workflow
  ./scripts/run-conformance-local.sh

  # Run with faster development builds (debug mode)
  ./scripts/run-conformance-local.sh --dev

  # Skip building if images already exist
  ./scripts/run-conformance-local.sh --skip-build

  # Skip both build and deploy (just run tests)
  ./scripts/run-conformance-local.sh --skip-build --skip-deploy

  # Use a different cluster name
  ./scripts/run-conformance-local.sh --cluster-name my-test-cluster

  # See what commands would be run without executing them
  ./scripts/run-conformance-local.sh --dry-run
EOF
    exit 0
}

# =============================================================================
# ARGUMENT PARSING
# =============================================================================

#######################################
# Parses command-line arguments and sets global configuration variables.
# Arguments:
#   $@ - All command-line arguments passed to the script
# Globals:
#   CLUSTER_NAME - Set to the specified or default cluster name
#   SKIP_BUILD   - Set to true if --skip-build is provided
#   SKIP_DEPLOY  - Set to true if --skip-deploy is provided
#   DRY_RUN      - Set to true if --dry-run is provided
#######################################
parse_arguments() {
    CLUSTER_NAME="${DEFAULT_CLUSTER_NAME}"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --skip-build)
                SKIP_BUILD=true
                shift
                ;;
            --skip-deploy)
                SKIP_DEPLOY=true
                shift
                ;;
            --cluster-name)
                if [[ -z "${2:-}" ]]; then
                    error_exit "--cluster-name requires a value"
                fi
                CLUSTER_NAME="$2"
                shift 2
                ;;
            --dry-run)
                DRY_RUN=true
                shift
                ;;
            --dev)
                USE_DEV_BUILD=true
                shift
                ;;
            --help|-h)
                show_help
                ;;
            *)
                error_exit "Unknown option: $1. Use --help for usage information."
                ;;
        esac
    done
}

# =============================================================================
# PREREQUISITE CHECKS (Non-recoverable)
# =============================================================================

#######################################
# Verifies that the Docker daemon is running.
# This is a non-recoverable check - if Docker is not running, the script exits.
#######################################
check_docker_running() {
    info "Checking if Docker is running..."

    if ! docker info &>/dev/null; then
        error_exit "Docker is not running. Please start the Docker daemon and try again."
    fi

    success "Docker is running"
}

#######################################
# Verifies that kubectl is installed and available in PATH.
# This is a non-recoverable check - kubectl must be installed manually.
#######################################
check_kubectl_available() {
    info "Checking if kubectl is available..."

    if ! command -v kubectl &>/dev/null; then
        error_exit "kubectl is not installed. Please install kubectl and try again.
See: https://kubernetes.io/docs/tasks/tools/install-kubectl/"
    fi

    success "kubectl is available"
}

#######################################
# Verifies that Kind is installed and available in PATH.
# This is a non-recoverable check - Kind must be installed manually.
#######################################
check_kind_available() {
    info "Checking if Kind is available..."

    if ! command -v kind &>/dev/null; then
        error_exit "Kind is not installed. Please install Kind and try again.
See: https://kind.sigs.k8s.io/docs/user/quick-start/#installation"
    fi

    success "Kind is available"
}

#######################################
# Runs all prerequisite checks that are non-recoverable.
# If any check fails, the script will exit with an error message.
#######################################
check_prerequisites() {
    info "=== Phase: Prerequisites ==="

    check_docker_running
    check_kubectl_available
    check_kind_available

    success "All prerequisites satisfied"
    echo ""
}

# =============================================================================
# ENVIRONMENT VERIFICATION (Non-recoverable)
# =============================================================================

#######################################
# Verifies that the GATEWAY_CONFORMANCE_SUITE environment variable is set
# and points to a valid Gateway API repository with a conformance directory.
# This is non-recoverable - the user must configure this manually.
#######################################
verify_conformance_suite_env() {
    info "=== Phase: Environment Verification ==="

    # Check if the environment variable is set
    if [[ -z "${GATEWAY_CONFORMANCE_SUITE:-}" ]]; then
        error_exit "GATEWAY_CONFORMANCE_SUITE environment variable is not set.

Please configure it in your .envrc.local file:
    export GATEWAY_CONFORMANCE_SUITE=/path/to/gateway-api

The path should point to the root of the Gateway API repository clone."
    fi

    info "GATEWAY_CONFORMANCE_SUITE is set to: ${GATEWAY_CONFORMANCE_SUITE}"

    # Verify the path exists
    if [[ ! -d "${GATEWAY_CONFORMANCE_SUITE}" ]]; then
        error_exit "GATEWAY_CONFORMANCE_SUITE path does not exist: ${GATEWAY_CONFORMANCE_SUITE}

Please clone the Gateway API repository:
    git clone https://github.com/wack/gateway-api.git ${GATEWAY_CONFORMANCE_SUITE}"
    fi

    # Verify the conformance directory exists within the repository
    local readonly conformance_dir="${GATEWAY_CONFORMANCE_SUITE}/conformance"
    if [[ ! -d "${conformance_dir}" ]]; then
        error_exit "Conformance directory not found at: ${conformance_dir}

The GATEWAY_CONFORMANCE_SUITE should point to the root of the Gateway API repository,
not the conformance subdirectory. The repository should contain a 'conformance/' directory."
    fi

    success "Environment verified: conformance suite found at ${conformance_dir}"
    echo ""
}

# =============================================================================
# KUBERNETES CLUSTER MANAGEMENT (Recoverable)
# =============================================================================

#######################################
# Checks if the specified Kind cluster exists.
# Returns:
#   0 if the cluster exists, 1 otherwise
#######################################
kind_cluster_exists() {
    local readonly cluster_name="$1"
    kind get clusters 2>/dev/null | grep -q "^${cluster_name}$"
}

#######################################
# Creates a new Kind cluster if it doesn't exist.
# This is a recoverable operation - if the cluster is missing, we create it.
# Arguments:
#   Uses global CLUSTER_NAME variable
#######################################
ensure_kind_cluster_exists() {
    info "Checking if Kind cluster '${CLUSTER_NAME}' exists..."

    if kind_cluster_exists "${CLUSTER_NAME}"; then
        success "Kind cluster '${CLUSTER_NAME}' already exists"
    else
        warn "Kind cluster '${CLUSTER_NAME}' does not exist, creating it..."
        run_cmd cargo make kind-create

        if [[ "${DRY_RUN}" != true ]] && ! kind_cluster_exists "${CLUSTER_NAME}"; then
            error_exit "Failed to create Kind cluster '${CLUSTER_NAME}'"
        fi

        success "Kind cluster '${CLUSTER_NAME}' created successfully"
    fi
}

#######################################
# Ensures kubectl is configured to use the correct Kind cluster context.
# This is a recoverable operation - we switch context if needed.
# Arguments:
#   Uses global CLUSTER_NAME variable
#######################################
ensure_correct_kubectl_context() {
    local readonly expected_context="kind-${CLUSTER_NAME}"

    info "Checking kubectl context..."

    local current_context
    current_context=$(kubectl config current-context 2>/dev/null || echo "")

    if [[ "${current_context}" == "${expected_context}" ]]; then
        success "kubectl context is already set to '${expected_context}'"
    else
        if [[ -n "${current_context}" ]]; then
            warn "kubectl context is '${current_context}', switching to '${expected_context}'..."
        else
            warn "No kubectl context set, switching to '${expected_context}'..."
        fi

        run_cmd cargo make kind-use
        success "Switched kubectl context to '${expected_context}'"
    fi
}

#######################################
# Verifies that the Kubernetes cluster is accessible and ready.
# This validates that we can communicate with the cluster after context setup.
#######################################
verify_cluster_accessible() {
    info "Verifying cluster is accessible..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl get nodes"
        success "Cluster accessibility check skipped (dry-run mode)"
        return 0
    fi

    if ! kubectl get nodes &>/dev/null; then
        error_exit "Cannot access Kubernetes cluster. Please check your cluster status."
    fi

    # Display node status for confirmation
    info "Cluster nodes:"
    kubectl get nodes

    success "Cluster is accessible"
}

#######################################
# Orchestrates all Kubernetes cluster setup steps with recovery logic.
# Creates the cluster if missing, sets the correct context, and verifies access.
#######################################
setup_kubernetes_cluster() {
    info "=== Phase: Kubernetes Cluster Setup ==="

    ensure_kind_cluster_exists
    ensure_correct_kubectl_context
    verify_cluster_accessible

    success "Kubernetes cluster is ready"
    echo ""
}

# =============================================================================
# BUILD AND LOAD IMAGES
# =============================================================================

#######################################
# Verifies that the Rust project compiles successfully.
# This is a non-recoverable check - compilation errors require code fixes.
#######################################
verify_rust_compiles() {
    info "Verifying Rust project compiles..."

    if ! run_cmd cargo check; then
        error_exit "Rust project failed to compile. Please fix the compilation errors and try again."
    fi

    success "Rust project compiles successfully"
}

#######################################
# Builds the Docker images for control plane and data plane.
# This is a non-recoverable operation - build failures require investigation.
# Uses debug builds when --dev flag is set for faster compilation.
#######################################
build_docker_images() {
    info "Building Docker images..."

    local build_task="docker-build-all"
    if [[ "${USE_DEV_BUILD}" == true ]]; then
        build_task="docker-build-all-dev"
        info "Using development (debug) build for faster compilation"
    fi

    if ! run_cmd cargo make "${build_task}"; then
        error_exit "Docker image build failed. Please check the build output for errors."
    fi

    success "Docker images built successfully"
}

#######################################
# Loads Docker images into the Kind cluster.
# This is an idempotent operation - loading images that already exist is safe.
# Arguments:
#   Uses global CLUSTER_NAME variable
#######################################
load_images_into_kind() {
    info "Loading images into Kind cluster..."

    # Load control plane image
    info "Loading multiway-controlplane:latest..."
    if ! run_cmd kind load docker-image multiway-controlplane:latest --name "${CLUSTER_NAME}"; then
        error_exit "Failed to load control plane image into Kind cluster"
    fi

    # Load data plane image
    info "Loading multiway-dataplane:latest..."
    if ! run_cmd kind load docker-image multiway-dataplane:latest --name "${CLUSTER_NAME}"; then
        error_exit "Failed to load data plane image into Kind cluster"
    fi

    success "Images loaded into Kind cluster"
}

#######################################
# Orchestrates the build and image loading phase.
# Can be skipped with --skip-build flag for faster iteration.
#######################################
build_and_load_images() {
    info "=== Phase: Build and Load Images ==="

    if [[ "${SKIP_BUILD}" == true ]]; then
        warn "Skipping build phase (--skip-build specified)"
        echo ""
        return 0
    fi

    verify_rust_compiles
    build_docker_images
    load_images_into_kind

    success "Build and load phase complete"
    echo ""
}

# =============================================================================
# DEPLOY GATEWAY COMPONENTS (Recoverable)
# =============================================================================

#######################################
# Checks if the gateway namespace exists in the cluster.
# Returns:
#   0 if the namespace exists, 1 otherwise
#######################################
namespace_exists() {
    local readonly ns="$1"
    kubectl get namespace "${ns}" &>/dev/null
}

#######################################
# Cleans up any existing gateway deployments by deleting the namespace.
# This ensures a fresh state before deploying new components.
# The function is idempotent - it safely handles the case where the
# namespace doesn't exist.
#######################################
cleanup_existing_deployment() {
    info "Cleaning up existing deployments in namespace '${DEFAULT_NAMESPACE}'..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl delete namespace ${DEFAULT_NAMESPACE} --ignore-not-found"
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl wait --for=delete namespace/${DEFAULT_NAMESPACE} --timeout=60s"
        success "Cleanup skipped (dry-run mode)"
        return 0
    fi

    # Check if namespace exists before attempting deletion
    if ! namespace_exists "${DEFAULT_NAMESPACE}"; then
        success "Namespace '${DEFAULT_NAMESPACE}' does not exist, nothing to clean up"
        return 0
    fi

    # Delete the namespace (this removes all resources within it)
    info "Deleting namespace '${DEFAULT_NAMESPACE}' and all its resources..."
    if ! kubectl delete namespace "${DEFAULT_NAMESPACE}" --ignore-not-found; then
        error_exit "Failed to delete namespace '${DEFAULT_NAMESPACE}'"
    fi

    # Wait for the namespace to be fully deleted
    # This is important because Kubernetes namespace deletion is asynchronous
    info "Waiting for namespace deletion to complete..."
    if ! kubectl wait --for=delete namespace/"${DEFAULT_NAMESPACE}" --timeout=60s 2>/dev/null; then
        # The wait command may fail if the namespace is already gone, which is fine
        if namespace_exists "${DEFAULT_NAMESPACE}"; then
            error_exit "Namespace '${DEFAULT_NAMESPACE}' was not deleted within timeout"
        fi
    fi

    success "Existing deployments cleaned up"
}

#######################################
# Creates a fresh namespace for the gateway components.
# This should be called after cleanup_existing_deployment.
#######################################
create_fresh_namespace() {
    info "Creating fresh namespace '${DEFAULT_NAMESPACE}'..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl create namespace ${DEFAULT_NAMESPACE}"
        success "Namespace creation skipped (dry-run mode)"
        return 0
    fi

    if ! kubectl create namespace "${DEFAULT_NAMESPACE}"; then
        error_exit "Failed to create namespace '${DEFAULT_NAMESPACE}'"
    fi

    success "Namespace '${DEFAULT_NAMESPACE}' created"
}

#######################################
# Installs or updates the Gateway API CRDs in the cluster.
# This is an idempotent operation - running it multiple times is safe.
#######################################
install_gateway_api_crds() {
    info "Installing Gateway API CRDs..."

    if ! run_cmd cargo make gateway-api-install; then
        error_exit "Failed to install Gateway API CRDs"
    fi

    success "Gateway API CRDs installed"
}

#######################################
# Deploys the gateway controller to the cluster.
# Assumes the namespace has already been created by create_fresh_namespace().
#######################################
deploy_gateway_controller() {
    info "Deploying gateway controller..."

    # Deploy the controller using cargo make
    if ! run_cmd cargo make deploy; then
        error_exit "Failed to deploy gateway controller"
    fi

    success "Gateway controller deployed"
}

#######################################
# Waits for the gateway controller pods to become ready.
# Uses an initial timeout, with recovery logic to wait longer if needed.
# Arguments:
#   Uses global DEFAULT_NAMESPACE, DEFAULT_POD_LABEL, and timeout constants
#######################################
wait_for_controller_ready() {
    info "Waiting for controller pods to be ready..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl wait --for=condition=Ready pods -l ${DEFAULT_POD_LABEL} -n ${DEFAULT_NAMESPACE} --timeout=${DEFAULT_POD_READY_TIMEOUT}"
        success "Pod readiness check skipped (dry-run mode)"
        return 0
    fi

    # First attempt with standard timeout
    if kubectl wait --for=condition=Ready pods -l "${DEFAULT_POD_LABEL}" -n "${DEFAULT_NAMESPACE}" --timeout="${DEFAULT_POD_READY_TIMEOUT}" 2>/dev/null; then
        success "Controller pods are ready"
        return 0
    fi

    # Recovery: try with extended timeout
    warn "Pods not ready within ${DEFAULT_POD_READY_TIMEOUT}, extending wait to ${DEFAULT_EXTENDED_POD_READY_TIMEOUT}..."

    if kubectl wait --for=condition=Ready pods -l "${DEFAULT_POD_LABEL}" -n "${DEFAULT_NAMESPACE}" --timeout="${DEFAULT_EXTENDED_POD_READY_TIMEOUT}" 2>/dev/null; then
        success "Controller pods are ready (after extended wait)"
        return 0
    fi

    # Show pod status for debugging
    warn "Pods still not ready. Current pod status:"
    kubectl get pods -n "${DEFAULT_NAMESPACE}" -l "${DEFAULT_POD_LABEL}"

    error_exit "Controller pods failed to become ready within ${DEFAULT_EXTENDED_POD_READY_TIMEOUT}.
Please check the pod logs for errors:
    kubectl logs -n ${DEFAULT_NAMESPACE} -l ${DEFAULT_POD_LABEL}"
}

#######################################
# Orchestrates the gateway component deployment phase.
# Can be skipped with --skip-deploy flag for faster iteration.
#
# Steps:
#   1. Clean up any existing deployments (delete namespace)
#   2. Install Gateway API CRDs (cluster-scoped, not affected by namespace deletion)
#   3. Create fresh namespace
#   4. Deploy gateway controller
#   5. Wait for controller pods to be ready
#######################################
deploy_gateway_components() {
    info "=== Phase: Deploy Gateway Components ==="

    if [[ "${SKIP_DEPLOY}" == true ]]; then
        warn "Skipping deploy phase (--skip-deploy specified)"
        echo ""
        return 0
    fi

    cleanup_existing_deployment
    install_gateway_api_crds
    create_fresh_namespace
    deploy_gateway_controller
    wait_for_controller_ready

    success "Gateway components deployed and ready"
    echo ""
}

# =============================================================================
# RUN CONFORMANCE TESTS
# =============================================================================

#######################################
# Runs the Gateway API conformance test suite from the local repository.
# Test failures are expected output and do not cause the script to fail.
#######################################
run_conformance_tests() {
    info "=== Phase: Run Conformance Tests ==="

    info "Running conformance tests from: ${GATEWAY_CONFORMANCE_SUITE}"

    # Change to the conformance suite directory and run tests
    # Note: We use a subshell to avoid changing the script's working directory
    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} cd ${GATEWAY_CONFORMANCE_SUITE} && make conformance"
        success "Conformance test run skipped (dry-run mode)"
        return 0
    fi

    # Run the conformance tests
    # We use 'set +e' temporarily because test failures should not cause script exit
    set +e
    (
        cd "${GATEWAY_CONFORMANCE_SUITE}" && make conformance
    )
    local readonly test_exit_code=$?
    set -e

    echo ""
    if [[ ${test_exit_code} -eq 0 ]]; then
        success "Conformance tests completed successfully"
    else
        warn "Conformance tests completed with failures (exit code: ${test_exit_code})"
        info "Review the test output above for details on failed tests"
    fi

    return ${test_exit_code}
}

# =============================================================================
# MAIN EXECUTION
# =============================================================================

#######################################
# Main entry point for the script.
# Orchestrates all phases of the conformance test workflow.
# Arguments:
#   $@ - All command-line arguments
#######################################
main() {
    # Parse command-line arguments
    parse_arguments "$@"

    echo ""
    info "==========================================="
    info "Gateway API Local Conformance Test Runner"
    info "==========================================="
    echo ""
    info "Configuration:"
    info "  Cluster name: ${CLUSTER_NAME}"
    info "  Skip build:   ${SKIP_BUILD}"
    info "  Skip deploy:  ${SKIP_DEPLOY}"
    info "  Dev build:    ${USE_DEV_BUILD}"
    info "  Dry run:      ${DRY_RUN}"
    echo ""

    # Run all phases in order
    check_prerequisites
    verify_conformance_suite_env
    setup_kubernetes_cluster
    build_and_load_images
    deploy_gateway_components
    run_conformance_tests

    echo ""
    success "==========================================="
    success "Conformance test workflow complete"
    success "==========================================="
}

# Run main function with all script arguments
main "$@"
