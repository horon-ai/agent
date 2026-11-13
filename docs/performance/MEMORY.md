# Memory

Language: **English** | [한국어](MEMORY.ko.md)

This covers the memory regression budget and the server memory incident response procedure.

## Memory regression budget

Status: active guardrail
Updated: 2026-10-20

This document sets horon-agent's current memory regression budget.

For live-server diagnosis and the treatment for each cause, see the
[HoronAgent Server Memory Incident Runbook](MEMORY.md).

The goal is not to freeze memory usage forever. It is to make a memory change meet these conditions.
- Measurable
- Reviewable
- Explained intent

Where possible, the budgets below bind to counters and caps the codebase already exposes rather than to guessed RSS numbers.

### How metrics are collected

Do not add new temporary instrumentation; use the existing debug surfaces.

- Agent and session memory profile over the debug socket: `agent:memory`
  (`crates/app-core/src/server/debug_command_exec.rs:401`)
- Fast server incident triage: `server:memory-incident`
  (`crates/app-core/src/server/debug_server_state.rs:170`)
- Whole-server attribution: `server:memory`
  (`crates/app-core/src/server/debug_server_state.rs:184`)
- Process lifetime timeline: `python scripts/perf/analyze_memory_log.py --days 1`

Main sources in the code:
- `crates/base/src/runtime_memory_log.rs`
- `crates/app-core/src/server/debug_server_state.rs`

Terminal UI surfaces such as `:debug memory` and `:debug markdown:memory` do not exist in this build. The terminal client
`horon-tui` draws plain styled text and keeps no markdown or diagram cache, so the caps that hung off those caches have no
counterpart here and were lowered in this document for the same reason.

### Budget model

Two kinds of budget are used.

1. Hard caps
- Explicit limits a cache already enforces.
- A regression here means the code changed or bypassed the cap.

2. Ratchet expectations
- Expected relationships between memory counters.
- A regression here is allowed only with an explanation and updated documents and tests.

### Hard caps

#### Runtime memory log budget

Source: `crates/base/src/runtime_memory_log.rs`

The log file count, pending event queue, and sampling intervals are values the code enforces directly. A smaller value from config does not
push the sampling interval cap below its floor (`crates/base/src/runtime_memory_log.rs:482-495`, `:521-524`).

| Metric | Budget | Why |
|---|---:|---|
| Server log file count | `<= 90` | `MAX_SERVER_LOG_FILES`. Beyond that the oldest files are deleted (`:19`, `:602-606`) |
| Client log file count | `<= 90` | `MAX_CLIENT_LOG_FILES` (`:28`) |
| Pending event count | `<= 64` | `MAX_PENDING_EVENTS`. Beyond that the oldest events are dropped (`:31`, `:343-344`) |
| Pending category count | `<= 8` | `MAX_PENDING_CATEGORIES` (`:32`, `:364`) |
| Process sample interval | `>= 15s` | `MIN_PROCESS_INTERVAL_SECS` (`:10`) |
| Attribution sample interval | `>= 60s` | `MIN_ATTRIBUTION_INTERVAL_SECS` (`:12`) |

Review steps required on a violation:
- Explain why the limit changed
- Update this document
- Confirm log rotation still works

#### Markdown cache budget

An earlier TUI rendered markdown and diagrams and put explicit caps on its highlight, render, source, and image state caches. The shipped
`horon-tui` draws plain styled text and holds none of those caches, so there is no cap to keep here. When a corresponding cache comes back,
restore the cap and its reasoning in this section.

### Ratchet expectations

#### Session and transcript memory

Source: `crates/app-core/src/server/debug_server_state.rs:660-760`

These are not strict caps yet, but the relationship is expected. `agent:memory` and `server:memory` export these summed per session.

| Metric relationship | Expectation |
|---|---|
| `session_message_count` vs `session_provider_cache_json_bytes` | The provider cache follows the transcript but must not balloon on its own |
| `session_provider_cache_json_bytes` vs `session_json_bytes` | Ordinary chat flow should keep the same order of magnitude |
| `session_provider_cache_tool_result_bytes` vs `session_tool_result_bytes` | The cache must not hold a larger tool result than the original |
| `session_provider_cache_large_blob_bytes` vs `session_large_blob_bytes` | Large blobs should stay only on the original side |
| `session_payload_text_bytes` | A large value needs an explanation. It usually means raw tool output stayed in the session too long |

