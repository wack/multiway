#!/usr/bin/env bash
# shellcheck disable=SC1091  # lib.sh is sourced at runtime from SCRIPT_DIR
#
# cluster-up.sh
#
# Ensures the DigitalOcean Kubernetes cluster is accessible and creates
# an isolated namespace for conformance testing. If the cluster doesn't exist,
# it is created automatically. The namespace is derived from the current git
# branch name (lowercased, sanitized).
#
# Usage:
#   ./cluster-up.sh [OPTIONS]
#
# Options:
#   --cluster-name NAME   Name of the DigitalOcean cluster (default: mw-conformance)
#   --namespace NAME      Namespace to create (default: derived from git branch)
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

# The default conformance cluster. This cluster is long-lived and shared across
# all conformance test runs. If it doesn't exist, it will be created automatically.
readonly DEFAULT_CLUSTER_NAME="mw-conformance"

# =============================================================================
# GLOBAL STATE
# =============================================================================

# These variables are set by parse_arguments() and used throughout the script.
CLUSTER_NAME="${DEFAULT_CLUSTER_NAME}"
NAMESPACE=""
DRY_RUN=false

# =============================================================================
# SCRIPT-SPECIFIC FUNCTIONS
# =============================================================================

#######################################
# Prints the help message and exits.
#######################################
show_help() {
    cat << EOF
Usage: $(basename "$0") [OPTIONS]

Ensures the DigitalOcean Kubernetes cluster is accessible and creates
an isolated namespace for conformance testing. If the cluster doesn't
exist, it will be created automatically.

The namespace defaults to a sanitized version of the current git branch name,
lowercased (e.g., branch "robbie/multi-1101" becomes "multi-1101").

Options:
  --cluster-name NAME   Name of the DigitalOcean cluster (default: ${DEFAULT_CLUSTER_NAME})
  --namespace NAME      Namespace to create (default: derived from git branch)
  --dry-run             Print commands without executing them
  --help                Show this help message

Environment Variables:
  DO_CLUSTER_NAME         Override the cluster name
  CONFORMANCE_NAMESPACE   Override the namespace

Examples:
  # Prepare namespace for current branch
  ./cluster-up.sh

  # Use a specific namespace (e.g., for a Linear ticket)
  ./cluster-up.sh --namespace multi-1101

  # See what commands would be run
  ./cluster-up.sh --dry-run
EOF
    exit 0
}

#######################################
# Derives a namespace name from the current git branch.
# Extracts the last path component (e.g., "robbie/multi-1101" -> "multi-1101")
# and lowercases it.
#######################################
get_default_namespace() {
    local branch_name
    if branch_name=$(git rev-parse --abbrev-ref HEAD 2>/dev/null); then
        # Take the last path component (after the last /)
        local base_name="${branch_name##*/}"
        # Lowercase and sanitize for Kubernetes namespace rules
        echo "${base_name}" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9-]/-/g; s/^-//; s/-$//'
    else
        echo "multiway-system"
    fi
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
    # Default cluster name from environment variable
    if [[ -n "${DO_CLUSTER_NAME:-}" ]]; then
        CLUSTER_NAME="${DO_CLUSTER_NAME}"
    fi

    # Default namespace from environment variable, then git branch
    if [[ -n "${CONFORMANCE_NAMESPACE:-}" ]]; then
        NAMESPACE="${CONFORMANCE_NAMESPACE}"
    else
        NAMESPACE=$(get_default_namespace)
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
            --namespace)
                if [[ -z "${2:-}" ]]; then
                    error_exit "--namespace requires a value"
                fi
                NAMESPACE="$2"
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
# CLUSTER AND NAMESPACE MANAGEMENT
#
# Note: do_cluster_exists is provided by lib.sh
# =============================================================================

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
# Creates a fresh namespace for this conformance run.
# Deletes the namespace first if it already exists, to ensure a clean slate.
#######################################
create_namespace() {
    info "Setting up namespace '${NAMESPACE}'..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl delete namespace ${NAMESPACE} --ignore-not-found --wait=true"
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl create namespace ${NAMESPACE}"
        success "Namespace setup skipped (dry-run mode)"
        return 0
    fi

    # Delete existing namespace if present (ensures clean state)
    if kubectl get namespace "${NAMESPACE}" &>/dev/null; then
        warn "Namespace '${NAMESPACE}' already exists, deleting for clean state..."
        if ! kubectl delete namespace "${NAMESPACE}" --wait=true; then
            error_exit "Failed to delete existing namespace '${NAMESPACE}'"
        fi
        # Wait for deletion to fully complete
        if ! kubectl wait --for=delete namespace/"${NAMESPACE}" --timeout=60s 2>/dev/null; then
            if kubectl get namespace "${NAMESPACE}" &>/dev/null; then
                error_exit "Namespace '${NAMESPACE}' was not deleted within timeout"
            fi
        fi
    fi

    # Create fresh namespace
    if ! kubectl create namespace "${NAMESPACE}"; then
        error_exit "Failed to create namespace '${NAMESPACE}'"
    fi

    success "Namespace '${NAMESPACE}' created"
}

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
# Ensures the cluster is accessible and namespace is ready.
# If the cluster doesn't exist, creates it automatically.
#######################################
ensure_ready() {
    info "=== Cluster and Namespace Setup ==="

    if do_cluster_exists "${CLUSTER_NAME}"; then
        success "Cluster '${CLUSTER_NAME}' already exists"
    else
        warn "Cluster '${CLUSTER_NAME}' does not exist, creating it..."
        create_cluster
    fi

    save_kubeconfig
    verify_cluster_accessible
    create_namespace

    success "Cluster and namespace ready"
    echo ""
}

# =============================================================================
# MAIN EXECUTION
# =============================================================================

main() {
    parse_arguments "$@"

    echo ""
    info "==========================================="
    info "Conformance Namespace Setup"
    info "==========================================="
    echo ""
    info "Configuration:"
    info "  Cluster name: ${CLUSTER_NAME}"
    info "  Namespace:    ${NAMESPACE}"
    info "  Dry run:      ${DRY_RUN}"
    echo ""

    check_prerequisites
    ensure_ready

    echo ""
    success "==========================================="
    success "Namespace setup complete"
    success "==========================================="
    echo ""
    info "kubectl context is set to cluster '${CLUSTER_NAME}'."
    info "Namespace '${NAMESPACE}' is ready for conformance testing."
}

main "$@"
