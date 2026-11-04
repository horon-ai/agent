#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cargo_cmd="$repo_root/scripts/dev/cargo.sh"

run_cargo() {
  (cd "$repo_root" && "$cargo_cmd" "$@")
}

echo "=== Fast test loop (library + primary horon-agent binary) ==="
# The default product feature set is PDF extraction, and `full` adds local
# embeddings. Both have full-suite coverage, but compiling them on every
# inner-loop test adds crates and peak RSS. Keep the fast loop minimal unless
# explicitly overridden with HORON_AGENT_DEV_FEATURE_PROFILE=default/full.
export HORON_AGENT_DEV_FEATURE_PROFILE="${HORON_AGENT_DEV_FEATURE_PROFILE:-minimal}"
echo "Feature profile: $HORON_AGENT_DEV_FEATURE_PROFILE"

# Only the primary `horon-agent` binary contains unit tests, so `--bins` would
# needlessly build and link extra copies of the full graph.
run_cargo test --lib --bin horon-agent "$@"

echo ""
if [[ -x "$repo_root/target/release/horon-agent" ]]; then
  echo "=== Startup regression check (release binary) ==="
  "$repo_root/scripts/perf/check_startup.sh" "$repo_root/target/release/horon-agent"
  echo ""
else
  echo "Skipping startup regression check: build release first with cargo build --release"
  echo ""
fi

echo "For full coverage, run: scripts/test/e2e.sh"