Review steps required on a violation:
- Show the memory profile before and after the change
- Explain which retention path grew
- Fix the duplication before raising the budget

#### Runtime memory log expectations

Source: `crates/base/src/runtime_memory_log.rs`

The runtime memory log is a regression detector, not a debug feature.

Expected behavior:
- The server and client logs alone should explain large changes in these items.
  - Session and transcript totals
  - Provider cache totals
  - Background job totals
  - Side panel totals
- A new large memory owner must emit an attributable signal rather than show up only as unexplained RSS growth

Review steps required on a violation:
- Add or improve attribution before accepting the memory increase

### Review checklist for memory-affecting changes

When changing code that uses a lot of memory, record the following and ship it with the change.

1. Which counters changed?
- `agent:memory` for session and provider cache behavior
- `server:memory` for whole-server attribution

2. Did a hard cap change?
- If so, explain why the old cap was insufficient

3. Did duplication grow?
- The original transcript
- The provider cache
- The materialized provider view
- Display copies
- Side panel copies

4. Was observability sufficient?
- If memory grew, can the logs and profile explain where?

### Current initial budget summary

The concrete limits the code enforces today are these
(`crates/base/src/runtime_memory_log.rs`).

- Server and client log files: 90 each
- Pending events: 64, pending categories: 8
- Process sample interval floor: 15 seconds, attribution sample interval floor: 60 seconds

Changing one of these limits on purpose means updating this document in the same PR.

## Idle session hibernation: releasing derived caches in idle sessions

Status: active behavior
Updated: 2026-11-07

The daemon keeps sessions resident between turns, so the derived caches one idle session holds pile up with the session count. This section
defines when, what, and under which conditions it releases the part that can be rebuilt from the originals. The code is
`crates/app-core/src/agent/idle_hibernation.rs` (release) and
`crates/app-core/src/server/idle_hibernation.rs` (the 10-second sweep).

### Configuration

| Item | Default | Other values |
|---|---:|---|
| `[server] idle_hibernation_secs` | `1800` | `0` turns the sweep off. `HORON_AGENT_IDLE_HIBERNATION_SECS` overrides it for one process |

The sweep rereads the threshold every 10 seconds, so editing the config file applies without a restart. After the threshold passes, the actual
release is up to one tick late, and a session refused by a gate is retried after a full threshold rather than every tick. With `0`, the
per-session idle window state is cleared too.

### What is released

`hibernate_idle_caches` drops only these two things.

- **provider projection**: through `release_provider_messages_cache()` it clears `provider_messages_cache`,
  the prefix hash cache, the cache length, and their memory profile counters. It resets the cache mode to `Full`, so the
  next `provider_messages()` rebuilds them instead of sending an empty transcript.
- **the tool output index**: it replaces `tool_call_ids` and `tool_result_ids` with new `HashSet`s instead of `clear()` so the
  allocation itself is released, and resets `tool_output_scan_index` to 0.

### What is kept

Release touches only derived caches and never persistent or original state.

- The original transcript and event originals (`events.jsonl`, `blobs/`), and the resident active context
- The frozen system prompt and the locked tool surface (`listed_tools_index`). The next turn does not renegotiate the tool list.
- `pruned_tool_results` (pruning decisions), `stream_rule_ledger` (the stream rule ledger), and the original auxiliary state in `event_state`
- Rewind undo state, provider handles, pending alerts, and resume state

The release path does not save or checkpoint. So this path cannot mask a save the migration lease skipped.

### Gates

The sweep and the release proceed only when all of these hold.

- The session roster state is not `running` and the session has no turn in progress
  (`turn_cancel_registry`)
- There is no migration lease, `pending_alerts` and `pending_resume` are empty, and the soft interrupt queue depth is 0
- The compaction manager can be taken with `try_read` and no compaction is running
- No tool is running (`running_tool_registry`), and none of the tools the active context names is in flight. If a tool that went
  to the background is still there, it is left alone because recovery on the next turn would produce the result twice.
- Both the last activity (`session_metrics`) and the time since the last state change are at or past the threshold. A session with no
  recorded activity counts as passing the activity condition. A session released once is not released again until its state changes.

