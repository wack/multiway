#!/usr/bin/env bash
#
# pick-next.sh
#
# Development loop helper script for implementing Gateway API conformance tests.
# This script:
#   1. Concatenates all tier CSV files in priority order (1 = highest)
#   2. Finds the first test with status "in-progress" or "false"
#   3. If status is "false", enables the test by removing t.Skip() from the conformance suite
#
# Usage:
#   ./pick-next.sh [OPTIONS]
#
# Options:
#   --dry-run           Print what would be done without making changes
#   --show-next         Show the next test to work on without enabling it
#   --list-all          List all tests in priority order with their status
#   --help              Show this help message
#
# Environment Variables:
#   GATEWAY_CONFORMANCE_SUITE   Path to the Gateway API repository root (required for enabling tests)
#

set -euo pipefail

# =============================================================================
# CONFIGURATION
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TIERS_DIR="${SCRIPT_DIR}/test-tiers"

# Tier files in priority order (1 = highest priority)
TIER_FILES=(
    "tier-1-essential.csv"
    "tier-2-important-http.csv"
    "tier-3-production.csv"
    "tier-4-advanced.csv"
    "tier-5-observability.csv"
    "tier-6-validation.csv"
    "tier-7-not-relevant.csv"
)

# Colors for output
COLOR_RED='\033[0;31m'
COLOR_GREEN='\033[0;32m'
COLOR_YELLOW='\033[0;33m'
COLOR_BLUE='\033[0;34m'
COLOR_RESET='\033[0m'

# =============================================================================
# GLOBAL STATE
# =============================================================================

DRY_RUN=false
SHOW_NEXT=false
LIST_ALL=false

# =============================================================================
# LOGGING FUNCTIONS
# =============================================================================

info() {
    echo -e "${COLOR_BLUE}[INFO]${COLOR_RESET} $*"
}

success() {
    echo -e "${COLOR_GREEN}[SUCCESS]${COLOR_RESET} $*"
}

warn() {
    echo -e "${COLOR_YELLOW}[WARN]${COLOR_RESET} $*"
}

error() {
    echo -e "${COLOR_RED}[ERROR]${COLOR_RESET} $*" >&2
}

error_exit() {
    error "$*"
    exit 1
}

# =============================================================================
# HELP
# =============================================================================

show_help() {
    cat << 'EOF'
Usage: pick-next.sh [OPTIONS]

Development loop helper for Gateway API conformance test implementation.

This script manages the test implementation workflow by:
  1. Concatenating all tier CSV files in priority order (tier-1 = highest)
  2. Finding the first test with status "in-progress" or "false"
  3. If status is "false", enabling the test by removing t.Skip() in the conformance suite

Options:
  --dry-run           Print what would be done without making changes
  --show-next         Show the next test to work on without enabling it
  --list-all          List all tests in priority order with their status
  --help              Show this help message

Environment Variables:
  GATEWAY_CONFORMANCE_SUITE   Path to the Gateway API repository root (required for enabling tests)

Examples:
  # Show the next test to implement
  ./pick-next.sh --show-next

  # Enable the next test (remove t.Skip())
  ./pick-next.sh

  # See what would be done without making changes
  ./pick-next.sh --dry-run

  # List all tests with their current status
  ./pick-next.sh --list-all
EOF
    exit 0
}

# =============================================================================
# ARGUMENT PARSING
# =============================================================================

parse_arguments() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --dry-run)
                DRY_RUN=true
                shift
                ;;
            --show-next)
                SHOW_NEXT=true
                shift
                ;;
            --list-all)
                LIST_ALL=true
                shift
                ;;
            --help|-h)
                show_help
                ;;
            *)
                error_exit "Unknown option: $1. Use --help for usage information."
                ;;
        esac
    done
}

# =============================================================================
# CSV PROCESSING FUNCTIONS
# =============================================================================

