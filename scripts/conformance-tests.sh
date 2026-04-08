#!/bin/bash
#
# Gateway API Conformance Test Management Script
#
# This script helps manage Gateway API conformance tests by:
# 1. Listing all available conformance tests from the upstream repo
# 2. Generating a list of test names that can be used to run/skip tests
# 3. Running a subset of tests based on an include/exclude list
#
# Usage:
#   ./scripts/conformance-tests.sh list                  # List all tests
#   ./scripts/conformance-tests.sh list --output FILE    # Save list to file
#   ./scripts/conformance-tests.sh run --tests "Test1,Test2"  # Run specific tests
#   ./scripts/conformance-tests.sh run --file tests.txt  # Run tests from file
#
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
CACHE_DIR="${PROJECT_ROOT}/.cache/gateway-api"
DEFAULT_VERSION="${GATEWAY_API_VERSION:-1.4.1}"
DEFAULT_OUTPUT="${PROJECT_ROOT}/conformance/all-tests.txt"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

usage() {
    cat <<EOF
Gateway API Conformance Test Management

Usage:
    $(basename "$0") <command> [options]

Commands:
    list        List all conformance tests from the upstream repo
    run         Run conformance tests (subset or all)
    help        Show this help message

List Options:
    --version VERSION    Gateway API version (default: $DEFAULT_VERSION)
    --output FILE        Output file for test list (default: $DEFAULT_OUTPUT)
    --refresh            Force refresh from upstream (ignore cache)
    --json               Output as JSON instead of plain text
    --categories         Group tests by category (HTTPRoute, Gateway, etc.)

Run Options:
    --tests "T1,T2,..."  Comma-separated list of tests to run
    --file FILE          File containing test names (one per line)
    --skip "T1,T2,..."   Comma-separated list of tests to skip
    --skip-file FILE     File containing test names to skip (one per line)

Examples:
    # List all tests
    $(basename "$0") list

    # List tests and save to file
    $(basename "$0") list --output my-tests.txt

    # List tests grouped by category
    $(basename "$0") list --categories

    # Run specific tests only
    $(basename "$0") run --tests "HTTPRouteSimpleSameNamespace,GatewayWithAttachedRoutes"

    # Run tests from a file
    $(basename "$0") run --file passing-tests.txt

    # Run all tests except some
    $(basename "$0") run --skip "HTTPRouteTimeout,GRPCRouteWeight"

EOF
}

log_info() {
    echo -e "${BLUE}INFO:${NC} $1"
}

log_success() {
    echo -e "${GREEN}SUCCESS:${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}WARN:${NC} $1"
}

log_error() {
    echo -e "${RED}ERROR:${NC} $1" >&2
}

# Fetch the test files from the upstream repo
fetch_test_files() {
    local version="$1"
    local refresh="$2"
    local cache_version_dir="${CACHE_DIR}/v${version}"

    # Check if we have a cached version with .go files
    local cached_count=0
    if [ -d "$cache_version_dir" ]; then
        cached_count=$(find "$cache_version_dir" -name "*.go" 2>/dev/null | wc -l)
    fi

    if [ "$cached_count" -gt 0 ] && [ "$refresh" != "true" ]; then
        log_info "Using cached test files from $cache_version_dir ($cached_count files)"
        return 0
    fi

    log_info "Fetching test files for Gateway API v${version}..."

    mkdir -p "$cache_version_dir"

    # We'll use the GitHub API to list files and download the Go test files
    local api_url="https://api.github.com/repos/kubernetes-sigs/gateway-api/contents/conformance/tests?ref=v${version}"
    local raw_base="https://raw.githubusercontent.com/kubernetes-sigs/gateway-api/v${version}/conformance/tests"

    # Fetch the file list
    local file_list
    file_list=$(curl -sL "$api_url" | grep -o '"name": "[^"]*\.go"' | cut -d'"' -f4 | grep -v "^main\.go$" || true)

    if [ -z "$file_list" ]; then
        log_error "Failed to fetch file list from GitHub API"
        return 1
    fi

    # Download each Go file
    local count=0
    for file in $file_list; do
        if curl -sL "${raw_base}/${file}" -o "${cache_version_dir}/${file}" 2>/dev/null; then
            count=$((count + 1))
        fi
        # Show progress every 10 files
        if [ $((count % 10)) -eq 0 ]; then
            printf "."
        fi
    done
    echo ""

    log_success "Downloaded $count test files"
}

