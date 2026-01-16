#!/usr/bin/env bash
################################################################################
# Development Loop Helper Script
#
# This script automates the process of selecting and enabling the next
# conformance test from the prioritized tier CSV files.
#
# Dependencies:
#   - ast-grep (for Go code manipulation): cargo install ast-grep-cli
#   - Standard Unix tools: awk, sed, grep, tail
#
# Environment Variables:
#   - GATEWAY_CONFORMANCE_SUITE: Path to the gateway-api repository clone
#
# Exit Codes:
#   0 - Success (test found and processed, or all tests complete)
#   1 - Error (missing dependencies, invalid paths, etc.)
################################################################################

set -euo pipefail

################################################################################
# Constants and Configuration
################################################################################

# ANSI color codes for terminal output
readonly RED='\033[0;31m'
readonly GREEN='\033[0;32m'
readonly YELLOW='\033[1;33m'
readonly BLUE='\033[0;34m'
readonly NC='\033[0m' # No Color

# Directory paths
readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly TIERS_DIR="${SCRIPT_DIR}/test-tiers"

################################################################################
# Text Transformation Functions
################################################################################

# Convert PascalCase test name to kebab-case filename
#
# This function transforms Gateway API test names (which use PascalCase) into
# the corresponding Go source file names (which use kebab-case).
#
# Arguments:
#   $1 - PascalCase test name (e.g., "HTTPRouteSimpleSameNamespace")
#
# Output:
#   Kebab-case filename (e.g., "httproute-simple-same-namespace")
#
# Examples:
#   HTTPRouteSimpleSameNamespace -> httproute-simple-same-namespace
#   GatewayWithAttachedRoutes    -> gateway-with-attached-routes
#   TLSRouteSimple               -> tlsroute-simple
to_kebab_case() {
    local input="$1"

    # Insert hyphen between lowercase/digit and uppercase
    # Example: "route1Simple" -> "route1-Simple"
    local step1
    step1=$(echo "${input}" | sed 's/\([a-z0-9]\)\([A-Z]\)/\1-\2/g')

    # Insert hyphen between uppercase letters when followed by lowercase
    # Example: "HTTPRoute" -> "HTTP-Route"
    local step2
    step2=$(echo "${step1}" | sed 's/\([A-Z]\)\([A-Z][a-z]\)/\1-\2/g')

    # Convert entire string to lowercase
    echo "${step2}" | tr '[:upper:]' '[:lower:]'
}

################################################################################
# CSV Processing Functions
################################################################################

# Find the next test to implement from tier CSV files
#
# This function scans tier CSV files in priority order (tier-1 through tier-7)
# and returns the first test with status "false" or "in-progress". Tests with
# status "true" are considered complete and are skipped.
#
# The function processes tiers sequentially, ensuring higher-priority tests
# are always selected before lower-priority ones.
#
# CSV Format:
#   test_name,description,implemented
#   HTTPRouteMatching,Path and header matching...,false
#
# Output:
#   Pipe-separated string: "test_name|description|status"
#   Example: "HTTPRouteMatching|Path and header matching...|false"
#
# Returns:
#   0 - Test found (output written to stdout)
#   1 - No eligible test found (all tests complete)
find_next_test() {
    local tier_file
    local test_name
    local description
    local status

    # Process tiers 1-7 in priority order
    # Tier 1 = highest priority (essential functionality)
    # Tier 7 = lowest priority (not relevant to implementation)
    for tier in {1..7}; do
        # Find the tier file (handles different suffixes like tier-1-essential.csv)
        # Use head to get first match if multiple files exist
        tier_file=$(ls "${TIERS_DIR}"/tier-${tier}-*.csv 2>/dev/null | head -n1)

        # Skip this tier if no file found
        if [[ -z "${tier_file}" ]]; then
            echo -e "${YELLOW}Warning: No file found for tier ${tier}${NC}" >&2
            continue
        fi

        # Read CSV file line by line, skipping the header row
        # IFS=, sets the field separator to comma for CSV parsing
        # tail -n +2 skips the first line (header)
        while IFS=, read -r test_name description status; do
            # Trim leading/trailing whitespace from status field
            # This handles cases like "false " or " in-progress"
            status=$(echo "${status}" | xargs)

            # Check if this test needs work
            # "false" = not started, "in-progress" = work in progress
            if [[ "${status}" == "false" ]] || [[ "${status}" == "in-progress" ]]; then
                # Return test information as pipe-separated string
                # Using pipe separator to avoid issues with commas in descriptions
                echo "${test_name}|${description}|${status}"
                return 0
            fi
        done < <(tail -n +2 "${tier_file}")
    done

    # No eligible test found - all tests are complete
    return 1
}

