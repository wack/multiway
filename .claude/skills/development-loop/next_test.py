#!/usr/bin/env python3
"""
Development Loop Helper Script

This script automates the process of selecting and enabling the next conformance test
from the prioritized tier CSV files.

Steps:
1. Concatenates all tier CSV files (tier-1 through tier-7) in priority order
2. Finds the first test with status "in-progress" or "false"
3. Locates the corresponding test file in the Gateway API conformance suite
4. Removes the t.Skip() call to enable the test
"""

import csv
import os
import re
import sys
from pathlib import Path
from typing import Optional, Tuple


def convert_to_kebab_case(name: str) -> str:
    """
    Convert PascalCase test name to kebab-case filename.

    Example: HTTPRouteSimpleSameNamespace -> httproute-simple-same-namespace
    """
    # Insert hyphens before uppercase letters (except at the start)
    s1 = re.sub('(.)([A-Z][a-z]+)', r'\1-\2', name)
    # Insert hyphens before uppercase letters followed by lowercase
    s2 = re.sub('([a-z0-9])([A-Z])', r'\1-\2', s1)
    return s2.lower()


def find_next_test(tiers_dir: Path) -> Optional[Tuple[str, str, str]]:
    """
    Find the first test with status "in-progress" or "false".

    Returns:
        Tuple of (test_name, description, status) or None if no test found
    """
    # Process tiers in order (1-7)
    for tier_num in range(1, 8):
        tier_file = tiers_dir / f"tier-{tier_num}-*.csv"

        # Find the actual tier file (handles variable suffixes)
        tier_files = list(tiers_dir.glob(f"tier-{tier_num}-*.csv"))
        if not tier_files:
            print(f"Warning: No file found matching tier-{tier_num}-*.csv", file=sys.stderr)
            continue

        tier_file = tier_files[0]

        with open(tier_file, 'r') as f:
            reader = csv.DictReader(f)
            for row in reader:
                test_name = row['test_name']
                description = row['description']
                status = row['implemented']

                if status in ('in-progress', 'false'):
                    return (test_name, description, status)

    return None


def find_test_file(conformance_suite: Path, test_name: str) -> Optional[Path]:
    """
    Locate the test file in the conformance suite.

    Args:
        conformance_suite: Path to the gateway-api repository root
        test_name: PascalCase test name (e.g., "HTTPRouteSimpleSameNamespace")

    Returns:
        Path to the test file or None if not found
    """
    # Convert test name to kebab-case filename
    filename = convert_to_kebab_case(test_name) + ".go"

    # Look in the conformance/tests directory
    test_file = conformance_suite / "conformance" / "tests" / filename

    if test_file.exists():
        return test_file

    # Also check in mesh subdirectory
    mesh_test_file = conformance_suite / "conformance" / "tests" / "mesh" / filename
    if mesh_test_file.exists():
        return mesh_test_file

    return None


def remove_skip_call(test_file: Path) -> bool:
    """
    Remove t.Skip() call from the test file.

    Args:
        test_file: Path to the Go test file

    Returns:
        True if skip was removed, False if no skip found
    """
    with open(test_file, 'r') as f:
        content = f.read()

    # Pattern to match t.Skip() calls (handles various formatting)
    # Matches:
    #   t.Skip()
    #   t.Skip("reason")
    #   t.Skip("multi-line reason with \n newlines")
    #   t.Skipf("formatted %s", arg)
    skip_pattern = re.compile(
        r'^\s*t\.Skip(?:f)?\s*\([^)]*\)\s*$',
        re.MULTILINE
    )

    # Check if skip exists
    if not skip_pattern.search(content):
        return False

    # Remove the skip call
    modified_content = skip_pattern.sub('', content)

    # Write back to file
    with open(test_file, 'w') as f:
        f.write(modified_content)

    return True


def main():
    """Main entry point."""
    # Get paths from environment or use defaults
    script_dir = Path(__file__).parent
    tiers_dir = script_dir / "test-tiers"

    conformance_suite = os.environ.get('GATEWAY_CONFORMANCE_SUITE')
    if not conformance_suite:
        print("Error: GATEWAY_CONFORMANCE_SUITE environment variable not set", file=sys.stderr)
        print("Please set it to the path of your gateway-api repository clone", file=sys.stderr)
        sys.exit(1)

    conformance_suite = Path(conformance_suite)
    if not conformance_suite.exists():
        print(f"Error: GATEWAY_CONFORMANCE_SUITE path does not exist: {conformance_suite}", file=sys.stderr)
        sys.exit(1)

    # Find next test
    print("Scanning tier CSV files for next test...")
    result = find_next_test(tiers_dir)

    if not result:
        print("No tests found with status 'in-progress' or 'false'")
        print("All tests are complete!")
        sys.exit(0)

    test_name, description, status = result

    print(f"\n{'='*70}")
    print(f"Next Test: {test_name}")
    print(f"Status: {status}")
    print(f"Description: {description}")
    print(f"{'='*70}\n")

    # Find test file
    test_file = find_test_file(conformance_suite, test_name)
    if not test_file:
        print(f"Error: Could not find test file for {test_name}", file=sys.stderr)
        print(f"Expected file: {convert_to_kebab_case(test_name)}.go", file=sys.stderr)
        print(f"Searched in: {conformance_suite}/conformance/tests/", file=sys.stderr)
        sys.exit(1)

    print(f"Found test file: {test_file}")

    # If status is "false", enable the test by removing t.Skip()
    if status == "false":
        print("\nEnabling test by removing t.Skip() call...")
        if remove_skip_call(test_file):
            print(f"✓ Successfully removed t.Skip() from {test_file}")
            print("\nNext steps:")
            print("1. Run the conformance suite to observe the test failure")
            print("2. Diagnose the root cause")
            print("3. Implement the fix")
            print("4. Update the CSV status to 'in-progress' or 'true' as appropriate")
        else:
            print(f"Note: No t.Skip() call found in {test_file}")
            print("The test may already be enabled or use a different skip mechanism")
    else:
        print(f"\nTest is already in-progress. Test file: {test_file}")
        print("\nNext steps:")
        print("1. Continue implementing the fix")
        print("2. Run the conformance suite to verify")
        print("3. Update CSV status to 'true' when complete")


if __name__ == "__main__":
    main()
