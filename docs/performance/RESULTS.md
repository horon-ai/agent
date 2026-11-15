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

<!-- bench-auto:start commit=f7a53f1ebbc9a5dd4d9461c0aef60877aef898b5 -->

## Headless (`horon-agent`)

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `f7a53f1ebbc9a5dd4d9461c0aef60877aef898b5`
- Measured at: 2026-11-15 13:29 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/43714998312> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

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
| horon | horon-agent v0.1.0-dev (f7a53f1) | 2.9 ms | 115.6 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 7.5 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 6.2 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 30.6 ms | 435.0 MB |
| OpenCode | 1.19.5 | 341.6 ms | 613.4 MB |
| pi | 1.5.0 | 168.4 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 557.0 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 84.1 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 44.2 ms | 42.2 ms–184.5 ms | baseline | 47.8 MB | baseline | yes | 10 | 3,778 / 2,390 (1 tools) | — |
| jcode | 140.4 ms | 139.9 ms–141.1 ms | 3.18× more | 42.1 MB | 1.14× less | yes | 5 | 3,292 / 1,433 (1 tools) | — |
| Claude Code | 348.9 ms | 339.8 ms–361.2 ms | 7.89× more | 207.2 MB | 4.34× more | yes | — | 45,780 / 29,653 (12 tools) | — |
| Codex CLI | 364.0 ms | 357.0 ms–444.1 ms | 8.23× more | 198.6 MB | 4.15× more | yes | 6 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 1.96 s | 1.92 s–2.13 s | 44.29× more | 589.2 MB | 12.33× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 277.9 ms | 271.3 ms–288.5 ms | 6.29× more | 113.4 MB | 2.37× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.39 s | 1.38 s–1.65 s | 31.50× more | 413.7 MB | 8.65× more | yes | 66 | 29,858 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 4.29 s | 4.20 s–4.70 s | 96.93× more | 677.0 MB | 14.16× more | yes | 36 | 27,443 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 45.8 ms | 45.3 ms–46.2 ms | baseline | 40.4 MB | baseline | — | yes | — | 11,301 / 9,255 (8 tools) |
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
| horon | 40.5 ms | 39.8 ms–41.2 ms | baseline | 38.1 MB | baseline | skill-uri | yes | — |
| jcode | 47.4 ms | 46.7 ms–47.6 ms | 1.17× more | 41.0 MB | 1.08× more | skill-tool | yes | — |
| Claude Code | 331.2 ms | 322.4 ms–342.6 ms | 8.18× more | 203.2 MB | 5.33× more | skill-tool | yes | — |
| Codex CLI | 403.4 ms | 382.6 ms–453.6 ms | 9.96× more | 196.7 MB | 5.16× more | file-read | yes | — |
| OpenCode | 2.24 s | 2.19 s–2.27 s | 55.21× more | 616.3 MB | 16.17× more | skill-tool | yes | — |
| pi | 381.4 ms | 378.6 ms–384.9 ms | 9.42× more | 126.2 MB | 3.31× more | file-read | yes | — |
| oh-my-pi (omp) | 1.32 s | 1.30 s–1.32 s | 32.60× more | 333.9 MB | 8.76× more | file-read | yes | — |
| oh-my-openagent (omo) | 4.69 s | 4.65 s–4.70 s | 115.88× more | 663.4 MB | 17.40× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 61.0 ms | 58.4 ms–62.0 ms | baseline | 44.7 MB | baseline | mcp-call | yes | 49.9 ms | — |
| jcode | 161.4 ms | 160.7 ms–161.7 ms | 2.65× more | 54.1 MB | 1.21× more | mcp-action | yes | 139.6 ms | — |
| Claude Code | 423.4 ms | 410.9 ms–435.3 ms | 6.95× more | 226.6 MB | 5.07× more | mcp-tool | yes | 297.4 ms | — |
| Codex CLI | 347.7 ms | 323.2 ms–390.7 ms | 5.70× more | 193.4 MB | 4.33× more | mcp-tool | yes | 235.5 ms | — |
| OpenCode | 2.07 s | 2.06 s–2.10 s | 33.95× more | 579.0 MB | 12.95× more | mcp-tool | yes | 1.90 s | — |
| pi | 412.0 ms | 407.5 ms–416.8 ms | 6.76× more | 138.1 MB | 3.09× more | mcp-tool | yes | 363.7 ms | — |
| oh-my-pi (omp) | 1.31 s | 1.30 s–1.33 s | 21.57× more | 343.2 MB | 7.67× more | mcp-route | yes | 1.25 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 37.3 MB | 200.5 KB | baseline | 0.80 | 1.00 | 44.8 MB | 560.6 KB |
| jcode | 42.7 MB | 97.8 KB | 2.05× less | 0.00 | — | 46.0 MB | 278.5 KB |
| Claude Code | 151.8 MB | 150.1 MB | 766.55× more | — | — | 187.7 MB | 181.1 MB |
| Codex CLI | 129.8 MB | 132.9 MB | 678.46× more | — | — | 160.7 MB | 164.7 MB |
| OpenCode | 242.5 MB | 612.5 KB | 3.05× more | — | — | 451.1 MB | 8.0 MB |
| pi | 108.8 MB | 108.6 MB | 554.35× more | — | — | 132.4 MB | 127.4 MB |
| oh-my-pi (omp) | 312.6 MB | 117.8 MB | 601.41× more | — | — | 371.1 MB | 352.9 MB |
| oh-my-openagent (omo) | 307.3 MB | 58.2 MB | 297.39× more | — | — | 590.4 MB | 586.8 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.8 MB | 47.7 MB | 49.8 MB |
| jcode | 46.0 MB | 47.9 MB | 48.5 MB |
| Claude Code | 187.7 MB | 924.6 MB | 1.8 GB |
| Codex CLI | 160.7 MB | 812.8 MB | 1.6 GB |
| OpenCode | 451.1 MB | 526.2 MB | 526.5 MB |
| pi | 132.4 MB | 645.1 MB | 1.2 GB |
| oh-my-pi (omp) | 371.1 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 590.4 MB | 2.9 GB | 5.7 GB |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.88 ms
  Max:    4.19 ms
  Mean:   4.00 ms
  Median: 3.99 ms
  Stdev:  0.09 ms

--version:
  Min:    3.75 ms
  Max:    4.24 ms
  Mean:   3.87 ms
  Median: 3.85 ms
  Stdev:  0.14 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.53 ms
  Max:    20.62 ms
  Mean:   20.57 ms
  Median: 20.57 ms
  Stdev:  0.03 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.0%
  logging_init        0.2       0.2   15.8%
  nofile_limit        0.6       0.4   26.0%
  perm_harden         0.6       0.0    0.9%
  args_parse          0.6       0.0    0.0%
  config_loaded       0.8       0.2   13.9%
  server_new          1.3       0.5   35.6%
  socket_bound        1.4       0.1    7.8%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.68 ms
  Max:    20.80 ms
  Mean:   20.76 ms
  Median: 20.76 ms
  Stdev:  0.03 ms

