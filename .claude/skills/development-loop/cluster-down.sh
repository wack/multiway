#!/usr/bin/env bash
# shellcheck disable=SC1091  # lib.sh is sourced at runtime from SCRIPT_DIR
#
# cluster-down.sh
#
# Destroys the DigitalOcean Kubernetes cluster and cleans up the kubectl context.
# This script is designed to be run when the conformance testing session is complete.
#
# The cluster name defaults to a sanitized version of the current git branch,
# prefixed with "mw-" (e.g., branch "feature/my-test" becomes "mw-feature-my-test").
#
# Usage:
#   ./cluster-down.sh [OPTIONS]
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
source "${SCRIPT_DIR}/../conformance/lib.sh"

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

Destroys the DigitalOcean Kubernetes cluster and cleans up the kubectl context.

The cluster name defaults to a sanitized version of the current git branch,
prefixed with "${CLUSTER_NAME_PREFIX}-" (e.g., "feature/my-test" becomes "${CLUSTER_NAME_PREFIX}-feature-my-test").

Options:
  --cluster-name NAME   Name of the DigitalOcean cluster (default: ${default_name})
  --dry-run             Print commands without executing them
  --help                Show this help message

Environment Variables:
  DO_CLUSTER_NAME       Override the cluster name

Examples:
  # Destroy cluster for current branch
  ./cluster-down.sh

  # Destroy a specific cluster
  ./cluster-down.sh --cluster-name my-test-cluster

  # See what commands would be run
  ./cluster-down.sh --dry-run
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
# Gets the kubectl context name for a DigitalOcean cluster.
# DigitalOcean contexts are typically named do-<region>-<cluster-name>
# Returns the context name via stdout, or empty string if not found.
#######################################
get_kubectl_context_name() {
    local -r cluster_name="$1"
    kubectl config get-contexts -o name 2>/dev/null | grep "${cluster_name}" | head -1 || echo ""
}

#######################################
# Deletes the kubectl context for the cluster.
#######################################
delete_kubectl_context() {
    info "Cleaning up kubectl context..."

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

    # Get the current context to check if we need to switch
    local current_context
    current_context=$(kubectl config current-context 2>/dev/null || echo "")

    # If the context we're deleting is the current one, unset it first
    if [[ "${current_context}" == "${context_name}" ]]; then
        warn "Unsetting current kubectl context..."
        kubectl config unset current-context 2>/dev/null || true
    fi

    # Delete the context
    info "Deleting context '${context_name}'..."
    kubectl config delete-context "${context_name}" 2>/dev/null || true

    # Try to delete associated cluster and user entries
    # These are typically named with the same pattern
    local cluster_entry
    cluster_entry=$(kubectl config get-clusters 2>/dev/null | grep "${CLUSTER_NAME}" | head -1 || echo "")
    if [[ -n "${cluster_entry}" ]]; then
        info "Deleting cluster entry '${cluster_entry}'..."
        kubectl config delete-cluster "${cluster_entry}" 2>/dev/null || true
    fi

    local user_entry
    user_entry=$(kubectl config get-users 2>/dev/null | grep "${CLUSTER_NAME}" | head -1 || echo "")
    if [[ -n "${user_entry}" ]]; then
        info "Deleting user entry '${user_entry}'..."
        kubectl config delete-user "${user_entry}" 2>/dev/null || true
    fi

    success "kubectl context cleaned up"
}

#######################################
# Destroys the DigitalOcean Kubernetes cluster.
#######################################
destroy_cluster() {
    info "=== Cluster Destruction ==="

    if [[ "${DRY_RUN}" == true ]]; then
        if do_cluster_exists "${CLUSTER_NAME}"; then
            info "Cluster '${CLUSTER_NAME}' exists"
        else
            info "Cluster '${CLUSTER_NAME}' does not exist"
        fi
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} doctl kubernetes cluster delete ${CLUSTER_NAME} --force --dangerous"
        success "Cluster destruction skipped (dry-run mode)"
        delete_kubectl_context
        return 0
    fi

    # Check if cluster exists
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

    # Clean up kubectl context
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
    info "DigitalOcean Kubernetes Cluster Shutdown"
    info "==========================================="
    echo ""
    info "Configuration:"
    info "  Cluster name: ${CLUSTER_NAME}"
    info "  Dry run:      ${DRY_RUN}"
    echo ""

    check_prerequisites
    destroy_cluster

    echo ""
    success "==========================================="
    success "Cluster shutdown complete"
    success "==========================================="
    echo ""
    info "The DigitalOcean cluster has been destroyed."
    info "You will no longer be charged for cluster resources."
}

main "$@"
