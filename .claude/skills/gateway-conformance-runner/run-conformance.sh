#!/usr/bin/env bash
#
# run-conformance.sh
#
# Runs the Gateway API conformance test suite against a DigitalOcean Kubernetes cluster.
# This script handles image building, deployment, and test execution.
#
# IMPORTANT: This script assumes the cluster is already running. Use cluster-up.sh
# to start the cluster before running this script, and cluster-down.sh to stop it
# when finished.
#
# Usage:
#   ./run-conformance.sh [OPTIONS]
#
# Options:
#   --skip-build      Skip the Rust compilation and Docker image build steps
#   --skip-deploy     Skip the gateway controller deployment step
#   --dry-run         Print commands without executing them
#   --help            Show this help message
#

set -euo pipefail

# =============================================================================
# CONFIGURATION
# =============================================================================

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

SKIP_BUILD=false
SKIP_DEPLOY=false
DRY_RUN=false

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

Runs the Gateway API conformance test suite against a DigitalOcean Kubernetes cluster.

IMPORTANT: The cluster must already be running. Use cluster-up.sh to start it first.

Options:
  --skip-build      Skip the Rust compilation and Docker image build steps
  --skip-deploy     Skip the gateway controller deployment step
  --dry-run         Print commands without executing them
  --help            Show this help message

Environment Variables:
  GATEWAY_CONFORMANCE_SUITE   Path to the Gateway API repository root (required)
  DOCKER_REGISTRY             Container registry URL (required for build, e.g., ghcr.io/myorg)

Examples:
  # Run full conformance test workflow
  ./run-conformance.sh

  # Skip building if images already exist
  ./run-conformance.sh --skip-build

  # Skip both build and deploy (just run tests)
  ./run-conformance.sh --skip-build --skip-deploy

  # See what commands would be run without executing them
  ./run-conformance.sh --dry-run
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
#######################################
parse_arguments() {
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
            --dry-run)
                DRY_RUN=true
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
# PREREQUISITE CHECKS
# =============================================================================

#######################################
# Verifies that the Docker daemon is running.
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
# Verifies that the Kubernetes cluster is accessible.
#######################################
verify_cluster_accessible() {
    info "Verifying cluster is accessible..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl get nodes"
        success "Cluster accessibility check skipped (dry-run mode)"
        return 0
    fi

    if ! kubectl get nodes &>/dev/null; then
        error_exit "Cannot access Kubernetes cluster.

Please ensure the cluster is running by executing:
    ./cluster-up.sh

If the cluster is running, check your kubectl context with:
    kubectl config current-context"
    fi

    info "Cluster nodes:"
    kubectl get nodes

    success "Cluster is accessible"
}

#######################################
# Runs all prerequisite checks.
#######################################
check_prerequisites() {
    info "=== Phase: Prerequisites ==="

    check_docker_running
    check_kubectl_available
    verify_cluster_accessible

    success "All prerequisites satisfied"
    echo ""
}

# =============================================================================
# ENVIRONMENT VERIFICATION
# =============================================================================

