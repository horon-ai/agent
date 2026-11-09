#!/usr/bin/env bash
# Run the official acp-tck suite (pinned, v1 by default or v2 with --protocol-version 2)
# against a spawned, isolated daemon and gate on its verdict.
#
# The suite drives `horon-agent acp` as a stdio subprocess, and that process talks to a
# daemon over a Unix socket. This script owns everything around it: it puts the pinned TCK
# checkout in place, builds the agent, prepares a scratch home (scripted provider, config),
# starts one daemon, runs the whole suite for the selected protocol
# version with the pinned flags, writes the JSON report, and kills only the processes it
# started. The daemon and the scripted provider run as exec'd children, so the recorded PID
# belongs to the process itself; cleanup stops that PID, waits a bounded time for it to exit,
# and only then removes the scratch root.
#
# The gate holds when the report says the run is conformant, is not blocked by auth or a
# version mismatch, and carries no MANDATORY FAIL/NOT_TESTED and no CAPABILITY FAIL. A run
# that falls short prints the tier counts, keeps the report, and exits non-zero.
#
# Usage:
#   scripts/test/acp_tck.sh [--protocol-version 1|2] [--report PATH] [--scratch DIR] [--keep]
#                      [--agent BIN] [--tck-dir DIR] [--timeout S] [--require-pass LIST]
#                      [--] [extra acp-tck args...]
#
#   --protocol-version
#                  which conformance suite to run: 1 (default) or 2; passed straight to
#                  `acp-tck --protocol-version`. The suite validates against its own vendored
#                  schema per version (v1-unstable, v2 alpha.9).
#   --report       JSON report path (default: $ACP_TCK_EVIDENCE_DIR/acp-tck-v<N>-report.json
#                  when ACP_TCK_EVIDENCE_DIR is set, otherwise
#                  $CARGO_TARGET_DIR/acp-tck-v<N>-report.json)
#   --scratch      scratch root to use instead of a fresh temporary directory
#   --keep         keep the scratch root even when the run passes
#   --agent        agent binary to test (default: build `horon-agent` and use its debug binary)
#   --tck-dir      TCK checkout (default: $CARGO_TARGET_DIR/acp-tck)
#   --timeout      per-response timeout handed to the TCK (default: 90)
#   --require-pass comma-separated requirement ids that must each report PASS; a FAIL,
#                  SKIPPED, or NOT_TESTED result for a listed id fails the gate (default:
#                  empty, or the value of $ACP_TCK_REQUIRE_PASS when that is set)
#
# Environment: ACP_TCK_EVIDENCE_DIR (default report directory), ACP_TCK_REQUIRE_PASS (same as
# --require-pass), CARGO_TARGET_DIR (as for cargo), HORON_AGENT_ACP_PROTOCOL_V2 (the v2 kill
# switch, forwarded to the daemon and the agent so a --protocol-version 2 run can prove the
# negative).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TCK_REPOSITORY="https://github.com/agentclientprotocol/acp-tck"
TCK_COMMIT="b7f40fb3c003c45926547be5a1f7e2b374035815"
AUTH_METHOD="horon-agent-credentials"
TOOL_PROFILE="acp"
# The default `--tck-cancel-prompt` names a compiler; the scripted provider holds that turn
# open, so `session/cancel` lands while the prompt is still in flight.
CANCEL_NEEDLE="compiler"

usage() {
    sed -n '2,/^set -euo pipefail$/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
}

fail() {
    printf 'acp-tck: %s\n' "$*" >&2
    exit 1
}

note() {
    printf 'acp-tck: %s\n' "$*" >&2
}

REPORT=""
SCRATCH=""
AGENT_BIN=""
TCK_DIR=""
KEEP=0
PROTOCOL_VERSION=1
RESPONSE_TIMEOUT=90
REQUIRE_PASS_SPEC=""
REQUIRED_PASSES=()
EXTRA_ARGS=()

