# Performance: measured results and their limits

Language: **English** | [한국어](RESULTS.ko.md)

This document records the measurements the `agent-runtime-performance-2` plan left on baseline B0 (the commit immediately before the plan started). The plan landed 19 performance commits on `dev`, and the same range holds one `fix(cli)` commit unrelated to this plan that has no bearing on the numbers below. The measured surfaces are the session store's hashes and history pages, the SSE parser, replication export, the result cache's threshold decision, and the attach/detach soak. Every measurement was taken on this machine (Apple M1 Max, debug test profile) by alternating before and after on the same machine.

All raw artifacts live under `.omo/evidence/agent-runtime-performance-2/`, a path ignored by `.gitignore:19`. The plan asked for the key numbers to be copied into the PR bodies, but in practice most PR bodies were one line. So anyone who clones this repository today cannot see a single measurement from this work. This page moves that record somewhere tracked; every number is copied verbatim from the evidence files above.

## At a glance

The charts are the reading; every exact number sits in the table of the section it belongs to, folded under a summary line so the tables never stand between the charts.

![Session store: one request, before and after the plan](charts/record-01-session-store.svg)

![fsync calls per one-shot prompt on the GitHub Linux runner](charts/record-05-fsync-trend.svg)

- One history page from a 100k-event session reads 18,554 B instead of 62,169,922 B and takes 5.9 ms instead of 26.2 s; a 200 MiB source hash peaks at 65,600 B instead of 268,435,520 B. Time and read bytes are the same as before.
- fsync calls per one-shot prompt on the GitHub Linux runner fell from 40 cold and 35 warm to 15 and 14 across the plans below. jcode stays at 5, because it does not commit conversation records; that gap is structural, not a tuning target.
- On macOS, one-shot went from 872.4 ms to 290.4 ms and start→first request from 236.4 ms to 78.7 ms, measured by alternating both builds on one machine. jcode's one-shot on the same runs was 294.8 ms.
- Linux one-shot went from 207.9 ms to 168.0 ms, still behind jcode's 146.3 ms. The gap on Windows is larger and is not claimed as closed.
- Memory per added session after the `jcode-parity-optimization` plan is 149.6 KB on Linux against jcode's 57.4 KB, down from 254.7 KB.
- No wall-clock improvement was established for the loading plan, and its A/A control says why: two runs of the same binary differ by more than any effect the plan could claim.

## Charts: latest automatic measurement

<!-- bench-charts:start -->

`.github/workflows/benchmark-docs.yml` redraws these charts from the competitor comparison of the automatic measurement below. Do not edit this section by hand. Lower is better in every chart, and horon is the blue bar.

![Benchmark summary](charts/00-summary.svg)

| Metric | Chart |
| --- | --- |
| `--version` time | [01-version.svg](charts/01-version.svg) |
| Install size | [02-install-size.svg](charts/02-install-size.svg) |
| One-shot prompt time | [03-oneshot-time.svg](charts/03-oneshot-time.svg) |
| One-shot prompt memory | [04-oneshot-memory.svg](charts/04-oneshot-memory.svg) |
| Skill invocation time | [05-skill-time.svg](charts/05-skill-time.svg) |
| MCP tool call time | [06-mcp-time.svg](charts/06-mcp-time.svg) |
| Memory with the most sessions | [07-sessions-memory.svg](charts/07-sessions-memory.svg) |
| Memory per additional session | [08-per-session-memory.svg](charts/08-per-session-memory.svg) |
| fsync calls per one-shot prompt | [09-fsync-calls.svg](charts/09-fsync-calls.svg) |
| First session memory | [10-first-session-memory.svg](charts/10-first-session-memory.svg) |

The interactive client is a second binary; these charts are drawn from the `tui` suite only and are never mixed with the headless charts above.

| TUI metric | Chart |
| --- | --- |
| TUI latency | [tui-01-latency.svg](charts/tui-01-latency.svg) |
| TUI memory and size | [tui-02-memory.svg](charts/tui-02-memory.svg) |

<!-- bench-charts:end -->

## Measured results

Dimension labels follow this rule. `MEMORY` is allocation and memory, `TIME` is wall-clock time, `WORK` is iteration counts and byte volumes, and `RESOURCES` is file descriptor, thread and task counts. Dimensions are listed separately so that an improvement in one is not read as an improvement in another.

<details>
<summary>Exact numbers</summary>

| Change | Dimension | Before | After |
| --- | --- | --- | --- |
| Streaming source hash, peak allocation (200 MiB log) | `MEMORY` | 268,435,520 B | 65,600 B (4,092×) |
| Streaming source hash, bytes read | no change | 209,715,200 B | 209,715,200 B |
| Streaming source hash, wall clock | `TIME` (no improvement) | 6,434 ms | 6,404 ms |
| History page, flattened messages (100k events, 20-item page) | `WORK` | 99,999 | 20 |
| History page, bytes read | `WORK` | 62,169,922 B | 18,554 B |
| History page, wall clock | `TIME` | 26.2 s | 5.9 ms |
| SSE, parses of a 12-payload stream | `WORK` | 24 | 12 |
| Replication export, serialization count (64 → 256 event pages) | `WORK` | 129 | 513 (4× the events, 4.0× the serializations) |
| Replication export, bytes against the reference implementation (same 64 → 256) | `WORK` | 8,445,218 → 138,506,722 B (16.4×) | 102,903 → 411,795 B (4.0×) |
| Cache facade, over-limit path (temporary allocation · canonical serialization) | `MEMORY` | 3,146,158 B, 1 time | 0 B, 0 times |
| Cache facade, wall clock | `TIME` (no improvement) | about 19 ms per MiB | about 19 ms per MiB |
| Attach/detach soak, resources after 200 cycles | `RESOURCES` | fd 14, threads 25, tasks 2,141 | fd 14, threads 22, tasks 2,141 |

</details>

A few points are easy to misread from the table alone, so they are set out here.

- The source hash digest is the same as before on all five fixtures (61 KB, 618 KB, 6.2 MB, 62 MB, 200 MiB). The only thing that changed is peak allocation; the bytes read and the time taken are unchanged.
- The history page's `WORK` and `TIME` are two faces of the same request. Fetching one 20-item page from a 100k-event session used to replay the whole session, flatten 99,999 messages and read 62 MB; it now reads only the 20 records the index points at.
- In replication export, a 4× larger page made the old reference implementation grow bytes 16.4×, while the current implementation grows them 4.0×. So the cost is proportional to page length, not log length. Page bytes are byte-identical to B0, and the fixed digests `135cf07d...` (length 240,882) and `4a4855d4...` (length 26,979) reproduce unchanged.
- The cache facade no longer serializes canonical JSON once just to decide whether the cap is exceeded. The 1 MiB boundary is byte-exact (1,048,575 B is cached, 1,048,577 B is skipped). The path that actually caches still serializes once.
- The soak ran 200 cycles, 10 windows, 588.99 s. It measures the increment per window, and the failed rounds, quiescence misses (`quiescence_misses`) and non-returned resources (`non_returning`) are all empty.

### Is one page fetched at the same cost as the log grows?

With an index in place, fetching a 20-item page grew bytes read by only 0.5% when the log grew 100×. Replayed messages are 0 at all three sizes.

![One history page costs the same as the log grows 100×](charts/record-02-page-cost.svg)

<details>
<summary>Exact numbers</summary>

| Events | Log size | Bytes the page read | Wall clock |
| --- | --- | --- | --- |
| 1,000 | 514,853 B | 15,375 B | 4.28 ms |
| 10,000 | 5,168,853 B | 15,415 B | 4.68 ms |
| 100,000 | 51,888,853 B | 15,455 B | 4.80 ms |

</details>

The log sizes in this table come from sessions the growth test builds itself, so they differ from the 100k-event fixture in the previous table (62,155,848 B). It is not the same measurement.

### Commits the table does not cover

The remaining commits changed bounds, the measurement surface or a decision rather than forming a before/after pair. There is a commit that makes NDJSON framing safe at UTF-8 boundaries and adds a frame limit, a commit that moves the hook's stdin write inside its deadline, and a commit that widens tool latency from execute to the whole dispatch. There is also a commit that adds count and byte caps plus a visible overflow to the event stream, a commit that removes the context copy taken before the compression decision, and a commit that moves bridge writes into a bounded queue outside the select loop. The measurement surface changed with a commit that fixes benchmark scripts that never ran because of syntax errors and adds a fixed fixture generator, a commit that adds step counters, allocation probes and a same-machine A/B runner, and a commit that makes the p99 key mandatory. The rest are the decision record rejecting four structural items on measurement, two commits adding the structural gate, the report template and the A/B guide, and a follow-up that cleans up a duplicate closure in the index header check.

## Measured but not adopted

Six items were measured and then rejected. Each item carries the numbers a later attempt would have to beat to overturn that decision.

