#!/usr/bin/env bash
# check_memory.sh - Automated pass/fail gate over horon-agent memory.
#
# Reads one idle memory measurement and compares it with thresholds so a
# regression fails loudly (nightly job, pre-release check) instead of being
# rediscovered when the machine starts swapping.
#
# Baseline (2026-07-04, selfdev build, 8 MB / 322-message hog session), after
# the retention + live-heap work (mmap-threshold pin, idle trim, syntect
# regex-onig backend, inline-image payload release):
#
#   idle rss_anon:              ~38 MB   (was ~69 MB)
#   idle live heap (allocated): ~30 MB   (was ~60 MB)
#
# Thresholds sit roughly halfway between the fixed and regressed values, so
# normal variance passes but a real regression toward old behavior fails.
#
# Metric source:
#   --metrics-file <path>   read a metric object (or a JSONL probe stream, in
#                           which case the last `idle` phase line is used)
#   --metrics-json '<json>' read one metric object inline
#   (default)               collect from a live daemon with
#                           `horon-agent debug memory` on the debug socket
#
# A required metric that the source does not carry is a `measurement_error`,
# never a zero. A required metric the platform cannot provide (rss_anon needs
# /proc, which macOS does not have; allocator stats need a build with them
# enabled) is reported as `unsupported` with the reason, never as zero.
#
# Usage:
#   scripts/perf/check_memory.sh [--session <id>] [--idle-secs <n>]
#                                     [--metrics-file <path>]
#                                     [--metrics-json '<json>']
#
# Env overrides:
#   HORON_AGENT_MEMGATE_SESSION               probe session id (optional)
#   HORON_AGENT_MEMGATE_MAX_RSS_ANON_KB       idle rss_anon bound (default 56320 = 55 MiB)
#   HORON_AGENT_MEMGATE_MAX_LIVE_HEAP_BYTES   idle allocated_bytes bound (default 47185920 = 45 MiB)
#   HORON_AGENT_MEMGATE_BINARY                horon-agent binary for live collection
#   HORON_AGENT_MEMGATE_SOCKET                daemon socket for live collection
#
# Exit codes: 0 pass, 1 threshold exceeded, 2 could not measure
# (status `measurement_error` or `unsupported`), 3 skipped (the requested probe
# session is genuinely absent on this machine).
#
# Emits one machine-parseable line: `HORON_AGENT_MEMGATE_RESULT {json}`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SESSION_ID="${HORON_AGENT_MEMGATE_SESSION:-}"
SESSION_SUPPLIED=0
[[ -n "$SESSION_ID" ]] && SESSION_SUPPLIED=1
IDLE_SECS=60
MAX_RSS_ANON_KB="${HORON_AGENT_MEMGATE_MAX_RSS_ANON_KB:-56320}"
MAX_LIVE_HEAP_BYTES="${HORON_AGENT_MEMGATE_MAX_LIVE_HEAP_BYTES:-47185920}"
METRICS_FILE=""
METRICS_JSON=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --session) SESSION_ID="$2"; SESSION_SUPPLIED=1; shift 2 ;;
        --idle-secs) IDLE_SECS="$2"; shift 2 ;;
        --metrics-file) METRICS_FILE="$2"; shift 2 ;;
        --metrics-json) METRICS_JSON="$2"; shift 2 ;;
        -h|--help) sed -n '2,50p' "$0"; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

command -v jq >/dev/null || { echo "jq required" >&2; exit 2; }

emit() { printf 'HORON_AGENT_MEMGATE_RESULT %s\n' "$1"; }

measurement_error() {
    emit "$(jq -cn --arg reason "$1" '{status:"measurement_error", reason:$reason}')"
    echo "[memgate] measurement_error: $1" >&2
    exit 2
}

unsupported() {
    emit "$(jq -cn --arg metric "$1" --arg reason "$2" \
        '{status:"unsupported", metric:$metric, reason:$reason}')"
    echo "[memgate] unsupported: $1 ($2)" >&2
    exit 2
}

# A generated id keeps the run reproducible in the payload without pinning one
# machine's session file, which is what the old default did.
if [[ -z "$SESSION_ID" ]]; then
    SESSION_ID="memgate_$(date -u +%Y%m%dT%H%M%SZ)_$$"
fi

