#!/usr/bin/env bash
# End-to-end check for the TypeScript SDK against a real harness bridge.
#
# Unit tests use a mock server, which cannot catch translation mismatches (the
# bridge answers `send_message` with a `message_accepted` event rather than a
# reply, and only a live run reveals that). This script builds the bridge,
# points it at the running daemon, and drives one real turn through the SDK.
#
# Usage: scripts/sdk/test_e2e.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

sdk_dir="$repo_root/packages/sdk/typescript"
socket="${TMPDIR:-/tmp}/horon-agent-sdk-e2e-$$.sock"
log="${TMPDIR:-/tmp}/horon-agent-sdk-e2e-$$.log"

echo "== building SDK =="
npm --prefix "$sdk_dir" install --no-audit --no-fund --silent
npm --prefix "$sdk_dir" run build --silent

# The full binary, for the `launch()` checks: those start a real instance via
# `horon-agent api-bridge`, which only the shipped binary provides.
echo "== building horon-agent =="
cargo build --profile selfdev -p horon-agent --bin horon-agent
horon_agent_bin="$repo_root/target/selfdev/horon-agent"

echo "== building bridge =="
# The standalone bridge binary, not `horon-agent api-bridge`, because the full horon-agent
# binary takes minutes to link and this script must stay fast enough to run
# per-change. The subcommand is a thin wrapper over the same `run_bridge`, and
# `api_bridge_socket_flags_do_not_collide` covers the wiring it adds.
cargo build --profile selfdev -p horon-agent-harness-api-server --bin horon-agent-harness-api-bridge

# The bridge translates onto whatever daemon the environment points at, and that
# daemon answers with whatever provider the environment selected. Inheriting
# HORON_AGENT_ACTIVE_PROVIDER therefore makes this suite pass or fail based on the
# caller's shell: a stale login for one provider turns every turn into a fatal
# auth error, even though the same machine answers fine through failover. Let
# provider selection fall back to the configured default instead.
echo "== starting bridge on $socket =="
HORON_AGENT_API_SOCKET="$socket" HORON_AGENT_ACTIVE_PROVIDER="" ./target/selfdev/horon-agent-harness-api-bridge >"$log" 2>&1 &
bridge_pid=$!
cleanup() {
  kill "$bridge_pid" 2>/dev/null || true
  rm -f "$socket"
}
trap cleanup EXIT

for _ in $(seq 1 50); do
  [ -S "$socket" ] && break
  sleep 0.1
done
if [ ! -S "$socket" ]; then
  echo "bridge failed to start:"; cat "$log"; exit 1
fi

echo "== driving a real turn =="
HORON_AGENT_API_SOCKET="$socket" node "$sdk_dir/test/live-turn.mjs"

echo "== exercising the control surface =="
HORON_AGENT_API_SOCKET="$socket" node "$sdk_dir/test/live-control.mjs"

echo "== exercising models, effort, compaction, rename, and undo =="
HORON_AGENT_API_SOCKET="$socket" node "$sdk_dir/test/live-capabilities.mjs"

# `launch()` starts its own daemon and bridge, so these run against a private
# instance rather than the shared socket above. That is the point: the
# isolation they check is a property of a *separate* instance, and running
# them against the shared bridge would prove nothing.
echo "== launching a private instance =="
node "$sdk_dir/test/live-launch.mjs" "$horon_agent_bin"

echo "== checking instance isolation and path safety =="
node "$sdk_dir/test/live-isolation.mjs" "$horon_agent_bin"

echo "== checking every documented launch option =="
node "$sdk_dir/test/live-options.mjs" "$horon_agent_bin"

echo "== checking mid-turn message ids, cancellation, and background results =="
node "$sdk_dir/test/live-soft-interrupt-ids.mjs" "$horon_agent_bin"

echo "== bridge log =="
cat "$log"
echo "SDK e2e passed."
