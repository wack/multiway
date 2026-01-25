#!/usr/bin/env bash
# shellcheck disable=SC1091  # lib.sh is sourced at runtime from SCRIPT_DIR
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
# The cluster name defaults to a sanitized version of the current git branch,
# prefixed with "mw-" (e.g., branch "feature/my-test" becomes "mw-feature-my-test").
#
# Usage:
#   ./run-conformance.sh [OPTIONS]
#
# Options:
#   --release           Use production Dockerfile (higher optimization, slower builds)
#   --skip-build        Skip the Rust compilation and Docker image build steps
#   --skip-deploy       Skip the gateway controller deployment step
#   --cluster-name NAME Name of the cluster (default: derived from git branch)
#   --dry-run           Print commands without executing them
#   --help              Show this help message
#

set -euo pipefail

# =============================================================================
# LOAD SHARED LIBRARY
# =============================================================================

# Source the shared library of functions. This provides logging functions,
# command execution utilities, cluster name helpers, and CLI tool checks.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/lib.sh"

# =============================================================================
# CONFIGURATION
# =============================================================================

# Script-specific constants for gateway deployment and pod management.
readonly DEFAULT_NAMESPACE="multiway-system"
readonly DEFAULT_POD_LABEL="app.kubernetes.io/name=multiway"
readonly DEFAULT_POD_READY_TIMEOUT="120s"
readonly DEFAULT_EXTENDED_POD_READY_TIMEOUT="300s"

# =============================================================================
# GLOBAL STATE
# =============================================================================

# These variables are set by parse_arguments() and used throughout the script.
# They control which phases of the workflow are executed and how the script
# identifies the target cluster.
CLUSTER_NAME=""
DEV_BUILD=true
SKIP_BUILD=false
SKIP_DEPLOY=false
DRY_RUN=false

# =============================================================================
# SCRIPT-SPECIFIC FUNCTIONS
# =============================================================================

