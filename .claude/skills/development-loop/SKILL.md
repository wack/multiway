---
name: development-loop
description: Red-green-refactor development loop for implementing Gateway API conformance tests. Use this skill when working on implementing new conformance tests for the multiway project. It guides the agent through selecting the next test to implement based on priority tiers, running the conformance suite, diagnosing failures, and implementing fixes.
---

# Development Loop for Gateway API Conformance Implementation

You are an expert in implementing Kubernetes Gateway API conformance tests. You follow a disciplined red-green-refactor development loop to systematically implement support for each test case in the official Gateway API conformance suite.

## Overview

This skill guides you through a development loop for implementing conformance tests one at a time. Each iteration of the loop:
1. Selects the highest-priority unimplemented test
2. Verifies the test is currently skipped
3. Enables the test and observes the failure
4. Diagnoses the root cause
5. Implements and verifies the fix
6. Documents the results

**CRITICAL**: All conformance tests MUST be run **locally** using the `conformance` skill's local testing workflow. Never run tests in-cluster during development.

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

## Development Loop Steps

### Step 1: Select the Next Test

Use the `pick-next.sh` helper script to select and enable the next test:

```bash
# See what test is next without enabling it
./pick-next.sh --show-next

# Enable the next test (removes t.Skip() and marks as in-progress)
./pick-next.sh
```

The script will:
1. Scan tier CSV files in priority order (tier-1 first, tier-7 last)
2. Find the first test where `implemented` is `false` or `in-progress`
3. If `false`, enable the test by removing `t.Skip()` from the conformance suite
4. Update the CSV status to `in-progress`

**IMPORTANT**: After running `pick-next.sh`, you MUST inform the user which test was selected by clearly stating:
- The **test name** (e.g., `HTTPRouteSimpleSameNamespace`)
- The **test description** (e.g., "Basic HTTP routing from a route to a backend service in the same namespace")

This ensures the user understands what functionality is being implemented in this iteration.

**Example output to user:**
> The next test to implement is **HTTPRouteSimpleSameNamespace**: Basic HTTP routing from a route to a backend service in the same namespace. This is the foundation of all routing functionality.

### Step 2: Verify Test is Currently Skipped

Before making any code changes, verify the current state:

1. Ensure the `GATEWAY_CONFORMANCE_SUITE` environment variable is set
2. Navigate to `$GATEWAY_CONFORMANCE_SUITE`
3. Use the `conformance` skill to run the conformance suite locally
4. Verify:
   - The selected test is currently **skipped** (not running)
   - All other enabled tests are **passing**

If other tests are failing, stop and address those failures first before enabling a new test.

### Step 3: Enable the Test and Observe Failure

1. Enable the test by removing it from the skip list or adding it to the enabled tests in the conformance configuration
2. Run the conformance suite again using `conformance`
3. Observe and capture the test failure output
4. Document the specific failure message and any relevant stack traces

### Step 4: Handle Test Results

**If the test passes immediately:**
- Update the CSV file to change `implemented` from `in-progress` to `true`
- Document this finding (the feature was already implemented)
- Return to Step 1 to select the next test

**If the test fails:**
- Proceed to Step 5 (Diagnosis)

### Step 5: Diagnose the Failure

#### 5a: Attempt to Create a Unit Test (Recommended)

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

If you cannot successfully create a unit test, proceed to the next step.

#### 5b: Investigate Root Cause

1. Analyze the failure message to identify the failing assertion
2. Trace through the code to understand the request flow:
   - Control plane: How are resources being reconciled?
   - Data plane: How are requests being routed?
3. Identify the specific code paths responsible for the failure
4. Document your findings

#### 5c: File a Bug Report

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

### Step 6: Implement the Fix

1. Make the necessary code changes to fix the identified issue
2. Keep changes minimal and focused on the specific test
3. Follow the project's coding conventions and patterns

### Step 7: Verify the Fix

1. If you created a unit test in Step 5a, run it first:
   ```bash
   cargo nextest run [test_name]
   ```
2. Run the full conformance suite using `conformance`
3. Verify:
   - The previously failing test now **passes**
   - No other tests have regressed

If verification fails, return to Step 5 to continue diagnosis.

### Step 8: Document and Report

Once the test passes:

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

3. Return to Step 1 to continue with the next test

## Running Conformance Tests Locally

Always use the `conformance` skill for running conformance tests. The local testing workflow provides:
- Faster iteration cycles
- Real-time output for debugging
- Direct access to test logs
- Ability to run individual tests

Key commands:
```bash
# Verify environment
echo $GATEWAY_CONFORMANCE_SUITE

# Run conformance tests locally
cd $GATEWAY_CONFORMANCE_SUITE && make conformance
```

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
- **Multiple tests failing**: Address failing tests before enabling new ones
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

### Example Workflow

```bash
# 1. See what test to work on next
./pick-next.sh --show-next

# 2. Enable the test (removes t.Skip() and marks as in-progress)
./pick-next.sh

# 3. Run conformance tests to see the failure
cd $GATEWAY_CONFORMANCE_SUITE && make conformance

# 4. Implement the fix in the multiway codebase

# 5. Verify the fix passes
cd $GATEWAY_CONFORMANCE_SUITE && make conformance

# 6. Manually update the CSV to mark as 'true' when complete
```

## Files and Directories

- `./pick-next.sh`: Helper script for development loop automation
- `./test-tiers/*.csv`: Test priority lists and implementation status
- `./bug-reports/`: Diagnostic reports for failing tests
- `$GATEWAY_CONFORMANCE_SUITE/conformance`: The official conformance test suite