# Extract test names from Go files
extract_test_names() {
    local cache_version_dir="$1"
    local tests=()

    # Look for ShortName patterns in Go files
    # Pattern: ShortName: "TestName"
    for file in "$cache_version_dir"/*.go; do
        [ -f "$file" ] || continue

        # Extract ShortName values using grep and sed
        while IFS= read -r line; do
            # Extract the test name from ShortName: "TestName" pattern
            local test_name
            test_name=$(echo "$line" | sed -n 's/.*ShortName:[[:space:]]*"\([^"]*\)".*/\1/p')
            if [ -n "$test_name" ]; then
                tests+=("$test_name")
            fi
        done < <(grep -h 'ShortName:' "$file" 2>/dev/null || true)
    done

    # Sort and deduplicate
    printf '%s\n' "${tests[@]}" | sort -u
}

# Categorize tests by their prefix
categorize_tests() {
    local tests=("$@")

    declare -A categories

    for test in "${tests[@]}"; do
        local category
        if [[ "$test" == HTTPRoute* ]]; then
            category="HTTPRoute"
        elif [[ "$test" == Gateway* ]]; then
            category="Gateway"
        elif [[ "$test" == GatewayClass* ]]; then
            category="GatewayClass"
        elif [[ "$test" == GRPCRoute* ]]; then
            category="GRPCRoute"
        elif [[ "$test" == TLSRoute* ]]; then
            category="TLSRoute"
        elif [[ "$test" == TCPRoute* ]]; then
            category="TCPRoute"
        elif [[ "$test" == UDPRoute* ]]; then
            category="UDPRoute"
        elif [[ "$test" == BackendTLSPolicy* ]]; then
            category="BackendTLSPolicy"
        elif [[ "$test" == Mesh* ]]; then
            category="Mesh"
        else
            category="Other"
        fi

        if [ -z "${categories[$category]}" ]; then
            categories[$category]="$test"
        else
            categories[$category]="${categories[$category]}|$test"
        fi
    done

    # Print categories
    for category in $(echo "${!categories[@]}" | tr ' ' '\n' | sort); do
        echo ""
        echo "=== $category ==="
        echo "${categories[$category]}" | tr '|' '\n' | sort
    done
}

# List command
cmd_list() {
    local version="$DEFAULT_VERSION"
    local output=""
    local refresh="false"
    local json="false"
    local categories="false"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)
                version="$2"
                shift 2
                ;;
            --output)
                output="$2"
                shift 2
                ;;
            --refresh)
                refresh="true"
                shift
                ;;
            --json)
                json="true"
                shift
                ;;
            --categories)
                categories="true"
                shift
                ;;
            *)
                log_error "Unknown option: $1"
                usage
                exit 1
                ;;
        esac
    done

    local cache_version_dir="${CACHE_DIR}/v${version}"

    fetch_test_files "$version" "$refresh"

    log_info "Extracting test names..."

    # Get test names
    local tests
    tests=$(extract_test_names "$cache_version_dir")

    local test_count
    test_count=$(echo "$tests" | wc -l)

    log_success "Found $test_count conformance tests"

    if [ "$json" = "true" ]; then
        # Output as JSON array
        echo "$tests" | jq -R -s 'split("\n") | map(select(length > 0))'
    elif [ "$categories" = "true" ]; then
        # Output with categories
        mapfile -t test_array <<< "$tests"
        categorize_tests "${test_array[@]}"
    else
        # Plain text output
        echo ""
        echo "$tests"
    fi

    # Save to file if requested
    if [ -n "$output" ]; then
        mkdir -p "$(dirname "$output")"
        echo "$tests" > "$output"
        log_success "Test list saved to $output"
    fi
}