| Item | Decision | Numbers behind it |
| --- | --- | --- |
| Idle session eviction policy | not adopted | 0.999 fds and 0.998 threads per session, 0.41 MB RSS. The slope does not bend from 1 to 1,000 sessions. The `job_count` that `server:memory` reports is 0 at all five population counts, and the soak returns the session count to 0 |
| `globalEvents` multiplexing | not adopted (protocol change) | 7.4 consumer-side fds and 3.0 threads per watched session. The daemon's own share is 1.0 connection per session, and the bridge attaches one session per connection |
| Narrowing the session map lock scope | not adopted | Removing an unrelated session waits for the whole import. 861 / 5,561 / 11,255 / 24,499 ms at 1 / 20 / 60 / 150 events, ratio 1.00 |
| Replacing the 25 ms interrupt poll timer with notification | not adopted | It would save a worst case of 29.7 ms (27.6 ms on average) at the cost of restructuring a wakeup once per watch-group round |
| Overlapping the `run.stopped` journal write with the close-save (#144, #129) | not adopted (slower) | Windows last request → exit, median against `dev` in the same job: 39.8 → 56.4 ms over 5 samples (run 42759006826) and 35.2 → 42.8 ms over 20 samples (run 42764256757; IQR 33.0–40.9 vs 40.7–45.5). Linux and macOS were ties. #146 later removed the close-save flush itself, which brought the same span to 32.5 ms |
| Resolving the fixed Windows shell environment once per process (#124, #122) | not adopted (tie) | Tool round-trip span on Windows, 20 samples: 74.3 → 73.5 ms (run 42735692366), inside the min–max range. Nearly all of the round trip is the `cmd.exe` start, not the per-call env table or PATH search |

Rejecting something on numbers is not a failure. The plan defined success as "when no improvement shows up, the item ends as not adopted with its reasoning attached. That counts as success too." The last column above is that reasoning.

## What these numbers do not say

This section is the point of this page. Leave out any one of the statements below and the page leads the reader to the wrong conclusion.

- **The source hash improvement covers the hashing stage only, and the store path streams its validation.** `crates/base/src/session/event_commit.rs:120-133` hashes the captured prefix through one 64 KiB buffer and `:148-166` compares the reserved tail in a buffer of the same size, so saving a large log no longer allocates a copy of it. The plan recorded 156,518 B for a 7 KB log and 8,293,580 B for a 553 KB log from a save whose peak did follow the log size; those figures do not describe this path any more.
- **The history page improvement covers repeated paging while the head does not move.** The index is tied to the log's identity stamp (device, inode, length, mtime, ctime), so a single append, import or reset invalidates it. A session that keeps growing therefore pays the old cost once more and rebuilds the index. That rebuild cost was not measured. A single page fetch from a new head is not faster.
- **The cache improvement is allocation, not throughput.** Wall clock is about 19 ms per MiB on both sides. Escaping cost dominates either way.
- **The soak asserts fds, threads, queues, pending requests and tasks. Not RSS.** RSS rose from 68.4 MB to 79.5 MB and then stayed flat (peak 83.7 MB), and is recorded as an observation only.
- **The 200-cycle soak resolves one leaked resource per 20 cycles.** The plan asked for 1,000 cycles; that form now runs weekly in `Attach/Detach Soak` (`.github/workflows/soak.yml`) and its first run was clean, while the pull-request form is the same test at 20 cycles inside the structural gate (`scripts/perf/check_structural.py:136-146`).
- **The structural gate did not assert the frame limit when this plan closed.** The counters the gate read then included no frame count, and the frame limit was held by `crates/harness-api-server/src/framing_tests.rs` alone. The gate now has a `frame` section (`scripts/perf/check_structural.py`): the bridge reader and the SDK reader each report `max_frame_bytes`, `accepted_frame_bytes` and `rejected_frame_bytes`, and the gate requires the cap to equal `frame_max_bytes` (64 MiB) and the smallest refused frame to be exactly one byte above the largest accepted one. It still counts no frames.
- **The `shared_context` map these numbers once covered no longer exists.** It was removed with the rest of the topic-channel store (#299), so the soak no longer samples it and the table above carries no figure for it. The historical reading stands as recorded: 0 at every population count, which meant "nobody used it" rather than "nothing leaks". What survives in the `server:memory` payload is the swarm block beside it — `member_count`, `status_counts`, `swarm_count`, `swarm_membership_count`, `plan_count`, `plan_item_count` and their byte estimates (`crates/app-core/src/server/debug_server_state.rs`) — and that block is what a future soak would sample.
- **The replication export's serialization factor is fixed per page.** It is 513 at the 256-event cap, but export time still grows with the log (278.2 ms for 1,000 events, 2,657.2 ms for 10,000). Bytes are flat within a page, not across the whole export.
- **The cache facade's warm-restore time is a single-shot measurement.** It includes page cache variance, and in the same case the range covers 108.6 / 901.8 ms on the lane and 108.9 / 499.7 ms for the lead's re-run. Only the byte-exact boundary decision is reproducible.
- **The absolute milliseconds of the four not-adopted decisions are not release numbers.** They were taken in the debug test profile in a scratch home; what transfers is the ratio (wait time equals import time, session creation about 1.2 s).
- **The 7.4 fds and 3.0 threads for `globalEvents` are consumer-side costs.** The SDK and the daemon ran in the same process, so both ends were counted together. The 100-session and 1,000-session numbers are arithmetic on this per-session value, not a second measurement.

## How to reproduce

Give each measurement its own `CARGO_TARGET_DIR` and run them one at a time. Two test binaries on one target directory produce a different failure every time, and that failure says nothing about the code.

```bash
export CARGO_TARGET_DIR="$PWD/.target-measure"
```

Measurement logs come from the repository's generator. Below are the 200 MiB log and the 100k-event log.

```bash
python3 scripts/perf/fixtures/gen_event_log.py --bytes 209715200 --seed 20261021 --out /tmp/event-log-200mib/
python3 scripts/perf/fixtures/gen_event_log.py --events 100000 --seed 20261021 --out /tmp/event-log-100000/
```

For the source hash, the probe example prints a full read and a streaming digest side by side.

```bash
cargo build --locked -p horon-agent-base --example source_hash_probe
"$CARGO_TARGET_DIR/debug/examples/source_hash_probe" hash /tmp/event-log-200mib/events.jsonl
```

For the history page, the 100k-event fixture is passed in through an environment variable, and a separate test checks equality with the committed 500-record baseline.

```bash
HISTORY_PAGE_PERF_FIXTURE=/tmp/event-log-100000/events.jsonl \
  cargo test --locked -p horon-agent-harness-api-server --lib history_page_perf -- --nocapture --test-threads=1
cargo test --locked -p horon-agent-harness-api-server --lib pages_match_the_committed_pre_change_baseline -- --nocapture
```

For the SSE parser, the same event sequence and parse count are printed at four chunk sizes.

```bash
cargo test --locked -p horon-agent-provider-openai --lib stream::stream_tool_tests::chunked_replay_yields_the_same_events_at_every_chunk_size -- --nocapture
```

For replication export, tests in the repository check page byte equality and the cap. The QA harness that printed the serialization counts in the table (129 / 513) lives in `.omo/evidence/agent-runtime-performance-2/qa_task14.py` and is untracked, so a reader who clones the repository cannot reproduce those counts directly. The digests and lengths do reproduce.

```bash
cargo test --locked -p horon-agent-session-store replication -- --nocapture
```

For the cache facade, a test prints the serialization counts on both sides of the boundary and an example prints the allocation peak.

```bash
cargo test --locked -p horon-agent-base --lib event_cache -- --nocapture
cargo build --locked -p horon-agent-base --example facade_cache_probe
```

The soak has a 200-cycle default form and a 1,000-cycle variant behind `#[ignore]`. The 1,000-cycle form is what the scheduled `Attach/Detach Soak` workflow (`.github/workflows/soak.yml`) runs weekly; it is not part of `CI Required`, so a retention failure never blocks a pull request, and it opens or updates one tracking issue instead of failing a review.

```bash
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test --locked -p horon-agent --test e2e soak_attach_detach::soak_attach_detach_returns_every_counter_to_baseline -- --exact --nocapture
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test -p horon-agent --test e2e soak_attach_detach_1000_cycles -- --ignored --exact --nocapture
```

The scheduled run is reproducible by hand: `gh workflow run soak.yml --ref dev -f cycles=1000`. Its report is the `soak-attach-detach-report` artifact, and `python3 scripts/perf/check_soak.py <captured log>` renders the same Markdown from a local run.

### The 1,000-cycle baseline on a hosted runner

The first run was the `dev` push after the workflow merged ([run 43600180217](https://github.com/horon-ai/agent/actions/runs/43600180217), commit `af41e0b7`), on `ubuntu-24.04` with `HORON_AGENT_PERF_COUNTERS=1`.

| Measure | Value |
| --- | --- |
| Cycles | 1,000, in 10 windows of 100 |
| Cycle time | 7 min 38 s (13:34:30Z → 13:42:08Z), about 0.46 s per cycle |
| Whole job | 10 min 56 s including the cold build |
| Verdict | `STRUCTURAL_PERF section=soak cycles=1000 fd_retention=0 thread_retention=0 non_returning=0` |
| Report | **Clean.** Every counter returned to its baseline |

| Counter | Baseline → final | Slope per 100 cycles |
| --- | --- | --- |
| `fd_count` | 16 → 16 | +0.000 |
| `thread_count` | 16 → 16 | +0.000 |
| `queue_depth_high_water` | 0 → 1 | +0.100 |
| `pending_requests`, `interrupt_poll_wakeups`, `background_tasks`, `live_sessions`, `connected_clients` | 0 → 0 | +0.000 |

`queue_depth_high_water` is a high-water mark, not a final value: it records the deepest queue seen at any point, so a transient 1 is not retention. The 180-minute job timeout leaves more than an order of magnitude of headroom over the measured run.

How to use the structural gate and the A/B runner, the required keys of the report template, and the procedure for raising a bound are in [`docs/performance/BENCHMARK.md`](BENCHMARK.md). The gate command is `python3 scripts/perf/check_structural.py` and the A/B runner is `scripts/perf/ab_perf.py`.

## Status

The plan has 17 items: 14 are done and 3 are partially done (`[~]`). Partially done means the following.

| Item | Status | Meaning |
| --- | --- | --- |
| 7. NDJSON framing caps for both SDKs | `[~]` | The code and tests are on `dev` and verified. The evidence file this item pointed at disappeared with a worktree and was not reconstructed. The behavior is proven, the evidence trail is not |
| 12. Auxiliary and result cache byte accounting | `[~]` | The code is on `dev`, and the lead re-ran the decisive lines and reproduced them. The lane's raw evidence file is gone, and the file now in its place is a reconstruction that labels itself `recovered` |
| 15. Attach/detach soak | `[x]` | The 1,000-cycle plan item now runs: `.github/workflows/soak.yml` runs it weekly and on demand, and the first run completed 1,000 cycles clean in 7 min 38 s (`fd_retention=0`, `thread_retention=0`, `non_returning=0`, run 43600180217). The 200-cycle form stays the pull-request gate, run at 20 cycles by the structural gate (`scripts/perf/check_structural.py:136-146`) |
| 17. CI structural gate and report template | `[~]` | The gate really runs (42 bounds in `scripts/perf/structural_budget.json`, `EXIT=0`, `EXIT=1` on violation, `EXIT=2` when a measurement is missing, 84 contract tests in `scripts/perf/test_check_structural.py`). Two conditions from the plan remain. There is no frame count counter, and "passes at B0" is structurally impossible (the code that prints the `STRUCTURAL_PERF` line landed after B0). The gate now asserts the frame cap itself in its `frame` section for both readers, and still reads no frame count |

The other 13 items are done.

## Measurements from the `agent-rust-loading-durable-runtime` plan (separate plan, separate baseline)

The table above records the `agent-runtime-performance-2` plan. Below is what the
`agent-rust-loading-durable-runtime` plan, which ran later, left behind; both its baseline commit
and its measurement tools are different. Numbers from the two plans are not compared in one table.

The raw artifacts for this plan are under `.omo/evidence/agent-rust-loading-durable-runtime/`.

### Time improvement: not established

The measurement tools (`scripts/perf/bench_loading.py` and `crates/base/examples/loading_probe.rs`)
are outputs of this plan, so the baseline commit immediately before it does not contain the same
tools. Instead of comparing two different measurement programs, the plan used a control (A/A) that
alternately measured two configurations of the same binary 30 times each
(`aa_control: true`). The result is `improvement_claim: false`.

| Segment | A p50 / p95 (ms) | B p50 / p95 (ms) |
| --- | --- | --- |
| attach_history | 671.2 / 1579.2 | 691.7 / 2047.0 |
| provider_dispatch | 12.0 / 26.5 | 13.3 / 66.2 |
| resume_ready | 309.9 / 733.9 | 299.2 / 675.1 |
| session_save | 906.8 / 1271.5 | 993.2 / 1412.7 |
| skill_catalog | 0.2 / 0.2 | 0.2 / 1.4 |

The spread between two runs of the same binary is larger than any effect this plan could claim, and
it does not lean in one direction. So **no wall-clock improvement is claimed.**

### Structural counters: measured, and the bounds hold

These items are judged by counters instead of time. There are two kinds of evidence, and they
measure different things.

- `python3 scripts/perf/check_structural.py` exited 0, **passing all 45 bounds** (0 failures, 0
  missing). The counters this gate actually reads come in six groups: queue depth, pending
  requests, history page cost, hash buffer, frame limit, and the attach/detach soak.
- The items in the table below are **not** read by that gate. A named test per item asserts each
  one directly.

| Item | Dimension | What changed | Test that pins it |
| --- | --- | --- | --- |
| Store validation | `MEMORY` | Commit validation streams through a fixed 64 KiB buffer instead of allocating the whole file. Source length only adds passes | `hash_stream` |
| Request snapshot | `WORK` | Deep-copying messages in the same request no longer grows with history length | `request_snapshot_reuse` |
| Skill list | `WORK` | At most one overlay scan per request, section cap 49,152 B (UTF-8) | `skill_snapshot_budget` |
| Connection setup | `WORK` | Zero built-in tool constructors called outside core, and the start path does not wait for prewarm networking | `loading_deferred_setup` |
| Session resume | `WORK` | Live session reload and duplicate save during resume removed | `loading_live_resume` |

`MEMORY` and `TIME` are not mixed. None of the items above claims a wall-clock improvement.

### What this plan did not measure

- Release profile numbers. Every measurement is in the debug profile.
- Throughput at multi-device or production scale.
- Provider call speed. Provider network time is outside this plan's scope.

## Measurements from the `agent-irc-bus` plan

These are measurements from the `agent-irc-bus` plan, run separately from the two records above. That plan added the mailbox bus between agents and the `irc` tool; baseline B0 is `50eed12` (the commit immediately before the plan started on `dev`) and the candidate is `c9d9d8c` (wave 3 merge). Numbers in this record are not compared with the two earlier records.

The raw artifacts are in `.hyper/qa/agent-irc-bus/11-happy.txt` (the A/B report JSON, three throughput runs, and the structural gate's `irc` area) and `.hyper/qa/agent-irc-bus/11-failure.txt` (the A/A control). `.hyper/` is ignored by the repository, so this section is the tracked record.

### Measurement conditions

| Item | Value |
| --- | --- |
| Machine | Apple M1 Max, 10 cores, 64 GiB memory |
| Kernel | `Darwin Alpoxs-MacBook.local 25.4.0 ... RELEASE_ARM64_T6000 arm64` |
| Toolchain | `rustc 1.99.1`, `cargo 1.99.1` |
| Build profile | release, no feature set (both sides `cargo build --locked --release -p horon-agent-base --example perf_counters_surface`) |
| Workload | One run of `perf_counters_surface` (a fresh process per run) |
| Load during measurement | Load averages 50.10 / 44.73 / 44.59 (1 / 5 / 15 minutes), 4 `rustc` and `cargo` processes |
| Repetitions | 3 warmups, 10 runs per side, the two sides alternating |
| B0 binary | `48c69b5c...`, commit `50eed12` |
| Candidate binary | `a82fb191...`, commit `c9d9d8c` |

Compilations unrelated to this plan were running in several repositories during the measurement. The observed load averages over that period ranged from 36 to 64, and the `rustc` and `cargo` process count was observed between 4 and 17. So the absolute times in this section are not values from a quiet machine.

### A/B: no regression

`scripts/perf/ab_perf.py` exited 0 and the adoption decision is `not-adopted-no-measurable-change`. It is not `reject-regression`, which would mean a regression; the median difference is inside the noise band, so the value claims neither improvement nor regression.

| Item | B0 | Candidate | Difference |
| --- | --- | --- | --- |
| Median | 5.23 ms | 5.39 ms | +0.16 ms (+3.09%) |
| p95 | 7.06 ms | 7.10 ms | +0.05 ms |
| Noise band (`max(3*1.4826*MAD, 1% of the median)`) | | | ±1.35 ms, inside the band |
| `cpu_user_ms` median | 2.22 ms | 2.18 ms | -0.04 ms |
| `cpu_system_ms` median | 1.55 ms | 1.60 ms | +0.05 ms |
| `peak_rss_bytes` median | 6,045,696 B | 6,029,312 B | -16,384 B |
| `fd_count` / `thread_count` median | 3 / 1 | 3 / 1 | same |
| Failures / timeouts | 0 / 0 | 0 / 0 | |
| `read_bytes` median | 0 B | 0 B | same |

For `allocated_bytes`, `pss_bytes` and `retained_bytes`, which the workload did not report, the runner leaves `null` rather than 0 and lists the three names in `counts.missing`. p99 was refused because there were 10 samples, with the reason that it needs at least 20.

### A/A control: no false regression

The control run, with the same candidate binary on both sides, ended as `control-no-change`. Medians were 6.48 ms and 6.70 ms, a 0.22 ms (3.43%) difference, but inside the ±1.58 ms noise band. So the 0.16 ms difference in the A/B above is smaller than this instrument can resolve, which is evidence that the runner does not invent regressions.

### IRC throughput

`cargo run --locked --release -p horon-agent-irc-core --example irc_throughput` was run 3 times. The message count is the default 100,000 and `broadcast64` rounds down to a multiple of 64, giving 99,968.

![IRC bus throughput inside one process](charts/record-03-irc-throughput.svg)

<details>
<summary>Exact numbers</summary>

| Scenario | Run 1 | Run 2 | Run 3 | Median | Range |
| --- | --- | --- | --- | --- | --- |
| `direct_waiter` (msg/s) | 105,291 | 107,434 | 111,523 | 107,434 | 105,291 ~ 111,523 |
| `direct_deliver` (msg/s) | 2,410,858 | 2,229,008 | 2,392,230 | 2,392,230 | 2,229,008 ~ 2,410,858 |
| `broadcast64` (msg/s) | 3,237,934 | 3,141,178 | 3,141,375 | 3,141,375 | 3,141,178 ~ 3,237,934 |

</details>

`direct_waiter` uses 64 receiver task pairs (`WAITER_PAIRS = 64`) waiting in `wait` at the same time. From the second round on, each receiver task sends one message to its peer that creates a reply obligation and then enters `wait`, and the driver confirms all 64 waiters are registered before sending one message per live pair. Round trips within a pair are ordered, but the 64 pairs run concurrently. `direct_deliver` is a fake delivery path that returns the injection immediately, and `broadcast64` is the path that sends one body to 64 recipients.

todo 6 first measured `direct_waiter` at 39,128 msg/s (`6-happy.txt`, 100,000 messages in 2,555,678 us); after todo 12 removed the timer round trip that every `wait` call carried, this measurement gives 107,434 msg/s. That is about 2.75×. todo 12's own measurement is 124,071 msg/s (`12-happy.txt`), and this value is lower. That looks like a load difference at measurement time, since the code path is the same.

### Structural gate `irc`

`python3 scripts/perf/check_structural.py --only irc` exited 0, passing all five bounds.

| Counter | Observed | Bound |
| --- | --- | --- |
| `mailbox_high_water` | 100 | at most 100 |
| `mailbox_dropped` | 900 | equal to 900 |
| `broadcast_body_allocations` | 1 | equal to 1 |
| `waiters_live_after_churn` | 0 | equal to 0 |
| `broadcast_inflight_high_water` | 16 | at most 16 |

### What these numbers do not say

- **The A/B workload measures process start and counter printing, not the bus.** `perf_counters_surface` does not start a daemon; it prints a counter snapshot once. The IRC bus is off in the default configuration and never reaches this path, so all this A/B says is "no regression in shared code". The throughput table answers for the bus itself.
- **Throughput is measured inside one process.** The example puts `IrcBus` with a fake directory and a fake deliverer in one process. It does not include daemon transport, serialization, session state transitions or model calls. Response time for a real agent conversation cannot be predicted from these numbers.
- **The `direct_waiter` number is sensitive to machine load.** The three runs here range from 105,291 to 111,523, and against todo 6's 39,128 (`6-happy.txt`) that is about 2.75×. The difference between todo 12's 124,071 (`12-happy.txt`) and this value looks like a load difference at measurement time, so the value worth carrying over is the ratio, not the absolute throughput.
- **The A/B absolute times are not values from a quiet machine.** Observed load averages during the measurement ranged from 36 to 64, with `cargo test`, `cargo clippy` and `cargo build` running in unrelated repositories. Because the A/B alternates the two sides and the A/A confirms the noise band, the direction of the comparison holds, but the single-digit millisecond absolute values do not reproduce under other conditions.
- **There is no p99.** With 10 samples the runner refused it, citing fewer than 20.
- **Three metrics were not measured.** `allocated_bytes`, `pss_bytes` and `retained_bytes` are `null` and listed in `counts.missing` because the workload did not report them. Do not read them as 0.
- **The structural gate does not measure time.** The five values above say only that shapes are inside their bounds, not that anything is fast.

## Measurements from the `platform-adapter-perf` plan

These are measurements from the `platform-adapter-perf` plan, run separately from the three records above. That plan gathered OS-specific code into five `crates/platform-*` crates, removed fsyncs for derived caches that can be rebuilt from session storage, built the HTTPS client's TLS configuration once per process, and turned price-list refresh off by default. Baseline B0 is `738712a` (the commit immediately before the plan started on `dev`) and the candidate is `215bfdb` (the work branch). Numbers in this record are not compared with the three earlier records.

The raw artifacts are under `.hyper/qa/platform-adapter-perf/after/`. `.hyper/` is ignored by the repository, so this section is the tracked record.

| File | Contents |
| --- | --- |
| `compare-macos.json`, `compare-linux.json`, `compare-linux-times.json` | Three-line comparison (horon B0, horon candidate, jcode). Linux `compare-linux.json` uses `--syscalls`; time numbers are from `compare-linux-times.json` |
| `fsync-linux.txt` | fsync and fdatasync call counts for one one-shot, cold and warm |
| `ab-*.json` | Same-machine A/B reports (the runner's output JSON as is) |
| `github-baseline/`, `github-candidate/` | Artifacts of GitHub `benchmark.yml` runs 42294971747 (`--ref dev`) and 42294976581 (branch) |
| `structural.log` | Full `check_structural.py` output |

### Measurement conditions

| Item | macOS | Linux | GitHub runner |
| --- | --- | --- | --- |
| Machine | Apple M1 Max, 10 cores | Docker `rust:1-trixie`, linux/aarch64, 10 cores | `ubuntu-latest`, linux/x86_64, 4 cores |
| Toolchain | `rustc 1.99.1` | the container's distro rustc | `stable` (at run time) |
| Profile | release | release | release |
| The two binaries within one measurement | same machine, alternating | same | different runs, different runner instances |
| Competing tool | jcode v0.99.1 (`7206efd0c`) | same | same |

Every line within one measurement talks to the same mock model server, so no API fees or secrets are involved. macOS and Linux measured the B0 binary and the candidate binary within one run; for GitHub, the `dev` run and the branch run were started separately. So in the table below the GitHub column is "two runs", not "before and after".

### Before and after across three environments

one-shot is the median of a single `run` question (startup, one `ls` tool round trip, final response). start→first request runs from connecting to the api-bridge socket to the first `list_sessions` response. fsync is the number of fsync and fdatasync calls a single one-shot makes. Memory is the idle RSS of the daemon process tree.

![Start path on macOS and Linux: before and after](charts/record-04-platform-time.svg)

<details>
<summary>Exact numbers</summary>

| Metric | Environment | B0 `738712a` | Candidate `215bfdb` | Change | jcode | Candidate / jcode |
| --- | --- | --- | --- | --- | --- | --- |
| one-shot median | macOS | 872.4 ms | 290.4 ms | -66.7% | 294.8 ms | 0.99× |
| one-shot median | Linux (Docker) | 207.9 ms | 168.0 ms | -19.2% | 146.3 ms | 1.15× |
| one-shot median | GitHub runner | 185.3 ms | 499.8 ms | separate runs, not comparable | 145.5 ms / 357.8 ms | not comparable |
| start→first request | macOS | 236.4 ms | 78.7 ms | -66.7% | not applicable | not applicable |
| start→first request | Linux (Docker) | 57.0 ms | 57.0 ms | -0.03% | not applicable | not applicable |
| start→first request | GitHub runner | 23.7 ms | 25.3 ms | separate runs | not applicable | not applicable |
| fsync and fdatasync calls | Linux (Docker) | 175 cold / 170 warm | 40 cold / 35 warm | -77% | 19 cold / 5 warm | 7.0× |
| fsync and fdatasync calls | GitHub runner | cannot measure | 40 cold / 35 warm | cannot measure | 19 / 10 / 5 calls | 3.5× |
| First-session idle RSS | macOS | 32.73 MiB | 29.52 MiB | -9.8% | 37.34 MiB | 0.79× |
| First-session idle RSS | Linux (Docker) | 32.05 MiB | 33.37 MiB | +4.1% | 40.29 MiB | 0.83× |
| Increment per session | macOS | 592 KiB | 320 KiB | -46% | 544 KiB | 0.59× |
| Increment per session | Linux (Docker) | 1396 KiB | 944 KiB | -32% | 144 KiB | 6.6× |
| First-session idle RSS | GitHub runner | cannot measure | 39.9 MiB | cannot measure | 43.0 MiB | 0.93× |
| Increment per session | GitHub runner | cannot measure | 319 KiB | cannot measure | 38 KiB | 8.3× |

</details>

The candidate column in this table was measured with the `215bfdb` binary. Of the changes that later landed on the branch, only one touched the start path, the deferred HTTP client (`b6dcfc0`); the time and memory re-measurements are in the "Memory" section below. The 11 fixes from the F2 quality review also left these numbers unchanged, and re-measuring with the same runner gives Linux fsync cold 40 · warm 35 again ("fsync" in `F2-review.md`).

The decision to hold fsync at 40 or below uses the values from the `bench_compare.py --syscalls` runner (cold 40 · warm 35). todo 6's own driver used a different HOME and prompt shape and measured cold 45 · warm 40 on the same candidate binary, and in that run only warm meets the 40 bound ("result against the plan" in `6-linux-fsync.txt`).

Absolute values on hosted runners differ per machine and mean nothing, so the GitHub column says only that "the workflow runs on the branch as-is and fsync counts are recorded too". All three session measurements in the GitHub B0 run failed with `daemon socket not ready`. That run uses `dev`'s `bench_compare.py`, and the fix for the removal of `serve --debug-socket` (todo 13) is still only on the branch. So the session values from GitHub B0 are not 0.

### Same-machine A/B: no regression on the time paths

`scripts/perf/ab_perf.py` runs the two binaries alternately in blocks and reports the noise band `max(3*1.4826*MAD, 1% of the median)` alongside. Only differences outside the band are called an improvement or a regression.

One row is one path. one-shot, start→first request and the session paths go through a small probe that does the setup for them (scratch HOME, mock model address, daemon start), so the absolute values here are larger than in the environment table above by the probe's own startup. Both sides use the same probe, so the direction of the comparison is unchanged.

<details>
<summary>Exact numbers</summary>

| Path | Environment | B0 | Candidate | Difference | Noise band | Verdict |
| --- | --- | --- | --- | --- | --- | --- |
| one-shot | macOS | 1121.85 ms | 435.79 ms | -61.2% | ±93.95 ms | improved |
| start→first request | macOS | 236.36 ms | 78.72 ms | -66.7% | ±10.36 ms | improved |
| Two session paths | macOS | 3295.26 ms | 2259.88 ms | -31.4% | ±131.13 ms | improved |
| `--version` load | macOS | 7.76 ms | 7.52 ms | -3.0% | ±1.36 ms | inside the band |
| `perf_counters_surface` | macOS | 4.74 ms | 4.80 ms | +1.3% | ±1.41 ms | inside the band |
| one-shot | Linux | 265.02 ms | 217.62 ms | -17.9% | ±14.61 ms | improved |
| `--version` load | Linux | 2.60 ms | 2.63 ms | +1.2% | ±0.39 ms | inside the band |
| `perf_counters_surface` | Linux | 0.46 ms | 0.47 ms | +1.6% | ±0.08 ms | inside the band |
| start→first request | Linux | 57.03 ms | 57.02 ms | -0.03% | ±2.93 ms | inside the band |
| One session path | Linux | 14882 ms | 14836 ms | -0.3% | ±148.82 ms | inside the band |
| Two session paths | Linux | 14936 ms | 14858 ms | -0.5% | ±149.36 ms | inside the band |

</details>

Tool latency and startup steps were checked separately on the same machine. Alternating the 14 cases in `bench_tools.py` three times each gives median differences between -15.7% and +5.4%, and the cases that grew are `ls` (+5.4%), `agentgrep` (+1.1%) and `read (repo Cargo.toml)` (+0.9%). For `ls`, B0's three runs are 2.21 / 2.46 / 3.57 ms and the candidate's are 2.58 / 2.59 / 2.64 ms, which falls inside B0's own run-to-run spread. `bench_startup.py` gives a server-ready median of 10.8–10.9 ms (B0) and 11.0 ms (candidate), and a total bridge time of 18.1–18.6 ms and 17.9–18.6 ms, which overlap. The 20–40% spread across tool cases unrelated to the change in the GitHub candidate run is a runner instance difference and does not reproduce on the same machine.

macOS dropped sharply because the two costs this plan targeted existed only on macOS. The path that took `F_FULLFSYNC` on every save, derived caches included, now syncs the source only, and the path that read a keychain certificate per reqwest client became one construction per process. As a control, `--version` load and `perf_counters_surface` are inside the band in both environments. Neither path touches storage or TLS, so they are exactly where a regression in shared code would have shown up first.

### Memory: the first Linux daemon RSS numbers, and what re-measuring showed

The time paths got slower nowhere. Memory was first recorded as an increase in Linux daemon RSS (`rss`), and only later was it established that the measurement method itself cannot compare two builds. The first numbers come first, then the re-measurement and the fix below them.

<details>
<summary>Exact numbers</summary>

| Measurement | Environment | B0 | Candidate | Difference |
| --- | --- | --- | --- | --- |
| Daemon idle, 0 sessions | macOS | 27.82 MiB | 24.09 MiB | -3.73 MiB (-13.4%) |
| Daemon idle, 1 session | macOS | 32.34 MiB | 28.95 MiB | -3.39 MiB (-10.5%) |
| Daemon idle, 0 sessions | Linux | 27.88 MiB | 29.01 MiB | +1.13 MiB (+4.1%) |
| Daemon idle, 1 session | Linux | 31.74 MiB | 32.75 MiB | +1.01 MiB (+3.2%) |
| one-shot peak RSS (8 runs) | Linux | 32.46 MiB | 32.87 MiB | +0.41 MiB (+1.3%, ranges overlap) |
| Same-binary control (A/A) | Linux | 27.83 MiB | 27.87 MiB | +0.03 MiB (+0.1%) |

</details>

The Linux difference is in the executable's resident pages, not the heap. Summing `/proc/<pid>/smaps` by mapping type for the 1-session state gives the following.

| Mapping | B0 | Candidate | Difference |
| --- | --- | --- | --- |
| Executable `r-xp` | 26176 KiB | 27136 KiB | +960 KiB |
| Executable `r--p` | 1508 KiB | 1508 KiB | 0 |
| Anonymous `rw-p` | 1796 KiB | 1730 KiB | -66 KiB |
| Everything else | 1784 KiB | 1784 KiB | 0 |

Running the same measurement once more gives an `r-xp` difference between +960 and +1472 KiB and an anonymous difference between -66 and +156 KiB. The direction is the same both times and the size is overwhelmingly on the executable side. With no sessions the direction is the same, `r-xp` +1152 KiB and anonymous +10 KiB. Minor page fault medians are 1574 and 1584, effectively equal, and major page faults are 0 on both sides. The binary grew 276,928 B and `.text` grew 100,576 B, so the added residency is larger than the file size. That means more code pages are actually touched. The per-session increment, by contrast, fell from 1396 KiB (B0) to 944 KiB.

#### Re-measured: the difference was page cache, not code

The first numbers came from an A/A control that writes the same file on both sides, so they only guarantee "this one binary measures repeatedly". Building the same source twice independently and measuring with the same method moves more than that difference.

| Control | rss median | `r-xp` median |
| --- | --- | --- |
| Same commit, build 1 vs build 2 | 22.22 vs 21.63 MiB | 22068 vs 22028 KiB |
| Same commit, build 1 vs build 3 | 22.22 vs 26.33 MiB | 22068 vs 21948 KiB |
| Candidate commit, build 1 vs build 2 | 27.00 vs 23.17 MiB | 22576 vs 22716 KiB |
| Same bytes, different path | 14.39 vs 26.85 MiB | 11276 vs 24076 KiB |

The cause is that resident `r-xp` pages are not executed code but page cache folios the kernel faulted in alongside it. The median length of a resident run is 32–48 pages (64 KiB folios) while the executed run is 1–2 pages. Two builds of the same source therefore differ by up to 8 MiB in residency.

The measurement was changed to values the cache cannot move. It counts the 4 KiB code pages executed in a callgrind instruction dump (`--dump-instr=yes`), and `rss` was measured after dropping the page cache with `posix_fadvise(DONTNEED)` before every start and pinning `fault_around_bytes` to 4096.

| Build | Executed code pages | `rss` median (controlled) |
| --- | --- | --- |
| Baseline 738712a #1 | 1765 | 26.42 MiB |
| Baseline #2 | 1765 | 26.37 MiB |
| Candidate f5f5868 #1 | 1771 | 26.92 MiB |
| Candidate #2 | 1778 | - |
| Candidate #3 | 1771 | - |

The build-to-build spread within one side (baseline 1759–1765) equals or exceeds the difference between the sides (1765 vs 1771). So the "+1.0–1.3 MiB of resident executable code" recorded first is not a regression attributable to this change.

#### What was actually fixed: building the TLS trust store at startup

`serve` builds the tool registry at startup. At that point `WebFetchTool::new`, `WebSearchTool::new`, `DiscoverToolsTool::new` and `NotificationDispatcher::new` each held `shared_http_client()`, and building that client means parsing all 449 trust-store anchors.

    shared_http_client -> ClientBuilder::build -> openssl_probe::probe
      -> rustls_native_certs::load_pem_certs
      = 83% of startup instructions, 194 of 878 executed code pages

The four constructors no longer hold the client. `shared_http_client()` is already a process-wide `OnceLock`, so calling it at the point where a request is sent is enough.

| Side | Executed code pages | `rss` median | `r-xp` median |
| --- | --- | --- | --- |
| Baseline | 1765 | 26.42 MiB | 22068 KiB |
| Candidate before the fix | 1771 | 26.92 MiB | 22576 KiB |
| Candidate after the fix | 1755 | 26.59 MiB | 22208 KiB |

After the fix the executed code pages are below the baseline (1755 vs 1765). The remaining `rss` difference of +0.17 MiB is inside the same-source build-to-build spread seen above (up to 8 MiB in the controlled measurement, 0.6 MiB within the same pair) and does not appear as an executed-code difference.

The rest of the start path is the same on both sides. System calls up to idle are 5416 vs 5428 (46 of 53 kinds identical) and files opened are 364 vs 365.

### Structural gate

`python3 scripts/perf/check_structural.py` exited 0, passing all 55 bounds (0 failures, 0 missing). This gate does not read wall-clock time, so it holds a different axis from the time numbers above.

### What these numbers do not say

- **The two runs on GitHub runners are not a before and after comparison.** They are different hosted runner instances, and only the branch workflow turns on `--syscalls`, so only the candidate run has strace attached. In the candidate run even binaries unrelated to this change got uniformly slower: jcode 145.5 → 357.8 ms, Claude Code 452.1 → 913.9 ms, Codex CLI 369.4 → 2273.1 ms, OpenCode 2.98 → 5.81 s, pi 928.4 → 2109.5 ms. Unchanged executables got more than twice as slow, so the difference between the two runs comes from the runner and the instrumentation, not from this plan. The strace cost measured on the same Docker machine was +255 to +259 ms per one-shot, and tool latency in the candidate run is uniformly higher even for unchanged cases (`bash (true)` 2.2 → 3.1 ms, `read (tiny file)` 0.5 → 0.6 ms). This plan's regression verdict uses the two environments measured on the same machine, macOS and Docker Linux.
- **macOS's -66.7% covers only this plan's two costs.** `--version` load and `perf_counters_surface`, which go through neither the store path nor the client construction path, are inside the band. Model latency, tool execution time and prompt assembly time are not part of this number.
- **fsync call counts are only counted in a Linux container.** macOS has no `strace`, and this plan did not change macOS's default syncing (a plain fsync, only with the setting `[storage] sync = "fast"`).
- **The first Linux daemon RSS reading of +1.0–1.3 MiB was not a regression.** It was measuring page cache folios the kernel faulted in alongside the code, not the code that ran. Two builds of the same source vary more than that, so the number cannot decide this change. Re-measured with executed code pages, which cache cannot move, the candidate is below the baseline (1755 vs 1765 after the fix) and the controlled `rss` difference is inside the same-source build spread. The path that built the TLS trust store at startup was found and removed instead (see "What was actually fixed" above).
- **First-session RSS and the per-session increment are different axes.** The Linux candidate is +1.3 MiB higher on the first session while the per-session increment is -452 KiB lower. The two values are not merged into one "memory" figure.
- **`perf_counters_surface` does not start a daemon.** All this workload says is that there is no regression in shared code; the cost of the store path or the TLS path is answered by the one-shot table.
- **There is no p99.** With 12 or fewer samples the runner refused it, citing fewer than 20.

## Measurements from the `cache-context-session-opt` plan

This plan puts a `prompt_cache_key` and a retention period on OpenAI-compatible requests (`ee9f46bc`), stops recomputing that key for the same input (`263ab39e`), brings the compression tail into the recovery band (0.8 of the baseline) (`6e7f5052`), and merges a batch save's log syncs into one at the end of the batch (`626eebb3`). Performance is measured only on GitHub Actions. The candidate is `dev`'s `e95b8773`, and the measurement is the 2026-11-07 `benchmark.yml` run 42371020359 (`--ref dev`, linux, macos and windows all succeeding) plus the `structural-perf.yml` run 42371014382 (succeeding). The comparison baseline is the same-runner values recorded in the `platform-adapter-perf` section above.

| Metric | Environment | Earlier record | Candidate `e95b8773` | Change |
| --- | --- | --- | --- | --- |
| one-shot fsync and fdatasync calls (3 samples) | GitHub Linux runner | 40 cold / 35 warm | 32 cold / 27 warm | -20% cold, -23% warm |
| Of which fsync / fdatasync (cold) | GitHub Linux runner | not applicable | 16 / 16 | not applicable |
| Multiple against jcode (warm) | GitHub Linux runner | 7.0× (35 / 5) | 5.4× (27 / 5) | gap narrowed |
| one-shot time median | GitHub macOS runner | not applicable | 175.52 ms (jcode 341.8 ms) | not applicable |
| one-shot time median | GitHub Windows runner | not applicable | 269.02 ms (jcode 175.37 ms) | not applicable |
| Structural performance gate | GitHub Linux runner | passes | passes | no regression |

The calls that went away are log-line fdatasyncs. That a 3-message batch save's `LogFileSync` dropped from 3 to 1 is pinned by `crates/session-store/tests/batch_sync.rs`.

### What these numbers do not say

- **The one-shot time has 3 samples.** This section only records the current value per OS; it is not an A/B against earlier time values on the same runner. The automatic measurement section below keeps re-measuring the same suites on `main`.
- **The effect of the cache key is not here.** The benchmark uses a mock model server, so no cache hits occur. Hits show up only as `cached_tokens` from a real model vendor. Key computation cost is memoized so that it costs one comparison as long as the input does not change within a session (`crates/provider-openai/src/chat/prompt_cache_memo.rs`).
- **The compression recovery band is not a time metric.** Unit tests confirm that the tail left by a 24k window drops from 10,800 to 3,625 tokens while the 200k window stays at 140,000 / 20,000.

## Measurements from the `jcode-parity-optimization` plan

This plan skips the price-list read and the second config parse on default runs (`76a510fd`), replaces the per-session store thread with a shared writer pool of at most 4 (`c75dd149`), commits the dirty marker with a single directory fsync (`af13232d`), and drops the derived cache of sessions idle for more than 30 minutes (`1266059d`). Performance is measured only on GitHub Actions. The `benchmark.yml` `baseline_ref` input measured the `main` baseline (`b62a79c9`) and the candidate **inside one job, on the same runner, against the same mock model server, alternating**, so before/after below is a same-machine A/B. The runs are 42388003895 from 2026-11-08 (linux and macos, sessions 1·10·100, 3 turns, one-shot 3 samples) and 42389421614 (macos, one-shot 20 samples).

![Per-session cost after the jcode-parity plan](charts/record-06-per-session-memory.svg)

<details>
<summary>Exact numbers</summary>

| Metric | Environment | Before `b62a79c9` | After | jcode v0.99.1 |
| --- | --- | --- | --- | --- |
| Memory slope per added session | GitHub Linux runner | 254.7 KB | 149.6 KB | 57.4 KB |
| Memory slope per added session | GitHub macOS runner | 249.6 KB | 185.1 KB | 153.4 KB |
| Threads per session | GitHub Linux runner | 1.14 | 0.16 | 0.07 |
| Threads per session | GitHub macOS runner | 1.34 | 0.26 | not measurable |
| Tree RSS after a 100-session turn | GitHub Linux runner | 96.2 MB | 79.2 MB | 65.5 MB |
| Tree RSS after a 100-session turn | GitHub macOS runner | 84.5 MB | 75.4 MB | 70.1 MB |
| one-shot fsync and fdatasync calls (cold, first sample) | GitHub Linux runner | 32 | 27 | 19 |
| one-shot fsync and fdatasync calls (warm, last sample) | GitHub Linux runner | 27 | 22 | 5 |
| one-shot time median (3 samples) | GitHub Linux runner | 367.0 ms | 363.9 ms | 288.7 ms |
| one-shot time median (20 samples) | GitHub macOS runner | 219.3 ms | 215.8 ms | 312.9 ms |

</details>

Fds per session are 1.00 before and after in both environments.

### What these numbers do not say

- **The 3-sample macOS median looked like a regression.** In 42388003895 the after value was 384.3 ms and the before value 254.4 ms, but the samples were spread across 616·384·245 ms (after) and 594·254·235 ms (before). Repeating the same comparison with 20 samples in 42389421614 gave medians of 215.8 against 219.3 ms and minima of 174.7 against 173.5 ms, with no difference. That is why `benchmark.yml` gained the `oneshot_runs` input.
- **The gap with jcode still exists.** On Linux, memory per added session is about 2.6× jcode's and one-shot is 1.26× slower. This plan narrowed the gap rather than removing it.
- **The per-session thread target (slope below 0.1) was met only on Linux.** The slope across the 1·10·100 session points was 0.16 on Linux and 0.26 on macOS. In 42397804493, which adds session 500 (same-runner A/B, one-shot 20 samples), Linux is 0.05 (was 1.04, jcode 0.02) and macOS is 0.40 (was 1.45). On Linux the three-point value was noise from runtime threads at low session counts, but macOS stays above 0.1 even at 500 sessions. The writer pool is at most 4, so the remaining macOS slope comes from somewhere else (it looks like the daemon's blocking worker threads); the cause was not confirmed. Tree RSS after a 500-session turn is Linux 289.2 → 189.9 MB (jcode 159.9 MB) and macOS 226.8 → 161.7 MB (jcode 156.8 MB).
- **macOS one-shot medians point in opposite directions across two measurements.** With 20 samples, 42389421614 gives 215.8 after against 219.3 ms before, and 42397804493 gives 199.4 against 188.7 ms (+5.7%); the minima were the same both times (174.7 against 173.5, 168.7 against 168.6 ms). Linux in the same run is 410.6 against 407.6 ms. It was not judged a regression, but it does mean the hosted runner's noise band is about ±6%.
- **The per-path A/B in `ab_perf.py` was not run.** This plan's time non-regression verdict rests on the one-shot table above (same runner, alternating) and the structural performance gate; per-path A/B for `perf_counters_surface` or start→first request is not in the record.
- **The effect of hibernation is not in the table above.** The benchmark rests briefly right after a turn and then measures, so the 30-minute window never passes. Whether release actually happens is checked by `crates/app-core/src/server/idle_hibernation_tests.rs`.

## Measurements from the `skill-mcp-loading-omp` plan

This plan changed how the skill list, work-folder skills and MCP user config load. The verdict comes from a single run of `.github/workflows/loading-ab.yml`. Numbers measured on a development machine were not used for the verdict.

### Measurement conditions

- Run: [run 42461323656](https://github.com/horon-ai/agent/actions/runs/42461323656), 2026-11-08, both sides built in release and alternated within one `ubuntu-24.04` job.
- Baseline `23007c9b` (the commit where the loading counters first landed), candidate `83f2b6c7`. Toolchain `rustc 1.99.1`.
- `bench_loading.py --mode warm --events 0 --skills 0,200,1000`, 3 warmups, 30 measurements each. Two conditions: an 8 KiB body with a 300 KB `~/.claude.json`, and both at 0. The fixture turns on `[mcp] claude_user = true`, so both sides read the same file.
- One measurement step is 9 requests (`REPEAT_REQUESTS`).

### Counters: all three conditions hold

| Fixture (8 KiB body, 300 KB MCP) | Counter | Baseline | Candidate |
| --- | --- | --- | --- |
| 200 skills | `skill_catalog` `skill_bytes_read` | 1,656,290 | 819,200 |
| 1000 skills | `skill_catalog` `skill_bytes_read` | 8,281,890 | 4,096,000 |
| 0, 200 and 1000 skills | `mcp_config_load` `mcp_config_reads` | 9 | 0 |
| 0, 200 and 1000 skills | `skill_request_overlay` `skill_file_reads` | 0 | 0 |

The `skill_bytes_read` values for the 0-body condition (23,490 / 117,890) are the same on both sides. The files have only a front matter and no body to save. `skill_request_overlay` file reads are 0 on the baseline too. The baseline rewrites the same list within the request scope and instead rescans the folder on every request. That cost shows up in the times below.

### Time (p50, p95 in parentheses, ms)

![Skill and MCP loading: baseline and candidate in one job](charts/record-07-skill-mcp.svg)

<details>
<summary>Exact numbers</summary>

| Step | Condition | Baseline | Candidate |
| --- | --- | --- | --- |
| `skill_request_overlay` | 200 skills, 8 KiB body | 28.30 (29.09) | 4.79 (4.96) |
| `skill_request_overlay` | 1000 skills, 8 KiB body | 149.03 (153.92) | 26.46 (27.83) |
| `skill_catalog` | 200 skills, 8 KiB body | 4.41 (4.53) | 3.00 (3.11) |
| `skill_catalog` | 1000 skills, 8 KiB body | 21.88 (22.64) | 15.00 (15.32) |
| `skill_catalog` | 1000 skills, 0 body | 15.71 (15.98) | 14.82 (15.28) |
| `mcp_config_load` | 300 KB MCP | 3.46 (3.71) | 0.22 (0.26) |

</details>

The candidate medians for all 14 cases in `bench_tools.py` equal or beat the baseline (5 samples each, same job).

### Fixed during measurement

- In the first run ([run 42458666340](https://github.com/horon-ai/agent/actions/runs/42458666340)) `skill_catalog` in the 0-body condition exceeded the baseline's p95 (1000 skills 9.65 → 13.52 ms). The cause was name-collision handling walking the earlier list from the start for every skill, and building a namespace string and comparison key per skill even with no collision. `abd7618e` and `83f2b6c7` added the name index, compute the namespace only on collision, drop one thread, and parse front matter within the first 4 KiB without a heap buffer; the table above is the result after those fixes.
- The benchmark script's MCP fixture check changed from "was the file read again during the measurement step" to "was the server from that file loaded" (`b3a053b3`). Not re-reading a file the parse cache already holds is the point of this change.

### What these numbers do not say

- The runner differs per run, so the same baseline moved by several ms between runs (0 body, 1000 skills `skill_catalog` baseline 9.65 / 15.62 / 15.71 ms). Comparisons were only made between the two sides within the same run.
- Cold start (an empty page cache) was not measured.
- Windows has no ctime, so a skill file edited to the same size with its modification time restored may not show up on the next request (`docs/runtime/PROMPT.md`).

## Fewer fsyncs and the structural difference from jcode (#47)

Three changes landed. A session that has never made an explicit choice no longer writes `false` to `local-head-explicit.json` (A), and the session writer no longer re-flushes the parent of the already existing `sessions/` root on every open (B). The durable runtime journal, in the other direction, now commits a newly created folder's entry to its parent (C). Before C, `runtime.jsonl` could vanish along with its folder after a power loss even though it had already returned `Ok`. The contract is in [`../protocol/SESSION_STORE.md`](../protocol/SESSION_STORE.md). The measurement is `benchmark.yml` run 42531992327 on 2026-11-09, with baseline `dev` (`c3e3ceeb`) and candidate (`d9d2c836`) alternating within one job on the same runner. one-shot has 5 samples. The fsync counts across plans are the second chart under "At a glance".

<details>
<summary>Exact numbers</summary>

| Metric | Environment | Before `c3e3ceeb` | After `d9d2c836` | jcode v0.99.1 |
| --- | --- | --- | --- | --- |
| one-shot fsync and fdatasync calls (warm) | GitHub Linux runner | 16 | 15 | 5 |
| one-shot time median (min–max) | GitHub Linux runner | 60.5 ms (54.2–133.1) | 57.7 ms (54.3–111.4) | 146.9 ms |
| one-shot time median (min–max) | GitHub macOS runner | 98.3 ms (90.3–389.4) | 115.1 ms (96.1–393.0) | 362.4 ms |
| one-shot time median (min–max) | GitHub Windows runner | 257.5 ms (235.0–562.6) | 284.8 ms (225.8–468.6) | 146.9 ms |
| Skill call time median (min–max) | GitHub Windows runner | 243.3 ms (220.2–260.9) | 264.6 ms (235.0–366.3) | 205.9 ms |
| MCP call time median (min–max) | GitHub Linux runner | 72.3 ms (70.2–144.0) | 101.8 ms (72.3–288.0) | 169.0 ms |
| MCP call time median (min–max) | GitHub Windows runner | 287.5 ms (276.9–302.2) | 299.3 ms (277.5–350.7) | 317.8 ms |
| Memory per added session, after a turn | GitHub Linux runner | 516.7 KB | 404.2 KB | 218.0 KB |
| Memory per added session, after a turn | GitHub Windows runner | 378.5 KB | 353.8 KB | 324.9 KB |

</details>

Every before/after difference in the time cells is inside its min–max range, so it is read as a tie. This change never adds a flush to the time path; it only removes them (C's flush happens only when the journal folder is first created and is not paid per append).

Windows one-shot and skill calls are slower than jcode regardless of this change (#44, #45). This table does not claim to have narrowed that.

### The remaining gap of 10 calls is structural

- jcode's one-shot fsyncs all come from its usage-statistics SQLite (`model-usage-v1.sqlite3`, WAL). Conversation records (the session journal and snapshots) are written with `append_json_line_fast` and `write_json_fast` and are not flushed. So jcode is safe against process crashes but loses conversations still in the kernel on a power cut.
- All 15 of horon's calls are commits of source data. One per event log commit, the folder entry at session creation, the local state file (`local-runtime.json`) and its folder, and the durable runtime journal and its folder.
- Per the AGENTS.md policy, the following were not added. Group commit that flushes once at the end of a turn (tool result records would stay uncommitted), an async flush thread (it commits after `Ok`, which weakens the contract), and `sync_file_range` with `O_DSYNC` (either not durable or more calls).
- Going lower requires a storage format change that merges the local state file and the event log into one record. That is a separate design task.

### What these numbers do not say

- **fsync call counts are only counted on Linux.** macOS has no `strace`. The three changes apply at the same ratio to macOS `F_FULLFSYNC` and Windows `FlushFileBuffers`, but those counts were not measured.
- **A cold first run may add one call.** C also commits the home folder entry the first time it creates `durable-runtime/`, so it costs one more per home directory.

## Close-save and per-session memory: cause diagnosis and structural difference (#44, #45, #47, #48)

After splitting the lag in Windows single-shot runs and skill calls, and the per-session memory growth, into diagnostic records, only one fix landed that does not weaken data safety. Closing at the end of a run only changes state, yet the auxiliary record (`AuxiliaryRecorded`) also carried the state, pid and last activity time (`runtime_history`), so every close did an event log commit, a binding commit and a derived cache rewrite. Nothing reads these values and the source is `local-runtime.json`, so they were removed from the auxiliary record ([`../protocol/SESSION_STORE.md`](../protocol/SESSION_STORE.md)). The measurement is `benchmark.yml` run 42572765871 on 2026-11-09, with baseline `dev` (`fca59cd6`) and candidate (`1dd5cd2d`) alternating within one job on the same runner. There are 5 samples.

| Metric | Environment | Before | After | jcode v0.99.1 |
| --- | --- | --- | --- | --- |
| one-shot fsync and fdatasync calls (warm) | GitHub Linux runner | 15 | 14 | 5 |
| one-shot last request → exit (median) | GitHub Linux runner | 11.5 ms | 8.2 ms | 4.4 ms |
| one-shot last request → exit (median) | GitHub Windows runner | 61.8 ms | 46.5 ms | 11.9 ms |
| one-shot time median (min–max) | GitHub Linux runner | 61.5 ms (60–63) | 58.7 ms (57–100) | 144.9 ms |
| one-shot time median (min–max) | GitHub Windows runner | 255.8 ms (224–303) | 224.3 ms (220–305) | 151.7 ms |
| Skill call time median (min–max) | GitHub Linux runner | 57.0 ms (55–60) | 53.4 ms (52–54) | 56.3 ms (55–58) |
| Skill call time median (min–max) | GitHub Windows runner | 260.5 ms (256–306) | 250.6 ms (233–263) | 232.7 ms (211–256) |
| MCP call time median (min–max) | GitHub Windows runner | 301.6 ms (281–363) | 269.8 ms (255–279) | 337.2 ms |

Linux skill calls no longer overlap jcode's range and are now faster. The Windows time cells overlap before and after, so they are read only for direction.

### What remains in the Windows shutdown phase

The Windows one-shot shutdown phase, split with a diagnostic build (run 42568847961, per-phase timing logs), was 15 ms saving the final response (about 8 ms event log commit, about 4.5 ms derived cache rewrite), 10 ms recording the `run.stopped` journal, 18 ms close-save, and about 4 ms process cleanup. This fix reduced only close-save, to a single binding commit. The remaining shares were not reduced, for the reasons below.

- The event log and the `run.stopped` journal are source data. Windows `FlushFileBuffers` costs several ms per call, but removing it would lose conversation and run state on a power cut. jcode does not commit conversation records, so this difference is structural.
- The derived cache rewritten on every commit (about 4.3 ms, no flush) is read by other clients that attach mid-turn, so its write cadence was not changed.
- The bash run in the tool round trip (about 23 ms on Windows) is the cost of running the benchmark's `ls` through `cmd.exe /D /S /C`. It cannot be reduced without changing the shell.
- The first HTTP client construction in the startup phase costs 5 ms, and the guess that reading the certificate store was the cause was rejected by measurement.

### Per-session memory (#48)

In run 42568289247 (Linux, right after a turn, the 1·5·10 session slope), of the 477 KB RSS per added session, anonymous memory was 440 KB, data the allocator reported as live was 234 KB, and RSS after `allocator:purge` was 417 KB. Comparing 1 and 10 sessions with heaptrack (diagnostic run 42571121373), the live heap grows about 247 KB per session, and the largest growth points are session creation 29 KB, the locked tool list 10 KB and the event position index 8 KB, all data a session actually uses. There is no leak.

- Returning memory only freed 60 KB, so the other roughly 200 KB is fragmentation inside the allocator's arena. Returning more often would have little effect.
- On macOS the live data growth is 140 KB for horon against 191 KB for jcode, and the after-turn growth in this run's Windows measurement is 310 KB for horon against 396 KB for jcode. The Linux difference comes from allocator (glibc) fragmentation, and changing the allocator carries a high regression risk on other paths, so it was not done here.

## Measurements from the `perf-bench-harness-prompt` plan

This plan measured the request bytes and prompt surface a turn pays for, cut them, and bounded them in the structural gate (#324). It also changes one behaviour: an edit to `.horon/prompt-overlay.md`, `preferred-tools.md` or `system-prompt.md` during a session now takes effect at the next session boundary (new session, `/clear`, working-directory change, restore), the rule AGENTS.md already followed.

### Measurement conditions

- Three-OS A/B: [run 43667470921](https://github.com/horon-ai/agent/actions/runs/43667470921), candidate `f5993e9d`, baseline `horon-base` built from `dev` `5285d2a4` in the same job. `suites=compare`, `oneshot_runs=5`, median with min–max. A difference inside the min–max overlap is a tie.
- Linux A/B with `compare,startup,tools`: [run 43670649131](https://github.com/horon-ai/agent/actions/runs/43670649131).
- macOS re-measurement of `compare`: [run 43672354272](https://github.com/horon-ai/agent/actions/runs/43672354272), taken because the first macOS `--version` sample did not overlap.
- Byte and counter values: structural gate [run 43666931419](https://github.com/horon-ai/agent/actions/runs/43666931419) and ci.yml [run 43662099986](https://github.com/horon-ai/agent/actions/runs/43662099986), identical on ubuntu and macOS. The before values come from wave 1 on the same branch (`308f6c5e`, equal to `dev` for these surfaces).

### Prompt surface and session context

![What one turn sends before the model is called](charts/record-08-prompt-surface.svg)

<details>
<summary>Exact numbers</summary>

| Surface | Before | After |
| --- | --- | --- |
| Eager core tool catalog (8 schemas) | 9,714 B | 9,151 B |
| Core one-shot first request `tools` (mock server) | 9,818 B | 9,255 B |
| Static prompt (core, no prompt files) | 1,367 B, 319 tokens | 1,322 B, 305 tokens |
| `# Additional tools` index | 629 B, 5 rows cut mid-sentence | 585 B, 1 row (`todo`) |
| Prompt source probes over 9 unchanged requests | 54 | 0 |
| Session-context `git` processes, outside a repository / in a work tree | 1 / 3 | 0 / 1 |
| GPU probe processes without a PCI display controller | 1 | 0 |

</details>

PR #332 set the tool catalog ceiling to 9,151 B. Shortening the shared `intent` description in `1dfdd06a` brought the eight schemas to 8,975 B, the current ceiling. The static prompt measured 1,322 B and nine unchanged requests made zero prompt-source reads. On the same Linux runner, the first request's `tools` field fell from 9,255 B at `83da1c39` to 9,079 B. This puts the catalog 25 B below the 9,000 B target and the first request 739 B below the original 9,818 B. [Structural measurements](https://github.com/horon-ai/agent/actions/runs/44190404248/job/134418534689) · [First requests sent to the mock server](https://github.com/horon-ai/agent/actions/runs/44190400494/artifacts/14378517143)

### Time and memory (median ms, min–max in parentheses)

![One-shot, core, skill and MCP time by OS, before and after](charts/record-09-prompt-time.svg)

<details>
<summary>Exact numbers</summary>

| OS | Scenario | `horon-base` (dev) | Candidate |
| --- | --- | --- | --- |
| Linux | one-shot | 40.24 (39.92–179.35) | 29.35 (28.39–44.31) |
| Linux | core one-shot | 42.92 (41.29–49.03) | 31.31 (30.72–45.41) |
| Linux | skill call | 37.43 (36.77–38.08) | 26.31 (25.22–26.45) |
| Linux | MCP tool call | 55.62 (50.58–57.65) | 40.16 (39.54–44.96) |
| macOS | one-shot | 54.93 (53.10–495.17) | 47.07 (44.89–265.97) |
| macOS | core one-shot | 60.63 (55.86–88.40) | 49.28 (48.89–127.21) |
| macOS | skill call | 108.27 (85.84–128.53) | 62.82 (51.71–87.13) |
| macOS | MCP tool call | 82.51 (80.09–191.23) | 76.41 (73.10–206.67) |
| Windows | one-shot | 170.15 (158.82–229.05) | 188.26 (142.17–370.69) |
| Windows | core one-shot | 200.21 (171.71–436.16) | 179.55 (149.18–427.49) |
| Windows | skill call | 176.06 (158.48–180.42) | 145.75 (139.41–231.53) |
| Windows | MCP tool call | 302.51 (201.68–494.52) | 268.93 (172.28–412.46) |

</details>

- One-shot peak RSS: Linux 46.6 → 39.4 MB, macOS 31.6 → 24.0 MB, Windows 33.9 → 33.6 MB. One-shot fsync calls stay at 10 on Linux for both sides.
- The Linux run 43670649131 agrees: one-shot 48.69 → 34.59 ms, skill call 43.12 → 30.72 ms, MCP tool call 78.07 → 49.12 ms, peak RSS 40.0 → 38.3 MB, fsync 10 and 10.
- No scenario got slower with non-overlapping ranges. Windows one-shot (170.15 → 188.26) and macOS MCP tool call in run 43672354272 (101.94 → 107.63) have a worse median inside overlapping ranges, which is a tie.
- `--version`: the first macOS sample was 7.36 (6.99–7.50) → 7.89 (7.79–8.18) ms, which does not overlap. The re-measurement gave 9.27 (8.96–10.48) → 8.29 (7.99–8.43) ms, non-overlapping the other way, and Linux and Windows overlap in both runs. The `--version` path has no code change on this branch, so the first sample is runner noise.
- Startup and tools suites have no same-job baseline and are reported only: `serve` socket ready 20.39 ms (20.57 ms in the dev baseline run 43612646876), bridge no-op reply 21.59 ms (21.89 ms).

### What these numbers do not say

- The time cut is not attributed. Fewer `git` processes per session context and less per-request prompt work both run on the measured paths, but no run splits the difference between them.
- `[features] foreign_rules` (off by default) still reads its rule files on every request and is not counted by the prompt source reads.
- Both Windows benchmark builds print the same two `linker stdout` lines for the baseline and the candidate. They come from the benchmark's Windows release link, not from this change; the ci.yml runs of this branch have no warning.

## Automatic measurement: latest measured commit on `main`

When a commit on `main` passes both CI and the structural performance gate, and the agent code (`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`) has changed since the previous measurement, `.github/workflows/benchmark-docs.yml` runs the full benchmark suite separately per OS on Linux, macOS and Windows and rewrites the section below on both `main` and `dev`. There is one section per OS, and one run writes both this file and `RESULTS.ko.md`, so the English and Korean blocks always name the same commit. `suites=all` covers every regular suite and never `install`, so no row below times an installation; install times come from a dispatch of the install suite, which measures a pinned published release rather than the measured commit. How the measurement works and what each suite means are in "Automatic measurement" in [`BENCHMARK.md`](BENCHMARK.md).

<!-- bench-auto:start commit=2f63e444a5d4330d6f651189b54fe6e64eb84866 -->

## Headless (`horon-agent`)

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `2f63e444a5d4330d6f651189b54fe6e64eb84866`
- Measured at: 2026-11-19 14:51 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/44537447832> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (2f63e44)` | Success |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (2f63e44)` | Success |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (2f63e44)` | Success |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (2f63e44) | 2.0 ms | 112.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 6.8 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 6.8 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 29.6 ms | 435.0 MB |
| OpenCode | 1.19.5 | 347.5 ms | 613.4 MB |
| pi | 1.5.0 | 169.2 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 526.7 ms | 413.9 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 89.6 ms | 520.7 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 42.0 ms | 40.9 ms–219.1 ms | baseline | 46.5 MB | baseline | yes | 10 | 3,756 / 2,368 (1 tools) | — |
| jcode | 137.6 ms | 137.2 ms–138.7 ms | 3.27× more | 42.1 MB | 1.10× less | yes | 5 | 3,292 / 1,433 (1 tools) | — |
| Claude Code | 345.8 ms | 330.8 ms–349.6 ms | 8.22× more | 208.0 MB | 4.47× more | yes | — | 45,780 / 29,653 (12 tools) | — |
| Codex CLI | 354.8 ms | 348.9 ms–394.5 ms | 8.44× more | 203.1 MB | 4.37× more | yes | 3 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 2.06 s | 1.96 s–2.21 s | 48.96× more | 580.5 MB | 12.49× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 281.4 ms | 280.2 ms–292.1 ms | 6.69× more | 114.4 MB | 2.46× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.32 s | 1.30 s–1.33 s | 31.30× more | 354.6 MB | 7.63× more | yes | 31 | 29,858 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 4.33 s | 4.26 s–4.79 s | 103.03× more | 756.3 MB | 16.27× more | yes | 36 | 26,669 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 44.4 ms | 43.5 ms–44.9 ms | baseline | 43.1 MB | baseline | — | yes | — | 11,125 / 9,079 (8 tools) |
| jcode | — | — | — | — | — | — | — | skipped: jcode has no core tool profile | — |
| Claude Code | — | — | — | — | — | — | — | skipped: Claude Code has no core tool profile | — |
| Codex CLI | — | — | — | — | — | — | — | skipped: Codex CLI has no core tool profile | — |
| OpenCode | — | — | — | — | — | — | — | skipped: OpenCode has no core tool profile | — |
| pi | — | — | — | — | — | — | — | skipped: pi has no core tool profile | — |
| oh-my-pi (omp) | — | — | — | — | — | — | — | skipped: oh-my-pi (omp) has no core tool profile | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | — | skipped: oh-my-openagent (omo) has no core tool profile | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 38.0 ms | 37.6 ms–38.9 ms | baseline | 37.9 MB | baseline | skill-uri | yes | — |
| jcode | 45.8 ms | 45.1 ms–48.3 ms | 1.21× more | 41.2 MB | 1.09× more | skill-tool | yes | — |
| Claude Code | 332.6 ms | 324.4 ms–339.6 ms | 8.76× more | 202.2 MB | 5.34× more | skill-tool | yes | — |
| Codex CLI | 398.8 ms | 386.6 ms–415.7 ms | 10.51× more | 204.1 MB | 5.39× more | file-read | yes | — |
| OpenCode | 2.40 s | 2.36 s–2.43 s | 63.32× more | 615.4 MB | 16.25× more | skill-tool | yes | — |
| pi | 363.4 ms | 359.8 ms–365.4 ms | 9.58× more | 125.4 MB | 3.31× more | file-read | yes | — |
| oh-my-pi (omp) | 1.35 s | 1.34 s–1.37 s | 35.50× more | 344.0 MB | 9.08× more | file-read | yes | — |
| oh-my-openagent (omo) | 4.58 s | 4.54 s–4.61 s | 120.77× more | 692.5 MB | 18.28× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 57.3 ms | 56.4 ms–57.7 ms | baseline | 43.1 MB | baseline | mcp-call | yes | 47.2 ms | — |
| jcode | 160.3 ms | 159.2 ms–160.8 ms | 2.80× more | 52.0 MB | 1.21× more | mcp-action | yes | 139.1 ms | — |
| Claude Code | 419.3 ms | 409.4 ms–423.3 ms | 7.31× more | 224.1 MB | 5.20× more | mcp-tool | yes | 297.4 ms | — |
| Codex CLI | 343.2 ms | 342.9 ms–395.4 ms | 5.99× more | 197.0 MB | 4.58× more | mcp-tool | yes | 207.7 ms | — |
| OpenCode | 2.17 s | 2.12 s–2.20 s | 37.90× more | 566.3 MB | 13.15× more | mcp-tool | yes | 1.98 s | — |
| pi | 392.5 ms | 390.8 ms–404.2 ms | 6.85× more | 139.1 MB | 3.23× more | mcp-tool | yes | 354.2 ms | — |
| oh-my-pi (omp) | 1.34 s | 1.33 s–1.36 s | 23.43× more | 347.5 MB | 8.07× more | mcp-route | yes | 1.27 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 37.6 MB | 175.8 KB | baseline | 0.12 | 1.00 | 1 | 0.00 | 45.2 MB | 475.7 KB |
| jcode | 42.7 MB | 148.3 KB | 1.19× less | 0.00 | — | 1 | 0.00 | 46.4 MB | 272.3 KB |
| Claude Code | 152.6 MB | 149.6 MB | 871.23× more | — | — | 1 | 1.00 | 187.3 MB | 181.0 MB |
| Codex CLI | 128.9 MB | 132.4 MB | 771.06× more | — | — | 1 | 0.98 | 161.4 MB | 164.7 MB |
| OpenCode | 243.7 MB | 0.0 B | — | — | — | 1 | 0.00 | 453.9 MB | 10.7 MB |
| pi | 109.1 MB | 111.5 MB | 649.21× more | — | — | 1 | 1.00 | 131.4 MB | 130.7 MB |
| oh-my-pi (omp) | 377.2 MB | 127.8 MB | 743.99× more | — | — | 1 | 1.00 | 363.7 MB | 360.4 MB |
| oh-my-openagent (omo) | 326.1 MB | 60.0 MB | 349.31× more | — | — | 2 | 0.89 | 611.5 MB | 577.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 45.2 MB | 47.0 MB | 49.4 MB |
| jcode | 46.4 MB | 47.4 MB | 48.8 MB |
| Claude Code | 187.3 MB | 920.3 MB | 1.8 GB |
| Codex CLI | 161.4 MB | 814.3 MB | 1.6 GB |
| OpenCode | 453.9 MB | 476.6 MB | 548.5 MB |
| pi | 131.4 MB | 647.6 MB | 1.3 GB |
| oh-my-pi (omp) | 363.7 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 611.5 MB | 3.1 GB | 5.7 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 1 / 1 | 8 / 5 | 10 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 2 / 4 | 5 / 20 | 10 / 41 |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    1.86 ms
  Max:    2.48 ms
  Mean:   2.02 ms
  Median: 1.95 ms
  Stdev:  0.19 ms

--version:
  Min:    1.73 ms
  Max:    1.81 ms
  Mean:   1.77 ms
  Median: 1.77 ms
  Stdev:  0.03 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.39 ms
  Max:    20.42 ms
  Mean:   20.40 ms
  Median: 20.40 ms
  Stdev:  0.01 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.3%
  logging_init        0.1       0.1   14.9%
  nofile_limit        0.3       0.2   21.9%
  perm_harden         0.3       0.0    0.8%
  args_parse          0.3       0.0    0.1%
  config_loaded       0.4       0.1   12.6%
  server_new          0.7       0.3   42.1%
  socket_bound        0.7       0.1    7.4%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.51 ms
  Max:    20.83 ms
  Mean:   20.71 ms
  Median: 20.73 ms
  Stdev:  0.10 ms

Bridge stage: hello_ok:
  Min:    20.92 ms
  Max:    21.30 ms
  Mean:   21.15 ms
  Median: 21.17 ms
  Stdev:  0.11 ms

Bridge stage: no-op reply:
  Min:    21.24 ms
  Max:    21.64 ms
  Mean:   21.47 ms
  Median: 21.50 ms
  Stdev:  0.13 ms

Bridge total (socket -> no-op):
  Min:    21.24 ms
  Max:    21.64 ms
  Mean:   21.47 ms
  Median: 21.50 ms
  Stdev:  0.13 ms

============================================================
Summary:
  Binary load median:      ~2.0 ms
  Server ready median:     ~20.4 ms
  Bridge total median:     ~21.5 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (9f742330ad52)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (59ecb999befb, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-f7uyiwju
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.013/0.013    p95    0.014/0.015    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.092/0.092    p95    0.096/0.097    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.134/0.128    p95    0.158/0.154    delta  -0.006 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.913/6.910    p95  111.212/15.325   delta  -0.003 ms  control-no-change
  total store_read_bytes p50 23080  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23088 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23088 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.294/2.311    p95    2.338/2.380    delta  +0.017 ms  control-no-change
  skill_request_overlay p50    3.909/3.904    p95    3.950/3.986    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.132/0.133    p95    0.138/0.134    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   13.926/13.795   p95   62.680/68.180   delta  -0.131 ms  control-no-change
  total store_read_bytes p50 23076  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23072..23084 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23084 (4 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.013/0.013    p95    0.014/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.094/0.095    p95    0.113/0.123    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.127/0.128    p95    0.142/0.257    delta  +0.001 ms  control-no-change
  attach_history    p50  198.205/199.213  p95  214.594/209.145  delta  +1.007 ms  control-no-change
  resume_ready      p50   44.939/44.877   p95   46.135/45.785   delta  -0.062 ms  control-no-change
  provider_dispatch p50    1.120/1.129    p95    1.143/1.176    delta  +0.009 ms  control-no-change
  session_save      p50    6.579/6.581    p95  216.587/111.345  delta  +0.002 ms  control-no-change
  total store_read_bytes p50 5311401  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 198.205 ms (load 164.796 + history 33.248)
    selection          p50     0.028 ms  min     0.027  max     0.028  ratio 0.0% of anchor
    checkpoint_recent  p50   127.788 ms  min   127.687  max   128.063  ratio 64.5% of anchor
    log_read           p50     0.064 ms  min     0.060  max     0.070  ratio 0.0% of anchor
    full_scan          p50    16.927 ms  min    16.902  max    16.994  ratio 8.5% of anchor
    blob_read          p50     0.006 ms  min     0.003  max     0.034  ratio 0.0% of anchor
    graph_replay       p50    14.016 ms  min    13.251  max    14.185  ratio 7.1% of anchor
    metadata_json      p50     0.006 ms  min     0.005  max     0.007  ratio 0.0% of anchor
    messages_json      p50     0.800 ms  min     0.800  max     0.807  ratio 0.4% of anchor
    messages_decode    p50     0.752 ms  min     0.741  max     0.785  ratio 0.4% of anchor
    source_hash        p50     0.423 ms  min     0.417  max     0.424  ratio 0.2% of anchor
    history_rescan     p50    35.036 ms  min    34.800  max    35.154  ratio 17.7% of anchor
    page_index         p50    37.837 ms  min    37.787  max    39.184  ratio 19.1% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189185..4189193 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099135 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23063..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189193 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099131..1099135 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23093 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.353/2.327    p95    2.494/2.433    delta  -0.026 ms  control-no-change
  skill_request_overlay p50    3.895/3.892    p95    3.920/4.059    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.134/0.133    p95    0.142/0.138    delta  -0.001 ms  control-no-change
  attach_history    p50  202.504/201.068  p95  207.381/208.041  delta  -1.436 ms  control-no-change
  resume_ready      p50   45.233/45.228   p95   46.294/46.195   delta  -0.004 ms  control-no-change
  provider_dispatch p50    1.144/1.126    p95    1.178/1.183    delta  -0.018 ms  control-no-change
  session_save      p50    6.492/6.529    p95    9.171/11.276   delta  +0.037 ms  control-no-change
  total store_read_bytes p50 5311409  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 202.504 ms (load 168.007 + history 32.910)
    selection          p50     0.031 ms  min     0.030  max     0.031  ratio 0.0% of anchor
    checkpoint_recent  p50   131.628 ms  min   129.661  max   131.927  ratio 65.0% of anchor
    log_read           p50     0.070 ms  min     0.068  max     0.074  ratio 0.0% of anchor
    full_scan          p50    17.061 ms  min    16.970  max    17.131  ratio 8.4% of anchor
    blob_read          p50     0.004 ms  min     0.004  max     0.006  ratio 0.0% of anchor
    graph_replay       p50    13.237 ms  min    12.961  max    13.864  ratio 6.5% of anchor
    metadata_json      p50     0.004 ms  min     0.004  max     0.004  ratio 0.0% of anchor
    messages_json      p50     0.791 ms  min     0.790  max     0.803  ratio 0.4% of anchor
    messages_decode    p50     0.750 ms  min     0.747  max     0.751  ratio 0.4% of anchor
    source_hash        p50     0.423 ms  min     0.412  max     0.435  ratio 0.2% of anchor
    history_rescan     p50    34.532 ms  min    32.934  max    34.848  ratio 17.1% of anchor
    page_index         p50    38.779 ms  min    38.095  max    39.190  ratio 19.1% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23064..23090 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099128..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23058..23090 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.013/0.013    p95    0.015/0.013    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.096/0.096    p95    0.097/0.106    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.130/0.129    p95    0.145/0.142    delta  -0.001 ms  control-no-change
  attach_history    p50 2604.853/2609.312 p95 2651.949/2649.356 delta  +4.458 ms  control-no-change
  resume_ready      p50  515.770/516.567  p95  557.375/538.656  delta  +0.797 ms  control-no-change
  provider_dispatch p50   13.669/13.750   p95   14.538/15.171   delta  +0.081 ms  control-no-change
  session_save      p50    7.159/7.083    p95   27.856/28.162   delta  -0.076 ms  control-no-change
  total store_read_bytes p50 53417618  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2604.853 ms (load 2186.334 + history 422.007)
    selection          p50     0.043 ms  min     0.038  max     0.043  ratio 0.0% of anchor
    checkpoint_recent  p50  1720.952 ms  min  1714.412  max  1734.212  ratio 66.1% of anchor
    log_read           p50     0.601 ms  min     0.588  max     0.631  ratio 0.0% of anchor
    full_scan          p50   183.976 ms  min   183.973  max   188.310  ratio 7.1% of anchor
    blob_read          p50     0.067 ms  min     0.064  max     0.077  ratio 0.0% of anchor
    graph_replay       p50   193.208 ms  min   185.867  max   196.645  ratio 7.4% of anchor
    metadata_json      p50     0.006 ms  min     0.006  max     0.006  ratio 0.0% of anchor
    messages_json      p50     7.811 ms  min     7.722  max     8.170  ratio 0.3% of anchor
    messages_decode    p50    12.745 ms  min    12.478  max    13.512  ratio 0.5% of anchor
    source_hash        p50     3.905 ms  min     3.897  max     3.942  ratio 0.1% of anchor
    history_rescan     p50   427.252 ms  min   414.947  max   427.446  ratio 16.4% of anchor
    page_index         p50   445.084 ms  min   443.861  max   445.199  ratio 17.1% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23069..23093 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210238..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23085 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.381/2.423    p95    2.407/2.448    delta  +0.042 ms  control-no-change
  skill_request_overlay p50    3.879/3.877    p95    3.940/3.947    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.135/0.136    p95    0.139/0.145    delta  +0.001 ms  control-no-change
  attach_history    p50 2641.810/2652.256 p95 2670.654/2735.872 delta +10.447 ms  control-no-change
  resume_ready      p50  537.080/542.155  p95  554.724/584.148  delta  +5.075 ms  control-no-change
  provider_dispatch p50   14.559/14.578   p95   15.421/15.044   delta  +0.019 ms  control-no-change
  session_save      p50    7.032/7.003    p95  116.212/32.110   delta  -0.029 ms  control-no-change
  total store_read_bytes p50 53417619  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2641.810 ms (load 2205.688 + history 430.061)
    selection          p50     0.044 ms  min     0.040  max     0.044  ratio 0.0% of anchor
    checkpoint_recent  p50  1734.626 ms  min  1691.245  max  1759.992  ratio 65.7% of anchor
    log_read           p50     0.568 ms  min     0.567  max     0.583  ratio 0.0% of anchor
    full_scan          p50   184.911 ms  min   183.922  max   186.357  ratio 7.0% of anchor
    blob_read          p50     0.065 ms  min     0.065  max     0.067  ratio 0.0% of anchor
    graph_replay       p50   185.365 ms  min   179.195  max   188.129  ratio 7.0% of anchor
    metadata_json      p50     0.006 ms  min     0.006  max     0.008  ratio 0.0% of anchor
    messages_json      p50     7.912 ms  min     7.741  max     8.056  ratio 0.3% of anchor
    messages_decode    p50    12.340 ms  min    12.068  max    12.488  ratio 0.5% of anchor
    source_hash        p50     3.930 ms  min     3.930  max     3.932  ratio 0.1% of anchor
    history_rescan     p50   429.145 ms  min   421.180  max   459.345  ratio 16.2% of anchor
    page_index         p50   442.756 ms  min   428.137  max   448.310  ratio 16.8% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210238..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23065..23089 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23089 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (9f742330ad52)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (59ecb999befb, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-s_gsf6i9
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.011/0.011    p95    0.012/0.012    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.085/0.085    p95    0.104/0.094    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.113/0.113    p95    0.125/0.134    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    5.866/6.014    p95   10.399/15.130   delta  +0.148 ms  control-no-change
  total store_read_bytes p50 23794  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23073, 23081, 23784, 23804, 24503, 24507, 24515, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23077, 23081, 23792, 23796, 23800, 24515, 24519, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23073..24523 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24523 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.015/2.036    p95    2.063/2.199    delta  +0.021 ms  control-no-change
  skill_request_overlay p50    3.408/3.470    p95    3.554/3.699    delta  +0.061 ms  control-no-change
  mcp_config_load   p50    0.112/0.115    p95    0.119/0.122    delta  +0.003 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50    7.515/6.812    p95   21.204/15.993   delta  -0.703 ms  control-no-change
  total store_read_bytes p50 23795  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23063, 23073, 23081, 23792, 23794, 23796, 24493, 24495, 24515, 24531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23075, 23077, 23780, 23792, 23806, 24511, 24513, 24515, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23063..24531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24517 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.011/0.011    p95    0.012/0.012    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.084/0.083    p95    0.094/0.101    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.111/0.109    p95    0.123/0.122    delta  -0.001 ms  control-no-change
  attach_history    p50   60.552/60.601   p95   63.114/61.874   delta  +0.048 ms  control-no-change
  resume_ready      p50   34.069/34.099   p95   34.518/35.127   delta  +0.030 ms  control-no-change
  provider_dispatch p50    0.874/0.882    p95    0.905/0.891    delta  +0.007 ms  control-no-change
  session_save      p50    6.517/6.179    p95   29.400/22.153   delta  -0.338 ms  control-no-change
  total store_read_bytes p50 2841376  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23797, 23805, 24500, 24528, 25235, 25237, 25960, 25966, 26661, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23797, 23801, 24508, 24528, 25228, 25243, 25950, 25952, 26673, 26675]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23797..26671 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23797..26675 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.012/2.027    p95    2.068/2.142    delta  +0.015 ms  control-no-change
  skill_request_overlay p50    3.436/3.448    p95    3.550/3.794    delta  +0.013 ms  control-no-change
  mcp_config_load   p50    0.114/0.115    p95    0.119/0.134    delta  +0.000 ms  control-no-change
  attach_history    p50   61.448/63.092   p95   63.311/64.996   delta  +1.643 ms  control-no-change
  resume_ready      p50   34.695/34.651   p95   35.780/35.766   delta  -0.045 ms  control-no-change
  provider_dispatch p50    0.885/0.869    p95    0.897/0.923    delta  -0.015 ms  control-no-change
  session_save      p50    7.309/6.763    p95  179.266/22.215   delta  -0.546 ms  control-no-change
  total store_read_bytes p50 2841376  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23792, 23804, 24503, 24515, 25232, 25244, 25951, 25955, 26666, 26682]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23796, 23798, 24505, 24519, 25230, 25234, 25236, 25939, 25943, 26662]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23792..26682 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23796..26662 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.012/0.011    p95    0.014/0.012    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.083/0.083    p95    0.097/0.086    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.110/0.111    p95    0.120/0.118    delta  +0.000 ms  control-no-change
  attach_history    p50  729.539/741.517  p95  742.756/888.623  delta +11.978 ms  control-no-change
  resume_ready      p50  388.691/391.521  p95  401.724/416.744  delta  +2.830 ms  control-no-change
  provider_dispatch p50   10.398/10.234   p95   10.859/11.856   delta  -0.164 ms  control-no-change
  session_save      p50    6.239/6.104    p95   15.023/209.570  delta  -0.135 ms  control-no-change
  total store_read_bytes p50 28640390  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23794, 23798, 24513, 24525, 25232, 25234, 25963, 25965, 26660, 26674]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23788, 23796, 24509, 24515, 25248, 25250, 25953, 25955, 26670, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23794..26674 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23788..26680 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.062/1.989    p95    4.190/2.122    delta  -0.073 ms  control-no-change
  skill_request_overlay p50    3.490/3.422    p95    7.440/3.518    delta  -0.069 ms  control-no-change
  mcp_config_load   p50    0.117/0.116    p95    0.282/0.123    delta  -0.000 ms  control-no-change
  attach_history    p50  733.974/740.165  p95  785.286/767.367  delta  +6.190 ms  control-no-change
  resume_ready      p50  399.854/396.451  p95  450.727/428.241  delta  -3.403 ms  control-no-change
  provider_dispatch p50   10.249/10.232   p95   27.506/18.720   delta  -0.017 ms  control-no-change
  session_save      p50    7.126/6.949    p95    7.236/12.439   delta  -0.176 ms  control-no-change
  total store_read_bytes p50 28640397  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23800, 23804, 24523, 24527, 25240, 25955, 25959, 26686]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23804, 23812, 24523, 24527, 25224, 25234, 25951, 25965, 26672, 26674]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23800..26686 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23804..26674 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.4 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 1.8 ms | 2.0 ms | 1.7 ms | 2.0 ms | 5 |
| write (10KB) | 2.0 ms | 2.2 ms | 2.0 ms | 2.2 ms | 5 |
| edit (simple replace) | 1.8 ms | 1.9 ms | 1.8 ms | 1.9 ms | 5 |
| ls (scripts dir) | 1.0 ms | 1.2 ms | 0.9 ms | 1.2 ms | 5 |
| agentgrep (grep in tree) | 2.2 ms | 2.3 ms | 2.0 ms | 2.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.8 ms | 1.9 ms | 1.4 ms | 1.9 ms | 5 |
| bash (true) | 1.9 ms | 2.0 ms | 1.9 ms | 2.0 ms | 5 |
| bash (echo hello) | 1.8 ms | 1.9 ms | 1.8 ms | 1.9 ms | 5 |
| bash (ls fixture tree) | 2.9 ms | 3.0 ms | 2.8 ms | 3.0 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | 0.242 ms | 0.241 ms | 0.252 ms | 0.000 ms | 5.64 | 0.00 | 5 × 50 | schedstat |
| read (1000 lines) | 0.267 ms | 0.252 ms | 0.334 ms | 0.000 ms | 5.50 | 0.00 | 5 × 50 | schedstat |
| read (10 lines @ offset) | 0.271 ms | 0.263 ms | 0.273 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| read (repo Cargo.toml) | 0.216 ms | 0.215 ms | 0.221 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| write (small) | 0.614 ms | 0.548 ms | 0.681 ms | 0.000 ms | 19.28 | 0.00 | 5 × 50 | schedstat |
| write (10KB) | 0.812 ms | 0.762 ms | 0.834 ms | 0.000 ms | 20.44 | 0.00 | 5 × 50 | schedstat |
| edit (simple replace) | 0.687 ms | 0.649 ms | 0.698 ms | 0.000 ms | 19.20 | 0.04 | 5 × 50 | schedstat |
| ls (scripts dir) | 0.646 ms | 0.627 ms | 0.682 ms | 0.000 ms | 5.78 | 0.00 | 5 × 50 | schedstat |
| agentgrep (grep in tree) | 1.049 ms | 1.041 ms | 1.062 ms | 0.000 ms | 10.34 | 0.18 | 5 × 50 | schedstat |
| agentgrep (find **/*.rs) | 1.217 ms | 1.161 ms | 1.235 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| bash (true) | 0.557 ms | 0.555 ms | 0.572 ms | 1.200 ms | 7.36 | 0.00 | 5 × 50 | schedstat |
| bash (echo hello) | 0.567 ms | 0.566 ms | 0.577 ms | 1.200 ms | 8.16 | 0.00 | 5 × 50 | schedstat |
| bash (ls fixture tree) | 0.640 ms | 0.639 ms | 0.646 ms | 2.400 ms | 8.66 | 0.00 | 5 × 50 | schedstat |
| invalid tool (dispatch only) | 0.043 ms | 0.042 ms | 0.043 ms | 0.000 ms | 2.42 | 0.00 | 5 × 50 | schedstat |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 40.3 ms | 40.3 ms | 40.3 ms | 40.9 ms | 0 |
| 10 | 145.6 ms | 160.7 ms | 160.7 ms | 161.3 ms | 0 |
| 40 | 628.7 ms | 663.1 ms | 673.1 ms | 674.5 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 254.4 ms | 254.4 ms | 254.4 ms | 1 |
| leaf_touch | src/main.rs | 313.7 ms | 313.7 ms | 313.7 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 20.60 s | 20.60 s | 20.60 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 28.97 s | 28.97 s | 28.97 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (2f63e44) | 8.4 ms | 94.3 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.0 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 18.1 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 52.3 ms | 346.5 MB |
| OpenCode | 1.19.5 | 351.7 ms | 344.6 MB |
| pi | 1.5.0 | 195.1 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 508.6 ms | 274.3 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 128.5 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 54.1 ms | 50.6 ms–416.9 ms | baseline | 27.2 MB | baseline | yes | 3,597 / 2,368 (1 tools) | — |
| jcode | 364.1 ms | 350.9 ms–587.6 ms | 6.72× more | 34.1 MB | 1.25× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 405.4 ms | 366.2 ms–719.1 ms | 7.49× more | 200.9 MB | 7.39× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 318.1 ms | 279.3 ms–1.03 s | 5.88× more | 143.8 MB | 5.29× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 2.25 s | 1.73 s–3.84 s | 41.62× more | 786.7 MB | 28.94× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 312.7 ms | 304.8 ms–350.6 ms | 5.78× more | 120.6 MB | 4.43× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.39 s | 1.22 s–1.58 s | 25.62× more | 405.3 MB | 14.91× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 6.35 s | 5.86 s–8.42 s | 117.27× more | 903.9 MB | 33.25× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 55.1 ms | 52.1 ms–59.9 ms | baseline | 27.8 MB | baseline | — | yes | — | 10,966 / 9,079 (8 tools) |
| jcode | — | — | — | — | — | — | — | skipped: jcode has no core tool profile | — |
| Claude Code | — | — | — | — | — | — | — | skipped: Claude Code has no core tool profile | — |
| Codex CLI | — | — | — | — | — | — | — | skipped: Codex CLI has no core tool profile | — |
| OpenCode | — | — | — | — | — | — | — | skipped: OpenCode has no core tool profile | — |
| pi | — | — | — | — | — | — | — | skipped: pi has no core tool profile | — |
| oh-my-pi (omp) | — | — | — | — | — | — | — | skipped: oh-my-pi (omp) has no core tool profile | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | — | skipped: oh-my-openagent (omo) has no core tool profile | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 49.4 ms | 46.1 ms–56.7 ms | baseline | 23.2 MB | baseline | skill-uri | yes | — |
| jcode | 285.4 ms | 198.0 ms–385.7 ms | 5.78× more | 25.4 MB | 1.09× more | skill-tool | yes | — |
| Claude Code | 343.3 ms | 318.4 ms–352.9 ms | 6.95× more | 189.3 MB | 8.14× more | skill-tool | yes | — |
| Codex CLI | 338.0 ms | 316.5 ms–376.6 ms | 6.84× more | 143.1 MB | 6.15× more | file-read | yes | — |
| OpenCode | 3.58 s | 3.28 s–4.78 s | 72.49× more | 769.9 MB | 33.12× more | skill-tool | yes | — |
| pi | 411.0 ms | 403.0 ms–470.3 ms | 8.32× more | 132.3 MB | 5.69× more | file-read | yes | — |
| oh-my-pi (omp) | 1.49 s | 1.43 s–2.02 s | 30.17× more | 409.5 MB | 17.61× more | file-read | yes | — |
| oh-my-openagent (omo) | 7.29 s | 7.01 s–9.39 s | 147.59× more | 873.3 MB | 37.56× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 80.8 ms | 78.1 ms–100.0 ms | baseline | 19.5 MB | baseline | mcp-call | yes | 61.2 ms | — |
| jcode | 418.5 ms | 353.2 ms–491.9 ms | 5.18× more | 52.1 MB | 2.67× more | mcp-action | yes | 397.5 ms | — |
| Claude Code | 418.8 ms | 410.7 ms–511.8 ms | 5.18× more | 215.3 MB | 11.03× more | mcp-tool | yes | 309.9 ms | — |
| Codex CLI | 336.4 ms | 326.8 ms–343.2 ms | 4.16× more | 142.6 MB | 7.30× more | mcp-tool | yes | 223.2 ms | — |
| OpenCode | 3.61 s | 2.94 s–4.04 s | 44.63× more | 752.8 MB | 38.54× more | mcp-tool | yes | 3.03 s | — |
| pi | 457.4 ms | 433.6 ms–597.1 ms | 5.66× more | 153.9 MB | 7.88× more | mcp-tool | yes | 399.7 ms | — |
| oh-my-pi (omp) | 1.55 s | 1.36 s–1.56 s | 19.18× more | 406.2 MB | 20.80× more | mcp-route | yes | 1.34 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 28.9 MB | 135.6 KB | baseline | 0.32 | 1.00 | 1 | 0.00 | 34.9 MB | 396.6 KB |
| jcode | 36.4 MB | 218.5 KB | 1.61× more | — | — | 1 | 0.00 | 40.0 MB | 346.9 KB |
| Claude Code | 159.3 MB | 157.3 MB | 1,188× more | — | — | 1 | 1.00 | 189.2 MB | 185.4 MB |
| Codex CLI | 99.3 MB | 83.0 MB | 626.72× more | — | — | 2 | 1.21 | 106.8 MB | 107.4 MB |
| OpenCode | 242.9 MB | 2.5 MB | 18.79× more | — | — | 1 | 0.00 | 578.9 MB | 4.6 MB |
| pi | 118.5 MB | 115.2 MB | 869.63× more | — | — | 1 | 1.00 | 148.6 MB | 147.7 MB |
| oh-my-pi (omp) | 389.6 MB | 206.8 MB | 1,561× more | — | — | 1 | 1.00 | 465.5 MB | 320.9 MB |
| oh-my-openagent (omo) | 129.7 MB | 51.4 MB | 388.17× more | — | — | 1 | 1.00 | 830.9 MB | 335.2 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.9 MB | 36.7 MB | 38.4 MB |
| jcode | 40.0 MB | 41.3 MB | 43.1 MB |
| Claude Code | 189.2 MB | 933.4 MB | 1.8 GB |
| Codex CLI | 106.8 MB | 537.0 MB | 1.0 GB |
| OpenCode | 578.9 MB | 597.1 MB | 620.2 MB |
| pi | 148.6 MB | 738.4 MB | 1.4 GB |
| oh-my-pi (omp) | 465.5 MB | 1.8 GB | 3.3 GB |
| oh-my-openagent (omo) | 830.9 MB | 3.0 GB | 3.8 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 2 / 1 | 8 / 5 | 13 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 1 / 4 | 5 / 20 | 10 / 40 |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    7.75 ms
  Max:    12.77 ms
  Mean:   9.83 ms
  Median: 9.27 ms
  Stdev:  1.58 ms

--version:
  Min:    14.16 ms
  Max:    20.50 ms
  Mean:   15.94 ms
  Median: 15.60 ms
  Stdev:  1.79 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    33.08 ms
  Max:    115.87 ms
  Mean:   77.21 ms
  Median: 89.39 ms
  Stdev:  27.25 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.3%
  logging_init        1.5       1.5   56.9%
  nofile_limit        1.8       0.3   10.8%
  perm_harden         1.8       0.1    2.1%
  args_parse          1.8       0.0    0.0%
  config_loaded       2.1       0.3   11.2%
  server_new          2.4       0.3   10.2%
  socket_bound        2.6       0.2    8.5%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    26.37 ms
  Max:    101.93 ms
  Mean:   68.18 ms
  Median: 73.56 ms
  Stdev:  24.15 ms

Bridge stage: hello_ok:
  Min:    27.35 ms
  Max:    103.38 ms
  Mean:   69.24 ms
  Median: 74.54 ms
  Stdev:  24.17 ms

Bridge stage: no-op reply:
  Min:    27.86 ms
  Max:    104.35 ms
  Mean:   69.89 ms
  Median: 75.22 ms
  Stdev:  24.23 ms

Bridge total (socket -> no-op):
  Min:    27.86 ms
  Max:    104.35 ms
  Mean:   69.89 ms
  Median: 75.22 ms
  Stdev:  24.23 ms

============================================================
Summary:
  Binary load median:      ~9.3 ms
  Server ready median:     ~89.4 ms
  Bridge total median:     ~75.2 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f4e27dc87ca7)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (19590f0cec4e, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-tde6w2f2
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.024/0.025    p95    0.038/0.036    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.210/0.217    p95    0.369/0.283    delta  +0.007 ms  control-no-change
  mcp_config_load   p50    0.245/0.255    p95    0.357/0.312    delta  +0.010 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   14.010/15.428   p95   23.590/95.649   delta  +1.418 ms  control-no-change
  total store_read_bytes p50 22940  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22954 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22954 (4 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.225/3.819    p95    5.665/4.444    delta  -0.406 ms  control-no-change
  skill_request_overlay p50    5.259/4.891    p95    6.068/7.560    delta  -0.369 ms  control-no-change
  mcp_config_load   p50    0.267/0.273    p95    0.370/0.398    delta  +0.007 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.005/0.001    delta  -0.000 ms  control-drift
  session_save      p50   16.307/15.746   p95   21.902/19.574   delta  -0.560 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22954 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22918..22950 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.025/0.028    p95    0.034/0.046    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.243/0.245    p95    0.282/0.295    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.283/0.281    p95    0.299/0.304    delta  -0.002 ms  control-no-change
  attach_history    p50  233.627/260.016  p95  255.595/318.750  delta +26.389 ms  control-no-change
  resume_ready      p50   59.964/62.086   p95   77.004/83.571   delta  +2.122 ms  control-no-change
  provider_dispatch p50    1.040/1.180    p95    1.242/2.077    delta  +0.140 ms  control-no-change
  session_save      p50   14.472/14.845   p95   45.482/49.158   delta  +0.373 ms  control-no-change
  total store_read_bytes p50 5311305  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 233.627 ms (load 198.950 + history 35.033)
    selection          p50     0.070 ms  min     0.048  max     0.073  ratio 0.0% of anchor
    checkpoint_recent  p50   153.453 ms  min   151.497  max   165.236  ratio 65.7% of anchor
    log_read           p50     0.070 ms  min     0.067  max     0.097  ratio 0.0% of anchor
    full_scan          p50    22.213 ms  min    20.972  max    24.028  ratio 9.5% of anchor
    blob_read          p50     0.006 ms  min     0.005  max     0.006  ratio 0.0% of anchor
    graph_replay       p50    16.192 ms  min    13.733  max    16.813  ratio 6.9% of anchor
    metadata_json      p50     0.020 ms  min     0.015  max     0.036  ratio 0.0% of anchor
    messages_json      p50     0.866 ms  min     0.725  max     1.171  ratio 0.4% of anchor
    messages_decode    p50     0.545 ms  min     0.519  max     0.634  ratio 0.2% of anchor
    source_hash        p50     2.073 ms  min     1.997  max     2.922  ratio 0.9% of anchor
    history_rescan     p50    47.603 ms  min    42.093  max    50.659  ratio 20.4% of anchor
    page_index         p50    50.930 ms  min    50.608  max    58.555  ratio 21.8% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189201..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22964 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099143..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22968 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.145/4.142    p95    6.102/5.182    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    5.286/5.293    p95    7.050/6.981    delta  +0.007 ms  control-no-change
  mcp_config_load   p50    0.276/0.269    p95    0.669/0.442    delta  -0.007 ms  control-no-change
  attach_history    p50  263.091/260.494  p95  298.677/295.757  delta  -2.597 ms  control-no-change
  resume_ready      p50   59.874/61.813   p95   68.736/88.764   delta  +1.939 ms  control-no-change
  provider_dispatch p50    1.001/1.077    p95    1.449/1.834    delta  +0.076 ms  control-no-change
  session_save      p50   14.497/14.854   p95   51.119/32.729   delta  +0.357 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 263.091 ms (load 226.565 + history 37.581)
    selection          p50     0.045 ms  min     0.040  max     0.068  ratio 0.0% of anchor
    checkpoint_recent  p50   164.651 ms  min   154.623  max   169.778  ratio 62.6% of anchor
    log_read           p50     0.081 ms  min     0.079  max     0.082  ratio 0.0% of anchor
    full_scan          p50    20.529 ms  min    20.104  max    20.813  ratio 7.8% of anchor
    blob_read          p50     0.005 ms  min     0.005  max     0.006  ratio 0.0% of anchor
    graph_replay       p50    14.785 ms  min    14.107  max    16.283  ratio 5.6% of anchor
    metadata_json      p50     0.014 ms  min     0.012  max     0.019  ratio 0.0% of anchor
    messages_json      p50     0.771 ms  min     0.731  max     2.260  ratio 0.3% of anchor
    messages_decode    p50     0.489 ms  min     0.485  max     0.489  ratio 0.2% of anchor
    source_hash        p50     2.056 ms  min     1.986  max     2.073  ratio 0.8% of anchor
    history_rescan     p50    39.265 ms  min    37.837  max    40.974  ratio 14.9% of anchor
    page_index         p50    44.343 ms  min    43.621  max    44.958  ratio 16.9% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22967 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22964 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.033/0.030    p95    0.076/0.037    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.221/0.218    p95    0.666/0.324    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.264/0.257    p95    0.668/0.404    delta  -0.007 ms  control-no-change
  attach_history    p50 2965.518/2960.937 p95 3735.853/3322.781 delta  -4.581 ms  control-no-change
  resume_ready      p50  740.259/773.123  p95  919.936/939.662  delta +32.864 ms  control-no-change
  provider_dispatch p50   11.523/11.846   p95   15.865/18.206   delta  +0.323 ms  control-no-change
  session_save      p50   25.375/19.180   p95   35.875/45.499   delta  -6.195 ms  control-no-change
  total store_read_bytes p50 53417521  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2965.518 ms (load 2491.246 + history 419.266)
    selection          p50     0.079 ms  min     0.078  max     0.813  ratio 0.0% of anchor
    checkpoint_recent  p50  2135.125 ms  min  2052.704  max  2315.665  ratio 72.0% of anchor
    log_read           p50     2.486 ms  min     1.332  max     2.630  ratio 0.1% of anchor
    full_scan          p50   256.517 ms  min   223.812  max   284.124  ratio 8.6% of anchor
    blob_read          p50     0.071 ms  min     0.068  max     0.075  ratio 0.0% of anchor
    graph_replay       p50   211.882 ms  min   203.481  max   266.415  ratio 7.1% of anchor
    metadata_json      p50     0.035 ms  min     0.021  max     0.042  ratio 0.0% of anchor
    messages_json      p50    14.627 ms  min     8.015  max    15.102  ratio 0.5% of anchor
    messages_decode    p50    12.694 ms  min    11.933  max    15.639  ratio 0.4% of anchor
    source_hash        p50    20.578 ms  min    19.037  max    21.541  ratio 0.7% of anchor
    history_rescan     p50   443.714 ms  min   443.102  max   487.495  ratio 15.0% of anchor
    page_index         p50   453.658 ms  min   412.323  max   530.375  ratio 15.3% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184297..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210247..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22949..22975 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22975 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.460/4.524    p95    5.558/6.336    delta  +0.064 ms  control-no-change
  skill_request_overlay p50    5.530/5.626    p95    8.943/10.180   delta  +0.096 ms  control-no-change
  mcp_config_load   p50    0.285/0.286    p95    0.541/0.490    delta  +0.000 ms  control-no-change
  attach_history    p50 2936.378/3033.122 p95 3518.760/3580.549 delta +96.744 ms  control-no-change
  resume_ready      p50  783.393/802.524  p95  976.143/1031.780 delta +19.130 ms  control-no-change
  provider_dispatch p50   12.101/14.531   p95   18.954/20.083   delta  +2.429 ms  control-no-change
  session_save      p50   22.953/25.785   p95   38.823/103.822  delta  +2.832 ms  control-no-change
  total store_read_bytes p50 53417518  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2936.378 ms (load 2497.239 + history 471.603)
    selection          p50     0.115 ms  min     0.066  max     0.165  ratio 0.0% of anchor
    checkpoint_recent  p50  2127.532 ms  min  2019.338  max  2523.475  ratio 72.5% of anchor
    log_read           p50     1.462 ms  min     1.229  max     2.498  ratio 0.0% of anchor
    full_scan          p50   259.577 ms  min   259.568  max   349.707  ratio 8.8% of anchor
    blob_read          p50     0.114 ms  min     0.064  max     0.171  ratio 0.0% of anchor
    graph_replay       p50   250.551 ms  min   242.866  max   282.692  ratio 8.5% of anchor
    metadata_json      p50     0.032 ms  min     0.025  max     0.062  ratio 0.0% of anchor
    messages_json      p50    22.929 ms  min    12.076  max    24.907  ratio 0.8% of anchor
    messages_decode    p50    14.178 ms  min    11.963  max    17.128  ratio 0.5% of anchor
    source_hash        p50    26.917 ms  min    21.271  max    27.287  ratio 0.9% of anchor
    history_rescan     p50   533.280 ms  min   460.869  max   538.786  ratio 18.2% of anchor
    page_index         p50   526.720 ms  min   499.274  max   590.832  ratio 17.9% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22967 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210249..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22971 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f4e27dc87ca7)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (19590f0cec4e, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-xpgwkrn3
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.030/0.029    p95    0.036/0.085    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.244/0.251    p95    0.350/0.422    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.281/0.280    p95    0.348/0.299    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   13.539/14.341   p95   39.099/16.676   delta  +0.802 ms  control-no-change
  total store_read_bytes p50 23665  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22951, 23649, 23665, 24367, 24375, 24379, 24391]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22947, 23653, 23665, 23669, 24367, 24371, 24375]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22951..24391 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22947..24375 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.032/4.134    p95    5.840/4.864    delta  +0.102 ms  control-no-change
  skill_request_overlay p50    5.253/5.271    p95    6.670/6.384    delta  +0.018 ms  control-no-change
  mcp_config_load   p50    0.277/0.274    p95    0.301/0.307    delta  -0.003 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   14.316/13.571   p95   23.004/14.711   delta  -0.745 ms  control-no-change
  total store_read_bytes p50 23663  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22931, 22935, 22945, 23659, 23661, 23665, 24367, 24373, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22935, 22949, 22957, 23663, 23667, 23673, 24365, 24369, 24373, 24377]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22931..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22935..24377 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.025/0.028    p95    0.077/0.040    delta  +0.004 ms  control-no-change
  skill_request_overlay p50    0.240/0.228    p95    0.806/0.257    delta  -0.012 ms  control-no-change
  mcp_config_load   p50    0.267/0.262    p95    0.421/0.302    delta  -0.006 ms  control-no-change
  attach_history    p50  105.317/106.466  p95  151.749/166.591  delta  +1.149 ms  control-no-change
  resume_ready      p50   65.516/70.225   p95   80.887/126.357  delta  +4.709 ms  control-no-change
  provider_dispatch p50    1.187/1.212    p95    1.309/1.340    delta  +0.024 ms  control-no-change
  session_save      p50   14.060/15.583   p95   20.443/25.328   delta  +1.523 ms  control-no-change
  total store_read_bytes p50 2841253  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23678, 24386, 24388, 25100, 25106, 25802, 25808, 26528, 26534]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 23678, 24380, 24388, 25094, 25812, 26522, 26534]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26534 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26534 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.879/4.483    p95    9.696/12.839   delta  -0.396 ms  control-no-change
  skill_request_overlay p50    5.596/5.585    p95    6.828/13.143   delta  -0.011 ms  control-no-change
  mcp_config_load   p50    0.269/0.297    p95    0.294/0.859    delta  +0.028 ms  control-no-change
  attach_history    p50  110.187/121.376  p95  139.626/165.329  delta +11.189 ms  control-no-change
  resume_ready      p50   84.565/80.616   p95  106.521/127.912  delta  -3.949 ms  control-no-change
  provider_dispatch p50    1.418/1.407    p95    1.871/1.740    delta  -0.011 ms  control-no-change
  session_save      p50   22.306/17.703   p95   38.442/31.074   delta  -4.603 ms  control-no-change
  total store_read_bytes p50 2841251  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23676, 23678, 24382, 24386, 25084, 25102, 25810, 25820, 26524, 26532]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23658, 23670, 24380, 24388, 25094, 25102, 25812, 26526, 26530]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23676..26532 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23658..26530 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.037/0.031    p95    0.192/0.067    delta  -0.007 ms  control-no-change
  skill_request_overlay p50    0.244/0.268    p95    1.037/0.610    delta  +0.024 ms  control-no-change
  mcp_config_load   p50    0.277/0.294    p95    0.969/0.623    delta  +0.017 ms  control-no-change
  attach_history    p50 1682.940/1575.441 p95 2159.316/1821.048 delta -107.499 ms  control-no-change
  resume_ready      p50 1089.213/963.415  p95 1233.380/1302.587 delta -125.798 ms  control-no-change
  provider_dispatch p50   15.128/15.052   p95   26.326/25.294   delta  -0.076 ms  control-no-change
  session_save      p50   28.406/28.943   p95   51.609/45.717   delta  +0.536 ms  control-no-change
  total store_read_bytes p50 28640284  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23673, 24375, 24383, 25105, 25113, 25807, 25815, 26521, 26541]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23661, 23677, 24381, 24397, 25097, 25099, 25805, 25827, 26525, 26531]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26541 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23661..26531 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.192/4.139    p95    5.824/9.472    delta  -0.054 ms  control-no-change
  skill_request_overlay p50    5.295/5.353    p95    6.824/7.915    delta  +0.058 ms  control-no-change
  mcp_config_load   p50    0.295/0.289    p95    0.604/0.456    delta  -0.006 ms  control-no-change
  attach_history    p50 1396.894/1353.383 p95 1689.842/1762.444 delta -43.512 ms  control-no-change
  resume_ready      p50  850.071/797.340  p95 1085.572/895.634  delta -52.731 ms  control-no-change
  provider_dispatch p50   13.438/14.289   p95   18.092/16.968   delta  +0.851 ms  control-no-change
  session_save      p50   26.799/22.278   p95   47.177/32.080   delta  -4.521 ms  control-no-change
  total store_read_bytes p50 28640269  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23680, 24394, 24400, 25092, 25100, 25806, 25818, 26528]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23672, 23676, 24378, 24386, 25112, 25116, 25810, 26520, 26532]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26528 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23672..26532 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.8 ms | 1.3 ms | 0.6 ms | 1.3 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.6 ms | 0.4 ms | 0.6 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| write (small) | 2.1 ms | 2.1 ms | 1.9 ms | 2.1 ms | 5 |
| write (10KB) | 2.6 ms | 3.9 ms | 2.4 ms | 3.9 ms | 5 |
| edit (simple replace) | 2.3 ms | 4.4 ms | 2.0 ms | 4.4 ms | 5 |
| ls (scripts dir) | 0.8 ms | 3.3 ms | 0.7 ms | 3.3 ms | 5 |
| agentgrep (grep in tree) | 5.2 ms | 5.7 ms | 4.1 ms | 5.7 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 1.6 ms | 1.4 ms | 1.6 ms | 5 |
| bash (true) | 5.0 ms | 6.1 ms | 4.7 ms | 6.1 ms | 5 |
| bash (echo hello) | 5.3 ms | 7.2 ms | 4.9 ms | 7.2 ms | 5 |
| bash (ls fixture tree) | 7.7 ms | 8.6 ms | 7.5 ms | 8.6 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | — | — | — | — | — | — | 0 | — |
| read (1000 lines) | — | — | — | — | — | — | 0 | — |
| read (10 lines @ offset) | — | — | — | — | — | — | 0 | — |
| read (repo Cargo.toml) | — | — | — | — | — | — | 0 | — |
| write (small) | — | — | — | — | — | — | 0 | — |
| write (10KB) | — | — | — | — | — | — | 0 | — |
| edit (simple replace) | — | — | — | — | — | — | 0 | — |
| ls (scripts dir) | — | — | — | — | — | — | 0 | — |
| agentgrep (grep in tree) | — | — | — | — | — | — | 0 | — |
| agentgrep (find **/*.rs) | — | — | — | — | — | — | 0 | — |
| bash (true) | — | — | — | — | — | — | 0 | — |
| bash (echo hello) | — | — | — | — | — | — | 0 | — |
| bash (ls fixture tree) | — | — | — | — | — | — | 0 | — |
| invalid tool (dispatch only) | — | — | — | — | — | — | 0 | — |

CPU unmeasured: no /proc CPU counters for the daemon on this system

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 58.6 ms | 58.6 ms | 58.6 ms | 58.7 ms | 0 |
| 10 | 220.2 ms | 226.1 ms | 226.1 ms | 226.3 ms | 0 |
| 40 | 637.6 ms | 707.8 ms | 707.8 ms | 708.4 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.55 s | 1.55 s | 1.55 s | 1 |
| leaf_touch | src/main.rs | 755.3 ms | 755.3 ms | 755.3 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 29.05 s | 29.05 s | 29.05 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 68.58 s | 68.58 s | 68.58 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (2f63e44) | 13.2 ms | 95.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 15.1 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 42.1 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 94.0 ms | 479.4 MB |
| OpenCode | 1.19.5 | 500.9 ms | 605.3 MB |
| pi | 1.5.0 | 258.4 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 657.1 ms | 292.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 194.5 ms | 514.9 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 164.2 ms | 151.8 ms–236.8 ms | baseline | 129.1 MB | baseline | yes | 3,664 / 2,368 (1 tools) | — |
| jcode | 131.5 ms | 123.5 ms–215.3 ms | 1.25× less | 32.0 MB | 4.03× less | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.27 s | 1.25 s–2.06 s | 7.72× more | 273.4 MB | 2.12× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 1.90 s | 1.67 s–2.10 s | 11.58× more | 327.8 MB | 2.54× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 4.78 s | 4.63 s–7.51 s | 29.14× more | 842.1 MB | 6.52× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 450.7 ms | 438.2 ms–483.8 ms | 2.75× more | 124.0 MB | 1.04× less | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.87 s | 2.74 s–3.33 s | 17.50× more | 431.9 MB | 3.35× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 26.80 s | 17.99 s–39.31 s | 163.27× more | 563.9 MB | 4.37× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 206.3 ms | 178.6 ms–232.7 ms | baseline | 36.1 MB | baseline | — | yes | — | 11,033 / 9,079 (8 tools) |
| jcode | — | — | — | — | — | — | — | skipped: jcode has no core tool profile | — |
| Claude Code | — | — | — | — | — | — | — | skipped: Claude Code has no core tool profile | — |
| Codex CLI | — | — | — | — | — | — | — | skipped: Codex CLI has no core tool profile | — |
| OpenCode | — | — | — | — | — | — | — | skipped: OpenCode has no core tool profile | — |
| pi | — | — | — | — | — | — | — | skipped: pi has no core tool profile | — |
| oh-my-pi (omp) | — | — | — | — | — | — | — | skipped: oh-my-pi (omp) has no core tool profile | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | — | skipped: oh-my-openagent (omo) has no core tool profile | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 243.4 ms | 200.0 ms–303.6 ms | baseline | 24.5 MB | baseline | skill-uri | yes | — |
| jcode | 195.4 ms | 178.0 ms–214.8 ms | 1.25× less | 23.5 MB | 1.04× less | skill-tool | yes | — |
| Claude Code | 901.1 ms | 872.8 ms–1.05 s | 3.70× more | 238.9 MB | 9.75× more | skill-tool | yes | — |
| Codex CLI | 2.04 s | 1.69 s–2.11 s | 8.38× more | 262.0 MB | 10.69× more | file-read | yes | — |
| OpenCode | 6.45 s | 6.42 s–6.79 s | 26.48× more | 742.0 MB | 30.28× more | skill-tool | yes | — |
| pi | 611.6 ms | 583.8 ms–708.0 ms | 2.51× more | 97.8 MB | 3.99× more | file-read | yes | — |
| oh-my-pi (omp) | 3.17 s | 2.72 s–3.66 s | 13.03× more | 422.4 MB | 17.24× more | file-read | yes | — |
| oh-my-openagent (omo) | 20.38 s | 18.24 s–29.40 s | 83.72× more | 542.2 MB | 22.13× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 265.1 ms | 220.6 ms–369.6 ms | baseline | 37.3 MB | baseline | mcp-call | yes | 139.0 ms | — |
| jcode | 345.9 ms | 311.8 ms–449.3 ms | 1.30× more | 36.9 MB | same | mcp-action | yes | 217.2 ms | — |
| Claude Code | 970.1 ms | 910.0 ms–1.05 s | 3.66× more | 274.7 MB | 7.37× more | mcp-tool | yes | 818.5 ms | — |
| Codex CLI | 956.9 ms | 811.0 ms–2.73 s | 3.61× more | 177.9 MB | 4.77× more | mcp-tool | yes | 777.8 ms | — |
| OpenCode | 7.26 s | 6.64 s–10.00 s | 27.38× more | 3.3 GB | 91.57× more | mcp-tool | yes | 5.78 s | — |
| pi | 680.9 ms | 674.8 ms–788.6 ms | 2.57× more | 118.4 MB | 3.18× more | mcp-tool | yes | 545.8 ms | — |
| oh-my-pi (omp) | 3.52 s | 2.83 s–4.06 s | 13.29× more | 440.0 MB | 11.80× more | mcp-route | yes | 2.60 s | — |
| oh-my-openagent (omo) | 21.73 s | 19.58 s–26.73 s | 81.96× more | 562.5 MB | 15.09× more | mcp-tool | yes | 18.15 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 22.2 MB | 110.0 KB | baseline | — | — | 1 | 0.00 | 29.8 MB | 344.6 KB |
| jcode | 22.0 MB | 69.8 KB | 1.58× less | — | — | 1 | 0.00 | 27.1 MB | 406.3 KB |
| Claude Code | 153.9 MB | 166.1 MB | 1,546× more | — | — | 2 | 2.23 | 185.4 MB | 189.6 MB |
| Codex CLI | 125.8 MB | 96.1 MB | 893.95× more | — | — | 6 | 3.00 | 165.1 MB | 116.0 MB |
| OpenCode | 246.2 MB | 0.0 B | — | — | — | 2 | 0.00 | 435.8 MB | 6.9 MB |
| pi | 91.2 MB | 90.7 MB | 844.29× more | — | — | 2 | 2.00 | 101.6 MB | 101.0 MB |
| oh-my-pi (omp) | 330.3 MB | 130.4 MB | 1,214× more | — | — | 2 | 2.00 | 342.0 MB | 340.4 MB |
| oh-my-openagent (omo) | 148.9 MB | 133.2 MB | 1,240× more | — | — | 4 | 4.00 | 416.4 MB | 443.1 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.8 MB | 31.7 MB | 32.9 MB |
| jcode | 27.1 MB | 28.6 MB | 30.6 MB |
| Claude Code | 185.4 MB | 948.6 MB | 1.8 GB |
| Codex CLI | 165.1 MB | 648.1 MB | 1.2 GB |
| OpenCode | 435.8 MB | 492.0 MB | 499.7 MB |
| pi | 101.6 MB | 505.2 MB | 1010.6 MB |
| oh-my-pi (omp) | 342.0 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 416.4 MB | 2.1 GB | 4.0 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 2 / 2 | 10 / 10 | 22 / 20 |
| Codex CLI | 6 / 6 | 18 / 18 | 33 / 30 |
| OpenCode | 2 / 2 | 2 / 2 | 2 / 2 |
| pi | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-pi (omp) | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-openagent (omo) | 4 / 6 | 20 / 50 | 40 / 64 |

**Errors recorded during measurement**

- oh-my-openagent (omo) @ 10 sessions: turn 2 session 6: Connection error.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 6: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    14.34 ms
  Max:    15.61 ms
  Mean:   14.61 ms
  Median: 14.51 ms
  Stdev:  0.37 ms

--version:
  Min:    13.76 ms
  Max:    15.78 ms
  Mean:   14.38 ms
  Median: 14.09 ms
  Stdev:  0.64 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.32 ms
  Max:    28.17 ms
  Mean:   23.19 ms
  Median: 22.73 ms
  Stdev:  1.77 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.3       1.3   19.1%
  nofile_limit        1.8       0.5    8.1%
  perm_harden         4.4       2.6   38.9%
  args_parse          4.4       0.0    0.0%
  config_loaded       5.4       0.9   13.7%
  server_new          6.4       1.0   14.9%
  socket_bound        6.7       0.3    5.2%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~14.5 ms
  Server ready median:     ~22.7 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6cb0c264e226)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (aa178149eb6c, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-l2rabmgl
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.101/0.101    p95    0.118/0.103    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.740/0.741    p95    1.430/0.779    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.927/0.912    p95    1.009/0.951    delta  -0.015 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.002/0.001    delta  -0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   49.280/50.452   p95   63.726/51.507   delta  +1.172 ms  control-no-change
  total store_read_bytes p50 22950  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22960 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22894..22956 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   14.060/14.031   p95   16.640/15.193   delta  -0.030 ms  control-no-change
  skill_request_overlay p50   37.314/37.276   p95   43.149/37.854   delta  -0.039 ms  control-no-change
  mcp_config_load   p50    0.996/0.969    p95    1.086/1.098    delta  -0.027 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.005    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   57.346/61.789   p95   77.509/103.402  delta  +4.443 ms  control-no-change
  total store_read_bytes p50 22956  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22968 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22900..22960 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.100/0.100    p95    0.127/0.123    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.743/0.780    p95    0.970/0.818    delta  +0.037 ms  control-no-change
  mcp_config_load   p50    0.948/0.938    p95    1.097/0.992    delta  -0.010 ms  control-no-change
  attach_history    p50  362.206/364.279  p95  367.391/387.287  delta  +2.073 ms  control-no-change
  resume_ready      p50  144.810/145.456  p95  156.344/150.543  delta  +0.646 ms  control-no-change
  provider_dispatch p50    1.587/1.580    p95    1.621/1.663    delta  -0.007 ms  control-no-change
  session_save      p50   45.737/47.092   p95   69.525/55.629   delta  +1.355 ms  control-no-change
  total store_read_bytes p50 5311214  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 362.206 ms (load 315.842 + history 46.548)
    selection          p50     0.123 ms  min     0.116  max     0.149  ratio 0.0% of anchor
    checkpoint_recent  p50   261.262 ms  min   260.500  max   266.811  ratio 72.1% of anchor
    log_read           p50     0.171 ms  min     0.159  max     0.197  ratio 0.0% of anchor
    full_scan          p50    25.536 ms  min    25.459  max    25.652  ratio 7.1% of anchor
    blob_read          p50     0.004 ms  min     0.003  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    18.955 ms  min    18.811  max    19.590  ratio 5.2% of anchor
    metadata_json      p50     0.009 ms  min     0.009  max     0.017  ratio 0.0% of anchor
    messages_json      p50     1.445 ms  min     1.415  max     1.445  ratio 0.4% of anchor
    messages_decode    p50     0.945 ms  min     0.905  max     0.949  ratio 0.3% of anchor
    source_hash        p50     0.505 ms  min     0.481  max     0.511  ratio 0.1% of anchor
    history_rescan     p50    45.311 ms  min    45.264  max    46.128  ratio 12.5% of anchor
    page_index         p50    69.309 ms  min    67.550  max    71.439  ratio 19.1% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22939..22968 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22923..22972 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.984/15.162   p95   15.535/16.418   delta  +0.178 ms  control-no-change
  skill_request_overlay p50   37.118/37.180   p95   37.817/40.510   delta  +0.061 ms  control-no-change
  mcp_config_load   p50    0.972/0.962    p95    1.038/1.016    delta  -0.010 ms  control-no-change
  attach_history    p50  364.640/360.173  p95  393.184/367.597  delta  -4.468 ms  control-no-change
  resume_ready      p50  145.677/145.067  p95  151.526/158.421  delta  -0.610 ms  control-no-change
  provider_dispatch p50    1.518/1.519    p95    1.584/1.570    delta  +0.001 ms  control-no-change
  session_save      p50   48.411/50.092   p95   54.576/61.856   delta  +1.681 ms  control-no-change
  total store_read_bytes p50 5311217  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 364.640 ms (load 317.040 + history 46.690)
    selection          p50     0.148 ms  min     0.146  max     0.148  ratio 0.0% of anchor
    checkpoint_recent  p50   271.155 ms  min   263.524  max   275.445  ratio 74.4% of anchor
    log_read           p50     0.169 ms  min     0.149  max     0.181  ratio 0.0% of anchor
    full_scan          p50    25.812 ms  min    25.704  max    26.077  ratio 7.1% of anchor
    blob_read          p50     0.005 ms  min     0.003  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    19.309 ms  min    19.282  max    20.283  ratio 5.3% of anchor
    metadata_json      p50     0.009 ms  min     0.009  max     0.009  ratio 0.0% of anchor
    messages_json      p50     1.438 ms  min     1.422  max     1.520  ratio 0.4% of anchor
    messages_decode    p50     0.972 ms  min     0.926  max     0.980  ratio 0.3% of anchor
    source_hash        p50     0.505 ms  min     0.491  max     0.514  ratio 0.1% of anchor
    history_rescan     p50    45.817 ms  min    45.296  max    46.514  ratio 12.6% of anchor
    page_index         p50    67.981 ms  min    66.201  max    69.851  ratio 18.6% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22936..22976 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22972 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.101/0.101    p95    0.135/0.131    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.766/0.753    p95    0.834/0.820    delta  -0.013 ms  control-no-change
  mcp_config_load   p50    0.919/0.906    p95    1.026/0.959    delta  -0.013 ms  control-no-change
  attach_history    p50 4077.018/4077.666 p95 4102.168/4103.049 delta  +0.648 ms  control-no-change
  resume_ready      p50 1524.580/1528.909 p95 1537.710/1536.894 delta  +4.330 ms  control-no-change
  provider_dispatch p50   16.943/16.571   p95   18.048/17.785   delta  -0.372 ms  control-no-change
  session_save      p50   45.276/44.277   p95   52.495/48.612   delta  -0.999 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 4077.018 ms (load 3537.741 + history 540.073)
    selection          p50     0.143 ms  min     0.142  max     0.149  ratio 0.0% of anchor
    checkpoint_recent  p50  3023.910 ms  min  3012.509  max  3025.102  ratio 74.2% of anchor
    log_read           p50     1.673 ms  min     1.653  max     1.833  ratio 0.0% of anchor
    full_scan          p50   262.749 ms  min   260.728  max   263.440  ratio 6.4% of anchor
    blob_read          p50     0.065 ms  min     0.062  max     0.067  ratio 0.0% of anchor
    graph_replay       p50   245.092 ms  min   242.827  max   264.607  ratio 6.0% of anchor
    metadata_json      p50     0.013 ms  min     0.012  max     0.013  ratio 0.0% of anchor
    messages_json      p50    20.058 ms  min    19.904  max    20.421  ratio 0.5% of anchor
    messages_decode    p50    15.107 ms  min    13.841  max    15.614  ratio 0.4% of anchor
    source_hash        p50     4.662 ms  min     4.522  max     4.729  ratio 0.1% of anchor
    history_rescan     p50   531.071 ms  min   530.553  max   557.219  ratio 13.0% of anchor
    page_index         p50   593.496 ms  min   586.020  max   622.276  ratio 14.6% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22931..22973 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22975 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.572/14.707   p95   15.888/15.596   delta  +0.135 ms  control-no-change
  skill_request_overlay p50   37.711/37.805   p95   47.551/46.807   delta  +0.094 ms  control-no-change
  mcp_config_load   p50    0.994/0.989    p95    1.421/1.346    delta  -0.005 ms  control-no-change
  attach_history    p50 4087.228/4109.465 p95 4227.287/4773.957 delta +22.237 ms  control-no-change
  resume_ready      p50 1523.848/1530.143 p95 1549.087/1630.703 delta  +6.296 ms  control-no-change
  provider_dispatch p50   17.983/17.195   p95   22.671/19.764   delta  -0.788 ms  control-no-change
  session_save      p50   45.428/46.081   p95  397.726/77.774   delta  +0.654 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 4087.228 ms (load 3549.249 + history 541.386)
    selection          p50     0.153 ms  min     0.134  max     0.211  ratio 0.0% of anchor
    checkpoint_recent  p50  2996.752 ms  min  2984.384  max  3010.270  ratio 73.3% of anchor
    log_read           p50     1.655 ms  min     1.588  max     1.693  ratio 0.0% of anchor
    full_scan          p50   260.883 ms  min   259.596  max   266.187  ratio 6.4% of anchor
    blob_read          p50     0.065 ms  min     0.064  max     0.068  ratio 0.0% of anchor
    graph_replay       p50   245.313 ms  min   243.528  max   254.399  ratio 6.0% of anchor
    metadata_json      p50     0.013 ms  min     0.012  max     0.013  ratio 0.0% of anchor
    messages_json      p50    19.928 ms  min    19.878  max    20.011  ratio 0.5% of anchor
    messages_decode    p50    16.007 ms  min    15.997  max    16.112  ratio 0.4% of anchor
    source_hash        p50     4.574 ms  min     4.503  max     4.643  ratio 0.1% of anchor
    history_rescan     p50   536.416 ms  min   524.816  max   537.513  ratio 13.1% of anchor
    page_index         p50   578.396 ms  min   577.827  max   599.834  ratio 14.2% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22927..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22946..22979 (8 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6cb0c264e226)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (aa178149eb6c, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-9hltqr7g
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.101/0.099    p95    0.114/0.107    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.770/0.750    p95    0.857/0.934    delta  -0.021 ms  control-no-change
  mcp_config_load   p50    0.937/0.921    p95    1.023/0.947    delta  -0.016 ms  control-no-change
  attach_history    p50    0.001/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   35.250/35.481   p95   78.387/45.242   delta  +0.231 ms  control-no-change
  total store_read_bytes p50 23638  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22901, 22941, 22947, 22957, 23637, 23640, 23642, 24337, 24351, 24359]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22899, 22949, 22958, 22961, 23603, 23635, 23636, 24343, 24348, 24355]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22901..24359 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22899..24355 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   14.987/14.834   p95   20.135/17.201   delta  -0.153 ms  control-no-change
  skill_request_overlay p50   38.277/38.849   p95   39.911/42.545   delta  +0.572 ms  control-no-change
  mcp_config_load   p50    0.955/0.977    p95    1.095/1.057    delta  +0.022 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.009/0.007    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50  112.103/114.032  p95  203.740/249.008  delta  +1.929 ms  control-no-change
  total store_read_bytes p50 23268  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22907, 22950, 22952, 22953, 23584, 23612, 23624, 23643, 24334]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22947, 22951, 22955, 22961, 23616, 23634, 23648, 24343, 24353, 24356]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22907..24334 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22947..24356 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.100/0.105    p95    0.123/0.139    delta  +0.005 ms  control-no-change
  skill_request_overlay p50    0.795/0.745    p95    1.060/0.821    delta  -0.050 ms  control-no-change
  mcp_config_load   p50    0.893/0.914    p95    1.305/1.043    delta  +0.021 ms  control-no-change
  attach_history    p50  199.988/198.835  p95  215.488/205.023  delta  -1.154 ms  control-no-change
  resume_ready      p50  153.178/150.387  p95  163.393/155.892  delta  -2.792 ms  control-no-change
  provider_dispatch p50    1.621/1.606    p95    2.353/2.683    delta  -0.015 ms  control-no-change
  session_save      p50   35.763/72.877   p95  237.795/272.882  delta +37.114 ms  control-drift
  total store_read_bytes p50 2841122  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23651, 23656, 24338, 24367, 25041, 25044, 25753, 25772, 26431, 26463]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23626, 23636, 24349, 24359, 25044, 25045, 25743, 25746, 26441, 26466]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23651..26463 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23626..26466 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   15.273/15.300   p95   15.531/15.575   delta  +0.027 ms  control-no-change
  skill_request_overlay p50   38.387/38.613   p95   40.479/41.143   delta  +0.226 ms  control-no-change
  mcp_config_load   p50    0.974/0.975    p95    1.046/1.053    delta  +0.001 ms  control-no-change
  attach_history    p50  201.367/200.596  p95  205.920/203.608  delta  -0.771 ms  control-no-change
  resume_ready      p50  153.939/153.681  p95  159.249/158.078  delta  -0.258 ms  control-no-change
  provider_dispatch p50    1.583/1.550    p95    1.724/1.670    delta  -0.032 ms  control-no-change
  session_save      p50   37.306/38.895   p95   80.196/65.659   delta  +1.589 ms  control-no-change
  total store_read_bytes p50 2841124  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23659, 23671, 24342, 24346, 25043, 25045, 25736, 25754, 26453, 26471]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23666, 23667, 24337, 24346, 25050, 25057, 25713, 25754, 26421, 26434]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23659..26471 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23666..26434 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.110/0.097    p95    0.146/0.147    delta  -0.013 ms  control-no-change
  skill_request_overlay p50    0.769/0.750    p95    0.858/0.878    delta  -0.019 ms  control-no-change
  mcp_config_load   p50    0.945/0.923    p95    1.005/1.016    delta  -0.022 ms  control-no-change
  attach_history    p50 2095.247/2099.515 p95 2115.283/2134.536 delta  +4.268 ms  control-no-change
  resume_ready      p50 1574.441/1568.713 p95 1596.460/1594.178 delta  -5.728 ms  control-no-change
  provider_dispatch p50   17.124/18.783   p95   18.888/21.260   delta  +1.659 ms  control-no-change
  session_save      p50   34.874/35.903   p95  191.836/147.915  delta  +1.029 ms  control-no-change
  total store_read_bytes p50 28640162  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23618, 23674, 24369, 24377, 25061, 25070, 25725, 25735, 26460, 26468]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23629, 23658, 24358, 24377, 25051, 25068, 25743, 25758, 26445, 26474]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23618..26468 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23629..26474 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.518/14.195   p95   15.997/16.317   delta  -0.323 ms  control-no-change
  skill_request_overlay p50   37.412/36.999   p95   41.733/38.738   delta  -0.413 ms  control-no-change
  mcp_config_load   p50    1.006/0.975    p95    1.182/1.008    delta  -0.031 ms  control-no-change
  attach_history    p50 2097.239/2092.690 p95 2130.809/2125.258 delta  -4.548 ms  control-no-change
  resume_ready      p50 1560.724/1556.405 p95 1581.445/1575.623 delta  -4.319 ms  control-no-change
  provider_dispatch p50   17.107/16.785   p95   18.657/18.447   delta  -0.321 ms  control-no-change
  session_save      p50   34.670/33.104   p95   49.411/50.483   delta  -1.566 ms  control-no-change
  total store_read_bytes p50 28640140  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 23663, 24344, 24358, 25041, 25045, 25752, 25764, 26442, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23622, 23644, 24341, 24385, 25036, 25051, 25719, 25738, 26412, 26420]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23622..26420 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 13.0 ms | 22.8 ms | 10.4 ms | 22.8 ms | 5 |
| write (10KB) | 12.5 ms | 12.6 ms | 10.5 ms | 12.6 ms | 5 |
| edit (simple replace) | 12.5 ms | 17.7 ms | 9.9 ms | 17.7 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.4 ms | 8.0 ms | 5.3 ms | 8.0 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 5.3 ms | 4.3 ms | 5.3 ms | 5 |
| bash (true) | 10.5 ms | 10.5 ms | 9.9 ms | 10.5 ms | 5 |
| bash (echo hello) | 10.5 ms | 10.5 ms | 9.7 ms | 10.5 ms | 5 |
| bash (ls fixture tree) | 12.5 ms | 13.0 ms | 12.0 ms | 13.0 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | — | — | — | — | — | — | 0 | — |
| read (1000 lines) | — | — | — | — | — | — | 0 | — |
| read (10 lines @ offset) | — | — | — | — | — | — | 0 | — |
| read (repo Cargo.toml) | — | — | — | — | — | — | 0 | — |
| write (small) | — | — | — | — | — | — | 0 | — |
| write (10KB) | — | — | — | — | — | — | 0 | — |
| edit (simple replace) | — | — | — | — | — | — | 0 | — |
| ls (scripts dir) | — | — | — | — | — | — | 0 | — |
| agentgrep (grep in tree) | — | — | — | — | — | — | 0 | — |
| agentgrep (find **/*.rs) | — | — | — | — | — | — | 0 | — |
| bash (true) | — | — | — | — | — | — | 0 | — |
| bash (echo hello) | — | — | — | — | — | — | 0 | — |
| bash (ls fixture tree) | — | — | — | — | — | — | 0 | — |
| invalid tool (dispatch only) | — | — | — | — | — | — | 0 | — |

CPU unmeasured: no /proc CPU counters for the daemon on this system

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 152.9 ms | 152.9 ms | 152.9 ms | 153.0 ms | 0 |
| 10 | 504.0 ms | 507.9 ms | 507.9 ms | 508.3 ms | 0 |
| 40 | 1.42 s | 1.73 s | 1.73 s | 1.73 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 607.8 ms | 607.8 ms | 607.8 ms | 1 |
| leaf_touch | src\main.rs | 747.3 ms | 747.3 ms | 747.3 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 23.57 s | 23.57 s | 23.57 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 34.13 s | 34.13 s | 34.13 s | 1 |

## TUI (`horon-tui`)

The interactive client is a second binary and is measured on its own. These numbers come from the `tui` suite and are never compared with the headless tables above: `first_frame_ms` and `direct_first_frame_ms` are two launches of two different programs, and the handoff is the difference between them.

![TUI (`horon-tui`)](charts/tui-01-latency.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 27.7 ms | 26.4 ms–43.3 ms | 5 |
| First frame (direct `horon-tui`) | 27.4 ms | 26.9 ms–28.9 ms | 5 |
| TTY handoff cost | 0.5 ms | -1.9 ms–16.4 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.5 ms | 50 |
| Idle RSS after 5 s | 5.9 MB | 5.8 MB–6.0 MB | 5 |
| RSS after one turn | 6.0 MB | 6.0 MB–6.1 MB | 5 |
| Frame draw time (p50) | 0.8 ms | 0.8 ms–0.8 ms | 5 |
| Frame draw time (p95) | 0.8 ms | 0.8 ms–1.2 ms | 5 |
| Install size (`horon-tui`) | 6.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.1 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.3 µs | 0.3 µs–0.4 µs | 5 |
| First held frame after scrolling | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| First key dispatch through completed draw | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame redraw without input (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 60.8 ms | 45.1 ms–262.5 ms | 5 |
| First frame (direct `horon-tui`) | 54.0 ms | 39.5 ms–77.4 ms | 5 |
| TTY handoff cost | 6.8 ms | -32.3 ms–211.5 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.2 ms–0.8 ms | 50 |
| Idle RSS after 5 s | 9.5 MB | 9.4 MB–9.6 MB | 5 |
| RSS after one turn | 9.7 MB | 9.7 MB–9.8 MB | 5 |
| Frame draw time (p50) | 0.8 ms | 0.8 ms–0.8 ms | 5 |
| Frame draw time (p95) | 1.0 ms | 0.9 ms–2.0 ms | 5 |
| Install size (`horon-tui`) | 5.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.4 ms–0.6 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.2 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.4 µs | 0.4 µs–1.5 µs | 5 |
| First held frame after scrolling | 0.7 ms | 0.6 ms–1.1 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.4 ms–0.6 ms | 5 |
| First key dispatch through completed draw | 0.7 ms | 0.6 ms–1.1 ms | 5 |
| Held frame redraw without input (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |

### Windows

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | — | — | — |
| First frame (direct `horon-tui`) | — | — | — |
| TTY handoff cost | — | — | — |
| Key press to echo (p95) | — | — | — |
| Idle RSS after 5 s | — | — | — |
| RSS after one turn | — | — | — |
| Frame draw time (p50) | 1.5 ms | 1.5 ms–1.5 ms | 5 |
| Frame draw time (p95) | 2.3 ms | 1.5 ms–2.3 ms | 5 |
| Install size (`horon-tui`) | 5.3 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.4 ms–0.5 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.1 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.3 µs | 0.3 µs–0.4 µs | 5 |
| First held frame after scrolling | 0.7 ms | 0.6 ms–0.7 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.4 ms–0.5 ms | 5 |
| First key dispatch through completed draw | 0.7 ms | 0.6 ms–0.7 ms | 5 |
| Held frame redraw without input (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.4 ms–0.7 ms | 5 |

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
