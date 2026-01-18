#!/usr/bin/env bash
#
# cluster-up.sh
#
# Ensures the DigitalOcean Kubernetes cluster is running and ready for use.
# If the cluster exists, it clears the gateway namespace for a fresh state.
# If the cluster doesn't exist, it creates one.
# Either way, it ensures the kubectl context is properly configured.
#
# The cluster name defaults to a sanitized version of the current git branch,
# prefixed with "mw-" (e.g., branch "feature/my-test" becomes "mw-feature-my-test").
#
# Usage:
#   ./cluster-up.sh [OPTIONS]
#
# Options:
#   --cluster-name NAME   Name of the DigitalOcean cluster (default: derived from git branch)
#   --dry-run             Print commands without executing them
#   --help                Show this help message
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

# Script-specific constants.
readonly DEFAULT_NAMESPACE="multiway-system"

# =============================================================================
# GLOBAL STATE
# =============================================================================

# These variables are set by parse_arguments() and used throughout the script.
CLUSTER_NAME=""
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

Ensures the DigitalOcean Kubernetes cluster is running and ready for use.
If the cluster exists, clears the gateway namespace for a fresh state.
If the cluster doesn't exist, creates a new one.

The cluster name defaults to a sanitized version of the current git branch,
prefixed with "${CLUSTER_NAME_PREFIX}-" (e.g., "feature/my-test" becomes "${CLUSTER_NAME_PREFIX}-feature-my-test").

Options:
  --cluster-name NAME   Name of the DigitalOcean cluster (default: ${default_name})
  --dry-run             Print commands without executing them
  --help                Show this help message

Environment Variables:
  DO_CLUSTER_NAME       Override the cluster name
  DO_REGION             DigitalOcean region for cluster (default: nyc3)

Examples:
  # Start or prepare cluster for current branch
  ./cluster-up.sh

  # Use a specific cluster name
  ./cluster-up.sh --cluster-name my-test-cluster

  # See what commands would be run
  ./cluster-up.sh --dry-run
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
    # Default to environment variable, then git branch-based name
    if [[ -n "${DO_CLUSTER_NAME:-}" ]]; then
        CLUSTER_NAME="${DO_CLUSTER_NAME}"
    else
        CLUSTER_NAME=$(get_default_cluster_name)
    fi

    while [[ $# -gt 0 ]]; do
        case "$1" in
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
# PREREQUISITE CHECKS
#
# Note: check_doctl_available and check_kubectl_available are provided by lib.sh
# =============================================================================

#######################################
# Runs all prerequisite checks.
#######################################
check_prerequisites() {
    info "=== Checking Prerequisites ==="
    check_doctl_available
    check_kubectl_available
    success "All prerequisites satisfied"
    echo ""
}

# =============================================================================
# CLUSTER MANAGEMENT
#
# Note: do_cluster_exists is provided by lib.sh
# =============================================================================

#######################################
# Creates a new DigitalOcean Kubernetes cluster.
# Uses cargo make do-create which handles all configuration.
#######################################
create_cluster() {
    info "Creating DigitalOcean Kubernetes cluster '${CLUSTER_NAME}'..."

    # Export the cluster name so cargo make can use it
    export DO_CLUSTER_NAME="${CLUSTER_NAME}"

    if ! run_cmd cargo make do-create; then
        error_exit "Failed to create DigitalOcean cluster '${CLUSTER_NAME}'"
    fi

    success "Cluster '${CLUSTER_NAME}' created successfully"
}

#######################################
# Saves the kubeconfig for the cluster.
#######################################
save_kubeconfig() {
    info "Saving kubeconfig for cluster '${CLUSTER_NAME}'..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} doctl kubernetes cluster kubeconfig save ${CLUSTER_NAME}"
        return 0
    fi

    if ! doctl kubernetes cluster kubeconfig save "${CLUSTER_NAME}"; then
        error_exit "Failed to save kubeconfig for cluster '${CLUSTER_NAME}'"
    fi

    success "Kubeconfig saved"
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
        error_exit "Cannot access Kubernetes cluster. Please check your cluster status."
    fi

    info "Cluster nodes:"
    kubectl get nodes

    success "Cluster is accessible"
}

#######################################
# Clears the gateway namespace to ensure a fresh state.
# Deletes the namespace if it exists, which removes all resources within it.
#######################################
clear_namespace() {
    info "Clearing namespace '${DEFAULT_NAMESPACE}' for fresh state..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl delete namespace ${DEFAULT_NAMESPACE} --ignore-not-found"
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl wait --for=delete namespace/${DEFAULT_NAMESPACE} --timeout=60s"
        success "Namespace cleanup skipped (dry-run mode)"
        return 0
    fi

    # Check if namespace exists
    if ! kubectl get namespace "${DEFAULT_NAMESPACE}" &>/dev/null; then
        success "Namespace '${DEFAULT_NAMESPACE}' does not exist, nothing to clear"
        return 0
    fi

    # Delete the namespace
    info "Deleting namespace '${DEFAULT_NAMESPACE}' and all its resources..."
    if ! kubectl delete namespace "${DEFAULT_NAMESPACE}" --ignore-not-found; then
        error_exit "Failed to delete namespace '${DEFAULT_NAMESPACE}'"
    fi

    # Wait for deletion to complete
    info "Waiting for namespace deletion to complete..."
    if ! kubectl wait --for=delete namespace/"${DEFAULT_NAMESPACE}" --timeout=60s 2>/dev/null; then
        # The wait command may fail if the namespace is already gone
        if kubectl get namespace "${DEFAULT_NAMESPACE}" &>/dev/null; then
            error_exit "Namespace '${DEFAULT_NAMESPACE}' was not deleted within timeout"
        fi
    fi

    success "Namespace '${DEFAULT_NAMESPACE}' cleared"
}

#######################################
# Ensures the cluster is up and ready.
# Creates if missing, clears namespace if existing.
#######################################
ensure_cluster_ready() {
    info "=== Cluster Setup ==="

    if do_cluster_exists "${CLUSTER_NAME}"; then
        success "DigitalOcean cluster '${CLUSTER_NAME}' already exists"
        save_kubeconfig
        verify_cluster_accessible
        clear_namespace
    else
        warn "DigitalOcean cluster '${CLUSTER_NAME}' does not exist"
        create_cluster
        save_kubeconfig
        verify_cluster_accessible
    fi

    success "Cluster is ready for use"
    echo ""
}

# =============================================================================
# MAIN EXECUTION
# =============================================================================

main() {
    parse_arguments "$@"

    echo ""
    info "==========================================="
    info "DigitalOcean Kubernetes Cluster Startup"
    info "==========================================="
    echo ""
    info "Configuration:"
    info "  Cluster name: ${CLUSTER_NAME}"
    info "  Dry run:      ${DRY_RUN}"
    echo ""

    check_prerequisites
    ensure_cluster_ready

    echo ""
    success "==========================================="
    success "Cluster startup complete"
    success "==========================================="
    echo ""
    info "kubectl context is now set to the cluster."
    info "You can now run conformance tests or deploy applications."
}

main "$@"