#######################################
# Prints the help message and exits.
#######################################
show_help() {
    local default_name
    default_name=$(get_default_cluster_name)

    cat << EOF
Usage: $(basename "$0") [OPTIONS]

Runs the Gateway API conformance test suite against a DigitalOcean Kubernetes cluster.

IMPORTANT: The cluster must already be running. Use cluster-up.sh to start it first.

The cluster name defaults to a sanitized version of the current git branch,
prefixed with "${CLUSTER_NAME_PREFIX}-" (e.g., "feature/my-test" becomes "${CLUSTER_NAME_PREFIX}-feature-my-test").

Options:
  --release           Use production Dockerfile (higher optimization, slower builds)
  --skip-build        Skip the Rust compilation and Docker image build steps
  --skip-deploy       Skip the gateway controller deployment step
  --cluster-name NAME Name of the cluster (default: ${default_name})
  --dry-run           Print commands without executing them
  --help              Show this help message

Environment Variables:
  GATEWAY_CONFORMANCE_SUITE   Path to the Gateway API repository root (required)
  DOCKER_REGISTRY             Container registry URL (required for build, e.g., ghcr.io/myorg)
  DO_CLUSTER_NAME             Override the cluster name

Examples:
  # Run full conformance test workflow
  ./run-conformance.sh

  # Skip building if images already exist
  ./run-conformance.sh --skip-build

  # Skip both build and deploy (just run tests)
  ./run-conformance.sh --skip-build --skip-deploy

  # Use a specific cluster name
  ./run-conformance.sh --cluster-name my-test-cluster

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
# Globals:
#   CLUSTER_NAME - Set to the specified, environment, or branch-derived cluster name
#   SKIP_BUILD   - Set to true if --skip-build is provided
#   SKIP_DEPLOY  - Set to true if --skip-deploy is provided
#   DRY_RUN      - Set to true if --dry-run is provided
#######################################
parse_arguments() {
    # Default to environment variable, then git branch-based name
    if [[ -n "${DO_CLUSTER_NAME:-}" ]]; then
        CLUSTER_NAME="${DO_CLUSTER_NAME}"
    else
        CLUSTER_NAME=$(get_default_cluster_name)
    fi

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --release)
                DEV_BUILD=false
                shift
                ;;
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
#
# These checks verify that the local development environment is properly
# configured. Failures here require manual intervention - the script cannot
# automatically recover from missing tools or a stopped Docker daemon.
#
# Note: check_docker_running and check_kubectl_available are provided by lib.sh
# =============================================================================

#######################################
# Verifies that the Kubernetes cluster is accessible.
# This check confirms we can communicate with the cluster after the context
# has been set up by cluster-up.sh. Unlike Kind clusters which are local,
# DigitalOcean clusters require network connectivity and valid credentials.
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
# Runs all prerequisite checks that are non-recoverable.
# If any check fails, the script will exit with an error message explaining
# what needs to be fixed before the script can proceed.
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
# ENVIRONMENT VERIFICATION (Non-recoverable)
#
# These checks verify that required environment variables are set and point
# to valid locations. The user must configure these manually before running
# the conformance tests.
# =============================================================================

#######################################
# Verifies that the GATEWAY_CONFORMANCE_SUITE environment variable is set
# and points to a valid Gateway API repository with a conformance directory.
# This is non-recoverable - the user must configure this manually by cloning
# the Gateway API repository and setting the environment variable.
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
    git clone https://github.com/kubernetes-sigs/gateway-api.git ${GATEWAY_CONFORMANCE_SUITE}"
    fi

    # Verify the conformance directory exists within the repository
    local -r conformance_dir="${GATEWAY_CONFORMANCE_SUITE}/conformance"
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
#
# This phase compiles the Rust code, builds Docker images, and pushes them
# to the container registry. The actual build logic is in build-docker.sh,
# which can also be run standalone.
# =============================================================================

#######################################
# Orchestrates the build and image push phase by calling build-docker.sh.
# Can be skipped with --skip-build flag for faster iteration when images
# have already been built and pushed (e.g., when only re-running tests).
#######################################
build_and_push_images() {
    if [[ "${SKIP_BUILD}" == true ]]; then
        info "=== Phase: Build and Push Images ==="
        warn "Skipping build phase (--skip-build specified)"
        echo ""
        return 0
    fi

    # Build arguments for build-docker.sh
    local -a build_args=()

    if [[ "${DEV_BUILD}" == false ]]; then
        build_args+=("--release")
    fi

    if [[ "${DRY_RUN}" == true ]]; then
        build_args+=("--dry-run")
    fi

    # Call the build script
    # Note: ${array[@]+...} syntax handles empty arrays with set -u
    "${SCRIPT_DIR}/build-docker.sh" ${build_args[@]+"${build_args[@]}"}
}

# =============================================================================
# DEPLOY GATEWAY COMPONENTS (Recoverable)
#
# This phase deploys the gateway controller to the Kubernetes cluster. Most
# operations here are recoverable - if something exists from a previous run,
# we clean it up and start fresh. This ensures a clean state for each test run.
# =============================================================================

#######################################
# Checks if the gateway namespace exists in the cluster.
# Used to determine if cleanup is needed before deployment.
#######################################
namespace_exists() {
    local -r ns="$1"
    kubectl get namespace "${ns}" &>/dev/null
}

#######################################
# Cleans up any existing gateway deployments by deleting the namespace.
# This is a recoverable operation - we delete the namespace to ensure a
# clean slate for the new deployment. Deleting the namespace removes all
# resources within it (deployments, services, configmaps, etc.).
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
# The namespace isolates the gateway resources from other workloads in the
# cluster and makes cleanup straightforward (delete the namespace).
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
# CRDs (Custom Resource Definitions) must be installed before deploying
# resources that use them. This is idempotent - running it multiple times
# is safe and will update CRDs if the definitions have changed.
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
# This applies the Kubernetes manifests that define the controller deployment,
# RBAC permissions, and related resources.
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
# Pods may take time to start due to image pulling, resource allocation,
# or initialization logic. We wait with an initial timeout and extend it
# if needed, providing diagnostic information if pods fail to start.
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
# Can be skipped with --skip-deploy flag when the controller is already
# deployed and you only want to re-run the conformance tests.
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
#
# This phase runs the official Gateway API conformance test suite against
# the deployed gateway controller. Test failures are expected during
# development and are reported as warnings rather than causing the script
# to exit with an error.
# =============================================================================

#######################################
# Runs the Gateway API conformance test suite from the local repository.
# Test failures are expected output and do not cause the script to fail.
# The conformance tests are run from the Gateway API repository clone,
# which must be configured via the GATEWAY_CONFORMANCE_SUITE environment
# variable. We capture the exit code to report pass/fail status but allow
# the script to complete even if tests fail.
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
    local -r test_exit_code=$?
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
    info "  Cluster name: ${CLUSTER_NAME}"
    info "  Dev build:    ${DEV_BUILD}"
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
