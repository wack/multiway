#!/bin/bash
# shellcheck disable=SC2086  # Intentional word splitting for ARGS
set -e

echo "=== Gateway API Conformance Test Runner ==="
echo "Gateway Class: ${GATEWAY_CLASS_NAME}"
echo "Supported Features: ${SUPPORTED_FEATURES}"
echo "Conformance Profiles: ${CONFORMANCE_PROFILES:-none}"
echo "Exempt Features: ${EXEMPT_FEATURES:-none}"
echo "Skip Tests: ${SKIP_TESTS:-none}"
echo "Run Test: ${RUN_TEST:-all}"
echo "Cleanup Base Resources: ${CLEANUP_BASE_RESOURCES}"
echo "Show Debug: ${SHOW_DEBUG}"
echo "Report Output: ${REPORT_OUTPUT:-none}"
echo "==========================================="

# Build the command arguments
ARGS=""

if [ -n "${GATEWAY_CLASS_NAME}" ]; then
    ARGS="${ARGS} --gateway-class=${GATEWAY_CLASS_NAME}"
fi

if [ -n "${SUPPORTED_FEATURES}" ]; then
    ARGS="${ARGS} --supported-features=${SUPPORTED_FEATURES}"
fi

if [ -n "${CONFORMANCE_PROFILES}" ]; then
    ARGS="${ARGS} --conformance-profiles=${CONFORMANCE_PROFILES}"
fi

if [ -n "${EXEMPT_FEATURES}" ]; then
    ARGS="${ARGS} --exempt-features=${EXEMPT_FEATURES}"
fi

if [ -n "${SKIP_TESTS}" ]; then
    ARGS="${ARGS} --skip-tests=${SKIP_TESTS}"
fi

if [ -n "${RUN_TEST}" ]; then
    ARGS="${ARGS} --run-test=${RUN_TEST}"
fi

if [ "${CLEANUP_BASE_RESOURCES}" = "false" ]; then
    ARGS="${ARGS} --cleanup-base-resources=false"
fi

if [ "${SHOW_DEBUG}" = "true" ]; then
    ARGS="${ARGS} --show-debug"
fi

if [ -n "${REPORT_OUTPUT}" ]; then
    ARGS="${ARGS} --report-output=${REPORT_OUTPUT}"
fi

echo ""
echo "Running: conformance.test -test.v ${ARGS}"
echo ""

# Run the conformance tests
# -test.v enables verbose output
exec /usr/local/bin/conformance.test -test.v ${ARGS}