# Skip (not fail) when a session the caller asked for does not exist on this
# machine: thresholds are only meaningful against a fixed workload.
if [[ "$SESSION_SUPPLIED" -eq 1 ]] \
    && ! ls "$HOME/.horon/sessions/${SESSION_ID}"* >/dev/null 2>&1; then
    emit "$(jq -cn --arg session "$SESSION_ID" \
        '{status:"skipped", reason:("probe session " + $session + " not found")}')"
    exit 3
fi

# ---------------------------------------------------------------------------
# Metric collection.
# ---------------------------------------------------------------------------
collect_live_metrics() {
    local binary="${HORON_AGENT_MEMGATE_BINARY:-}"
    if [[ -z "$binary" ]]; then
        binary="$(command -v horon-agent || true)"
    fi
    if [[ -z "$binary" ]]; then
        local candidate
        for candidate in "$SCRIPT_DIR/../../target/release/horon-agent" \
                         "$SCRIPT_DIR/../../target/debug/horon-agent" \
                         "$HOME/.local/bin/horon-agent"; do
            [[ -x "$candidate" ]] && { binary="$candidate"; break; }
        done
    fi
    [[ -n "$binary" && -x "$binary" ]] || measurement_error "horon-agent binary not found for live collection"
    local socket="${HORON_AGENT_MEMGATE_SOCKET:-${HORON_AGENT_SOCKET:-}}"
    local -a argv=("$binary" debug memory)
    [[ -n "$socket" ]] && argv+=(-s "$socket")

    local raw
    local debug_stderr
    debug_stderr="$(mktemp)"
    if ! raw="$(HORON_AGENT_DEBUG_CONTROL=1 "${argv[@]}" 2>"$debug_stderr")"; then
        local detail
        detail="$(tr '\n' ' ' < "$debug_stderr" | cut -c1-300)"
        rm -f "$debug_stderr"
        measurement_error "live collection failed: ${argv[*]} (${detail:-no stderr})"
    fi
    rm -f "$debug_stderr"

    local proc_reason="" rss_anon="null"
    if [[ -d /proc ]]; then
        local server_pid
        server_pid="$(pgrep -f 'horon-agent.*serve' | head -1 || true)"
        if [[ -n "$server_pid" && -r "/proc/${server_pid}/status" ]]; then
            rss_anon="$(awk '/^RssAnon:/{print $2}' "/proc/${server_pid}/status")"
            [[ -n "$rss_anon" ]] || proc_reason="RssAnon missing from /proc/${server_pid}/status"
        else
            proc_reason="no readable /proc status for a running horon-agent serve process"
        fi
    else
        proc_reason="no /proc on $(uname -s); rss_anon needs Linux /proc/<pid>/status"
    fi

    METRICS="$(jq -cn --argjson raw "$raw" --arg proc_reason "$proc_reason" --argjson rss_anon "$rss_anon" '
        ($raw.process // {}) as $p
        | ($p.allocator.stats.allocated_bytes // null) as $alloc
        | {
            phase: "idle",
            session: null,
            proc: {
              rss_anon_kb: $rss_anon,
              rss_anon_kb_reason: (if $proc_reason == "" then null else $proc_reason end)
            },
            client: {
              allocated_bytes: $alloc,
              allocated_bytes_reason: (
                if $alloc == null
                  then "allocator stats unavailable (allocator=" + ($p.allocator.name // "unknown") + ", stats_available=" + (($p.allocator.stats_available // false) | tostring) + ")"
                  else null
                end
              )
            },
            source: "live"
          }')"
}

load_metrics() {
    if [[ -n "$METRICS_JSON" ]]; then
        METRICS="$METRICS_JSON"
        return 0
    fi
    if [[ -n "$METRICS_FILE" ]]; then
        [[ -r "$METRICS_FILE" ]] || measurement_error "metrics file not readable: $METRICS_FILE"
        # A probe stream is JSONL with one object per phase; prefer the last
        # idle line, else the last object.
        METRICS="$(jq -c -s 'if (map(select(.phase == "idle")) | length) > 0
                  then (map(select(.phase == "idle")) | last)
                  else last end' "$METRICS_FILE")" \
            || measurement_error "metrics file is not a JSON object or JSONL stream: $METRICS_FILE"
        return 0
    fi
    collect_live_metrics
}

METRICS=""
load_metrics
[[ -n "$METRICS" ]] || measurement_error "no metrics produced"

# metric_state reports how a metric is present: number, null, or absent.
metric_state() {
    jq -r --arg section "$1" --arg key "$2" '
        if (.[$section] | type) == "object" and (.[$section] | has($key))
        then (.[$section][$key] | type)
        else "absent" end' <<<"$METRICS"
}

metric_reason() {
    jq -r --arg section "$1" --arg key "$2" --arg fallback "$3" \
        '.[$section][$key + "_reason"] // $fallback' <<<"$METRICS"
}

require_metric() {
    local section="$1" key="$2" label="$3" state
    state="$(metric_state "$section" "$key")"
    case "$state" in
        number)
            METRIC_VALUE="$(jq -r --arg section "$section" --arg key "$key" '.[$section][$key]' <<<"$METRICS")"
            ;;
        null)
            unsupported "$label" "$(metric_reason "$section" "$key" "$label cannot be measured on this platform")"
            ;;
        *)
            measurement_error "required metric missing from the measurement: $label (${section}.${key})"
            ;;
    esac
}