If any of the roster lock, session lock, or Agent lock cannot be taken, the tick is skipped without waiting. The sweep must not queue ahead of a
turn. If anything was released, `release_retained_heap` is called once after every lock is dropped, and that one log line records the session
list, the released provider cache bytes, the tool index entry count, and RSS before and after.

### On wake

Release creates no disk dependency, no lifecycle hook, and no environment re-snapshot. The next turn only rebuilds the two caches from the
resident active context, and that turn's request body carries the same tool pairs as before the release
(`idle_hibernation_next_turn_sends_original_tool_pairs_without_duplicates`).

### How much it actually saves

horon already drops the provider projection right before every request (`turn_loops.rs`,
`turn_streaming_mpsc.rs`). So what remains between turns is usually the tool index allocation, and the projection an idle session had picked up
again (re-materialized by a compaction check or a memory profile read) disappears with it. In other words, hibernation's share is the tool
index plus a rebuilt projection, and in a session where `server:memory`'s
`session_provider_cache_*` is already 0 only the tool index entry count is observable. Both values are confirmed through the log line above
and `server:memory`.

## Allocator A/B: glibc stays the Linux default

Measured on 2026-11-10 with the `compare` suite in GitHub Actions (run 42748612662, Linux x86_64, 4 CPUs, 5 samples per cell). `horon` was built with `--features jemalloc` and `horon-base` was the same commit with the default glibc allocator, both in the same job (`docs/performance/BENCHMARK.md`, "Allocator A/B").

| Cell | glibc (`horon-base`) | jemalloc (`horon`) | Reading |
|---|---|---|---|
| One-shot time (median) | 65.3 ms | 64.6 ms | tie (ranges overlap) |
| One-shot peak tree RSS | 31.8 MB | 37.8 MB | jemalloc 1.19× more |
| MCP call time (median) | 86.0 ms | 84.4 ms | tie |
| First session RSS (idle) | 36.8 MB | 39.2 MB | jemalloc more |
| Each additional session (idle) | 75.2 KB | 268.7 KB | jemalloc more |
| Each additional session (post-turn) | 354.5 KB | 487.2 KB | jemalloc more |
| Install size | 110.9 MB | 111.4 MB | jemalloc more |

Time is a tie and every memory cell is worse with jemalloc, so the Linux default stays glibc. The `jemalloc` feature remains available for profiling (`jemalloc-prof`). Re-run the A/B before revisiting this.

## HoronAgent server memory incident runbook

Status: the runbook in use today
Updated: 2026-07-14

This runbook answers two questions.

1. What is using the horon-agent server's memory?
2. What is the safest next action for that cause?

The goal is not to react just because RSS is high. Before changing or stopping anything, separate live application state, memory the allocator
holds, and non-heap mappings.

### Classification with one command

Run this.

```bash
horon debug 'server:memory-incident'
```

It is the first command to run during an incident. It is deliberately light. It does not lock or serialize every Agent record, so it works
even with thousands of sessions up.

The report contains:

- RSS, PSS, anonymous PSS, allocator live bytes
- PSS growth over 15 minutes
- Live, headless, detached, and connected session counts
- Resident sessions counted by state
- The swarms with the most resident Agents
- Severity and the primary cause classification
- A list of actions ordered by cause

Store the JSON output in the incident notes before changing state.

### Offline timeline analysis

Runtime memory logging is on by default and writes daily JSONL files to this path.

```text
~/.horon/logs/memory/
```

Analyze the most recent server process lifetime.

```bash
python scripts/perf/analyze_memory_log.py --days 1
```

The analyzer picks the most recent server and client process instances by default. That matters because comparing PSS across a server restart
produces a spike that does not exist. Use `--all-instances` only for forensics across instances.

List the recorded process lifetimes and pick the pre-restart incident yourself.

```bash
python scripts/perf/analyze_memory_log.py --days 1 --list-instances
python scripts/perf/analyze_memory_log.py --days 1 --instance <server-instance-id>
```

For after-the-fact analysis, `--instance` is recommended. It keeps one process lifetime, without mixing the high-memory server with the
low-memory server that followed it.

Machine-readable output:

```bash
python scripts/perf/analyze_memory_log.py --days 1 --json > /tmp/horon-agent-memory-analysis.json
```

### Severity thresholds

The built-in incident report starts from these operational thresholds.

