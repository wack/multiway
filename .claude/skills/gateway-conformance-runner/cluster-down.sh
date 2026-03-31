#!/usr/bin/env bash
# shellcheck disable=SC1091  # lib.sh is sourced at runtime from SCRIPT_DIR
#
# cluster-down.sh
#
# Tears down the conformance test namespace, cleaning up all resources deployed
# during the test run. The shared cluster is NOT destroyed — only the namespace
# is deleted.
#
# The namespace defaults to a sanitized version of the current git branch name,
# lowercased (e.g., branch "robbie/multi-1101" becomes "multi-1101").
#
# Usage:
#   ./cluster-down.sh [OPTIONS]
#
# Options:
#   --cluster-name NAME   Name of the DigitalOcean cluster (default: mw-conformance)
#   --namespace NAME      Namespace to delete (default: derived from git branch)
#   --destroy-cluster     Also destroy the DigitalOcean cluster and remove kubeconfig
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

# The shared conformance cluster. This cluster is long-lived and never destroyed.
readonly DEFAULT_CLUSTER_NAME="mw-conformance"

# =============================================================================
# GLOBAL STATE
# =============================================================================

# These variables are set by parse_arguments() and used throughout the script.
CLUSTER_NAME="${DEFAULT_CLUSTER_NAME}"
NAMESPACE=""
DESTROY_CLUSTER=false
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

Tears down the conformance test namespace. By default the cluster is preserved.
Use --destroy-cluster to also destroy the DigitalOcean cluster and clean up
the local kubeconfig.

The namespace defaults to a sanitized version of the current git branch name,
lowercased (e.g., branch "robbie/multi-1101" becomes "multi-1101").

Options:
  --cluster-name NAME   Name of the DigitalOcean cluster (default: ${DEFAULT_CLUSTER_NAME})
  --namespace NAME      Namespace to delete (default: derived from git branch)
  --destroy-cluster     Also destroy the DigitalOcean cluster and remove kubeconfig
  --dry-run             Print commands without executing them
  --help                Show this help message

Environment Variables:
  DO_CLUSTER_NAME         Override the cluster name
  CONFORMANCE_NAMESPACE   Override the namespace

Examples:
  # Delete namespace for current branch
  ./cluster-down.sh

  # Delete a specific namespace
  ./cluster-down.sh --namespace multi-1101

  # Delete namespace AND destroy the cluster to save costs
  ./cluster-down.sh --destroy-cluster

  # See what commands would be run
  ./cluster-down.sh --dry-run
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
            --destroy-cluster)
                DESTROY_CLUSTER=true
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
#
# Note: check_kubectl_available is provided by lib.sh
# =============================================================================

#######################################
# Runs all prerequisite checks.
#######################################
check_prerequisites() {
    info "=== Checking Prerequisites ==="
    check_kubectl_available
    if [[ "${DESTROY_CLUSTER}" == true ]]; then
        check_doctl_available
    fi
    success "All prerequisites satisfied"
    echo ""
}

# =============================================================================
# NAMESPACE TEARDOWN
# =============================================================================

#######################################
# Deletes the conformance test namespace and all its resources.
# This is the primary cleanup mechanism — deleting the namespace removes
# all deployments, services, configmaps, and other resources within it.
#######################################
delete_namespace() {
    info "=== Namespace Teardown ==="

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl delete namespace ${NAMESPACE} --wait=true --ignore-not-found"
        success "Namespace deletion skipped (dry-run mode)"
        return 0
    fi

    # Check if namespace exists
    if ! kubectl get namespace "${NAMESPACE}" &>/dev/null; then
        success "Namespace '${NAMESPACE}' does not exist, nothing to clean up"
        return 0
    fi

    info "Deleting namespace '${NAMESPACE}' and all its resources..."
    if ! kubectl delete namespace "${NAMESPACE}" --wait=true --ignore-not-found; then
        error_exit "Failed to delete namespace '${NAMESPACE}'"
    fi

    # Wait for deletion to fully complete
    info "Waiting for namespace deletion to complete..."
    if ! kubectl wait --for=delete namespace/"${NAMESPACE}" --timeout=120s 2>/dev/null; then
        # The wait command may fail if the namespace is already gone
        if kubectl get namespace "${NAMESPACE}" &>/dev/null; then
            error_exit "Namespace '${NAMESPACE}' was not deleted within timeout"
        fi
    fi

    success "Namespace '${NAMESPACE}' deleted"
    echo ""
}

# =============================================================================
# CLUSTER DESTRUCTION (optional, triggered by --destroy-cluster)
#
# Note: do_cluster_exists is provided by lib.sh
# =============================================================================