require_metric proc rss_anon_kb idle_rss_anon_kb
RSS_ANON_KB="$METRIC_VALUE"
require_metric client allocated_bytes idle_live_heap_bytes
LIVE_HEAP_BYTES="$METRIC_VALUE"

# Informational server-side numbers (never gate: server load varies). Nothing
# here substitutes a zero for a failed read: without /proc the metric is
# reported as unsupported with its reason.
if [[ -r "/proc/self/status" ]]; then
    SERVER_PID="$(pgrep -f 'horon-agent serve' | head -1 || true)"
    if [[ -n "$SERVER_PID" && -r "/proc/${SERVER_PID}/status" ]]; then
        SERVER_INFO="$(jq -cn --argjson rss "$(awk '/VmRSS/{print $2*1024}' "/proc/${SERVER_PID}/status")" \
            '{supported: true, rss_bytes: $rss}')"
    else
        SERVER_INFO='{"supported": false, "reason": "no live horon-agent serve process with readable /proc status"}'
    fi
else
    SERVER_INFO="$(jq -cn --arg reason "no /proc on $(uname -s)" '{supported: false, reason: $reason}')"
fi

FAILURES=()
if (( RSS_ANON_KB > MAX_RSS_ANON_KB )); then
    FAILURES+=("idle rss_anon ${RSS_ANON_KB}kB > ${MAX_RSS_ANON_KB}kB")
fi
if (( LIVE_HEAP_BYTES > MAX_LIVE_HEAP_BYTES )); then
    FAILURES+=("idle live heap ${LIVE_HEAP_BYTES}B > ${MAX_LIVE_HEAP_BYTES}B")
fi

STATUS="pass"
(( ${#FAILURES[@]} > 0 )) && STATUS="fail"

jq -cn \
    --arg status "$STATUS" \
    --arg session "$SESSION_ID" \
    --argjson rss_anon_kb "$RSS_ANON_KB" \
    --argjson live_heap_bytes "$LIVE_HEAP_BYTES" \
    --argjson max_rss_anon_kb "$MAX_RSS_ANON_KB" \
    --argjson max_live_heap_bytes "$MAX_LIVE_HEAP_BYTES" \
    --argjson server "$SERVER_INFO" \
    --args '{status:$status, session:$session, idle_rss_anon_kb:$rss_anon_kb,
             idle_live_heap_bytes:$live_heap_bytes,
             thresholds:{rss_anon_kb:$max_rss_anon_kb, live_heap_bytes:$max_live_heap_bytes},
             server:$server, failures:$ARGS.positional}' -- "${FAILURES[@]+"${FAILURES[@]}"}" \
    | sed 's/^/HORON_AGENT_MEMGATE_RESULT /'

if [[ "$STATUS" == "fail" ]]; then
    printf '[memgate] FAIL: %s\n' "${FAILURES[@]}" >&2
    exit 1
fi
echo "[memgate] PASS: idle rss_anon ${RSS_ANON_KB}kB (<= ${MAX_RSS_ANON_KB}), live heap ${LIVE_HEAP_BYTES}B (<= ${MAX_LIVE_HEAP_BYTES})" >&2