while [ "$#" -gt 0 ]; do
    case "$1" in
        --protocol-version)
            case "${2:-}" in
                1|2) PROTOCOL_VERSION="${2:-}" ;;
                *) fail "--protocol-version takes 1 or 2" ;;
            esac
            shift 2 ;;
        --report) REPORT="${2:-}"; [ -n "$REPORT" ] || fail "--report needs a path"; shift 2 ;;
        --scratch) SCRATCH="${2:-}"; [ -n "$SCRATCH" ] || fail "--scratch needs a directory"; shift 2 ;;
        --agent) AGENT_BIN="${2:-}"; [ -n "$AGENT_BIN" ] || fail "--agent needs a binary"; shift 2 ;;
        --tck-dir) TCK_DIR="${2:-}"; [ -n "$TCK_DIR" ] || fail "--tck-dir needs a directory"; shift 2 ;;
        --timeout) RESPONSE_TIMEOUT="${2:-}"; [ -n "$RESPONSE_TIMEOUT" ] || fail "--timeout needs seconds"; shift 2 ;;
        --require-pass) REQUIRE_PASS_SPEC="${2:-}"; [ -n "$REQUIRE_PASS_SPEC" ] || fail "--require-pass needs a comma-separated list"; shift 2 ;;
        --keep) KEEP=1; shift ;;
        --) shift; EXTRA_ARGS=("$@"); break ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown argument $1" ;;
    esac
done

for tool in git cargo python3 uv; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is required"
done

# --require-pass wins over the environment; both are comma-separated, and blanks are dropped.
if [ -z "$REQUIRE_PASS_SPEC" ]; then
    REQUIRE_PASS_SPEC="${ACP_TCK_REQUIRE_PASS:-}"