#######################################
# Concatenates all tier CSV files in priority order.
# Strips the header row from all files except the first.
# Outputs to stdout.
#######################################
concatenate_tier_files() {
    local first_file=true

    for tier_file in "${TIER_FILES[@]}"; do
        local file_path="${TIERS_DIR}/${tier_file}"

        if [[ ! -f "${file_path}" ]]; then
            warn "Tier file not found: ${file_path}"
            continue
        fi

        if [[ "${first_file}" == true ]]; then
            # Include header from first file
            cat "${file_path}"
            first_file=false
        else
            # Skip header (first line) from subsequent files
            tail -n +2 "${file_path}"
        fi
    done
}

#######################################
# Finds the first test with status "in-progress" or "false".
# Returns: test_name,description,status (CSV format)
# Exit code: 0 if found, 1 if not found
#######################################
find_next_test() {
    local csv_data
    csv_data=$(concatenate_tier_files)

    # Skip header and find first row with in-progress or false
    echo "${csv_data}" | tail -n +2 | while IFS=',' read -r test_name description implemented; do
        # Trim whitespace from implemented status
        implemented=$(echo "${implemented}" | tr -d '[:space:]')

        if [[ "${implemented}" == "in-progress" ]] || [[ "${implemented}" == "false" ]]; then
            echo "${test_name},${description},${implemented}"
            return 0
        fi
    done
}

