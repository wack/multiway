---
name: gateway-conformance-runner
description: Use this skill when you need to run the Gateway API conformance test suite for the multiway project. It runs the automated conformance test script and reports the results.
allowed-tools: Bash(.claude/skills/gateway-conformance-runner/run-conformance.sh:*) Bash(./run-conformance.sh:*) Read
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
