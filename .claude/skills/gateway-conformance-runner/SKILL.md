---
name: gateway-conformance-runner
description: Use this skill when you need to run the Gateway API conformance test suite for the multiway project. It includes setting up a DigitalOcean Kubernetes cluster, building and deploying the gateway controller, running the official conformance tests, and analyzing the results. The skill handles the complete workflow from cluster creation to test execution and log retrieval.
hooks:
  pre: .claude/skills/gateway-conformance-runner/cluster-up.sh
  post: .claude/skills/gateway-conformance-runner/cluster-down.sh
---

You are responsible for running the Gateway API conformance test suite for the multiway project and reporting the results.

# Cluster Lifecycle

This skill includes automatic cluster management through hooks:

- **Startup Hook** (`cluster-up.sh`): Runs before the skill starts. Creates the DigitalOcean Kubernetes cluster if it doesn't exist, or clears the namespace if it does. Ensures kubectl context is properly configured.
- **Shutdown Hook** (`cluster-down.sh`): Runs after the skill completes. Destroys the DigitalOcean cluster and cleans up kubectl context to avoid unnecessary costs.

You can also run these scripts manually:

```bash
# Start or prepare the cluster
.claude/skills/gateway-conformance-runner/cluster-up.sh

# Destroy the cluster when done
.claude/skills/gateway-conformance-runner/cluster-down.sh
```

# Running Conformance Tests

Use the automated script located at `.claude/skills/gateway-conformance-runner/run-conformance.sh` to run the conformance tests.

**Note**: The cluster must be running before executing this script. If using the skill hooks, the cluster is started automatically. If running manually, use `cluster-up.sh` first.

## Basic Usage

```bash
.claude/skills/gateway-conformance-runner/run-conformance.sh
```

## Script Options

| Option | Description |
|--------|-------------|
| `--skip-build` | Skip Rust compilation and Docker image building (use when images already exist) |
| `--skip-deploy` | Skip gateway controller deployment (use when controller is already running) |
| `--dry-run` | Print commands without executing them |
| `--help` | Show help message |

## What the Script Does

The script automates the conformance testing workflow (cluster is managed separately):

1. **Prerequisites Check**: Verifies Docker and kubectl are available, cluster is accessible
2. **Environment Verification**: Checks that `GATEWAY_CONFORMANCE_SUITE` and `DO_REGISTRY` environment variables are set
3. **Build & Push**: Compiles the Rust project, builds Docker images, and pushes them to DigitalOcean Container Registry
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
7. **`DO_REGION`** (optional) environment variable for the cluster region (default: `nyc1`):
   ```bash
   export DO_REGION=nyc1
   ```

## Example Commands

```bash
# Full test run (build, deploy, test)
.claude/skills/gateway-conformance-runner/run-conformance.sh

# Quick re-test (skip build, images already exist)
.claude/skills/gateway-conformance-runner/run-conformance.sh --skip-build

# Just run tests (controller already deployed)
.claude/skills/gateway-conformance-runner/run-conformance.sh --skip-build --skip-deploy

# Preview what would be executed
.claude/skills/gateway-conformance-runner/run-conformance.sh --dry-run
```

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
2. **Build Pipeline**: Build Docker images for the gateway controller and ensure they're properly pushed to DigitalOcean Container Registry
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
5. Use `cargo make conformance-cleanup` between test runs to ensure clean state (in-cluster only)
6. Check that the gateway controller is fully deployed before running tests
7. The shutdown hook will delete the cluster automatically; if running manually, use `cluster-down.sh`

**Available Docker Build Commands**:
The project provides several cargo make tasks for building Docker images:
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