| Signal | Warning | Severe |
|---|---:|---:|
| PSS | 1 GiB | 2 GiB |
| PSS growth over 15 minutes | 256 MiB | 1 GiB |
| Resident Agent sessions | 128 | 512 |

Crossing a threshold starts an investigation. It does not mean a destructive cleanup is allowed.

### Decision tree

#### 1. `runaway_live_session_population`

Evidence:

- The live Agent count is high or growing fast
- Headless or detached sessions far outnumber connected clients
- Allocator live bytes grow with the session count
- One or more swarms take a large share in `top_live_swarms`

Actions:

1. Stop whatever creates sessions or put a cap on it.
2. Run `horon debug 'swarm:list'` and look at the largest live swarm.
3. Remove workers that are no longer needed with `swarm list` and `swarm cleanup` on the owning coordinator.
4. Do not delete sessions blindly. First check whether discarding in-progress work is acceptable.
5. Run `server:memory-incident` again. Live sessions, allocator live bytes, and PSS must fall together.
6. Only if released memory is still held high after that, run `allocator:purge`.

Reason: the allocator purge cannot release a live Agent runtime.

#### 2. `allocator_retention`

Evidence:

- Resident memory the allocator is estimated to hold is 256 MiB or more
- Held resident memory is 25% or more of PSS
- Allocator live bytes are clearly below anonymous PSS

Actions:

```bash
horon debug 'server:memory-incident' > /tmp/before.json
horon debug 'allocator:purge'
horon debug 'server:memory-incident' > /tmp/after.json
```

If PSS drops a lot, that memory was held by the allocator. If it grows back repeatedly, look at allocation churn and allocator decay instead of
raising the memory budget.

#### 3. `session_payload_growth`

Evidence:

- Tracked records, provider cache, tool, and blob bytes explain more than half of allocator live memory
- One or more sessions take a large share in `top_by_json_bytes`

Actions:

1. Run `horon debug 'server:memory'` to see the full attribution.
2. Look at the provider cache, tool results, large blobs, and payload text.
3. Do one of compaction, summarization, pruning, or moving large outputs out of the body.
4. Add or tighten a hard cap before accepting a larger steady state.

#### 4. `unattributed_live_heap`

Evidence:

- Allocator live bytes exceed 1 GiB
- The session count and allocator retention cannot explain the heap
- Live heap attribution coverage stays below 50%

Actions:

1. Capture `server:memory` and the runtime log analysis.
2. If a missing owner is clear, add a counter for that owner.
3. If ownership is still unclear, use a `jemalloc-prof` build.

```bash
horon debug 'allocator:profile:on'
horon debug 'allocator:profile:dump /tmp/horon-agent-server.heap'
```

A stock system-allocator build cannot produce allocation stack profiles. Do not conclude heap ownership from RSS alone.

#### 5. `non_heap_or_mapping_growth`

Evidence:

- PSS is high but allocator live bytes are not
- File-backed, shared memory, and thread stack mappings are growing

Actions:

```bash
cat /proc/<server-pid>/smaps_rollup
pmap -x <server-pid> | sort -k3 -nr | head -40
ps -T -p <server-pid> -o pid,tid,%cpu,time,comm,wchan:32
```

Look at model mappings, shared memory, thread creation, and large anonymous mappings outside the allocator.

### Escalation steps

Use the cheapest reliable evidence first.

1. `server:memory-incident`, usually under a second and non-blocking
2. The runtime JSONL analyzer, process lifetime trends and incident classification
3. `server:memory`, per-Agent attribution and expensive
4. An allocator purge A/B test, only for held memory
5. A jemalloc heap profile, only for unexplained live heap
6. Correlating OS mappings with a CPU profiler

### Required incident record

Store these.

- Server ID, version, git hash, uptime
- PSS, anonymous PSS, allocator live, held resident bytes
- Live/headless/connected session counts
- Top live swarms and counts by state
- The 15-minute growth
- The action chosen and measurements before and after
- Whether in-progress work was preserved

### Resolution criteria

The incident is resolved only when one of these is true.

- The confirmed live owner shrank and PSS fell with it
- An allocator purge proved held memory and the cause of recurrence was fixed
- Mapping growth was confirmed and a cap was put in place
- A heap profile found the owner and a regression test or cap was added
- The higher steady state was shown to be intended, documented, and given an explicit budget

Do not close an incident on "memory dropped after a restart" alone. A restart erases evidence and reveals no cause.