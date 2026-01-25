---
name: conformance
description: Use this skill when you need to run the Gateway API conformance test suite for the multiway project. It includes setting up a DigitalOcean Kubernetes cluster, building and deploying the gateway controller, running the official conformance tests, and analyzing the results. The skill handles the complete workflow from cluster creation to test execution and log retrieval.
---

You are responsible for running the Gateway API conformance test suite for the multiway project and reporting the results.

# IMPORTANT: Cluster Setup Required

**Before doing anything else, you MUST run the cluster setup script:**

```bash
.claude/skills/conformance/cluster-up.sh
```

This creates the DigitalOcean Kubernetes cluster (or prepares an existing one) and sets your kubectl context. **Do not skip this step** - it ensures you're testing against the correct environment.

# Cluster Lifecycle

- **Cluster Setup** (`cluster-up.sh`): Run at the start of the skill. Creates the DigitalOcean Kubernetes cluster if it doesn't exist, or clears the namespace if it does. Ensures kubectl context is properly configured.
- **Cluster Cleanup** (`cluster-down.sh`): Run manually when you want to destroy the cluster. This is NOT run automatically - you must run it yourself when done to avoid unnecessary costs.

## Cluster Naming

The cluster name defaults to a sanitized version of the current git branch, prefixed with `mw-`. For example:
- Branch `main` → cluster `mw-main`
- Branch `feature/my-test` → cluster `mw-feature-my-test`
- Branch `claude/migrate-kind-to-digitalocean-Ytxrr` → cluster `mw-claude-migrate-kind-to-digitalocean-ytxrr`

This allows multiple developers or branches to have isolated clusters without conflicts.

You can override the cluster name with `--cluster-name` or the `DO_CLUSTER_NAME` environment variable.

You can also run these scripts manually:

```bash
# Start or prepare the cluster for current branch
.claude/skills/conformance/cluster-up.sh

# Use a specific cluster name
.claude/skills/conformance/cluster-up.sh --cluster-name my-cluster

# Destroy the cluster when done
.claude/skills/conformance/cluster-down.sh
```

# Running Conformance Tests

Use the automated script located at `.claude/skills/conformance/run-conformance.sh` to run the conformance tests.

**Note**: The cluster must be running before executing this script. Run `cluster-up.sh` first if the cluster isn't already set up.

## Basic Usage

```bash
.claude/skills/conformance/run-conformance.sh
```

## Script Options

| Option | Description |
|--------|-------------|
| `--skip-build` | Skip Rust compilation and Docker image building (use when images already exist) |
| `--skip-deploy` | Skip gateway controller deployment (use when controller is already running) |
| `--cluster-name NAME` | Specify cluster name (default: derived from git branch, e.g., `mw-main`) |
| `--dry-run` | Print commands without executing them |
| `--help` | Show help message |

## What the Script Does

The script automates the conformance testing workflow (cluster is managed separately):

1. **Prerequisites Check**: Verifies Docker and kubectl are available, cluster is accessible
2. **Environment Verification**: Checks that `GATEWAY_CONFORMANCE_SUITE` and `DOCKER_REGISTRY` environment variables are set
3. **Build & Push**: Calls `build-docker.sh` to compile the Rust project, build Docker images, and push them to the container registry (can be run standalone)
4. **Deploy**: Cleans up any existing deployments, installs Gateway API CRDs, creates a fresh namespace, deploys the gateway controller, and waits for pods to be ready
5. **Test Execution**: Runs the conformance tests from the local Gateway API repository

## Prerequisites

Before running the skill, ensure:

1. **Docker** is installed and running
2. **kubectl** is installed
3. **doctl** (DigitalOcean CLI) is installed and authenticated
4. **Gateway API repository** is cloned locally
5. **`GATEWAY_CONFORMANCE_SUITE`** environment variable is set in `.envrc.local`:
   ```bash
   export GATEWAY_CONFORMANCE_SUITE=/path/to/gateway-api
   ```
6. **`DOCKER_REGISTRY`** environment variable is set in `.envrc.local`:
   ```bash
   export DOCKER_REGISTRY=ghcr.io/myorg
   ```
7. **`DO_REGION`** (optional) environment variable for the cluster region (default: `nyc3`):
   ```bash
   export DO_REGION=nyc3
   ```

## Example Commands

```bash
# Full test run (build, deploy, test)
.claude/skills/conformance/run-conformance.sh

# Quick re-test (skip build, images already exist)
.claude/skills/conformance/run-conformance.sh --skip-build

# Just run tests (controller already deployed)
.claude/skills/conformance/run-conformance.sh --skip-build --skip-deploy

# Preview what would be executed
.claude/skills/conformance/run-conformance.sh --dry-run
```

