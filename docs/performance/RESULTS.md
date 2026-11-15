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
| 17. CI structural gate and report template | `[~]` | The gate really runs (38 bounds in `scripts/perf/structural_budget.json`, `EXIT=0`, `EXIT=1` on violation, `EXIT=2` when a measurement is missing, 77 contract tests in `scripts/perf/test_check_structural.py`). Two conditions from the plan remain. There is no frame count counter, and "passes at B0" is structurally impossible (the code that prints the `STRUCTURAL_PERF` line landed after B0). The gate now asserts the frame cap itself in its `frame` section for both readers, and still reads no frame count |

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

The structural gate now bounds each of these at the after value (`prefix_tools_bytes_max` 9151, down from 9751).

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
- The plan targeted a catalog of at most 9,000 B and a core one-shot `tools` cut of at least 700 B. The `intent` description keeps "shown in the UI" because a contract test requires it, and `bash` keeps its note that `rg` skips hidden and ignored files, which another test pins, so the catalog is 9,151 B and the cut is 563 B.
- `[features] foreign_rules` (off by default) still reads its rule files on every request and is not counted by the prompt source reads.
- Both Windows benchmark builds print the same two `linker stdout` lines for the baseline and the candidate. They come from the benchmark's Windows release link, not from this change; the ci.yml runs of this branch have no warning.

## Automatic measurement: latest measured commit on `main`

When a commit on `main` passes both CI and the structural performance gate, and the agent code (`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`) has changed since the previous measurement, `.github/workflows/benchmark-docs.yml` runs the full benchmark suite separately per OS on Linux, macOS and Windows and rewrites the section below on both `main` and `dev`. There is one section per OS. How the measurement works and what each suite means are in "Automatic measurement" in [`BENCHMARK.md`](BENCHMARK.md).

<!-- bench-auto:start commit=f7a53f1ebbc9a5dd4d9461c0aef60877aef898b5 -->