#######################################
# Verifies that the GATEWAY_CONFORMANCE_SUITE environment variable is set
# and points to a valid Gateway API repository with a conformance directory.
#######################################
verify_conformance_suite_env() {
    info "=== Phase: Environment Verification ==="

    if [[ -z "${GATEWAY_CONFORMANCE_SUITE:-}" ]]; then
        error_exit "GATEWAY_CONFORMANCE_SUITE environment variable is not set.

Please configure it in your .envrc.local file:
    export GATEWAY_CONFORMANCE_SUITE=/path/to/gateway-api

The path should point to the root of the Gateway API repository clone."
    fi

    info "GATEWAY_CONFORMANCE_SUITE is set to: ${GATEWAY_CONFORMANCE_SUITE}"

    if [[ ! -d "${GATEWAY_CONFORMANCE_SUITE}" ]]; then
        error_exit "GATEWAY_CONFORMANCE_SUITE path does not exist: ${GATEWAY_CONFORMANCE_SUITE}

Please clone the Gateway API repository:
    git clone https://github.com/kubernetes-sigs/gateway-api.git ${GATEWAY_CONFORMANCE_SUITE}"
    fi

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
# BUILD AND PUSH IMAGES
# =============================================================================

#######################################
# Verifies that the Rust project compiles successfully.
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
#######################################
build_docker_images() {
    info "Building Docker images..."

    if ! run_cmd cargo make docker-build-all; then
        error_exit "Docker image build failed. Please check the build output for errors."
    fi

    success "Docker images built successfully"
}

#######################################
# Pushes Docker images to the container registry.
#######################################
push_images_to_registry() {
    info "Pushing images to container registry..."

    if [[ -z "${DOCKER_REGISTRY:-}" ]]; then
        error_exit "DOCKER_REGISTRY environment variable is not set.
Please set it to your container registry URL.
Example: export DOCKER_REGISTRY=ghcr.io/myorg"
    fi

    if ! run_cmd cargo make do-push-images; then
        error_exit "Failed to push images to registry"
    fi

    success "Images pushed to registry"
}

#######################################
# Orchestrates the build and image push phase.
#######################################
build_and_push_images() {
    info "=== Phase: Build and Push Images ==="

    if [[ "${SKIP_BUILD}" == true ]]; then
        warn "Skipping build phase (--skip-build specified)"
        echo ""
        return 0
    fi

    verify_rust_compiles
    build_docker_images
    push_images_to_registry

    success "Build and push phase complete"
    echo ""
}

# =============================================================================
# DEPLOY GATEWAY COMPONENTS
# =============================================================================

#######################################
# Checks if the gateway namespace exists in the cluster.
#######################################
namespace_exists() {
    local readonly ns="$1"
    kubectl get namespace "${ns}" &>/dev/null
}

#######################################
# Cleans up any existing gateway deployments by deleting the namespace.
#######################################
cleanup_existing_deployment() {
    info "Cleaning up existing deployments in namespace '${DEFAULT_NAMESPACE}'..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl delete namespace ${DEFAULT_NAMESPACE} --ignore-not-found"
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl wait --for=delete namespace/${DEFAULT_NAMESPACE} --timeout=60s"
        success "Cleanup skipped (dry-run mode)"
        return 0
    fi

    if ! namespace_exists "${DEFAULT_NAMESPACE}"; then
        success "Namespace '${DEFAULT_NAMESPACE}' does not exist, nothing to clean up"
        return 0
    fi

    info "Deleting namespace '${DEFAULT_NAMESPACE}' and all its resources..."
    if ! kubectl delete namespace "${DEFAULT_NAMESPACE}" --ignore-not-found; then
        error_exit "Failed to delete namespace '${DEFAULT_NAMESPACE}'"
    fi

    info "Waiting for namespace deletion to complete..."
    if ! kubectl wait --for=delete namespace/"${DEFAULT_NAMESPACE}" --timeout=60s 2>/dev/null; then
        if namespace_exists "${DEFAULT_NAMESPACE}"; then
            error_exit "Namespace '${DEFAULT_NAMESPACE}' was not deleted within timeout"
        fi
    fi

    success "Existing deployments cleaned up"
}

#######################################
# Creates a fresh namespace for the gateway components.
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
#######################################
deploy_gateway_controller() {
    info "Deploying gateway controller..."

    if ! run_cmd cargo make deploy; then
        error_exit "Failed to deploy gateway controller"
    fi

    success "Gateway controller deployed"
}

#######################################
# Waits for the gateway controller pods to become ready.
#######################################
wait_for_controller_ready() {
    info "Waiting for controller pods to be ready..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl wait --for=condition=Ready pods -l ${DEFAULT_POD_LABEL} -n ${DEFAULT_NAMESPACE} --timeout=${DEFAULT_POD_READY_TIMEOUT}"
        success "Pod readiness check skipped (dry-run mode)"
        return 0
    fi

    if kubectl wait --for=condition=Ready pods -l "${DEFAULT_POD_LABEL}" -n "${DEFAULT_NAMESPACE}" --timeout="${DEFAULT_POD_READY_TIMEOUT}" 2>/dev/null; then
        success "Controller pods are ready"
        return 0
    fi

    warn "Pods not ready within ${DEFAULT_POD_READY_TIMEOUT}, extending wait to ${DEFAULT_EXTENDED_POD_READY_TIMEOUT}..."

    if kubectl wait --for=condition=Ready pods -l "${DEFAULT_POD_LABEL}" -n "${DEFAULT_NAMESPACE}" --timeout="${DEFAULT_EXTENDED_POD_READY_TIMEOUT}" 2>/dev/null; then
        success "Controller pods are ready (after extended wait)"
        return 0
    fi

    warn "Pods still not ready. Current pod status:"
    kubectl get pods -n "${DEFAULT_NAMESPACE}" -l "${DEFAULT_POD_LABEL}"

    error_exit "Controller pods failed to become ready within ${DEFAULT_EXTENDED_POD_READY_TIMEOUT}.
Please check the pod logs for errors:
    kubectl logs -n ${DEFAULT_NAMESPACE} -l ${DEFAULT_POD_LABEL}"
}

#######################################
# Orchestrates the gateway component deployment phase.
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

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} cd ${GATEWAY_CONFORMANCE_SUITE} && make conformance"
        success "Conformance test run skipped (dry-run mode)"
        return 0
    fi

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

main() {
    parse_arguments "$@"

    echo ""
    info "==========================================="
    info "Gateway API Conformance Test Runner"
    info "==========================================="
    echo ""
    info "Configuration:"
    info "  Skip build:   ${SKIP_BUILD}"
    info "  Skip deploy:  ${SKIP_DEPLOY}"
    info "  Dry run:      ${DRY_RUN}"
    echo ""

    check_prerequisites
    verify_conformance_suite_env
    build_and_push_images
    deploy_gateway_components
    run_conformance_tests

    echo ""
    success "==========================================="
    success "Conformance test workflow complete"
    success "==========================================="
}

main "$@"
