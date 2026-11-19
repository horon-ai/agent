# Performance: measured results and their limits

Language: **English** | [한국어](RESULTS.ko.md)

This document records the measurements the `agent-runtime-performance-2` plan left on baseline B0 (the commit immediately before the plan started). The plan landed 19 performance commits on `dev`, and the same range holds one `fix(cli)` commit unrelated to this plan that has no bearing on the numbers below. The measured surfaces are the session store's hashes and history pages, the SSE parser, replication export, the result cache's threshold decision, and the attach/detach soak. Every measurement was taken on this machine (Apple M1 Max, debug test profile) by alternating before and after on the same machine.

All raw artifacts live under `.omo/evidence/agent-runtime-performance-2/`, a path ignored by `.gitignore:19`. The plan asked for the key numbers to be copied into the PR bodies, but in practice most PR bodies were one line. So anyone who clones this repository today cannot see a single measurement from this work. This page moves that record somewhere tracked; every number is copied verbatim from the evidence files above.

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

A few points are easy to misread from the table alone, so they are set out here.

- The source hash digest is the same as before on all five fixtures (61 KB, 618 KB, 6.2 MB, 62 MB, 200 MiB). The only thing that changed is peak allocation; the bytes read and the time taken are unchanged.
- The history page's `WORK` and `TIME` are two faces of the same request. Fetching one 20-item page from a 100k-event session used to replay the whole session, flatten 99,999 messages and read 62 MB; it now reads only the 20 records the index points at.
- In replication export, a 4× larger page made the old reference implementation grow bytes 16.4×, while the current implementation grows them 4.0×. So the cost is proportional to page length, not log length. Page bytes are byte-identical to B0, and the fixed digests `135cf07d...` (length 240,882) and `4a4855d4...` (length 26,979) reproduce unchanged.
- The cache facade no longer serializes canonical JSON once just to decide whether the cap is exceeded. The 1 MiB boundary is byte-exact (1,048,575 B is cached, 1,048,577 B is skipped). The path that actually caches still serializes once.
- The soak ran 200 cycles, 10 windows, 588.99 s. It measures the increment per window, and the failed rounds, quiescence misses (`quiescence_misses`) and non-returned resources (`non_returning`) are all empty.

### Is one page fetched at the same cost as the log grows?

With an index in place, fetching a 20-item page grew bytes read by only 0.5% when the log grew 100×. Replayed messages are 0 at all three sizes.

| Events | Log size | Bytes the page read | Wall clock |
| --- | --- | --- | --- |
| 1,000 | 514,853 B | 15,375 B | 4.28 ms |
| 10,000 | 5,168,853 B | 15,415 B | 4.68 ms |
| 100,000 | 51,888,853 B | 15,455 B | 4.80 ms |

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

| Scenario | Run 1 | Run 2 | Run 3 | Median | Range |
| --- | --- | --- | --- | --- | --- |
| `direct_waiter` (msg/s) | 105,291 | 107,434 | 111,523 | 107,434 | 105,291 ~ 111,523 |
| `direct_deliver` (msg/s) | 2,410,858 | 2,229,008 | 2,392,230 | 2,392,230 | 2,229,008 ~ 2,410,858 |
| `broadcast64` (msg/s) | 3,237,934 | 3,141,178 | 3,141,375 | 3,141,375 | 3,141,178 ~ 3,237,934 |

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

The candidate column in this table was measured with the `215bfdb` binary. Of the changes that later landed on the branch, only one touched the start path, the deferred HTTP client (`b6dcfc0`); the time and memory re-measurements are in the "Memory" section below. The 11 fixes from the F2 quality review also left these numbers unchanged, and re-measuring with the same runner gives Linux fsync cold 40 · warm 35 again ("fsync" in `F2-review.md`).

The decision to hold fsync at 40 or below uses the values from the `bench_compare.py --syscalls` runner (cold 40 · warm 35). todo 6's own driver used a different HOME and prompt shape and measured cold 45 · warm 40 on the same candidate binary, and in that run only warm meets the 40 bound ("result against the plan" in `6-linux-fsync.txt`).

Absolute values on hosted runners differ per machine and mean nothing, so the GitHub column says only that "the workflow runs on the branch as-is and fsync counts are recorded too". All three session measurements in the GitHub B0 run failed with `daemon socket not ready`. That run uses `dev`'s `bench_compare.py`, and the fix for the removal of `serve --debug-socket` (todo 13) is still only on the branch. So the session values from GitHub B0 are not 0.

### Same-machine A/B: no regression on the time paths

`scripts/perf/ab_perf.py` runs the two binaries alternately in blocks and reports the noise band `max(3*1.4826*MAD, 1% of the median)` alongside. Only differences outside the band are called an improvement or a regression.

One row is one path. one-shot, start→first request and the session paths go through a small probe that does the setup for them (scratch HOME, mock model address, daemon start), so the absolute values here are larger than in the environment table above by the probe's own startup. Both sides use the same probe, so the direction of the comparison is unchanged.

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

Tool latency and startup steps were checked separately on the same machine. Alternating the 14 cases in `bench_tools.py` three times each gives median differences between -15.7% and +5.4%, and the cases that grew are `ls` (+5.4%), `agentgrep` (+1.1%) and `read (repo Cargo.toml)` (+0.9%). For `ls`, B0's three runs are 2.21 / 2.46 / 3.57 ms and the candidate's are 2.58 / 2.59 / 2.64 ms, which falls inside B0's own run-to-run spread. `bench_startup.py` gives a server-ready median of 10.8–10.9 ms (B0) and 11.0 ms (candidate), and a total bridge time of 18.1–18.6 ms and 17.9–18.6 ms, which overlap. The 20–40% spread across tool cases unrelated to the change in the GitHub candidate run is a runner instance difference and does not reproduce on the same machine.

macOS dropped sharply because the two costs this plan targeted existed only on macOS. The path that took `F_FULLFSYNC` on every save, derived caches included, now syncs the source only, and the path that read a keychain certificate per reqwest client became one construction per process. As a control, `--version` load and `perf_counters_surface` are inside the band in both environments. Neither path touches storage or TLS, so they are exactly where a regression in shared code would have shown up first.

### Memory: the first Linux daemon RSS numbers, and what re-measuring showed

The time paths got slower nowhere. Memory was first recorded as an increase in Linux daemon RSS (`rss`), and only later was it established that the measurement method itself cannot compare two builds. The first numbers come first, then the re-measurement and the fix below them.

| Measurement | Environment | B0 | Candidate | Difference |
| --- | --- | --- | --- | --- |
| Daemon idle, 0 sessions | macOS | 27.82 MiB | 24.09 MiB | -3.73 MiB (-13.4%) |
| Daemon idle, 1 session | macOS | 32.34 MiB | 28.95 MiB | -3.39 MiB (-10.5%) |
| Daemon idle, 0 sessions | Linux | 27.88 MiB | 29.01 MiB | +1.13 MiB (+4.1%) |
| Daemon idle, 1 session | Linux | 31.74 MiB | 32.75 MiB | +1.01 MiB (+3.2%) |
| one-shot peak RSS (8 runs) | Linux | 32.46 MiB | 32.87 MiB | +0.41 MiB (+1.3%, ranges overlap) |
| Same-binary control (A/A) | Linux | 27.83 MiB | 27.87 MiB | +0.03 MiB (+0.1%) |

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

| Step | Condition | Baseline | Candidate |
| --- | --- | --- | --- |
| `skill_request_overlay` | 200 skills, 8 KiB body | 28.30 (29.09) | 4.79 (4.96) |
| `skill_request_overlay` | 1000 skills, 8 KiB body | 149.03 (153.92) | 26.46 (27.83) |
| `skill_catalog` | 200 skills, 8 KiB body | 4.41 (4.53) | 3.00 (3.11) |
| `skill_catalog` | 1000 skills, 8 KiB body | 21.88 (22.64) | 15.00 (15.32) |
| `skill_catalog` | 1000 skills, 0 body | 15.71 (15.98) | 14.82 (15.28) |
| `mcp_config_load` | 300 KB MCP | 3.46 (3.71) | 0.22 (0.26) |

The candidate medians for all 14 cases in `bench_tools.py` equal or beat the baseline (5 samples each, same job).

### Fixed during measurement