################################################################################
# File Location Functions
################################################################################

# Find the test file in the Gateway API conformance suite
#
# This function locates the Go source file for a given test name by:
# 1. Converting the PascalCase test name to kebab-case filename
# 2. Searching in the main conformance/tests/ directory
# 3. Falling back to the conformance/tests/mesh/ subdirectory
#
# Arguments:
#   $1 - PascalCase test name (e.g., "HTTPRouteSimpleSameNamespace")
#   $2 - Path to the gateway-api repository root
#
# Output:
#   Absolute path to the test file (e.g., "/path/to/gateway-api/conformance/tests/httproute-simple-same-namespace.go")
#
# Returns:
#   0 - Test file found (path written to stdout)
#   1 - Test file not found
find_test_file() {
    local test_name="$1"
    local conformance_suite="$2"

    local kebab_name
    local filename
    local test_file

    # Convert PascalCase to kebab-case and add .go extension
    # Example: "HTTPRouteSimpleSameNamespace" -> "httproute-simple-same-namespace.go"
    kebab_name=$(to_kebab_case "${test_name}")
    filename="${kebab_name}.go"

    # Check main tests directory first
    # Most tests are located here
    test_file="${conformance_suite}/conformance/tests/${filename}"
    if [[ -f "${test_file}" ]]; then
        echo "${test_file}"
        return 0
    fi

    # Check mesh subdirectory as fallback
    # Some mesh-specific tests are in a subdirectory
    test_file="${conformance_suite}/conformance/tests/mesh/${filename}"
    if [[ -f "${test_file}" ]]; then
        echo "${test_file}"
        return 0
    fi

    # Test file not found in either location
    return 1
}

################################################################################
# Code Modification Functions
################################################################################

# Remove t.Skip() call from a Go test file using ast-grep
#
# This function uses ast-grep (a structural search/replace tool) to safely
# remove t.Skip() calls from Go test files. Unlike regex-based approaches,
# ast-grep understands Go syntax and won't accidentally modify strings,
# comments, or other non-code elements.
#
# The function performs three steps:
# 1. Verify ast-grep is installed
# 2. Check if t.Skip() exists in the file
# 3. Remove all t.Skip() calls using AST-based transformation
#
# Arguments:
#   $1 - Path to the Go test file to modify
#
# Returns:
#   0 - t.Skip() was found and removed successfully
#   1 - ast-grep not installed OR no t.Skip() found in file
#
# Side Effects:
#   Modifies the file in-place, removing all t.Skip() calls
#
# Pattern Explanation:
#   't.Skip($$$)' matches:
#     - t.Skip() with no arguments
#     - t.Skip("reason") with a string argument
#     - t.Skipf("format %s", arg) with multiple arguments
#   The '$$$' is ast-grep's wildcard for "zero or more arguments"
remove_skip_call() {
    local test_file="$1"

    # Verify ast-grep is available
    # ast-grep must be installed separately via: cargo install ast-grep-cli
    if ! command -v ast-grep &> /dev/null; then
        echo -e "${RED}Error: ast-grep is not installed${NC}" >&2
        echo -e "${YELLOW}Install with: cargo install ast-grep-cli${NC}" >&2
        return 1
    fi

    # Check if t.Skip() exists in the file before attempting modification
    # This prevents unnecessary file writes and provides better error reporting
    # Redirect stderr to /dev/null to avoid cluttering output
    if ! ast-grep --pattern 't.Skip($$$)' "${test_file}" &> /dev/null; then
        # No t.Skip() found - this is not an error, just means test is already enabled
        return 1
    fi

    # Remove t.Skip() calls using ast-grep's rewrite capability
    # --pattern: The AST pattern to search for
    # --rewrite: Replace with empty string (effectively deleting the line)
    # --update-all: Modify the file in-place (all occurrences)
    ast-grep --pattern 't.Skip($$$)' --rewrite '' "${test_file}" --update-all

    return 0
}

################################################################################
# Main Entry Point
################################################################################