Bridge stage: hello_ok:
  Min:    21.34 ms
  Max:    21.89 ms
  Mean:   21.44 ms
  Median: 21.39 ms
  Stdev:  0.16 ms

Bridge stage: no-op reply:
  Min:    21.78 ms
  Max:    22.46 ms
  Mean:   21.91 ms
  Median: 21.87 ms
  Stdev:  0.20 ms

Bridge total (socket -> no-op):
  Min:    21.78 ms
  Max:    22.46 ms
  Mean:   21.91 ms
  Median: 21.87 ms
  Stdev:  0.20 ms

============================================================
Summary:
  Binary load median:      ~4.0 ms
  Server ready median:     ~20.6 ms
  Bridge total median:     ~21.9 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (d204df43dbd0)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-7b9ile2g
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.011/0.011    p95    0.012/0.012    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.087/0.087    p95    0.109/0.107    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.114/0.114    p95    0.115/0.117    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50    6.137/7.259    p95   31.410/22.950   delta  +1.122 ms  control-drift
  total store_read_bytes p50 23078  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23056..23084 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23080 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.961/1.950    p95    2.017/2.426    delta  -0.010 ms  control-no-change
  skill_request_overlay p50    3.517/3.528    p95    3.573/4.157    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.116/0.117    p95    0.122/0.169    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   15.186/26.266   p95   31.444/74.404   delta +11.081 ms  control-drift
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23084 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23072..23088 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.011/0.011    p95    0.012/0.013    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.085/0.086    p95    0.098/0.103    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.113/0.115    p95    0.115/0.131    delta  +0.001 ms  control-no-change
  attach_history    p50  179.424/186.260  p95  186.567/245.960  delta  +6.836 ms  control-no-change
  resume_ready      p50   34.799/34.855   p95   35.358/35.637   delta  +0.056 ms  control-no-change
  provider_dispatch p50    0.823/0.818    p95    0.848/0.836    delta  -0.005 ms  control-no-change
  session_save      p50    8.958/5.696    p95   23.059/68.603   delta  -3.262 ms  control-no-change
  total store_read_bytes p50 5311405  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23066..23090 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23062..23086 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.948/1.971    p95    2.093/2.056    delta  +0.023 ms  control-no-change
  skill_request_overlay p50    3.507/3.515    p95    3.653/3.696    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.115/0.118    p95    0.121/0.121    delta  +0.003 ms  control-no-change
  attach_history    p50  178.585/179.497  p95  218.057/189.456  delta  +0.912 ms  control-no-change
  resume_ready      p50   34.531/34.533   p95   35.639/35.989   delta  +0.002 ms  control-no-change
  provider_dispatch p50    0.821/0.807    p95    0.864/0.854    delta  -0.014 ms  control-no-change
  session_save      p50    5.609/5.700    p95  148.017/59.941   delta  +0.090 ms  control-no-change
  total store_read_bytes p50 5311406  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23064..23090 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189193..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099135..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23094 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.012/0.012    p95    0.012/0.013    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.085/0.086    p95    0.091/0.088    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.112/0.114    p95    0.116/0.122    delta  +0.002 ms  control-no-change
  attach_history    p50 2335.812/2318.679 p95 2388.838/2399.083 delta -17.133 ms  control-no-change
  resume_ready      p50  388.529/389.608  p95  398.475/398.996  delta  +1.079 ms  control-no-change
  provider_dispatch p50    9.872/9.824    p95   10.980/10.776   delta  -0.048 ms  control-no-change
  session_save      p50    6.151/6.038    p95   71.446/6.518    delta  -0.112 ms  control-no-change
  total store_read_bytes p50 53417617  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23069..23089 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23069..23093 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.968/1.986    p95    2.028/2.092    delta  +0.018 ms  control-no-change
  skill_request_overlay p50    3.473/3.514    p95    3.916/3.709    delta  +0.041 ms  control-no-change
  mcp_config_load   p50    0.117/0.119    p95    0.140/0.125    delta  +0.003 ms  control-no-change
  attach_history    p50 2330.769/2324.514 p95 2426.805/2400.118 delta  -6.255 ms  control-no-change
  resume_ready      p50  389.217/386.862  p95  414.361/398.748  delta  -2.355 ms  control-no-change
  provider_dispatch p50   10.051/10.078   p95   10.671/10.704   delta  +0.027 ms  control-no-change
  session_save      p50    6.107/6.112    p95    6.266/6.617    delta  +0.006 ms  control-no-change
  total store_read_bytes p50 53417622  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23077..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23073..23089 (9 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (d204df43dbd0)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-bd9sbtdx
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.012/0.012    p95    0.016/0.014    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.092/0.092    p95    0.094/0.112    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.124/0.124    p95    0.135/0.147    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.581/6.512    p95   15.042/6.770    delta  -0.068 ms  control-no-change
  total store_read_bytes p50 23804  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23075, 23077, 23798, 23804, 24499, 24515, 24517, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23081, 23093, 23792, 23800, 24515, 24519, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23075..24523 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24523 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.425/2.394    p95    2.447/2.489    delta  -0.031 ms  control-no-change
  skill_request_overlay p50    3.988/3.996    p95    4.109/4.053    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.133/0.133    p95    0.144/0.140    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   11.184/10.677   p95   23.367/11.714   delta  -0.507 ms  control-no-change
  total store_read_bytes p50 23808  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23057, 23065, 23081, 23792, 23804, 23812, 24515, 24519, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23077, 23079, 23083, 23784, 23792, 23796, 24515, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23057..24523 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24523 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.012/0.012    p95    0.013/0.014    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.091/0.091    p95    0.103/0.091    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.122/0.122    p95    0.126/0.130    delta  +0.000 ms  control-no-change
  attach_history    p50   78.150/79.102   p95   81.100/80.116   delta  +0.952 ms  control-no-change
  resume_ready      p50   43.022/42.939   p95   43.750/43.752   delta  -0.084 ms  control-no-change
  provider_dispatch p50    1.070/1.075    p95    1.112/1.086    delta  +0.005 ms  control-no-change
  session_save      p50    6.804/6.824    p95   49.112/28.260   delta  +0.019 ms  control-no-change
  total store_read_bytes p50 2841369  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23793, 23799, 24512, 24526, 25227, 25231, 25243, 25942, 25948, 26665]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23781, 23801, 24512, 24516, 25223, 25231, 25958, 25966, 26677, 26685]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23793..26665 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23781..26685 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.360/2.385    p95    2.456/2.441    delta  +0.025 ms  control-no-change
  skill_request_overlay p50    3.886/3.869    p95    3.901/3.958    delta  -0.018 ms  control-no-change
  mcp_config_load   p50    0.133/0.132    p95    0.142/0.142    delta  -0.001 ms  control-no-change
  attach_history    p50   80.472/80.430   p95   81.718/81.225   delta  -0.042 ms  control-no-change
  resume_ready      p50   44.207/43.881   p95   46.077/44.257   delta  -0.327 ms  control-no-change
  provider_dispatch p50    1.069/1.042    p95    1.094/1.084    delta  -0.026 ms  control-no-change
  session_save      p50    8.603/8.553    p95   15.285/16.059   delta  -0.051 ms  control-no-change
  total store_read_bytes p50 2841374  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23793, 23797, 24508, 24524, 25233, 25235, 25950, 25960, 26673, 26683]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23793, 23797, 24512, 24524, 25235, 25237, 25952, 25958, 26671, 26677]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23793..26683 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23793..26677 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.013/0.013    p95    0.017/0.015    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.094/0.096    p95    0.104/0.097    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.127/0.128    p95    0.136/0.129    delta  +0.000 ms  control-no-change
  attach_history    p50  921.309/919.546  p95  929.746/942.751  delta  -1.764 ms  control-no-change
  resume_ready      p50  496.900/505.218  p95  515.314/528.214  delta  +8.318 ms  control-no-change
  provider_dispatch p50   12.589/12.273   p95   13.482/14.400   delta  -0.317 ms  control-no-change
  session_save      p50    6.771/6.881    p95   77.300/41.912   delta  +0.110 ms  control-no-change
  total store_read_bytes p50 28640394  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 23802, 24511, 24513, 25236, 25238, 25947, 25953, 26670, 26688]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23804, 23812, 24523, 24527, 25226, 25234, 25957, 25969, 26680, 26684]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26688 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23804..26684 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.388/2.424    p95    2.510/2.667    delta  +0.036 ms  control-no-change
  skill_request_overlay p50    3.884/3.900    p95    3.968/4.055    delta  +0.016 ms  control-no-change
  mcp_config_load   p50    0.131/0.134    p95    0.136/0.144    delta  +0.003 ms  control-drift
  attach_history    p50  929.840/920.739  p95  936.083/935.284  delta  -9.101 ms  control-no-change
  resume_ready      p50  532.815/520.177  p95  548.800/543.525  delta -12.638 ms  control-no-change
  provider_dispatch p50   12.892/12.428   p95   13.686/12.821   delta  -0.465 ms  control-no-change
  session_save      p50   10.757/10.695   p95   94.025/158.924  delta  -0.062 ms  control-no-change
  total store_read_bytes p50 28640399  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 23804, 24517, 24523, 25238, 25246, 25953, 25969, 26672]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23796, 24519, 24527, 25216, 25230, 25953, 25961, 26672, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26672 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23796..26680 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.3 ms | 0.3 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.3 ms | 0.3 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 1.3 ms | 143.1 ms | 1.2 ms | 143.1 ms | 5 |
