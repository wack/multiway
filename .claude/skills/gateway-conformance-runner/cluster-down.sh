#!/usr/bin/env bash
#
# cluster-down.sh
#
# Destroys the DigitalOcean Kubernetes cluster and cleans up the kubectl context.
# This script is designed to be run when the conformance testing session is complete.
#
# Usage:
#   ./cluster-down.sh [OPTIONS]
#
# Options:
#   --cluster-name NAME   Name of the DigitalOcean cluster (default: multiway-local)
#   --dry-run             Print commands without executing them
#   --help                Show this help message
#

set -euo pipefail

# =============================================================================
# CONFIGURATION
# =============================================================================

readonly DEFAULT_CLUSTER_NAME="multiway-local"

# Color codes for output formatting
readonly COLOR_RED='\033[0;31m'
readonly COLOR_GREEN='\033[0;32m'
readonly COLOR_YELLOW='\033[0;33m'
readonly COLOR_BLUE='\033[0;34m'
readonly COLOR_RESET='\033[0m'

# =============================================================================
# GLOBAL STATE
# =============================================================================

CLUSTER_NAME=""
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

Destroys the DigitalOcean Kubernetes cluster and cleans up the kubectl context.

Options:
  --cluster-name NAME   Name of the DigitalOcean cluster (default: ${DEFAULT_CLUSTER_NAME})
  --dry-run             Print commands without executing them
  --help                Show this help message

Environment Variables:
  DO_CLUSTER_NAME       Alternative way to specify cluster name

Examples:
  # Destroy the default cluster
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
    CLUSTER_NAME="${DO_CLUSTER_NAME:-${DEFAULT_CLUSTER_NAME}}"

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
# =============================================================================

#######################################
# Verifies that doctl is installed and available in PATH.
#######################################
check_doctl_available() {
    info "Checking if doctl is available..."

    if ! command -v doctl &>/dev/null; then
        error_exit "doctl is not installed. Please install the DigitalOcean CLI and try again.
See: https://docs.digitalocean.com/reference/doctl/how-to/install/"
    fi

    success "doctl is available"
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
# =============================================================================

#######################################
# Checks if the specified DigitalOcean cluster exists.
# Returns:
#   0 if the cluster exists, 1 otherwise
#######################################
do_cluster_exists() {
    local readonly cluster_name="$1"
    doctl kubernetes cluster list --format Name --no-header 2>/dev/null | grep -q "^${cluster_name}$"
}

#######################################
# Gets the kubectl context name for a DigitalOcean cluster.
# DigitalOcean contexts are typically named do-<region>-<cluster-name>
# Returns the context name via stdout, or empty string if not found.
#######################################
get_kubectl_context_name() {
    local readonly cluster_name="$1"
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
