#!/bin/bash
# End-to-end test script for horon-agent

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cargo_cmd="$repo_root/scripts/dev/cargo.sh"

run_cargo() {
    (cd "$repo_root" && "$cargo_cmd" "$@")
}

echo "=== E2E Testing Script for horon-agent ==="
echo ""

# Test 1: Check binary exists and runs
echo "Test 1: Check horon-agent binary..."
if command -v horon-agent &> /dev/null; then
    echo "✓ horon-agent binary found"
    horon-agent --version
else
    echo "✗ horon-agent binary not found"
    exit 1
fi

# Test 2: Run the workspace test suite (unit tests plus the tests/e2e harness)
echo ""
echo "Test 2: Run workspace tests..."
run_cargo test 2>&1 | tail -5
echo "✓ Unit and E2E tests passed"

if [[ "${HORON_AGENT_REAL_PROVIDER:-0}" == "1" ]]; then
    echo ""
    echo "Test 3: Real provider smoke (HORON_AGENT_REAL_PROVIDER=1)..."
    "$repo_root/scripts/test/provider_smoke.sh"
    echo "✓ Real provider smoke passed"
fi

echo ""
echo "=== All tests passed! ==="
echo ""
echo "Inner loop with the minimal feature profile: scripts/test/fast.sh"
echo "To exercise the agent directly:"
echo "  horon-agent run \"...\"    # Send one message and exit"
echo "  horon-agent serve         # Start the agent daemon"
echo "  horon-agent acp           # Serve the Agent Client Protocol adapter"
