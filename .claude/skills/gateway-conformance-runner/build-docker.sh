#!/usr/bin/env bash
# shellcheck disable=SC1091  # lib.sh is sourced at runtime from SCRIPT_DIR
#
# build-docker.sh
#
# Builds and pushes Docker images for the gateway controller to a container registry.
# This script handles Rust compilation verification, Docker image building, and
# pushing images to the configured registry.
#
# This script can be run standalone or called from run-conformance.sh.
#
# Usage:
#   ./build-docker.sh [OPTIONS]
#
# Options:
#   --release       Use production Dockerfile (higher optimization, slower builds)
#   --skip-push     Build images but don't push to registry
#   --dry-run       Print commands without executing them
#   --help          Show this help message
#

set -euo pipefail

# =============================================================================
# LOAD SHARED LIBRARY
# =============================================================================

# Source the shared library of functions. This provides logging functions,
# command execution utilities, and CLI tool checks.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/lib.sh"

# =============================================================================
# GLOBAL STATE
# =============================================================================

# These variables are set by parse_arguments() and used throughout the script.
DEV_BUILD=true
SKIP_PUSH=false
DRY_RUN=false

# =============================================================================
# SCRIPT-SPECIFIC FUNCTIONS
# =============================================================================

#######################################
# Prints the help message and exits.
#######################################
show_help() {
    cat << 'EOF'
Usage: build-docker.sh [OPTIONS]

Builds and pushes Docker images for the gateway controller.

Options:
  --release       Use production Dockerfile (higher optimization, slower builds)
  --skip-push     Build images but don't push to registry
  --dry-run       Print commands without executing them
  --help          Show this help message

Environment Variables:
  DOCKER_REGISTRY   Container registry URL (required for push, e.g., ghcr.io/myorg)

Examples:
  # Build and push images (dev mode - faster builds)
  ./build-docker.sh

  # Build release images and push
  ./build-docker.sh --release

  # Build only, don't push
  ./build-docker.sh --skip-push

  # See what commands would be run
  ./build-docker.sh --dry-run
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
#   DEV_BUILD  - Set to false if --release is provided
#   SKIP_PUSH  - Set to true if --skip-push is provided
#   DRY_RUN    - Set to true if --dry-run is provided
#######################################
parse_arguments() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --release)
                DEV_BUILD=false
                shift
                ;;
            --skip-push)
                SKIP_PUSH=true
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
# BUILD FUNCTIONS
# =============================================================================

#######################################
# Verifies that the Rust project compiles successfully.
# This is a non-recoverable check - compilation errors require code fixes.
# We run this before building Docker images to fail fast on code errors.
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
# The images are tagged for pushing to the container registry.
# If DEV_BUILD is true, uses Dockerfile.dev for faster builds.
#######################################
build_docker_images() {
    if [[ "${DEV_BUILD}" == true ]]; then
        info "Building Docker images (dev mode - faster builds)..."
        export DOCKERFILE="Dockerfile.dev"
    else
        info "Building Docker images (release mode)..."
    fi

    if ! run_cmd cargo make docker-build-all; then
        error_exit "Docker image build failed. Please check the build output for errors."
    fi

    success "Docker images built successfully"
}

#######################################
# Pushes Docker images to the container registry.
# DigitalOcean clusters must pull images from a registry, so this function
# pushes the built images so the cluster can access them.
#######################################
push_images_to_registry() {
    info "Pushing images to container registry..."

    if [[ -z "${DOCKER_REGISTRY:-}" ]]; then
        error_exit "DOCKER_REGISTRY environment variable is not set.
Please set it to your container registry URL.
Example: export DOCKER_REGISTRY=ghcr.io/myorg"
    fi

    # Ensure DOCKERFILE is set for dev builds
    if [[ "${DEV_BUILD}" == true ]]; then
        export DOCKERFILE="Dockerfile.dev"
    fi

    if ! run_cmd cargo make do-push-images; then
        error_exit "Failed to push images to registry"
    fi

    success "Images pushed to registry"
}

# =============================================================================
# MAIN EXECUTION
# =============================================================================

#######################################
# Main entry point for the script.
# Orchestrates the build and push workflow.
#######################################
main() {
    parse_arguments "$@"

    echo ""
    info "==========================================="
    info "Docker Image Build & Push"
    info "==========================================="
    echo ""
    info "Configuration:"
    info "  Dev build:  ${DEV_BUILD}"
    info "  Skip push:  ${SKIP_PUSH}"
    info "  Dry run:    ${DRY_RUN}"
    echo ""

    info "=== Phase: Prerequisites ==="
    check_docker_running
    success "Prerequisites satisfied"
    echo ""

    info "=== Phase: Build Images ==="
    verify_rust_compiles
    build_docker_images
    success "Build phase complete"
    echo ""

    if [[ "${SKIP_PUSH}" == true ]]; then
        warn "Skipping push phase (--skip-push specified)"
    else
        info "=== Phase: Push Images ==="
        push_images_to_registry
        success "Push phase complete"
    fi

    echo ""
    success "==========================================="
    success "Docker image workflow complete"
    success "==========================================="
}

# Only run main if script is executed directly (not sourced)
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
