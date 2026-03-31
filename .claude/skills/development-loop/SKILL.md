---
name: development-loop
description: Red-green-refactor development loop for implementing Gateway API conformance tests. Use this skill when working on implementing new conformance tests for the multiway project. It guides the agent through selecting the next unblocked Linear ticket, creating a Graphite-tracked branch, running the conformance suite in an isolated Kubernetes namespace, diagnosing failures, and implementing fixes.
---

# Development Loop for Gateway API Conformance Implementation

You are an expert in implementing Kubernetes Gateway API conformance tests. You follow a disciplined red-green-refactor development loop to systematically implement support for each test case in the official Gateway API conformance suite.

## Overview

This skill guides you through a development loop for implementing conformance tests one ticket at a time. Each iteration:

1. Selects the next unblocked ticket from Linear
2. Creates a Graphite-tracked branch so Linear auto-transitions the ticket
3. Sets up an isolated namespace on the shared conformance cluster
4. Runs conformance tests and observes failures
5. Diagnoses root causes and implements fixes
6. Tears down the namespace and submits the PR

## Prerequisites

- **Linear MCP**: The Linear MCP tools must be available for querying tickets and checking dependencies
- **Graphite CLI (`gt`)**: Must be installed for branch creation and PR stacking
- **Shared cluster**: The `mw-conformance` DigitalOcean Kubernetes cluster is used for all conformance runs. If it doesn't exist yet, `cluster-up.sh` will create it automatically via `cargo make do-create`.
- **Environment variables** (configured in `.envrc.local`):
  - `GATEWAY_CONFORMANCE_SUITE` — path to the Gateway API repository clone
  - `DOCKER_REGISTRY` — container registry URL (e.g., `ghcr.io/wack`)

## Development Loop Steps

### Step 1: Select the Next Ticket from Linear

Query Linear for the next ticket to work on:

1. Use the Linear MCP `list_issues` tool to fetch issues in the **"MultiWay: API Gateway"** project with state **"Todo"**
2. For each candidate ticket, use `get_issue` with `includeRelations: true` to check its dependency graph
3. **Skip any ticket whose blockers are still open** — examine the `blockedBy` relations and reject tickets where any blocking issue has a status other than "Done" or "Canceled"
4. Select the first unblocked ticket (prefer lower issue numbers, as they represent foundational work that later tickets build upon)

**IMPORTANT**: After selecting a ticket, clearly inform the user:
- The **ticket ID** (e.g., `MULTI-1101`)
- The **ticket title** (e.g., "Tier 1 — Core routing conformance (7 tests)")
- A brief summary of what the ticket covers

**Example output to user:**
> The next ticket to work on is **MULTI-1101**: Tier 1 — Core routing conformance (7 tests). This covers the 7 most essential conformance tests including basic routing, path matching, weighted backends, and listener hostname matching.

If no unblocked "Todo" tickets exist, inform the user and stop.

### Step 2: Create a Branch and Start Work

Every Linear issue has a `gitBranchName` field (e.g., `robbie/multi-1101`). Use this for the branch name so that Linear's GitHub integration can automatically track the ticket's lifecycle.

1. **Create the branch with Graphite** so PRs can be stacked:
   ```bash
   gt create <gitBranchName>
   ```
   This creates a new branch tracked by Graphite, branched from the current stack.

2. **Push the branch to the remote** so Linear detects it and auto-transitions the ticket to "In Progress":
   ```bash
   git push -u origin <gitBranchName>
   ```

3. **Confirm the transition in Linear** — as a safety net, also update the ticket status via the Linear MCP:
   ```
   save_issue(id: "MULTI-XXXX", state: "In Progress")
   ```

### Step 3: Set Up the Conformance Namespace

The project uses a single shared cluster (`mw-conformance`) instead of spinning up a new cluster for each ticket. Each ticket gets its own namespace for isolation, so multiple agents can run conformance suites in parallel without conflicting.

