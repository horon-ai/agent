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
| Attach/detach soak, resources after 200 cycles | `RESOURCES` | fd 14, threads 25, tasks 2,141, `shared_context` 0 | fd 14, threads 22, tasks 2,141, `shared_context` 0 |

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
| Idle session eviction policy | not adopted | 0.999 fds and 0.998 threads per session, 0.41 MB RSS. The slope does not bend from 1 to 1,000 sessions. The `shared_context` entry and the debug job are 0 at all five population counts, and the soak returns the session count to 0 |
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
- **The 200-cycle soak resolves one leaked resource per 20 cycles.** The plan asked for 1,000 cycles and that item is `[~]`. The 1,000-cycle variant is in the repository behind `#[ignore]`.
- **The structural gate does not assert the frame limit.** The counters the gate reads include no frame count. The frame limit is held by `crates/harness-api-server/src/framing_tests.rs`.
- **`shared_context` has no removal path anywhere in the tree** (`crates/app-core/src/server.rs:771`). It measured 0 at every population count, but that 0 means "nobody used it", not "nothing leaks". The soak only checks that this map does not grow monotonically either.
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

The soak has a 200-cycle default form and a 1,000-cycle variant behind `#[ignore]`.

```bash
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test --locked -p horon-agent --test e2e soak_attach_detach::soak_attach_detach_returns_every_counter_to_baseline -- --exact --nocapture
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test -p horon-agent --test e2e soak_attach_detach_1000_cycles -- --ignored --exact --nocapture
```

How to use the structural gate and the A/B runner, the required keys of the report template, and the procedure for raising a bound are in [`docs/performance/BENCHMARK.md`](BENCHMARK.md). The gate command is `python3 scripts/perf/check_structural.py` and the A/B runner is `scripts/perf/ab_perf.py`.

## Status

The plan has 17 items: 13 are done and 4 are partially done (`[~]`). Partially done means the following.

| Item | Status | Meaning |
| --- | --- | --- |
| 7. NDJSON framing caps for both SDKs | `[~]` | The code and tests are on `dev` and verified. The evidence file this item pointed at disappeared with a worktree and was not reconstructed. The behavior is proven, the evidence trail is not |
| 12. Auxiliary and result cache byte accounting | `[~]` | The code is on `dev`, and the lead re-ran the decisive lines and reproduced them. The lane's raw evidence file is gone, and the file now in its place is a reconstruction that labels itself `recovered` |
| 15. Attach/detach soak | `[~]` | The soak itself is valid but ran 200 cycles rather than the 1,000 the plan required. The 1,000-cycle variant is in the repository behind `#[ignore]` |
| 17. CI structural gate and report template | `[~]` | The gate really runs (35 bounds, `EXIT=0`, `EXIT=1` on violation, `EXIT=2` when a measurement is missing, 40 contract tests). Two conditions from the plan remain. There is no frame count counter, and "passes at B0" is structurally impossible (the code that prints the `STRUCTURAL_PERF` line landed after B0) |

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

one-shot is the median of a single headless prompt (startup, one `ls` tool round trip, final response). start→first request runs from connecting to the api-bridge socket to the first `list_sessions` response. fsync is the number of fsync and fdatasync calls a single one-shot makes. Memory is the idle RSS of the daemon process tree.

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

## Automatic measurement: latest measured commit on `main`

When a commit on `main` passes both CI and the structural performance gate, and the agent code (`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`) has changed since the previous measurement, `.github/workflows/benchmark-docs.yml` runs the full benchmark suite separately per OS on Linux, macOS and Windows and rewrites the section below on both `main` and `dev`. There is one section per OS. How the measurement works and what each suite means are in "Automatic measurement" in [`BENCHMARK.md`](BENCHMARK.md).

<!-- bench-auto:start commit=775c49115a6d550215a0ca7cc06fcd31f0838bcc -->