fi
if [ -n "$REQUIRE_PASS_SPEC" ]; then
    TRIM_IFS=$IFS
    IFS=','
    for entry in $REQUIRE_PASS_SPEC; do
        entry="${entry#"${entry%%[![:space:]]*}"}"
        entry="${entry%"${entry##*[![:space:]]}"}"
        if [ -n "$entry" ]; then
            REQUIRED_PASSES+=("$entry")
        fi
    done
    IFS=$TRIM_IFS
    [ "${#REQUIRED_PASSES[@]}" -gt 0 ] || fail "--require-pass listed no requirement ids"
fi

if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    CARGO_TARGET_DIR="$REPO_ROOT/target"
elif case "$CARGO_TARGET_DIR" in /*) false ;; *) true ;; esac; then
    CARGO_TARGET_DIR="$REPO_ROOT/$CARGO_TARGET_DIR"
fi
mkdir -p "$CARGO_TARGET_DIR"
CARGO_TARGET_DIR="$(cd "$CARGO_TARGET_DIR" && pwd)"

if [ -z "$REPORT" ]; then
    if [ -n "${ACP_TCK_EVIDENCE_DIR:-}" ]; then
        REPORT="$ACP_TCK_EVIDENCE_DIR/acp-tck-v$PROTOCOL_VERSION-report.json"
    else
        REPORT="$CARGO_TARGET_DIR/acp-tck-v$PROTOCOL_VERSION-report.json"
    fi
fi
case "$REPORT" in /*) ;; *) REPORT="$PWD/$REPORT" ;; esac
mkdir -p "$(dirname "$REPORT")" || fail "could not create the report directory for $REPORT"
[ -n "$TCK_DIR" ] || TCK_DIR="$CARGO_TARGET_DIR/acp-tck"

if [ -z "$SCRATCH" ]; then
    SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/acp-tck.XXXXXX")"
    OWN_SCRATCH=1
else
    mkdir -p "$SCRATCH"
    OWN_SCRATCH=0
fi
SCRATCH="$(cd "$SCRATCH" && pwd)"
HOME_DIR="$SCRATCH/home"
RUNTIME_DIR="$SCRATCH/rt"
AGENT_CWD="$SCRATCH/cwd"
DAEMON_LOG="$SCRATCH/daemon.log"
PROVIDER_LOG="$SCRATCH/provider.log"
SOCKET="$RUNTIME_DIR/horon-agent.sock"

DAEMON_PID=""
PROVIDER_PID=""

STOP_GRACE_SECONDS=10

# Stop one child we started: signal it, wait a short bound for it to exit, escalate to
# SIGKILL if it is still there, then reap it. Every step tolerates an already-exited PID.
stop_process() {
    local pid="$1" name="$2" deadline
    [ -n "$pid" ] || return 0
    kill -0 "$pid" 2>/dev/null || {
        wait "$pid" 2>/dev/null || true
        return 0
    }
    kill "$pid" 2>/dev/null || true
    deadline=$((SECONDS + STOP_GRACE_SECONDS))
    while [ "$SECONDS" -lt "$deadline" ]; do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
        note "the $name (pid $pid) ignored SIGTERM; sending SIGKILL"
        kill -KILL "$pid" 2>/dev/null || true
    fi
    wait "$pid" 2>/dev/null || true
    return 0
}

cleanup() {
    local status=$?
    stop_process "$DAEMON_PID" "daemon"
    stop_process "$PROVIDER_PID" "scripted provider"
    DAEMON_PID=""
    PROVIDER_PID=""
    if [ "$status" -eq 0 ] && [ "$KEEP" -eq 0 ] && [ "$OWN_SCRATCH" -eq 1 ]; then
        # Failing to remove scratch never changes the run's verdict.
        rm -rf "$SCRATCH" 2>/dev/null || note "could not remove the scratch root $SCRATCH"
    else
        note "scratch kept: $SCRATCH (daemon log: $DAEMON_LOG)"
    fi
    return "$status"
}

# Route every terminating signal through the EXIT trap so cleanup always runs.
on_signal() {
    note "received SIG$1; stopping"
    exit "$2"
}
trap cleanup EXIT
trap 'on_signal INT 130' INT
trap 'on_signal TERM 143' TERM
trap 'on_signal HUP 129' HUP

if [ ! -d "$TCK_DIR" ]; then
    note "cloning acp-tck into $TCK_DIR"
    mkdir -p "$(dirname "$TCK_DIR")"
    git clone --quiet "$TCK_REPOSITORY" "$TCK_DIR" || fail "cloning $TCK_REPOSITORY failed"
fi
git -C "$TCK_DIR" rev-parse --git-dir >/dev/null 2>&1 \
    || fail "$TCK_DIR exists but is not a checkout of $TCK_REPOSITORY"
git -C "$TCK_DIR" -c advice.detachedHead=false checkout --quiet "$TCK_COMMIT" 2>/dev/null \
    || fail "the pinned commit $TCK_COMMIT is not in $TCK_DIR"
HEAD_COMMIT="$(git -C "$TCK_DIR" rev-parse HEAD)"
[ "$HEAD_COMMIT" = "$TCK_COMMIT" ] \
    || fail "$TCK_DIR is at $HEAD_COMMIT, not the pinned $TCK_COMMIT"

if [ -z "$AGENT_BIN" ]; then
    note "building horon-agent (target dir $CARGO_TARGET_DIR)"
    (cd "$REPO_ROOT" && CARGO_TARGET_DIR="$CARGO_TARGET_DIR" cargo build --locked -p horon-agent) \
        || fail "building horon-agent failed"
    AGENT_BIN="$CARGO_TARGET_DIR/debug/horon-agent"
fi
case "$AGENT_BIN" in /*) ;; *) AGENT_BIN="$PWD/$AGENT_BIN" ;; esac
[ -x "$AGENT_BIN" ] || fail "the agent binary $AGENT_BIN is missing or not executable"

mkdir -p "$HOME_DIR" "$RUNTIME_DIR" "$AGENT_CWD" "$SCRATCH/config" "$SCRATCH/cache"
chmod 700 "$SCRATCH" "$HOME_DIR" "$RUNTIME_DIR"
printf '[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 2048\n' \
    >"$HOME_DIR/config.toml"

# A Chat Completions endpoint shaped like tests/support/lifecycle.rs's ScriptedProvider: one text
# delta and a stop finish reason per turn, and a turn that stays open when the prompt names the
# cancel needle, so cancellation is exercised rather than raced.
cat >"$SCRATCH/provider.py" <<'PROVIDER'
import argparse
import json
import socket
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PLAIN_TEXT = "scripted acp-tck turn"
HOLD_BUDGET_SECONDS = 120.0


def last_user_text(body):
    try:
        value = json.loads(body)
    except ValueError:
        return ""
    messages = value.get("messages")
    if not isinstance(messages, list):
        return ""
    last = ""
    for message in messages:
        if message.get("role") != "user":
            continue
        content = message.get("content")
        if isinstance(content, str):
            last = content
            continue
        if not isinstance(content, list):
            continue
        for block in content:
            if isinstance(block, dict) and isinstance(block.get("text"), str):
                last = block["text"]
    return last


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def read_body(self):
        if "chunked" in (self.headers.get("transfer-encoding") or "").lower():
            body = b""
            while True:
                line = self.rfile.readline().strip()
                size = int((line.split(b";")[0] or b"0"), 16)
                if size == 0:
                    self.rfile.readline()
                    return body
                body += self.rfile.read(size)
                self.rfile.readline()
        length = int(self.headers.get("content-length") or 0)
        return self.rfile.read(length) if length > 0 else b""

    def record(self, body):
        with open(ARGS.log, "a", encoding="utf-8") as stream:
            stream.write(json.dumps({"path": self.path, "body": body}) + "\n")

    def do_GET(self):
        body = json.dumps({"data": [{"id": "scripted", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def do_POST(self):
        raw = self.read_body()
        body = raw.decode("utf-8", "replace")
        self.record(body)
        prompt = last_user_text(body)
        if ARGS.hold_needle and ARGS.hold_needle in prompt:
            self.hold_open()
            return
        frames = [
            {"id": "chatcmpl-scripted", "object": "chat.completion.chunk", "model": "scripted-model",
             "choices": [{"index": 0, "delta": {"role": "assistant", "content": PLAIN_TEXT}, "finish_reason": None}]},
            {"id": "chatcmpl-scripted", "object": "chat.completion.chunk", "model": "scripted-model",
             "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        ]
        payload = ("".join("data: " + json.dumps(frame) + "\n\n" for frame in frames)
                   + "data: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(payload)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(payload)
        self.close_connection = True

    def hold_open(self):
        # No content-length: the body is close-delimited, which is what lets the agent read
        # the delta while the turn is still open. The peer closing the connection (the agent
        # cancelling the turn or exiting) ends the wait.
        frame = json.dumps({
            "id": "chatcmpl-scripted", "object": "chat.completion.chunk", "model": "scripted-model",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": PLAIN_TEXT}, "finish_reason": None}],
        })
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(("data: " + frame + "\n\n").encode())
        self.wfile.flush()
        self.close_connection = True
        deadline = time.monotonic() + HOLD_BUDGET_SECONDS
        self.connection.settimeout(0.5)
        while time.monotonic() < deadline:
            try:
                if not self.connection.recv(256):
                    return
            except socket.timeout:
                continue
            except OSError:
                return


def main():
    global ARGS
    parser = argparse.ArgumentParser()
    parser.add_argument("--port-file", required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("--hold-needle", default="")
    ARGS = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    with open(ARGS.port_file, "w", encoding="utf-8") as stream:
        stream.write(str(server.server_address[1]))
    sys.stderr.write("scripted provider listening on %s\n" % (server.server_address,))
    sys.stderr.flush()
    server.serve_forever()


main()
PROVIDER

# exec inside the subshell makes the recorded PID the provider itself, so cleanup signals the
# process that holds the port instead of a wrapper that leaves it orphaned.
(
    exec python3 "$SCRATCH/provider.py" --port-file "$SCRATCH/provider.port" --log "$PROVIDER_LOG" \
        --hold-needle "$CANCEL_NEEDLE"
) >"$SCRATCH/provider.err" 2>&1 &
PROVIDER_PID=$!

deadline=$((SECONDS + 30))
while [ ! -s "$SCRATCH/provider.port" ]; do
    kill -0 "$PROVIDER_PID" 2>/dev/null || fail "the scripted provider exited; see $SCRATCH/provider.err"
    [ "$SECONDS" -lt "$deadline" ] || fail "the scripted provider never reported a port"
    sleep 0.05
done
PROVIDER_PORT="$(cat "$SCRATCH/provider.port")"
BASE_URL="http://127.0.0.1:$PROVIDER_PORT/v1"
note "scripted provider on $BASE_URL"

# The same isolated environment every child gets, as `--agent-env K=V` pairs for the TCK.
ENV_PAIRS=(
    "HORON_HOME=$HOME_DIR"
    "HORON_AGENT_RUNTIME_DIR=$RUNTIME_DIR"
    "HOME=$SCRATCH"
    "XDG_CONFIG_HOME=$SCRATCH/config"
    "XDG_CACHE_HOME=$SCRATCH/cache"
    "HORON_AGENT_NO_AUTO_UPDATE=1"
    "HORON_AGENT_MEMORY_ENABLED=0"
    "HORON_AGENT_MEMORY_SIDECAR_ENABLED=0"
    "HORON_AGENT_NON_INTERACTIVE=1"
    "HORON_AGENT_TOOL_PROFILE=$TOOL_PROFILE"
    "HORON_AGENT_SOCKET=$SOCKET"
)

# The v2 kill switch is the one setting a caller flips through this script's own environment;
# forwarding it lets a `--protocol-version 2` run prove that the negative is reported as a
# version mismatch instead of passing. Unset stays unset, so the default run is untouched.
if [ -n "${HORON_AGENT_ACP_PROTOCOL_V2:-}" ]; then
    ENV_PAIRS+=("HORON_AGENT_ACP_PROTOCOL_V2=$HORON_AGENT_ACP_PROTOCOL_V2")
fi

# The daemon's only model path is the scripted OpenAI-compatible endpoint, declared as a
# provider profile so the ACP turn can reach it without a plugin runtime.
cat >>"$HOME_DIR/config.toml" <<PROVIDER_PROFILE

[provider]
default_provider = "acp-tck"
default_model = "scripted-model"

[providers.acp-tck]
type = "openai-compatible"
base_url = "$BASE_URL"
auth = "none"
default_model = "scripted-model"
models = [{ id = "scripted-model" }]
PROVIDER_PROFILE

note "starting the isolated daemon"
# exec inside the subshell (bash 3.2 does not do it for the last command) makes $! the daemon's
# own PID; otherwise cleanup kills the wrapper and the daemon survives as a PPID-1 orphan that
# races the scratch removal.
(
    cd "$SCRATCH" && exec env -i PATH="$PATH" "${ENV_PAIRS[@]}" "$AGENT_BIN" serve --no-update
) >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

socket_deadline=$((SECONDS + 120))
while ! [ -S "$SOCKET" ]; do
    kill -0 "$DAEMON_PID" 2>/dev/null || fail "the daemon exited; see $DAEMON_LOG"
    [ "$SECONDS" -lt "$socket_deadline" ] || fail "the daemon never created $SOCKET; see $DAEMON_LOG"
    sleep 0.05
done

AGENT_ENV_ARGS=()
for pair in "${ENV_PAIRS[@]}"; do
    AGENT_ENV_ARGS+=(--agent-env "$pair")
done

rm -f "$REPORT"
note "running acp-tck v$PROTOCOL_VERSION $TCK_COMMIT (report: $REPORT)"
set +e
env -u HORON_AGENT_SOCKET -u HORON_AGENT_API_SOCKET \
    uv run --project "$TCK_DIR" --locked acp-tck \
    --agent-cwd "$AGENT_CWD" \
    --timeout "$RESPONSE_TIMEOUT" \
    --startup-timeout 120 \
    --test-timeout 300 \
    --protocol-version "$PROTOCOL_VERSION" \
    --auth-method "$AUTH_METHOD" \
    --report-json "$REPORT" \
    "${AGENT_ENV_ARGS[@]}" \
    ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"} \
    -- "$AGENT_BIN" acp
TCK_STATUS=$?
set -e

if [ ! -f "$REPORT" ]; then
    fail "acp-tck wrote no report at $REPORT (exit $TCK_STATUS); see $DAEMON_LOG"
fi

python3 - "$REPORT" "$TCK_STATUS" "$REQUIRE_PASS_SPEC" <<'GATE'
import json
import sys

report_path, tck_status = sys.argv[1], int(sys.argv[2])
require_spec = sys.argv[3] if len(sys.argv) > 3 else ""
required_passes = [item.strip() for item in require_spec.split(",") if item.strip()]
with open(report_path, encoding="utf-8") as stream:
    report = json.load(stream)
verdict = report.get("verdict") or {}
counts = verdict.get("tier_counts") or {}


def count(tier, status):
    return int((counts.get(tier) or {}).get(status) or 0)


tiers = [
    "%s pass=%d fail=%d skipped=%d not_tested=%d"
    % (tier, count(tier, "PASS"), count(tier, "FAIL"), count(tier, "SKIPPED"), count(tier, "NOT_TESTED"))
    for tier in ("MANDATORY", "CAPABILITY", "ADVISORY", "INFORMATIONAL")
]
conformant = bool(verdict.get("conformant"))
blocked_by_auth = bool(verdict.get("blocked_by_auth"))
blocked_by_version = bool(verdict.get("blocked_by_version_mismatch"))
print(
    "acp-tck v%s: %s | conformant=%s blocked_by_auth=%s blocked_by_version_mismatch=%s"
    % (
        report.get("protocol_version"),
        " ".join(tiers),
        str(conformant).lower(),
        str(blocked_by_auth).lower(),
        str(blocked_by_version).lower(),
    )
)
gates = {
    "conformant": conformant,
    "!blocked_by_auth": not blocked_by_auth,
    "!blocked_by_version_mismatch": not blocked_by_version,
    "MANDATORY FAIL == 0": count("MANDATORY", "FAIL") == 0,
    "MANDATORY NOT_TESTED == 0": count("MANDATORY", "NOT_TESTED") == 0,
    "CAPABILITY FAIL == 0": count("CAPABILITY", "FAIL") == 0,
}
failed = [name for name, held in gates.items() if not held]
if failed:
    print("acp-tck gate: NOT CONFORMANT (failing: %s)" % ", ".join(failed), file=sys.stderr)
    raise SystemExit(1)

if required_passes:
    status_by_id = {
        requirement.get("id"): requirement.get("status") or "MISSING"
        for requirement in report.get("requirements") or []
    }
    unmet = [
        "%s=%s" % (requirement_id, status_by_id.get(requirement_id, "MISSING"))
        for requirement_id in required_passes
        if status_by_id.get(requirement_id) != "PASS"
    ]
    if unmet:
        print(
            "acp-tck gate: required requirements are not PASS (%s)" % ", ".join(unmet),
            file=sys.stderr,
        )
        raise SystemExit(1)
    print("acp-tck gate: required passes held (%s)" % ", ".join(required_passes))
if tck_status != 0:
    print("acp-tck gate: the suite exited %d although the report is conformant" % tck_status, file=sys.stderr)
    raise SystemExit(tck_status)
print("acp-tck gate: CONFORMANT")
GATE