The namespace name is the **lowercased ticket ID** (e.g., `multi-1101`). Kubernetes namespaces must be lowercase.

1. **Switch to the conformance cluster context**:
   ```bash
   doctl kubernetes cluster kubeconfig save mw-conformance
   ```

2. **Verify the cluster is accessible**:
   ```bash
   kubectl get nodes
   ```
   If this fails, the cluster may not exist yet. The `cluster-up.sh` script handles this automatically — it will create the cluster if it's missing.

3. **Create a namespace for this ticket**:
   ```bash
   kubectl create namespace <ticket-id-lowercase>
   ```
   If the namespace already exists (e.g., from a previous attempt), delete it first to ensure a clean slate:
   ```bash
   kubectl delete namespace <ticket-id-lowercase> --wait=true --ignore-not-found
   kubectl create namespace <ticket-id-lowercase>
   ```

4. **Install/update Gateway API CRDs** (idempotent, safe to run every time):
   ```bash
   cargo make gateway-api-install
   ```

### Step 4: Build, Deploy, and Run Conformance Tests

Use the `gateway-conformance-runner` skill's `run-conformance.sh` script. It accepts `--namespace` to target your ticket's namespace and `--cluster-name` to specify the shared cluster.

**First run** (builds images, deploys, and runs tests):
```bash
.claude/skills/gateway-conformance-runner/run-conformance.sh \
  --cluster-name mw-conformance \
  --namespace <ticket-id-lowercase>
```