- In the first run ([run 42458666340](https://github.com/horon-ai/agent/actions/runs/42458666340)) `skill_catalog` in the 0-body condition exceeded the baseline's p95 (1000 skills 9.65 → 13.52 ms). The cause was name-collision handling walking the earlier list from the start for every skill, and building a namespace string and comparison key per skill even with no collision. `abd7618e` and `83f2b6c7` added the name index, compute the namespace only on collision, drop one thread, and parse front matter within the first 4 KiB without a heap buffer; the table above is the result after those fixes.
- The benchmark script's MCP fixture check changed from "was the file read again during the measurement step" to "was the server from that file loaded" (`b3a053b3`). Not re-reading a file the parse cache already holds is the point of this change.

### What these numbers do not say

- The runner differs per run, so the same baseline moved by several ms between runs (0 body, 1000 skills `skill_catalog` baseline 9.65 / 15.62 / 15.71 ms). Comparisons were only made between the two sides within the same run.
- Cold start (an empty page cache) was not measured.
- Windows has no ctime, so a skill file edited to the same size with its modification time restored may not show up on the next request (`docs/runtime/PROMPT.md`).

## Fewer fsyncs and the structural difference from jcode (#47)

Three changes landed. A session that has never made an explicit choice no longer writes `false` to `local-head-explicit.json` (A), and the session writer no longer re-flushes the parent of the already existing `sessions/` root on every open (B). The durable runtime journal, in the other direction, now commits a newly created folder's entry to its parent (C). Before C, `runtime.jsonl` could vanish along with its folder after a power loss even though it had already returned `Ok`. The contract is in [`../protocol/SESSION_STORE.md`](../protocol/SESSION_STORE.md). The measurement is `benchmark.yml` run 42531992327 on 2026-11-09, with baseline `dev` (`c3e3ceeb`) and candidate (`d9d2c836`) alternating within one job on the same runner. one-shot has 5 samples.

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

| Surface | Before | After |
| --- | --- | --- |
| Eager core tool catalog (8 schemas) | 9,714 B | 9,151 B |
| Core one-shot first request `tools` (mock server) | 9,818 B | 9,255 B |
| Static prompt (core, no prompt files) | 1,367 B, 319 tokens | 1,322 B, 305 tokens |
| `# Additional tools` index | 629 B, 5 rows cut mid-sentence | 585 B, 1 row (`todo`) |
| Prompt source probes over 9 unchanged requests | 54 | 0 |
| Session-context `git` processes, outside a repository / in a work tree | 1 / 3 | 0 / 1 |
| GPU probe processes without a PCI display controller | 1 | 0 |

PR #332 set the tool catalog ceiling to 9,151 B. Shortening the shared `intent` description in `1dfdd06a` brought the eight schemas to 8,975 B, the current ceiling. The static prompt measured 1,322 B and nine unchanged requests made zero prompt-source reads. On the same Linux runner, the first request's `tools` field fell from 9,255 B at `83da1c39` to 9,079 B. This puts the catalog 25 B below the 9,000 B target and the first request 739 B below the original 9,818 B. [Structural measurements](https://github.com/horon-ai/agent/actions/runs/44190404248/job/134418534689) · [First requests sent to the mock server](https://github.com/horon-ai/agent/actions/runs/44190400494/artifacts/14378517143)

### Time and memory (median ms, min–max in parentheses)

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

<!-- bench-auto:start commit=e249038d0170c302206c22930c598f852a868c0c -->

## Headless (`horon-agent`)

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `e249038d0170c302206c22930c598f852a868c0c`
- Measured at: 2026-11-19 01:55 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/44430342278> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (e249038)` | Success |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (e249038)` | Success |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (e249038)` | Success |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (e249038) | 3.5 ms | 112.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.5 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 10.2 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 43.4 ms | 435.0 MB |
| OpenCode | 1.19.5 | 490.7 ms | 613.4 MB |
| pi | 1.5.0 | 243.2 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 665.6 ms | 413.8 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 118.3 ms | 520.7 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 41.9 ms | 41.0 ms–56.5 ms | baseline | 39.1 MB | baseline | yes | 10 | 3,709 / 2,368 (1 tools) | — |
| jcode | 146.1 ms | 143.1 ms–210.3 ms | 3.49× more | 41.9 MB | 1.07× more | yes | 5 | 3,245 / 1,433 (1 tools) | — |
| Claude Code | 432.0 ms | 423.5 ms–442.0 ms | 10.31× more | 206.4 MB | 5.28× more | yes | — | 45,782 / 29,653 (12 tools) | — |
| Codex CLI | 458.4 ms | 426.6 ms–746.2 ms | 10.94× more | 202.3 MB | 5.18× more | yes | 6 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 2.73 s | 2.70 s–3.01 s | 65.15× more | 587.7 MB | 15.04× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 408.1 ms | 404.3 ms–416.1 ms | 9.74× more | 113.7 MB | 2.91× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.96 s | 1.78 s–2.06 s | 46.80× more | 395.8 MB | 10.13× more | yes | 68 | 29,860 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 6.06 s | 5.90 s–6.41 s | 144.55× more | 655.9 MB | 16.78× more | yes | 36 | 26,678 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 45.1 ms | 43.9 ms–46.1 ms | baseline | 40.9 MB | baseline | — | yes | — | 11,078 / 9,079 (8 tools) |
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
| horon | 36.3 ms | 35.2 ms–36.6 ms | baseline | 17.7 MB | baseline | skill-uri | yes | — |
| jcode | 56.9 ms | 55.6 ms–57.8 ms | 1.57× more | 40.6 MB | 2.30× more | skill-tool | yes | — |
| Claude Code | 434.4 ms | 423.4 ms–446.7 ms | 11.97× more | 206.9 MB | 11.71× more | skill-tool | yes | — |
| Codex CLI | 547.7 ms | 520.2 ms–578.7 ms | 15.09× more | 198.8 MB | 11.25× more | file-read | yes | — |
| OpenCode | 3.20 s | 3.15 s–3.37 s | 88.29× more | 600.6 MB | 34.00× more | skill-tool | yes | — |
| pi | 531.2 ms | 523.1 ms–539.8 ms | 14.64× more | 125.1 MB | 7.08× more | file-read | yes | — |
| oh-my-pi (omp) | 1.75 s | 1.72 s–1.80 s | 48.23× more | 341.1 MB | 19.31× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.61 s | 6.46 s–6.67 s | 182.05× more | 666.2 MB | 37.71× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 61.5 ms | 61.3 ms–62.2 ms | baseline | 42.7 MB | baseline | mcp-call | yes | 45.6 ms | — |
| jcode | 175.4 ms | 174.8 ms–177.5 ms | 2.85× more | 53.7 MB | 1.26× more | mcp-action | yes | 144.4 ms | — |
| Claude Code | 538.7 ms | 525.3 ms–551.3 ms | 8.76× more | 224.6 MB | 5.26× more | mcp-tool | yes | 396.9 ms | — |
| Codex CLI | 474.1 ms | 456.3 ms–476.1 ms | 7.71× more | 195.7 MB | 4.59× more | mcp-tool | yes | 298.6 ms | — |
| OpenCode | 3.04 s | 2.97 s–3.08 s | 49.35× more | 565.8 MB | 13.26× more | mcp-tool | yes | 2.76 s | — |
| pi | 594.7 ms | 585.1 ms–616.6 ms | 9.67× more | 139.1 MB | 3.26× more | mcp-tool | yes | 524.0 ms | — |
| oh-my-pi (omp) | 1.79 s | 1.77 s–1.82 s | 29.12× more | 363.0 MB | 8.51× more | mcp-route | yes | 1.65 s | — |
| oh-my-openagent (omo) | 6.91 s | 6.71 s–6.96 s | 112.40× more | 677.3 MB | 15.88× more | mcp-tool | yes | 3.37 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 36.9 MB | 146.6 KB | baseline | 0.68 | 1.00 | 1 | 0.00 | 44.4 MB | 456.6 KB |
| jcode | 42.4 MB | 104.0 KB | 1.41× less | 0.23 | — | 1 | 0.00 | 45.9 MB | 289.5 KB |
| Claude Code | 150.6 MB | 146.5 MB | 1,023× more | — | — | 1 | 1.00 | 187.2 MB | 180.1 MB |
| Codex CLI | 157.9 MB | 129.6 MB | 905.11× more | — | — | 5 | 0.57 | 161.8 MB | 163.5 MB |
| OpenCode | 244.6 MB | 468.3 KB | 3.19× more | — | — | 1 | 0.00 | 452.4 MB | 5.3 MB |
| pi | 108.0 MB | 109.3 MB | 763.17× more | — | — | 1 | 1.00 | 117.9 MB | 127.6 MB |
| oh-my-pi (omp) | 290.7 MB | 66.4 MB | 463.65× more | — | — | 1 | 1.00 | 377.4 MB | 358.2 MB |
| oh-my-openagent (omo) | 269.7 MB | 36.6 MB | 255.48× more | — | — | 1 | 1.00 | 604.1 MB | 539.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.4 MB | 47.0 MB | 48.5 MB |
| jcode | 45.9 MB | 47.5 MB | 48.5 MB |
| Claude Code | 187.2 MB | 923.1 MB | 1.8 GB |
| Codex CLI | 161.8 MB | 809.6 MB | 1.6 GB |
| OpenCode | 452.4 MB | 515.6 MB | 502.9 MB |
| pi | 117.9 MB | 643.7 MB | 1.2 GB |
| oh-my-pi (omp) | 377.4 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 604.1 MB | 2.7 GB | 5.3 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 5 / 1 | 5 / 5 | 10 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 1 / 4 | 5 / 20 | 10 / 40 |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    2.49 ms
  Max:    3.19 ms
  Mean:   2.62 ms
  Median: 2.54 ms
  Stdev:  0.21 ms

--version:
  Min:    2.35 ms
  Max:    2.80 ms
  Mean:   2.61 ms
  Median: 2.58 ms
  Stdev:  0.15 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.59 ms
  Max:    20.88 ms
  Mean:   20.70 ms
  Median: 20.67 ms
  Stdev:  0.10 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        0.2       0.2   15.6%
  nofile_limit        0.4       0.3   24.5%
  perm_harden         0.5       0.0    0.7%
  args_parse          0.5       0.0    0.1%
  config_loaded       0.6       0.1   12.2%
  server_new          1.0       0.4   38.9%
  socket_bound        1.1       0.1    7.8%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.67 ms
  Max:    20.82 ms
  Mean:   20.72 ms
  Median: 20.71 ms
  Stdev:  0.04 ms

Bridge stage: hello_ok:
  Min:    21.35 ms
  Max:    21.59 ms
  Mean:   21.46 ms
  Median: 21.45 ms
  Stdev:  0.08 ms

Bridge stage: no-op reply:
  Min:    21.73 ms
  Max:    21.97 ms
  Mean:   21.87 ms
  Median: 21.90 ms
  Stdev:  0.09 ms

Bridge total (socket -> no-op):
  Min:    21.73 ms
  Max:    21.97 ms
  Mean:   21.87 ms
  Median: 21.90 ms
  Stdev:  0.09 ms

============================================================
Summary:
  Binary load median:      ~2.5 ms
  Server ready median:     ~20.7 ms
  Bridge total median:     ~21.9 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (3a3ed1bbefef)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (0ef398c77010, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-u1gd5f05
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.016/0.015    p95    0.019/0.018    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.139/0.127    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.178/0.183    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    9.531/9.535    p95   15.414/11.414   delta  +0.004 ms  control-no-change
  total store_read_bytes p50 23080  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23080 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23076 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.749/2.766    p95    2.881/2.811    delta  +0.016 ms  control-no-change
  skill_request_overlay p50    4.844/4.892    p95    5.099/4.970    delta  +0.047 ms  control-no-change
  mcp_config_load   p50    0.148/0.149    p95    0.178/0.161    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   12.488/12.423   p95   13.319/13.685   delta  -0.065 ms  control-no-change
  total store_read_bytes p50 23074  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23080 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23084 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.015    p95    0.030/0.016    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.135/0.140    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.145/0.168    delta  -0.000 ms  control-no-change
  attach_history    p50  248.263/247.264  p95  253.183/253.290  delta  -0.998 ms  control-no-change
  resume_ready      p50   54.033/54.008   p95   56.992/55.168   delta  -0.025 ms  control-no-change
  provider_dispatch p50    1.441/1.436    p95    1.463/1.855    delta  -0.005 ms  control-no-change
  session_save      p50    8.300/8.175    p95    8.684/12.207   delta  -0.126 ms  control-no-change
  total store_read_bytes p50 5311399  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 248.263 ms (load 204.512 + history 41.807)
    selection          p50     0.033 ms  min     0.032  max     0.033  ratio 0.0% of anchor
    checkpoint_recent  p50   164.839 ms  min   158.502  max   165.011  ratio 66.4% of anchor
    log_read           p50     0.087 ms  min     0.055  max     0.093  ratio 0.0% of anchor
    full_scan          p50    22.869 ms  min    22.548  max    23.862  ratio 9.2% of anchor
    blob_read          p50     0.006 ms  min     0.003  max     0.008  ratio 0.0% of anchor
    graph_replay       p50    17.676 ms  min    15.585  max    18.751  ratio 7.1% of anchor
    metadata_json      p50     0.007 ms  min     0.006  max     0.008  ratio 0.0% of anchor
    messages_json      p50     0.964 ms  min     0.955  max     1.050  ratio 0.4% of anchor
    messages_decode    p50     0.955 ms  min     0.929  max     1.061  ratio 0.4% of anchor
    source_hash        p50     0.465 ms  min     0.450  max     0.487  ratio 0.2% of anchor
    history_rescan     p50    45.181 ms  min    42.459  max    46.785  ratio 18.2% of anchor
    page_index         p50    50.899 ms  min    46.472  max    50.990  ratio 20.5% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23082 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23066..23086 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.775/2.715    p95    2.847/2.804    delta  -0.060 ms  control-no-change
  skill_request_overlay p50    4.878/4.824    p95    4.948/5.087    delta  -0.055 ms  control-no-change
  mcp_config_load   p50    0.149/0.148    p95    0.170/0.157    delta  -0.001 ms  control-no-change
  attach_history    p50  248.978/248.945  p95  255.057/253.611  delta  -0.033 ms  control-no-change
  resume_ready      p50   53.985/53.959   p95   54.883/54.935   delta  -0.025 ms  control-no-change
  provider_dispatch p50    1.427/1.424    p95    1.494/1.463    delta  -0.003 ms  control-no-change
  session_save      p50    8.252/8.042    p95    9.477/8.839    delta  -0.210 ms  control-no-change
  total store_read_bytes p50 5311394  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 248.978 ms (load 207.650 + history 41.167)
    selection          p50     0.035 ms  min     0.031  max     0.037  ratio 0.0% of anchor
    checkpoint_recent  p50   164.376 ms  min   162.987  max   169.719  ratio 66.0% of anchor
    log_read           p50     0.076 ms  min     0.070  max     0.081  ratio 0.0% of anchor
    full_scan          p50    22.991 ms  min    22.797  max    23.338  ratio 9.2% of anchor
    blob_read          p50     0.005 ms  min     0.004  max     0.006  ratio 0.0% of anchor
    graph_replay       p50    17.765 ms  min    16.314  max    18.600  ratio 7.1% of anchor
    metadata_json      p50     0.004 ms  min     0.004  max     0.008  ratio 0.0% of anchor
    messages_json      p50     1.003 ms  min     0.965  max     1.032  ratio 0.4% of anchor
    messages_decode    p50     0.985 ms  min     0.925  max     0.985  ratio 0.4% of anchor
    source_hash        p50     0.467 ms  min     0.446  max     0.489  ratio 0.2% of anchor
    history_rescan     p50    46.646 ms  min    42.396  max    48.153  ratio 18.7% of anchor
    page_index         p50    51.003 ms  min    46.487  max    52.900  ratio 20.5% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23066..23078 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189191 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099133 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23044..23082 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.016    p95    0.017/0.026    delta  +0.001 ms  control-drift
  skill_request_overlay p50    0.108/0.106    p95    0.145/0.144    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.146/0.146    p95    0.150/0.168    delta  -0.000 ms  control-no-change
  attach_history    p50 3144.049/3091.298 p95 3164.870/3197.200 delta -52.751 ms  control-no-change
  resume_ready      p50  593.887/593.726  p95  600.965/601.657  delta  -0.162 ms  control-no-change
  provider_dispatch p50   15.807/15.862   p95   16.180/16.107   delta  +0.054 ms  control-no-change
  session_save      p50    9.067/8.970    p95   12.619/9.748    delta  -0.097 ms  control-no-change
  total store_read_bytes p50 53417608  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 3144.049 ms (load 2636.715 + history 494.099)
    selection          p50     0.043 ms  min     0.043  max     0.047  ratio 0.0% of anchor
    checkpoint_recent  p50  2099.385 ms  min  2083.666  max  2109.971  ratio 66.8% of anchor
    log_read           p50     0.654 ms  min     0.571  max     0.821  ratio 0.0% of anchor
    full_scan          p50   244.841 ms  min   244.453  max   246.049  ratio 7.8% of anchor
    blob_read          p50     0.058 ms  min     0.055  max     0.059  ratio 0.0% of anchor
    graph_replay       p50   222.830 ms  min   219.056  max   231.784  ratio 7.1% of anchor
    metadata_json      p50     0.006 ms  min     0.006  max     0.006  ratio 0.0% of anchor
    messages_json      p50     8.880 ms  min     8.824  max     8.942  ratio 0.3% of anchor
    messages_decode    p50    12.947 ms  min    12.206  max    13.028  ratio 0.4% of anchor
    source_hash        p50     4.434 ms  min     4.404  max     4.440  ratio 0.1% of anchor
    history_rescan     p50   499.681 ms  min   498.807  max   520.433  ratio 15.9% of anchor
    page_index         p50   517.909 ms  min   515.285  max   530.147  ratio 16.5% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23059..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23057..23088 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.701/2.719    p95    2.789/2.819    delta  +0.018 ms  control-no-change
  skill_request_overlay p50    4.668/4.665    p95    4.744/4.704    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.148/0.148    p95    0.155/0.159    delta  -0.000 ms  control-no-change
  attach_history    p50 2989.217/2977.297 p95 3082.346/3090.316 delta -11.920 ms  control-no-change
  resume_ready      p50  576.214/577.572  p95  590.262/588.816  delta  +1.358 ms  control-no-change
  provider_dispatch p50   15.536/15.485   p95   15.903/16.492   delta  -0.051 ms  control-no-change
  session_save      p50    8.368/8.230    p95    8.739/8.992    delta  -0.138 ms  control-no-change
  total store_read_bytes p50 53417607  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2989.217 ms (load 2521.101 + history 464.603)
    selection          p50     0.037 ms  min     0.035  max     0.039  ratio 0.0% of anchor
    checkpoint_recent  p50  2063.039 ms  min  2026.064  max  2064.311  ratio 69.0% of anchor
    log_read           p50     0.529 ms  min     0.473  max     0.567  ratio 0.0% of anchor
    full_scan          p50   243.938 ms  min   243.477  max   243.945  ratio 8.2% of anchor
    blob_read          p50     0.046 ms  min     0.030  max     0.049  ratio 0.0% of anchor
    graph_replay       p50   199.660 ms  min   197.220  max   205.446  ratio 6.7% of anchor
    metadata_json      p50     0.007 ms  min     0.006  max     0.007  ratio 0.0% of anchor
    messages_json      p50     8.861 ms  min     8.726  max     8.864  ratio 0.3% of anchor
    messages_decode    p50    12.357 ms  min    11.707  max    12.498  ratio 0.4% of anchor
    source_hash        p50     4.413 ms  min     4.394  max     4.426  ratio 0.1% of anchor
    history_rescan     p50   486.790 ms  min   483.183  max   488.669  ratio 16.3% of anchor
    page_index         p50   507.505 ms  min   496.842  max   511.243  ratio 17.0% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23069..23089 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23061..23085 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (3a3ed1bbefef)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (0ef398c77010, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-gwmw45ap
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.007/0.007    p95    0.008/0.008    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.051/0.038    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.054/0.053    p95    0.072/0.085    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    4.230/4.185    p95    4.317/4.383    delta  -0.046 ms  control-no-change
  total store_read_bytes p50 23788  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23066, 23077, 23081, 23085, 23785, 23792, 23796, 24507, 24516, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23081, 23792, 23796, 23800, 24515, 24519, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23066..24523 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24523 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    1.304/1.326    p95    1.439/1.369    delta  +0.021 ms  control-no-change
  skill_request_overlay p50    1.845/1.849    p95    1.948/2.001    delta  +0.004 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.076/0.102    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    5.239/5.170    p95    5.350/5.533    delta  -0.069 ms  control-no-change
  total store_read_bytes p50 23789  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23069, 23081, 23085, 23784, 23794, 23800, 24515, 24527]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23077, 23085, 23792, 23800, 24495, 24513, 24519, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23069..24527 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24523 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.006/0.006    p95    0.006/0.006    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.039    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.054/0.054    delta  -0.000 ms  control-no-change
  attach_history    p50   58.883/58.720   p95   59.777/59.254   delta  -0.163 ms  control-no-change
  resume_ready      p50   27.342/27.410   p95   27.622/30.713   delta  +0.068 ms  control-no-change
  provider_dispatch p50    0.989/0.995    p95    1.004/1.033    delta  +0.006 ms  control-no-change
  session_save      p50    4.927/4.726    p95    5.259/6.119    delta  -0.201 ms  control-no-change
  total store_read_bytes p50 2841375  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23797, 24514, 25233, 25237, 25948, 25968, 26671, 26675]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23797, 23801, 24508, 24512, 25231, 25237, 25940, 25962, 26667, 26681]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23797..26675 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23797..26681 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.342/1.320    p95    1.522/1.497    delta  -0.022 ms  control-no-change
  skill_request_overlay p50    1.865/1.862    p95    1.944/2.047    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.059/0.068    delta  -0.000 ms  control-no-change
  attach_history    p50   57.921/58.136   p95   58.353/58.560   delta  +0.215 ms  control-no-change
  resume_ready      p50   27.160/27.176   p95   27.376/27.275   delta  +0.016 ms  control-no-change
  provider_dispatch p50    0.975/0.973    p95    1.014/0.995    delta  -0.002 ms  control-no-change
  session_save      p50    5.207/5.251    p95    5.563/5.330    delta  +0.044 ms  control-no-change
  total store_read_bytes p50 2841379  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23781, 23793, 24516, 24524, 25235, 25243, 25958, 25962, 26665]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23793, 23797, 24500, 24510, 25229, 25241, 25960, 25964, 26667, 26675]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23781..26665 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23793..26675 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.038    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.059/0.060    delta  +0.000 ms  control-no-change
  attach_history    p50  669.065/672.622  p95  679.374/681.146  delta  +3.557 ms  control-no-change
  resume_ready      p50  322.469/324.479  p95  329.262/326.833  delta  +2.010 ms  control-no-change
  provider_dispatch p50   11.404/11.430   p95   11.648/11.572   delta  +0.026 ms  control-no-change
  session_save      p50    4.839/4.962    p95    4.955/5.146    delta  +0.123 ms  control-no-change
  total store_read_bytes p50 28640398  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23783, 23789, 24504, 24522, 25241, 25245, 25948, 26667, 26683]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23787, 23793, 24510, 24522, 25225, 25956, 26675, 26679]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23783..26683 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23787..26679 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.331/1.302    p95    1.463/1.321    delta  -0.029 ms  control-no-change
  skill_request_overlay p50    1.886/1.861    p95    2.031/2.041    delta  -0.025 ms  control-no-change
  mcp_config_load   p50    0.059/0.058    p95    0.067/0.089    delta  -0.000 ms  control-no-change
  attach_history    p50  674.123/675.665  p95  687.788/688.425  delta  +1.542 ms  control-no-change
  resume_ready      p50  331.630/334.303  p95  337.481/337.917  delta  +2.673 ms  control-no-change
  provider_dispatch p50   11.426/11.456   p95   11.618/11.701   delta  +0.029 ms  control-no-change
  session_save      p50    5.438/5.590    p95    5.892/6.102    delta  +0.152 ms  control-no-change
  total store_read_bytes p50 28640388  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23802, 23816, 24517, 24523, 25218, 25244, 25945, 25955, 26666, 26678]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23796, 23806, 24517, 24535, 25230, 25240, 25953, 25955, 26670, 26674]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23802..26678 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23796..26674 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |
| read (1000 lines) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.2 ms | 0.2 ms | 0.2 ms | 0.2 ms | 5 |
| read (repo Cargo.toml) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |
| write (small) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| write (10KB) | 0.7 ms | 0.8 ms | 0.7 ms | 0.8 ms | 5 |
| edit (simple replace) | 0.6 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| ls (scripts dir) | 0.5 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| agentgrep (grep in tree) | 1.2 ms | 1.3 ms | 1.1 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 0.9 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.3 ms | 1.6 ms | 1.2 ms | 1.6 ms | 5 |
| bash (echo hello) | 1.3 ms | 1.4 ms | 1.2 ms | 1.4 ms | 5 |
| bash (ls fixture tree) | 2.2 ms | 2.3 ms | 2.2 ms | 2.3 ms | 5 |
| invalid tool (dispatch only) | 0.0 ms | 0.1 ms | 0.0 ms | 0.1 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | 0.120 ms | 0.113 ms | 0.123 ms | 0.000 ms | 5.22 | 0.00 | 5 × 50 | schedstat |
| read (1000 lines) | 0.209 ms | 0.199 ms | 0.213 ms | 0.000 ms | 5.34 | 0.00 | 5 × 50 | schedstat |
| read (10 lines @ offset) | 0.205 ms | 0.200 ms | 0.206 ms | 0.000 ms | 5.28 | 0.00 | 5 × 50 | schedstat |
| read (repo Cargo.toml) | 0.151 ms | 0.146 ms | 0.163 ms | 0.000 ms | 5.24 | 0.00 | 5 × 50 | schedstat |
| write (small) | 0.424 ms | 0.411 ms | 0.431 ms | 0.000 ms | 16.90 | 0.32 | 5 × 50 | schedstat |
| write (10KB) | 0.560 ms | 0.551 ms | 0.568 ms | 0.000 ms | 17.62 | 0.04 | 5 × 50 | schedstat |
| edit (simple replace) | 0.427 ms | 0.399 ms | 0.467 ms | 0.000 ms | 17.24 | 0.28 | 5 × 50 | schedstat |
| ls (scripts dir) | 0.436 ms | 0.425 ms | 0.460 ms | 0.000 ms | 5.24 | 0.00 | 5 × 50 | schedstat |
| agentgrep (grep in tree) | 0.832 ms | 0.824 ms | 0.846 ms | 0.000 ms | 10.08 | 0.10 | 5 × 50 | schedstat |
| agentgrep (find **/*.rs) | 0.839 ms | 0.821 ms | 0.864 ms | 0.000 ms | 5.24 | 0.00 | 5 × 50 | schedstat |
| bash (true) | 0.406 ms | 0.388 ms | 0.422 ms | 1.000 ms | 7.76 | 0.00 | 5 × 50 | schedstat |
| bash (echo hello) | 0.431 ms | 0.421 ms | 0.438 ms | 1.000 ms | 8.12 | 0.00 | 5 × 50 | schedstat |
| bash (ls fixture tree) | 0.510 ms | 0.503 ms | 0.513 ms | 1.800 ms | 8.78 | 0.00 | 5 × 50 | schedstat |
| invalid tool (dispatch only) | 0.023 ms | 0.022 ms | 0.024 ms | 0.000 ms | 2.96 | 0.00 | 5 × 50 | schedstat |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 57.1 ms | 57.1 ms | 57.1 ms | 57.7 ms | 0 |
| 10 | 155.8 ms | 190.4 ms | 190.4 ms | 191.3 ms | 0 |
| 40 | 656.3 ms | 666.4 ms | 667.1 ms | 667.8 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 209.2 ms | 209.2 ms | 209.2 ms | 1 |
| leaf_touch | src/main.rs | 254.5 ms | 254.5 ms | 254.5 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 15.78 s | 15.78 s | 15.78 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 22.40 s | 22.40 s | 22.40 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (e249038) | 8.6 ms | 94.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 11.0 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 24.2 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 98.3 ms | 346.5 MB |
| OpenCode | 1.19.5 | 466.5 ms | 344.6 MB |
| pi | 1.5.0 | 387.4 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 998.5 ms | 274.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 203.4 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 85.9 ms | 77.7 ms–508.4 ms | baseline | 29.7 MB | baseline | yes | 3,597 / 2,368 (1 tools) | — |
| jcode | 591.9 ms | 406.6 ms–672.6 ms | 6.89× more | 34.0 MB | 1.14× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 560.1 ms | 441.6 ms–752.6 ms | 6.52× more | 198.9 MB | 6.69× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 816.5 ms | 446.8 ms–1.54 s | 9.51× more | 132.9 MB | 4.47× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 3.86 s | 3.73 s–5.54 s | 44.98× more | 784.3 MB | 26.38× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 689.2 ms | 643.7 ms–792.8 ms | 8.03× more | 126.2 MB | 4.24× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 3.17 s | 2.50 s–3.46 s | 36.93× more | 458.4 MB | 15.42× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 12.30 s | 11.57 s–12.52 s | 143.25× more | 874.9 MB | 29.42× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 101.9 ms | 69.8 ms–120.7 ms | baseline | 30.9 MB | baseline | — | yes | — | 10,966 / 9,079 (8 tools) |
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
| horon | 69.2 ms | 59.1 ms–134.5 ms | baseline | 24.1 MB | baseline | skill-uri | yes | — |
| jcode | 296.3 ms | 274.8 ms–325.5 ms | 4.28× more | 38.4 MB | 1.59× more | skill-tool | yes | — |
| Claude Code | 600.3 ms | 531.7 ms–690.6 ms | 8.67× more | 198.6 MB | 8.25× more | skill-tool | yes | — |
| Codex CLI | 561.8 ms | 507.9 ms–793.9 ms | 8.12× more | 130.5 MB | 5.42× more | file-read | yes | — |
| OpenCode | 5.30 s | 4.93 s–6.19 s | 76.55× more | 782.5 MB | 32.50× more | skill-tool | yes | — |
| pi | 789.5 ms | 734.8 ms–1.17 s | 11.41× more | 134.8 MB | 5.60× more | file-read | yes | — |
| oh-my-pi (omp) | 3.00 s | 2.27 s–3.74 s | 43.40× more | 459.9 MB | 19.10× more | file-read | yes | — |
| oh-my-openagent (omo) | 9.80 s | 9.72 s–13.91 s | 141.57× more | 897.1 MB | 37.26× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 106.6 ms | 101.2 ms–123.9 ms | baseline | 38.3 MB | baseline | mcp-call | yes | 81.2 ms | — |
| jcode | 404.8 ms | 396.2 ms–427.8 ms | 3.80× more | 48.1 MB | 1.26× more | mcp-action | yes | 373.9 ms | — |
| Claude Code | 645.1 ms | 566.6 ms–956.8 ms | 6.05× more | 224.7 MB | 5.87× more | mcp-tool | yes | 520.1 ms | — |
| Codex CLI | 514.6 ms | 460.6 ms–620.4 ms | 4.83× more | 137.6 MB | 3.59× more | mcp-tool | yes | 316.3 ms | — |
| OpenCode | 5.23 s | 4.85 s–5.45 s | 49.03× more | 701.0 MB | 18.31× more | mcp-tool | yes | 4.53 s | — |
| pi | 1.05 s | 869.6 ms–1.12 s | 9.85× more | 146.7 MB | 3.83× more | mcp-tool | yes | 892.8 ms | — |
| oh-my-pi (omp) | 2.68 s | 2.50 s–3.13 s | 25.16× more | 433.5 MB | 11.32× more | mcp-route | yes | 2.28 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 28.8 MB | 128.5 KB | baseline | 0.43 | 1.00 | 1 | 0.00 | 35.1 MB | 369.7 KB |
| jcode | 36.4 MB | 207.2 KB | 1.61× more | — | — | 1 | 0.00 | 40.0 MB | 365.5 KB |
| Claude Code | 157.9 MB | 145.5 MB | 1,160× more | — | — | 1 | 1.00 | 190.5 MB | 182.2 MB |
| Codex CLI | 114.3 MB | 78.4 MB | 624.76× more | — | — | 2 | 0.89 | 106.9 MB | 107.8 MB |
| OpenCode | 258.4 MB | 978.6 KB | 7.61× more | — | — | 1 | 0.00 | 564.5 MB | 24.9 MB |
| pi | 114.5 MB | 115.8 MB | 922.66× more | — | — | 1 | 1.00 | 147.8 MB | 147.9 MB |
| oh-my-pi (omp) | 347.5 MB | 166.2 MB | 1,324× more | — | — | 1 | 1.00 | 438.9 MB | 327.0 MB |
| oh-my-openagent (omo) | 57.1 MB | 43.1 MB | 343.73× more | — | — | 1 | 1.00 | 845.5 MB | 326.5 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.1 MB | 36.3 MB | 38.3 MB |
| jcode | 40.0 MB | 41.5 MB | 43.2 MB |
| Claude Code | 190.5 MB | 931.7 MB | 1.8 GB |
| Codex CLI | 106.9 MB | 536.4 MB | 1.1 GB |
| OpenCode | 564.5 MB | 581.9 MB | 782.3 MB |
| pi | 147.8 MB | 738.1 MB | 1.4 GB |
| oh-my-pi (omp) | 438.9 MB | 2.0 GB | 3.3 GB |
| oh-my-openagent (omo) | 845.5 MB | 2.9 GB | 3.8 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 2 / 1 | 5 / 5 | 10 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 1 / 4 | 5 / 20 | 10 / 40 |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    9.54 ms
  Max:    16.84 ms
  Mean:   12.59 ms
  Median: 12.38 ms
  Stdev:  2.85 ms

--version:
  Min:    9.09 ms
  Max:    17.60 ms
  Mean:   11.92 ms
  Median: 11.64 ms
  Stdev:  2.46 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    28.37 ms
  Max:    103.10 ms
  Mean:   78.17 ms
  Median: 96.41 ms
  Stdev:  31.29 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.0%
  logging_init        0.9       0.9   10.1%
  nofile_limit        1.2       0.3    2.9%
  perm_harden         1.2       0.1    0.7%
  args_parse          1.2       0.0    0.0%
  config_loaded       8.4       7.2   79.8%
  server_new          8.7       0.3    3.1%
  socket_bound        9.0       0.3    3.3%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    55.53 ms
  Max:    123.67 ms
  Mean:   84.89 ms
  Median: 85.59 ms
  Stdev:  24.87 ms

Bridge stage: hello_ok:
  Min:    56.95 ms
  Max:    125.85 ms
  Mean:   86.45 ms
  Median: 87.89 ms
  Stdev:  25.16 ms

Bridge stage: no-op reply:
  Min:    57.89 ms
  Max:    126.98 ms
  Mean:   87.40 ms
  Median: 89.33 ms
  Stdev:  25.25 ms

Bridge total (socket -> no-op):
  Min:    57.89 ms
  Max:    126.98 ms
  Mean:   87.40 ms
  Median: 89.33 ms
  Stdev:  25.25 ms

============================================================
Summary:
  Binary load median:      ~12.4 ms
  Server ready median:     ~96.4 ms
  Bridge total median:     ~89.3 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f9cbe8ee6468)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (02a293eb6b3a, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-prwm5zsm
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.023/0.021    p95    0.033/0.029    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.217/0.207    p95    0.237/0.244    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.253/0.239    p95    0.308/0.285    delta  -0.014 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.006    delta  +0.000 ms  control-no-change
  session_save      p50   11.894/11.400   p95   27.078/12.021   delta  -0.494 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22958 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22926..22958 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    3.836/3.659    p95    9.817/4.245    delta  -0.178 ms  control-no-change
  skill_request_overlay p50    4.640/4.568    p95    7.891/5.726    delta  -0.071 ms  control-no-change
  mcp_config_load   p50    0.246/0.231    p95    0.340/0.280    delta  -0.016 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   13.268/13.730   p95   32.700/30.599   delta  +0.461 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22958 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22906..22954 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.024/0.024    p95    0.035/0.040    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.229/0.240    p95    0.307/0.369    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.270/0.295    p95    0.331/0.469    delta  +0.026 ms  control-no-change
  attach_history    p50  246.991/256.570  p95  560.073/326.425  delta  +9.579 ms  control-no-change
  resume_ready      p50   61.192/64.241   p95   81.137/76.160   delta  +3.049 ms  control-no-change
  provider_dispatch p50    1.113/1.198    p95    1.502/1.401    delta  +0.084 ms  control-no-change
  session_save      p50   12.973/17.056   p95   74.764/99.364   delta  +4.084 ms  control-no-change
  total store_read_bytes p50 5311304  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 246.991 ms (load 209.983 + history 36.176)
    selection          p50     0.057 ms  min     0.046  max     0.080  ratio 0.0% of anchor
    checkpoint_recent  p50   187.116 ms  min   172.291  max   197.368  ratio 75.8% of anchor
    log_read           p50     0.072 ms  min     0.054  max     0.089  ratio 0.0% of anchor
    full_scan          p50    24.513 ms  min    23.670  max    25.036  ratio 9.9% of anchor
    blob_read          p50     0.005 ms  min     0.005  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    15.898 ms  min    15.644  max    19.221  ratio 6.4% of anchor
    metadata_json      p50     0.004 ms  min     0.004  max     0.004  ratio 0.0% of anchor
    messages_json      p50     0.851 ms  min     0.816  max     1.522  ratio 0.3% of anchor
    messages_decode    p50     0.598 ms  min     0.579  max     0.651  ratio 0.2% of anchor
    source_hash        p50     2.414 ms  min     2.376  max     2.444  ratio 1.0% of anchor
    history_rescan     p50    43.156 ms  min    41.740  max    45.331  ratio 17.5% of anchor
    page_index         p50    50.583 ms  min    45.265  max    73.273  ratio 20.5% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22946..22968 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22968 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.279/4.691    p95    4.679/5.220    delta  +0.412 ms  control-no-change
  skill_request_overlay p50    5.412/5.593    p95    5.843/6.510    delta  +0.180 ms  control-no-change
  mcp_config_load   p50    0.290/0.312    p95    0.320/0.368    delta  +0.021 ms  control-no-change
  attach_history    p50  266.706/269.273  p95  279.112/295.513  delta  +2.567 ms  control-no-change
  resume_ready      p50   64.202/66.901   p95   70.253/68.244   delta  +2.700 ms  control-no-change
  provider_dispatch p50    1.225/1.279    p95    1.393/1.776    delta  +0.053 ms  control-no-change
  session_save      p50   14.954/23.156   p95   37.633/42.330   delta  +8.202 ms  control-drift
  total store_read_bytes p50 5311306  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 266.706 ms (load 230.007 + history 38.965)
    selection          p50     0.040 ms  min     0.039  max     0.049  ratio 0.0% of anchor
    checkpoint_recent  p50   168.358 ms  min   168.184  max   192.250  ratio 63.1% of anchor
    log_read           p50     0.074 ms  min     0.061  max     0.077  ratio 0.0% of anchor
    full_scan          p50    22.125 ms  min    22.026  max    22.509  ratio 8.3% of anchor
    blob_read          p50     0.004 ms  min     0.003  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    14.796 ms  min    14.323  max    15.716  ratio 5.5% of anchor
    metadata_json      p50     0.004 ms  min     0.004  max     0.007  ratio 0.0% of anchor
    messages_json      p50     0.792 ms  min     0.783  max     0.819  ratio 0.3% of anchor
    messages_decode    p50     0.589 ms  min     0.558  max     0.674  ratio 0.2% of anchor
    source_hash        p50     2.264 ms  min     2.221  max     2.733  ratio 0.8% of anchor
    history_rescan     p50    40.946 ms  min    36.622  max    41.281  ratio 15.4% of anchor
    page_index         p50    44.516 ms  min    44.293  max    48.374  ratio 16.7% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099143..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22936..22960 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22946..22964 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.025/0.026    p95    0.078/0.035    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.236/0.226    p95    0.297/0.542    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.264/0.274    p95    0.302/0.463    delta  +0.010 ms  control-no-change
  attach_history    p50 2780.621/2714.804 p95 3136.797/2969.156 delta -65.817 ms  control-no-change
  resume_ready      p50  735.569/681.876  p95  800.318/965.665  delta -53.693 ms  control-no-change
  provider_dispatch p50   12.464/12.461   p95   15.653/14.797   delta  -0.003 ms  control-no-change
  session_save      p50   16.702/17.051   p95   50.364/73.930   delta  +0.348 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2780.621 ms (load 2353.483 + history 417.989)
    selection          p50     0.068 ms  min     0.057  max     0.604  ratio 0.0% of anchor
    checkpoint_recent  p50  1837.568 ms  min  1762.211  max  2057.928  ratio 66.1% of anchor
    log_read           p50     1.224 ms  min     1.120  max     1.402  ratio 0.0% of anchor
    full_scan          p50   220.996 ms  min   201.972  max   231.274  ratio 7.9% of anchor
    blob_read          p50     0.070 ms  min     0.068  max     0.071  ratio 0.0% of anchor
    graph_replay       p50   172.848 ms  min   141.870  max   210.479  ratio 6.2% of anchor
    metadata_json      p50     0.005 ms  min     0.005  max     0.006  ratio 0.0% of anchor
    messages_json      p50     7.682 ms  min     6.531  max    10.948  ratio 0.3% of anchor
    messages_decode    p50     8.393 ms  min     8.301  max    15.072  ratio 0.3% of anchor
    source_hash        p50    19.451 ms  min    18.392  max    21.469  ratio 0.7% of anchor
    history_rescan     p50   370.865 ms  min   360.742  max   413.858  ratio 13.3% of anchor
    page_index         p50   368.554 ms  min   358.233  max   370.132  ratio 13.3% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22935..22971 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22971 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.168/4.114    p95    4.899/4.595    delta  -0.054 ms  control-no-change
  skill_request_overlay p50    5.333/5.278    p95    5.781/5.872    delta  -0.055 ms  control-no-change
  mcp_config_load   p50    0.277/0.280    p95    0.299/0.341    delta  +0.003 ms  control-no-change
  attach_history    p50 2771.764/2755.535 p95 3069.861/2900.750 delta -16.229 ms  control-no-change
  resume_ready      p50  685.341/665.497  p95  724.815/735.011  delta -19.845 ms  control-no-change
  provider_dispatch p50   12.181/12.476   p95   13.571/13.628   delta  +0.295 ms  control-no-change
  session_save      p50   19.266/19.457   p95   29.966/21.759   delta  +0.190 ms  control-no-change
  total store_read_bytes p50 53417515  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2771.764 ms (load 2353.429 + history 398.962)
    selection          p50     0.063 ms  min     0.058  max     0.318  ratio 0.0% of anchor
    checkpoint_recent  p50  1920.201 ms  min  1866.570  max  1925.210  ratio 69.3% of anchor
    log_read           p50     1.328 ms  min     1.187  max     1.367  ratio 0.0% of anchor
    full_scan          p50   249.766 ms  min   228.557  max   255.186  ratio 9.0% of anchor
    blob_read          p50     0.074 ms  min     0.070  max     0.078  ratio 0.0% of anchor
    graph_replay       p50   192.387 ms  min   180.757  max   192.767  ratio 6.9% of anchor
    metadata_json      p50     0.008 ms  min     0.006  max     0.009  ratio 0.0% of anchor
    messages_json      p50     8.093 ms  min     7.717  max     9.253  ratio 0.3% of anchor
    messages_decode    p50    10.779 ms  min    10.571  max    11.744  ratio 0.4% of anchor
    source_hash        p50    23.219 ms  min    20.767  max    24.004  ratio 0.8% of anchor
    history_rescan     p50   426.977 ms  min   415.804  max   447.507  ratio 15.4% of anchor
    page_index         p50   417.171 ms  min   408.766  max   460.844  ratio 15.1% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210253..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22963 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22971 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f9cbe8ee6468)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (02a293eb6b3a, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-74z7cbmf
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.063    p95    0.044/0.112    delta  +0.035 ms  control-drift
  skill_request_overlay p50    0.262/0.412    p95    0.348/0.719    delta  +0.150 ms  control-drift
  mcp_config_load   p50    0.308/0.495    p95    1.209/0.765    delta  +0.187 ms  control-drift
  attach_history    p50    0.000/0.001    p95    0.002/0.008    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   30.934/30.857   p95   47.037/65.916   delta  -0.078 ms  control-no-change
  total store_read_bytes p50 23660  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22943, 22951, 23659, 23661, 23665, 24371, 24373]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22943, 22945, 22947, 23655, 23661, 23677, 24355, 24385, 24391]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24373 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24391 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    8.869/5.791    p95   11.857/13.118   delta  -3.077 ms  control-no-change
  skill_request_overlay p50   10.229/7.749    p95   12.665/15.919   delta  -2.480 ms  control-no-change
  mcp_config_load   p50    0.299/0.283    p95    0.773/1.196    delta  -0.016 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   24.896/25.647   p95   40.242/45.636   delta  +0.751 ms  control-no-change
  total store_read_bytes p50 23652  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22935, 22949, 22951, 22959, 23647, 23657, 23679, 24373, 24383, 24387]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22945, 22951, 23651, 23657, 23665, 24361, 24379, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22935..24387 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24383 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.035/0.047    p95    0.069/0.090    delta  +0.012 ms  control-no-change
  skill_request_overlay p50    0.232/0.309    p95    1.001/0.615    delta  +0.077 ms  control-drift
  mcp_config_load   p50    0.269/0.355    p95    0.540/0.874    delta  +0.086 ms  control-no-change
  attach_history    p50  147.051/143.851  p95  176.024/222.151  delta  -3.199 ms  control-no-change
  resume_ready      p50   86.480/93.645   p95  102.742/103.216  delta  +7.164 ms  control-no-change
  provider_dispatch p50    1.193/1.190    p95    2.794/2.881    delta  -0.003 ms  control-no-change
  session_save      p50   24.201/33.569   p95   34.532/62.012   delta  +9.368 ms  control-no-change
  total store_read_bytes p50 2841244  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23666, 24380, 24392, 25090, 25098, 25804, 25816, 26518, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23672, 23676, 24382, 24384, 25098, 25808, 25816, 26512, 26516]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23666..26530 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23672..26516 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.364/5.769    p95    8.288/9.119    delta  +1.405 ms  control-no-change
  skill_request_overlay p50    7.811/6.598    p95   12.279/13.590   delta  -1.213 ms  control-no-change
  mcp_config_load   p50    0.397/0.289    p95    0.971/0.720    delta  -0.108 ms  control-no-change
  attach_history    p50  131.333/119.635  p95  159.318/131.489  delta -11.698 ms  control-no-change
  resume_ready      p50   74.929/75.769   p95   88.348/92.572   delta  +0.840 ms  control-no-change
  provider_dispatch p50    1.120/1.120    p95    1.690/1.800    delta  +0.000 ms  control-no-change
  session_save      p50   24.230/24.613   p95   28.384/55.211   delta  +0.383 ms  control-no-change
  total store_read_bytes p50 2841261  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23668, 23678, 24374, 24388, 25094, 25112, 25820, 25822, 26520, 26532]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23656, 23670, 24376, 24378, 25083, 25104, 25808, 25811, 26501, 26522]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23668..26532 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23656..26522 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.031/0.026    p95    0.071/0.075    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.256/0.242    p95    0.581/0.658    delta  -0.014 ms  control-no-change
  mcp_config_load   p50    0.273/0.279    p95    0.515/0.670    delta  +0.006 ms  control-no-change
  attach_history    p50 1577.880/1519.299 p95 1706.761/1734.149 delta -58.581 ms  control-no-change
  resume_ready      p50  917.271/961.900  p95 1068.966/1106.812 delta +44.630 ms  control-no-change
  provider_dispatch p50   15.652/15.432   p95   38.967/21.310   delta  -0.220 ms  control-no-change
  session_save      p50   25.285/26.223   p95   32.795/86.996   delta  +0.938 ms  control-no-change
  total store_read_bytes p50 28640266  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23677, 23679, 24379, 24385, 25079, 25103, 25805, 25817, 26529, 26531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23663, 23665, 24377, 24395, 25087, 25101, 25813, 25819, 26527, 26535]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23677..26531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23663..26535 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.266/6.256    p95    6.104/10.255   delta  +1.990 ms  control-drift
  skill_request_overlay p50    6.687/8.027    p95   12.288/55.329   delta  +1.340 ms  control-no-change
  mcp_config_load   p50    0.289/0.299    p95    0.640/0.574    delta  +0.010 ms  control-no-change
  attach_history    p50 1503.410/1417.444 p95 1793.951/1696.413 delta -85.967 ms  control-no-change
  resume_ready      p50  909.297/929.255  p95 1482.742/1178.097 delta +19.958 ms  control-no-change
  provider_dispatch p50   13.389/14.887   p95   17.775/22.266   delta  +1.498 ms  control-no-change
  session_save      p50   30.508/31.748   p95  149.449/141.035  delta  +1.240 ms  control-no-change
  total store_read_bytes p50 28640276  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23661, 23675, 24383, 24389, 25097, 25105, 25811, 25823, 26527, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23664, 24382, 24384, 25088, 25104, 25798, 25806, 26520, 26524]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23661..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26524 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.5 ms | 0.6 ms | 0.2 ms | 0.6 ms | 5 |
| read (1000 lines) | 1.1 ms | 3.3 ms | 0.7 ms | 3.3 ms | 5 |
| read (10 lines @ offset) | 0.7 ms | 1.2 ms | 0.5 ms | 1.2 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| write (small) | 2.4 ms | 3.6 ms | 2.2 ms | 3.6 ms | 5 |
| write (10KB) | 2.8 ms | 3.4 ms | 2.6 ms | 3.4 ms | 5 |
| edit (simple replace) | 2.6 ms | 3.0 ms | 2.2 ms | 3.0 ms | 5 |
| ls (scripts dir) | 1.1 ms | 1.7 ms | 1.0 ms | 1.7 ms | 5 |
| agentgrep (grep in tree) | 5.5 ms | 9.2 ms | 4.6 ms | 9.2 ms | 5 |
| agentgrep (find **/*.rs) | 2.3 ms | 3.4 ms | 1.9 ms | 3.4 ms | 5 |
| bash (true) | 8.5 ms | 11.9 ms | 7.3 ms | 11.9 ms | 5 |
| bash (echo hello) | 11.2 ms | 11.8 ms | 7.7 ms | 11.8 ms | 5 |
| bash (ls fixture tree) | 12.9 ms | 14.9 ms | 8.6 ms | 14.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

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
| 1 | 84.8 ms | 84.8 ms | 84.8 ms | 85.0 ms | 0 |
| 10 | 181.9 ms | 189.8 ms | 189.8 ms | 190.0 ms | 0 |
| 40 | 750.1 ms | 801.4 ms | 802.1 ms | 803.1 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 488.5 ms | 488.5 ms | 488.5 ms | 1 |
| leaf_touch | src/main.rs | 442.6 ms | 442.6 ms | 442.6 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 18.43 s | 18.43 s | 18.43 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 24.24 s | 24.24 s | 24.24 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (e249038) | 12.7 ms | 92.8 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 18.6 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 47.7 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 103.0 ms | 479.4 MB |
| OpenCode | 1.19.5 | 578.4 ms | 605.3 MB |
| pi | 1.5.0 | 321.0 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 776.4 ms | 292.4 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 215.4 ms | 514.9 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 175.9 ms | 165.0 ms–258.3 ms | baseline | 32.8 MB | baseline | yes | 3,664 / 2,368 (1 tools) | — |
| jcode | 157.8 ms | 149.1 ms–218.7 ms | 1.11× less | 32.3 MB | same | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.53 s | 1.40 s–3.26 s | 8.71× more | 272.1 MB | 8.30× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 1.77 s | 1.54 s–2.09 s | 10.07× more | 266.7 MB | 8.13× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 5.36 s | 5.30 s–9.09 s | 30.45× more | 787.1 MB | 24.00× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 576.0 ms | 561.3 ms–629.5 ms | 3.28× more | 120.5 MB | 3.67× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 3.22 s | 3.10 s–3.41 s | 18.33× more | 413.8 MB | 12.62× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 21.72 s | 18.37 s–22.48 s | 123.52× more | 551.1 MB | 16.80× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 187.3 ms | 175.4 ms–216.8 ms | baseline | 33.8 MB | baseline | — | yes | — | 11,033 / 9,079 (8 tools) |
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
| horon | 172.1 ms | 164.7 ms–190.1 ms | baseline | 24.6 MB | baseline | skill-uri | yes | — |
| jcode | 190.2 ms | 182.3 ms–202.3 ms | 1.11× more | 24.1 MB | 1.02× less | skill-tool | yes | — |
| Claude Code | 977.0 ms | 967.9 ms–994.3 ms | 5.68× more | 236.9 MB | 9.64× more | skill-tool | yes | — |
| Codex CLI | 2.18 s | 1.96 s–8.45 s | 12.69× more | 263.9 MB | 10.74× more | file-read | yes | — |
| OpenCode | 8.23 s | 8.11 s–8.37 s | 47.81× more | 3.1 GB | 127.33× more | skill-tool | yes | — |
| pi | 721.9 ms | 715.3 ms–749.2 ms | 4.20× more | 98.0 MB | 3.99× more | file-read | yes | — |
| oh-my-pi (omp) | 7.75 s | 4.36 s–11.59 s | 45.07× more | 408.7 MB | 16.64× more | file-read | yes | — |
| oh-my-openagent (omo) | 22.46 s | 20.77 s–23.07 s | 130.52× more | 534.7 MB | 21.77× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 211.3 ms | 199.1 ms–246.9 ms | baseline | 37.2 MB | baseline | mcp-call | yes | 134.6 ms | — |
| jcode | 322.9 ms | 305.9 ms–1.01 s | 1.53× more | 36.7 MB | same | mcp-action | yes | 238.6 ms | — |
| Claude Code | 1.02 s | 1.02 s–1.03 s | 4.85× more | 280.8 MB | 7.54× more | mcp-tool | yes | 890.1 ms | — |
| Codex CLI | 2.18 s | 2.06 s–2.98 s | 10.30× more | 180.2 MB | 4.84× more | mcp-tool | yes | 677.2 ms | — |
| OpenCode | 8.21 s | 8.01 s–8.57 s | 38.85× more | 2.8 GB | 77.05× more | mcp-tool | yes | 6.47 s | — |
| pi | 813.3 ms | 807.4 ms–838.0 ms | 3.85× more | 117.7 MB | 3.16× more | mcp-tool | yes | 681.9 ms | — |
| oh-my-pi (omp) | 3.91 s | 3.85 s–4.16 s | 18.52× more | 442.0 MB | 11.87× more | mcp-route | yes | 2.89 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 22.2 MB | 93.8 KB | baseline | — | — | 1 | 0.00 | 29.6 MB | 348.3 KB |
| jcode | 22.0 MB | 76.1 KB | 1.23× less | — | — | 1 | 0.00 | 26.8 MB | 317.2 KB |
| Claude Code | 154.5 MB | 152.4 MB | 1,665× more | — | — | 2 | 3.61 | 187.3 MB | 190.8 MB |
| Codex CLI | 125.8 MB | 97.2 MB | 1,061× more | — | — | 6 | 3.00 | 165.9 MB | 116.2 MB |
| OpenCode | 245.1 MB | 956.6 KB | 10.20× more | — | — | 2 | 0.00 | 435.7 MB | 4.4 MB |
| pi | 91.2 MB | 90.6 MB | 989.31× more | — | — | 2 | 2.00 | 101.2 MB | 100.9 MB |
| oh-my-pi (omp) | 302.1 MB | 88.2 MB | 963.55× more | — | — | 2 | 2.00 | 349.5 MB | 338.8 MB |
| oh-my-openagent (omo) | 145.9 MB | 108.7 MB | 1,187× more | — | — | 4 | 3.89 | 412.6 MB | 404.0 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.6 MB | 32.2 MB | 32.7 MB |
| jcode | 26.8 MB | 28.6 MB | 29.6 MB |
| Claude Code | 187.3 MB | 958.3 MB | 1.9 GB |
| Codex CLI | 165.9 MB | 656.8 MB | 1.2 GB |
| OpenCode | 435.7 MB | 641.3 MB | 488.5 MB |
| pi | 101.2 MB | 505.0 MB | 1009.3 MB |
| oh-my-pi (omp) | 349.5 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 412.6 MB | 2.2 GB | 4.0 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 2 / 2 | 10 / 10 | 34 / 20 |
| Codex CLI | 6 / 6 | 18 / 18 | 33 / 30 |
| OpenCode | 2 / 2 | 2 / 2 | 2 / 2 |
| pi | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-pi (omp) | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-openagent (omo) | 4 / 6 | 20 / 58 | 39 / 76 |

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    13.86 ms
  Max:    15.66 ms
  Mean:   14.52 ms
  Median: 14.55 ms
  Stdev:  0.52 ms

--version:
  Min:    13.52 ms
  Max:    14.76 ms
  Mean:   14.29 ms
  Median: 14.42 ms
  Stdev:  0.38 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.44 ms
  Max:    28.00 ms
  Mean:   23.40 ms
  Median: 22.76 ms
  Stdev:  1.68 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.5       1.5   17.8%
  nofile_limit        2.1       0.6    7.4%
  perm_harden         5.3       3.2   37.7%
  args_parse          5.3       0.0    0.0%
  config_loaded       6.7       1.4   16.3%
  server_new          7.9       1.2   14.2%
  socket_bound        8.5       0.6    6.5%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~14.5 ms
  Server ready median:     ~22.8 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (df92a5eef817)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (fbd5ecd482d1, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-ny5po7yz
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.161/0.160    p95    0.165/0.180    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    1.256/1.283    p95    1.278/1.402    delta  +0.027 ms  control-no-change
  mcp_config_load   p50    1.456/1.449    p95    1.496/1.486    delta  -0.007 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   57.882/55.428   p95   67.034/60.495   delta  -2.455 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22924..22968 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22914..22968 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   23.249/24.031   p95   23.687/24.996   delta  +0.782 ms  control-no-change
  skill_request_overlay p50   55.424/53.804   p95   70.361/60.718   delta  -1.619 ms  control-no-change
  mcp_config_load   p50    1.565/1.529    p95    1.909/1.622    delta  -0.037 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   64.216/60.688   p95   69.159/83.098   delta  -3.528 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22910..22968 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22960 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.156/0.158    p95    0.210/0.186    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    1.251/1.210    p95    1.342/1.265    delta  -0.041 ms  control-no-change
  mcp_config_load   p50    1.471/1.441    p95    1.586/1.470    delta  -0.030 ms  control-no-change
  attach_history    p50  507.193/516.417  p95  533.985/534.697  delta  +9.224 ms  control-no-change
  resume_ready      p50  231.963/235.272  p95  245.082/245.729  delta  +3.309 ms  control-no-change
  provider_dispatch p50    2.013/1.974    p95    2.122/2.241    delta  -0.039 ms  control-no-change
  session_save      p50   50.610/51.195   p95   52.929/56.650   delta  +0.585 ms  control-no-change
  total store_read_bytes p50 5311224  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 507.193 ms (load 447.633 + history 59.769)
    selection          p50     0.190 ms  min     0.188  max     0.221  ratio 0.0% of anchor
    checkpoint_recent  p50   384.659 ms  min   380.213  max   385.101  ratio 75.8% of anchor
    log_read           p50     0.218 ms  min     0.199  max     0.248  ratio 0.0% of anchor
    full_scan          p50    32.651 ms  min    32.574  max    32.972  ratio 6.4% of anchor
    blob_read          p50     0.008 ms  min     0.006  max     0.008  ratio 0.0% of anchor
    graph_replay       p50    25.567 ms  min    23.888  max    26.409  ratio 5.0% of anchor
    metadata_json      p50     0.013 ms  min     0.013  max     0.023  ratio 0.0% of anchor
    messages_json      p50     1.668 ms  min     1.637  max     1.937  ratio 0.3% of anchor
    messages_decode    p50     1.331 ms  min     1.135  max     1.333  ratio 0.3% of anchor
    source_hash        p50     0.593 ms  min     0.578  max     0.734  ratio 0.1% of anchor
    history_rescan     p50    60.337 ms  min    59.381  max    62.073  ratio 11.9% of anchor
    page_index         p50    88.420 ms  min    87.717  max    95.566  ratio 17.4% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22932..22980 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22911..22960 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   24.813/25.822   p95   28.081/36.639   delta  +1.008 ms  control-no-change
  skill_request_overlay p50   56.084/56.415   p95   63.938/86.315   delta  +0.332 ms  control-no-change
  mcp_config_load   p50    1.529/1.546    p95    2.310/2.029    delta  +0.017 ms  control-no-change
  attach_history    p50  510.280/514.817  p95  869.719/628.494  delta  +4.537 ms  control-no-change
  resume_ready      p50  234.043/235.687  p95  296.722/252.885  delta  +1.644 ms  control-no-change
  provider_dispatch p50    1.934/1.902    p95    3.085/2.009    delta  -0.032 ms  control-no-change
  session_save      p50   53.524/49.867   p95  514.077/103.074  delta  -3.657 ms  control-no-change
  total store_read_bytes p50 5311221  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 510.280 ms (load 451.232 + history 59.503)
    selection          p50     0.188 ms  min     0.176  max     0.214  ratio 0.0% of anchor
    checkpoint_recent  p50   411.634 ms  min   383.376  max   420.401  ratio 80.7% of anchor
    log_read           p50     0.191 ms  min     0.189  max     0.213  ratio 0.0% of anchor
    full_scan          p50    39.240 ms  min    32.566  max    44.540  ratio 7.7% of anchor
    blob_read          p50     0.007 ms  min     0.006  max     0.008  ratio 0.0% of anchor
    graph_replay       p50    28.442 ms  min    24.619  max    32.710  ratio 5.6% of anchor
    metadata_json      p50     0.012 ms  min     0.011  max     0.013  ratio 0.0% of anchor
    messages_json      p50     1.612 ms  min     1.605  max     2.017  ratio 0.3% of anchor
    messages_decode    p50     1.266 ms  min     1.077  max     1.679  ratio 0.2% of anchor
    source_hash        p50     0.573 ms  min     0.560  max     0.628  ratio 0.1% of anchor
    history_rescan     p50    60.469 ms  min    59.908  max    73.018  ratio 11.9% of anchor
    page_index         p50    91.023 ms  min    86.788  max   155.185  ratio 17.8% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189150..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099101..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22926..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099104..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22972 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.158/0.159    p95    0.195/0.205    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    1.247/1.227    p95    1.304/1.369    delta  -0.020 ms  control-no-change
  mcp_config_load   p50    1.446/1.468    p95    1.513/1.562    delta  +0.022 ms  control-no-change
  attach_history    p50 5571.474/5568.400 p95 5668.776/5613.086 delta  -3.074 ms  control-no-change
  resume_ready      p50 2413.501/2405.820 p95 2454.509/2446.347 delta  -7.680 ms  control-no-change
  provider_dispatch p50   21.190/20.983   p95   21.541/21.902   delta  -0.207 ms  control-no-change
  session_save      p50   48.247/47.260   p95   51.884/51.478   delta  -0.988 ms  control-no-change
  total store_read_bytes p50 53417420  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 5571.474 ms (load 4917.795 + history 660.003)
    selection          p50     0.238 ms  min     0.194  max     0.243  ratio 0.0% of anchor
    checkpoint_recent  p50  4270.946 ms  min  4246.248  max  4285.788  ratio 76.7% of anchor
    log_read           p50     1.933 ms  min     1.924  max     1.944  ratio 0.0% of anchor
    full_scan          p50   332.374 ms  min   324.346  max   335.425  ratio 6.0% of anchor
    blob_read          p50     0.050 ms  min     0.049  max     0.051  ratio 0.0% of anchor
    graph_replay       p50   299.008 ms  min   287.887  max   303.128  ratio 5.4% of anchor
    metadata_json      p50     0.015 ms  min     0.015  max     0.015  ratio 0.0% of anchor
    messages_json      p50    21.340 ms  min    21.323  max    21.517  ratio 0.4% of anchor
    messages_decode    p50    17.822 ms  min    17.740  max    17.991  ratio 0.3% of anchor
    source_hash        p50     5.134 ms  min     5.124  max     5.186  ratio 0.1% of anchor
    history_rescan     p50   655.293 ms  min   654.894  max   679.214  ratio 11.8% of anchor
    page_index         p50   710.076 ms  min   694.059  max   715.926  ratio 12.7% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210214..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22927..22975 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22979 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   23.598/24.171   p95   24.168/30.763   delta  +0.573 ms  control-no-change
  skill_request_overlay p50   54.508/55.135   p95   54.913/64.759   delta  +0.627 ms  control-no-change
  mcp_config_load   p50    1.511/1.546    p95    1.608/1.751    delta  +0.035 ms  control-no-change
  attach_history    p50 5569.933/5564.143 p95 5643.630/5683.045 delta  -5.790 ms  control-no-change
  resume_ready      p50 2382.174/2382.422 p95 2427.959/2404.737 delta  +0.248 ms  control-no-change
  provider_dispatch p50   20.881/20.779   p95   22.159/21.175   delta  -0.101 ms  control-no-change
  session_save      p50   44.812/46.174   p95   48.472/51.761   delta  +1.363 ms  control-no-change
  total store_read_bytes p50 53417432  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 5569.933 ms (load 4912.281 + history 651.068)
    selection          p50     0.201 ms  min     0.193  max     0.247  ratio 0.0% of anchor
    checkpoint_recent  p50  4231.726 ms  min  4221.550  max  4235.118  ratio 76.0% of anchor
    log_read           p50     1.940 ms  min     1.876  max     2.009  ratio 0.0% of anchor
    full_scan          p50   325.514 ms  min   325.054  max   332.298  ratio 5.8% of anchor
    blob_read          p50     0.052 ms  min     0.051  max     0.052  ratio 0.0% of anchor
    graph_replay       p50   297.162 ms  min   290.678  max   298.396  ratio 5.3% of anchor
    metadata_json      p50     0.014 ms  min     0.013  max     0.016  ratio 0.0% of anchor
    messages_json      p50    21.418 ms  min    21.216  max    21.419  ratio 0.4% of anchor
    messages_decode    p50    17.003 ms  min    16.535  max    17.284  ratio 0.3% of anchor
    source_hash        p50     5.154 ms  min     5.078  max     5.205  ratio 0.1% of anchor
    history_rescan     p50   651.316 ms  min   643.645  max   653.297  ratio 11.7% of anchor
    page_index         p50   705.759 ms  min   704.094  max   730.814  ratio 12.7% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22974 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184244..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210207..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22939..22979 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (df92a5eef817)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (fbd5ecd482d1, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-utuwbxfd
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.076/0.079    p95    0.120/0.120    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.631/0.629    p95    0.725/0.954    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.794/0.778    p95    0.962/0.940    delta  -0.015 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  session_save      p50   34.637/34.536   p95   36.767/36.692   delta  -0.101 ms  control-no-change
  total store_read_bytes p50 23622  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22927, 22933, 22934, 22955, 23616, 23627, 23643, 24308, 24327, 24346]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22909, 22917, 22949, 23634, 23648, 23652, 24347, 24351, 24357, 24359]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22927..24346 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22909..24359 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   12.361/12.169   p95   16.596/13.118   delta  -0.192 ms  control-no-change
  skill_request_overlay p50   31.453/31.225   p95   33.683/46.306   delta  -0.228 ms  control-no-change
  mcp_config_load   p50    0.813/0.841    p95    0.872/1.043    delta  +0.028 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.004/0.005    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-drift
  session_save      p50   40.202/39.766   p95   42.130/44.225   delta  -0.435 ms  control-no-change
  total store_read_bytes p50 23656  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22929, 22930, 22941, 23620, 23653, 23658, 24338, 24352, 24355, 24357]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22925, 22941, 23618, 23644, 23652, 24333, 24335, 24345, 24359]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22929..24357 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22925..24359 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.074/0.074    p95    0.092/0.084    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.581/0.561    p95    0.837/0.663    delta  -0.020 ms  control-no-change
  mcp_config_load   p50    0.742/0.753    p95    0.856/0.863    delta  +0.011 ms  control-no-change
  attach_history    p50  171.497/168.361  p95  191.754/172.853  delta  -3.136 ms  control-no-change
  resume_ready      p50  120.605/121.524  p95  124.704/131.275  delta  +0.919 ms  control-no-change
  provider_dispatch p50    1.584/1.570    p95    1.755/1.677    delta  -0.014 ms  control-no-change
  session_save      p50   32.120/32.842   p95   35.630/35.770   delta  +0.722 ms  control-no-change
  total store_read_bytes p50 2841127  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23641, 23657, 24346, 24360, 25036, 25066, 25739, 25757, 26430, 26448]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23658, 23659, 24346, 24353, 25033, 25035, 25726, 25756, 26406, 26433]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23641..26448 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23658..26433 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   12.100/12.295   p95   18.886/12.887   delta  +0.195 ms  control-no-change
  skill_request_overlay p50   31.045/30.905   p95   32.944/36.753   delta  -0.139 ms  control-no-change
  mcp_config_load   p50    0.807/0.842    p95    0.903/0.944    delta  +0.034 ms  control-no-change
  attach_history    p50  168.150/166.268  p95  187.227/176.463  delta  -1.881 ms  control-no-change
  resume_ready      p50  119.493/117.958  p95  126.381/134.495  delta  -1.535 ms  control-no-change
  provider_dispatch p50    1.556/1.551    p95    1.764/1.711    delta  -0.004 ms  control-no-change
  session_save      p50   39.222/39.191   p95   42.408/40.719   delta  -0.032 ms  control-no-change
  total store_read_bytes p50 2841129  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23642, 23651, 24310, 24374, 25021, 25077, 25708, 25726, 26443, 26461]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23659, 23679, 24346, 24358, 25049, 25061, 25724, 25760, 26435, 26455]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23642..26461 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23659..26455 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.074/0.079    p95    0.106/0.117    delta  +0.005 ms  control-no-change
  skill_request_overlay p50    0.609/0.621    p95    0.684/0.794    delta  +0.012 ms  control-no-change
  mcp_config_load   p50    0.759/0.773    p95    0.882/0.839    delta  +0.014 ms  control-no-change
  attach_history    p50 1824.726/1822.797 p95 1850.600/1851.459 delta  -1.930 ms  control-no-change
  resume_ready      p50 1284.232/1295.390 p95 1329.096/1328.144 delta +11.158 ms  control-no-change
  provider_dispatch p50   16.817/16.798   p95   17.622/19.657   delta  -0.019 ms  control-no-change
  session_save      p50   33.379/33.490   p95   36.504/69.927   delta  +0.111 ms  control-no-change
  total store_read_bytes p50 28640164  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23637, 23650, 24328, 24332, 25062, 25073, 25733, 25760, 26386, 26428]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23652, 23668, 24327, 24359, 25045, 25070, 25752, 25773, 26427, 26431]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23637..26428 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23652..26431 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   12.145/12.334   p95   12.817/13.322   delta  +0.189 ms  control-no-change
  skill_request_overlay p50   32.810/31.692   p95   34.683/33.017   delta  -1.118 ms  control-no-change
  mcp_config_load   p50    0.863/0.866    p95    1.103/0.982    delta  +0.003 ms  control-no-change
  attach_history    p50 1796.592/1820.685 p95 1830.106/1861.950 delta +24.093 ms  control-no-change
  resume_ready      p50 1277.042/1273.733 p95 1311.805/1340.287 delta  -3.309 ms  control-no-change
  provider_dispatch p50   16.840/16.891   p95   18.854/18.131   delta  +0.051 ms  control-no-change
  session_save      p50   33.627/34.995   p95   35.942/47.621   delta  +1.367 ms  control-no-change
  total store_read_bytes p50 28640148  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23650, 23668, 24345, 24357, 25031, 25072, 25738, 25759, 26447, 26450]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23614, 23639, 24363, 24366, 25028, 25050, 25741, 25757, 26423, 26452]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23650..26450 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23614..26452 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.3 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 7.8 ms | 10.4 ms | 7.4 ms | 10.4 ms | 5 |
| write (10KB) | 7.4 ms | 10.0 ms | 7.4 ms | 10.0 ms | 5 |
| edit (simple replace) | 7.9 ms | 9.9 ms | 7.4 ms | 9.9 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.8 ms | 7.9 ms | 7.4 ms | 7.9 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 5.3 ms | 4.8 ms | 5.3 ms | 5 |
| bash (true) | 10.3 ms | 10.6 ms | 9.7 ms | 10.6 ms | 5 |
| bash (echo hello) | 12.8 ms | 15.3 ms | 10.5 ms | 15.3 ms | 5 |
| bash (ls fixture tree) | 13.0 ms | 15.1 ms | 12.5 ms | 15.1 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |

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
| 1 | 219.7 ms | 219.7 ms | 219.7 ms | 219.9 ms | 0 |
| 10 | 667.6 ms | 669.9 ms | 669.9 ms | 671.6 ms | 0 |
| 40 | 2.04 s | 2.51 s | 2.51 s | 2.52 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 763.7 ms | 763.7 ms | 763.7 ms | 1 |
| leaf_touch | src\main.rs | 917.9 ms | 917.9 ms | 917.9 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 39.39 s | 39.39 s | 39.39 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 47.71 s | 47.71 s | 47.71 s | 1 |

## TUI (`horon-tui`)

The interactive client is a second binary and is measured on its own. These numbers come from the `tui` suite and are never compared with the headless tables above: `first_frame_ms` and `direct_first_frame_ms` are two launches of two different programs, and the handoff is the difference between them.

![TUI (`horon-tui`)](charts/tui-01-latency.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 38.4 ms | 38.1 ms–169.8 ms | 5 |
| First frame (direct `horon-tui`) | 41.1 ms | 40.2 ms–41.6 ms | 5 |
| TTY handoff cost | -2.1 ms | -2.8 ms–128.2 ms | 5 |
| Key press to echo (p95) | 0.1 ms | 0.1 ms–0.3 ms | 50 |
| Idle RSS after 5 s | 5.9 MB | 5.8 MB–6.0 MB | 5 |
| RSS after one turn | 6.1 MB | 5.9 MB–6.2 MB | 5 |
| Frame draw time (p50) | 0.7 ms | 0.7 ms–0.7 ms | 5 |
| Frame draw time (p95) | 0.8 ms | 0.8 ms–0.8 ms | 5 |
| Install size (`horon-tui`) | 6.3 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.3 ms | 0.3 ms–0.3 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.3 ms–0.4 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.1 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.2 µs | 0.2 µs–0.3 µs | 5 |
| First held frame after scrolling | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Key dispatch through completed draw (p50) | 0.3 ms | 0.3 ms–0.3 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.3 ms–0.4 ms | 5 |
| First key dispatch through completed draw | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame redraw without input (p50) | 0.3 ms | 0.3 ms–0.3 ms | 5 |
| Held frame redraw without input (p95) | 0.3 ms | 0.3 ms–0.4 ms | 5 |

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 55.0 ms | 49.6 ms–85.8 ms | 5 |
| First frame (direct `horon-tui`) | 46.9 ms | 35.9 ms–55.8 ms | 5 |
| TTY handoff cost | 13.7 ms | -0.2 ms–30.0 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.4 ms | 50 |
| Idle RSS after 5 s | 9.5 MB | 9.4 MB–9.5 MB | 5 |
| RSS after one turn | 9.7 MB | 9.6 MB–9.7 MB | 5 |
| Frame draw time (p50) | 0.7 ms | 0.6 ms–0.7 ms | 5 |
| Frame draw time (p95) | 0.8 ms | 0.7 ms–0.9 ms | 5 |
| Install size (`horon-tui`) | 5.3 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.3 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.3 ms–0.5 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.1 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.3 µs | 0.2 µs–0.4 µs | 5 |
| First held frame after scrolling | 0.6 ms | 0.5 ms–0.7 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.3 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.3 ms–0.5 ms | 5 |
| First key dispatch through completed draw | 0.6 ms | 0.5 ms–0.7 ms | 5 |
| Held frame redraw without input (p50) | 0.3 ms | 0.3 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.3 ms–0.4 ms | 5 |

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
| Frame draw time (p50) | 1.9 ms | 1.8 ms–1.9 ms | 5 |
| Frame draw time (p95) | 1.9 ms | 1.9 ms–2.1 ms | 5 |
| Install size (`horon-tui`) | 5.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame draw (p95) | 0.6 ms | 0.6 ms–1.0 ms | 5 |
| Scroll key handling (p50) | 0.1 µs | 0.1 µs–0.1 µs | 5 |
| Scroll key handling (p95) | 0.2 µs | 0.2 µs–0.5 µs | 5 |
| First held frame after scrolling | 0.9 ms | 0.9 ms–0.9 ms | 5 |
| Key dispatch through completed draw (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Key dispatch through completed draw (p95) | 0.6 ms | 0.6 ms–1.0 ms | 5 |
| First key dispatch through completed draw | 0.9 ms | 0.9 ms–0.9 ms | 5 |
| Held frame redraw without input (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame redraw without input (p95) | 0.6 ms | 0.6 ms–0.6 ms | 5 |

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
