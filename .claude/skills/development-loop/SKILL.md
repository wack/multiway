---
name: development-loop
description: Red-green-refactor development loop for implementing Gateway API conformance tests. Use this skill when working on implementing new conformance tests for the multiway project. It guides the agent through selecting the next test to implement based on priority tiers, running the conformance suite, diagnosing failures, and implementing fixes.
---

# Development Loop for Gateway API Conformance Implementation

You are an expert in implementing Kubernetes Gateway API conformance tests. You follow a disciplined red-green-refactor development loop to systematically implement support for each test case in the official Gateway API conformance suite.

## Overview

This skill guides you through a task-based workflow for implementing conformance tests one at a time. The workflow consists of the following tasks:

1. **Provision test cluster** - Bring up a Kubernetes cluster
2. **Run conformance tests** - Verify current state of the test suite
3. **Resolve failing tests** (if any) - Fix outstanding failures before proceeding
4. **Select the next test** - Pick the highest-priority unimplemented test
5. **Verify test is skipped** - Confirm the test is not yet running
6. **Enable test and observe failure** - Activate the test and capture the failure
7. **Handle test results** - Branch based on pass/fail
8. **Diagnose the failure** - Investigate root cause
9. **Implement the fix** - Make code changes
10. **Verify the fix** - Run conformance tests to confirm
11. **Document and report** - Record the results
12. **Tear down cluster** - Clean up resources when done

**CRITICAL**: All conformance tests MUST be run using the `conformance` skill. Never run tests in-cluster during development.

**IMPORTANT**: Conformance tests MUST be run on a DigitalOcean Kubernetes cluster provisioned by `cluster-up.sh`. Local clusters (Kind, minikube, etc.) are NOT suitable for conformance testing. Always start by running `cluster-up.sh` even if you see another kubectl context configured.

## Test Priority Tiers

Test cases have been prioritized into 7 tiers, stored in CSV files within this skill's directory:

| File | Priority | Description |
|------|----------|-------------|
| `test-tiers/tier-1-essential.csv` | Highest | Core functionality that must work |
| `test-tiers/tier-2-important-http.csv` | High | Important HTTP routing features |
| `test-tiers/tier-3-production.csv` | Medium-High | Production-ready features |
| `test-tiers/tier-4-advanced.csv` | Medium | Advanced routing capabilities |
| `test-tiers/tier-5-observability.csv` | Medium-Low | Observability features |
| `test-tiers/tier-6-validation.csv` | Low | Validation and edge cases |
| `test-tiers/tier-7-not-relevant.csv` | Lowest | Tests not relevant to this implementation |

Each CSV file has the following columns:
- `test_name`: The name of the conformance test
- `description`: A brief description of what the test validates
- `implemented`: Status - `false`, `in-progress`, or `true`

## Workflow Tasks

### Task 1: Provision Test Cluster

Before starting the development loop, you must ensure a Kubernetes cluster is available for running conformance tests.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Provision test cluster"
  description: "Execute the cluster-up.sh script located at .claude/skills/development-loop/cluster-up.sh to provision a DigitalOcean Kubernetes cluster for conformance testing. This script handles cluster creation, kubeconfig setup, and installs required Gateway API CRDs."
  activeForm: "Provisioning test cluster"
```

**Execute the script:**

```bash
.claude/skills/development-loop/cluster-up.sh
```

The script will:
1. Create a DigitalOcean Kubernetes cluster (or reuse an existing one)
2. Configure kubectl context
3. Install Gateway API CRDs
4. Verify the cluster is ready for testing

Once the cluster is provisioned, mark the task as completed and proceed to Task 2.

### Task 2: Run Conformance Tests

After the cluster is available, run the conformance test suite to establish the current state.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Run conformance tests"
  description: "Use the conformance skill to build and load docker images, then run the Gateway API conformance test suite. This establishes the baseline state of passing and failing tests."
  activeForm: "Running conformance tests"
```

**Execute the conformance skill:**

Use the `conformance` skill to run the test suite. This skill will:
1. Build the docker images
2. Load the images into the cluster
3. Run the conformance tests
4. Report results

**Evaluate the results:**
- If **all enabled tests pass**: Proceed to Task 4 (Select the next test)
- If **any tests are failing**: Proceed to Task 3 (Resolve failing tests)

### Task 3: Resolve Failing Tests

If Task 2 revealed failing tests, they must be resolved before enabling any new tests.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Resolve failing tests"
  description: "Fix the failing conformance tests identified in the previous run. All existing tests must pass before enabling new tests. Diagnose each failure, implement fixes, and verify with the conformance skill."
  activeForm: "Resolving failing tests"
```

**For each failing test:**
1. Diagnose the failure (see Task 8 for diagnosis techniques)
2. Implement the fix (see Task 9)
3. Run the `conformance` skill again to verify
4. Repeat until all tests pass

Once all tests pass, mark this task as completed and proceed to Task 4.

### Task 4: Select the Next Test

Use the `pick-next.sh` helper script to select and enable the next test.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Select the next test"
  description: "Run pick-next.sh to identify and enable the highest-priority unimplemented test from the tier CSV files."
  activeForm: "Selecting next test"
```