# Building Docker Images

The `build-docker.sh` script handles Docker image building and pushing separately from the full conformance workflow. This is useful when you want to build images without running tests, or when troubleshooting build issues.

## Basic Usage

```bash
.claude/skills/conformance/build-docker.sh
```

## Script Options

| Option | Description |
|--------|-------------|
| `--release` | Use production Dockerfile (higher optimization, slower builds) |
| `--skip-push` | Build images but don't push to registry |
| `--dry-run` | Print commands without executing them |
| `--help` | Show help message |

## What the Script Does

1. **Prerequisites Check**: Verifies Docker is running
2. **Rust Compilation**: Runs `cargo check` to verify the project compiles
3. **Docker Build**: Builds control plane and data plane images using `cargo make docker-build-all`
4. **Push to Registry**: Pushes images to the configured `DOCKER_REGISTRY` using `cargo make do-push-images`

## Example Commands

```bash
# Build and push images (dev mode - faster builds)
.claude/skills/conformance/build-docker.sh

# Build release images (production optimization)
.claude/skills/conformance/build-docker.sh --release

# Build only, don't push to registry
.claude/skills/conformance/build-docker.sh --skip-push

# Preview what would be executed
.claude/skills/conformance/build-docker.sh --dry-run
```

**Note**: The `run-conformance.sh` script calls `build-docker.sh` internally during its build phase. Use `--skip-build` with `run-conformance.sh` to skip this step if images are already built.

# Reporting Results

When reporting test results to the user:

- Provide a summary of tests passed vs failed
- List any specific test cases that failed with their error messages
- Include relevant log excerpts for debugging
- Do not attempt to debug or fix the errors; only report them

Your job is to run the conformance tests and report the results. Do not debug error messages or suggest fixes.

# Error Handling & Troubleshooting

If the script fails to execute, there are a handful of tools that
you may wish to invoke to get conformance testing back on track.

If the script fails, consider these responsibilities:

1. **Cluster Management**: The `cluster-up.sh` and `cluster-down.sh` scripts handle cluster lifecycle
2. **Build Pipeline**: Use `build-docker.sh` to build and push Docker images for the gateway controller
3. **Deployment**: Deploy the gateway controller and all necessary CRDs following the project's established patterns
4. **Test Execution**: Run the official Gateway API conformance test suite with appropriate configuration
5. **Results Analysis**: Retrieve and interpret test logs, identifying failures and their root causes

When encountering issues:
- If `GATEWAY_CONFORMANCE_SUITE` is not set, guide the user to configure it in `.envrc.local`
- If `DOCKER_REGISTRY` is not set, guide the user to configure it in `.envrc.local`
- If kubectl context is wrong, run `cluster-up.sh` to reset the context
- If DigitalOcean cluster creation fails, check doctl authentication and DigitalOcean account quotas
- If image push fails, verify registry authentication (e.g., `docker login ghcr.io`)
- If tests fail, analyze logs for specific failure points but do not suggest fixes
- If deployment fails, check resource definitions and cluster state

**Best Practices**:

1. Always verify kubectl context before running tests to avoid running against production clusters
2. Ensure the DigitalOcean cluster is clean before running tests to avoid state pollution
3. Verify all prerequisites (Docker, doctl, kubectl, Go) are installed and functioning
4. Verify that the Rust project will compile before building Docker images: `cargo check`
5. Check that the gateway controller is fully deployed before running tests
6. Run `cluster-down.sh` when done to delete the cluster and avoid unnecessary costs

**Available Docker Build Commands**:
- `build-docker.sh` - Build and push Docker images (recommended - handles full workflow)
- `build-docker.sh --skip-push` - Build images without pushing to registry
- `cargo make docker-build-all` - Build both control plane and data plane for current platform
- `cargo make docker-build-controlplane` - Build only control plane
- `cargo make docker-build-dataplane` - Build only data plane
- `cargo make docker-build-all-cross` - Build for both amd64 and arm64 (multi-platform)
- `cargo make docker-build-all-push` - Build and push multi-platform images (requires DOCKER_REGISTRY env var)

**Cluster Lifecycle Commands**:
- `cluster-up.sh` - Create or prepare the DigitalOcean cluster
- `cluster-down.sh` - Destroy the DigitalOcean cluster
- `cargo make do-create` - Create a new cluster (called by cluster-up.sh)
- `cargo make do-delete` - Delete the cluster (called by cluster-down.sh)
- `cargo make do-use` - Save kubeconfig for the cluster
