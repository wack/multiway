# Multiway Gateway

A Kubernetes Gateway API implementation built with Rust, featuring separate control plane and data plane components.

## Architecture

Multiway consists of two main components that run as separate processes in a Kubernetes cluster:

- **Control Plane** (`multiway-controlplane`) - Watches Gateway API resources and configures data plane instances
- **Data Plane** (`multiway-dataplane`) - HTTP/HTTPS proxy powered by proxy-core (monoio-based), with one deployment per Gateway resource

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

## Conformance Testing

This project includes infrastructure for running the official [Gateway API conformance test suite](https://gateway-api.sigs.k8s.io/concepts/conformance/) to validate the implementation.

### Quick Start

```bash
# Run all conformance tests
cargo make conformance

# Run only specific tests
./scripts/conformance-tests.sh run --tests "HTTPRouteSimpleSameNamespace,GatewayWithAttachedRoutes"

# Run tests from a file
./scripts/conformance-tests.sh run --file conformance/passing-tests.txt
```

### Managing Tests

The `scripts/conformance-tests.sh` script helps manage which conformance tests to run:

```bash
# List all available tests (76 total)
./scripts/conformance-tests.sh list

# List tests grouped by category
./scripts/conformance-tests.sh list --categories

# Save test list to a file
./scripts/conformance-tests.sh list --output my-tests.txt

# Run specific tests
./scripts/conformance-tests.sh run --tests "HTTPRouteSimpleSameNamespace"

# Run tests from a file (one test name per line)
./scripts/conformance-tests.sh run --file conformance/passing-tests.txt

# Skip certain tests
./scripts/conformance-tests.sh run --skip "HTTPRouteTimeout,GRPCRouteWeight"
```

### Incremental Conformance Workflow

Since this gateway implementation will initially pass only a subset of tests, use this workflow to track progress:

1. **View all available tests:**
   ```bash
   ./scripts/conformance-tests.sh list --categories
   ```

2. **Edit the passing tests file** to include tests you expect to pass:
   ```bash
   # Edit conformance/passing-tests.txt
   # Uncomment test names as you implement features
   ```

3. **Run only the passing tests** to verify no regressions:
   ```bash
   ./scripts/conformance-tests.sh run --file conformance/passing-tests.txt
   ```

4. **As you implement more features**, uncomment additional tests in `passing-tests.txt` and re-run.

### Test Categories

| Category | Description |
|----------|-------------|
| HTTPRoute | HTTP routing, matching, redirects, rewrites, headers |
| Gateway | Gateway lifecycle, listeners, TLS configuration |
| GRPCRoute | gRPC routing and matching |
| TLSRoute | TLS passthrough routing |
| UDPRoute | UDP routing |
| BackendTLSPolicy | Backend TLS configuration |

### Individual Conformance Commands

```bash
# Build the conformance test Docker image
cargo make conformance-build

# Load the image into kind cluster
cargo make conformance-load

# Run the conformance job
cargo make conformance-run

# View logs from the last run
cargo make conformance-logs

# Clean up conformance resources
cargo make conformance-cleanup
```