| write (10KB) | 1.5 ms | 1.8 ms | 1.4 ms | 1.8 ms | 5 |
| edit (simple replace) | 1.3 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| ls (scripts dir) | 0.7 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| agentgrep (grep in tree) | 1.9 ms | 2.0 ms | 1.9 ms | 2.0 ms | 5 |
| agentgrep (find **/*.rs) | 1.3 ms | 1.7 ms | 1.2 ms | 1.7 ms | 5 |
| bash (true) | 1.8 ms | 1.9 ms | 1.7 ms | 1.9 ms | 5 |
| bash (echo hello) | 1.8 ms | 1.8 ms | 1.6 ms | 1.8 ms | 5 |
| bash (ls fixture tree) | 2.8 ms | 2.9 ms | 2.8 ms | 2.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 46.4 ms | 46.4 ms | 46.4 ms | 46.5 ms | 0 |
| 10 | 156.2 ms | 195.7 ms | 195.7 ms | 198.7 ms | 0 |
| 40 | 687.8 ms | 748.8 ms | 792.9 ms | 794.4 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 249.8 ms | 249.8 ms | 249.8 ms | 1 |
| leaf_touch | src/main.rs | 381.8 ms | 381.8 ms | 381.8 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 18.50 s | 18.50 s | 18.50 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 26.62 s | 26.62 s | 26.62 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (f7a53f1) | 12.9 ms | 97.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 8.7 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 20.4 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 87.9 ms | 346.5 MB |
| OpenCode | 1.19.5 | 381.8 ms | 344.6 MB |
| pi | 1.5.0 | 224.8 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 835.1 ms | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 142.2 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 93.7 ms | 82.9 ms–368.7 ms | baseline | 31.1 MB | baseline | yes | 3,619 / 2,390 (1 tools) | — |
| jcode | 374.4 ms | 351.9 ms–579.0 ms | 4.00× more | 34.0 MB | 1.10× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 511.1 ms | 381.0 ms–858.4 ms | 5.46× more | 197.7 MB | 6.36× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 473.4 ms | 363.2 ms–1.37 s | 5.05× more | 142.6 MB | 4.59× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 3.51 s | 3.27 s–6.18 s | 37.52× more | 776.8 MB | 25.00× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 450.1 ms | 433.2 ms–488.3 ms | 4.81× more | 121.3 MB | 3.90× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 3.33 s | 2.75 s–5.90 s | 35.54× more | 470.2 MB | 15.13× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 8.38 s | 6.45 s–10.63 s | 89.46× more | 866.0 MB | 27.86× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 84.3 ms | 71.1 ms–98.0 ms | baseline | 31.5 MB | baseline | — | yes | — | 11,142 / 9,255 (8 tools) |
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
| horon | 67.9 ms | 61.8 ms–84.6 ms | baseline | 24.5 MB | baseline | skill-uri | yes | — |
| jcode | 338.8 ms | 296.1 ms–582.8 ms | 4.99× more | 37.2 MB | 1.52× more | skill-tool | yes | — |
| Claude Code | 445.4 ms | 426.6 ms–513.7 ms | 6.56× more | 198.1 MB | 8.10× more | skill-tool | yes | — |
| Codex CLI | 441.8 ms | 412.2 ms–594.7 ms | 6.50× more | 143.2 MB | 5.86× more | file-read | yes | — |
| OpenCode | 4.73 s | 4.46 s–5.93 s | 69.57× more | 799.2 MB | 32.68× more | skill-tool | yes | — |
| pi | 585.5 ms | 531.8 ms–629.1 ms | 8.62× more | 134.5 MB | 5.50× more | file-read | yes | — |
| oh-my-pi (omp) | 2.19 s | 2.04 s–2.46 s | 32.22× more | 413.2 MB | 16.90× more | file-read | yes | — |
| oh-my-openagent (omo) | 8.33 s | 6.39 s–11.44 s | 122.69× more | 888.8 MB | 36.35× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 91.2 ms | 88.8 ms–142.2 ms | baseline | 41.8 MB | baseline | mcp-call | yes | 74.7 ms | — |
| jcode | 392.7 ms | 356.3 ms–440.2 ms | 4.30× more | 52.9 MB | 1.27× more | mcp-action | yes | 366.6 ms | — |
| Claude Code | 625.2 ms | 510.1 ms–646.2 ms | 6.85× more | 222.8 MB | 5.34× more | mcp-tool | yes | 471.4 ms | — |
| Codex CLI | 388.0 ms | 329.1 ms–484.8 ms | 4.25× more | 142.8 MB | 3.42× more | mcp-tool | yes | 252.8 ms | — |
| OpenCode | 3.84 s | 3.71 s–4.30 s | 42.09× more | 727.1 MB | 17.41× more | mcp-tool | yes | 3.27 s | — |
| pi | 657.8 ms | 616.2 ms–881.0 ms | 7.21× more | 138.2 MB | 3.31× more | mcp-tool | yes | 565.5 ms | — |
| oh-my-pi (omp) | 1.84 s | 1.55 s–2.04 s | 20.19× more | 412.5 MB | 9.88× more | mcp-route | yes | 1.53 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 29.0 MB | 130.9 KB | baseline | 0.45 | 1.00 | 35.0 MB | 340.9 KB |
| jcode | 36.5 MB | 207.0 KB | 1.58× more | — | — | 40.1 MB | 344.9 KB |
| Claude Code | 159.1 MB | 148.8 MB | 1,164× more | — | — | 190.2 MB | 181.8 MB |
| Codex CLI | 113.7 MB | 82.0 MB | 641.25× more | — | — | 107.3 MB | 107.7 MB |
| OpenCode | 243.5 MB | 2.6 MB | 20.41× more | — | — | 558.4 MB | 12.8 MB |
| pi | 120.5 MB | 116.9 MB | 914.28× more | — | — | 149.0 MB | 148.2 MB |
| oh-my-pi (omp) | 341.2 MB | 163.5 MB | 1,279× more | — | — | 464.5 MB | 310.1 MB |
| oh-my-openagent (omo) | 56.7 MB | 45.2 MB | 353.45× more | — | — | 831.3 MB | 319.8 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.0 MB | 36.6 MB | 38.0 MB |
| jcode | 40.1 MB | 41.4 MB | 43.1 MB |
| Claude Code | 190.2 MB | 933.6 MB | 1.8 GB |
| Codex CLI | 107.3 MB | 536.3 MB | 1.1 GB |
| OpenCode | 558.4 MB | 622.9 MB | 674.4 MB |
| pi | 149.0 MB | 740.1 MB | 1.4 GB |
| oh-my-pi (omp) | 464.5 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 831.3 MB | 2.9 GB | 3.7 GB |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    10.56 ms
  Max:    22.08 ms
  Mean:   13.68 ms
  Median: 12.89 ms
  Stdev:  3.16 ms

--version:
  Min:    11.13 ms
  Max:    22.84 ms
  Mean:   13.90 ms
  Median: 12.45 ms
  Stdev:  3.59 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    27.10 ms
  Max:    165.00 ms
  Mean:   80.13 ms
  Median: 79.91 ms
  Stdev:  44.15 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.2%
  logging_init        0.6       0.6   32.7%
  nofile_limit        1.1       0.5   24.6%
  perm_harden         1.1       0.0    2.5%
  args_parse          1.1       0.0    0.0%
  config_loaded       1.4       0.2   12.9%
  server_new          1.6       0.2   13.1%
  socket_bound        1.9       0.3   14.1%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    40.09 ms
  Max:    160.66 ms
  Mean:   109.12 ms
  Median: 116.94 ms
  Stdev:  43.39 ms

Bridge stage: hello_ok:
  Min:    40.72 ms
  Max:    161.68 ms
  Mean:   110.04 ms
  Median: 117.65 ms
  Stdev:  43.65 ms

Bridge stage: no-op reply:
  Min:    41.26 ms
  Max:    162.13 ms
  Mean:   110.61 ms
  Median: 118.12 ms
  Stdev:  43.68 ms

Bridge total (socket -> no-op):
  Min:    41.26 ms
  Max:    162.13 ms
  Mean:   110.61 ms
  Median: 118.12 ms
  Stdev:  43.68 ms

============================================================
Summary:
  Binary load median:      ~12.9 ms
  Server ready median:     ~79.9 ms
  Bridge total median:     ~118.1 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f436a434a010)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-vw269k4y
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.026    p95    0.076/0.074    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.326/0.250    p95    0.622/1.277    delta  -0.075 ms  control-no-change
  mcp_config_load   p50    0.324/0.313    p95    0.655/0.530    delta  -0.011 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.006    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.012/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   19.971/21.265   p95   31.416/49.735   delta  +1.295 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22954 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22954 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.977/6.733    p95    9.754/15.791   delta  +1.756 ms  control-no-change
  skill_request_overlay p50    7.888/10.328   p95   16.858/17.008   delta  +2.440 ms  control-no-change
  mcp_config_load   p50    0.333/0.298    p95    4.047/1.241    delta  -0.036 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   26.871/32.985   p95   38.967/130.751  delta  +6.114 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22950 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22958 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.026    p95    0.093/0.056    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.228/0.260    p95    0.614/0.594    delta  +0.031 ms  control-no-change
  mcp_config_load   p50    0.293/0.272    p95    0.712/0.638    delta  -0.021 ms  control-no-change
  attach_history    p50  361.874/346.623  p95  455.593/463.027  delta -15.252 ms  control-no-change
  resume_ready      p50   83.339/68.942   p95   98.523/93.703   delta -14.397 ms  control-no-change
  provider_dispatch p50    1.368/1.092    p95    2.512/1.732    delta  -0.276 ms  control-no-change
  session_save      p50   21.690/22.239   p95   52.587/49.749   delta  +0.549 ms  control-no-change
  total store_read_bytes p50 5311304  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189201..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099142..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22960 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22946..22964 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    5.883/4.578    p95    8.068/7.549    delta  -1.305 ms  control-no-change
  skill_request_overlay p50    7.216/6.087    p95   15.326/12.008   delta  -1.129 ms  control-no-change
  mcp_config_load   p50    0.285/0.277    p95    0.893/0.625    delta  -0.008 ms  control-no-change
  attach_history    p50  382.144/358.293  p95  443.192/410.265  delta -23.852 ms  control-no-change
  resume_ready      p50   74.600/82.474   p95  113.156/96.725   delta  +7.874 ms  control-no-change
  provider_dispatch p50    1.052/1.041    p95    2.195/1.295    delta  -0.011 ms  control-no-change
  session_save      p50   24.229/25.985   p95   33.604/44.697   delta  +1.757 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22946..22976 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22964 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.030/0.028    p95    0.036/0.073    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.238/0.229    p95    0.282/0.687    delta  -0.009 ms  control-no-change
  mcp_config_load   p50    0.277/0.411    p95    0.399/0.952    delta  +0.134 ms  control-drift
  attach_history    p50 4723.265/4645.659 p95 5360.487/5233.260 delta -77.606 ms  control-no-change
  resume_ready      p50 1006.184/968.793  p95 1183.687/1039.243 delta -37.390 ms  control-no-change
  provider_dispatch p50   14.464/11.379   p95   21.905/16.165   delta  -3.085 ms  control-no-change
  session_save      p50   47.282/27.973   p95  112.339/29.766   delta -19.309 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22971 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22971 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.519/5.413    p95    7.215/22.632   delta  +0.894 ms  control-no-change
  skill_request_overlay p50    6.213/7.952    p95   10.974/14.659   delta  +1.739 ms  control-no-change
  mcp_config_load   p50    0.281/0.311    p95    0.578/0.748    delta  +0.030 ms  control-no-change
  attach_history    p50 4548.510/4482.762 p95 5907.361/5348.775 delta -65.748 ms  control-no-change
  resume_ready      p50  946.558/889.013  p95 1200.717/1126.128 delta -57.546 ms  control-no-change
  provider_dispatch p50   14.226/15.704   p95   18.788/18.510   delta  +1.479 ms  control-no-change
  session_save      p50   31.534/28.966   p95   51.306/75.213   delta  -2.568 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210253..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22953..22971 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184297..42184305 (5 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210247..11210255 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22967 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f436a434a010)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-a32f1fmj
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.024/0.022    p95    0.037/0.028    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.218/0.210    p95    0.247/0.219    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.245/0.233    p95    0.263/0.251    delta  -0.012 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   11.059/10.761   p95   12.043/12.060   delta  -0.297 ms  control-no-change
  total store_read_bytes p50 23655  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22927, 22939, 22943, 22959, 23653, 23657, 23673, 24359, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22937, 22951, 22955, 23657, 23669, 23671, 24367, 24371, 24377]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22927..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22937..24377 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    3.364/3.480    p95    3.959/4.818    delta  +0.116 ms  control-no-change
  skill_request_overlay p50    4.379/4.389    p95    4.984/5.506    delta  +0.010 ms  control-no-change
  mcp_config_load   p50    0.229/0.231    p95    0.251/0.288    delta  +0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-drift
  session_save      p50   11.626/11.369   p95   12.812/16.761   delta  -0.258 ms  control-no-change
  total store_read_bytes p50 23662  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22945, 22947, 23657, 23659, 23665, 24361, 24373, 24375, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22945, 22947, 22951, 23659, 23671, 23681, 24369, 24375, 24385]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22945..24383 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22945..24385 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.024/0.025    p95    0.029/0.027    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.205/0.210    p95    0.275/0.248    delta  +0.004 ms  control-no-change
  mcp_config_load   p50    0.234/0.239    p95    0.305/0.271    delta  +0.005 ms  control-no-change
  attach_history    p50   88.308/95.209   p95  101.409/114.716  delta  +6.902 ms  control-no-change
  resume_ready      p50   61.370/59.404   p95   81.219/64.385   delta  -1.966 ms  control-no-change
  provider_dispatch p50    1.044/1.018    p95    1.141/1.235    delta  -0.026 ms  control-no-change
  session_save      p50   11.760/11.835   p95   21.064/23.123   delta  +0.075 ms  control-no-change
  total store_read_bytes p50 2841248  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23674, 24368, 24376, 25080, 25100, 25778, 25790, 26528]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23658, 23660, 24384, 24386, 25094, 25104, 25804, 25810, 26520, 26536]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26528 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23658..26536 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.569/3.370    p95    4.735/3.814    delta  -0.199 ms  control-no-change
  skill_request_overlay p50    4.625/4.329    p95    5.477/4.856    delta  -0.296 ms  control-no-change
  mcp_config_load   p50    0.240/0.231    p95    0.266/0.258    delta  -0.010 ms  control-no-change
  attach_history    p50   88.266/86.708   p95   91.831/91.706   delta  -1.558 ms  control-no-change
  resume_ready      p50   59.347/59.276   p95   60.302/63.850   delta  -0.071 ms  control-no-change
  provider_dispatch p50    1.062/1.039    p95    1.083/2.049    delta  -0.024 ms  control-no-change
  session_save      p50   13.597/13.284   p95   16.210/20.968   delta  -0.313 ms  control-no-change
  total store_read_bytes p50 2841253  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23668, 23676, 24378, 24382, 25088, 25102, 25806, 25824, 26524, 26528]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23661, 23669, 24379, 24391, 25095, 25105, 25803, 25809, 26519, 26521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23668..26528 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23661..26521 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.026/0.025    p95    0.031/0.046    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.208/0.227    p95    0.220/0.285    delta  +0.019 ms  control-no-change
  mcp_config_load   p50    0.239/0.250    p95    0.260/0.305    delta  +0.012 ms  control-no-change
  attach_history    p50 1083.293/989.526  p95 1485.675/1122.172 delta -93.766 ms  control-no-change
  resume_ready      p50  678.890/598.983  p95  903.966/881.845  delta -79.907 ms  control-no-change
  provider_dispatch p50    9.605/9.703    p95   17.382/13.231   delta  +0.098 ms  control-no-change
  session_save      p50   13.586/13.508   p95   23.344/83.296   delta  -0.078 ms  control-no-change
  total store_read_bytes p50 28640263  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23673, 24387, 24391, 25095, 25097, 25815, 25821, 26521, 26535]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23677, 24383, 24399, 25093, 25109, 25795, 25811, 26533, 26537]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26535 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26537 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.270/4.378    p95   10.765/6.621    delta  +0.109 ms  control-no-change
  skill_request_overlay p50    5.819/5.784    p95   10.564/9.749    delta  -0.035 ms  control-no-change
  mcp_config_load   p50    0.295/0.288    p95    0.368/0.703    delta  -0.007 ms  control-no-change
  attach_history    p50 1258.967/1288.654 p95 1691.201/1710.623 delta +29.687 ms  control-no-change
  resume_ready      p50  778.115/822.441  p95  989.330/1064.124 delta +44.326 ms  control-no-change
  provider_dispatch p50   11.740/11.354   p95   16.086/12.903   delta  -0.386 ms  control-no-change
  session_save      p50   22.551/28.926   p95   48.665/68.621   delta  +6.375 ms  control-no-change
  total store_read_bytes p50 28640267  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23657, 23675, 24379, 24395, 25085, 25099, 25811, 25825, 26531, 26533]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23673, 24391, 24395, 25105, 25107, 25805, 25815, 26535, 26537]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23657..26533 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26537 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.5 ms | 0.2 ms | 0.5 ms | 5 |
| read (1000 lines) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 1.0 ms | 0.5 ms | 1.0 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 3.0 ms | 4.3 ms | 2.2 ms | 4.3 ms | 5 |
| write (10KB) | 2.9 ms | 5.1 ms | 2.8 ms | 5.1 ms | 5 |
| edit (simple replace) | 2.9 ms | 3.7 ms | 2.5 ms | 3.7 ms | 5 |
| ls (scripts dir) | 1.0 ms | 2.4 ms | 0.9 ms | 2.4 ms | 5 |
| agentgrep (grep in tree) | 5.2 ms | 6.0 ms | 3.9 ms | 6.0 ms | 5 |
| agentgrep (find **/*.rs) | 1.4 ms | 1.6 ms | 1.4 ms | 1.6 ms | 5 |
| bash (true) | 5.7 ms | 6.3 ms | 4.8 ms | 6.3 ms | 5 |
| bash (echo hello) | 8.3 ms | 9.1 ms | 6.3 ms | 9.1 ms | 5 |
| bash (ls fixture tree) | 9.8 ms | 13.7 ms | 8.5 ms | 13.7 ms | 5 |
| invalid tool (dispatch only) | 0.2 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 58.0 ms | 58.0 ms | 58.0 ms | 58.1 ms | 0 |
| 10 | 216.3 ms | 217.0 ms | 217.0 ms | 217.2 ms | 0 |
| 40 | 1.37 s | 1.38 s | 1.38 s | 1.38 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.57 s | 1.57 s | 1.57 s | 1 |
| leaf_touch | src/main.rs | 1.21 s | 1.21 s | 1.21 s | 1 |
| core_touch | crates/app-core/src/lib.rs | 50.23 s | 50.23 s | 50.23 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 45.90 s | 45.90 s | 45.90 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (f7a53f1) | 11.7 ms | 96.9 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 12.6 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 41.5 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 90.7 ms | 479.4 MB |
| OpenCode | 1.19.5 | 461.0 ms | 605.3 MB |
| pi | 1.5.0 | 242.9 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 655.4 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 172.8 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 160.1 ms | 153.4 ms–230.9 ms | baseline | 35.5 MB | baseline | yes | 3,686 / 2,390 (1 tools) | — |
| jcode | 173.5 ms | 122.6 ms–207.6 ms | 1.08× more | 31.1 MB | 1.14× less | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.13 s | 1.12 s–1.84 s | 7.05× more | 273.8 MB | 7.71× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 2.00 s | 1.90 s–2.55 s | 12.52× more | 257.1 MB | 7.24× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 4.48 s | 4.43 s–7.30 s | 28.01× more | 796.3 MB | 22.43× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 426.6 ms | 414.1 ms–461.4 ms | 2.67× more | 125.7 MB | 3.54× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 3.09 s | 2.86 s–5.91 s | 19.32× more | 417.9 MB | 11.77× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 18.29 s | 16.40 s–24.54 s | 114.27× more | 527.8 MB | 14.86× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 162.0 ms | 156.8 ms–205.7 ms | baseline | 34.6 MB | baseline | — | yes | — | 11,209 / 9,255 (8 tools) |
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
| horon | 176.3 ms | 169.2 ms–220.6 ms | baseline | 25.0 MB | baseline | skill-uri | yes | — |
| jcode | 223.5 ms | 213.0 ms–253.4 ms | 1.27× more | 23.7 MB | 1.06× less | skill-tool | yes | — |
| Claude Code | 820.5 ms | 798.8 ms–1.05 s | 4.65× more | 238.5 MB | 9.54× more | skill-tool | yes | — |
| Codex CLI | 2.45 s | 2.43 s–3.40 s | 13.90× more | 253.2 MB | 10.13× more | file-read | yes | — |
| OpenCode | 7.67 s | 7.60 s–8.21 s | 43.51× more | 773.4 MB | 30.94× more | skill-tool | yes | — |
| pi | 584.0 ms | 564.8 ms–602.6 ms | 3.31× more | 97.8 MB | 3.91× more | file-read | yes | — |
| oh-my-pi (omp) | 2.96 s | 2.56 s–15.56 s | 16.76× more | 390.4 MB | 15.62× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.07 s | 18.38 s–22.34 s | 119.49× more | 547.7 MB | 21.91× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 323.4 ms | 220.0 ms–349.5 ms | baseline | 37.5 MB | baseline | mcp-call | yes | 153.3 ms | — |
| jcode | 338.9 ms | 332.8 ms–383.8 ms | 1.05× more | 36.7 MB | same | mcp-action | yes | 223.8 ms | — |
| Claude Code | 940.6 ms | 879.0 ms–1.23 s | 2.91× more | 274.1 MB | 7.32× more | mcp-tool | yes | 804.2 ms | — |
| Codex CLI | 904.1 ms | 816.3 ms–3.74 s | 2.80× more | 177.6 MB | 4.74× more | mcp-tool | yes | 672.1 ms | — |
| OpenCode | 7.28 s | 6.98 s–9.52 s | 22.52× more | 808.9 MB | 21.60× more | mcp-tool | yes | 6.00 s | — |
| pi | 644.6 ms | 623.4 ms–739.5 ms | 1.99× more | 116.9 MB | 3.12× more | mcp-tool | yes | 507.5 ms | — |
| oh-my-pi (omp) | 5.11 s | 3.39 s–17.05 s | 15.79× more | 431.6 MB | 11.52× more | mcp-route | yes | 3.45 s | — |
| oh-my-openagent (omo) | 21.09 s | 19.77 s–26.74 s | 65.22× more | 564.5 MB | 15.07× more | mcp-tool | yes | 18.04 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.5 MB | 125.5 KB | baseline | — | — | 29.9 MB | 452.7 KB |
| jcode | 22.0 MB | 60.7 KB | 2.07× less | — | — | 27.1 MB | 321.5 KB |
| Claude Code | 154.0 MB | 179.5 MB | 1,464× more | — | — | 187.5 MB | 188.9 MB |
| Codex CLI | 145.2 MB | 115.4 MB | 941.48× more | — | — | 164.7 MB | 117.1 MB |
| OpenCode | 243.7 MB | 14.8 MB | 120.91× more | — | — | 428.1 MB | 26.7 MB |
| pi | 91.1 MB | 90.8 MB | 740.47× more | — | — | 101.0 MB | 100.9 MB |
| oh-my-pi (omp) | 307.6 MB | 156.5 MB | 1,276× more | — | — | 342.4 MB | 336.1 MB |
| oh-my-openagent (omo) | 145.2 MB | 132.2 MB | 1,078× more | — | — | 412.4 MB | 405.3 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.9 MB | 32.3 MB | 33.9 MB |
| jcode | 27.1 MB | 28.7 MB | 29.9 MB |
| Claude Code | 187.5 MB | 945.5 MB | 1.8 GB |
| Codex CLI | 164.7 MB | 648.5 MB | 1.2 GB |
| OpenCode | 428.1 MB | 491.9 MB | 665.1 MB |
| pi | 101.0 MB | 505.0 MB | 1009.1 MB |
| oh-my-pi (omp) | 342.4 MB | 1.6 GB | 3.3 GB |
| oh-my-openagent (omo) | 412.4 MB | 2.1 GB | 4.0 GB |

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    10.74 ms
  Max:    11.39 ms
  Mean:   11.13 ms
  Median: 11.18 ms
  Stdev:  0.23 ms