## Headless (`horon-agent`)

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `f7a53f1ebbc9a5dd4d9461c0aef60877aef898b5`
- Measured at: 2026-11-15 14:09 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/43719041648> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (f7a53f1)` | Success |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (f7a53f1)` | Success |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (f7a53f1)` | Success |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (f7a53f1) | 4.2 ms | 115.6 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.2 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 10.5 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 43.7 ms | 435.0 MB |
| OpenCode | 1.19.5 | 466.4 ms | 613.4 MB |
| pi | 1.5.0 | 238.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 650.8 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 122.0 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 43.1 ms | 41.2 ms–49.9 ms | baseline | 39.8 MB | baseline | yes | 10 | 3,731 / 2,390 (1 tools) | — |
| jcode | 144.1 ms | 143.7 ms–254.0 ms | 3.35× more | 41.7 MB | 1.05× more | yes | 5 | 3,245 / 1,433 (1 tools) | — |
| Claude Code | 440.9 ms | 417.7 ms–460.2 ms | 10.24× more | 206.9 MB | 5.19× more | yes | — | 45,782 / 29,653 (12 tools) | — |
| Codex CLI | 475.7 ms | 466.3 ms–564.1 ms | 11.05× more | 190.3 MB | 4.78× more | yes | 6 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 2.91 s | 2.73 s–3.00 s | 67.62× more | 560.9 MB | 14.08× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 404.1 ms | 403.4 ms–414.2 ms | 9.39× more | 113.3 MB | 2.84× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.01 s | 1.74 s–2.13 s | 46.64× more | 395.7 MB | 9.93× more | yes | 68 | 29,860 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 6.11 s | 6.02 s–6.63 s | 141.82× more | 663.2 MB | 16.65× more | yes | 36 | 27,452 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 44.2 ms | 44.1 ms–45.4 ms | baseline | 42.3 MB | baseline | — | yes | — | 11,254 / 9,255 (8 tools) |
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
| horon | 36.9 ms | 36.7 ms–41.2 ms | baseline | 18.1 MB | baseline | skill-uri | yes | — |
| jcode | 58.5 ms | 56.1 ms–64.1 ms | 1.58× more | 40.4 MB | 2.24× more | skill-tool | yes | — |
| Claude Code | 440.6 ms | 428.2 ms–443.6 ms | 11.95× more | 206.9 MB | 11.46× more | skill-tool | yes | — |
| Codex CLI | 550.4 ms | 541.3 ms–558.9 ms | 14.92× more | 191.4 MB | 10.60× more | file-read | yes | — |
| OpenCode | 3.27 s | 3.24 s–3.31 s | 88.78× more | 601.6 MB | 33.31× more | skill-tool | yes | — |
| pi | 531.1 ms | 521.2 ms–542.7 ms | 14.40× more | 126.8 MB | 7.02× more | file-read | yes | — |
| oh-my-pi (omp) | 2.02 s | 1.77 s–2.05 s | 54.65× more | 381.3 MB | 21.12× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.69 s | 6.41 s–6.72 s | 181.33× more | 674.2 MB | 37.33× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 61.9 ms | 60.5 ms–62.8 ms | baseline | 45.5 MB | baseline | mcp-call | yes | 46.2 ms | — |
| jcode | 177.5 ms | 175.3 ms–181.2 ms | 2.87× more | 56.5 MB | 1.24× more | mcp-action | yes | 144.6 ms | — |
| Claude Code | 540.9 ms | 534.1 ms–548.6 ms | 8.74× more | 227.4 MB | 5.00× more | mcp-tool | yes | 397.7 ms | — |
| Codex CLI | 495.6 ms | 477.6 ms–498.4 ms | 8.01× more | 196.8 MB | 4.33× more | mcp-tool | yes | 293.2 ms | — |
| OpenCode | 2.93 s | 2.87 s–2.95 s | 47.44× more | 570.3 MB | 12.53× more | mcp-tool | yes | 2.67 s | — |
| pi | 563.1 ms | 561.5 ms–587.6 ms | 9.10× more | 139.1 MB | 3.06× more | mcp-tool | yes | 504.0 ms | — |
| oh-my-pi (omp) | 2.06 s | 1.77 s–2.11 s | 33.26× more | 390.7 MB | 8.59× more | mcp-route | yes | 1.66 s | — |
| oh-my-openagent (omo) | 6.87 s | 6.83 s–6.90 s | 111.08× more | 673.3 MB | 14.80× more | mcp-tool | yes | 3.38 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 37.0 MB | 231.7 KB | baseline | 0.80 | 1.00 | 44.3 MB | 634.3 KB |
| jcode | 42.9 MB | 120.3 KB | 1.93× less | 0.00 | — | 46.4 MB | 216.9 KB |
| Claude Code | 150.3 MB | 148.5 MB | 656.30× more | — | — | 185.1 MB | 180.7 MB |
| Codex CLI | 164.1 MB | 131.4 MB | 580.53× more | — | — | 162.0 MB | 163.9 MB |
| OpenCode | 240.8 MB | 918.3 KB | 3.96× more | — | — | 444.5 MB | 9.2 MB |
| pi | 109.5 MB | 107.3 MB | 474.04× more | — | — | 132.4 MB | 127.8 MB |
| oh-my-pi (omp) | 335.0 MB | 67.1 MB | 296.54× more | — | — | 362.2 MB | 357.3 MB |
| oh-my-openagent (omo) | 272.3 MB | 37.4 MB | 165.17× more | — | — | 588.0 MB | 576.9 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.3 MB | 47.4 MB | 49.9 MB |
| jcode | 46.4 MB | 47.1 MB | 48.3 MB |
| Claude Code | 185.1 MB | 914.6 MB | 1.8 GB |
| Codex CLI | 162.0 MB | 812.1 MB | 1.6 GB |
| OpenCode | 444.5 MB | 485.8 MB | 527.7 MB |
| pi | 132.4 MB | 620.8 MB | 1.3 GB |
| oh-my-pi (omp) | 362.2 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 588.0 MB | 3.0 GB | 5.7 GB |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    2.66 ms
  Max:    3.09 ms
  Mean:   2.88 ms
  Median: 2.87 ms
  Stdev:  0.13 ms

--version:
  Min:    2.30 ms
  Max:    2.63 ms
  Mean:   2.53 ms
  Median: 2.54 ms
  Stdev:  0.09 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.40 ms
  Max:    20.45 ms
  Mean:   20.41 ms
  Median: 20.41 ms
  Stdev:  0.02 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.3%
  logging_init        0.1       0.1   16.7%
  nofile_limit        0.4       0.2   25.8%
  perm_harden         0.4       0.0    0.9%
  args_parse          0.4       0.0    0.1%
  config_loaded       0.5       0.1   11.7%
  server_new          0.8       0.3   37.3%
  socket_bound        0.8       0.1    7.1%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.55 ms
  Max:    20.81 ms
  Mean:   20.68 ms
  Median: 20.67 ms
  Stdev:  0.08 ms

Bridge stage: hello_ok:
  Min:    21.02 ms
  Max:    21.34 ms
  Mean:   21.18 ms
  Median: 21.19 ms
  Stdev:  0.09 ms

Bridge stage: no-op reply:
  Min:    21.38 ms
  Max:    21.69 ms
  Mean:   21.50 ms
  Median: 21.50 ms
  Stdev:  0.08 ms

Bridge total (socket -> no-op):
  Min:    21.38 ms
  Max:    21.69 ms
  Mean:   21.50 ms
  Median: 21.50 ms
  Stdev:  0.08 ms

============================================================
Summary:
  Binary load median:      ~2.9 ms
  Server ready median:     ~20.4 ms
  Bridge total median:     ~21.5 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (d204df43dbd0)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-b3tqi4o5
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.035/0.032    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.109/0.107    p95    0.140/0.133    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.219/0.165    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    9.219/9.250    p95   11.512/12.289   delta  +0.032 ms  control-no-change
  total store_read_bytes p50 23070  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23056..23084 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23088 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.789/2.792    p95    2.872/2.885    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    4.847/4.856    p95    5.107/4.965    delta  +0.009 ms  control-no-change
  mcp_config_load   p50    0.149/0.150    p95    0.151/0.160    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   12.070/12.435   p95   12.608/20.689   delta  +0.365 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23080 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23080 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.015    p95    0.035/0.025    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.106    p95    0.108/0.149    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.190/0.151    delta  +0.000 ms  control-no-change
  attach_history    p50  304.306/303.491  p95  307.457/307.527  delta  -0.815 ms  control-no-change
  resume_ready      p50   54.413/54.530   p95   55.455/54.832   delta  +0.117 ms  control-no-change
  provider_dispatch p50    1.352/1.373    p95    1.389/1.411    delta  +0.021 ms  control-no-change
  session_save      p50    8.176/8.090    p95    8.504/8.539    delta  -0.086 ms  control-no-change
  total store_read_bytes p50 5311402  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23066..23086 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23062..23086 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.803/2.844    p95    2.935/2.951    delta  +0.041 ms  control-no-change
  skill_request_overlay p50    4.820/4.862    p95    4.923/5.006    delta  +0.042 ms  control-no-change
  mcp_config_load   p50    0.149/0.149    p95    0.157/0.161    delta  -0.000 ms  control-no-change
  attach_history    p50  308.290/308.160  p95  316.389/311.427  delta  -0.130 ms  control-no-change
  resume_ready      p50   54.732/54.478   p95   55.697/54.819   delta  -0.253 ms  control-no-change
  provider_dispatch p50    1.379/1.376    p95    1.428/1.898    delta  -0.003 ms  control-no-change
  session_save      p50    8.366/8.239    p95    9.445/8.330    delta  -0.127 ms  control-no-change
  total store_read_bytes p50 5311398  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23066..23082 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23058..23082 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.016    p95    0.018/0.017    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.107/0.107    p95    0.108/0.128    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.148/0.148    p95    0.169/0.178    delta  -0.000 ms  control-no-change
  attach_history    p50 3945.201/3907.311 p95 3992.407/3947.387 delta -37.890 ms  control-no-change
  resume_ready      p50  625.801/624.753  p95  640.838/642.426  delta  -1.048 ms  control-no-change
  provider_dispatch p50   16.455/16.213   p95   17.070/17.593   delta  -0.242 ms  control-no-change
  session_save      p50    8.543/8.609    p95    9.315/9.733    delta  +0.066 ms  control-no-change
  total store_read_bytes p50 53417606  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23065..23093 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 42184283..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210237..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23045..23089 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.770/2.850    p95    2.929/2.889    delta  +0.080 ms  control-no-change
  skill_request_overlay p50    4.794/4.770    p95    5.009/4.858    delta  -0.024 ms  control-no-change
  mcp_config_load   p50    0.149/0.151    p95    0.153/0.160    delta  +0.002 ms  control-no-change
  attach_history    p50 3920.395/3908.052 p95 3969.795/3929.240 delta -12.343 ms  control-no-change
  resume_ready      p50  625.867/624.011  p95  633.124/655.901  delta  -1.857 ms  control-no-change
  provider_dispatch p50   15.786/16.066   p95   17.742/16.509   delta  +0.279 ms  control-no-change
  session_save      p50    8.347/8.527    p95    8.629/9.036    delta  +0.180 ms  control-no-change
  total store_read_bytes p50 53417605  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184287..42184289 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23045..23085 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184287..42184289 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210243 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23067..23081 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (d204df43dbd0)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-yukyapfn
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.016    p95    0.021/0.021    delta  +0.001 ms  control-drift
  skill_request_overlay p50    0.104/0.104    p95    0.167/0.135    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.142/0.141    p95    0.198/0.197    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    9.021/9.390    p95   10.438/10.520   delta  +0.368 ms  control-no-change
  total store_read_bytes p50 23805  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23073, 23081, 23779, 23799, 23811, 24505, 24513, 24517]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23077, 23081, 23783, 23791, 23799, 24501, 24509]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23073..24517 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24509 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.803/2.832    p95    2.891/2.902    delta  +0.028 ms  control-no-change
  skill_request_overlay p50    4.749/4.750    p95    4.833/4.894    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.149/0.147    p95    0.164/0.156    delta  -0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   10.315/10.005   p95   11.303/11.155   delta  -0.310 ms  control-no-change
  total store_read_bytes p50 23776  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23057, 23069, 23071, 23773, 23779, 23789, 24501, 24503, 24517]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23075, 23085, 23769, 23783, 23803, 24497, 24515, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23057..24517 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24517 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.016/0.016    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.104/0.104    p95    0.138/0.134    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.141    p95    0.177/0.161    delta  -0.002 ms  control-no-change
  attach_history    p50   99.757/100.613  p95  102.287/103.246  delta  +0.856 ms  control-no-change
  resume_ready      p50   53.567/54.532   p95   54.512/56.840   delta  +0.965 ms  control-no-change
  provider_dispatch p50    1.353/1.369    p95    1.396/1.408    delta  +0.015 ms  control-no-change
  session_save      p50    8.240/8.461    p95   12.037/13.754   delta  +0.221 ms  control-no-change
  total store_read_bytes p50 2841367  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23780, 23796, 24502, 24514, 25222, 25244, 25938, 26660, 26676]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23774, 23784, 24498, 24504, 25228, 25240, 25946, 25950, 26644, 26668]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23780..26676 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23774..26668 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.795/2.779    p95    2.819/2.886    delta  -0.016 ms  control-no-change
  skill_request_overlay p50    4.717/4.731    p95    4.836/4.991    delta  +0.014 ms  control-no-change
  mcp_config_load   p50    0.148/0.147    p95    0.159/0.170    delta  -0.001 ms  control-no-change
  attach_history    p50   99.806/101.511  p95  101.797/103.406  delta  +1.705 ms  control-no-change
  resume_ready      p50   53.634/53.948   p95   54.832/55.893   delta  +0.314 ms  control-no-change
  provider_dispatch p50    1.340/1.335    p95    1.400/1.370    delta  -0.006 ms  control-no-change
  session_save      p50   10.071/9.829    p95   15.610/10.562   delta  -0.242 ms  control-no-change
  total store_read_bytes p50 2841367  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23776, 23794, 24496, 24510, 25226, 25240, 25940, 25950, 26662]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23784, 23796, 24502, 24522, 25224, 25238, 25952, 25954, 26658, 26672]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23776..26662 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23784..26672 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.030/0.017    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.104/0.105    p95    0.124/0.124    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.142/0.142    p95    0.161/0.185    delta  +0.001 ms  control-no-change
  attach_history    p50 1127.581/1119.326 p95 1146.587/1126.173 delta  -8.255 ms  control-no-change
  resume_ready      p50  599.618/614.080  p95  623.547/632.621  delta +14.463 ms  control-no-change
  provider_dispatch p50   15.295/16.041   p95   16.485/18.685   delta  +0.746 ms  control-no-change
  session_save      p50    8.194/7.975    p95    9.698/8.397    delta  -0.219 ms  control-no-change
  total store_read_bytes p50 28640382  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23783, 23803, 24509, 24517, 25231, 25949, 25965, 26659, 26667]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23795, 23799, 24517, 25231, 25233, 25937, 25951, 26665, 26679]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23783..26667 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23795..26679 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.785/2.780    p95    2.889/3.471    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    4.666/4.712    p95    4.857/4.970    delta  +0.046 ms  control-no-change
  mcp_config_load   p50    0.146/0.146    p95    0.162/0.161    delta  +0.000 ms  control-no-change
  attach_history    p50 1104.546/1097.570 p95 1132.358/1116.135 delta  -6.975 ms  control-no-change
  resume_ready      p50  609.316/597.623  p95  631.949/624.059  delta -11.693 ms  control-no-change
  provider_dispatch p50   15.159/14.679   p95   16.311/15.249   delta  -0.480 ms  control-no-change
  session_save      p50    9.558/9.481    p95    9.989/9.970    delta  -0.077 ms  control-no-change
  total store_read_bytes p50 28640388  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23795, 23799, 24521, 24525, 25235, 25239, 25945, 25951, 26659, 26669]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23787, 23799, 24513, 24517, 25219, 25221, 25947, 25953, 26657, 26671]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23795..26669 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23787..26671 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.3 ms | 0.3 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 1.1 ms | 2.9 ms | 1.0 ms | 2.9 ms | 5 |
| write (10KB) | 1.3 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| edit (simple replace) | 1.1 ms | 1.1 ms | 1.1 ms | 1.1 ms | 5 |
| ls (scripts dir) | 0.6 ms | 0.6 ms | 0.5 ms | 0.6 ms | 5 |
| agentgrep (grep in tree) | 1.4 ms | 1.4 ms | 1.3 ms | 1.4 ms | 5 |
| agentgrep (find **/*.rs) | 1.2 ms | 1.2 ms | 1.2 ms | 1.2 ms | 5 |
| bash (true) | 1.6 ms | 1.9 ms | 1.6 ms | 1.9 ms | 5 |
| bash (echo hello) | 1.6 ms | 1.7 ms | 1.6 ms | 1.7 ms | 5 |
| bash (ls fixture tree) | 2.8 ms | 2.8 ms | 2.8 ms | 2.8 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 81.5 ms | 81.5 ms | 81.5 ms | 82.5 ms | 0 |
| 10 | 322.3 ms | 388.5 ms | 388.5 ms | 389.1 ms | 0 |
| 40 | 1.14 s | 1.15 s | 1.16 s | 1.16 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 204.8 ms | 204.8 ms | 204.8 ms | 1 |
| leaf_touch | src/main.rs | 314.8 ms | 314.8 ms | 314.8 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 16.09 s | 16.09 s | 16.09 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 22.62 s | 22.62 s | 22.62 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 5 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (f7a53f1) | 7.5 ms | 97.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.7 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 15.8 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 45.6 ms | 346.5 MB |
| OpenCode | 1.19.5 | 297.3 ms | 344.6 MB |
| pi | 1.5.0 | 180.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 334.2 ms | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 90.2 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 55.2 ms | 47.0 ms–207.9 ms | baseline | 31.8 MB | baseline | yes | 3,619 / 2,390 (1 tools) | — |
| jcode | 366.4 ms | 332.3 ms–492.5 ms | 6.63× more | 34.1 MB | 1.07× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 310.7 ms | 276.3 ms–486.2 ms | 5.63× more | 198.1 MB | 6.24× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 1.06 s | 522.6 ms–1.71 s | 19.19× more | 155.4 MB | 4.89× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 1.86 s | 1.79 s–2.64 s | 33.65× more | 716.7 MB | 22.56× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 308.9 ms | 280.7 ms–315.4 ms | 5.59× more | 123.5 MB | 3.89× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 942.7 ms | 889.0 ms–1.01 s | 17.07× more | 369.1 MB | 11.62× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 4.00 s | 3.79 s–5.11 s | 72.46× more | 883.1 MB | 27.80× more | yes | 27,501 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 76.9 ms | 57.1 ms–100.5 ms | baseline | 32.6 MB | baseline | — | yes | — | 11,142 / 9,255 (8 tools) |
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
| horon | 48.8 ms | 47.1 ms–63.3 ms | baseline | 23.6 MB | baseline | skill-uri | yes | — |
| jcode | 273.0 ms | 245.3 ms–306.2 ms | 5.60× more | 36.7 MB | 1.55× more | skill-tool | yes | — |
| Claude Code | 248.5 ms | 238.8 ms–274.9 ms | 5.10× more | 198.1 MB | 8.39× more | skill-tool | yes | — |
| Codex CLI | 579.4 ms | 419.1 ms–754.1 ms | 11.88× more | 145.3 MB | 6.15× more | file-read | yes | — |
| OpenCode | 2.88 s | 2.82 s–3.11 s | 59.10× more | 733.1 MB | 31.05× more | skill-tool | yes | — |
| pi | 351.1 ms | 332.6 ms–366.0 ms | 7.20× more | 135.5 MB | 5.74× more | file-read | yes | — |
| oh-my-pi (omp) | 947.3 ms | 939.0 ms–952.3 ms | 19.43× more | 359.5 MB | 15.23× more | file-read | yes | — |
| oh-my-openagent (omo) | 4.04 s | 3.97 s–4.59 s | 82.84× more | 884.3 MB | 37.46× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 66.6 ms | 65.0 ms–91.0 ms | baseline | 19.4 MB | baseline | mcp-call | yes | 51.0 ms | — |
| jcode | 304.5 ms | 284.6 ms–310.2 ms | 4.57× more | 52.3 MB | 2.70× more | mcp-action | yes | 282.4 ms | — |
| Claude Code | 349.2 ms | 336.0 ms–396.1 ms | 5.24× more | 225.2 MB | 11.61× more | mcp-tool | yes | 244.9 ms | — |
| Codex CLI | 792.6 ms | 422.9 ms–1.11 s | 11.90× more | 144.8 MB | 7.47× more | mcp-tool | yes | 175.3 ms | — |
| OpenCode | 2.36 s | 2.31 s–2.60 s | 35.47× more | 733.8 MB | 37.84× more | mcp-tool | yes | 2.09 s | — |
| pi | 340.2 ms | 329.0 ms–353.7 ms | 5.11× more | 151.2 MB | 7.80× more | mcp-tool | yes | 299.1 ms | — |
| oh-my-pi (omp) | 1.01 s | 972.4 ms–1.10 s | 15.20× more | 370.5 MB | 19.11× more | mcp-route | yes | 964.6 ms | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | no | — | failed: no tool that could reach the fixture was offered (one request, no tool call) |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 29.3 MB | 139.9 KB | baseline | 0.11 | 1.00 | 35.4 MB | 390.6 KB |
| jcode | 36.7 MB | 210.5 KB | 1.50× more | — | — | 40.2 MB | 332.3 KB |
| Claude Code | 159.8 MB | 158.1 MB | 1,157× more | — | — | 190.0 MB | 187.1 MB |
| Codex CLI | 103.9 MB | 83.6 MB | 612.08× more | — | — | 109.2 MB | 108.9 MB |
| OpenCode | 268.8 MB | 555.9 KB | 3.97× more | — | — | 587.0 MB | 11.7 MB |
| pi | 118.4 MB | 116.6 MB | 853.18× more | — | — | 148.4 MB | 148.0 MB |
| oh-my-pi (omp) | 445.7 MB | 276.0 MB | 2,019× more | — | — | 468.1 MB | 448.5 MB |
| oh-my-openagent (omo) | 439.6 MB | 14.6 MB | 106.78× more | — | — | 905.4 MB | 800.9 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.4 MB | 37.0 MB | 38.8 MB |
| jcode | 40.2 MB | 42.2 MB | 43.2 MB |
| Claude Code | 190.0 MB | 935.3 MB | 1.8 GB |
| Codex CLI | 109.2 MB | 544.5 MB | 1.1 GB |
| OpenCode | 587.0 MB | 621.5 MB | 691.5 MB |
| pi | 148.4 MB | 742.3 MB | 1.4 GB |
| oh-my-pi (omp) | 468.1 MB | 2.3 GB | 4.4 GB |
| oh-my-openagent (omo) | 905.4 MB | 4.2 GB | 7.9 GB |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    7.63 ms
  Max:    10.70 ms
  Mean:   8.85 ms
  Median: 8.46 ms
  Stdev:  0.96 ms

--version:
  Min:    7.27 ms
  Max:    12.25 ms
  Mean:   8.54 ms
  Median: 7.66 ms
  Stdev:  1.69 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.94 ms
  Max:    95.16 ms
  Mean:   55.38 ms
  Median: 53.56 ms
  Stdev:  26.27 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.0%
  logging_init        0.2       0.2    1.7%
  nofile_limit        0.5       0.2    1.9%
  perm_harden         0.5       0.0    0.3%
  args_parse          0.5       0.0    0.0%
  config_loaded      12.7      12.2   93.2%
  server_new         12.9       0.2    1.5%
  socket_bound       13.1       0.2    1.5%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    27.05 ms
  Max:    91.74 ms
  Mean:   50.93 ms
  Median: 41.03 ms
  Stdev:  26.00 ms

Bridge stage: hello_ok:
  Min:    27.61 ms
  Max:    92.50 ms
  Mean:   51.85 ms
  Median: 42.00 ms
  Stdev:  26.01 ms

Bridge stage: no-op reply:
  Min:    28.11 ms
  Max:    92.93 ms
  Mean:   52.33 ms
  Median: 42.52 ms
  Stdev:  26.03 ms

Bridge total (socket -> no-op):
  Min:    28.11 ms
  Max:    92.93 ms
  Mean:   52.33 ms
  Median: 42.52 ms
  Stdev:  26.03 ms

============================================================
Summary:
  Binary load median:      ~8.5 ms
  Server ready median:     ~53.6 ms
  Bridge total median:     ~42.5 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f436a434a010)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-6mxceo2y
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.026/0.023    p95    0.037/0.032    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.271/0.225    p95    0.409/0.305    delta  -0.046 ms  control-no-change
  mcp_config_load   p50    0.291/0.270    p95    0.369/0.420    delta  -0.020 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.011    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   16.583/17.568   p95   25.075/36.662   delta  +0.985 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22958 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22958 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    3.998/3.945    p95    6.686/4.969    delta  -0.053 ms  control-no-change
  skill_request_overlay p50    5.217/5.248    p95    8.133/6.760    delta  +0.032 ms  control-no-change
  mcp_config_load   p50    0.267/0.255    p95    0.300/0.292    delta  -0.013 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   16.386/16.055   p95   21.850/20.402   delta  -0.331 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22938..22958 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22954 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.026/0.024    p95    0.032/0.027    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.227/0.218    p95    0.394/0.240    delta  -0.009 ms  control-no-change
  mcp_config_load   p50    0.271/0.251    p95    0.305/0.286    delta  -0.020 ms  control-no-change
  attach_history    p50  272.718/272.927  p95  311.657/297.349  delta  +0.209 ms  control-no-change
  resume_ready      p50   58.544/59.229   p95   64.689/61.883   delta  +0.684 ms  control-no-change
  provider_dispatch p50    1.028/0.992    p95    1.069/1.099    delta  -0.036 ms  control-no-change
  session_save      p50   14.607/13.426   p95   30.609/15.573   delta  -1.181 ms  control-no-change
  total store_read_bytes p50 5311304  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22939..22964 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22964 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.101/4.053    p95    4.466/4.252    delta  -0.048 ms  control-no-change
  skill_request_overlay p50    5.375/5.290    p95    5.988/5.728    delta  -0.084 ms  control-no-change
  mcp_config_load   p50    0.281/0.257    p95    0.328/0.324    delta  -0.023 ms  control-no-change
  attach_history    p50  268.804/267.525  p95  287.625/294.072  delta  -1.279 ms  control-no-change
  resume_ready      p50   58.072/57.797   p95   78.413/60.438   delta  -0.276 ms  control-no-change
  provider_dispatch p50    1.022/0.984    p95    1.148/1.124    delta  -0.038 ms  control-no-change
  session_save      p50   14.994/14.154   p95   19.567/20.852   delta  -0.841 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22946..22968 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22962 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.026/0.027    p95    0.043/0.126    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.250/0.240    p95    0.451/0.465    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.277/0.285    p95    0.594/0.491    delta  +0.008 ms  control-no-change
  attach_history    p50 3662.543/3849.519 p95 4104.020/4149.639 delta +186.976 ms  control-no-change
  resume_ready      p50  738.442/806.200  p95  926.459/1017.512 delta +67.759 ms  control-no-change
  provider_dispatch p50   10.548/10.256   p95   12.987/22.668   delta  -0.292 ms  control-no-change
  session_save      p50   17.633/20.231   p95   26.211/32.746   delta  +2.599 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184303 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210253..11210254 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22954..22970 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184297..42184303 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210250..11210254 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22970 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.714/5.421    p95    6.779/8.341    delta  +0.707 ms  control-no-change
  skill_request_overlay p50    7.044/6.395    p95    9.704/7.912    delta  -0.650 ms  control-no-change
  mcp_config_load   p50    0.316/0.280    p95    0.417/0.592    delta  -0.035 ms  control-no-change
  attach_history    p50 4449.365/4324.827 p95 5318.989/5516.880 delta -124.538 ms  control-no-change
  resume_ready      p50  833.753/900.726  p95 1155.326/1351.762 delta +66.973 ms  control-no-change
  provider_dispatch p50   14.272/13.070   p95   16.590/18.841   delta  -1.202 ms  control-no-change
  session_save      p50   32.688/29.691   p95  202.746/38.568   delta  -2.997 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22942..22971 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22967 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f436a434a010)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-em7vxqq7
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.029/0.026    p95    0.055/0.083    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.234/0.247    p95    0.526/0.642    delta  +0.013 ms  control-no-change
  mcp_config_load   p50    0.251/0.266    p95    0.809/0.740    delta  +0.015 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   28.287/24.621   p95   59.804/36.711   delta  -3.666 ms  control-no-change
  total store_read_bytes p50 23656  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22933, 22945, 22947, 23653, 23659, 23665, 24379, 24385, 24387]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22941, 22951, 22959, 23653, 23655, 23661, 24359, 24371, 24377, 24379]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22933..24387 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22941..24379 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    5.017/4.143    p95   11.340/7.004    delta  -0.874 ms  control-no-change
  skill_request_overlay p50    6.548/6.468    p95   10.067/12.818   delta  -0.080 ms  control-no-change
  mcp_config_load   p50    0.286/0.290    p95    0.845/1.349    delta  +0.004 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.004    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.007/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   20.859/20.621   p95   27.914/30.088   delta  -0.239 ms  control-no-change
  total store_read_bytes p50 23659  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22939, 22943, 22947, 23653, 23657, 23661, 24355, 24367, 24375, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22937, 22943, 22945, 22947, 23663, 23669, 24373, 24383, 24385]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22939..24383 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22937..24385 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.032/0.025    p95    0.065/0.034    delta  -0.006 ms  control-no-change
  skill_request_overlay p50    0.222/0.225    p95    0.593/0.522    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    0.269/0.257    p95    0.508/0.658    delta  -0.012 ms  control-no-change
  attach_history    p50  115.452/125.811  p95  135.075/159.758  delta +10.359 ms  control-no-change
  resume_ready      p50   65.536/73.013   p95   78.579/91.736   delta  +7.477 ms  control-no-change
  provider_dispatch p50    1.079/1.131    p95    1.648/1.886    delta  +0.051 ms  control-no-change
  session_save      p50   19.025/20.124   p95   21.886/27.791   delta  +1.099 ms  control-no-change
  total store_read_bytes p50 2841257  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23681, 23689, 24379, 24391, 25101, 25803, 25819, 26521, 26533]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23677, 24383, 25095, 25801, 25817, 26527, 26535]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23681..26533 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26535 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.796/4.221    p95    7.036/5.514    delta  -0.576 ms  control-no-change
  skill_request_overlay p50    6.319/5.461    p95    9.660/9.592    delta  -0.858 ms  control-no-change
  mcp_config_load   p50    0.277/0.281    p95    0.336/0.402    delta  +0.004 ms  control-no-change
  attach_history    p50  117.361/115.078  p95  132.266/140.746  delta  -2.283 ms  control-no-change
  resume_ready      p50   85.548/88.453   p95  103.916/119.599  delta  +2.905 ms  control-no-change
  provider_dispatch p50    1.100/1.087    p95    2.597/2.317    delta  -0.013 ms  control-no-change
  session_save      p50   22.356/27.572   p95   51.744/55.040   delta  +5.216 ms  control-no-change
  total store_read_bytes p50 2841253  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23673, 24379, 24387, 25085, 25109, 25811, 25823, 26509, 26533]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23673, 23677, 24387, 24399, 25091, 25101, 25805, 25811, 26519, 26525]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26533 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23673..26525 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.028/0.031    p95    0.039/0.137    delta  +0.004 ms  control-no-change
  skill_request_overlay p50    0.222/0.243    p95    0.309/1.091    delta  +0.021 ms  control-no-change
  mcp_config_load   p50    0.259/0.273    p95    0.497/1.072    delta  +0.015 ms  control-no-change
  attach_history    p50 1295.644/1260.328 p95 1602.918/1522.567 delta -35.316 ms  control-no-change
  resume_ready      p50  776.192/746.050  p95  911.933/957.812  delta -30.141 ms  control-no-change
  provider_dispatch p50   10.271/10.697   p95   13.825/20.547   delta  +0.425 ms  control-no-change
  session_save      p50   19.029/19.748   p95   29.056/32.379   delta  +0.718 ms  control-no-change
  total store_read_bytes p50 28640271  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23693, 24391, 25095, 25097, 25805, 25823, 26529, 26531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23673, 23677, 24379, 24387, 25105, 25117, 25811, 25819, 26529, 26533]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26531 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23673..26533 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.661/4.601    p95    9.315/9.101    delta  -0.059 ms  control-no-change
  skill_request_overlay p50    6.260/5.757    p95    9.217/8.455    delta  -0.503 ms  control-no-change
  mcp_config_load   p50    0.304/0.280    p95    0.734/0.961    delta  -0.024 ms  control-no-change
  attach_history    p50 1386.890/1473.374 p95 1844.619/1906.460 delta +86.484 ms  control-no-change
  resume_ready      p50  915.299/853.018  p95 1214.425/1099.496 delta -62.281 ms  control-no-change
  provider_dispatch p50   14.638/13.470   p95   24.962/18.914   delta  -1.168 ms  control-no-change
  session_save      p50   30.329/25.323   p95  108.969/144.492  delta  -5.006 ms  control-no-change
  total store_read_bytes p50 28640262  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23663, 23685, 24377, 24395, 25081, 25093, 25815, 26533, 26545]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23661, 23669, 24377, 24391, 25095, 25103, 25809, 25813, 26515, 26519]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23663..26545 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23661..26519 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.5 ms | 0.8 ms | 0.3 ms | 0.8 ms | 5 |
| read (1000 lines) | 0.6 ms | 1.0 ms | 0.5 ms | 1.0 ms | 5 |
| read (10 lines @ offset) | 0.8 ms | 1.3 ms | 0.5 ms | 1.3 ms | 5 |
| read (repo Cargo.toml) | 0.5 ms | 0.8 ms | 0.4 ms | 0.8 ms | 5 |
| write (small) | 3.5 ms | 7.0 ms | 2.8 ms | 7.0 ms | 5 |
| write (10KB) | 3.5 ms | 4.0 ms | 3.0 ms | 4.0 ms | 5 |
| edit (simple replace) | 3.2 ms | 3.6 ms | 2.2 ms | 3.6 ms | 5 |
| ls (scripts dir) | 0.9 ms | 2.3 ms | 0.8 ms | 2.3 ms | 5 |
| agentgrep (grep in tree) | 4.2 ms | 6.0 ms | 3.5 ms | 6.0 ms | 5 |
| agentgrep (find **/*.rs) | 2.1 ms | 2.2 ms | 1.9 ms | 2.2 ms | 5 |
| bash (true) | 6.7 ms | 9.3 ms | 6.4 ms | 9.3 ms | 5 |
| bash (echo hello) | 6.6 ms | 11.4 ms | 5.7 ms | 11.4 ms | 5 |
| bash (ls fixture tree) | 10.2 ms | 15.4 ms | 9.7 ms | 15.4 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 68.4 ms | 68.4 ms | 68.4 ms | 68.5 ms | 0 |
| 10 | 131.2 ms | 131.9 ms | 131.9 ms | 132.3 ms | 0 |
| 40 | 519.5 ms | 522.8 ms | 523.7 ms | 524.5 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 990.4 ms | 990.4 ms | 990.4 ms | 1 |
| leaf_touch | src/main.rs | 641.6 ms | 641.6 ms | 641.6 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 25.44 s | 25.44 s | 25.44 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 39.31 s | 39.31 s | 39.31 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (f7a53f1) | 11.6 ms | 96.9 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 14.1 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 33.7 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 75.6 ms | 479.4 MB |
| OpenCode | 1.19.5 | 387.6 ms | 605.3 MB |
| pi | 1.5.0 | 197.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 500.1 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 146.8 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 118.3 ms | 117.2 ms–170.9 ms | baseline | 35.3 MB | baseline | yes | 3,686 / 2,390 (1 tools) | — |
| jcode | 129.3 ms | 117.9 ms–167.8 ms | 1.09× more | 30.8 MB | 1.15× less | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.09 s | 1.03 s–1.55 s | 9.25× more | 282.8 MB | 8.01× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 1.10 s | 1.07 s–1.41 s | 9.31× more | 265.9 MB | 7.53× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 3.49 s | 3.32 s–6.11 s | 29.51× more | 795.7 MB | 22.54× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 334.3 ms | 325.6 ms–360.2 ms | 2.83× more | 124.8 MB | 3.54× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.83 s | 1.73 s–1.98 s | 15.49× more | 378.4 MB | 10.72× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 14.66 s | 11.56 s–15.07 s | 123.91× more | 566.5 MB | 16.04× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 119.0 ms | 115.5 ms–147.5 ms | baseline | 34.4 MB | baseline | — | yes | — | 11,209 / 9,255 (8 tools) |
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
| horon | 129.3 ms | 120.6 ms–140.7 ms | baseline | 24.6 MB | baseline | skill-uri | yes | — |
| jcode | 153.1 ms | 147.2 ms–164.8 ms | 1.18× more | 23.6 MB | 1.04× less | skill-tool | yes | — |
| Claude Code | 658.9 ms | 633.7 ms–663.0 ms | 5.10× more | 237.5 MB | 9.63× more | skill-tool | yes | — |
| Codex CLI | 1.39 s | 1.35 s–1.49 s | 10.72× more | 263.2 MB | 10.68× more | file-read | yes | — |
| OpenCode | 5.29 s | 5.22 s–5.39 s | 40.94× more | 699.4 MB | 28.38× more | skill-tool | yes | — |
| pi | 498.2 ms | 444.5 ms–593.0 ms | 3.85× more | 97.7 MB | 3.96× more | file-read | yes | — |
| oh-my-pi (omp) | 2.51 s | 2.29 s–2.66 s | 19.42× more | 363.2 MB | 14.73× more | file-read | yes | — |
| oh-my-openagent (omo) | 15.04 s | 12.67 s–15.31 s | 116.36× more | 549.0 MB | 22.27× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 161.3 ms | 149.6 ms–189.5 ms | baseline | 37.4 MB | baseline | mcp-call | yes | 104.3 ms | — |
| jcode | 257.9 ms | 255.0 ms–261.4 ms | 1.60× more | 36.4 MB | 1.03× less | mcp-action | yes | 188.6 ms | — |
| Claude Code | 701.0 ms | 671.3 ms–767.5 ms | 4.35× more | 278.9 MB | 7.45× more | mcp-tool | yes | 585.7 ms | — |
| Codex CLI | 622.0 ms | 590.2 ms–658.9 ms | 3.86× more | 176.6 MB | 4.72× more | mcp-tool | yes | 455.4 ms | — |
| OpenCode | 5.52 s | 5.08 s–5.71 s | 34.23× more | 740.5 MB | 19.78× more | mcp-tool | yes | 4.30 s | — |
| pi | 524.4 ms | 513.4 ms–560.7 ms | 3.25× more | 117.5 MB | 3.14× more | mcp-tool | yes | 420.8 ms | — |
| oh-my-pi (omp) | 2.46 s | 2.15 s–2.57 s | 15.23× more | 360.5 MB | 9.63× more | mcp-route | yes | 2.06 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.4 MB | 125.8 KB | baseline | — | — | 30.1 MB | 395.2 KB |
| jcode | 22.1 MB | 54.7 KB | 2.30× less | — | — | 27.3 MB | 310.8 KB |
| Claude Code | 155.1 MB | 164.0 MB | 1,335× more | — | — | 189.7 MB | 189.2 MB |
| Codex CLI | 136.2 MB | 69.7 MB | 567.09× more | — | — | 172.6 MB | 92.7 MB |
| OpenCode | 245.6 MB | 0.0 B | — | — | — | 429.8 MB | 9.9 MB |
| pi | 91.0 MB | 90.4 MB | 736.39× more | — | — | 101.1 MB | 100.8 MB |
| oh-my-pi (omp) | 318.9 MB | 144.1 MB | 1,173× more | — | — | 337.4 MB | 340.2 MB |
| oh-my-openagent (omo) | 147.7 MB | 133.0 MB | 1,083× more | — | — | 414.6 MB | 403.0 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 30.1 MB | 32.1 MB | 33.6 MB |
| jcode | 27.3 MB | 28.8 MB | 30.1 MB |
| Claude Code | 189.7 MB | 943.2 MB | 1.8 GB |
| Codex CLI | 172.6 MB | 3.8 GB | 1.2 GB |
| OpenCode | 429.8 MB | 479.4 MB | 519.8 MB |
| pi | 101.1 MB | 505.9 MB | 1008.7 MB |
| oh-my-pi (omp) | 337.4 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 414.6 MB | 2.1 GB | 4.0 GB |

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    12.22 ms
  Max:    13.69 ms
  Mean:   12.96 ms
  Median: 12.86 ms
  Stdev:  0.46 ms

--version:
  Min:    11.88 ms
  Max:    13.44 ms
  Mean:   12.73 ms
  Median: 12.90 ms
  Stdev:  0.48 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.15 ms
  Max:    27.62 ms
  Mean:   22.89 ms
  Median: 22.33 ms
  Stdev:  1.67 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.5       1.5   24.2%
  nofile_limit        2.0       0.5    8.6%
  perm_harden         4.1       2.1   34.6%
  args_parse          4.1       0.0    0.1%
  config_loaded       4.9       0.8   13.6%
  server_new          5.7       0.8   13.0%
  socket_bound        6.1       0.4    5.9%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~12.9 ms
  Server ready median:     ~22.3 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6aa168e7cec1)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-0bgyj_lj
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.103/0.099    p95    0.118/0.138    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.748/0.798    p95    0.795/0.898    delta  +0.050 ms  control-no-change
  mcp_config_load   p50    0.889/0.924    p95    0.950/0.999    delta  +0.035 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  session_save      p50   48.307/48.513   p95   53.172/50.460   delta  +0.205 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22900..22968 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22924..22960 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   14.043/14.229   p95   15.001/14.871   delta  +0.187 ms  control-no-change
  skill_request_overlay p50   37.460/37.515   p95   51.863/39.975   delta  +0.055 ms  control-no-change
  mcp_config_load   p50    0.957/0.996    p95    1.039/1.102    delta  +0.039 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   57.902/55.471   p95   84.685/69.575   delta  -2.431 ms  control-no-change
  total store_read_bytes p50 22928  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22912..22960 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22918..22968 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.102/0.099    p95    0.120/0.125    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.783/0.745    p95    0.875/0.827    delta  -0.038 ms  control-no-change
  mcp_config_load   p50    0.921/0.927    p95    0.979/1.015    delta  +0.006 ms  control-no-change
  attach_history    p50  418.354/419.514  p95  425.673/431.078  delta  +1.160 ms  control-no-change
  resume_ready      p50  146.144/147.838  p95  155.202/153.827  delta  +1.694 ms  control-no-change
  provider_dispatch p50    1.454/1.503    p95    1.520/1.542    delta  +0.049 ms  control-no-change
  session_save      p50   46.148/48.427   p95  130.106/169.050  delta  +2.278 ms  control-no-change
  total store_read_bytes p50 5311205  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22912..22972 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22945..22976 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.457/15.388   p95   17.345/15.940   delta  -0.069 ms  control-no-change
  skill_request_overlay p50   37.819/37.349   p95   39.589/38.464   delta  -0.469 ms  control-no-change
  mcp_config_load   p50    0.967/1.028    p95    1.089/1.095    delta  +0.062 ms  control-no-change
  attach_history    p50  425.963/424.444  p95  445.904/584.001  delta  -1.519 ms  control-no-change
  resume_ready      p50  147.713/151.428  p95  150.750/156.018  delta  +3.715 ms  control-no-change
  provider_dispatch p50    1.426/1.428    p95    1.493/1.490    delta  +0.002 ms  control-no-change
  session_save      p50   52.289/57.065   p95  362.047/167.725  delta  +4.776 ms  control-no-change
  total store_read_bytes p50 5311211  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22980 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22974 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.099/0.100    p95    0.140/0.108    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.759/0.791    p95    0.850/0.842    delta  +0.032 ms  control-no-change
  mcp_config_load   p50    0.957/0.952    p95    1.051/1.016    delta  -0.005 ms  control-no-change
  attach_history    p50 4854.741/4837.733 p95 4940.935/4938.968 delta -17.008 ms  control-no-change
  resume_ready      p50 1556.616/1561.627 p95 1569.210/1572.143 delta  +5.011 ms  control-no-change
  provider_dispatch p50   15.977/15.956   p95   17.259/18.007   delta  -0.021 ms  control-no-change
  session_save      p50   44.647/43.412   p95   52.325/100.441  delta  -1.235 ms  control-no-change
  total store_read_bytes p50 53417422  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22918..22979 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184247..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210210..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22979 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.967/15.033   p95   15.644/15.610   delta  +0.066 ms  control-no-change
  skill_request_overlay p50   37.967/38.913   p95   39.723/42.377   delta  +0.945 ms  control-no-change
  mcp_config_load   p50    0.990/1.001    p95    1.171/1.050    delta  +0.011 ms  control-no-change
  attach_history    p50 4874.945/4895.914 p95 5150.881/5226.864 delta +20.969 ms  control-no-change
  resume_ready      p50 1550.243/1553.934 p95 1570.747/1576.762 delta  +3.691 ms  control-no-change
  provider_dispatch p50   16.545/16.588   p95   16.896/18.250   delta  +0.043 ms  control-no-change
  session_save      p50   44.705/45.314   p95   60.038/46.421   delta  +0.609 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22927..22979 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22975 (6 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6aa168e7cec1)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-6g3tov2o
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.151/0.151    p95    0.178/0.154    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    1.241/1.214    p95    1.285/1.279    delta  -0.027 ms  control-no-change
  mcp_config_load   p50    1.391/1.370    p95    1.489/1.582    delta  -0.021 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.017/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   62.133/63.450   p95   69.695/86.990   delta  +1.316 ms  control-no-change
  total store_read_bytes p50 23623  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22937, 22939, 22951, 22953, 23620, 23626, 23646, 23650, 24337, 24347]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22921, 22933, 22961, 22962, 23635, 23648, 23668, 24315, 24338, 24340]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22937..24347 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22921..24340 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   23.170/23.589   p95   25.289/24.823   delta  +0.419 ms  control-no-change
  skill_request_overlay p50   51.694/51.406   p95   58.909/53.115   delta  -0.288 ms  control-no-change
  mcp_config_load   p50    1.420/1.418    p95    1.651/1.728    delta  -0.001 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.008/0.010    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.002    p95    0.002/0.002    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   70.912/69.448   p95  117.305/81.727   delta  -1.464 ms  control-no-change
  total store_read_bytes p50 23615  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22933, 22935, 22941, 22951, 23588, 23642, 23658, 24305, 24341, 24349]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22921, 22945, 22950, 22953, 23616, 23631, 23634, 24325, 24328, 24351]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22933..24349 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22921..24351 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.153/0.151    p95    0.180/0.185    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    1.187/1.222    p95    1.252/1.309    delta  +0.036 ms  control-no-change
  mcp_config_load   p50    1.401/1.384    p95    1.439/1.513    delta  -0.018 ms  control-no-change
  attach_history    p50  286.002/292.979  p95  297.502/303.960  delta  +6.977 ms  control-drift
  resume_ready      p50  228.359/227.495  p95  231.555/234.158  delta  -0.864 ms  control-no-change
  provider_dispatch p50    1.736/1.736    p95    1.794/1.857    delta  -0.000 ms  control-no-change
  session_save      p50   50.357/55.178   p95   62.136/116.227  delta  +4.821 ms  control-no-change
  total store_read_bytes p50 2841116  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23670, 24301, 24362, 25026, 25049, 25737, 25756, 26419, 26431]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23629, 23654, 24342, 24349, 25056, 25066, 25738, 25765, 26428, 26459]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26431 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23629..26459 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   22.868/23.275   p95   27.363/25.485   delta  +0.407 ms  control-no-change
  skill_request_overlay p50   52.509/52.516   p95   55.319/57.967   delta  +0.006 ms  control-no-change
  mcp_config_load   p50    1.437/1.445    p95    1.643/1.526    delta  +0.008 ms  control-no-change
  attach_history    p50  282.863/286.781  p95  290.642/292.816  delta  +3.918 ms  control-no-change
  resume_ready      p50  225.793/226.124  p95  231.874/253.495  delta  +0.332 ms  control-no-change
  provider_dispatch p50    1.708/1.700    p95    1.793/1.746    delta  -0.009 ms  control-no-change
  session_save      p50   58.917/56.104   p95   66.979/69.787   delta  -2.813 ms  control-no-change
  total store_read_bytes p50 2841132  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23636, 23659, 24299, 24350, 25052, 25722, 25735, 26434, 26446]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23643, 23671, 24350, 24358, 25064, 25069, 25751, 25768, 26446, 26463]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23636..26446 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23643..26463 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.149/0.152    p95    0.165/0.232    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    1.212/1.192    p95    1.244/1.370    delta  -0.021 ms  control-no-change
  mcp_config_load   p50    1.390/1.415    p95    1.560/1.451    delta  +0.025 ms  control-no-change
  attach_history    p50 2989.747/2988.705 p95 3016.215/2998.920 delta  -1.042 ms  control-no-change
  resume_ready      p50 2379.000/2375.611 p95 2399.728/2390.873 delta  -3.389 ms  control-no-change
  provider_dispatch p50   17.717/17.370   p95   18.944/17.782   delta  -0.347 ms  control-no-change
  session_save      p50   54.600/51.702   p95   58.401/61.836   delta  -2.899 ms  control-no-change
  total store_read_bytes p50 28640148  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23673, 24360, 24365, 25032, 25070, 25709, 25745, 26420, 26439]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23615, 23668, 24352, 24355, 25019, 25023, 25723, 25734, 26420, 26461]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26439 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23615..26461 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   22.568/22.882   p95   25.366/24.375   delta  +0.315 ms  control-no-change
  skill_request_overlay p50   52.075/51.838   p95   54.730/52.912   delta  -0.237 ms  control-no-change
  mcp_config_load   p50    1.425/1.416    p95    1.822/1.523    delta  -0.009 ms  control-no-change
  attach_history    p50 2979.453/3007.255 p95 3453.738/3314.385 delta +27.802 ms  control-no-change
  resume_ready      p50 2377.476/2379.342 p95 2609.232/2801.723 delta  +1.866 ms  control-no-change
  provider_dispatch p50   17.466/17.673   p95   17.805/17.941   delta  +0.207 ms  control-no-change
  session_save      p50   52.548/54.254   p95  131.710/76.524   delta  +1.707 ms  control-no-change
  total store_read_bytes p50 28640136  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23650, 23658, 24357, 24365, 25046, 25048, 25765, 25769, 26436, 26456]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23662, 23666, 24337, 25034, 25046, 25735, 25777, 26424, 26430]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23650..26456 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23662..26430 (9 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| write (small) | 7.8 ms | 10.1 ms | 7.5 ms | 10.1 ms | 5 |
| write (10KB) | 12.8 ms | 25.9 ms | 7.6 ms | 25.9 ms | 5 |
| edit (simple replace) | 10.2 ms | 12.8 ms | 9.8 ms | 12.8 ms | 5 |
| ls (scripts dir) | 2.3 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| agentgrep (grep in tree) | 7.5 ms | 7.6 ms | 7.5 ms | 7.6 ms | 5 |
| agentgrep (find **/*.rs) | 7.5 ms | 8.1 ms | 7.5 ms | 8.1 ms | 5 |
| bash (true) | 12.8 ms | 13.2 ms | 12.4 ms | 13.2 ms | 5 |
| bash (echo hello) | 12.7 ms | 12.8 ms | 12.3 ms | 12.8 ms | 5 |
| bash (ls fixture tree) | 15.4 ms | 15.5 ms | 15.4 ms | 15.5 ms | 5 |
| invalid tool (dispatch only) | 2.8 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 161.7 ms | 161.7 ms | 161.7 ms | 161.8 ms | 0 |
| 10 | 444.2 ms | 445.5 ms | 445.5 ms | 447.0 ms | 0 |
| 40 | 1.60 s | 1.84 s | 1.84 s | 1.84 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 620.0 ms | 620.0 ms | 620.0 ms | 1 |
| leaf_touch | src\main.rs | 857.8 ms | 857.8 ms | 857.8 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 23.57 s | 23.57 s | 23.57 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 33.46 s | 33.46 s | 33.46 s | 1 |

## TUI (`horon-tui`)

The interactive client is a second binary and is measured on its own. These numbers come from the `tui` suite and are never compared with the headless tables above: `first_frame_ms` and `direct_first_frame_ms` are two launches of two different programs, and the handoff is the difference between them.

![TUI (`horon-tui`)](charts/tui-01-latency.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 30.7 ms | 28.0 ms–41.7 ms | 5 |
| First frame (direct `horon-tui`) | 26.9 ms | 26.2 ms–39.0 ms | 5 |
| TTY handoff cost | 4.5 ms | -10.3 ms–14.8 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–1.2 ms | 50 |
| Idle RSS after 5 s | 5.5 MB | 5.4 MB–5.5 MB | 5 |
| RSS after one turn | 5.7 MB | 5.6 MB–5.8 MB | 5 |
| Frame draw time (p50) | 1.5 ms | 1.5 ms–1.5 ms | 5 |
| Frame draw time (p95) | 1.6 ms | 1.5 ms–2.1 ms | 5 |
| Install size (`horon-tui`) | 6.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 102.9 ms | 73.9 ms–343.5 ms | 5 |
| First frame (direct `horon-tui`) | 102.8 ms | 82.1 ms–134.8 ms | 5 |
| TTY handoff cost | -11.2 ms | -27.6 ms–261.4 ms | 5 |
| Key press to echo (p95) | 0.3 ms | 0.2 ms–0.7 ms | 50 |
| Idle RSS after 5 s | 8.9 MB | 8.7 MB–9.0 MB | 5 |
| RSS after one turn | 9.1 MB | 9.0 MB–9.2 MB | 5 |
| Frame draw time (p50) | 1.2 ms | 1.2 ms–1.5 ms | 5 |
| Frame draw time (p95) | 3.2 ms | 3.1 ms–3.3 ms | 5 |
| Install size (`horon-tui`) | 5.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

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
| Frame draw time (p50) | 2.1 ms | 2.0 ms–2.2 ms | 5 |
| Frame draw time (p95) | 2.3 ms | 2.1 ms–2.3 ms | 5 |
| Install size (`horon-tui`) | 5.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
