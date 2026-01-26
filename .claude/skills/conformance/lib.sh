#!/usr/bin/env bash
#
# lib.sh
#
# Shared library of functions for the conformance skill scripts.
# This file should be sourced by other scripts, not executed directly.
#
# Usage:
#   # At the top of your script, after set -euo pipefail:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "${SCRIPT_DIR}/lib.sh"
#
# This library provides:
#   - Color constants for formatted output
#   - Logging functions (info, success, warn, error_exit)
#   - Command execution with dry-run support (run_cmd)
#   - Cluster name utilities (sanitize_cluster_name, get_default_cluster_name)
#   - CLI tool availability checks (check_kubectl_available, check_doctl_available, check_docker_running)
#   - DigitalOcean cluster operations (do_cluster_exists)
#
# Required variables that must be set by the sourcing script:
#   - DRY_RUN (boolean) - Controls whether run_cmd executes or just prints commands
#
# Optional variables that can be overridden:
#   - CLUSTER_NAME_PREFIX (default: "mw") - Prefix for auto-generated cluster names
#   - MAX_CLUSTER_NAME_LENGTH (default: 63) - Maximum length for cluster names
#

# Prevent direct execution
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    echo "Error: lib.sh should be sourced, not executed directly." >&2
    echo "Usage: source lib.sh" >&2
    exit 1
fi

# =============================================================================
# CONSTANTS
# =============================================================================

# Color codes for output formatting. Using these consistently across all
# scripts provides a unified visual experience and makes it easier to
# scan output for errors (red), warnings (yellow), successes (green),
# and informational messages (blue).
readonly LIB_COLOR_RED='\033[0;31m'
readonly LIB_COLOR_GREEN='\033[0;32m'
readonly LIB_COLOR_YELLOW='\033[0;33m'
readonly LIB_COLOR_BLUE='\033[0;34m'
readonly LIB_COLOR_RESET='\033[0m'

# Export color constants with the names scripts expect. We define them with
# LIB_ prefix first to avoid conflicts, then export with standard names.
# Using 'declare' instead of 'readonly' allows scripts to have already
# defined these (the library values are used if not already set).
: "${COLOR_RED:=${LIB_COLOR_RED}}"
: "${COLOR_GREEN:=${LIB_COLOR_GREEN}}"
: "${COLOR_YELLOW:=${LIB_COLOR_YELLOW}}"
: "${COLOR_BLUE:=${LIB_COLOR_BLUE}}"
: "${COLOR_RESET:=${LIB_COLOR_RESET}}"

# Cluster naming constants. DigitalOcean has specific requirements for
# cluster names: lowercase alphanumeric and hyphens only, max 63 chars.
# The prefix helps identify clusters created by this tooling.
: "${CLUSTER_NAME_PREFIX:=mw}"
: "${MAX_CLUSTER_NAME_LENGTH:=63}"

# =============================================================================
# LOGGING FUNCTIONS
#
# These functions provide consistent, color-coded output across all scripts.
# Each function adds a tag prefix ([INFO], [OK], [WARN], [ERROR]) to make
# it easy to scan logs and identify message severity at a glance.
# =============================================================================

#######################################
# Prints an informational message in blue.
# Use for general progress updates and status information.
# Arguments:
#   $1 - The message to print
#######################################
info() {
    local -r message="$1"
    echo -e "${COLOR_BLUE}[INFO]${COLOR_RESET} ${message}"
}

#######################################
# Prints a success message in green.
# Use when an operation completes successfully.
# Arguments:
#   $1 - The message to print
#######################################
success() {
    local -r message="$1"
    echo -e "${COLOR_GREEN}[OK]${COLOR_RESET} ${message}"
}

#######################################
# Prints a warning message in yellow.
# Use for non-fatal issues or important notices that don't stop execution.
# Arguments:
#   $1 - The message to print
#######################################
warn() {
    local -r message="$1"
    echo -e "${COLOR_YELLOW}[WARN]${COLOR_RESET} ${message}"
}

#######################################
# Prints an error message in red and exits with code 1.
# Use for fatal errors that prevent the script from continuing.
# The message is sent to stderr so it's visible even when stdout is redirected.
# Arguments:
#   $1 - The error message to print
#######################################
error_exit() {
    local -r message="$1"
    echo -e "${COLOR_RED}[ERROR]${COLOR_RESET} ${message}" >&2
    exit 1
}

# =============================================================================
# COMMAND EXECUTION
#
# The run_cmd function provides a unified way to execute commands with
# dry-run support. This is essential for testing script behavior without
# making actual changes to infrastructure.
# =============================================================================