--version:
  Min:    10.45 ms
  Max:    11.27 ms
  Mean:   10.81 ms
  Median: 10.82 ms
  Stdev:  0.25 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    16.79 ms
  Max:    22.43 ms
  Mean:   21.54 ms
  Median: 21.98 ms
  Stdev:  1.68 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.3       1.3   23.7%
  nofile_limit        1.7       0.4    7.6%
  perm_harden         3.7       1.9   35.4%
  args_parse          3.7       0.0    0.1%
  config_loaded       4.5       0.8   14.5%
  server_new          5.2       0.7   13.1%
  socket_bound        5.5       0.3    5.5%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~11.2 ms
  Server ready median:     ~22.0 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (7295f759c74d)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-8w4vugq9
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.103/0.104    p95    0.110/0.186    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.746/0.741    p95    0.964/0.818    delta  -0.006 ms  control-no-change
  mcp_config_load   p50    0.961/0.933    p95    1.012/1.002    delta  -0.027 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   46.873/48.026   p95   65.201/50.967   delta  +1.152 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22894..22964 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22904..22960 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   14.771/14.862   p95   15.534/19.116   delta  +0.091 ms  control-no-change
  skill_request_overlay p50   39.714/39.225   p95   42.920/51.308   delta  -0.488 ms  control-no-change
  mcp_config_load   p50    1.011/1.022    p95    1.041/1.141    delta  +0.010 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   56.496/52.860   p95  133.406/86.507   delta  -3.636 ms  control-no-change
  total store_read_bytes p50 22936  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22918..22956 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22906..22968 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.100/0.104    p95    0.104/0.172    delta  +0.004 ms  control-no-change
  skill_request_overlay p50    0.767/0.849    p95    0.794/1.035    delta  +0.082 ms  control-no-change
  mcp_config_load   p50    0.969/0.936    p95    1.070/1.122    delta  -0.033 ms  control-no-change
  attach_history    p50  429.774/430.257  p95  444.617/453.461  delta  +0.483 ms  control-no-change
  resume_ready      p50  152.559/149.972  p95  167.925/158.242  delta  -2.587 ms  control-no-change
  provider_dispatch p50    1.512/1.482    p95    1.880/1.610    delta  -0.030 ms  control-no-change
  session_save      p50   40.806/47.209   p95   52.242/137.938  delta  +6.403 ms  control-no-change
  total store_read_bytes p50 5311208  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22919..22979 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22927..22972 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.748/15.887   p95   16.859/18.976   delta  +0.139 ms  control-no-change
  skill_request_overlay p50   39.635/40.389   p95   45.058/42.097   delta  +0.754 ms  control-no-change
  mcp_config_load   p50    1.038/1.020    p95    1.135/1.107    delta  -0.018 ms  control-no-change
  attach_history    p50  433.860/436.367  p95  447.435/555.423  delta  +2.507 ms  control-no-change
  resume_ready      p50  150.434/151.553  p95  160.621/157.715  delta  +1.119 ms  control-no-change
  provider_dispatch p50    1.469/1.468    p95    2.033/1.534    delta  -0.001 ms  control-no-change
  session_save      p50   55.128/43.492   p95  120.998/87.106   delta -11.636 ms  control-no-change
  total store_read_bytes p50 5311207  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189149..4189153 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099101..1099105 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22931..22971 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189148..4189153 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099100..1099105 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22935..22971 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.100/0.106    p95    0.108/0.147    delta  +0.006 ms  control-no-change
  skill_request_overlay p50    0.772/0.757    p95    0.880/0.883    delta  -0.015 ms  control-no-change
  mcp_config_load   p50    0.988/0.968    p95    1.075/1.082    delta  -0.020 ms  control-no-change
  attach_history    p50 4877.090/4907.949 p95 5094.289/5212.117 delta +30.859 ms  control-no-change
  resume_ready      p50 1585.274/1580.715 p95 1702.828/1619.578 delta  -4.559 ms  control-no-change
  provider_dispatch p50   16.346/16.025   p95   19.857/19.064   delta  -0.320 ms  control-no-change
  session_save      p50   54.683/44.183   p95  411.835/343.526  delta -10.500 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22923..22983 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184250..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210213..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22919..22975 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.234/15.476   p95   18.190/18.367   delta  +0.242 ms  control-no-change
  skill_request_overlay p50   39.314/40.958   p95   42.805/44.010   delta  +1.644 ms  control-no-change
  mcp_config_load   p50    1.010/1.022    p95    1.125/1.392    delta  +0.012 ms  control-no-change
  attach_history    p50 4921.730/4923.471 p95 5033.648/5173.808 delta  +1.742 ms  control-no-change
  resume_ready      p50 1583.760/1589.096 p95 1599.248/1604.569 delta  +5.336 ms  control-no-change
  provider_dispatch p50   16.601/17.144   p95   18.788/19.319   delta  +0.542 ms  control-no-change
  session_save      p50   43.856/65.408   p95  240.469/416.159  delta +21.552 ms  control-no-change
  total store_read_bytes p50 53417430  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22922..22979 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22927..22975 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (7295f759c74d)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-z6anpfmv
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.105/0.099    p95    0.127/0.116    delta  -0.006 ms  control-no-change
  skill_request_overlay p50    0.753/0.789    p95    0.827/0.858    delta  +0.035 ms  control-no-change
  mcp_config_load   p50    0.917/0.895    p95    1.035/0.971    delta  -0.022 ms  control-no-change
  attach_history    p50    0.002/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   33.444/33.585   p95   47.944/36.097   delta  +0.142 ms  control-no-change
  total store_read_bytes p50 23647  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22913, 22939, 22953, 23642, 23646, 23648, 24325, 24329, 24333, 24359]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22927, 22938, 22943, 22949, 23622, 23629, 23664, 24318, 24330, 24341]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22913..24359 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22927..24341 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   14.481/14.424   p95   14.695/14.987   delta  -0.057 ms  control-no-change
  skill_request_overlay p50   36.918/37.132   p95   37.559/38.259   delta  +0.214 ms  control-no-change
  mcp_config_load   p50    0.944/0.931    p95    1.020/0.994    delta  -0.013 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   38.826/38.001   p95  258.359/81.518   delta  -0.825 ms  control-no-change
  total store_read_bytes p50 23648  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22939, 22953, 22957, 22969, 23642, 23654, 23664, 24329, 24339, 24361]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22930, 22931, 22951, 22957, 23628, 23647, 23648, 24323, 24357, 24358]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22939..24361 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22930..24358 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.103/0.102    p95    0.156/0.226    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.779/0.837    p95    0.843/1.203    delta  +0.058 ms  control-no-change
  mcp_config_load   p50    0.917/0.932    p95    0.967/1.180    delta  +0.016 ms  control-no-change
  attach_history    p50  202.836/200.081  p95  223.993/214.433  delta  -2.755 ms  control-no-change
  resume_ready      p50  151.162/152.660  p95  162.692/167.606  delta  +1.498 ms  control-no-change
  provider_dispatch p50    1.460/1.473    p95    1.516/1.539    delta  +0.012 ms  control-no-change
  session_save      p50   33.032/34.599   p95 2379.986/38.683   delta  +1.567 ms  control-no-change
  total store_read_bytes p50 2841114  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23645, 23675, 24364, 24370, 25031, 25036, 25722, 25743, 26438, 26439]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23645, 23675, 24353, 24372, 25031, 25052, 25766, 25767, 26449, 26450]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23645..26439 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23645..26450 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.538/14.646   p95   16.800/15.272   delta  +0.108 ms  control-no-change
  skill_request_overlay p50   36.967/37.227   p95   38.035/37.903   delta  +0.260 ms  control-no-change
  mcp_config_load   p50    0.946/0.962    p95    1.044/1.060    delta  +0.016 ms  control-no-change
  attach_history    p50  201.567/202.262  p95  203.749/205.193  delta  +0.695 ms  control-no-change
  resume_ready      p50  150.555/150.133  p95  155.278/154.122  delta  -0.422 ms  control-no-change
  provider_dispatch p50    1.448/1.443    p95    1.541/1.613    delta  -0.005 ms  control-no-change
  session_save      p50   32.692/38.941   p95   85.215/43.043   delta  +6.249 ms  control-drift
  total store_read_bytes p50 2841138  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23653, 23675, 24342, 24350, 25055, 25061, 25710, 25754, 26425, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23635, 23648, 24340, 24357, 25029, 25056, 25748, 25759, 26427, 26442]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23653..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23635..26442 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.099/0.100    p95    0.121/0.129    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.820/0.778    p95    0.878/1.042    delta  -0.042 ms  control-no-change
  mcp_config_load   p50    0.915/0.945    p95    1.001/1.178    delta  +0.030 ms  control-no-change
  attach_history    p50 2104.557/2120.867 p95 2168.053/2140.203 delta +16.310 ms  control-no-change
  resume_ready      p50 1586.920/1573.747 p95 1601.251/1607.933 delta -13.173 ms  control-no-change
  provider_dispatch p50   17.904/17.870   p95   18.536/18.280   delta  -0.034 ms  control-no-change
  session_save      p50   35.850/35.519   p95   54.451/2972.025 delta  -0.331 ms  control-no-change
  total store_read_bytes p50 28640146  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23614, 23651, 24341, 24355, 25035, 25067, 25742, 25768, 26441, 26463]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23673, 24328, 24360, 25044, 25060, 25759, 25765, 26440, 26470]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23614..26463 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26470 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.145/13.932   p95   14.546/16.601   delta  -0.213 ms  control-no-change
  skill_request_overlay p50   36.944/36.895   p95   37.564/42.977   delta  -0.049 ms  control-no-change
  mcp_config_load   p50    0.991/0.951    p95    1.052/1.160    delta  -0.039 ms  control-no-change
  attach_history    p50 2108.544/2096.896 p95 2120.841/2113.263 delta -11.648 ms  control-no-change
  resume_ready      p50 1569.082/1571.582 p95 1576.904/1578.862 delta  +2.501 ms  control-no-change
  provider_dispatch p50   17.306/17.185   p95   18.044/17.768   delta  -0.122 ms  control-no-change
  session_save      p50   33.965/35.224   p95   38.214/47.220   delta  +1.259 ms  control-no-change
  total store_read_bytes p50 28640142  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23668, 23670, 24327, 24361, 25023, 25068, 25744, 25751, 26443, 26454]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23650, 23662, 24365, 24367, 25052, 25076, 25738, 25750, 26451, 26467]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23668..26454 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23650..26467 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 7.9 ms | 10.5 ms | 7.4 ms | 10.5 ms | 5 |