# Run command
cmd_run() {
    local tests=""
    local tests_file=""
    local skip=""
    local skip_file=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --tests)
                tests="$2"
                shift 2
                ;;
            --file)
                tests_file="$2"
                shift 2
                ;;
            --skip)
                skip="$2"
                shift 2
                ;;
            --skip-file)
                skip_file="$2"
                shift 2
                ;;
            *)
                log_error "Unknown option: $1"
                usage
                exit 1
                ;;
        esac
    done

    # Build the RUN_TEST or SKIP_TESTS environment variable
    local run_test=""
    local skip_tests=""

    # Handle tests to run
    if [ -n "$tests" ]; then
        run_test="$tests"
    elif [ -n "$tests_file" ]; then
        if [ ! -f "$tests_file" ]; then
            log_error "Tests file not found: $tests_file"
            exit 1
        fi
        # Read file, skip comments and empty lines, join with commas
        run_test=$(grep -v '^#' "$tests_file" | grep -v '^$' | tr '\n' ',' | sed 's/,$//')
    fi

    # Handle tests to skip
    if [ -n "$skip" ]; then
        skip_tests="$skip"
    elif [ -n "$skip_file" ]; then
        if [ ! -f "$skip_file" ]; then
            log_error "Skip file not found: $skip_file"
            exit 1
        fi
        # Read file, skip comments and empty lines, join with commas
        skip_tests=$(grep -v '^#' "$skip_file" | grep -v '^$' | tr '\n' ',' | sed 's/,$//')
    fi

    # Modify the job.yaml to include the test selection
    log_info "Configuring conformance test job..."

    if [ -n "$run_test" ]; then
        log_info "Running tests: $run_test"
        export RUN_TEST="$run_test"
    fi

    if [ -n "$skip_tests" ]; then
        log_info "Skipping tests: $skip_tests"
        export SKIP_TESTS="$skip_tests"
    fi

    # Run the conformance tests using cargo make
    log_info "Starting conformance test run..."

    # We need to pass the environment to the job
    # Create a temporary modified job.yaml
    local job_yaml="${PROJECT_ROOT}/conformance/job.yaml"
    local temp_job="${PROJECT_ROOT}/conformance/.job-temp.yaml"

    # Copy the original job
    cp "$job_yaml" "$temp_job"

    # Add RUN_TEST env var if specified
    if [ -n "$run_test" ]; then
        # Insert RUN_TEST env var into the job
        sed -i "/- name: SHOW_DEBUG/a\\            - name: RUN_TEST\\n              value: \"$run_test\"" "$temp_job"
    fi

    # Add SKIP_TESTS env var if specified
    if [ -n "$skip_tests" ]; then
        # Insert SKIP_TESTS env var into the job
        sed -i "/- name: SHOW_DEBUG/a\\            - name: SKIP_TESTS\\n              value: \"$skip_tests\"" "$temp_job"
    fi

    # Apply the temporary job
    kubectl apply -f "$temp_job"

    # Clean up temp file
    rm -f "$temp_job"

    log_info "Waiting for conformance test job to complete..."

    # Wait for job completion
    local timeout=1800
    local elapsed=0
    local interval=5

    while [ $elapsed -lt $timeout ]; do
        local status
        status=$(kubectl get job gateway-api-conformance -n gateway-conformance-infra \
            -o jsonpath='{.status.conditions[?(@.status=="True")].type}' 2>/dev/null || echo "")

        if echo "$status" | grep -q "Complete"; then
            echo ""
            log_success "Conformance tests completed!"
            break
        elif echo "$status" | grep -q "Failed"; then
            echo ""
            log_warn "Some conformance tests failed"
            break
        fi

        sleep $interval
        elapsed=$((elapsed + interval))
        printf "."
    done

    if [ $elapsed -ge $timeout ]; then
        echo ""
        log_error "Conformance tests timed out after 30 minutes!"
    fi

    echo ""
    echo "=== Conformance Test Logs ==="
    echo ""
    kubectl logs -n gateway-conformance-infra job/gateway-api-conformance --tail=-1
}

# Main entry point
main() {
    if [ $# -eq 0 ]; then
        usage
        exit 0
    fi

    local command="$1"
    shift

    case "$command" in
        list)
            cmd_list "$@"
            ;;
        run)
            cmd_run "$@"
            ;;
        help|--help|-h)
            usage
            ;;
        *)
            log_error "Unknown command: $command"
            usage
            exit 1
            ;;
    esac
}

main "$@"