#######################################
# Executes a command, or prints it if in dry-run mode.
# This function checks the global DRY_RUN variable to determine behavior.
# In dry-run mode, the command is printed with a [DRY-RUN] prefix but not
# executed, and the function returns success (0).
#
# Arguments:
#   $@ - The command and its arguments to execute
# Returns:
#   The exit code of the command (0 in dry-run mode)
# Requires:
#   DRY_RUN - Global variable (must be "true" or "false")
#######################################
run_cmd() {
    if [[ "${DRY_RUN:-false}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} $*"
        return 0
    else
        "$@"
    fi
}

# =============================================================================
# CLUSTER NAME UTILITIES
#
# DigitalOcean Kubernetes cluster names must follow specific rules:
# - Lowercase letters, numbers, and hyphens only
# - Cannot start or end with a hyphen
# - Maximum 63 characters
#
# These functions help generate valid cluster names from git branch names,
# which often contain characters like slashes that aren't allowed.
# =============================================================================

#######################################
# Sanitizes a string for use as a DigitalOcean cluster name.
# Applies the following transformations:
#   - Converts to lowercase (DigitalOcean names are case-insensitive)
#   - Replaces non-alphanumeric characters with hyphens
#   - Collapses multiple consecutive hyphens into one
#   - Removes leading and trailing hyphens
#   - Truncates to maximum allowed length
#
# Arguments:
#   $1 - The string to sanitize
# Outputs:
#   The sanitized string suitable for use as a cluster name
#######################################
sanitize_cluster_name() {
    local name="$1"

    # Convert to lowercase
    name=$(echo "$name" | tr '[:upper:]' '[:lower:]')

    # Replace non-alphanumeric characters with hyphens
    name="${name//[^a-z0-9]/-}"

    # Collapse multiple consecutive hyphens into one
    # Using extglob for this pattern
    shopt -s extglob
    name="${name//+(-)/-}"
    shopt -u extglob

    # Remove leading and trailing hyphens
    name=$(echo "$name" | sed 's/^-//;s/-$//')

    # Truncate to max length
    echo "${name:0:${MAX_CLUSTER_NAME_LENGTH}}"
}

#######################################
# Gets the default cluster name based on the current git branch.
# This allows each branch to have its own isolated cluster, which is
# useful for parallel development and CI/CD pipelines.
#
# Falls back to "local" if not in a git repository or if git fails.
#
# Outputs:
#   The cluster name with prefix (e.g., "mw-feature-my-branch")
#######################################
get_default_cluster_name() {
    local branch_name

    # Try to get the current git branch
    if branch_name=$(git rev-parse --abbrev-ref HEAD 2>/dev/null); then
        local sanitized
        sanitized=$(sanitize_cluster_name "$branch_name")
        echo "${CLUSTER_NAME_PREFIX}-${sanitized}"
    else
        # Not in a git repo, use fallback
        echo "${CLUSTER_NAME_PREFIX}-local"
    fi
}

# =============================================================================
# PREREQUISITE CHECKS
#
# These functions verify that required CLI tools are installed and available.
# They provide helpful error messages with installation instructions when
# tools are missing. All checks are non-recoverable - if a tool is missing,
# the user must install it manually before the script can proceed.
# =============================================================================

#######################################
# Verifies that kubectl is installed and available in PATH.
# kubectl is required for interacting with Kubernetes clusters.
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
# Verifies that doctl (DigitalOcean CLI) is installed and available in PATH.
# doctl is required for creating and managing DigitalOcean Kubernetes clusters.
# This is a non-recoverable check - doctl must be installed manually.
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
# Verifies that the Docker daemon is running.
# Docker is required for building container images.
# This is a non-recoverable check - if Docker is not running, the script exits.
#######################################
check_docker_running() {
    info "Checking if Docker is running..."

    if ! docker info &>/dev/null; then
        error_exit "Docker is not running. Please start the Docker daemon and try again."
    fi

    success "Docker is running"
}

# =============================================================================
# DIGITALOCEAN CLUSTER OPERATIONS
#
# These functions interact with DigitalOcean's Kubernetes service via doctl.
# They provide building blocks for cluster lifecycle management.
# =============================================================================

#######################################
# Checks if the specified DigitalOcean cluster exists.
# Uses doctl to query the list of clusters and checks for an exact name match.
#
# Arguments:
#   $1 - The cluster name to check
# Returns:
#   0 if the cluster exists, 1 otherwise
#######################################
do_cluster_exists() {
    local -r cluster_name="$1"
    doctl kubernetes cluster list --format Name --no-header 2>/dev/null | grep -q "^${cluster_name}$"
}
