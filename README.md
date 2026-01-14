# Multiway Gateway

A Kubernetes Gateway API implementation built with Rust, featuring separate control plane and data plane components.

## Architecture

Multiway consists of two main components that run as separate processes in a Kubernetes cluster:

- **Control Plane** (`multiway-controlplane`) - Watches Gateway API resources and configures data plane instances
- **Data Plane** (`multiway-dataplane`) - HTTP/HTTPS proxy powered by Pingora, with one deployment per Gateway resource

## Prerequisites

- [kopium](https://github.com/kube-rs/kopium) - Required for generating Rust bindings from Kubernetes CRDs
- Docker with [buildx](https://docs.docker.com/build/buildx/) support - Required for building container images

## Docker Builds

The project provides two container images that must be built from the workspace root:

- **Control Plane**: `multiway-controlplane` (built from `crates/controlplane/Dockerfile`)
- **Data Plane**: `multiway-dataplane` (built from `crates/dataplane/Dockerfile`)

Both images support multi-architecture builds for `linux/amd64` and `linux/arm64` using Docker buildx.

### Building for Current Platform

To build images for your current platform (fastest for local development):

```bash
# Build both images
cargo make docker-build-all

# Or build individually
cargo make docker-build-controlplane
cargo make docker-build-dataplane
```

### Building for Multiple Platforms (Cross-Platform)

To build images for both AMD64 and ARM64 architectures:

```bash
# Build both images for multiple platforms
cargo make docker-build-all-cross

# Or build individually
cargo make docker-build-controlplane-cross
cargo make docker-build-dataplane-cross
```

**Important Notes:**
- Cross-platform builds use Docker buildx and will automatically set up the `multiway-builder` buildx instance if needed
- Multi-platform images cannot be loaded into your local Docker daemon - they must be pushed to a registry
- For local development and testing, use the single-platform build commands instead
- To distribute multi-platform images, use the push commands below

### Pushing to a Registry

To build and push multi-architecture images to a container registry:

```bash
# Set your registry (required)
export DOCKER_REGISTRY=ghcr.io/myorg

# Build and push both images
cargo make docker-build-all-push

# Or push individually
cargo make docker-build-controlplane-push
cargo make docker-build-dataplane-push
```

The images will be tagged as `${DOCKER_REGISTRY}/multiway-controlplane:latest` and `${DOCKER_REGISTRY}/multiway-dataplane:latest`.

## Gateway API CRDs

This project includes Kubernetes Gateway API CRD bindings. To update the CRDs and regenerate Rust bindings:

```bash
# Update GATEWAY_API_VERSION in Makefile.toml, then run:
cargo make gateway-api-sync
```

This downloads the CRD definitions to `.crds/v<version>/` and generates Rust bindings in `crates/gateway-crds/src/`.