**Subsequent runs** (skip the build if code hasn't changed):
```bash
.claude/skills/gateway-conformance-runner/run-conformance.sh \
  --cluster-name mw-conformance \
  --namespace <ticket-id-lowercase> \
  --skip-build
```

**Test-only re-runs** (controller is already deployed):
```bash
.claude/skills/gateway-conformance-runner/run-conformance.sh \
  --cluster-name mw-conformance \
  --namespace <ticket-id-lowercase> \
  --skip-build --skip-deploy
```

### Step 5: Handle Test Results

**If the target tests pass immediately:**
- The feature was already implemented — document this finding
- Proceed to Step 8 (Clean Up and Report)

**If tests fail:**
- Proceed to Step 6 (Diagnosis)

### Step 6: Diagnose the Failure

#### 6a: Create a Unit Test First (Recommended)

Before modifying production code, try to reproduce the failure as a fast, purely functional unit test. The multiway project follows a sans-I/O architecture, so most behavior can be tested without a cluster.

1. Study the conformance test implementation in `$GATEWAY_CONFORMANCE_SUITE/conformance`
2. Create a unit test using this project's patterns:
   - Use `WorldSnapshotBuilder` to set up cluster state
   - Call pure reconciliation functions (`reconcile_gateway`, `reconcile_httproute`, etc.)
   - Assert on the returned `ReconcileResult`

Unit tests run in milliseconds (no cluster, no async), which dramatically speeds up the fix-verify cycle.

#### 6b: Investigate Root Cause

1. Analyze the failure message to identify the failing assertion
2. Trace through the code:
   - **Control plane**: Is the ConfigMap being generated correctly?
   - **Data plane**: Is the proxy routing requests correctly?
3. Identify the specific code paths responsible

#### 6c: File a Bug Report

Create a Markdown file in `.claude/skills/development-loop/bug-reports/` documenting:

```markdown
# Bug Report: [Test Name]

## Test Description
[What the conformance test is validating]

## Failure Message
[The exact error message from the conformance test]

## Root Cause Analysis
[Your findings about why the test is failing]

## Affected Code
- Control plane: [relevant files/functions]
- Data plane: [relevant files/functions]

## Proposed Fix
[Your plan to address the issue]
```

### Step 7: Implement and Verify the Fix

1. Make the necessary code changes — keep them minimal and focused
2. Follow the project's coding conventions (see CLAUDE.md)
3. If you created a unit test in Step 6a, run it first for fast feedback:
   ```bash
   cargo nextest run <test_name>
   ```
4. Run the full conformance suite again:
   ```bash
   .claude/skills/gateway-conformance-runner/run-conformance.sh \
     --cluster-name mw-conformance \
     --namespace <ticket-id-lowercase> \
     --skip-build
   ```
5. Verify:
   - The previously failing tests now **pass**
   - No other tests have **regressed**

If verification fails, return to Step 6 to continue diagnosis.

### Step 8: Clean Up and Report

Once all tests for the ticket pass:

1. **Run formatting and linting** to ensure code quality:
   ```bash
   cargo make fmt
   cargo make clippy-flow
   ```

2. **Tear down the namespace** to free cluster resources and avoid conflicts:
   ```bash
   kubectl delete namespace <ticket-id-lowercase> --wait=true
   ```
   This removes all resources (deployments, services, configmaps, etc.) created for this ticket. Always do this, even if the ticket isn't fully complete.

3. **Commit your changes and submit a PR** via Graphite:
   ```bash
   gt submit
   ```
   When the PR is merged, Linear's GitHub integration will automatically transition the ticket to "Done".

4. **Check in about cluster teardown** — after the PR is submitted, ask the user whether they'd like to tear down the shared `mw-conformance` cluster to save on DigitalOcean costs. If the user says yes, run:
   ```bash
   .claude/skills/gateway-conformance-runner/cluster-down.sh --destroy-cluster
   ```
   This destroys the DigitalOcean cluster **and** removes its context, cluster entry, and user entry from the local kubeconfig, so it no longer appears in `kubectl config get-contexts`. The cluster can be recreated automatically by `cluster-up.sh` on the next run.

   If the user says no (or wants to keep running more tickets), the cluster stays up and you can continue to Step 6.

5. **Create a summary report**:

```markdown
## Ticket Completed: [MULTI-XXXX] [Ticket Title]

### Summary
[Brief description of what was implemented]

### Changes Made
- `path/to/file1.rs`: [description of changes]
- `path/to/file2.rs`: [description of changes]

### Unit Tests Added
[Yes/No - if yes, describe the test]

### Lessons Learned
[Any insights that might help with future tickets]
```

6. Return to Step 1 to continue with the next ticket.

## Best Practices

1. **One ticket at a time**: Focus on a single ticket per iteration
2. **Namespace isolation**: Always work in a ticket-specific namespace to avoid conflicts with other agents
3. **Minimal changes**: Make the smallest change needed to pass the tests
4. **Unit tests preferred**: Local unit tests (milliseconds) are far faster than conformance runs (minutes)
5. **No regressions**: Ensure all previously passing tests continue to pass
6. **Always clean up**: Delete the namespace when done, even if you're abandoning the ticket
7. **Document everything**: Bug reports and summaries help future iterations

## Error Recovery

- **Wrong kubectl context**: Run `doctl kubernetes cluster kubeconfig save mw-conformance`
- **Cluster not found**: The `cluster-up.sh` script will auto-create the cluster via `cargo make do-create`. If that fails, check DigitalOcean authentication (`doctl auth init`).
- **Namespace conflicts**: If a namespace already exists from a previous attempt, delete it first:
  ```bash
  kubectl delete namespace <ticket-id-lowercase> --wait=true --ignore-not-found
  ```
- **Stuck on a ticket**: Document findings in a bug report, commit work-in-progress, tear down the namespace, and move to the next unblocked ticket
- **No unblocked tickets**: Inform the user — all remaining "Todo" tickets are blocked by open dependencies
- **Build failures**: Run `cargo check` first to catch compilation errors before building Docker images

## Files and Directories

- `./bug-reports/`: Diagnostic reports for failing tests
- `$GATEWAY_CONFORMANCE_SUITE/conformance`: The official conformance test suite