#######################################
# Lists all tests with their status in priority order.
#######################################
list_all_tests() {
    local csv_data
    csv_data=$(concatenate_tier_files)

    echo ""
    echo "All tests in priority order:"
    echo "============================"
    echo ""
    printf "%-40s %-15s %s\n" "TEST NAME" "STATUS" "DESCRIPTION"
    printf "%-40s %-15s %s\n" "---------" "------" "-----------"

    # Skip header and print all rows
    echo "${csv_data}" | tail -n +2 | while IFS=',' read -r test_name description implemented; do
        implemented=$(echo "${implemented}" | tr -d '[:space:]')

        # Color code the status
        local status_colored
        case "${implemented}" in
            true)
                status_colored="${COLOR_GREEN}${implemented}${COLOR_RESET}"
                ;;
            in-progress)
                status_colored="${COLOR_YELLOW}${implemented}${COLOR_RESET}"
                ;;
            false)
                status_colored="${COLOR_RED}${implemented}${COLOR_RESET}"
                ;;
            *)
                status_colored="${implemented}"
                ;;
        esac

        # Truncate description if too long
        if [[ ${#description} -gt 50 ]]; then
            description="${description:0:47}..."
        fi

        printf "%-40s %-15b %s\n" "${test_name}" "${status_colored}" "${description}"
    done

    echo ""
}

# =============================================================================
# PORTABLE UTILITIES
# =============================================================================

#######################################
# Portable sed -i that works on both macOS and Linux.
# On macOS, sed -i requires an argument; on Linux it doesn't.
# Arguments:
#   $1 - sed expression
#   $2 - file to edit
#######################################
sed_inplace() {
    local expression="$1"
    local file="$2"

    if [[ "$(uname)" == "Darwin" ]]; then
        sed -i '' "${expression}" "${file}"
    else
        sed -i "${expression}" "${file}"
    fi
}

# =============================================================================
# AST-GREP FUNCTIONS
# =============================================================================

#######################################
# Checks if ast-grep is available.
# Returns: 0 if available, 1 if not
#######################################
check_ast_grep() {
    if command -v ast-grep &>/dev/null; then
        return 0
    fi

    # Check if it's available via cargo
    if [[ -f "${HOME}/.cargo/bin/ast-grep" ]]; then
        export PATH="${HOME}/.cargo/bin:${PATH}"
        return 0
    fi

    return 1
}

#######################################
# Installs ast-grep via cargo if not available.
#######################################
install_ast_grep() {
    info "Installing ast-grep via cargo..."

    if ! command -v cargo &>/dev/null; then
        error_exit "cargo is required to install ast-grep. Please install Rust first."
    fi

    if ! cargo install ast-grep; then
        error_exit "Failed to install ast-grep"
    fi

    export PATH="${HOME}/.cargo/bin:${PATH}"
    success "ast-grep installed successfully"
}

#######################################
# Finds the test file containing the specified test function.
# Arguments:
#   $1 - Test name (e.g., HTTPRouteSimpleSameNamespace)
# Returns: Path to the test file containing the test
#######################################
find_test_file() {
    local test_name="$1"
    local conformance_dir="${GATEWAY_CONFORMANCE_SUITE}/conformance"

    if [[ ! -d "${conformance_dir}" ]]; then
        error_exit "Conformance directory not found: ${conformance_dir}"
    fi

    # Search for the test name in Go files
    local test_file
    test_file=$(grep -rl "\"${test_name}\"" "${conformance_dir}" --include="*.go" 2>/dev/null | head -1)

    if [[ -z "${test_file}" ]]; then
        # Try searching for the test function directly
        test_file=$(grep -rl "func.*${test_name}" "${conformance_dir}" --include="*.go" 2>/dev/null | head -1)
    fi

    if [[ -z "${test_file}" ]]; then
        return 1
    fi

    echo "${test_file}"
}

#######################################
# Removes t.Skip() call from a test using ast-grep.
# Arguments:
#   $1 - Test name
#   $2 - Test file path
#######################################
remove_skip_with_ast_grep() {
    local test_name="$1"
    local test_file="$2"

    info "Using ast-grep to remove t.Skip() for test: ${test_name}"

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} Would remove t.Skip() from: ${test_file}"
        return 0
    fi

    # Create a temporary file for the ast-grep rule
    # Use a portable approach that works on both macOS and Linux
    local rule_file
    rule_file="${TMPDIR:-/tmp}/ast-grep-rule-$$.yaml"

    # Write the ast-grep rule to find and remove t.Skip() calls
    # This pattern matches t.Skip("reason") statements
    cat > "${rule_file}" << 'RULE'
id: remove-t-skip
language: go
rule:
  any:
    - pattern: t.Skip($$$)
    - pattern: t.Skipf($$$)
    - pattern: t.SkipNow()
fix: ""
RULE

    # Run ast-grep to remove t.Skip() calls
    # We use --rewrite mode to apply the fix
    if ! ast-grep scan --rule "${rule_file}" --update-all "${test_file}" 2>/dev/null; then
        # If ast-grep fails, fall back to sed
        warn "ast-grep failed, falling back to sed-based removal"
        remove_skip_with_sed "${test_name}" "${test_file}"
    fi

    rm -f "${rule_file}"
}

#######################################
# Fallback: Removes t.Skip() call using sed.
# Arguments:
#   $1 - Test name
#   $2 - Test file path
#######################################
remove_skip_with_sed() {
    local test_name="$1"
    local test_file="$2"

    info "Using sed to remove t.Skip() for test: ${test_name}"

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} Would use sed to remove t.Skip() from: ${test_file}"
        return 0
    fi

    # Remove lines containing t.Skip, t.Skipf, or t.SkipNow
    # This is a simple approach - ast-grep is more precise
    sed_inplace '/[[:space:]]*t\.Skip\(f\?\|Now\)(/d' "${test_file}"
}

#######################################
# Enables a test by removing its t.Skip() call.
# Arguments:
#   $1 - Test name
#######################################
enable_test() {
    local test_name="$1"

    info "Enabling test: ${test_name}"

    # Verify GATEWAY_CONFORMANCE_SUITE is set
    if [[ -z "${GATEWAY_CONFORMANCE_SUITE:-}" ]]; then
        error_exit "GATEWAY_CONFORMANCE_SUITE environment variable is not set.

Please set it to the path of your Gateway API repository clone:
    export GATEWAY_CONFORMANCE_SUITE=/path/to/gateway-api"
    fi

    # Find the test file
    local test_file
    if ! test_file=$(find_test_file "${test_name}"); then
        error_exit "Could not find test file for: ${test_name}

The test may not exist in the conformance suite, or it may use a different name.
Please check the conformance suite at: ${GATEWAY_CONFORMANCE_SUITE}/conformance"
    fi

    info "Found test in file: ${test_file}"

    # Check if ast-grep is available
    if ! check_ast_grep; then
        warn "ast-grep not found, attempting to install..."
        install_ast_grep
    fi

    # Remove t.Skip() using ast-grep
    if check_ast_grep; then
        remove_skip_with_ast_grep "${test_name}" "${test_file}"
    else
        warn "ast-grep not available, using sed fallback"
        remove_skip_with_sed "${test_name}" "${test_file}"
    fi

    success "Test enabled: ${test_name}"
    info "File modified: ${test_file}"
}