## Headless (`horon-agent`)

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `775c49115a6d550215a0ca7cc06fcd31f0838bcc`
- Measured at: 2026-11-13 21:15 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/43499875915> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (775c491)` | Success |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (775c491)` | Success |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (775c491)` | Success |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (775c491) | 5.7 ms | 115.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 14.4 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 12.2 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 53.0 ms | 435.0 MB |
| OpenCode | 1.19.5 | 494.8 ms | 613.4 MB |
| pi | 1.5.0 | 261.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 736.0 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 136.9 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 67.9 ms | 65.0 ms–1.07 s | baseline | 40.6 MB | baseline | yes | 10 | — |
| jcode | 157.8 ms | 156.3 ms–162.2 ms | 2.33× more | 38.8 MB | 1.05× less | yes | 5 | — |
| Claude Code | 556.3 ms | 533.1 ms–607.2 ms | 8.20× more | 206.0 MB | 5.07× more | yes | — | — |
| Codex CLI | 592.6 ms | 550.9 ms–625.0 ms | 8.73× more | 191.3 MB | 4.71× more | yes | 6 | — |
| OpenCode | 3.12 s | 2.97 s–3.26 s | 46.03× more | 572.0 MB | 14.09× more | yes | 4 | — |
| pi | 437.3 ms | 429.8 ms–463.3 ms | 6.44× more | 115.4 MB | 2.84× more | yes | — | — |
| oh-my-pi (omp) | 2.24 s | 2.18 s–2.43 s | 33.07× more | 394.5 MB | 9.72× more | yes | 71 | — |
| oh-my-openagent (omo) | 6.34 s | 6.26 s–6.97 s | 93.42× more | 653.5 MB | 16.10× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 61.0 ms | 60.4 ms–62.5 ms | baseline | 32.5 MB | baseline | skill-uri | yes | — |
| jcode | 73.2 ms | 71.1 ms–87.0 ms | 1.20× more | 42.4 MB | 1.30× more | skill-tool | yes | — |
| Claude Code | 523.5 ms | 517.7 ms–543.8 ms | 8.58× more | 206.3 MB | 6.35× more | skill-tool | yes | — |
| Codex CLI | 664.5 ms | 614.1 ms–716.0 ms | 10.89× more | 196.3 MB | 6.04× more | file-read | yes | — |
| OpenCode | 3.50 s | 3.47 s–3.52 s | 57.31× more | 614.7 MB | 18.92× more | skill-tool | yes | — |
| pi | 574.9 ms | 563.0 ms–580.5 ms | 9.42× more | 125.8 MB | 3.87× more | file-read | yes | — |
| oh-my-pi (omp) | 2.14 s | 1.90 s–2.25 s | 35.08× more | 381.8 MB | 11.75× more | file-read | yes | — |
| oh-my-openagent (omo) | 7.20 s | 6.97 s–7.39 s | 118.04× more | 662.5 MB | 20.39× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 94.3 ms | 92.8 ms–101.1 ms | baseline | 51.3 MB | baseline | mcp-call | yes | 71.6 ms | — |
| jcode | 194.4 ms | 194.2 ms–195.3 ms | 2.06× more | 55.1 MB | 1.07× more | mcp-action | yes | 155.9 ms | — |
| Claude Code | 649.2 ms | 630.0 ms–678.4 ms | 6.88× more | 227.2 MB | 4.43× more | mcp-tool | yes | 467.6 ms | — |
| Codex CLI | 585.5 ms | 560.8 ms–604.4 ms | 6.21× more | 195.2 MB | 3.80× more | mcp-tool | yes | 384.3 ms | — |
| OpenCode | 3.23 s | 3.16 s–3.27 s | 34.23× more | 560.6 MB | 10.92× more | mcp-tool | yes | 2.90 s | — |
| pi | 619.9 ms | 612.9 ms–644.5 ms | 6.57× more | 138.6 MB | 2.70× more | mcp-tool | yes | 550.3 ms | — |
| oh-my-pi (omp) | 2.23 s | 1.89 s–2.24 s | 23.64× more | 391.7 MB | 7.63× more | mcp-route | yes | 1.82 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 37.2 MB | 106.3 KB | baseline | 0.34 | 1.00 | 44.5 MB | 438.6 KB |
| jcode | 42.5 MB | 90.2 KB | 1.18× less | 0.00 | — | 46.1 MB | 255.7 KB |
| Claude Code | 150.1 MB | 146.2 MB | 1,408× more | — | — | 185.3 MB | 180.5 MB |
| Codex CLI | 129.4 MB | 132.2 MB | 1,273× more | — | — | 162.3 MB | 163.8 MB |
| OpenCode | 242.0 MB | 1.4 MB | 13.17× more | — | — | 451.5 MB | 5.0 MB |
| pi | 107.8 MB | 108.7 MB | 1,047× more | — | — | 132.7 MB | 125.4 MB |
| oh-my-pi (omp) | 338.8 MB | 70.1 MB | 675.26× more | — | — | 365.0 MB | 357.2 MB |
| oh-my-openagent (omo) | 330.3 MB | 30.0 MB | 289.34× more | — | — | 617.7 MB | 574.3 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.5 MB | 46.9 MB | 48.4 MB |
| jcode | 46.1 MB | 47.5 MB | 48.4 MB |
| Claude Code | 185.3 MB | 910.3 MB | 1.8 GB |
| Codex CLI | 162.3 MB | 809.6 MB | 1.6 GB |
| OpenCode | 451.5 MB | 521.9 MB | 499.8 MB |
| pi | 132.7 MB | 621.6 MB | 1.2 GB |
| oh-my-pi (omp) | 365.0 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 617.7 MB | 3.0 GB | 5.7 GB |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    2.90 ms
  Max:    3.21 ms
  Mean:   3.01 ms
  Median: 2.99 ms
  Stdev:  0.08 ms

--version:
  Min:    2.81 ms
  Max:    3.44 ms
  Mean:   2.94 ms
  Median: 2.87 ms
  Stdev:  0.19 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.40 ms
  Max:    20.58 ms
  Mean:   20.49 ms
  Median: 20.48 ms
  Stdev:  0.05 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.54 ms
  Max:    20.65 ms
  Mean:   20.60 ms
  Median: 20.60 ms
  Stdev:  0.04 ms

Bridge stage: hello_ok:
  Min:    21.01 ms
  Max:    21.26 ms
  Mean:   21.15 ms
  Median: 21.16 ms
  Stdev:  0.07 ms

Bridge stage: no-op reply:
  Min:    21.29 ms
  Max:    21.56 ms
  Mean:   21.46 ms
  Median: 21.48 ms
  Stdev:  0.08 ms

Bridge total (socket -> no-op):
  Min:    21.29 ms
  Max:    21.56 ms
  Mean:   21.46 ms
  Median: 21.48 ms
  Stdev:  0.08 ms

============================================================
Summary:
  Binary load median:      ~3.0 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.5 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (993cf2055188)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-3orrr93g
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.039    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.052/0.053    p95    0.053/0.053    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    4.991/4.765    p95    8.526/5.154    delta  -0.226 ms  control-no-change
  total store_read_bytes p50 23068  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23056..23092 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23080 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.196/1.181    p95    1.223/1.213    delta  -0.015 ms  control-no-change
  skill_request_overlay p50    1.820/1.821    p95    1.847/1.851    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.065/0.064    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.784/7.108    p95    8.652/8.714    delta  +0.324 ms  control-no-change
  total store_read_bytes p50 23078  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23092 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23084 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.005/0.005    p95    0.006/0.009    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.038/0.039    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.078/0.074    delta  -0.000 ms  control-no-change
  attach_history    p50  189.503/189.835  p95  192.513/194.001  delta  +0.332 ms  control-no-change
  resume_ready      p50   27.145/27.200   p95   27.288/27.371   delta  +0.055 ms  control-no-change
  provider_dispatch p50    0.931/0.928    p95    0.955/0.947    delta  -0.003 ms  control-no-change
  session_save      p50    5.008/4.988    p95    7.899/6.584    delta  -0.021 ms  control-no-change
  total store_read_bytes p50 5311413  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23074..23090 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23086 (5 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.172/1.179    p95    1.187/1.241    delta  +0.007 ms  control-no-change
  skill_request_overlay p50    1.837/1.829    p95    1.862/1.863    delta  -0.009 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.068/0.064    delta  -0.000 ms  control-no-change
  attach_history    p50  189.892/189.842  p95  193.014/191.941  delta  -0.051 ms  control-no-change
  resume_ready      p50   27.429/27.444   p95   27.807/27.598   delta  +0.015 ms  control-no-change
  provider_dispatch p50    0.921/0.943    p95    0.952/0.967    delta  +0.021 ms  control-no-change
  session_save      p50    4.489/4.430    p95    4.960/6.269    delta  -0.059 ms  control-no-change
  total store_read_bytes p50 5311407  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23070..23090 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23058..23090 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.005/0.005    p95    0.007/0.006    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.048/0.050    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.054/0.054    p95    0.062/0.062    delta  -0.000 ms  control-no-change
  attach_history    p50 2396.794/2422.417 p95 2445.801/2513.228 delta +25.623 ms  control-no-change
  resume_ready      p50  321.755/323.261  p95  332.752/330.479  delta  +1.506 ms  control-no-change
  provider_dispatch p50   11.015/11.007   p95   11.301/11.299   delta  -0.009 ms  control-no-change
  session_save      p50    4.776/5.123    p95    8.085/7.097    delta  +0.347 ms  control-no-change
  total store_read_bytes p50 53417620  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23073..23089 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23067..23085 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.189/1.182    p95    1.228/1.213    delta  -0.007 ms  control-no-change
  skill_request_overlay p50    1.814/1.810    p95    1.836/1.827    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.062/0.066    delta  -0.000 ms  control-no-change
  attach_history    p50 2461.576/2449.519 p95 2481.889/2481.179 delta -12.058 ms  control-no-change
  resume_ready      p50  326.865/328.593  p95  331.207/332.479  delta  +1.728 ms  control-no-change
  provider_dispatch p50   11.103/11.187   p95   11.497/11.477   delta  +0.083 ms  control-no-change
  session_save      p50    4.946/5.051    p95    5.263/8.915    delta  +0.104 ms  control-no-change
  total store_read_bytes p50 53417618  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23075..23087 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23073..23085 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (993cf2055188)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-xjnvs01_
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.013/0.012    p95    0.028/0.013    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.096/0.096    p95    0.115/0.096    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.128/0.128    p95    0.159/0.129    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    6.804/6.630    p95   75.105/54.692   delta  -0.174 ms  control-no-change
  total store_read_bytes p50 23798  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23065, 23077, 23093, 23796, 23800, 24511, 24515, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23073, 23077, 23788, 23804, 23812, 24511, 24515, 24519, 24527]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23065..24523 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24527 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.394/2.423    p95    2.481/2.462    delta  +0.030 ms  control-no-change
  skill_request_overlay p50    3.875/3.897    p95    4.010/3.932    delta  +0.022 ms  control-no-change
  mcp_config_load   p50    0.134/0.134    p95    0.141/0.154    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   17.278/11.673   p95  156.312/60.178   delta  -5.605 ms  control-no-change
  total store_read_bytes p50 23803  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23059, 23069, 23073, 23788, 23798, 23808, 24505, 24515, 24517, 24519]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23077, 23081, 23085, 23780, 23788, 23796, 24511, 24515, 24519]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23059..24519 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23077..24519 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.013/0.013    p95    0.014/0.014    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.092/0.092    p95    0.096/0.100    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.124/0.124    p95    0.128/0.139    delta  +0.000 ms  control-no-change
  attach_history    p50   80.541/81.206   p95   82.166/81.703   delta  +0.666 ms  control-no-change
  resume_ready      p50   44.167/44.080   p95   44.514/44.357   delta  -0.087 ms  control-no-change
  provider_dispatch p50    1.055/1.058    p95    1.070/1.071    delta  +0.004 ms  control-no-change
  session_save      p50   40.968/7.165    p95  259.829/259.930  delta -33.803 ms  control-no-change
  total store_read_bytes p50 2841371  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23792, 23800, 24507, 24515, 25232, 25234, 25949, 25955, 26662, 26668]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23798, 23802, 24513, 24517, 25224, 25228, 25234, 25935, 25951, 26668]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23792..26668 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23798..26668 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.429/2.425    p95    3.576/2.528    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    3.901/3.891    p95    5.593/4.274    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.136/0.134    p95    0.153/0.136    delta  -0.002 ms  control-no-change
  attach_history    p50   81.958/81.299   p95   94.453/103.070  delta  -0.659 ms  control-no-change
  resume_ready      p50   44.406/44.691   p95   45.090/50.858   delta  +0.285 ms  control-no-change
  provider_dispatch p50    1.052/1.046    p95    1.061/1.119    delta  -0.006 ms  control-no-change
  session_save      p50   15.118/8.769    p95   80.916/77.643   delta  -6.349 ms  control-no-change
  total store_read_bytes p50 2841370  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23785, 23809, 24502, 24516, 25235, 25241, 25940, 25958, 26669, 26675]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23783, 23801, 24516, 24522, 25235, 25239, 25950, 25956, 26673]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23785..26675 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23783..26673 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.013/0.013    p95    0.014/0.021    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.096/0.096    p95    0.106/0.115    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.128/0.128    p95    0.143/0.142    delta  -0.000 ms  control-no-change
  attach_history    p50  947.886/948.528  p95  975.219/957.461  delta  +0.643 ms  control-no-change
  resume_ready      p50  532.063/533.207  p95  553.004/552.339  delta  +1.144 ms  control-no-change
  provider_dispatch p50   14.091/13.708   p95   15.671/15.279   delta  -0.383 ms  control-no-change
  session_save      p50    6.888/6.970    p95  218.624/26.814   delta  +0.081 ms  control-no-change
  total store_read_bytes p50 28640390  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 23812, 24515, 24517, 25226, 25240, 25953, 25967, 26662, 26676]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23790, 23796, 24513, 24519, 25236, 25242, 25957, 25963, 26674, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26676 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23790..26680 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.451/2.445    p95    2.509/2.468    delta  -0.006 ms  control-no-change
  skill_request_overlay p50    3.936/3.927    p95    4.125/4.071    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.138/0.134    p95    0.148/0.145    delta  -0.004 ms  control-no-change
  attach_history    p50  943.567/944.721  p95  956.009/995.993  delta  +1.154 ms  control-no-change
  resume_ready      p50  557.537/556.554  p95  576.338/571.906  delta  -0.983 ms  control-no-change
  provider_dispatch p50   14.026/14.474   p95   15.076/16.522   delta  +0.448 ms  control-no-change
  session_save      p50   12.031/12.084   p95  210.729/69.809   delta  +0.054 ms  control-no-change
  total store_read_bytes p50 28640400  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23794, 23804, 24507, 24509, 25238, 25248, 25953, 25967, 26662, 26672]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23780, 23800, 24515, 24527, 25240, 25242, 25949, 25959, 26660, 26678]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23794..26672 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23780..26678 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 1.2 ms | 1.3 ms | 1.1 ms | 1.3 ms | 5 |
| write (10KB) | 1.4 ms | 2.9 ms | 1.3 ms | 2.9 ms | 5 |
| edit (simple replace) | 1.2 ms | 125.6 ms | 1.1 ms | 125.6 ms | 5 |
| ls (scripts dir) | 0.6 ms | 0.6 ms | 0.6 ms | 0.6 ms | 5 |
| agentgrep (grep in tree) | 1.4 ms | 1.4 ms | 1.4 ms | 1.4 ms | 5 |
| agentgrep (find **/*.rs) | 1.2 ms | 1.2 ms | 1.2 ms | 1.2 ms | 5 |
| bash (true) | 1.7 ms | 1.7 ms | 1.6 ms | 1.7 ms | 5 |
| bash (echo hello) | 1.7 ms | 1.7 ms | 1.6 ms | 1.7 ms | 5 |
| bash (ls fixture tree) | 2.9 ms | 3.0 ms | 2.8 ms | 3.0 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 61.1 ms | 61.1 ms | 61.1 ms | 61.3 ms | 0 |
| 10 | 198.9 ms | 226.9 ms | 226.9 ms | 227.1 ms | 0 |
| 40 | 883.7 ms | 890.7 ms | 892.2 ms | 892.6 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 277.1 ms | 277.1 ms | 277.1 ms | 1 |
| leaf_touch | src/main.rs | 421.4 ms | 421.4 ms | 421.4 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 20.25 s | 20.25 s | 20.25 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 28.72 s | 28.72 s | 28.72 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (775c491) | 8.0 ms | 97.1 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.6 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 23.0 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 59.6 ms | 346.5 MB |
| OpenCode | 1.19.5 | 315.1 ms | 344.6 MB |
| pi | 1.5.0 | 247.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 519.2 ms | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 220.7 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 70.3 ms | 63.1 ms–361.5 ms | baseline | 32.3 MB | baseline | yes | — |
| jcode | 396.0 ms | 329.7 ms–564.3 ms | 5.64× more | 34.1 MB | 1.06× more | yes | — |
| Claude Code | 422.2 ms | 387.3 ms–730.2 ms | 6.01× more | 199.1 MB | 6.17× more | yes | — |
| Codex CLI | 454.7 ms | 384.2 ms–1.19 s | 6.47× more | 136.9 MB | 4.24× more | yes | — |
| OpenCode | 2.95 s | 2.65 s–5.33 s | 41.98× more | 686.6 MB | 21.27× more | yes | — |
| pi | 413.1 ms | 348.4 ms–443.6 ms | 5.88× more | 122.8 MB | 3.80× more | yes | — |
| oh-my-pi (omp) | 1.66 s | 1.24 s–1.83 s | 23.57× more | 416.7 MB | 12.91× more | yes | — |
| oh-my-openagent (omo) | 9.23 s | 7.91 s–9.97 s | 131.32× more | 880.3 MB | 27.27× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 64.4 ms | 60.7 ms–110.7 ms | baseline | 23.5 MB | baseline | skill-uri | yes | — |
| jcode | 227.1 ms | 213.4 ms–371.1 ms | 3.52× more | 25.3 MB | 1.08× more | skill-tool | yes | — |
| Claude Code | 512.5 ms | 406.4 ms–536.5 ms | 7.95× more | 199.3 MB | 8.50× more | skill-tool | yes | — |
| Codex CLI | 478.6 ms | 383.1 ms–608.4 ms | 7.43× more | 144.4 MB | 6.16× more | file-read | yes | — |
| OpenCode | 3.57 s | 3.46 s–4.55 s | 55.45× more | 727.6 MB | 31.02× more | skill-tool | yes | — |
| pi | 452.4 ms | 441.6 ms–490.5 ms | 7.02× more | 135.1 MB | 5.76× more | file-read | yes | — |
| oh-my-pi (omp) | 2.19 s | 2.10 s–2.99 s | 34.00× more | 451.4 MB | 19.25× more | file-read | yes | — |
| oh-my-openagent (omo) | 9.22 s | 8.12 s–10.89 s | 143.09× more | 887.2 MB | 37.83× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 89.9 ms | 87.2 ms–114.0 ms | baseline | 19.5 MB | baseline | mcp-call | yes | 72.5 ms | — |
| jcode | 352.8 ms | 334.6 ms–395.6 ms | 3.93× more | 52.5 MB | 2.70× more | mcp-action | yes | 330.9 ms | — |
| Claude Code | 498.2 ms | 450.7 ms–634.8 ms | 5.54× more | 216.0 MB | 11.10× more | mcp-tool | yes | 375.3 ms | — |
| Codex CLI | 359.4 ms | 307.7 ms–399.1 ms | 4.00× more | 142.6 MB | 7.32× more | mcp-tool | yes | 223.3 ms | — |
| OpenCode | 3.28 s | 3.13 s–4.49 s | 36.50× more | 745.8 MB | 38.31× more | mcp-tool | yes | 2.87 s | — |
| pi | 516.2 ms | 499.0 ms–616.6 ms | 5.75× more | 132.7 MB | 6.81× more | mcp-tool | yes | 440.0 ms | — |
| oh-my-pi (omp) | 2.12 s | 1.77 s–2.68 s | 23.55× more | 408.2 MB | 20.97× more | mcp-route | yes | 1.88 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.9 MB | 169.7 KB | baseline | 0.67 | 1.00 | 35.1 MB | 380.3 KB |
| jcode | 36.5 MB | 209.7 KB | 1.24× more | — | — | 40.1 MB | 357.8 KB |
| Claude Code | 159.1 MB | 152.8 MB | 921.81× more | — | — | 190.5 MB | 184.1 MB |
| Codex CLI | 114.6 MB | 81.8 MB | 493.32× more | — | — | 107.2 MB | 107.3 MB |
| OpenCode | 260.8 MB | 637.6 KB | 3.76× more | — | — | 559.3 MB | 6.9 MB |
| pi | 119.6 MB | 116.1 MB | 700.57× more | — | — | 148.7 MB | 148.0 MB |
| oh-my-pi (omp) | 414.0 MB | 117.4 MB | 708.62× more | — | — | 440.9 MB | 298.5 MB |
| oh-my-openagent (omo) | 57.8 MB | 46.8 MB | 282.45× more | — | — | 814.8 MB | 316.2 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.1 MB | 36.8 MB | 38.5 MB |
| jcode | 40.1 MB | 41.4 MB | 43.2 MB |
| Claude Code | 190.5 MB | 932.2 MB | 1.8 GB |
| Codex CLI | 107.2 MB | 536.0 MB | 1.0 GB |
| OpenCode | 559.3 MB | 625.2 MB | 624.0 MB |
| pi | 148.7 MB | 740.8 MB | 1.4 GB |
| oh-my-pi (omp) | 440.9 MB | 1.7 GB | 3.1 GB |
| oh-my-openagent (omo) | 814.8 MB | 2.9 GB | 3.6 GB |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    8.53 ms
  Max:    12.71 ms
  Mean:   9.82 ms
  Median: 9.75 ms
  Stdev:  1.26 ms

--version:
  Min:    8.10 ms
  Max:    16.79 ms
  Mean:   10.71 ms
  Median: 9.60 ms
  Stdev:  2.95 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    24.66 ms
  Max:    97.66 ms
  Mean:   58.17 ms
  Median: 51.03 ms
  Stdev:  27.68 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    24.75 ms
  Max:    109.36 ms
  Mean:   74.17 ms
  Median: 74.56 ms
  Stdev:  26.67 ms

Bridge stage: hello_ok:
  Min:    25.25 ms
  Max:    110.05 ms
  Mean:   74.97 ms
  Median: 75.60 ms
  Stdev:  26.81 ms

Bridge stage: no-op reply:
  Min:    25.65 ms
  Max:    110.49 ms
  Mean:   75.45 ms
  Median: 76.26 ms
  Stdev:  26.84 ms

Bridge total (socket -> no-op):
  Min:    25.65 ms
  Max:    110.49 ms
  Mean:   75.45 ms
  Median: 76.26 ms
  Stdev:  26.84 ms

============================================================
Summary:
  Binary load median:      ~9.8 ms
  Server ready median:     ~51.0 ms
  Bridge total median:     ~76.3 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (3ba716d3e11a)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-k70otyvc
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.027/0.027    p95    0.030/0.072    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.246/0.262    p95    0.267/0.755    delta  +0.016 ms  control-no-change
  mcp_config_load   p50    0.313/0.299    p95    0.612/0.739    delta  -0.014 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.004/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   21.571/21.000   p95   26.985/25.692   delta  -0.572 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22918..22950 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22954 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.673/4.744    p95    6.893/5.516    delta  +0.071 ms  control-no-change
  skill_request_overlay p50    5.949/6.184    p95    6.861/7.066    delta  +0.235 ms  control-drift
  mcp_config_load   p50    0.317/0.313    p95    0.343/0.337    delta  -0.005 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   17.352/17.332   p95   51.129/30.547   delta  -0.019 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22954 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22954 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.027    p95    0.031/0.031    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.266/0.264    p95    0.294/0.313    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.309/0.310    p95    0.333/0.378    delta  +0.001 ms  control-no-change
  attach_history    p50  311.452/312.194  p95  334.663/335.352  delta  +0.742 ms  control-no-change
  resume_ready      p50   68.241/68.360   p95   73.772/74.059   delta  +0.118 ms  control-no-change
  provider_dispatch p50    1.128/1.175    p95    1.221/1.241    delta  +0.047 ms  control-no-change
  session_save      p50   15.624/14.789   p95   18.279/18.790   delta  -0.835 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (4 distinct values)
  SPREAD   a session_save store_read_bytes 22948..22972 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22972 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.584/4.569    p95    4.737/4.763    delta  -0.015 ms  control-no-change
  skill_request_overlay p50    5.900/5.845    p95    6.641/6.338    delta  -0.055 ms  control-no-change
  mcp_config_load   p50    0.302/0.302    p95    0.320/0.336    delta  -0.001 ms  control-no-change
  attach_history    p50  302.555/304.194  p95  341.049/314.153  delta  +1.639 ms  control-no-change
  resume_ready      p50   68.086/67.608   p95   73.179/70.053   delta  -0.478 ms  control-no-change
  provider_dispatch p50    1.130/1.173    p95    1.195/1.325    delta  +0.044 ms  control-no-change
  session_save      p50   17.076/15.269   p95   25.093/17.343   delta  -1.808 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099143..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22972 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22968 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.031    p95    0.035/0.057    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.262/0.270    p95    0.331/0.519    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.293/0.308    p95    0.316/0.551    delta  +0.014 ms  control-no-change
  attach_history    p50 3565.067/3739.653 p95 4221.376/4177.904 delta +174.586 ms  control-no-change
  resume_ready      p50  741.205/749.260  p95  901.132/885.502  delta  +8.056 ms  control-no-change
  provider_dispatch p50   11.733/11.543   p95   14.050/13.951   delta  -0.190 ms  control-no-change
  session_save      p50   20.573/22.530   p95   34.353/25.461   delta  +1.957 ms  control-no-change
  total store_read_bytes p50 53417515  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184295..42184305 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210246..11210255 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22953..22971 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.107/3.901    p95    4.965/4.533    delta  -0.206 ms  control-no-change
  skill_request_overlay p50    5.417/4.796    p95    6.902/5.613    delta  -0.621 ms  control-no-change
  mcp_config_load   p50    0.265/0.257    p95    0.352/0.290    delta  -0.008 ms  control-no-change
  attach_history    p50 3193.887/3426.218 p95 3494.482/4200.907 delta +232.330 ms  control-no-change
  resume_ready      p50  649.569/639.183  p95  826.203/958.248  delta -10.386 ms  control-no-change
  provider_dispatch p50   10.761/9.656    p95   12.178/12.824   delta  -1.105 ms  control-no-change
  session_save      p50   23.277/22.558   p95   29.674/32.267   delta  -0.719 ms  control-no-change
  total store_read_bytes p50 53417523  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184299..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210250..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22971 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184297..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210247..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22946..22975 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (3ba716d3e11a)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-201p20ll
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.032    p95    0.164/0.043    delta  +0.004 ms  control-no-change
  skill_request_overlay p50    0.256/0.239    p95    0.810/0.346    delta  -0.017 ms  control-no-change
  mcp_config_load   p50    0.267/0.282    p95    0.650/0.406    delta  +0.014 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   18.996/18.079   p95   38.722/42.651   delta  -0.917 ms  control-no-change
  total store_read_bytes p50 23658  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22937, 22943, 22955, 23649, 23655, 23661, 24351, 24371, 24377, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22947, 22951, 23657, 23665, 23677, 24361, 24363, 24371, 24379]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22937..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24379 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.561/6.888    p95    7.757/10.330   delta  +2.327 ms  control-drift
  skill_request_overlay p50    5.808/6.458    p95   11.321/11.023   delta  +0.650 ms  control-no-change
  mcp_config_load   p50    0.299/0.310    p95    1.044/0.689    delta  +0.011 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   19.396/21.517   p95   28.661/28.560   delta  +2.121 ms  control-no-change
  total store_read_bytes p50 23655  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22929, 22933, 22943, 22947, 23651, 23659, 23661, 24369, 24373, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22927, 22947, 22959, 23661, 23665, 24367, 24371, 24375, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22929..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22927..24383 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.034/0.032    p95    0.084/0.083    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.250/0.253    p95    0.793/0.739    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    0.283/0.284    p95    0.848/0.757    delta  +0.001 ms  control-no-change
  attach_history    p50  139.151/124.672  p95  217.299/194.877  delta -14.479 ms  control-no-change
  resume_ready      p50   91.701/84.881   p95  131.543/169.222  delta  -6.820 ms  control-no-change
  provider_dispatch p50    1.253/1.345    p95    2.958/2.806    delta  +0.092 ms  control-no-change
  session_save      p50   17.037/27.797   p95   68.149/132.215  delta +10.760 ms  control-no-change
  total store_read_bytes p50 2841256  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23674, 24380, 24388, 25094, 25102, 25800, 25816, 26518, 26526]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 24388, 24396, 25090, 25102, 25798, 25820, 26526, 26536]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26526 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26536 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.266/4.231    p95    5.902/5.165    delta  -0.035 ms  control-no-change
  skill_request_overlay p50    5.746/5.716    p95    6.860/6.583    delta  -0.030 ms  control-no-change
  mcp_config_load   p50    0.280/0.280    p95    0.348/0.545    delta  -0.000 ms  control-no-change
  attach_history    p50  110.636/114.690  p95  143.933/133.494  delta  +4.054 ms  control-no-change
  resume_ready      p50   68.075/68.498   p95   89.890/84.146   delta  +0.423 ms  control-no-change
  provider_dispatch p50    1.079/1.096    p95    1.171/1.392    delta  +0.017 ms  control-no-change
  session_save      p50   15.511/17.489   p95   23.437/31.529   delta  +1.978 ms  control-no-change
  total store_read_bytes p50 2841247  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23660, 23670, 24372, 24386, 25096, 25098, 25818, 25828, 26524, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23662, 23670, 24384, 24392, 25082, 25102, 25804, 25816, 26522, 26534]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23660..26530 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23662..26534 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.031/0.033    p95    0.128/0.143    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.249/0.274    p95    0.817/1.055    delta  +0.026 ms  control-no-change
  mcp_config_load   p50    0.285/0.271    p95    0.850/1.015    delta  -0.014 ms  control-no-change
  attach_history    p50 1454.489/1462.522 p95 1875.237/1692.245 delta  +8.033 ms  control-no-change
  resume_ready      p50  871.235/792.116  p95 1200.534/1032.153 delta -79.119 ms  control-drift
  provider_dispatch p50   13.552/13.885   p95   18.400/19.808   delta  +0.332 ms  control-no-change
  session_save      p50   20.730/27.672   p95   29.221/86.070   delta  +6.943 ms  control-no-change
  total store_read_bytes p50 28640265  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23677, 24381, 24391, 25093, 25103, 25815, 25821, 26525, 26531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23667, 23671, 24373, 24381, 25091, 25107, 25813, 25817, 26501, 26525]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23667..26525 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.488/4.554    p95    5.272/18.648   delta  +0.066 ms  control-no-change
  skill_request_overlay p50    5.957/5.490    p95    6.824/10.511   delta  -0.466 ms  control-no-change
  mcp_config_load   p50    0.290/0.272    p95    0.492/0.319    delta  -0.018 ms  control-no-change
  attach_history    p50 1140.377/1219.686 p95 1423.549/1362.330 delta +79.310 ms  control-no-change
  resume_ready      p50  783.237/735.207  p95  977.782/846.317  delta -48.031 ms  control-no-change
  provider_dispatch p50   11.067/11.674   p95   15.119/16.167   delta  +0.607 ms  control-no-change
  session_save      p50   23.225/21.192   p95   54.051/33.484   delta  -2.033 ms  control-no-change
  total store_read_bytes p50 28640267  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23677, 24383, 24387, 25099, 25101, 25801, 25819, 26523, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23673, 23677, 24387, 24391, 25091, 25105, 25809, 25823, 26525, 26527]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23673..26527 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.8 ms | 0.3 ms | 0.8 ms | 5 |
| read (1000 lines) | 0.8 ms | 1.4 ms | 0.7 ms | 1.4 ms | 5 |
| read (10 lines @ offset) | 0.9 ms | 1.4 ms | 0.6 ms | 1.4 ms | 5 |
| read (repo Cargo.toml) | 0.6 ms | 0.9 ms | 0.3 ms | 0.9 ms | 5 |
| write (small) | 3.4 ms | 7.2 ms | 2.6 ms | 7.2 ms | 5 |
| write (10KB) | 2.9 ms | 3.4 ms | 2.6 ms | 3.4 ms | 5 |
| edit (simple replace) | 2.6 ms | 5.3 ms | 2.4 ms | 5.3 ms | 5 |
| ls (scripts dir) | 1.1 ms | 1.8 ms | 0.9 ms | 1.8 ms | 5 |
| agentgrep (grep in tree) | 6.7 ms | 8.0 ms | 3.9 ms | 8.0 ms | 5 |
| agentgrep (find **/*.rs) | 1.8 ms | 2.3 ms | 1.6 ms | 2.3 ms | 5 |
| bash (true) | 6.4 ms | 9.0 ms | 5.5 ms | 9.0 ms | 5 |
| bash (echo hello) | 7.1 ms | 9.8 ms | 5.1 ms | 9.8 ms | 5 |
| bash (ls fixture tree) | 11.5 ms | 19.9 ms | 8.9 ms | 19.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 104.2 ms | 104.2 ms | 104.2 ms | 111.8 ms | 0 |
| 10 | 299.4 ms | 301.3 ms | 301.3 ms | 304.4 ms | 0 |
| 40 | 1.08 s | 1.19 s | 1.19 s | 1.19 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 372.3 ms | 372.3 ms | 372.3 ms | 1 |
| leaf_touch | src/main.rs | 524.5 ms | 524.5 ms | 524.5 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 20.72 s | 20.72 s | 20.72 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 24.70 s | 24.70 s | 24.70 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (775c491) | 14.9 ms | 96.8 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.6 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 44.6 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 109.6 ms | 479.4 MB |
| OpenCode | 1.19.5 | 589.9 ms | 605.3 MB |
| pi | 1.5.0 | 318.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 796.4 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 247.3 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 217.3 ms | 207.9 ms–289.6 ms | baseline | 29.0 MB | baseline | yes | — |
| jcode | 166.9 ms | 153.5 ms–229.3 ms | 1.30× less | 30.9 MB | 1.07× more | yes | — |
| Claude Code | 1.63 s | 1.61 s–2.38 s | 7.52× more | 276.2 MB | 9.54× more | yes | — |
| Codex CLI | 1.92 s | 1.57 s–2.43 s | 8.84× more | 256.2 MB | 8.84× more | yes | — |
| OpenCode | 5.75 s | 5.54 s–8.12 s | 26.45× more | 886.9 MB | 30.62× more | yes | — |
| pi | 560.0 ms | 553.6 ms–592.5 ms | 2.58× more | 120.3 MB | 4.15× more | yes | — |
| oh-my-pi (omp) | 3.16 s | 2.80 s–3.38 s | 14.52× more | 418.1 MB | 14.43× more | yes | — |
| oh-my-openagent (omo) | 20.98 s | 19.02 s–25.91 s | 96.53× more | 548.0 MB | 18.92× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 222.9 ms | 218.5 ms–235.9 ms | baseline | 25.2 MB | baseline | skill-uri | yes | — |
| jcode | 222.6 ms | 218.7 ms–240.6 ms | same | 24.0 MB | 1.05× less | skill-tool | yes | — |
| Claude Code | 1.03 s | 1.02 s–1.04 s | 4.63× more | 236.9 MB | 9.40× more | skill-tool | yes | — |
| Codex CLI | 2.40 s | 2.22 s–2.81 s | 10.78× more | 253.6 MB | 10.06× more | file-read | yes | — |
| OpenCode | 7.60 s | 7.38 s–8.00 s | 34.11× more | 756.4 MB | 30.02× more | skill-tool | yes | — |
| pi | 717.0 ms | 712.4 ms–742.1 ms | 3.22× more | 97.8 MB | 3.88× more | file-read | yes | — |
| oh-my-pi (omp) | 3.98 s | 3.79 s–4.53 s | 17.86× more | 408.2 MB | 16.20× more | file-read | yes | — |
| oh-my-openagent (omo) | 22.51 s | 19.66 s–23.18 s | 100.98× more | 542.0 MB | 21.51× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 267.0 ms | 252.9 ms–270.1 ms | baseline | 37.5 MB | baseline | mcp-call | yes | 186.1 ms | — |
| jcode | 339.5 ms | 327.1 ms–346.4 ms | 1.27× more | 36.5 MB | 1.03× less | mcp-action | yes | 242.6 ms | — |
| Claude Code | 1.15 s | 1.08 s–1.38 s | 4.29× more | 277.2 MB | 7.38× more | mcp-tool | yes | 964.5 ms | — |
| Codex CLI | 872.6 ms | 826.4 ms–900.4 ms | 3.27× more | 180.0 MB | 4.79× more | mcp-tool | yes | 671.6 ms | — |
| OpenCode | 7.38 s | 7.28 s–8.08 s | 27.63× more | 761.2 MB | 20.28× more | mcp-tool | yes | 5.81 s | — |
| pi | 802.8 ms | 788.2 ms–814.5 ms | 3.01× more | 117.8 MB | 3.14× more | mcp-tool | yes | 658.0 ms | — |
| oh-my-pi (omp) | 7.41 s | 3.99 s–14.59 s | 27.74× more | 422.6 MB | 11.26× more | mcp-route | yes | 7.22 s | — |
| oh-my-openagent (omo) | 23.33 s | 21.34 s–27.35 s | 87.37× more | 563.1 MB | 15.00× more | mcp-tool | yes | 20.44 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.8 MB | 110.8 KB | baseline | — | — | 30.4 MB | 310.5 KB |
| jcode | 21.8 MB | 87.6 KB | 1.27× less | — | — | 26.6 MB | 350.2 KB |
| Claude Code | 154.5 MB | 154.8 MB | 1,431× more | — | — | 188.6 MB | 191.2 MB |
| Codex CLI | 123.9 MB | 99.2 MB | 916.84× more | — | — | 165.4 MB | 118.5 MB |
| OpenCode | 234.7 MB | 1.5 MB | 13.40× more | — | — | 430.0 MB | 5.8 MB |
| pi | 90.6 MB | 90.9 MB | 839.86× more | — | — | 101.1 MB | 100.8 MB |
| oh-my-pi (omp) | 306.0 MB | 86.7 MB | 800.79× more | — | — | 338.4 MB | 336.3 MB |
| oh-my-openagent (omo) | 149.2 MB | 128.6 MB | 1,188× more | — | — | 411.8 MB | 399.3 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 30.4 MB | 32.1 MB | 33.2 MB |
| jcode | 26.6 MB | 28.4 MB | 29.7 MB |
| Claude Code | 188.6 MB | 946.9 MB | 1.9 GB |
| Codex CLI | 165.4 MB | 657.1 MB | 1.2 GB |
| OpenCode | 430.0 MB | 579.3 MB | 491.2 MB |
| pi | 101.1 MB | 505.2 MB | 1008.2 MB |
| oh-my-pi (omp) | 338.4 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 411.8 MB | 2.2 GB | 3.9 GB |

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    10.99 ms
  Max:    12.07 ms
  Mean:   11.42 ms
  Median: 11.37 ms
  Stdev:  0.28 ms

--version:
  Min:    10.78 ms
  Max:    11.11 ms
  Mean:   10.92 ms
  Median: 10.89 ms
  Stdev:  0.12 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.77 ms
  Max:    25.80 ms
  Mean:   22.37 ms
  Median: 22.02 ms
  Stdev:  1.22 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~11.4 ms
  Server ready median:     ~22.0 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6a64eefa67ac)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-pr584u0_
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.112/0.108    p95    0.212/0.170    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.847/0.808    p95    1.182/1.146    delta  -0.039 ms  control-no-change
  mcp_config_load   p50    1.053/1.072    p95    1.436/1.656    delta  +0.019 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.004/0.003    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.003/0.011    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.004/0.003    delta  +0.000 ms  control-no-change
  session_save      p50   49.255/50.980   p95   72.922/62.343   delta  +1.725 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22964 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22922..22972 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   15.348/14.007   p95   16.393/15.943   delta  -1.341 ms  control-no-change
  skill_request_overlay p50   41.936/40.499   p95   50.467/43.847   delta  -1.437 ms  control-no-change
  mcp_config_load   p50    1.098/1.065    p95    1.211/1.148    delta  -0.033 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.010    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.004    delta  -0.000 ms  control-drift
  session_save      p50   66.022/68.884   p95   79.306/146.574  delta  +2.862 ms  control-no-change
  total store_read_bytes p50 22937  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22918..22956 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22902..22964 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.103/0.091    p95    0.164/0.137    delta  -0.012 ms  control-drift
  skill_request_overlay p50    0.843/0.783    p95    1.198/0.818    delta  -0.060 ms  control-no-change
  mcp_config_load   p50    1.001/0.920    p95    1.132/1.027    delta  -0.081 ms  control-no-change
  attach_history    p50  494.920/474.775  p95  553.687/493.852  delta -20.144 ms  control-no-change
  resume_ready      p50  154.821/157.747  p95  185.127/162.424  delta  +2.925 ms  control-no-change
  provider_dispatch p50    1.718/1.707    p95    8.036/1.879    delta  -0.011 ms  control-no-change
  session_save      p50   39.523/40.505   p95   83.336/55.062   delta  +0.982 ms  control-no-change
  total store_read_bytes p50 5311217  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22915..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189148..4189155 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099099..1099106 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22931..22976 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.793/15.581   p95   17.845/16.568   delta  +0.788 ms  control-no-change
  skill_request_overlay p50   38.882/39.577   p95   41.150/43.124   delta  +0.694 ms  control-no-change
  mcp_config_load   p50    1.065/1.020    p95    1.182/1.147    delta  -0.045 ms  control-no-change
  attach_history    p50  497.455/501.250  p95  549.196/522.555  delta  +3.795 ms  control-no-change
  resume_ready      p50  154.566/156.667  p95  166.797/175.819  delta  +2.101 ms  control-no-change
  provider_dispatch p50    1.657/1.677    p95    1.827/1.838    delta  +0.020 ms  control-no-change
  session_save      p50   40.008/39.332   p95   43.230/42.075   delta  -0.676 ms  control-no-change
  total store_read_bytes p50 5311221  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22920..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099104..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22900..22984 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.095/0.108    p95    0.139/0.137    delta  +0.013 ms  control-no-change
  skill_request_overlay p50    0.765/0.845    p95    1.372/0.940    delta  +0.080 ms  control-no-change
  mcp_config_load   p50    0.959/1.008    p95    1.001/1.140    delta  +0.049 ms  control-no-change
  attach_history    p50 5586.857/5608.310 p95 6162.441/6112.798 delta +21.453 ms  control-no-change
  resume_ready      p50 1622.532/1627.550 p95 1882.785/1838.396 delta  +5.018 ms  control-no-change
  provider_dispatch p50   18.248/17.975   p95   21.326/21.508   delta  -0.273 ms  control-no-change
  session_save      p50   40.466/38.957   p95  148.818/174.288  delta  -1.509 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22923..22971 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22979 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.221/14.617   p95   15.329/15.912   delta  +0.396 ms  control-no-change
  skill_request_overlay p50   38.918/40.007   p95   42.296/43.082   delta  +1.089 ms  control-no-change
  mcp_config_load   p50    1.031/1.081    p95    1.253/1.181    delta  +0.050 ms  control-no-change
  attach_history    p50 5626.078/5587.476 p95 5694.994/5613.679 delta -38.602 ms  control-no-change
  resume_ready      p50 1630.133/1622.310 p95 1644.343/1646.351 delta  -7.822 ms  control-no-change
  provider_dispatch p50   17.679/17.988   p95   19.334/19.549   delta  +0.309 ms  control-no-change
  session_save      p50   39.553/39.508   p95   41.461/44.642   delta  -0.045 ms  control-no-change
  total store_read_bytes p50 53417432  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22941..22983 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184247..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210210..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22979 (8 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6a64eefa67ac)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-svgifs0a
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.158/0.155    p95    0.201/0.207    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    1.283/1.257    p95    1.436/1.631    delta  -0.027 ms  control-no-change
  mcp_config_load   p50    1.435/1.466    p95    1.533/1.826    delta  +0.032 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   47.287/46.866   p95   57.871/48.835   delta  -0.421 ms  control-no-change
  total store_read_bytes p50 23624  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22949, 22950, 22953, 23619, 23630, 23652, 23664, 24331, 24338]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22940, 22943, 22953, 22955, 23622, 23636, 23643, 24300, 24310, 24325]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22949..24338 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22940..24325 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   24.980/24.868   p95   27.924/30.236   delta  -0.112 ms  control-no-change
  skill_request_overlay p50   57.601/56.887   p95   67.368/60.386   delta  -0.715 ms  control-no-change
  mcp_config_load   p50    1.473/1.513    p95    2.067/1.871    delta  +0.040 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.009/0.007    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   51.688/53.722   p95   71.757/104.976  delta  +2.034 ms  control-no-change
  total store_read_bytes p50 23633  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22935, 22939, 22945, 22953, 23632, 23634, 23652, 24329, 24371]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22925, 22940, 22943, 22945, 23622, 23624, 23631, 24278, 24343, 24346]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22935..24371 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22925..24346 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.158/0.157    p95    0.197/0.190    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    1.245/1.224    p95    1.368/1.323    delta  -0.021 ms  control-no-change
  mcp_config_load   p50    1.474/1.475    p95    1.685/1.778    delta  +0.001 ms  control-no-change
  attach_history    p50  301.705/305.690  p95  313.647/310.950  delta  +3.985 ms  control-no-change
  resume_ready      p50  242.049/241.778  p95  247.893/244.460  delta  -0.271 ms  control-no-change
  provider_dispatch p50    1.895/1.938    p95    2.469/2.194    delta  +0.043 ms  control-no-change
  session_save      p50   45.953/44.224   p95   52.155/53.867   delta  -1.730 ms  control-no-change
  total store_read_bytes p50 2841122  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23639, 24330, 24362, 25027, 25057, 25758, 25768, 26423, 26457]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23655, 23659, 24352, 24362, 25065, 25067, 25744, 25748, 26467, 26471]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23639..26457 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23655..26471 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   24.676/24.609   p95   27.269/28.193   delta  -0.067 ms  control-no-change
  skill_request_overlay p50   54.236/53.597   p95   63.178/65.213   delta  -0.639 ms  control-no-change
  mcp_config_load   p50    1.432/1.515    p95    1.568/1.757    delta  +0.083 ms  control-drift
  attach_history    p50  300.612/297.883  p95  320.915/307.083  delta  -2.729 ms  control-no-change
  resume_ready      p50  239.691/234.497  p95  254.647/246.621  delta  -5.194 ms  control-no-change
  provider_dispatch p50    1.853/1.870    p95    2.032/2.161    delta  +0.017 ms  control-no-change
  session_save      p50   48.487/44.737   p95   53.851/58.487   delta  -3.750 ms  control-no-change
  total store_read_bytes p50 2841132  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23651, 23659, 24336, 24350, 25051, 25053, 25727, 25760, 26423, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23631, 23663, 24350, 24374, 25013, 25029, 25738, 25760, 26411, 26459]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23651..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23631..26459 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.160/0.156    p95    0.198/0.209    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    1.263/1.266    p95    1.590/1.572    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    1.476/1.416    p95    2.098/1.685    delta  -0.060 ms  control-no-change
  attach_history    p50 3159.223/3171.970 p95 3405.498/3396.427 delta +12.746 ms  control-no-change
  resume_ready      p50 2493.825/2489.944 p95 3275.382/2573.485 delta  -3.881 ms  control-no-change
  provider_dispatch p50   21.227/20.438   p95   24.582/24.148   delta  -0.789 ms  control-no-change
  session_save      p50   45.788/51.264   p95  151.232/139.333  delta  +5.476 ms  control-no-change
  total store_read_bytes p50 28640152  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23655, 23682, 24333, 24342, 25061, 25064, 25741, 25763, 26433, 26434]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 23675, 24341, 24346, 25051, 25067, 25710, 25746, 26424, 26461]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23655..26434 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26461 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   24.142/24.583   p95   25.682/27.174   delta  +0.441 ms  control-no-change
  skill_request_overlay p50   55.961/53.983   p95   60.782/58.007   delta  -1.978 ms  control-no-change
  mcp_config_load   p50    1.514/1.480    p95    1.918/1.744    delta  -0.034 ms  control-no-change
  attach_history    p50 3101.744/3142.632 p95 3212.901/3171.944 delta +40.888 ms  control-no-change
  resume_ready      p50 2450.901/2491.203 p95 2514.016/2606.656 delta +40.302 ms  control-no-change
  provider_dispatch p50   21.223/20.789   p95   28.120/21.986   delta  -0.434 ms  control-no-change
  session_save      p50   46.556/46.187   p95   73.127/53.966   delta  -0.369 ms  control-no-change
  total store_read_bytes p50 28640118  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23602, 23650, 24339, 24351, 25025, 25034, 25707, 25765, 26433, 26440]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23678, 24352, 24353, 25037, 25053, 25724, 25766, 26445, 26449]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23602..26440 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26449 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.8 ms | 3.6 ms | 2.3 ms | 3.6 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.9 ms | 2.2 ms | 2.9 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 8.0 ms | 10.7 ms | 7.4 ms | 10.7 ms | 5 |
| write (10KB) | 8.9 ms | 16.7 ms | 7.3 ms | 16.7 ms | 5 |
| edit (simple replace) | 7.9 ms | 10.4 ms | 7.4 ms | 10.4 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| agentgrep (grep in tree) | 7.5 ms | 7.5 ms | 7.4 ms | 7.5 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 5.3 ms | 4.8 ms | 5.3 ms | 5 |
| bash (true) | 13.1 ms | 15.6 ms | 10.0 ms | 15.6 ms | 5 |
| bash (echo hello) | 10.9 ms | 12.9 ms | 10.5 ms | 12.9 ms | 5 |
| bash (ls fixture tree) | 16.6 ms | 20.0 ms | 15.7 ms | 20.0 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.8 ms | 2.6 ms | 2.8 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 194.9 ms | 194.9 ms | 194.9 ms | 195.1 ms | 0 |
| 10 | 676.4 ms | 683.6 ms | 683.6 ms | 683.8 ms | 0 |
| 40 | 1.78 s | 2.16 s | 2.20 s | 2.20 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 574.1 ms | 574.1 ms | 574.1 ms | 1 |
| leaf_touch | src\main.rs | 779.6 ms | 779.6 ms | 779.6 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 23.84 s | 23.84 s | 23.84 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 33.95 s | 33.95 s | 33.95 s | 1 |

## TUI (`horon-tui`)

The interactive client is a second binary and is measured on its own. These numbers come from the `tui` suite and are never compared with the headless tables above: `first_frame_ms` and `direct_first_frame_ms` are two launches of two different programs, and the handoff is the difference between them.

![TUI (`horon-tui`)](charts/tui-01-latency.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 42.2 ms | 40.0 ms–53.7 ms | 5 |
| First frame (direct `horon-tui`) | 40.3 ms | 39.5 ms–47.4 ms | 5 |
| TTY handoff cost | 1.0 ms | -4.6 ms–12.7 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–1.3 ms | 50 |
| Idle RSS after 5 s | 5.5 MB | 5.3 MB–5.6 MB | 5 |
| RSS after one turn | 5.6 MB | 5.5 MB–5.7 MB | 5 |
| Frame draw time (p50) | 1.5 ms | 1.5 ms–1.5 ms | 5 |
| Frame draw time (p95) | 1.6 ms | 1.5 ms–2.4 ms | 5 |
| Install size (`horon-tui`) | 5.9 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 129.8 ms | 63.2 ms–413.4 ms | 5 |
| First frame (direct `horon-tui`) | 84.0 ms | 71.2 ms–108.4 ms | 5 |
| TTY handoff cost | 45.8 ms | -39.3 ms–340.1 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.5 ms | 50 |
| Idle RSS after 5 s | 8.9 MB | 8.8 MB–8.9 MB | 5 |
| RSS after one turn | 9.1 MB | 9.0 MB–9.1 MB | 5 |
| Frame draw time (p50) | 1.3 ms | 1.1 ms–1.8 ms | 5 |
| Frame draw time (p95) | 3.2 ms | 2.3 ms–3.5 ms | 5 |
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
| Frame draw time (p50) | 2.5 ms | 2.4 ms–2.6 ms | 5 |
| Frame draw time (p95) | 2.6 ms | 2.5 ms–2.7 ms | 5 |
| Install size (`horon-tui`) | 4.9 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
