# Performance: measured results and their limits

Language: **English** | [한국어](RESULTS.ko.md)

This document records the measurements the `agent-runtime-performance-2` plan left on baseline B0 (the commit immediately before the plan started). The plan landed 19 performance commits on `dev`, and the same range holds one `fix(cli)` commit unrelated to this plan that has no bearing on the numbers below. The measured surfaces are the session store's hashes and history pages, the SSE parser, replication export, the result cache's threshold decision, and the attach/detach soak. Every measurement was taken on this machine (Apple M1 Max, debug test profile) by alternating before and after on the same machine.

All raw artifacts live under `.omo/evidence/agent-runtime-performance-2/`, a path ignored by `.gitignore:26`. The plan asked for the key numbers to be copied into the PR bodies, but in practice most PR bodies were one line. So anyone who clones this repository today cannot see a single measurement from this work. This page moves that record somewhere tracked; every number is copied verbatim from the evidence files above.

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
| Idle session eviction policy | not adopted | 0.999 fds and 0.998 threads per session, 0.41 MB RSS. The slope does not bend from 1 to 1,000 sessions. The debug job count (`/debug/job_count`) is 0 at all five population counts, and the soak returns the session count to 0 |
| `globalEvents` multiplexing | not adopted (protocol change) | 7.4 consumer-side fds and 3.0 threads per watched session. The daemon's own share is 1.0 connection per session, and the bridge attaches one session per connection |
| Narrowing the session map lock scope | not adopted | Removing an unrelated session waits for the whole import. 861 / 5,561 / 11,255 / 24,499 ms at 1 / 20 / 60 / 150 events, ratio 1.00 |
| Replacing the 25 ms interrupt poll timer with notification | not adopted | It would save a worst case of 29.7 ms (27.6 ms on average) at the cost of restructuring a wakeup once per watch-group round |
| Overlapping the `run.stopped` journal write with the close-save (#144, #129) | not adopted (slower) | Windows last request → exit, median against `dev` in the same job: 39.8 → 56.4 ms over 5 samples (run 42759006826) and 35.2 → 42.8 ms over 20 samples (run 42764256757; IQR 33.0–40.9 vs 40.7–45.5). Linux and macOS were ties. #146 later removed the close-save flush itself, which brought the same span to 32.5 ms |
| Resolving the fixed Windows shell environment once per process (#124, #122) | not adopted (tie) | Tool round-trip span on Windows, 20 samples: 74.3 → 73.5 ms (run 42735692366), inside the min–max range. Nearly all of the round trip is the `cmd.exe` start, not the per-call env table or PATH search |

Rejecting something on numbers is not a failure. The plan defined success as "when no improvement shows up, the item ends as not adopted with its reasoning attached. That counts as success too." The last column above is that reasoning.

## What these numbers do not say

This section is the point of this page. Leave out any one of the statements below and the page leads the reader to the wrong conclusion.

- **The source hash improvement covers the hashing stage only.** The store path still reads the whole log. `crates/base/src/session/event_commit.rs:31` reads the log whole with `std::fs::read` to validate the prefix, so the peak allocation of a single save keeps growing with the log (156,518 B for a 7 KB log, 8,293,580 B for a 553 KB log). "The peak allocation of saving a 200 MiB log is 65 KB" is false.
- **The history page improvement covers repeated paging while the head does not move.** The index is tied to the log's identity stamp (device, inode, length, mtime, ctime), so a single append, import or reset invalidates it. A session that keeps growing therefore pays the old cost once more and rebuilds the index. That rebuild cost was not measured. A single page fetch from a new head is not faster.
- **The cache improvement is allocation, not throughput.** Wall clock is about 19 ms per MiB on both sides. Escaping cost dominates either way.
- **The soak asserts fds, threads, queues, pending requests and tasks. Not RSS.** RSS rose from 68.4 MB to 79.5 MB and then stayed flat (peak 83.7 MB), and is recorded as an observation only.
- **The 200-cycle soak resolves one leaked resource per 20 cycles.** The plan asked for 1,000 cycles; that form now runs weekly in `Attach/Detach Soak` (`.github/workflows/soak.yml`) and its first run was clean, while the 200-cycle gate in `ci.yml` stays the pull-request form.
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
| 15. Attach/detach soak | `[x]` | The 1,000-cycle plan item now runs: `.github/workflows/soak.yml` runs it weekly and on demand, and the first run completed 1,000 cycles clean in 7 min 38 s (`fd_retention=0`, `thread_retention=0`, `non_returning=0`, run 43600180217). The 200-cycle form stays the pull-request gate |
| 17. CI structural gate and report template | `[~]` | The gate really runs (35 bounds, `EXIT=0`, `EXIT=1` on violation, `EXIT=2` when a measurement is missing, 40 contract tests). Two conditions from the plan remain. There is no frame count counter, and "passes at B0" is structurally impossible (the code that prints the `STRUCTURAL_PERF` line landed after B0). The gate now asserts the frame cap itself in its `frame` section for both readers, and still reads no frame count |

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

<!-- bench-auto:start commit=cdc8cb87238fdd8433dcf87824f9cca449d230ab -->

## Headless (`horon-agent`)

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `cdc8cb87238fdd8433dcf87824f9cca449d230ab`
- Measured at: 2026-11-15 17:30 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/43736928002> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (cdc8cb8)` | Success |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (cdc8cb8)` | Success |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (cdc8cb8)` | Success |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (cdc8cb8) | 4.0 ms | 115.7 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.6 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 10.6 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 43.9 ms | 435.0 MB |
| OpenCode | 1.19.5 | 466.2 ms | 613.4 MB |
| pi | 1.5.0 | 236.8 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 647.5 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 119.3 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 53.5 ms | 40.5 ms–63.2 ms | baseline | 39.8 MB | baseline | yes | 10 | 3,731 / 2,390 (1 tools) | — |
| jcode | 143.8 ms | 143.6 ms–242.8 ms | 2.69× more | 38.8 MB | 1.03× less | yes | 5 | 3,245 / 1,433 (1 tools) | — |
| Claude Code | 440.5 ms | 425.4 ms–476.4 ms | 8.23× more | 206.8 MB | 5.19× more | yes | — | 45,782 / 29,653 (12 tools) | — |
| Codex CLI | 497.0 ms | 471.6 ms–517.7 ms | 9.29× more | 203.6 MB | 5.11× more | yes | 6 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 2.68 s | 2.62 s–2.89 s | 50.13× more | 569.5 MB | 14.30× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 417.2 ms | 392.5 ms–425.7 ms | 7.80× more | 115.2 MB | 2.89× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.02 s | 1.80 s–2.26 s | 37.71× more | 395.6 MB | 9.94× more | yes | 71 | 29,860 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 6.07 s | 5.94 s–6.52 s | 113.37× more | 667.2 MB | 16.76× more | yes | 36 | 27,452 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 43.9 ms | 43.2 ms–44.5 ms | baseline | 42.0 MB | baseline | — | yes | — | 11,254 / 9,255 (8 tools) |
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
| horon | 36.8 ms | 36.2 ms–37.3 ms | baseline | 18.3 MB | baseline | skill-uri | yes | — |
| jcode | 57.3 ms | 56.3 ms–57.8 ms | 1.56× more | 40.6 MB | 2.22× more | skill-tool | yes | — |
| Claude Code | 433.3 ms | 422.3 ms–457.7 ms | 11.77× more | 207.8 MB | 11.35× more | skill-tool | yes | — |
| Codex CLI | 580.0 ms | 471.4 ms–592.1 ms | 15.75× more | 191.3 MB | 10.45× more | file-read | yes | — |
| OpenCode | 3.15 s | 3.09 s–3.18 s | 85.43× more | 596.1 MB | 32.55× more | skill-tool | yes | — |
| pi | 518.1 ms | 514.4 ms–528.9 ms | 14.07× more | 125.6 MB | 6.86× more | file-read | yes | — |
| oh-my-pi (omp) | 1.99 s | 1.98 s–2.05 s | 54.15× more | 377.6 MB | 20.62× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.63 s | 6.44 s–6.84 s | 179.99× more | 672.2 MB | 36.71× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 63.2 ms | 62.4 ms–64.1 ms | baseline | 45.3 MB | baseline | mcp-call | yes | 46.7 ms | — |
| jcode | 176.4 ms | 175.6 ms–189.8 ms | 2.79× more | 56.4 MB | 1.24× more | mcp-action | yes | 143.8 ms | — |
| Claude Code | 533.5 ms | 525.6 ms–552.6 ms | 8.44× more | 226.7 MB | 5.00× more | mcp-tool | yes | 390.3 ms | — |
| Codex CLI | 534.5 ms | 499.6 ms–616.9 ms | 8.46× more | 194.1 MB | 4.28× more | mcp-tool | yes | 360.5 ms | — |
| OpenCode | 2.93 s | 2.92 s–3.00 s | 46.33× more | 569.1 MB | 12.55× more | mcp-tool | yes | 2.67 s | — |
| pi | 574.9 ms | 556.4 ms–591.7 ms | 9.10× more | 138.9 MB | 3.06× more | mcp-tool | yes | 511.4 ms | — |
| oh-my-pi (omp) | 2.03 s | 2.00 s–2.08 s | 32.07× more | 393.2 MB | 8.67× more | mcp-route | yes | 1.70 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 37.3 MB | 204.7 KB | baseline | 0.45 | 1.00 | 44.8 MB | 509.6 KB |
| jcode | 42.6 MB | 91.4 KB | 2.24× less | 0.12 | — | 46.1 MB | 266.4 KB |
| Claude Code | 150.1 MB | 149.5 MB | 747.82× more | — | — | 186.0 MB | 181.2 MB |
| Codex CLI | 128.5 MB | 132.3 MB | 661.95× more | — | — | 160.7 MB | 164.3 MB |
| OpenCode | 243.7 MB | 526.6 KB | 2.57× more | — | — | 450.6 MB | 6.5 MB |
| pi | 106.4 MB | 108.6 MB | 543.33× more | — | — | 130.2 MB | 123.2 MB |
| oh-my-pi (omp) | 335.3 MB | 74.0 MB | 370.09× more | — | — | 365.3 MB | 357.5 MB |
| oh-my-openagent (omo) | 293.0 MB | 35.2 MB | 175.88× more | — | — | 586.6 MB | 579.4 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.8 MB | 47.1 MB | 49.3 MB |
| jcode | 46.1 MB | 47.3 MB | 48.5 MB |
| Claude Code | 186.0 MB | 918.2 MB | 1.8 GB |
| Codex CLI | 160.7 MB | 812.4 MB | 1.6 GB |
| OpenCode | 450.6 MB | 475.4 MB | 509.1 MB |
| pi | 130.2 MB | 627.2 MB | 1.2 GB |
| oh-my-pi (omp) | 365.3 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 586.6 MB | 2.9 GB | 5.7 GB |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.26 ms
  Max:    3.70 ms
  Mean:   3.46 ms
  Median: 3.42 ms
  Stdev:  0.14 ms

--version:
  Min:    3.18 ms
  Max:    3.43 ms
  Mean:   3.27 ms
  Median: 3.26 ms
  Stdev:  0.08 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.48 ms
  Max:    20.70 ms
  Mean:   20.55 ms
  Median: 20.54 ms
  Stdev:  0.06 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.3%
  logging_init        0.2       0.2   19.5%
  nofile_limit        0.5       0.2   22.6%
  perm_harden         0.5       0.0    1.3%
  args_parse          0.5       0.0    0.1%
  config_loaded       0.6       0.1   12.9%
  server_new          1.0       0.4   34.7%
  socket_bound        1.1       0.1    8.6%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.59 ms
  Max:    20.70 ms
  Mean:   20.66 ms
  Median: 20.67 ms
  Stdev:  0.04 ms

Bridge stage: hello_ok:
  Min:    21.17 ms
  Max:    21.45 ms
  Mean:   21.27 ms
  Median: 21.24 ms
  Stdev:  0.08 ms

Bridge stage: no-op reply:
  Min:    21.57 ms
  Max:    21.85 ms
  Mean:   21.65 ms
  Median: 21.62 ms
  Stdev:  0.09 ms

Bridge total (socket -> no-op):
  Min:    21.57 ms
  Max:    21.85 ms
  Mean:   21.65 ms
  Median: 21.62 ms
  Stdev:  0.09 ms

============================================================
Summary:
  Binary load median:      ~3.4 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.6 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (eb44aea698aa)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-n9en460v
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.007/0.007    p95    0.007/0.008    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.039/0.039    p95    0.040/0.040    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.055/0.055    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    4.577/4.560    p95    6.460/4.705    delta  -0.017 ms  control-no-change
  total store_read_bytes p50 23080  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23084 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23084 (4 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.196/1.196    p95    1.294/1.215    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    1.869/1.880    p95    1.907/1.933    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.061/0.060    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.043/6.109    p95    6.665/7.632    delta  +0.066 ms  control-no-change
  total store_read_bytes p50 23074  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23088 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23084 (4 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.006/0.006    p95    0.008/0.006    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.040/0.040    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.054/0.054    p95    0.067/0.064    delta  +0.000 ms  control-no-change
  attach_history    p50  189.888/189.412  p95  191.745/191.246  delta  -0.476 ms  control-no-change
  resume_ready      p50   27.242/27.197   p95   27.365/27.370   delta  -0.045 ms  control-no-change
  provider_dispatch p50    0.937/0.940    p95    0.956/0.957    delta  +0.003 ms  control-no-change
  session_save      p50    4.601/4.463    p95    5.144/4.680    delta  -0.138 ms  control-no-change
  total store_read_bytes p50 5311403  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23060..23082 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189195 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23090 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.199/1.200    p95    1.220/1.244    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    1.871/1.883    p95    1.899/1.926    delta  +0.012 ms  control-no-change
  mcp_config_load   p50    0.059/0.059    p95    0.066/0.061    delta  -0.001 ms  control-no-change
  attach_history    p50  192.895/192.808  p95  194.607/193.292  delta  -0.087 ms  control-no-change
  resume_ready      p50   27.379/27.398   p95   27.685/27.627   delta  +0.019 ms  control-no-change
  provider_dispatch p50    0.944/0.961    p95    0.966/0.978    delta  +0.018 ms  control-no-change
  session_save      p50    4.646/4.760    p95    5.063/4.945    delta  +0.114 ms  control-no-change
  total store_read_bytes p50 5311411  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23066..23089 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23090 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.052    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.055/0.054    p95    0.056/0.058    delta  -0.001 ms  control-no-change
  attach_history    p50 2444.677/2446.968 p95 2488.196/2485.743 delta  +2.291 ms  control-no-change
  resume_ready      p50  325.747/325.300  p95  329.878/329.312  delta  -0.447 ms  control-no-change
  provider_dispatch p50   11.207/11.225   p95   11.453/11.335   delta  +0.018 ms  control-no-change
  session_save      p50    4.956/4.830    p95    5.198/5.013    delta  -0.126 ms  control-no-change
  total store_read_bytes p50 53417616  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23075..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23085 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.195/1.203    p95    1.228/1.215    delta  +0.008 ms  control-no-change
  skill_request_overlay p50    1.836/1.857    p95    1.877/1.870    delta  +0.020 ms  control-no-change
  mcp_config_load   p50    0.059/0.059    p95    0.071/0.069    delta  +0.000 ms  control-no-change
  attach_history    p50 2408.976/2408.600 p95 2437.541/2445.675 delta  -0.376 ms  control-no-change
  resume_ready      p50  322.861/321.503  p95  325.835/325.520  delta  -1.358 ms  control-no-change
  provider_dispatch p50   11.108/11.116   p95   11.419/11.559   delta  +0.008 ms  control-no-change
  session_save      p50    4.795/4.847    p95    5.058/5.140    delta  +0.052 ms  control-no-change
  total store_read_bytes p50 53417618  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184287..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210240..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23073..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23093 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (eb44aea698aa)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-n85dep3_
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.008/0.008    p95    0.009/0.009    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.051/0.051    p95    0.071/0.052    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.074    p95    0.089/0.104    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  session_save      p50    6.140/6.270    p95    7.782/7.010    delta  +0.129 ms  control-no-change
  total store_read_bytes p50 23791  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23069, 23077, 23081, 23787, 23791, 24501, 24513, 24517]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23065, 23073, 23787, 23799, 24505, 24509, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23069..24517 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23065..24517 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    1.712/1.723    p95    2.323/2.031    delta  +0.011 ms  control-no-change
  skill_request_overlay p50    2.484/2.461    p95    2.608/2.632    delta  -0.023 ms  control-no-change
  mcp_config_load   p50    0.079/0.079    p95    0.082/0.098    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50    7.736/7.961    p95    8.677/8.387    delta  +0.225 ms  control-no-change
  total store_read_bytes p50 23799  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23075, 23077, 23085, 23789, 23795, 23803, 24495, 24505, 24507, 24517]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23053, 23069, 23073, 23787, 23791, 24497, 24505, 24511, 24521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23075..24517 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23053..24521 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.008/0.008    p95    0.010/0.009    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.052/0.052    p95    0.069/0.066    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.072    p95    0.105/0.074    delta  -0.001 ms  control-no-change
  attach_history    p50   76.474/76.569   p95   77.403/79.047   delta  +0.096 ms  control-no-change
  resume_ready      p50   37.206/37.106   p95   37.579/37.418   delta  -0.100 ms  control-no-change
  provider_dispatch p50    1.216/1.214    p95    1.249/1.236    delta  -0.001 ms  control-no-change
  session_save      p50    6.622/7.165    p95    7.014/14.778   delta  +0.543 ms  control-no-change
  total store_read_bytes p50 2841355  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23772, 23788, 24518, 25218, 25224, 25940, 25942, 26654, 26664]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23792, 23794, 24496, 24516, 25220, 25226, 25936, 25938, 26658, 26660]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23772..26664 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23792..26660 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.717/1.716    p95    1.740/1.731    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    2.451/2.430    p95    2.508/2.466    delta  -0.021 ms  control-no-change
  mcp_config_load   p50    0.077/0.078    p95    0.078/0.084    delta  +0.000 ms  control-no-change
  attach_history    p50   76.003/76.914   p95   76.975/77.791   delta  +0.911 ms  control-no-change
  resume_ready      p50   37.045/37.072   p95   37.502/37.368   delta  +0.027 ms  control-no-change
  provider_dispatch p50    1.192/1.203    p95    1.224/1.279    delta  +0.011 ms  control-no-change
  session_save      p50    8.032/8.031    p95    8.609/8.507    delta  -0.001 ms  control-no-change
  total store_read_bytes p50 2841358  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 24514, 24522, 25220, 25228, 25950, 25954, 26660, 26664]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23792, 23796, 24514, 24518, 25232, 25244, 25930, 25938, 26668]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26664 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23792..26668 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.008/0.008    p95    0.010/0.009    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.053/0.052    p95    0.077/0.076    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.072    p95    0.074/0.075    delta  -0.001 ms  control-no-change
  attach_history    p50  856.072/851.085  p95  859.650/862.664  delta  -4.987 ms  control-no-change
  resume_ready      p50  437.438/436.177  p95  446.950/439.177  delta  -1.261 ms  control-no-change
  provider_dispatch p50   14.302/14.297   p95   14.833/14.776   delta  -0.005 ms  control-no-change
  session_save      p50    6.654/6.690    p95    7.217/6.978    delta  +0.035 ms  control-no-change
  total store_read_bytes p50 28640392  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23799, 24517, 24525, 25239, 25243, 25957, 25961, 26667, 26679]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23787, 23803, 24513, 24517, 25231, 25235, 25937, 25965, 26667]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23799..26679 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23787..26667 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.741/1.695    p95    1.808/1.729    delta  -0.046 ms  control-no-change
  skill_request_overlay p50    2.425/2.430    p95    2.462/2.563    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.077/0.078    p95    0.079/0.083    delta  +0.001 ms  control-no-change
  attach_history    p50  857.397/853.750  p95  864.692/864.272  delta  -3.647 ms  control-no-change
  resume_ready      p50  443.696/446.835  p95  451.232/450.961  delta  +3.140 ms  control-no-change
  provider_dispatch p50   14.621/14.285   p95   16.448/14.951   delta  -0.336 ms  control-no-change
  session_save      p50    8.281/8.129    p95    9.174/8.731    delta  -0.152 ms  control-no-change
  total store_read_bytes p50 28640378  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23789, 23797, 24511, 24519, 25225, 25229, 25947, 25951, 26657, 26677]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23795, 24517, 24521, 25231, 25239, 25945, 25957, 26663, 26667]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23789..26677 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23795..26667 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 0.6 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| write (10KB) | 0.8 ms | 0.9 ms | 0.8 ms | 0.9 ms | 5 |
| edit (simple replace) | 0.7 ms | 0.9 ms | 0.6 ms | 0.9 ms | 5 |
| ls (scripts dir) | 0.5 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| agentgrep (grep in tree) | 1.2 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.0 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.3 ms | 1.7 ms | 1.3 ms | 1.7 ms | 5 |
| bash (echo hello) | 1.4 ms | 1.5 ms | 1.4 ms | 1.5 ms | 5 |
| bash (ls fixture tree) | 2.3 ms | 2.3 ms | 2.2 ms | 2.3 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.0 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 76.6 ms | 76.6 ms | 76.6 ms | 76.8 ms | 0 |
| 10 | 250.1 ms | 256.4 ms | 256.4 ms | 256.8 ms | 0 |
| 40 | 845.5 ms | 923.7 ms | 924.8 ms | 926.0 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 264.0 ms | 264.0 ms | 264.0 ms | 1 |
| leaf_touch | src/main.rs | 407.1 ms | 407.1 ms | 407.1 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 19.80 s | 19.80 s | 19.80 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 27.00 s | 27.00 s | 27.00 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (cdc8cb8) | 11.2 ms | 97.3 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.4 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 30.6 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 94.5 ms | 346.5 MB |
| OpenCode | 1.19.5 | 525.3 ms | 344.6 MB |
| pi | 1.5.0 | 378.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 1.08 s | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 198.4 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 92.2 ms | 81.3 ms–441.9 ms | baseline | 30.0 MB | baseline | yes | 3,619 / 2,390 (1 tools) | — |
| jcode | 516.6 ms | 469.0 ms–957.5 ms | 5.60× more | 34.0 MB | 1.13× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 773.9 ms | 597.6 ms–1.20 s | 8.39× more | 202.7 MB | 6.76× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 584.7 ms | 454.6 ms–1.80 s | 6.34× more | 127.6 MB | 4.25× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 3.20 s | 2.71 s–7.24 s | 34.73× more | 746.0 MB | 24.87× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 580.4 ms | 527.3 ms–738.7 ms | 6.29× more | 122.9 MB | 4.10× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.34 s | 2.25 s–4.33 s | 25.42× more | 441.2 MB | 14.71× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 13.11 s | 11.98 s–14.18 s | 142.18× more | 908.8 MB | 30.29× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 95.2 ms | 74.1 ms–181.2 ms | baseline | 31.2 MB | baseline | — | yes | — | 11,142 / 9,255 (8 tools) |
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
| horon | 90.6 ms | 73.4 ms–152.8 ms | baseline | 30.1 MB | baseline | skill-uri | yes | — |
| jcode | 482.5 ms | 449.9 ms–659.7 ms | 5.33× more | 37.4 MB | 1.24× more | skill-tool | yes | — |
| Claude Code | 764.3 ms | 660.5 ms–875.1 ms | 8.44× more | 185.0 MB | 6.14× more | skill-tool | yes | — |
| Codex CLI | 676.4 ms | 631.8 ms–766.4 ms | 7.47× more | 144.2 MB | 4.79× more | file-read | yes | — |
| OpenCode | 5.41 s | 4.64 s–7.19 s | 59.74× more | 798.2 MB | 26.50× more | skill-tool | yes | — |
| pi | 902.0 ms | 730.0 ms–928.0 ms | 9.96× more | 134.0 MB | 4.45× more | file-read | yes | — |
| oh-my-pi (omp) | 3.06 s | 2.54 s–3.47 s | 33.75× more | 456.9 MB | 15.17× more | file-read | yes | — |
| oh-my-openagent (omo) | 13.04 s | 12.77 s–17.26 s | 143.94× more | 875.2 MB | 29.05× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 151.4 ms | 120.8 ms–221.9 ms | baseline | 39.7 MB | baseline | mcp-call | yes | 108.6 ms | — |
| jcode | 557.3 ms | 510.3 ms–658.5 ms | 3.68× more | 52.4 MB | 1.32× more | mcp-action | yes | 493.6 ms | — |
| Claude Code | 710.1 ms | 565.2 ms–1.27 s | 4.69× more | 214.7 MB | 5.41× more | mcp-tool | yes | 561.8 ms | — |
| Codex CLI | 688.4 ms | 395.3 ms–839.1 ms | 4.55× more | 142.4 MB | 3.59× more | mcp-tool | yes | 533.6 ms | — |
| OpenCode | 5.16 s | 4.85 s–5.79 s | 34.06× more | 734.2 MB | 18.50× more | mcp-tool | yes | 4.39 s | — |
| pi | 940.3 ms | 809.2 ms–1.29 s | 6.21× more | 131.6 MB | 3.32× more | mcp-tool | yes | 825.5 ms | — |
| oh-my-pi (omp) | 3.57 s | 2.61 s–3.81 s | 23.57× more | 467.2 MB | 11.77× more | mcp-route | yes | 2.62 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.7 MB | 136.5 KB | baseline | 0.34 | 1.00 | 35.0 MB | 354.2 KB |
| jcode | 36.5 MB | 186.0 KB | 1.36× more | — | — | 40.2 MB | 342.3 KB |
| Claude Code | 158.8 MB | 142.4 MB | 1,068× more | — | — | 186.9 MB | 181.4 MB |
| Codex CLI | 113.5 MB | 78.7 MB | 590.17× more | — | — | 107.4 MB | 107.4 MB |
| OpenCode | 261.4 MB | 910.8 KB | 6.67× more | — | — | 576.6 MB | 11.6 MB |
| pi | 118.7 MB | 116.8 MB | 875.75× more | — | — | 148.5 MB | 148.5 MB |
| oh-my-pi (omp) | 381.0 MB | 56.0 MB | 419.81× more | — | — | 450.0 MB | 317.9 MB |
| oh-my-openagent (omo) | 56.6 MB | 41.4 MB | 310.49× more | — | — | 784.4 MB | 328.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.0 MB | 36.5 MB | 38.1 MB |
| jcode | 40.2 MB | 41.2 MB | 43.2 MB |
| Claude Code | 186.9 MB | 920.5 MB | 1.8 GB |
| Codex CLI | 107.4 MB | 536.0 MB | 1.0 GB |
| OpenCode | 576.6 MB | 604.7 MB | 679.4 MB |
| pi | 148.5 MB | 740.0 MB | 1.4 GB |
| oh-my-pi (omp) | 450.0 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 784.4 MB | 3.2 GB | 3.7 GB |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    9.27 ms
  Max:    61.30 ms
  Mean:   19.53 ms
  Median: 16.01 ms
  Stdev:  15.12 ms

--version:
  Min:    11.80 ms
  Max:    27.24 ms
  Mean:   18.53 ms
  Median: 17.80 ms
  Stdev:  4.21 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    34.56 ms
  Max:    104.76 ms
  Mean:   73.61 ms
  Median: 83.26 ms
  Stdev:  27.50 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        0.6       0.5    6.4%
  nofile_limit        1.5       1.0   11.7%
  perm_harden         1.7       0.1    1.8%
  args_parse          1.7       0.0    0.4%
  config_loaded       7.5       5.7   67.5%
  server_new          8.1       0.6    6.9%
  socket_bound        8.5       0.5    5.3%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    22.81 ms
  Max:    128.20 ms
  Mean:   57.24 ms
  Median: 48.38 ms
  Stdev:  33.63 ms

Bridge stage: hello_ok:
  Min:    23.49 ms
  Max:    129.39 ms
  Mean:   58.25 ms
  Median: 49.88 ms
  Stdev:  33.74 ms

Bridge stage: no-op reply:
  Min:    23.93 ms
  Max:    129.99 ms
  Mean:   58.85 ms
  Median: 50.70 ms
  Stdev:  33.77 ms

Bridge total (socket -> no-op):
  Min:    23.93 ms
  Max:    129.99 ms
  Mean:   58.85 ms
  Median: 50.70 ms
  Stdev:  33.77 ms

============================================================
Summary:
  Binary load median:      ~16.0 ms
  Server ready median:     ~83.3 ms
  Bridge total median:     ~50.7 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (d3aa68ac9905)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-8pebf7yk
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.029/0.032    p95    0.082/0.072    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.254/0.291    p95    0.850/0.902    delta  +0.037 ms  control-no-change
  mcp_config_load   p50    0.310/0.290    p95    0.753/0.850    delta  -0.019 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   27.188/24.992   p95   38.415/34.700   delta  -2.197 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22926..22954 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22954 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    5.375/5.506    p95    7.596/8.654    delta  +0.131 ms  control-no-change
  skill_request_overlay p50    7.407/6.022    p95   10.268/11.776   delta  -1.385 ms  control-no-change
  mcp_config_load   p50    0.315/0.295    p95    0.718/0.550    delta  -0.020 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   21.545/28.652   p95   48.492/63.355   delta  +7.106 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22954 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22954 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.032/0.031    p95    0.083/0.431    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.268/0.229    p95    0.553/0.772    delta  -0.039 ms  control-no-change
  mcp_config_load   p50    0.299/0.288    p95    0.388/0.610    delta  -0.011 ms  control-no-change
  attach_history    p50  363.879/389.337  p95  429.791/450.669  delta +25.458 ms  control-no-change
  resume_ready      p50   77.963/86.094   p95  101.758/97.777   delta  +8.131 ms  control-no-change
  provider_dispatch p50    1.102/1.165    p95    1.835/1.691    delta  +0.063 ms  control-no-change
  session_save      p50   19.640/22.529   p95   23.871/30.379   delta  +2.890 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (4 distinct values)
  SPREAD   a session_save store_read_bytes 22952..22972 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22972 (5 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    3.934/4.078    p95    6.029/5.976    delta  +0.144 ms  control-no-change
  skill_request_overlay p50    4.980/5.124    p95    7.158/6.795    delta  +0.145 ms  control-no-change
  mcp_config_load   p50    0.264/0.265    p95    0.301/0.396    delta  +0.001 ms  control-no-change
  attach_history    p50  275.377/283.181  p95  392.095/434.773  delta  +7.804 ms  control-no-change
  resume_ready      p50   59.517/58.971   p95   95.546/105.513  delta  -0.546 ms  control-no-change
  provider_dispatch p50    1.010/0.994    p95    1.097/3.239    delta  -0.015 ms  control-no-change
  session_save      p50   17.383/16.431   p95   38.165/76.452   delta  -0.953 ms  control-no-change
  total store_read_bytes p50 5311307  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22946..22968 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22972 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.030    p95    0.032/0.059    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.247/0.234    p95    0.931/0.443    delta  -0.013 ms  control-no-change
  mcp_config_load   p50    0.281/0.275    p95    0.487/0.523    delta  -0.006 ms  control-no-change
  attach_history    p50 3658.807/4282.357 p95 4444.951/4679.815 delta +623.550 ms  control-no-change
  resume_ready      p50  741.936/756.099  p95 1058.313/927.943  delta +14.163 ms  control-no-change
  provider_dispatch p50   11.563/11.387   p95   16.070/17.825   delta  -0.175 ms  control-no-change
  session_save      p50   26.965/22.697   p95   51.671/54.265   delta  -4.268 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22949..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22966 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.015/4.802    p95    5.182/7.132    delta  +0.787 ms  control-no-change
  skill_request_overlay p50    5.025/6.143    p95   10.171/8.557    delta  +1.118 ms  control-drift
  mcp_config_load   p50    0.273/0.319    p95    0.375/0.554    delta  +0.046 ms  control-no-change
  attach_history    p50 3323.214/3512.878 p95 3745.697/4059.582 delta +189.664 ms  control-no-change
  resume_ready      p50  688.569/672.233  p95 1003.910/781.887  delta -16.336 ms  control-no-change
  provider_dispatch p50   10.804/9.716    p95   18.724/12.554   delta  -1.088 ms  control-no-change
  session_save      p50   23.748/22.040   p95   58.133/48.146   delta  -1.708 ms  control-no-change
  total store_read_bytes p50 53417513  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184299..42184305 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210250..11210255 (4 distinct values)
  SPREAD   a session_save store_read_bytes 22951..22967 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22967 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (d3aa68ac9905)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-8fr0m2ze
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.025/0.027    p95    0.058/0.102    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.239/0.245    p95    0.462/0.693    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.264/0.269    p95    0.298/1.171    delta  +0.005 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.003    delta  +0.000 ms  control-no-change
  session_save      p50   15.897/18.367   p95   21.652/23.721   delta  +2.470 ms  control-no-change
  total store_read_bytes p50 23661  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22947, 22957, 23661, 23667, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22937, 22939, 22941, 22951, 23649, 23661, 23669, 24375, 24391]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22947..24379 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22937..24391 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    6.284/5.346    p95   15.755/12.007   delta  -0.938 ms  control-no-change
  skill_request_overlay p50    8.042/6.570    p95   12.388/11.309   delta  -1.472 ms  control-no-change
  mcp_config_load   p50    0.296/0.298    p95    0.615/0.907    delta  +0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.002/0.003    delta  -0.000 ms  control-no-change
  session_save      p50   22.999/24.627   p95   56.092/37.758   delta  +1.628 ms  control-no-change
  total store_read_bytes p50 23658  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22939, 22941, 22951, 23655, 23661, 23665, 24373, 24375, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22943, 22945, 22947, 22957, 23655, 23657, 23663, 24373, 24379, 24387]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22939..24383 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22943..24387 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.028/0.031    p95    0.041/0.103    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.231/0.252    p95    0.243/0.462    delta  +0.021 ms  control-no-change
  mcp_config_load   p50    0.258/0.284    p95    0.276/0.504    delta  +0.026 ms  control-no-change
  attach_history    p50  121.948/127.216  p95  140.973/144.460  delta  +5.268 ms  control-no-change
  resume_ready      p50   70.974/75.760   p95   76.108/94.791   delta  +4.786 ms  control-no-change
  provider_dispatch p50    1.088/1.156    p95    1.483/2.318    delta  +0.068 ms  control-no-change
  session_save      p50   20.011/20.679   p95   23.820/59.566   delta  +0.669 ms  control-no-change
  total store_read_bytes p50 2841251  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23668, 24378, 24384, 25090, 25096, 25818, 25824, 26512, 26532]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23666, 23678, 24380, 24388, 25090, 25092, 25806, 25808, 26520, 26522]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26532 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23666..26522 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.419/5.046    p95    7.611/7.471    delta  +0.627 ms  control-no-change
  skill_request_overlay p50    5.370/6.007    p95    5.900/8.910    delta  +0.637 ms  control-no-change
  mcp_config_load   p50    0.274/0.283    p95    0.402/0.301    delta  +0.009 ms  control-no-change
  attach_history    p50  110.386/108.742  p95  141.105/123.595  delta  -1.644 ms  control-no-change
  resume_ready      p50   68.577/61.680   p95   90.244/89.996   delta  -6.897 ms  control-no-change
  provider_dispatch p50    1.089/1.165    p95    1.521/1.575    delta  +0.076 ms  control-no-change
  session_save      p50   17.575/18.612   p95   31.159/25.181   delta  +1.037 ms  control-no-change
  total store_read_bytes p50 2841260  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 23674, 24384, 24392, 25102, 25812, 25816, 26518, 26526]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23678, 24384, 24396, 25098, 25106, 25800, 25806, 26522, 26532]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26526 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26532 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.035/0.032    p95    0.068/0.045    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.245/0.227    p95    0.583/0.327    delta  -0.018 ms  control-no-change
  mcp_config_load   p50    0.286/0.262    p95    0.654/0.354    delta  -0.023 ms  control-no-change
  attach_history    p50 1337.974/1406.527 p95 1545.544/1591.726 delta +68.553 ms  control-no-change
  resume_ready      p50  873.576/850.192  p95  917.134/890.752  delta -23.385 ms  control-no-change
  provider_dispatch p50   11.975/11.944   p95   16.348/18.448   delta  -0.032 ms  control-no-change
  session_save      p50   19.349/18.483   p95   24.322/30.223   delta  -0.866 ms  control-no-change
  total store_read_bytes p50 28640290  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23677, 23685, 24375, 24391, 25113, 25117, 25807, 25815, 26529, 26533]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23663, 23667, 24385, 24389, 25101, 25811, 25815, 26517, 26529]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23677..26533 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23663..26529 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.530/4.389    p95    5.773/11.557   delta  -0.141 ms  control-no-change
  skill_request_overlay p50    5.515/6.072    p95    6.788/10.767   delta  +0.557 ms  control-no-change
  mcp_config_load   p50    0.281/0.306    p95    0.694/0.939    delta  +0.025 ms  control-no-change
  attach_history    p50 1231.209/1255.346 p95 1483.173/1559.813 delta +24.137 ms  control-no-change
  resume_ready      p50  793.989/814.290  p95  957.775/1032.826 delta +20.301 ms  control-no-change
  provider_dispatch p50   12.495/10.790   p95   16.305/17.981   delta  -1.704 ms  control-no-change
  session_save      p50   24.281/22.186   p95   42.511/77.513   delta  -2.095 ms  control-no-change
  total store_read_bytes p50 28640284  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23681, 24387, 24391, 25105, 25113, 25807, 25819, 26521, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23672, 23684, 24386, 24390, 25096, 25104, 25818, 25822, 26524]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23672..26524 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.2 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.5 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.6 ms | 0.5 ms | 0.6 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 2.0 ms | 2.1 ms | 1.7 ms | 2.1 ms | 5 |
| write (10KB) | 2.1 ms | 2.4 ms | 2.0 ms | 2.4 ms | 5 |
| edit (simple replace) | 1.9 ms | 2.2 ms | 1.7 ms | 2.2 ms | 5 |
| ls (scripts dir) | 0.9 ms | 1.2 ms | 0.8 ms | 1.2 ms | 5 |
| agentgrep (grep in tree) | 2.8 ms | 2.9 ms | 2.7 ms | 2.9 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 1.7 ms | 1.5 ms | 1.7 ms | 5 |
| bash (true) | 3.9 ms | 4.3 ms | 3.8 ms | 4.3 ms | 5 |
| bash (echo hello) | 3.9 ms | 4.4 ms | 3.7 ms | 4.4 ms | 5 |
| bash (ls fixture tree) | 6.6 ms | 6.7 ms | 6.4 ms | 6.7 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 136.8 ms | 136.8 ms | 136.8 ms | 137.0 ms | 0 |
| 10 | 440.5 ms | 441.3 ms | 441.3 ms | 441.8 ms | 0 |
| 40 | 2.36 s | 2.36 s | 2.36 s | 2.36 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 2.18 s | 2.18 s | 2.18 s | 1 |
| leaf_touch | src/main.rs | 1.32 s | 1.32 s | 1.32 s | 1 |
| core_touch | crates/app-core/src/lib.rs | 39.06 s | 39.06 s | 39.06 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 54.39 s | 54.39 s | 54.39 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (cdc8cb8) | 13.7 ms | 97.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.0 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 39.9 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 97.7 ms | 479.4 MB |
| OpenCode | 1.19.5 | 468.3 ms | 605.3 MB |
| pi | 1.5.0 | 253.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 637.9 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 187.5 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 206.5 ms | 155.6 ms–264.0 ms | baseline | 36.5 MB | baseline | yes | 3,686 / 2,390 (1 tools) | — |
| jcode | 161.8 ms | 136.0 ms–234.0 ms | 1.28× less | 28.2 MB | 1.29× less | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.23 s | 1.23 s–1.86 s | 5.97× more | 273.2 MB | 7.49× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 1.88 s | 1.39 s–3.35 s | 9.09× more | 262.7 MB | 7.21× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 4.53 s | 4.43 s–6.92 s | 21.93× more | 865.3 MB | 23.74× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 431.4 ms | 430.1 ms–472.6 ms | 2.09× more | 125.6 MB | 3.44× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.69 s | 2.63 s–3.06 s | 13.02× more | 413.6 MB | 11.35× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 19.34 s | 18.78 s–26.61 s | 93.63× more | 546.3 MB | 14.99× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 172.9 ms | 161.2 ms–218.6 ms | baseline | 33.6 MB | baseline | — | yes | — | 11,209 / 9,255 (8 tools) |
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
| horon | 169.3 ms | 155.9 ms–305.4 ms | baseline | 24.9 MB | baseline | skill-uri | yes | — |
| jcode | 213.8 ms | 188.6 ms–386.5 ms | 1.26× more | 23.6 MB | 1.05× less | skill-tool | yes | — |
| Claude Code | 946.2 ms | 828.4 ms–1.23 s | 5.59× more | 240.5 MB | 9.66× more | skill-tool | yes | — |
| Codex CLI | 1.98 s | 1.86 s–3.33 s | 11.71× more | 264.1 MB | 10.61× more | file-read | yes | — |
| OpenCode | 7.11 s | 6.73 s–7.36 s | 41.98× more | 739.9 MB | 29.73× more | skill-tool | yes | — |
| pi | 587.9 ms | 578.0 ms–606.6 ms | 3.47× more | 97.7 MB | 3.92× more | file-read | yes | — |
| oh-my-pi (omp) | 12.96 s | 3.52 s–13.69 s | 76.57× more | 409.2 MB | 16.44× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.11 s | 17.54 s–29.50 s | 124.70× more | 540.3 MB | 21.71× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 241.2 ms | 189.2 ms–362.3 ms | baseline | 37.4 MB | baseline | mcp-call | yes | 149.7 ms | — |
| jcode | 318.1 ms | 305.4 ms–779.2 ms | 1.32× more | 36.6 MB | 1.02× less | mcp-action | yes | 220.4 ms | — |
| Claude Code | 1.24 s | 998.1 ms–2.27 s | 5.15× more | 271.2 MB | 7.26× more | mcp-tool | yes | 1.10 s | — |
| Codex CLI | 1.02 s | 735.5 ms–2.08 s | 4.24× more | 179.1 MB | 4.79× more | mcp-tool | yes | 790.0 ms | — |
| OpenCode | 6.39 s | 5.99 s–7.13 s | 26.47× more | 3.2 GB | 87.37× more | mcp-tool | yes | 5.00 s | — |
| pi | 645.0 ms | 626.2 ms–1.58 s | 2.67× more | 119.0 MB | 3.18× more | mcp-tool | yes | 516.7 ms | — |
| oh-my-pi (omp) | 3.55 s | 3.21 s–3.72 s | 14.71× more | 439.3 MB | 11.75× more | mcp-route | yes | 2.66 s | — |
| oh-my-openagent (omo) | 22.20 s | 17.49 s–29.16 s | 92.03× more | 566.9 MB | 15.16× more | mcp-tool | yes | 19.01 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.5 MB | 133.3 KB | baseline | — | — | 30.2 MB | 277.7 KB |
| jcode | 22.0 MB | 84.6 KB | 1.57× less | — | — | 26.6 MB | 401.1 KB |
| Claude Code | 154.8 MB | 171.0 MB | 1,313× more | — | — | 187.8 MB | 189.7 MB |
| Codex CLI | 124.7 MB | 97.5 MB | 749.14× more | — | — | 162.9 MB | 116.5 MB |
| OpenCode | 246.7 MB | 15.8 MB | 121.74× more | — | — | 436.9 MB | 17.8 MB |
| pi | 90.8 MB | 90.6 MB | 696.21× more | — | — | 100.5 MB | 101.0 MB |
| oh-my-pi (omp) | 322.0 MB | 130.5 MB | 1,003× more | — | — | 335.6 MB | 340.4 MB |
| oh-my-openagent (omo) | 145.3 MB | 130.3 MB | 1,001× more | — | — | 411.7 MB | 403.4 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 30.2 MB | 32.4 MB | 32.7 MB |
| jcode | 26.6 MB | 28.8 MB | 30.2 MB |
| Claude Code | 187.8 MB | 950.3 MB | 1.9 GB |
| Codex CLI | 162.9 MB | 648.1 MB | 1.2 GB |
| OpenCode | 436.9 MB | 458.2 MB | 593.8 MB |
| pi | 100.5 MB | 504.6 MB | 1009.4 MB |
| oh-my-pi (omp) | 335.6 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 411.7 MB | 2.1 GB | 4.0 GB |

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    11.01 ms
  Max:    11.62 ms
  Mean:   11.19 ms
  Median: 11.12 ms
  Stdev:  0.20 ms

--version:
  Min:    10.73 ms
  Max:    13.90 ms
  Mean:   12.03 ms
  Median: 11.74 ms
  Stdev:  1.14 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.76 ms
  Max:    27.77 ms
  Mean:   22.51 ms
  Median: 21.94 ms
  Stdev:  1.86 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.3       1.3   23.7%
  nofile_limit        1.7       0.4    7.4%
  perm_harden         3.7       1.9   34.3%
  args_parse          3.7       0.0    0.0%
  config_loaded       4.5       0.8   14.7%
  server_new          5.3       0.8   14.4%
  socket_bound        5.6       0.3    5.5%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~11.1 ms
  Server ready median:     ~21.9 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (bd20c45db24b)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-nngru5v_
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.155/0.155    p95    0.213/0.213    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    1.179/1.192    p95    1.279/1.235    delta  +0.013 ms  control-no-change
  mcp_config_load   p50    1.410/1.410    p95    1.543/1.447    delta  -0.001 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.003    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  session_save      p50   54.304/55.576   p95   63.361/65.912   delta  +1.272 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22920..22960 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22916..22964 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   23.202/23.483   p95   33.191/35.470   delta  +0.281 ms  control-no-change
  skill_request_overlay p50   53.020/53.368   p95   58.012/57.962   delta  +0.348 ms  control-no-change
  mcp_config_load   p50    1.467/1.465    p95    1.593/1.672    delta  -0.002 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.005    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   61.386/62.669   p95   69.989/74.017   delta  +1.283 ms  control-no-change
  total store_read_bytes p50 22956  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22910..22964 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22926..22960 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.155/0.150    p95    0.253/0.155    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    1.213/1.202    p95    1.460/1.268    delta  -0.011 ms  control-no-change
  mcp_config_load   p50    1.429/1.409    p95    1.795/1.497    delta  -0.021 ms  control-no-change
  attach_history    p50  556.363/559.558  p95  562.346/567.784  delta  +3.196 ms  control-no-change
  resume_ready      p50  225.802/227.612  p95  232.395/235.091  delta  +1.810 ms  control-no-change
  provider_dispatch p50    1.718/1.748    p95    1.796/2.024    delta  +0.030 ms  control-no-change
  session_save      p50   46.892/46.494   p95   59.466/109.512  delta  -0.398 ms  control-no-change
  total store_read_bytes p50 5311217  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189149..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099100..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22907..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22976 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   23.704/23.998   p95   26.049/27.277   delta  +0.294 ms  control-no-change
  skill_request_overlay p50   52.388/52.674   p95   58.730/55.569   delta  +0.285 ms  control-no-change
  mcp_config_load   p50    1.436/1.461    p95    1.621/2.001    delta  +0.025 ms  control-no-change
  attach_history    p50  554.105/556.213  p95  568.980/570.666  delta  +2.108 ms  control-no-change
  resume_ready      p50  227.900/225.849  p95  235.897/237.280  delta  -2.051 ms  control-no-change
  provider_dispatch p50    1.707/1.681    p95    1.735/1.735    delta  -0.026 ms  control-no-change
  session_save      p50   49.532/46.547   p95   61.579/55.355   delta  -2.985 ms  control-no-change
  total store_read_bytes p50 5311226  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22944..22976 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22976 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.151/0.155    p95    0.166/0.218    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    1.229/1.234    p95    1.318/1.861    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    1.436/1.420    p95    1.496/2.157    delta  -0.016 ms  control-no-change
  attach_history    p50 6316.728/6334.928 p95 8243.767/6677.237 delta +18.200 ms  control-no-change
  resume_ready      p50 2387.696/2382.965 p95 2562.764/2456.150 delta  -4.731 ms  control-no-change
  provider_dispatch p50   17.956/18.297   p95   21.763/23.116   delta  +0.341 ms  control-no-change
  session_save      p50   51.686/52.347   p95   98.799/271.147  delta  +0.661 ms  control-no-change
  total store_read_bytes p50 53417418  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22927..22963 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22970 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   23.611/23.534   p95   24.354/24.669   delta  -0.078 ms  control-no-change
  skill_request_overlay p50   52.473/52.591   p95   54.394/58.076   delta  +0.118 ms  control-no-change
  mcp_config_load   p50    1.476/1.478    p95    1.511/1.534    delta  +0.001 ms  control-no-change
  attach_history    p50 6310.731/6312.044 p95 6353.843/6332.412 delta  +1.313 ms  control-no-change
  resume_ready      p50 2385.441/2372.398 p95 2405.375/2397.799 delta -13.043 ms  control-no-change
  provider_dispatch p50   17.441/18.042   p95   18.323/25.058   delta  +0.601 ms  control-drift
  session_save      p50   45.811/48.499   p95   61.327/55.241   delta  +2.688 ms  control-no-change
  total store_read_bytes p50 53417423  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22926..22969 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22971 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (bd20c45db24b)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-coeskliq
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.106/0.098    p95    0.130/0.099    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    0.744/0.753    p95    0.769/0.831    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.871/0.934    p95    1.006/0.993    delta  +0.064 ms  control-no-change
  attach_history    p50    0.002/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   39.197/41.553   p95   56.156/53.646   delta  +2.355 ms  control-no-change
  total store_read_bytes p50 23655  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22933, 22943, 22951, 23630, 23654, 23656, 24319, 24321, 24339]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22927, 22933, 22957, 23616, 23656, 24306, 24332, 24339]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22933..24339 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22927..24339 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   15.158/15.324   p95   15.313/20.521   delta  +0.166 ms  control-no-change
  skill_request_overlay p50   40.666/40.878   p95   41.740/43.536   delta  +0.211 ms  control-no-change
  mcp_config_load   p50    1.002/1.024    p95    1.079/1.104    delta  +0.021 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.004    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-drift
  session_save      p50   66.555/47.611   p95  164.102/171.072  delta -18.944 ms  control-no-change
  total store_read_bytes p50 23615  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22929, 22931, 22941, 22953, 23612, 23618, 23640, 23660, 24347, 24359]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22931, 22933, 22949, 22957, 23630, 23632, 23658, 24349, 24351, 24355]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22929..24359 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22931..24355 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.102/0.102    p95    0.109/0.146    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.781/0.779    p95    0.885/0.863    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.940/0.946    p95    1.002/0.993    delta  +0.005 ms  control-no-change
  attach_history    p50  205.947/206.107  p95  220.144/209.566  delta  +0.160 ms  control-no-change
  resume_ready      p50  155.807/154.881  p95  167.307/157.816  delta  -0.925 ms  control-no-change
  provider_dispatch p50    1.534/1.538    p95    2.263/1.636    delta  +0.004 ms  control-no-change
  session_save      p50   38.445/67.018   p95   45.964/177.095  delta +28.573 ms  control-drift
  total store_read_bytes p50 2841142  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23647, 23649, 24335, 24366, 25053, 25072, 25741, 25756, 26451, 26462]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23663, 23671, 24321, 24358, 25026, 25070, 25725, 25729, 26426, 26452]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23647..26462 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23663..26452 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   15.527/15.468   p95   18.768/17.021   delta  -0.059 ms  control-no-change
  skill_request_overlay p50   42.022/40.615   p95   48.031/45.098   delta  -1.407 ms  control-no-change
  mcp_config_load   p50    1.013/1.012    p95    1.098/1.104    delta  -0.002 ms  control-no-change
  attach_history    p50  203.305/204.596  p95  208.143/213.421  delta  +1.291 ms  control-no-change
  resume_ready      p50  155.189/154.415  p95  162.616/160.743  delta  -0.774 ms  control-no-change
  provider_dispatch p50    1.536/1.548    p95    1.840/1.990    delta  +0.013 ms  control-no-change
  session_save      p50   39.757/44.051   p95   45.435/144.915  delta  +4.294 ms  control-no-change
  total store_read_bytes p50 2841134  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23635, 23663, 24331, 24362, 25052, 25056, 25739, 25763, 26438, 26462]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23639, 23642, 24319, 24330, 25025, 25048, 25731, 25754, 26434, 26465]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23635..26462 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23639..26465 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.099/0.098    p95    0.122/0.105    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.779/0.757    p95    0.979/0.823    delta  -0.022 ms  control-no-change
  mcp_config_load   p50    0.935/0.933    p95    1.276/1.012    delta  -0.003 ms  control-no-change
  attach_history    p50 2188.091/2202.699 p95 2241.224/2228.971 delta +14.608 ms  control-no-change
  resume_ready      p50 1639.350/1633.845 p95 1648.391/1678.293 delta  -5.505 ms  control-no-change
  provider_dispatch p50   19.286/19.577   p95   19.889/20.576   delta  +0.291 ms  control-no-change
  session_save      p50   38.623/38.499   p95  216.703/48.126   delta  -0.124 ms  control-no-change
  total store_read_bytes p50 28640159  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 23674, 24327, 24333, 25056, 25068, 25759, 25779, 26426, 26474]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23638, 23674, 24353, 24377, 25011, 25044, 25756, 25763, 26434, 26447]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26474 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23638..26447 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.939/15.041   p95   16.680/15.734   delta  +0.102 ms  control-no-change
  skill_request_overlay p50   40.891/41.863   p95   50.506/52.536   delta  +0.972 ms  control-no-change
  mcp_config_load   p50    1.026/1.065    p95    1.144/1.166    delta  +0.040 ms  control-no-change
  attach_history    p50 2171.594/2180.637 p95 2208.250/2192.909 delta  +9.043 ms  control-no-change
  resume_ready      p50 1610.340/1609.501 p95 1618.371/1624.595 delta  -0.839 ms  control-no-change
  provider_dispatch p50   18.173/17.819   p95   19.259/19.788   delta  -0.354 ms  control-no-change
  session_save      p50   42.346/38.917   p95  110.871/75.029   delta  -3.428 ms  control-no-change
  total store_read_bytes p50 28640138  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23636, 23638, 24355, 24375, 25015, 25066, 25748, 25753, 26427, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23650, 23670, 24365, 25064, 25066, 25731, 25745, 26440, 26460]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23636..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23650..26460 (9 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.6 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.1 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| write (small) | 10.4 ms | 12.4 ms | 10.3 ms | 12.4 ms | 5 |
| write (10KB) | 10.4 ms | 12.9 ms | 9.8 ms | 12.9 ms | 5 |
| edit (simple replace) | 10.3 ms | 20.1 ms | 9.8 ms | 20.1 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 4.7 ms | 5.2 ms | 4.2 ms | 5.2 ms | 5 |
| agentgrep (find **/*.rs) | 5.2 ms | 5.2 ms | 4.7 ms | 5.2 ms | 5 |
| bash (true) | 7.8 ms | 7.8 ms | 7.3 ms | 7.8 ms | 5 |
| bash (echo hello) | 7.8 ms | 7.8 ms | 7.3 ms | 7.8 ms | 5 |
| bash (ls fixture tree) | 10.4 ms | 12.5 ms | 9.9 ms | 12.5 ms | 5 |
| invalid tool (dispatch only) | 2.6 ms | 2.6 ms | 2.1 ms | 2.6 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 198.3 ms | 198.3 ms | 198.3 ms | 198.4 ms | 0 |
| 10 | 531.1 ms | 532.0 ms | 532.0 ms | 533.9 ms | 0 |
| 40 | 2.03 s | 2.04 s | 2.04 s | 2.04 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 792.9 ms | 792.9 ms | 792.9 ms | 1 |
| leaf_touch | src\main.rs | 1.02 s | 1.02 s | 1.02 s | 1 |
| core_touch | crates\app-core\src\lib.rs | 29.33 s | 29.33 s | 29.33 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 40.08 s | 40.08 s | 40.08 s | 1 |

## TUI (`horon-tui`)

The interactive client is a second binary and is measured on its own. These numbers come from the `tui` suite and are never compared with the headless tables above: `first_frame_ms` and `direct_first_frame_ms` are two launches of two different programs, and the handoff is the difference between them.

![TUI (`horon-tui`)](charts/tui-01-latency.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 36.8 ms | 35.7 ms–176.4 ms | 5 |
| First frame (direct `horon-tui`) | 39.9 ms | 39.3 ms–41.3 ms | 5 |
| TTY handoff cost | -2.9 ms | -4.2 ms–135.1 ms | 5 |
| Key press to echo (p95) | 0.1 ms | 0.1 ms–0.9 ms | 50 |
| Idle RSS after 5 s | 5.4 MB | 5.4 MB–5.5 MB | 5 |
| RSS after one turn | 5.7 MB | 5.6 MB–5.8 MB | 5 |
| Frame draw time (p50) | 1.3 ms | 1.3 ms–1.3 ms | 5 |
| Frame draw time (p95) | 1.4 ms | 1.4 ms–1.4 ms | 5 |
| Install size (`horon-tui`) | 6.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 53.1 ms | 34.6 ms–98.9 ms | 5 |
| First frame (direct `horon-tui`) | 49.5 ms | 41.5 ms–68.2 ms | 5 |
| TTY handoff cost | 2.0 ms | -33.5 ms–49.4 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.4 ms | 50 |
| Idle RSS after 5 s | 8.9 MB | 8.9 MB–8.9 MB | 5 |
| RSS after one turn | 9.1 MB | 9.0 MB–9.1 MB | 5 |
| Frame draw time (p50) | 1.0 ms | 0.9 ms–1.0 ms | 5 |
| Frame draw time (p95) | 1.1 ms | 1.1 ms–1.1 ms | 5 |
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
| Frame draw time (p50) | 3.0 ms | 2.9 ms–3.1 ms | 5 |
| Frame draw time (p95) | 3.2 ms | 3.0 ms–3.3 ms | 5 |
| Install size (`horon-tui`) | 5.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