| write (10KB) | 7.4 ms | 10.1 ms | 7.4 ms | 10.1 ms | 5 |
| edit (simple replace) | 7.4 ms | 10.4 ms | 7.4 ms | 10.4 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.4 ms | 7.5 ms | 5.3 ms | 7.5 ms | 5 |
| agentgrep (find **/*.rs) | 4.9 ms | 5.3 ms | 4.8 ms | 5.3 ms | 5 |
| bash (true) | 10.5 ms | 13.1 ms | 10.0 ms | 13.1 ms | 5 |
| bash (echo hello) | 10.5 ms | 10.5 ms | 10.1 ms | 10.5 ms | 5 |
| bash (ls fixture tree) | 15.0 ms | 17.9 ms | 12.9 ms | 17.9 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 230.8 ms | 230.8 ms | 230.8 ms | 230.9 ms | 0 |
| 10 | 725.8 ms | 729.2 ms | 729.2 ms | 730.0 ms | 0 |
| 40 | 2.64 s | 2.67 s | 3.28 s | 3.28 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 584.3 ms | 584.3 ms | 584.3 ms | 1 |
| leaf_touch | src\main.rs | 807.7 ms | 807.7 ms | 807.7 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 22.48 s | 22.48 s | 22.48 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 32.03 s | 32.03 s | 32.03 s | 1 |

## TUI (`horon-tui`)

The interactive client is a second binary and is measured on its own. These numbers come from the `tui` suite and are never compared with the headless tables above: `first_frame_ms` and `direct_first_frame_ms` are two launches of two different programs, and the handoff is the difference between them.

![TUI (`horon-tui`)](charts/tui-01-latency.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 26.0 ms | 25.7 ms–27.8 ms | 5 |
| First frame (direct `horon-tui`) | 26.8 ms | 26.6 ms–27.0 ms | 5 |
| TTY handoff cost | -0.8 ms | -1.1 ms–0.8 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.2 ms–1.4 ms | 50 |
| Idle RSS after 5 s | 5.4 MB | 5.4 MB–5.5 MB | 5 |
| RSS after one turn | 5.6 MB | 5.6 MB–5.7 MB | 5 |
| Frame draw time (p50) | 1.5 ms | 1.5 ms–1.5 ms | 5 |
| Frame draw time (p95) | 1.6 ms | 1.6 ms–1.6 ms | 5 |
| Install size (`horon-tui`) | 6.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 82.8 ms | 51.1 ms–489.1 ms | 5 |
| First frame (direct `horon-tui`) | 65.5 ms | 56.3 ms–102.6 ms | 5 |
| TTY handoff cost | -5.2 ms | -21.0 ms–423.6 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.6 ms | 50 |
| Idle RSS after 5 s | 8.8 MB | 8.7 MB–8.9 MB | 5 |
| RSS after one turn | 9.0 MB | 8.9 MB–9.1 MB | 5 |
| Frame draw time (p50) | 1.7 ms | 1.3 ms–1.8 ms | 5 |
| Frame draw time (p95) | 3.2 ms | 3.0 ms–3.4 ms | 5 |
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
| Frame draw time (p50) | 2.0 ms | 1.9 ms–2.0 ms | 5 |
| Frame draw time (p95) | 2.5 ms | 2.3 ms–3.7 ms | 5 |
| Install size (`horon-tui`) | 5.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