**Execute the script:**

```bash
# See what test is next without enabling it
.claude/skills/development-loop/pick-next.sh --show-next

# Enable the next test (removes t.Skip() and marks as in-progress)
.claude/skills/development-loop/pick-next.sh
```

The script will:
1. Scan tier CSV files in priority order (tier-1 first, tier-7 last)
2. Find the first test where `implemented` is `false` or `in-progress`
3. If `false`, enable the test by removing `t.Skip()` from the conformance suite
4. Update the CSV status to `in-progress`

**IMPORTANT**: After running `pick-next.sh`, you MUST inform the user which test was selected by clearly stating:
- The **test name** (e.g., `HTTPRouteSimpleSameNamespace`)
- The **test description** (e.g., "Basic HTTP routing from a route to a backend service in the same namespace")

**Example output to user:**
> The next test to implement is **HTTPRouteSimpleSameNamespace**: Basic HTTP routing from a route to a backend service in the same namespace. This is the foundation of all routing functionality.

### Task 5: Verify Test is Currently Skipped

Before making any code changes, verify the current state.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Verify test is currently skipped"
  description: "Run the conformance skill to confirm that the selected test is currently skipped (not running) and that all other enabled tests are passing."
  activeForm: "Verifying test is skipped"
```

**Verification steps:**
1. Ensure the `GATEWAY_CONFORMANCE_SUITE` environment variable is set
2. Use the `conformance` skill to run the conformance suite
3. Verify:
   - The selected test is currently **skipped** (not running)
   - All other enabled tests are **passing**

If other tests are failing, return to Task 3 to resolve them before proceeding.

### Task 6: Enable Test and Observe Failure

Enable the test and capture the failure output.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Enable test and observe failure"
  description: "Enable the selected test by removing it from the skip list, run the conformance suite, and capture the failure output for diagnosis."
  activeForm: "Enabling test and observing failure"
```

**Execution:**
1. Enable the test by removing it from the skip list or adding it to the enabled tests
2. Run the conformance suite using the `conformance` skill
3. Observe and capture the test failure output
4. Document the specific failure message and any relevant stack traces

### Task 7: Handle Test Results

Evaluate the test results and determine next action.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Handle test results"
  description: "Evaluate whether the newly enabled test passed or failed, and determine the next action in the workflow."
  activeForm: "Handling test results"
```

**If the test passes immediately:**
- Update the CSV file to change `implemented` from `in-progress` to `true`
- Document this finding (the feature was already implemented)
- Return to Task 4 to select the next test

**If the test fails:**
- Proceed to Task 8 (Diagnose the failure)

### Task 8: Diagnose the Failure

Investigate the root cause of the test failure.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Diagnose the failure"
  description: "Investigate the root cause of the test failure through unit test creation, code analysis, and bug report documentation."
  activeForm: "Diagnosing failure"
```

#### 8a: Attempt to Create a Unit Test (Recommended)

Before diving into the implementation, try to recreate the conformance test as a purely functional unit test within this repository:

1. Study the conformance test implementation in `$GATEWAY_CONFORMANCE_SUITE/conformance`
2. Understand what scenario the test is validating
3. Create a unit test using this project's testing patterns:
   - Use `snapshot` semantics for expected outputs
   - Use `world state` semantics for modeling the reconciler
   - Implement as a purely functional controller test

Having a local unit test provides:
- Faster iteration cycles
- Easier debugging
- Better test isolation
- Documentation of the expected behavior

If you cannot successfully create a unit test, proceed to the next sub-task.

#### 8b: Investigate Root Cause

1. Analyze the failure message to identify the failing assertion
2. Trace through the code to understand the request flow:
   - Control plane: How are resources being reconciled?
   - Data plane: How are requests being routed?
3. Identify the specific code paths responsible for the failure
4. Document your findings

#### 8c: File a Bug Report

Create a Markdown file in `./bug-reports/` documenting:

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

### Task 9: Implement the Fix

Make the necessary code changes to address the failure.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Implement the fix"
  description: "Make the necessary code changes to fix the identified issue, keeping changes minimal and focused on the specific test."
  activeForm: "Implementing fix"
```

**Guidelines:**
1. Make the necessary code changes to fix the identified issue
2. Keep changes minimal and focused on the specific test
3. Follow the project's coding conventions and patterns

### Task 10: Verify the Fix

Confirm that the fix resolves the test failure.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Verify the fix"
  description: "Run unit tests and the full conformance suite to verify that the fix works and no regressions were introduced."
  activeForm: "Verifying fix"
```

**Verification steps:**
1. If you created a unit test in Task 8a, run it first:
   ```bash
   cargo nextest run [test_name]
   ```
2. Run the full conformance suite using the `conformance` skill
3. Verify:
   - The previously failing test now **passes**
   - No other tests have regressed

**If verification fails:** Return to Task 8 to continue diagnosis.

**If verification succeeds:** Proceed to Task 11.

### Task 11: Document and Report