# =============================================================================
# CSV UPDATE FUNCTIONS
# =============================================================================

#######################################
# Updates a test's status in its tier CSV file.
# Arguments:
#   $1 - Test name
#   $2 - New status (in-progress, true, false)
#######################################
update_test_status() {
    local test_name="$1"
    local new_status="$2"

    info "Updating status of ${test_name} to: ${new_status}"

    if [[ "${DRY_RUN}" == true ]]; then
        echo -e "${COLOR_YELLOW}[DRY-RUN]${COLOR_RESET} Would update ${test_name} status to: ${new_status}"
        return 0
    fi

    # Find which tier file contains this test
    for tier_file in "${TIER_FILES[@]}"; do
        local file_path="${TIERS_DIR}/${tier_file}"

        if [[ ! -f "${file_path}" ]]; then
            continue
        fi

        if grep -q "^${test_name}," "${file_path}"; then
            # Update the status in place
            sed_inplace "s/^\(${test_name},.*,\)[^,]*$/\1${new_status}/" "${file_path}"
            success "Updated ${test_name} in ${tier_file}"
            return 0
        fi
    done

    error "Test not found in any tier file: ${test_name}"
    return 1
}

# =============================================================================
# MAIN WORKFLOW
# =============================================================================

#######################################
# Main development loop workflow.
#######################################
main() {
    parse_arguments "$@"

    # Handle --list-all option
    if [[ "${LIST_ALL}" == true ]]; then
        list_all_tests
        exit 0
    fi

    echo ""
    info "==========================================="
    info "Development Loop Helper"
    info "==========================================="
    echo ""

    # Find the next test to work on
    local next_test
    next_test=$(find_next_test)

    if [[ -z "${next_test}" ]]; then
        success "All tests are implemented! No more tests to work on."
        exit 0
    fi

    # Parse the test info
    local test_name description status
    IFS=',' read -r test_name description status <<< "${next_test}"

    echo ""
    info "Next test to work on:"
    echo ""
    echo "  Test Name:   ${test_name}"
    echo "  Description: ${description}"
    echo "  Status:      ${status}"
    echo ""

    # Handle --show-next option
    if [[ "${SHOW_NEXT}" == true ]]; then
        if [[ "${status}" == "false" ]]; then
            info "To enable this test, run: ./pick-next.sh"
        elif [[ "${status}" == "in-progress" ]]; then
            info "This test is already in-progress. Continue working on it."
        fi
        exit 0
    fi

    # If status is "in-progress", just report it
    if [[ "${status}" == "in-progress" ]]; then
        warn "Test '${test_name}' is already in-progress."
        info "Continue working on this test, or manually update its status to 'true' when complete."
        exit 0
    fi

    # Status is "false" - enable the test
    info "Enabling test: ${test_name}"
    echo ""

    # Enable the test (remove t.Skip())
    enable_test "${test_name}"

    # Update the CSV status to "in-progress"
    update_test_status "${test_name}" "in-progress"

    echo ""
    success "==========================================="
    success "Test enabled and marked as in-progress"
    success "==========================================="
    echo ""
    echo "  Test Name:   ${test_name}"
    echo "  Description: ${description}"
    echo ""
    info "Next steps:"
    echo "  1. Run the conformance tests to see the failure"
    echo "  2. Diagnose and implement the fix"
    echo "  3. Verify the test passes"
    echo "  4. Update the CSV status to 'true' when complete"
    echo ""
}

main "$@"