# Main function - orchestrates the entire workflow
#
# This function coordinates all the steps needed to select and enable the next
# conformance test:
#
# 1. Validate environment (GATEWAY_CONFORMANCE_SUITE must be set)
# 2. Find the next test to implement from tier CSV files
# 3. Locate the corresponding Go test file
# 4. Enable the test by removing t.Skip() if status is "false"
# 5. Display next steps to the user
#
# Exit Codes:
#   0 - Success (test found and processed, or all tests complete)
#   1 - Error (environment not configured, test file not found, etc.)
main() {
    echo -e "${BLUE}Scanning tier CSV files for next test...${NC}"

    ############################################################################
    # Step 1: Validate Environment
    ############################################################################

    # Verify GATEWAY_CONFORMANCE_SUITE is set
    # This variable must point to the local clone of the gateway-api repository
    # Example: export GATEWAY_CONFORMANCE_SUITE=/home/user/gateway-api
    if [[ -z "${GATEWAY_CONFORMANCE_SUITE:-}" ]]; then
        echo -e "${RED}Error: GATEWAY_CONFORMANCE_SUITE environment variable not set${NC}" >&2
        echo "Please set it to the path of your gateway-api repository clone" >&2
        echo "Example: export GATEWAY_CONFORMANCE_SUITE=/path/to/gateway-api" >&2
        exit 1
    fi

    # Verify the path exists and is a directory
    if [[ ! -d "${GATEWAY_CONFORMANCE_SUITE}" ]]; then
        echo -e "${RED}Error: GATEWAY_CONFORMANCE_SUITE path does not exist: ${GATEWAY_CONFORMANCE_SUITE}${NC}" >&2
        exit 1
    fi

    ############################################################################
    # Step 2: Find Next Test to Implement
    ############################################################################

    # Scan tier CSV files in priority order to find next test
    # If no test found, all tests are complete - exit successfully
    local result
    if ! result=$(find_next_test); then
        echo -e "${GREEN}No tests found with status 'in-progress' or 'false'${NC}"
        echo -e "${GREEN}All tests are complete! 🎉${NC}"
        exit 0
    fi

    # Parse pipe-separated result into individual variables
    # Format: "test_name|description|status"
    local test_name
    local description
    local status
    IFS='|' read -r test_name description status <<< "${result}"

    ############################################################################
    # Step 3: Display Test Information
    ############################################################################

    echo ""
    echo "======================================================================"
    echo -e "${GREEN}Next Test: ${test_name}${NC}"
    echo -e "${YELLOW}Status: ${status}${NC}"
    echo -e "Description: ${description}"
    echo "======================================================================"
    echo ""

    ############################################################################
    # Step 4: Locate Test File
    ############################################################################

    # Find the Go source file for this test in the conformance suite
    local test_file
    if ! test_file=$(find_test_file "${test_name}" "${GATEWAY_CONFORMANCE_SUITE}"); then
        echo -e "${RED}Error: Could not find test file for ${test_name}${NC}" >&2
        echo -e "Expected file: $(to_kebab_case "${test_name}").go" >&2
        echo -e "Searched in: ${GATEWAY_CONFORMANCE_SUITE}/conformance/tests/" >&2
        exit 1
    fi

    echo -e "${BLUE}Found test file: ${test_file}${NC}"

    ############################################################################
    # Step 5: Enable Test (if status is "false")
    ############################################################################

    if [[ "${status}" == "false" ]]; then
        # Test not started yet - enable it by removing t.Skip()
        echo ""
        echo -e "${BLUE}Enabling test by removing t.Skip() call...${NC}"

        if remove_skip_call "${test_file}"; then
            # Successfully removed skip call
            echo -e "${GREEN}✓ Successfully removed t.Skip() from ${test_file}${NC}"
            echo ""
            echo "Next steps:"
            echo "1. Run the conformance suite to observe the test failure"
            echo "2. Diagnose the root cause of the failure"
            echo "3. Implement the fix in the multiway codebase"
            echo "4. Update the CSV status to 'in-progress' or 'true' as appropriate"
        else
            # No skip call found - test may already be enabled
            echo -e "${YELLOW}Note: No t.Skip() call found in ${test_file}${NC}"
            echo "The test may already be enabled or use a different skip mechanism"
        fi
    else
        # Test already in progress - just show the file location
        echo ""
        echo -e "${YELLOW}Test is already in-progress. Test file: ${test_file}${NC}"
        echo ""
        echo "Next steps:"
        echo "1. Continue implementing the fix in the multiway codebase"
        echo "2. Run the conformance suite to verify your changes"
        echo "3. Update CSV status to 'true' when the test passes"
    fi
}

################################################################################
# Script Execution
################################################################################

# Execute main function with all command-line arguments
main "$@"