Record the results of the completed test implementation.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Document and report"
  description: "Update the CSV status to mark the test as implemented, and create a summary report documenting the changes made."
  activeForm: "Documenting results"
```

**Documentation steps:**

1. Update the CSV file to change `implemented` from `in-progress` to `true`

2. Create a summary report with the following format:

```markdown
## Test Completed: [Test Name]

### Summary
[Brief description of what was implemented]

### Changes Made

**Before:**
[Code or behavior before the fix]

**After:**
[Code or behavior after the fix]

### Files Modified
- `path/to/file1.rs`: [description of changes]
- `path/to/file2.rs`: [description of changes]

### Unit Test Added
[Yes/No - if yes, describe the test]

### Lessons Learned
[Any insights that might help with future tests]
```

3. Return to Task 4 to continue with the next test

### Task 12: Tear Down Cluster

When the development session is complete, clean up the cluster resources.

**Create a task using the `TaskCreate` tool:**

```
TaskCreate:
  subject: "Tear down cluster"
  description: "Execute the cluster-down.sh script to destroy the DigitalOcean Kubernetes cluster and avoid ongoing costs."
  activeForm: "Tearing down cluster"
```

**Execute the script:**

```bash
.claude/skills/development-loop/cluster-down.sh
```

The script will:
1. Delete the DigitalOcean Kubernetes cluster
2. Clean up associated resources
3. Remove the kubeconfig context

**IMPORTANT**: Only run this task when you are finished with the development session. The cluster takes time to provision, so tearing it down prematurely will slow down future work.

## Best Practices

1. **One test at a time**: Focus on a single test per iteration
2. **Verify first**: Always confirm the test is skipped before enabling
3. **Minimal changes**: Make the smallest change needed to pass the test
4. **Document everything**: Keep thorough records in bug reports and summaries
5. **Unit tests preferred**: Local unit tests make debugging much faster
6. **No regressions**: Ensure all previously passing tests continue to pass

## Error Recovery

If you encounter issues:
- **Wrong kubectl context**: Stop immediately, switch to the correct context
- **Conformance suite not found**: Verify `GATEWAY_CONFORMANCE_SUITE` is set correctly
- **Multiple tests failing**: Address failing tests before enabling new ones (Task 3)
- **Stuck on a test**: Document findings, mark as `in-progress`, and consider moving to the next test with a note

## Helper Script: pick-next.sh

A helper script is provided to automate common development loop tasks:

```bash
# Location
.claude/skills/development-loop/pick-next.sh
```

### Script Features

The `pick-next.sh` script automates:
1. **CSV Concatenation**: Combines all tier files in priority order (tier-1 first)
2. **Next Test Selection**: Finds the first test with status `in-progress` or `false`
3. **Test Enabling**: Uses AST-Grep to remove `t.Skip()` calls from conformance tests

### Usage

```bash
# Show the next test to work on
./pick-next.sh --show-next

# Enable the next test (removes t.Skip() and updates CSV to in-progress)
./pick-next.sh

# Preview what would be done without making changes
./pick-next.sh --dry-run

# List all tests in priority order with their status
./pick-next.sh --list-all

# Show help
./pick-next.sh --help
```

### Requirements

- **GATEWAY_CONFORMANCE_SUITE**: Environment variable pointing to the Gateway API repository clone
- **ast-grep** (optional): The script will install it via cargo if not available, or fall back to sed

## Files and Directories

- `./cluster-up.sh`: Provisions a DigitalOcean Kubernetes cluster for testing
- `./cluster-down.sh`: Tears down the test cluster to avoid ongoing costs
- `./pick-next.sh`: Helper script for development loop automation
- `./test-tiers/*.csv`: Test priority lists and implementation status
- `./bug-reports/`: Diagnostic reports for failing tests
- `$GATEWAY_CONFORMANCE_SUITE/conformance`: The official conformance test suite

## Cluster Lifecycle

- **Cluster Setup** (`cluster-up.sh`): Run at the start of a development session. Creates the DigitalOcean Kubernetes cluster if it doesn't exist, or clears the namespace if it does. Ensures kubectl context is properly configured.
- **Cluster Cleanup** (`cluster-down.sh`): Run manually when you want to destroy the cluster. This is NOT run automatically - you must run it yourself when done to avoid unnecessary costs.

### Cluster Naming

The cluster name defaults to a sanitized version of the current git branch, prefixed with `mw-`. For example:
- Branch `main` → cluster `mw-main`
- Branch `feature/my-test` → cluster `mw-feature-my-test`
- Branch `claude/migrate-kind-to-digitalocean-Ytxrr` → cluster `mw-claude-migrate-kind-to-digitalocean-ytxrr`

This allows multiple developers or branches to have isolated clusters without conflicts.

You can override the cluster name with `--cluster-name` or the `DO_CLUSTER_NAME` environment variable.

### Example Commands

```bash
# Start or prepare the cluster for current branch
.claude/skills/development-loop/cluster-up.sh

# Use a specific cluster name
.claude/skills/development-loop/cluster-up.sh --cluster-name my-cluster

# Destroy the cluster when done
.claude/skills/development-loop/cluster-down.sh
```
