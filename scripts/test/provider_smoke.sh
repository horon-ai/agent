#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
provider=${HORON_AGENT_PROVIDER:-auto}
prompt=${1:-"Use the bash tool to run 'pwd', then use the ls tool to list the current directory, then respond with DONE."}
expect=${HORON_AGENT_TRACE_EXPECT:-DONE}
cargo_cmd="$repo_root/scripts/dev/cargo.sh"

echo "=== Real Provider Smoke ==="
echo "Provider: ${provider}"

echo ""
echo "Test 1: End-to-end trace"
if [[ ! -x "$repo_root/target/release/horon-agent" ]]; then
  (cd "$repo_root" && "$cargo_cmd" build --release)
fi

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

set +e
output=$(HORON_AGENT_HOME="$workdir" PATH="$repo_root/target/release:$PATH" \
  horon-agent run --no-update --trace --provider "$provider" "$prompt" 2>&1)
status=$?
set -e

printf "%s\n" "$output"

if [[ $status -ne 0 ]]; then
  echo "Trace failed with exit code $status" >&2
  exit $status
fi

if [[ -n "$expect" ]] && ! grep -q "$expect" <<<"$output"; then
  echo "Trace output did not include expected marker: ${expect}" >&2
  exit 1
fi

echo ""
echo "=== Real provider smoke OK ==="
