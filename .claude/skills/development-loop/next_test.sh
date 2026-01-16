#!/bin/bash
set -euo pipefail

# Development Loop Helper Script (Bash + ast-grep version)
#
# This script automates the process of selecting and enabling the next conformance test
# from the prioritized tier CSV files.
#
# Requirements:
#   - ast-grep (install: cargo install ast-grep-cli)
#   - GATEWAY_CONFORMANCE_SUITE environment variable set to gateway-api repo path

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Check prerequisites
if [ -z "${GATEWAY_CONFORMANCE_SUITE:-}" ]; then
    echo -e "${RED}Error: GATEWAY_CONFORMANCE_SUITE environment variable not set${NC}" >&2
    echo "Please set it to the path of your gateway-api repository clone" >&2
    exit 1
fi

if [ ! -d "$GATEWAY_CONFORMANCE_SUITE" ]; then
    echo -e "${RED}Error: GATEWAY_CONFORMANCE_SUITE path does not exist: $GATEWAY_CONFORMANCE_SUITE${NC}" >&2
    exit 1
fi

if ! command -v ast-grep &> /dev/null; then
    echo -e "${RED}Error: ast-grep not found${NC}" >&2
    echo "Install with: cargo install ast-grep-cli" >&2
    exit 1
fi

# Get the directory containing this script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TIERS_DIR="$SCRIPT_DIR/test-tiers"

# Convert PascalCase to kebab-case
# Example: HTTPRouteSimpleSameNamespace -> httproute-simple-same-namespace
to_kebab_case() {
    echo "$1" | sed -E 's/([a-z0-9])([A-Z])/\1-\2/g; s/([A-Z]+)([A-Z][a-z])/\1-\2/g' | tr '[:upper:]' '[:lower:]'
}

# Find the first test with status "in-progress" or "false"
find_next_test() {
    local test_name=""
    local description=""
    local status=""

    # Process tiers in order (1-7)
    for tier_num in {1..7}; do
        # Find the tier file (handles variable suffixes like tier-1-essential.csv)
        local tier_file=$(ls "$TIERS_DIR"/tier-${tier_num}-*.csv 2>/dev/null | head -1)

        if [ -z "$tier_file" ]; then
            echo -e "${YELLOW}Warning: No file found matching tier-${tier_num}-*.csv${NC}" >&2
            continue
        fi

        # Skip header line, process each row
        while IFS=',' read -r name desc impl; do
            if [ "$impl" = "in-progress" ] || [ "$impl" = "false" ]; then
                test_name="$name"
                description="$desc"
                status="$impl"
                echo "$test_name|$description|$status"
                return 0
            fi
        done < <(tail -n +2 "$tier_file")
    done

    return 1
}

# Find test file in conformance suite
find_test_file() {
    local test_name="$1"
    local filename="$(to_kebab_case "$test_name").go"

    # Check main tests directory
    local test_file="$GATEWAY_CONFORMANCE_SUITE/conformance/tests/$filename"
    if [ -f "$test_file" ]; then
        echo "$test_file"
        return 0
    fi

    # Check mesh subdirectory
    local mesh_file="$GATEWAY_CONFORMANCE_SUITE/conformance/tests/mesh/$filename"
    if [ -f "$mesh_file" ]; then
        echo "$mesh_file"
        return 0
    fi

    return 1
}

# Remove t.Skip() call using ast-grep
remove_skip_call() {
    local test_file="$1"

    # Check if t.Skip() exists
    if ! grep -q 't\.Skip' "$test_file"; then
        return 1
    fi

    # Use ast-grep to remove t.Skip() and t.Skipf() calls
    # The pattern matches both t.Skip() and t.Skipf(...)
    ast-grep --pattern 't.Skip($$$)' --rewrite '' "$test_file" --update 2>/dev/null || true
    ast-grep --pattern 't.Skipf($$$)' --rewrite '' "$test_file" --update 2>/dev/null || true

    # Verify it was removed
    if grep -q 't\.Skip' "$test_file"; then
        return 1
    fi

    return 0
}

# Main execution
main() {
    echo -e "${BLUE}Scanning tier CSV files for next test...${NC}"

    # Find next test
    local result
    if ! result=$(find_next_test); then
        echo -e "${GREEN}No tests found with status 'in-progress' or 'false'${NC}"
        echo "All tests are complete!"
        exit 0
    fi

    # Parse result
    IFS='|' read -r test_name description status <<< "$result"

    # Display test info
    echo ""
    echo "======================================================================"
    echo -e "${BLUE}Next Test:${NC} $test_name"
    echo -e "${BLUE}Status:${NC} $status"
    echo -e "${BLUE}Description:${NC} $description"
    echo "======================================================================"
    echo ""

    # Find test file
    local test_file
    if ! test_file=$(find_test_file "$test_name"); then
        echo -e "${RED}Error: Could not find test file for $test_name${NC}" >&2
        echo "Expected file: $(to_kebab_case "$test_name").go" >&2
        echo "Searched in: $GATEWAY_CONFORMANCE_SUITE/conformance/tests/" >&2
        exit 1
    fi

    echo -e "${GREEN}Found test file:${NC} $test_file"

    # If status is "false", enable the test by removing t.Skip()
    if [ "$status" = "false" ]; then
        echo ""
        echo -e "${BLUE}Enabling test by removing t.Skip() call...${NC}"

        if remove_skip_call "$test_file"; then
            echo -e "${GREEN}✓ Successfully removed t.Skip() from $test_file${NC}"
            echo ""
            echo "Next steps:"
            echo "1. Run the conformance suite to observe the test failure"
            echo "2. Diagnose the root cause"
            echo "3. Implement the fix"
            echo "4. Update the CSV status to 'in-progress' or 'true' as appropriate"
        else
            echo -e "${YELLOW}Note: No t.Skip() call found in $test_file${NC}"
            echo "The test may already be enabled or use a different skip mechanism"
        fi
    else
        echo ""
        echo -e "${YELLOW}Test is already in-progress.${NC} Test file: $test_file"
        echo ""
        echo "Next steps:"
        echo "1. Continue implementing the fix"
        echo "2. Run the conformance suite to verify"
        echo "3. Update CSV status to 'true' when complete"
    fi
}

main "$@"
