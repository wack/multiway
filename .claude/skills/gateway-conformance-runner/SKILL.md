---
name: gateway-conformance-runner
description: Use this skill when you need to run the Gateway API conformance test suite for the multiway project. It includes setting up a Kind cluster, building and deploying the gateway controller, running the official conformance tests, and analyzing the results. The skill handles the complete workflow from cluster creation to test execution and log retrieval. Examples:\n\n<example>\nContext: The user wants to verify their Gateway API implementation meets conformance standards.\nuser: "I need to run the conformance tests for our gateway"\nassistant: "I'll use the gateway-conformance-runner agent to set up the environment and run the full conformance test suite."\n<commentary>\nSince the user wants to run conformance tests, use the Task tool to launch the gateway-conformance-runner agent to handle the complete testing workflow.\n</commentary>\n</example>\n\n<example>\nContext: The user has made changes to the gateway controller and wants to verify conformance.\nuser: "Can you check if our gateway still passes the conformance tests after my latest changes?"\nassistant: "Let me use the gateway-conformance-runner agent to run the full conformance test suite and check the results."\n<commentary>\nThe user needs to verify conformance after changes, so use the gateway-conformance-runner agent to execute the tests.\n</commentary>\n</example>\n\n<example>\nContext: The user is debugging a failed conformance test.\nuser: "The HTTPRoute tests are failing, can you run them and show me the logs?"\nassistant: "I'll use the gateway-conformance-runner agent to run the conformance tests and retrieve the detailed logs for analysis."\n<commentary>\nSince the user needs to debug conformance test failures, use the gateway-conformance-runner agent to run tests and get logs.\n</commentary>\n</example>
---

You are responsible for running the Gateway API conformance test suite for the multiway project and reporting the results.

# Running Conformance Tests

Use the automated script located at `.claude/skills/gateway-conformance-runner/run-conformance.sh` to run the conformance tests.

## Basic Usage

```bash
.claude/skills/gateway-conformance-runner/run-conformance.sh
```

## Script Options

| Option | Description |
|--------|-------------|
| `--skip-build` | Skip Rust compilation and Docker image building (use when images already exist) |
| `--skip-deploy` | Skip gateway controller deployment (use when controller is already running) |
| `--cluster-name NAME` | Specify Kind cluster name (default: `multiway-local`) |
| `--dry-run` | Print commands without executing them |
| `--help` | Show help message |

## What the Script Does

The script automates the complete local conformance testing workflow:

1. **Prerequisites Check**: Verifies Docker, kubectl, and Kind are available
2. **Environment Verification**: Checks that `GATEWAY_CONFORMANCE_SUITE` environment variable is set and points to a valid Gateway API repository clone
3. **Cluster Setup**: Creates a Kind cluster if missing, switches kubectl context if needed
4. **Build & Load**: Compiles the Rust project, builds Docker images, and loads them into Kind
5. **Deploy**: Cleans up any existing deployments, installs Gateway API CRDs, creates a fresh namespace, deploys the gateway controller, and waits for pods to be ready
6. **Test Execution**: Runs the conformance tests from the local Gateway API repository

## Recovery Behavior

The script automatically recovers from common issues:

- **Missing Kind cluster**: Creates a new cluster
- **Wrong kubectl context**: Switches to the correct context
- **Existing deployments**: Deletes the namespace to ensure a clean state

Non-recoverable errors (missing tools, compilation failures, etc.) will cause the script to exit with a descriptive error message.

## Prerequisites

Before running the script, ensure:

1. **Docker** is installed and running
2. **kubectl** is installed
3. **Kind** is installed
4. **Gateway API repository** is cloned locally
5. **`GATEWAY_CONFORMANCE_SUITE`** environment variable is set in `.envrc.local`:
   ```bash
   export GATEWAY_CONFORMANCE_SUITE=/path/to/gateway-api
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

# **Error Handling & Troubleshooting**:

If the script fails to execute, there are a handful of tools that
you may wish to invoke to get conformance testing back on track.

If the script fails, consider these responsibilities of the script:
1. **Cluster Management**: Create and configure Kind clusters for testing, ensuring proper context switching and cluster readiness
2. **Build Pipeline**: Build Docker images for the gateway controller and ensure they're properly loaded into the Kind cluster
3. **Deployment**: Deploy the gateway controller and all necessary CRDs following the project's established patterns
4. **Test Execution**: Run the official Gateway API conformance test suite with appropriate configuration
5. **Results Analysis**: Retrieve and interpret test logs, identifying failures and their root causes


When encountering issues:
- If `GATEWAY_CONFORMANCE_SUITE` is not set, guide the user to configure it in `.envrc.local`
- If kubectl context is wrong, stop immediately and ask the user to verify their intended cluster
- If Kind cluster creation fails, check Docker daemon status and available resources
- If image build fails, verify Dockerfile syntax and dependencies
- If tests fail, analyze logs for specific failure points but do not suggest fixes
- If deployment fails, check resource definitions and cluster state

**Best Practices**:

1. Always verify kubectl context before running tests to avoid running against production clusters
2. Ensure the Kind cluster is clean before running tests to avoid state pollution
3. Verify all prerequisites (Docker, Kind, kubectl, Go) are installed and functioning
4. Verify that the Rust project will compile before building Docker images: `cargo check`
5. Use `cargo make conformance-cleanup` between test runs to ensure clean state (in-cluster only)
6. Check that the gateway controller is fully deployed before running tests
7. For local testing, ensure you're running from within the Gateway API repository conformance directory

**Available Docker Build Commands**:
The project provides several cargo make tasks for building Docker images:
- `cargo make docker-build-all` - Build both control plane and data plane for current platform
- `cargo make docker-build-controlplane` - Build only control plane
- `cargo make docker-build-dataplane` - Build only data plane
- `cargo make docker-build-all-cross` - Build for both amd64 and arm64 (multi-platform)
- `cargo make docker-build-all-push` - Build and push multi-platform images (requires DOCKER_REGISTRY env var)