#######################################
# Gets the kubectl context name for a DigitalOcean cluster.
# DigitalOcean contexts are typically named do-<region>-<cluster-name>.
# Returns the context name via stdout, or empty string if not found.
#######################################
get_kubectl_context_name() {
    local -r cluster_name="$1"
    kubectl config get-contexts -o name 2>/dev/null | grep "${cluster_name}" | head -1 || echo ""
}

#######################################
# Deletes the kubectl context, cluster entry, and user entry for the cluster.
# This cleans up the local kubeconfig so the destroyed cluster no longer
# appears in `kubectl config get-contexts`.
#######################################
delete_kubectl_context() {
    info "Cleaning up kubectl context for cluster '${CLUSTER_NAME}'..."

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl config delete-context <context-for-${CLUSTER_NAME}>"
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl config delete-cluster <cluster-for-${CLUSTER_NAME}>"
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} kubectl config delete-user <user-for-${CLUSTER_NAME}>"
        success "kubectl context cleanup skipped (dry-run mode)"
        return 0
    fi

    local context_name
    context_name=$(get_kubectl_context_name "${CLUSTER_NAME}")

    if [[ -z "${context_name}" ]]; then
        success "No kubectl context found for cluster '${CLUSTER_NAME}'"
        return 0
    fi

    info "Found kubectl context: ${context_name}"

    # If this is the current context, unset it first
    local current_context
    current_context=$(kubectl config current-context 2>/dev/null || echo "")
    if [[ "${current_context}" == "${context_name}" ]]; then
        warn "Unsetting current kubectl context..."
        kubectl config unset current-context 2>/dev/null || true
    fi

    # Delete context, cluster entry, and user entry
    kubectl config delete-context "${context_name}" 2>/dev/null || true

    local cluster_entry
    cluster_entry=$(kubectl config get-clusters 2>/dev/null | grep "${CLUSTER_NAME}" | head -1 || echo "")
    if [[ -n "${cluster_entry}" ]]; then
        kubectl config delete-cluster "${cluster_entry}" 2>/dev/null || true
    fi

    local user_entry
    user_entry=$(kubectl config get-users 2>/dev/null | grep "${CLUSTER_NAME}" | head -1 || echo "")
    if [[ -n "${user_entry}" ]]; then
        kubectl config delete-user "${user_entry}" 2>/dev/null || true
    fi

    success "kubectl context cleaned up"
}

#######################################
# Destroys the DigitalOcean Kubernetes cluster and cleans up kubeconfig.
#######################################
destroy_cluster() {
    info "=== Cluster Destruction ==="

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} doctl kubernetes cluster delete ${CLUSTER_NAME} --force --dangerous"
        success "Cluster destruction skipped (dry-run mode)"
        delete_kubectl_context
        return 0
    fi

    if ! do_cluster_exists "${CLUSTER_NAME}"; then
        warn "Cluster '${CLUSTER_NAME}' does not exist, nothing to destroy"
        delete_kubectl_context
        return 0
    fi

    info "Destroying DigitalOcean Kubernetes cluster '${CLUSTER_NAME}'..."
    warn "This action is irreversible!"

    if ! doctl kubernetes cluster delete "${CLUSTER_NAME}" --force --dangerous; then
        error_exit "Failed to destroy cluster '${CLUSTER_NAME}'"
    fi

    success "Cluster '${CLUSTER_NAME}' destroyed"

    delete_kubectl_context
    echo ""
}

# =============================================================================
# MAIN EXECUTION
# =============================================================================

main() {
    parse_arguments "$@"

    echo ""
    info "==========================================="
    info "Conformance Teardown"
    info "==========================================="
    echo ""
    info "Configuration:"
    info "  Cluster name:    ${CLUSTER_NAME}"
    info "  Namespace:       ${NAMESPACE} (will be deleted)"
    info "  Destroy cluster: ${DESTROY_CLUSTER}"
    info "  Dry run:         ${DRY_RUN}"
    echo ""

    check_prerequisites
    delete_namespace

    if [[ "${DESTROY_CLUSTER}" == true ]]; then
        destroy_cluster
    fi

    echo ""
    success "==========================================="
    success "Teardown complete"
    success "==========================================="
    echo ""
    info "Namespace '${NAMESPACE}' has been deleted."
    if [[ "${DESTROY_CLUSTER}" == true ]]; then
        info "Cluster '${CLUSTER_NAME}' has been destroyed and removed from kubeconfig."
    else
        info "Cluster '${CLUSTER_NAME}' remains running."
    fi
}

main "$@"
