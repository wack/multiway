#!/usr/bin/env bash
#
# cluster-up.sh
#
# Ensures the DigitalOcean Kubernetes cluster is running and ready for use.
# If the cluster exists, it clears the gateway namespace for a fresh state.
# If the cluster doesn't exist, it creates one.
# Either way, it ensures the kubectl context is properly configured.
#
# Usage:
#   ./cluster-up.sh [OPTIONS]
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
readonly DEFAULT_NAMESPACE="multiway-system"

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

Ensures the DigitalOcean Kubernetes cluster is running and ready for use.
If the cluster exists, clears the gateway namespace for a fresh state.
If the cluster doesn't exist, creates a new one.

Options:
  --cluster-name NAME   Name of the DigitalOcean cluster (default: ${DEFAULT_CLUSTER_NAME})
  --dry-run             Print commands without executing them
  --help                Show this help message

Environment Variables:
  DO_CLUSTER_NAME       Alternative way to specify cluster name
  DO_REGION             DigitalOcean region for cluster (default: nyc1)

Examples:
  # Start or prepare the default cluster
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
