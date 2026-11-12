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

<!-- bench-auto:start commit=805b408e00f6d228089979559a00ca6f502803a3 -->

## Headless (`horon-agent`)

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `805b408e00f6d228089979559a00ca6f502803a3`
- Measured at: 2026-11-12 18:57 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/43273766126> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (805b408)` | Success |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (805b408)` | Success |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (805b408)` | Success |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (805b408) | 3.2 ms | 114.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.2 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 9.0 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 38.9 ms | 435.0 MB |
| OpenCode | 1.19.5 | 363.1 ms | 613.4 MB |
| pi | 1.5.0 | 192.7 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 561.8 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 96.9 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 55.9 ms | 46.2 ms–372.4 ms | baseline | 47.5 MB | baseline | yes | 10 | — |
| jcode | 140.8 ms | 139.9 ms–151.0 ms | 2.52× more | 42.2 MB | 1.13× less | yes | 5 | — |
| Claude Code | 371.1 ms | 354.4 ms–484.3 ms | 6.64× more | 204.2 MB | 4.30× more | yes | — | — |
| Codex CLI | 480.5 ms | 439.8 ms–538.9 ms | 8.59× more | 192.4 MB | 4.05× more | yes | 6 | — |
| OpenCode | 2.13 s | 2.11 s–2.32 s | 38.05× more | 577.1 MB | 12.15× more | yes | 4 | — |
| pi | 320.8 ms | 315.9 ms–336.9 ms | 5.74× more | 114.3 MB | 2.41× more | yes | — | — |
| oh-my-pi (omp) | 2.97 s | 2.25 s–3.77 s | 53.14× more | 428.0 MB | 9.01× more | yes | 71 | — |
| oh-my-openagent (omo) | 4.97 s | 4.76 s–5.33 s | 88.93× more | 664.7 MB | 13.99× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 53.3 ms | 46.5 ms–78.0 ms | baseline | 37.9 MB | baseline | skill-uri | yes | — |
| jcode | 121.5 ms | 49.8 ms–265.5 ms | 2.28× more | 41.1 MB | 1.08× more | skill-tool | yes | — |
| Claude Code | 374.1 ms | 360.1 ms–387.5 ms | 7.02× more | 206.0 MB | 5.44× more | skill-tool | yes | — |
| Codex CLI | 583.2 ms | 529.7 ms–701.9 ms | 10.94× more | 192.1 MB | 5.07× more | file-read | yes | — |
| OpenCode | 2.58 s | 2.56 s–2.83 s | 48.32× more | 618.6 MB | 16.32× more | skill-tool | yes | — |
| pi | 416.3 ms | 413.6 ms–435.0 ms | 7.81× more | 125.9 MB | 3.32× more | file-read | yes | — |
| oh-my-pi (omp) | 1.42 s | 1.37 s–1.78 s | 26.58× more | 373.2 MB | 9.85× more | file-read | yes | — |
| oh-my-openagent (omo) | 5.39 s | 5.17 s–5.61 s | 101.02× more | 666.3 MB | 17.58× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 66.4 ms | 61.8 ms–86.8 ms | baseline | 50.6 MB | baseline | mcp-call | yes | 51.0 ms | — |
| jcode | 313.5 ms | 166.8 ms–530.1 ms | 4.72× more | 56.4 MB | 1.11× more | mcp-action | yes | 283.6 ms | — |
| Claude Code | 469.2 ms | 461.6 ms–489.6 ms | 7.07× more | 226.5 MB | 4.47× more | mcp-tool | yes | 340.1 ms | — |
| Codex CLI | 540.4 ms | 472.8 ms–698.6 ms | 8.14× more | 195.3 MB | 3.86× more | mcp-tool | yes | 384.7 ms | — |
| OpenCode | 2.45 s | 2.37 s–2.54 s | 36.94× more | 578.6 MB | 11.43× more | mcp-tool | yes | 2.22 s | — |
| pi | 460.3 ms | 439.7 ms–492.9 ms | 6.94× more | 139.3 MB | 2.75× more | mcp-tool | yes | 400.7 ms | — |
| oh-my-pi (omp) | 2.03 s | 1.64 s–3.11 s | 30.60× more | 415.0 MB | 8.19× more | mcp-route | yes | 1.37 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 37.0 MB | 137.8 KB | baseline | 0.45 | 1.00 | 44.7 MB | 375.8 KB |
| jcode | 42.7 MB | 129.8 KB | 1.06× less | 0.22 | — | 46.3 MB | 243.8 KB |
| Claude Code | 152.2 MB | 150.2 MB | 1,116× more | — | — | 187.2 MB | 180.9 MB |
| Codex CLI | 128.8 MB | 132.9 MB | 987.65× more | — | — | 162.4 MB | 164.6 MB |
| OpenCode | 242.4 MB | 0.0 B | — | — | — | 449.7 MB | 11.0 MB |
| pi | 107.2 MB | 108.8 MB | 808.13× more | — | — | 129.9 MB | 129.3 MB |
| oh-my-pi (omp) | 313.1 MB | 119.3 MB | 886.83× more | — | — | 365.4 MB | 356.5 MB |
| oh-my-openagent (omo) | 279.0 MB | 34.6 MB | 256.94× more | — | — | 584.4 MB | 589.0 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.7 MB | 45.7 MB | 47.9 MB |
| jcode | 46.3 MB | 47.9 MB | 48.5 MB |
| Claude Code | 187.2 MB | 925.0 MB | 1.8 GB |
| Codex CLI | 162.4 MB | 812.7 MB | 1.6 GB |
| OpenCode | 449.7 MB | 662.7 MB | 561.0 MB |
| pi | 129.9 MB | 622.5 MB | 1.3 GB |
| oh-my-pi (omp) | 365.4 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 584.4 MB | 2.9 GB | 5.7 GB |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    4.09 ms
  Max:    4.83 ms
  Mean:   4.40 ms
  Median: 4.40 ms
  Stdev:  0.22 ms

--version:
  Min:    3.92 ms
  Max:    4.37 ms
  Mean:   4.17 ms
  Median: 4.14 ms
  Stdev:  0.15 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.51 ms
  Max:    20.64 ms
  Mean:   20.59 ms
  Median: 20.58 ms
  Stdev:  0.04 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.66 ms
  Max:    20.92 ms
  Mean:   20.73 ms
  Median: 20.71 ms
  Stdev:  0.08 ms

Bridge stage: hello_ok:
  Min:    21.35 ms
  Max:    21.70 ms
  Mean:   21.48 ms
  Median: 21.47 ms
  Stdev:  0.13 ms

Bridge stage: no-op reply:
  Min:    21.76 ms
  Max:    22.32 ms
  Mean:   21.98 ms
  Median: 21.93 ms
  Stdev:  0.19 ms

Bridge total (socket -> no-op):
  Min:    21.76 ms
  Max:    22.32 ms
  Mean:   21.98 ms
  Median: 21.93 ms
  Stdev:  0.19 ms

============================================================
Summary:
  Binary load median:      ~4.4 ms
  Server ready median:     ~20.6 ms
  Bridge total median:     ~21.9 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (e05ee8546af7)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-bdn00wed
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.013/0.013    p95    0.013/0.014    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.097/0.096    p95    0.098/0.117    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.128/0.129    p95    0.140/0.140    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.168/6.995    p95  177.538/108.619  delta  -0.173 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23052..23084 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23092 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.280/2.276    p95    2.317/2.310    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    3.902/3.896    p95    4.007/3.936    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.132/0.132    p95    0.133/0.139    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50  109.228/91.242   p95  180.086/182.863  delta -17.986 ms  control-no-change
  total store_read_bytes p50 23076  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23088 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23088 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.012/0.012    p95    0.013/0.014    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.094/0.092    p95    0.122/0.117    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.125/0.127    p95    0.129/0.144    delta  +0.002 ms  control-no-change
  attach_history    p50  244.533/246.994  p95  256.637/276.005  delta  +2.462 ms  control-no-change
  resume_ready      p50   43.942/44.245   p95   44.863/45.254   delta  +0.303 ms  control-no-change
  provider_dispatch p50    1.084/1.087    p95    1.131/1.107    delta  +0.003 ms  control-no-change
  session_save      p50    6.499/11.194   p95  151.074/199.253  delta  +4.696 ms  control-drift
  total store_read_bytes p50 5311407  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23064..23089 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189193..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099135..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23086 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.264/2.279    p95    2.373/2.395    delta  +0.015 ms  control-no-change
  skill_request_overlay p50    3.907/3.893    p95    3.941/3.970    delta  -0.014 ms  control-no-change
  mcp_config_load   p50    0.134/0.132    p95    0.140/0.138    delta  -0.001 ms  control-no-change
  attach_history    p50  254.614/253.201  p95  264.492/368.866  delta  -1.413 ms  control-no-change
  resume_ready      p50   44.072/44.350   p95   44.605/44.959   delta  +0.278 ms  control-no-change
  provider_dispatch p50    1.092/1.081    p95    1.107/1.109    delta  -0.011 ms  control-no-change
  session_save      p50    6.277/6.453    p95   17.437/22.067   delta  +0.177 ms  control-no-change
  total store_read_bytes p50 5311411  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23058..23094 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189195 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099128..1099136 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23098 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.013/0.013    p95    0.015/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.096/0.099    p95    0.114/0.120    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    0.125/0.129    p95    0.130/0.137    delta  +0.003 ms  control-no-change
  attach_history    p50 3264.246/3257.788 p95 3310.492/3382.026 delta  -6.458 ms  control-no-change
  resume_ready      p50  505.573/500.376  p95  517.983/514.342  delta  -5.197 ms  control-no-change
  provider_dispatch p50   12.969/12.784   p95   13.541/14.071   delta  -0.185 ms  control-no-change
  session_save      p50    6.822/6.865    p95   69.439/40.233   delta  +0.043 ms  control-no-change
  total store_read_bytes p50 53417620  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184293 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210238..11210246 (4 distinct values)
  SPREAD   a session_save store_read_bytes 23072..23097 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184287..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210241..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23067..23093 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.320/2.407    p95    2.488/2.480    delta  +0.087 ms  control-drift
  skill_request_overlay p50    3.883/3.889    p95    4.006/3.996    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    0.134/0.134    p95    0.146/0.140    delta  -0.000 ms  control-no-change
  attach_history    p50 3292.036/3311.115 p95 3338.041/3347.671 delta +19.079 ms  control-no-change
  resume_ready      p50  517.792/515.955  p95  543.367/535.445  delta  -1.837 ms  control-no-change
  provider_dispatch p50   14.173/13.729   p95   15.050/14.895   delta  -0.444 ms  control-no-change
  session_save      p50    6.913/6.818    p95   15.558/55.014   delta  -0.095 ms  control-no-change
  total store_read_bytes p50 53417612  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184287..42184291 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210241..11210245 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23064..23096 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184291 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210244..11210245 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23088 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (e05ee8546af7)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-5pivi16u
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.007/0.007    p95    0.008/0.008    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.040/0.041    p95    0.054/0.069    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.057/0.059    p95    0.073/0.087    delta  +0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.741/7.609    p95   11.868/20.249   delta  -0.132 ms  control-no-change
  total store_read_bytes p50 23790  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23073, 23077, 23089, 23788, 23792, 24509, 24511, 24519]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23077, 23081, 23788, 23796, 23804, 24511, 24515]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23073..24519 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23077..24515 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    1.454/1.454    p95    1.556/1.539    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    1.986/1.993    p95    2.023/2.192    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.062/0.063    p95    0.076/0.074    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   15.147/13.768   p95  282.307/300.454  delta  -1.380 ms  control-no-change
  total store_read_bytes p50 23791  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23075, 23077, 23079, 23081, 23784, 23798, 23804, 24515, 24519, 24525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23067, 23081, 23089, 23794, 23796, 23800, 24511, 24521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23075..24525 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23067..24521 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.041/0.041    p95    0.052/0.058    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.067/0.060    delta  -0.001 ms  control-no-change
  attach_history    p50   63.280/62.878   p95   66.514/65.925   delta  -0.403 ms  control-no-change
  resume_ready      p50   29.781/29.864   p95   30.271/31.423   delta  +0.083 ms  control-no-change
  provider_dispatch p50    1.002/1.006    p95    1.016/1.020    delta  +0.004 ms  control-no-change
  session_save      p50    9.087/7.953    p95   19.492/12.393   delta  -1.135 ms  control-no-change
  total store_read_bytes p50 2841382  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23799, 23806, 24510, 24517, 25239, 25245, 25960, 25970, 26642, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23789, 23791, 24512, 24514, 25221, 25233, 25235, 25952, 25956, 26663]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23799..26671 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23789..26663 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.494/1.503    p95    1.575/1.576    delta  +0.008 ms  control-no-change
  skill_request_overlay p50    2.001/2.013    p95    2.036/2.034    delta  +0.012 ms  control-no-change
  mcp_config_load   p50    0.063/0.063    p95    0.064/0.066    delta  -0.000 ms  control-no-change
  attach_history    p50   64.365/64.066   p95   66.331/66.507   delta  -0.299 ms  control-no-change
  resume_ready      p50   30.101/29.810   p95   31.944/30.424   delta  -0.291 ms  control-no-change
  provider_dispatch p50    0.991/0.989    p95    1.042/1.029    delta  -0.003 ms  control-no-change
  session_save      p50   10.818/10.861   p95   12.696/20.904   delta  +0.043 ms  control-no-change
  total store_read_bytes p50 2841361  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23793, 23797, 24520, 25227, 25231, 25950, 25954, 26665, 26689]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23799, 23803, 24506, 24514, 25223, 25233, 25952, 25954, 26661]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23793..26689 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23799..26661 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.006/0.006    p95    0.008/0.009    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.041/0.041    p95    0.042/0.059    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.059/0.058    p95    0.075/0.076    delta  -0.000 ms  control-no-change
  attach_history    p50  749.229/745.610  p95  766.524/758.157  delta  -3.619 ms  control-no-change
  resume_ready      p50  374.758/374.842  p95  379.667/382.063  delta  +0.084 ms  control-no-change
  provider_dispatch p50   12.034/12.205   p95   12.550/12.612   delta  +0.171 ms  control-no-change
  session_save      p50    8.400/8.359    p95  119.898/14.126   delta  -0.040 ms  control-no-change
  total store_read_bytes p50 28640394  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 23804, 24505, 24519, 25244, 25246, 25961, 25963, 26678, 26680]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23804, 23808, 24515, 24523, 25226, 25248, 25953, 26656, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26680 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23804..26680 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.525/1.474    p95    1.728/1.659    delta  -0.052 ms  control-no-change
  skill_request_overlay p50    2.016/2.022    p95    2.089/2.137    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.063/0.064    p95    0.064/0.071    delta  +0.000 ms  control-no-change
  attach_history    p50  754.718/756.876  p95  757.635/764.178  delta  +2.158 ms  control-no-change
  resume_ready      p50  388.554/387.829  p95  394.518/391.664  delta  -0.725 ms  control-no-change
  provider_dispatch p50   12.382/12.229   p95   12.782/12.547   delta  -0.152 ms  control-no-change
  session_save      p50   13.133/11.621   p95   35.643/20.138   delta  -1.511 ms  control-no-change
  total store_read_bytes p50 28640383  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23800, 23804, 24515, 24519, 25226, 25957, 26680, 26684]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23796, 24505, 24519, 25228, 25242, 25945, 25961, 26672, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23800..26684 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23796..26680 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.5 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.6 ms | 0.4 ms | 0.6 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| write (small) | 1.3 ms | 1.4 ms | 1.2 ms | 1.4 ms | 5 |
| write (10KB) | 1.6 ms | 3.8 ms | 1.5 ms | 3.8 ms | 5 |
| edit (simple replace) | 1.3 ms | 1.5 ms | 1.2 ms | 1.5 ms | 5 |
| ls (scripts dir) | 0.8 ms | 0.8 ms | 0.7 ms | 0.8 ms | 5 |
| agentgrep (grep in tree) | 1.7 ms | 1.9 ms | 1.7 ms | 1.9 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| bash (true) | 2.1 ms | 2.2 ms | 2.1 ms | 2.2 ms | 5 |
| bash (echo hello) | 2.1 ms | 2.4 ms | 2.0 ms | 2.4 ms | 5 |
| bash (ls fixture tree) | 3.6 ms | 3.7 ms | 3.5 ms | 3.7 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 51.4 ms | 51.4 ms | 51.4 ms | 51.6 ms | 0 |
| 10 | 163.9 ms | 183.6 ms | 183.6 ms | 183.8 ms | 0 |
| 40 | 736.8 ms | 742.6 ms | 743.4 ms | 743.8 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 232.7 ms | 232.7 ms | 232.7 ms | 1 |
| leaf_touch | src/main.rs | 366.7 ms | 366.7 ms | 366.7 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 20.71 s | 20.71 s | 20.71 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 29.27 s | 29.27 s | 29.27 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (805b408) | 14.3 ms | 96.3 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.5 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 24.2 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 82.2 ms | 346.5 MB |
| OpenCode | 1.19.5 | 585.8 ms | 344.6 MB |
| pi | 1.5.0 | 419.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 718.6 ms | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 158.6 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 103.3 ms | 79.8 ms–527.4 ms | baseline | 30.7 MB | baseline | yes | — |
| jcode | 413.7 ms | 352.1 ms–588.0 ms | 4.00× more | 34.0 MB | 1.11× more | yes | — |
| Claude Code | 551.7 ms | 499.9 ms–1.02 s | 5.34× more | 199.9 MB | 6.50× more | yes | — |
| Codex CLI | 915.0 ms | 819.8 ms–2.31 s | 8.86× more | 143.6 MB | 4.67× more | yes | — |
| OpenCode | 3.60 s | 3.47 s–9.67 s | 34.81× more | 764.2 MB | 24.86× more | yes | — |
| pi | 585.9 ms | 433.5 ms–626.1 ms | 5.67× more | 125.1 MB | 4.07× more | yes | — |
| oh-my-pi (omp) | 2.30 s | 2.05 s–2.66 s | 22.28× more | 414.1 MB | 13.47× more | yes | — |
| oh-my-openagent (omo) | 10.10 s | 9.41 s–11.74 s | 97.79× more | 889.8 MB | 28.95× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 84.5 ms | 61.9 ms–108.9 ms | baseline | 30.2 MB | baseline | skill-uri | yes | — |
| jcode | 354.9 ms | 291.0 ms–366.2 ms | 4.20× more | 28.4 MB | 1.06× less | skill-tool | yes | — |
| Claude Code | 816.2 ms | 772.2 ms–1.07 s | 9.66× more | 186.0 MB | 6.15× more | skill-tool | yes | — |
| Codex CLI | 919.0 ms | 672.2 ms–1.65 s | 10.88× more | 136.2 MB | 4.51× more | file-read | yes | — |
| OpenCode | 5.14 s | 4.63 s–7.77 s | 60.90× more | 710.0 MB | 23.48× more | skill-tool | yes | — |
| pi | 601.2 ms | 568.2 ms–681.8 ms | 7.12× more | 132.0 MB | 4.36× more | file-read | yes | — |
| oh-my-pi (omp) | 2.09 s | 1.74 s–2.66 s | 24.69× more | 401.0 MB | 13.26× more | file-read | yes | — |
| oh-my-openagent (omo) | 10.53 s | 9.13 s–12.19 s | 124.61× more | 885.2 MB | 29.28× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 95.5 ms | 92.2 ms–111.9 ms | baseline | 19.1 MB | baseline | mcp-call | yes | 77.5 ms | — |
| jcode | 453.0 ms | 358.2 ms–551.5 ms | 4.75× more | 54.7 MB | 2.86× more | mcp-action | yes | 417.8 ms | — |
| Claude Code | 835.0 ms | 806.0 ms–1.25 s | 8.75× more | 227.2 MB | 11.88× more | mcp-tool | yes | 674.2 ms | — |
| Codex CLI | 1.14 s | 681.3 ms–1.47 s | 11.99× more | 143.3 MB | 7.49× more | mcp-tool | yes | 789.8 ms | — |
| OpenCode | 4.34 s | 3.91 s–5.07 s | 45.45× more | 693.5 MB | 36.26× more | mcp-tool | yes | 3.85 s | — |
| pi | 725.3 ms | 666.7 ms–819.1 ms | 7.60× more | 153.7 MB | 8.04× more | mcp-tool | yes | 628.6 ms | — |
| oh-my-pi (omp) | 2.46 s | 2.00 s–2.75 s | 25.78× more | 393.5 MB | 20.58× more | mcp-route | yes | 2.10 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.6 MB | 166.4 KB | baseline | 0.20 | 1.00 | 34.9 MB | 382.8 KB |
| jcode | 36.5 MB | 197.1 KB | 1.18× more | — | — | 40.0 MB | 349.6 KB |
| Claude Code | 156.8 MB | 145.3 MB | 894.13× more | — | — | 189.4 MB | 181.9 MB |
| Codex CLI | 101.2 MB | 79.3 MB | 488.18× more | — | — | 106.8 MB | 107.7 MB |
| OpenCode | 265.1 MB | 18.2 MB | 111.87× more | — | — | 569.6 MB | 43.7 MB |
| pi | 116.0 MB | 115.1 MB | 708.00× more | — | — | 147.9 MB | 147.6 MB |
| oh-my-pi (omp) | 341.9 MB | 95.3 MB | 586.29× more | — | — | 447.1 MB | 316.7 MB |
| oh-my-openagent (omo) | 57.8 MB | 36.2 MB | 222.59× more | — | — | 809.5 MB | 340.4 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.9 MB | 36.5 MB | 38.3 MB |
| jcode | 40.0 MB | 41.5 MB | 43.1 MB |
| Claude Code | 189.4 MB | 929.4 MB | 1.8 GB |
| Codex CLI | 106.8 MB | 535.4 MB | 1.1 GB |
| OpenCode | 569.6 MB | 603.0 MB | 952.5 MB |
| pi | 147.9 MB | 738.2 MB | 1.4 GB |
| oh-my-pi (omp) | 447.1 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 809.5 MB | 3.3 GB | 3.9 GB |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    11.03 ms
  Max:    28.79 ms
  Mean:   20.23 ms
  Median: 21.04 ms
  Stdev:  5.72 ms

--version:
  Min:    16.76 ms
  Max:    26.13 ms
  Mean:   20.58 ms
  Median: 19.30 ms
  Stdev:  3.22 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    26.31 ms
  Max:    99.05 ms
  Mean:   65.55 ms
  Median: 67.14 ms
  Stdev:  27.84 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    23.87 ms
  Max:    93.20 ms
  Mean:   57.66 ms
  Median: 57.00 ms
  Stdev:  23.54 ms

Bridge stage: hello_ok:
  Min:    24.45 ms
  Max:    94.88 ms
  Mean:   58.95 ms
  Median: 58.62 ms
  Stdev:  23.70 ms

Bridge stage: no-op reply:
  Min:    24.88 ms
  Max:    95.72 ms
  Mean:   59.61 ms
  Median: 59.34 ms
  Stdev:  23.72 ms

Bridge total (socket -> no-op):
  Min:    24.88 ms
  Max:    95.72 ms
  Mean:   59.61 ms
  Median: 59.34 ms
  Stdev:  23.72 ms

============================================================
Summary:
  Binary load median:      ~21.0 ms
  Server ready median:     ~67.1 ms
  Bridge total median:     ~59.3 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (4e8292a1600d)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-up3jn8s0
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.031/0.024    p95    0.151/0.065    delta  -0.007 ms  control-no-change
  skill_request_overlay p50    0.221/0.216    p95    0.838/0.994    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.262/0.260    p95    1.152/0.726    delta  -0.003 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   19.730/17.526   p95   41.201/34.927   delta  -2.204 ms  control-no-change
  total store_read_bytes p50 22950  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22954 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22958 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.113/4.104    p95    4.713/5.798    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    5.288/5.268    p95    6.930/10.609   delta  -0.020 ms  control-no-change
  mcp_config_load   p50    0.269/0.277    p95    0.358/0.373    delta  +0.007 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   18.093/20.185   p95   29.027/96.929   delta  +2.092 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22950 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22954 (4 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.026/0.028    p95    0.093/0.346    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.217/0.229    p95    0.466/0.419    delta  +0.013 ms  control-no-change
  mcp_config_load   p50    0.283/0.270    p95    0.525/0.490    delta  -0.013 ms  control-no-change
  attach_history    p50  359.598/355.779  p95  435.644/437.528  delta  -3.820 ms  control-no-change
  resume_ready      p50   69.177/72.366   p95   92.098/193.195  delta  +3.189 ms  control-no-change
  provider_dispatch p50    1.189/1.071    p95    2.071/2.102    delta  -0.118 ms  control-no-change
  session_save      p50   21.980/18.997   p95   75.975/149.671  delta  -2.983 ms  control-no-change
  total store_read_bytes p50 5311296  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22964 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22972 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.029/4.119    p95    5.174/4.988    delta  +0.090 ms  control-no-change
  skill_request_overlay p50    5.694/5.258    p95    7.188/6.068    delta  -0.435 ms  control-no-change
  mcp_config_load   p50    0.284/0.273    p95    0.355/0.516    delta  -0.011 ms  control-no-change
  attach_history    p50  313.061/303.770  p95  337.142/363.559  delta  -9.291 ms  control-no-change
  resume_ready      p50   63.776/58.438   p95   85.345/63.370   delta  -5.338 ms  control-no-change
  provider_dispatch p50    1.075/1.052    p95    1.826/1.203    delta  -0.024 ms  control-no-change
  session_save      p50   20.112/18.431   p95   28.453/23.819   delta  -1.681 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22972 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189199..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099137..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22936..22972 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.028    p95    0.060/0.038    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.214/0.225    p95    0.530/0.326    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.252/0.264    p95   10.177/0.420    delta  +0.012 ms  control-no-change
  attach_history    p50 3884.968/4180.926 p95 4135.646/4313.700 delta +295.957 ms  control-no-change
  resume_ready      p50  780.620/777.810  p95  856.050/869.334  delta  -2.810 ms  control-no-change
  provider_dispatch p50   10.096/11.193   p95   13.482/17.148   delta  +1.097 ms  control-drift
  session_save      p50   23.900/25.893   p95   33.753/134.738  delta  +1.993 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22935..22967 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22939..22975 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    5.285/4.163    p95    8.539/5.178    delta  -1.122 ms  control-no-change
  skill_request_overlay p50    5.527/5.273    p95    8.302/8.916    delta  -0.254 ms  control-no-change
  mcp_config_load   p50    0.268/0.282    p95    0.422/0.372    delta  +0.014 ms  control-no-change
  attach_history    p50 3690.017/3752.381 p95 4552.464/4148.584 delta +62.365 ms  control-no-change
  resume_ready      p50  788.908/773.100  p95 1004.648/885.420  delta -15.808 ms  control-no-change
  provider_dispatch p50   11.393/11.268   p95   18.122/15.055   delta  -0.125 ms  control-no-change
  session_save      p50   30.000/27.425   p95   58.466/129.763  delta  -2.575 ms  control-no-change
  total store_read_bytes p50 53417516  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184297..42184305 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22954..22967 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22979 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (4e8292a1600d)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-gsqng4s6
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.025/0.024    p95    0.034/0.028    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.212/0.216    p95    0.257/0.244    delta  +0.004 ms  control-no-change
  mcp_config_load   p50    0.253/0.251    p95    0.295/0.285    delta  -0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   11.287/11.476   p95   12.685/12.762   delta  +0.188 ms  control-no-change
  total store_read_bytes p50 23658  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22939, 22947, 22951, 23657, 23659, 23665, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22935, 22937, 22943, 22959, 23653, 23661, 23669, 24371, 24375, 24387]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22939..24379 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22935..24387 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.115/3.763    p95    5.483/5.454    delta  -0.352 ms  control-no-change
  skill_request_overlay p50    5.401/4.859    p95    5.871/5.831    delta  -0.543 ms  control-no-change
  mcp_config_load   p50    0.283/0.253    p95    0.571/0.315    delta  -0.031 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   12.724/13.101   p95   18.943/14.996   delta  +0.377 ms  control-no-change
  total store_read_bytes p50 23670  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22943, 22945, 23647, 23667, 23673, 24361, 24369, 24377, 24385]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22931, 22947, 22959, 23653, 23657, 23669, 24363, 24371, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24385 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22931..24383 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.026/0.036    p95    0.043/0.130    delta  +0.010 ms  control-no-change
  skill_request_overlay p50    0.248/0.242    p95    0.286/0.505    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.265/0.309    p95    0.446/0.543    delta  +0.044 ms  control-drift
  attach_history    p50  119.945/122.276  p95  140.438/168.354  delta  +2.331 ms  control-no-change
  resume_ready      p50   74.810/79.387   p95   95.116/113.012  delta  +4.578 ms  control-no-change
  provider_dispatch p50    1.128/1.281    p95    1.225/2.267    delta  +0.153 ms  control-no-change
  session_save      p50   15.786/15.832   p95   58.903/24.104   delta  +0.046 ms  control-no-change
  total store_read_bytes p50 2841243  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23666, 23668, 24366, 24368, 25084, 25102, 25802, 25812, 26506, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23662, 23666, 24382, 24388, 25100, 25110, 25804, 25806, 26516, 26530]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23666..26530 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23662..26530 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.036/4.084    p95    4.607/6.540    delta  +0.048 ms  control-no-change
  skill_request_overlay p50    5.362/4.994    p95    5.898/8.391    delta  -0.368 ms  control-no-change
  mcp_config_load   p50    0.274/0.271    p95    0.310/0.607    delta  -0.003 ms  control-no-change
  attach_history    p50  106.499/103.155  p95  116.749/129.301  delta  -3.343 ms  control-no-change
  resume_ready      p50   63.666/64.353   p95  101.869/73.112   delta  +0.687 ms  control-no-change
  provider_dispatch p50    1.133/1.050    p95    1.480/1.195    delta  -0.083 ms  control-no-change
  session_save      p50   15.129/13.859   p95   21.149/18.521   delta  -1.270 ms  control-no-change
  total store_read_bytes p50 2841254  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23672, 24380, 24394, 25090, 25102, 25800, 25812, 26526, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23670, 24388, 24396, 25098, 25106, 25812, 25820, 26518, 26526]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26530 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26526 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.030/0.029    p95    0.087/0.038    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.247/0.241    p95    0.603/0.442    delta  -0.006 ms  control-no-change
  mcp_config_load   p50    0.276/0.277    p95    0.723/0.551    delta  +0.000 ms  control-no-change
  attach_history    p50 1581.656/1601.475 p95 1984.641/2047.052 delta +19.820 ms  control-no-change
  resume_ready      p50 1052.308/988.267  p95 1279.042/1278.184 delta -64.041 ms  control-no-change
  provider_dispatch p50   12.787/13.215   p95   19.437/19.272   delta  +0.429 ms  control-no-change
  session_save      p50   26.001/22.715   p95   49.553/65.601   delta  -3.286 ms  control-no-change
  total store_read_bytes p50 28640276  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23671, 23677, 24385, 24387, 25085, 25117, 25815, 26515, 26533]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23661, 23685, 24383, 24387, 25099, 25109, 25799, 25821, 26525, 26527]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23671..26533 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23661..26527 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    5.337/5.015    p95    8.842/15.644   delta  -0.322 ms  control-no-change
  skill_request_overlay p50    6.190/6.056    p95    7.240/18.632   delta  -0.135 ms  control-no-change
  mcp_config_load   p50    0.340/0.303    p95    0.874/0.841    delta  -0.037 ms  control-no-change
  attach_history    p50 1371.598/1295.943 p95 1560.019/1547.454 delta -75.655 ms  control-no-change
  resume_ready      p50  860.208/766.411  p95 1271.788/971.724  delta -93.797 ms  control-no-change
  provider_dispatch p50   12.780/11.164   p95   23.227/15.628   delta  -1.616 ms  control-no-change
  session_save      p50   27.722/21.600   p95   48.236/32.286   delta  -6.122 ms  control-no-change
  total store_read_bytes p50 28640272  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23681, 24379, 24383, 25105, 25815, 25831, 26529, 26537]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23677, 23685, 24387, 25093, 25109, 25823, 26529, 26533]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26537 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23677..26533 (8 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.5 ms | 0.6 ms | 0.3 ms | 0.6 ms | 5 |
| read (1000 lines) | 0.7 ms | 0.8 ms | 0.6 ms | 0.8 ms | 5 |
| read (10 lines @ offset) | 0.6 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.8 ms | 0.4 ms | 0.8 ms | 5 |
| write (small) | 2.6 ms | 2.9 ms | 2.0 ms | 2.9 ms | 5 |
| write (10KB) | 3.0 ms | 3.5 ms | 2.7 ms | 3.5 ms | 5 |
| edit (simple replace) | 2.4 ms | 2.6 ms | 2.4 ms | 2.6 ms | 5 |
| ls (scripts dir) | 0.9 ms | 1.1 ms | 0.8 ms | 1.1 ms | 5 |
| agentgrep (grep in tree) | 4.0 ms | 4.1 ms | 3.5 ms | 4.1 ms | 5 |
| agentgrep (find **/*.rs) | 1.9 ms | 3.6 ms | 1.7 ms | 3.6 ms | 5 |
| bash (true) | 6.7 ms | 8.2 ms | 5.4 ms | 8.2 ms | 5 |
| bash (echo hello) | 6.4 ms | 8.4 ms | 5.5 ms | 8.4 ms | 5 |
| bash (ls fixture tree) | 8.1 ms | 9.4 ms | 7.9 ms | 9.4 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 84.9 ms | 84.9 ms | 84.9 ms | 85.1 ms | 0 |
| 10 | 310.6 ms | 313.1 ms | 313.1 ms | 313.3 ms | 0 |
| 40 | 1.37 s | 1.38 s | 1.38 s | 1.38 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.02 s | 1.02 s | 1.02 s | 1 |
| leaf_touch | src/main.rs | 512.5 ms | 512.5 ms | 512.5 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 19.70 s | 19.70 s | 19.70 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 33.44 s | 33.44 s | 33.44 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (805b408) | 14.1 ms | 95.8 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.1 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 43.2 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 106.7 ms | 479.4 MB |
| OpenCode | 1.19.5 | 597.2 ms | 605.3 MB |
| pi | 1.5.0 | 326.2 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 782.4 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 226.3 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 208.5 ms | 207.4 ms–293.3 ms | baseline | 30.5 MB | baseline | yes | — |
| jcode | 153.3 ms | 142.3 ms–237.3 ms | 1.36× less | 30.2 MB | same | yes | — |
| Claude Code | 1.62 s | 1.42 s–2.18 s | 7.75× more | 3.0 GB | 102.44× more | yes | — |
| Codex CLI | 1.98 s | 1.80 s–2.59 s | 9.48× more | 256.8 MB | 8.43× more | yes | — |
| OpenCode | 5.72 s | 5.57 s–9.04 s | 27.44× more | 910.9 MB | 29.90× more | yes | — |
| pi | 572.1 ms | 564.6 ms–644.7 ms | 2.74× more | 120.2 MB | 3.95× more | yes | — |
| oh-my-pi (omp) | 3.40 s | 3.24 s–3.61 s | 16.33× more | 409.9 MB | 13.45× more | yes | — |
| oh-my-openagent (omo) | 21.21 s | 18.60 s–22.94 s | 101.75× more | 543.1 MB | 17.83× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 220.5 ms | 213.8 ms–222.3 ms | baseline | 25.2 MB | baseline | skill-uri | yes | — |
| jcode | 200.9 ms | 192.0 ms–203.4 ms | 1.10× less | 23.0 MB | 1.10× less | skill-tool | yes | — |
| Claude Code | 1.01 s | 992.6 ms–1.03 s | 4.59× more | 240.4 MB | 9.52× more | skill-tool | yes | — |
| Codex CLI | 2.16 s | 2.12 s–2.79 s | 9.78× more | 254.7 MB | 10.09× more | file-read | yes | — |
| OpenCode | 8.08 s | 7.95 s–9.51 s | 36.66× more | 845.9 MB | 33.50× more | skill-tool | yes | — |
| pi | 745.0 ms | 728.5 ms–762.3 ms | 3.38× more | 97.8 MB | 3.87× more | file-read | yes | — |
| oh-my-pi (omp) | 3.99 s | 3.93 s–12.46 s | 18.08× more | 412.5 MB | 16.34× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.20 s | 19.73 s–25.15 s | 96.13× more | 538.9 MB | 21.34× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 269.4 ms | 241.3 ms–278.3 ms | baseline | 37.4 MB | baseline | mcp-call | yes | 183.6 ms | — |
| jcode | 318.5 ms | 316.5 ms–329.7 ms | 1.18× more | 36.3 MB | 1.03× less | mcp-action | yes | 227.7 ms | — |
| Claude Code | 1.13 s | 1.07 s–1.36 s | 4.19× more | 273.0 MB | 7.30× more | mcp-tool | yes | 956.9 ms | — |
| Codex CLI | 862.3 ms | 832.7 ms–3.07 s | 3.20× more | 179.1 MB | 4.79× more | mcp-tool | yes | 648.4 ms | — |
| OpenCode | 8.53 s | 7.92 s–11.33 s | 31.66× more | 749.0 MB | 20.03× more | mcp-tool | yes | 6.37 s | — |
| pi | 812.6 ms | 803.0 ms–817.9 ms | 3.02× more | 118.0 MB | 3.16× more | mcp-tool | yes | 680.4 ms | — |
| oh-my-pi (omp) | 6.86 s | 4.69 s–10.30 s | 25.47× more | 415.5 MB | 11.11× more | mcp-route | yes | 6.68 s | — |
| oh-my-openagent (omo) | 23.31 s | 19.82 s–25.44 s | 86.51× more | 563.8 MB | 15.07× more | mcp-tool | yes | 20.03 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.6 MB | 142.4 KB | baseline | — | — | 29.9 MB | 431.1 KB |
| jcode | 22.1 MB | 72.3 KB | 1.97× less | — | — | 26.8 MB | 340.2 KB |
| Claude Code | 153.8 MB | 155.1 MB | 1,115× more | — | — | 188.0 MB | 190.6 MB |
| Codex CLI | 124.7 MB | 97.3 MB | 699.27× more | — | — | 166.6 MB | 123.4 MB |
| OpenCode | 245.1 MB | 203.3 KB | 1.43× more | — | — | 430.0 MB | 6.6 MB |
| pi | 91.3 MB | 90.6 MB | 651.36× more | — | — | 101.7 MB | 100.8 MB |
| oh-my-pi (omp) | 300.4 MB | 75.5 MB | 542.84× more | — | — | 342.2 MB | 335.9 MB |
| oh-my-openagent (omo) | 145.1 MB | 128.4 MB | 923.23× more | — | — | 416.3 MB | 403.0 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.9 MB | 32.0 MB | 33.7 MB |
| jcode | 26.8 MB | 28.4 MB | 29.8 MB |
| Claude Code | 188.0 MB | 944.6 MB | 1.9 GB |
| Codex CLI | 166.6 MB | 657.9 MB | 1.2 GB |
| OpenCode | 430.0 MB | 546.2 MB | 495.6 MB |
| pi | 101.7 MB | 504.4 MB | 1009.1 MB |
| oh-my-pi (omp) | 342.2 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 416.3 MB | 2.1 GB | 4.0 GB |

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    11.02 ms
  Max:    11.93 ms
  Mean:   11.40 ms
  Median: 11.41 ms
  Stdev:  0.31 ms

--version:
  Min:    10.83 ms
  Max:    17.51 ms
  Mean:   13.26 ms
  Median: 12.33 ms
  Stdev:  2.38 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.96 ms
  Max:    27.32 ms
  Mean:   23.07 ms
  Median: 22.48 ms
  Stdev:  1.58 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~11.4 ms
  Server ready median:     ~22.5 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (827bcf138ec2)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-1hmerzmu
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.104/0.102    p95    0.159/0.121    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.754/0.772    p95    0.803/0.876    delta  +0.018 ms  control-no-change
  mcp_config_load   p50    0.930/0.917    p95    1.010/0.989    delta  -0.014 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.001    delta  +0.000 ms  control-drift
  session_save      p50   51.236/50.946   p95   98.876/141.963  delta  -0.290 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22924..22960 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22964 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   15.254/14.873   p95   16.363/15.723   delta  -0.381 ms  control-no-change
  skill_request_overlay p50   40.643/41.786   p95   44.650/46.016   delta  +1.143 ms  control-no-change
  mcp_config_load   p50    1.027/1.091    p95    1.147/1.219    delta  +0.064 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.004/0.005    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   59.321/67.318   p95  118.517/185.040  delta  +7.997 ms  control-no-change
  total store_read_bytes p50 22934  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22910..22964 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22902..22960 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.104/0.100    p95    0.140/0.136    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.773/0.817    p95    0.797/0.873    delta  +0.044 ms  control-no-change
  mcp_config_load   p50    0.967/0.956    p95    1.008/1.007    delta  -0.011 ms  control-no-change
  attach_history    p50  453.241/437.956  p95  527.248/588.913  delta -15.285 ms  control-no-change
  resume_ready      p50  152.479/152.291  p95  162.966/206.899  delta  -0.188 ms  control-no-change
  provider_dispatch p50    1.477/1.541    p95    1.552/1.654    delta  +0.064 ms  control-no-change
  session_save      p50   66.156/47.397   p95  204.522/369.586  delta -18.759 ms  control-no-change
  total store_read_bytes p50 5311213  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22931..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189149..4189153 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099101..1099105 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22924..22975 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.692/15.874   p95   16.396/17.761   delta  +0.182 ms  control-no-change
  skill_request_overlay p50   39.359/39.558   p95   40.698/44.390   delta  +0.199 ms  control-no-change
  mcp_config_load   p50    1.062/1.079    p95    1.187/1.154    delta  +0.017 ms  control-no-change
  attach_history    p50  447.852/445.973  p95  529.773/459.235  delta  -1.879 ms  control-no-change
  resume_ready      p50  153.400/152.909  p95  185.069/168.067  delta  -0.491 ms  control-no-change
  provider_dispatch p50    1.468/1.496    p95    1.521/1.960    delta  +0.027 ms  control-no-change
  session_save      p50   53.556/53.292   p95  169.546/164.440  delta  -0.264 ms  control-no-change
  total store_read_bytes p50 5311208  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22924..22968 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22897..22968 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.102/0.101    p95    0.134/0.134    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.784/0.808    p95    0.920/0.910    delta  +0.024 ms  control-no-change
  mcp_config_load   p50    0.977/0.963    p95    1.041/1.031    delta  -0.014 ms  control-no-change
  attach_history    p50 5041.514/5025.402 p95 5312.070/5074.502 delta -16.113 ms  control-no-change
  resume_ready      p50 1615.401/1609.245 p95 1638.766/1652.737 delta  -6.156 ms  control-no-change
  provider_dispatch p50   18.888/18.306   p95   24.594/19.543   delta  -0.583 ms  control-no-change
  session_save      p50   47.633/49.026   p95  111.106/146.654  delta  +1.393 ms  control-no-change
  total store_read_bytes p50 53417423  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184251 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210214..11210215 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22917..22978 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184247..42184251 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210211..11210215 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22970 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.255/15.315   p95   19.842/16.049   delta  +0.060 ms  control-no-change
  skill_request_overlay p50   39.175/39.900   p95   43.243/43.424   delta  +0.725 ms  control-no-change
  mcp_config_load   p50    1.081/1.051    p95    1.251/1.152    delta  -0.030 ms  control-no-change
  attach_history    p50 4942.467/4939.104 p95 5519.570/5031.975 delta  -3.364 ms  control-no-change
  resume_ready      p50 1578.372/1579.360 p95 1760.529/1619.367 delta  +0.987 ms  control-no-change
  provider_dispatch p50   18.014/18.393   p95   20.392/19.478   delta  +0.379 ms  control-no-change
  session_save      p50   48.911/53.030   p95  140.878/324.404  delta  +4.119 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22919..22983 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22979 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (827bcf138ec2)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-f02gizk1
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.092/0.093    p95    0.105/0.099    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.693/0.750    p95    0.748/0.955    delta  +0.057 ms  control-drift
  mcp_config_load   p50    0.907/0.932    p95    0.977/1.038    delta  +0.025 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   49.878/48.319   p95   63.239/50.253   delta  -1.559 ms  control-no-change
  total store_read_bytes p50 23630  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22910, 22918, 22934, 22957, 23610, 23651, 23657, 24326, 24338, 24339]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22913, 22943, 22949, 22957, 23636, 23650, 23654, 24337, 24341, 24355]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22910..24339 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22913..24355 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   14.309/14.397   p95   15.623/16.991   delta  +0.088 ms  control-no-change
  skill_request_overlay p50   38.975/38.086   p95   45.551/45.814   delta  -0.889 ms  control-no-change
  mcp_config_load   p50    0.946/0.990    p95    1.031/1.125    delta  +0.044 ms  control-no-change
  attach_history    p50    0.002/0.003    p95    0.006/0.006    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   63.569/63.426   p95  367.648/169.003  delta  -0.143 ms  control-no-change
  total store_read_bytes p50 23636  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22896, 22933, 22954, 22958, 23625, 23648, 23653, 24330, 24348, 24356]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22942, 22951, 22956, 22957, 23603, 23619, 23645, 23656, 24326, 24344]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22896..24356 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22942..24344 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.093/0.103    p95    0.097/0.161    delta  +0.010 ms  control-no-change
  skill_request_overlay p50    0.705/0.752    p95    1.055/0.801    delta  +0.047 ms  control-no-change
  mcp_config_load   p50    0.918/0.919    p95    1.069/1.169    delta  +0.002 ms  control-no-change
  attach_history    p50  207.111/206.069  p95  222.231/213.914  delta  -1.042 ms  control-no-change
  resume_ready      p50  152.962/151.390  p95  160.300/155.432  delta  -1.572 ms  control-no-change
  provider_dispatch p50    1.773/1.709    p95    2.224/1.737    delta  -0.065 ms  control-no-change
  session_save      p50   49.867/50.591   p95   71.553/51.891   delta  +0.724 ms  control-no-change
  total store_read_bytes p50 2841133  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23637, 23663, 24352, 24366, 25045, 25061, 25740, 25768, 26463]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23627, 23663, 24336, 24354, 25002, 25049, 25740, 25753, 26408, 26447]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23637..26463 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23627..26447 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.673/14.508   p95   16.279/16.874   delta  -0.164 ms  control-no-change
  skill_request_overlay p50   38.442/39.028   p95   41.874/45.670   delta  +0.586 ms  control-no-change
  mcp_config_load   p50    0.994/0.994    p95    1.034/1.125    delta  +0.000 ms  control-no-change
  attach_history    p50  208.359/207.404  p95  230.213/220.248  delta  -0.955 ms  control-no-change
  resume_ready      p50  149.262/155.367  p95  160.246/162.586  delta  +6.105 ms  control-no-change
  provider_dispatch p50    1.705/1.661    p95    2.131/1.811    delta  -0.044 ms  control-no-change
  session_save      p50   54.887/54.988   p95   61.014/63.594   delta  +0.101 ms  control-no-change
  total store_read_bytes p50 2841127  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23599, 23667, 24356, 24358, 25017, 25077, 25752, 25772, 26450, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23651, 23667, 24338, 24358, 25020, 25058, 25761, 26448, 26464]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23599..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23651..26464 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.092/0.092    p95    0.199/0.131    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.751/0.738    p95    0.813/0.792    delta  -0.013 ms  control-no-change
  mcp_config_load   p50    0.865/0.939    p95    1.141/1.030    delta  +0.074 ms  control-no-change
  attach_history    p50 2181.302/2169.503 p95 2600.420/2294.437 delta -11.798 ms  control-no-change
  resume_ready      p50 1599.370/1573.023 p95 1697.621/1709.557 delta -26.347 ms  control-no-change
  provider_dispatch p50   17.811/17.905   p95   23.589/20.202   delta  +0.094 ms  control-no-change
  session_save      p50   49.161/51.651   p95  214.742/229.047  delta  +2.490 ms  control-no-change
  total store_read_bytes p50 28640144  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23589, 23641, 24342, 24364, 25037, 25058, 25721, 25762, 26429, 26438]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23656, 23674, 24345, 24346, 25038, 25060, 25745, 25767, 26408, 26410]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23589..26438 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23656..26410 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.130/14.361   p95   16.414/18.365   delta  +0.231 ms  control-no-change
  skill_request_overlay p50   38.170/38.391   p95   41.345/41.754   delta  +0.221 ms  control-no-change
  mcp_config_load   p50    1.008/1.007    p95    1.081/1.091    delta  -0.001 ms  control-no-change
  attach_history    p50 2169.130/2161.407 p95 2210.605/2176.481 delta  -7.723 ms  control-no-change
  resume_ready      p50 1566.955/1564.135 p95 1598.768/1587.800 delta  -2.819 ms  control-no-change
  provider_dispatch p50   17.417/17.517   p95   18.630/24.040   delta  +0.100 ms  control-no-change
  session_save      p50   48.679/49.755   p95   51.789/68.989   delta  +1.076 ms  control-no-change
  total store_read_bytes p50 28640144  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23629, 23637, 24339, 24372, 25046, 25051, 25729, 25764, 26463, 26468]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23642, 23686, 24361, 24373, 25035, 25060, 25723, 25739, 26410, 26454]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23629..26468 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23642..26454 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.4 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| write (small) | 13.0 ms | 33.1 ms | 10.1 ms | 33.1 ms | 5 |
| write (10KB) | 12.5 ms | 20.2 ms | 10.4 ms | 20.2 ms | 5 |
| edit (simple replace) | 10.4 ms | 15.0 ms | 9.9 ms | 15.0 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 5.2 ms | 5.3 ms | 4.7 ms | 5.3 ms | 5 |
| agentgrep (find **/*.rs) | 5.2 ms | 5.2 ms | 4.7 ms | 5.2 ms | 5 |
| bash (true) | 7.8 ms | 7.8 ms | 7.3 ms | 7.8 ms | 5 |
| bash (echo hello) | 7.8 ms | 7.8 ms | 7.8 ms | 7.8 ms | 5 |
| bash (ls fixture tree) | 12.6 ms | 13.0 ms | 10.4 ms | 13.0 ms | 5 |
| invalid tool (dispatch only) | 2.6 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 274.6 ms | 274.6 ms | 274.6 ms | 274.8 ms | 0 |
| 10 | 1.11 s | 1.12 s | 1.12 s | 1.12 s | 0 |
| 40 | 2.00 s | 2.99 s | 3.24 s | 3.25 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 717.1 ms | 717.1 ms | 717.1 ms | 1 |
| leaf_touch | src\main.rs | 969.2 ms | 969.2 ms | 969.2 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 30.64 s | 30.64 s | 30.64 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 43.27 s | 43.27 s | 43.27 s | 1 |

## TUI (`horon-tui`)

The interactive client is a second binary and is measured on its own. These numbers come from the `tui` suite and are never compared with the headless tables above: `first_frame_ms` and `direct_first_frame_ms` are two launches of two different programs, and the handoff is the difference between them.

![TUI (`horon-tui`)](charts/tui-01-latency.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 48.7 ms | 46.3 ms–117.1 ms | 5 |
| First frame (direct `horon-tui`) | 42.8 ms | 42.3 ms–44.4 ms | 5 |
| TTY handoff cost | 5.9 ms | 1.9 ms–74.4 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.2 ms–1.0 ms | 50 |
| Idle RSS after 5 s | 5.4 MB | 5.2 MB–5.4 MB | 5 |
| RSS after one turn | 5.5 MB | 5.4 MB–5.6 MB | 5 |
| Frame draw time (p50) | 1.9 ms | 1.9 ms–1.9 ms | 5 |
| Frame draw time (p95) | 2.0 ms | 1.9 ms–2.0 ms | 5 |
| Install size (`horon-tui`) | 5.6 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 80.1 ms | 58.2 ms–383.1 ms | 5 |
| First frame (direct `horon-tui`) | 72.3 ms | 53.8 ms–107.7 ms | 5 |
| TTY handoff cost | 11.3 ms | -35.0 ms–315.9 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.2 ms–0.9 ms | 50 |
| Idle RSS after 5 s | 8.9 MB | 8.8 MB–8.9 MB | 5 |
| RSS after one turn | 9.0 MB | 9.0 MB–9.1 MB | 5 |
| Frame draw time (p50) | 1.4 ms | 1.2 ms–1.5 ms | 5 |
| Frame draw time (p95) | 3.0 ms | 2.7 ms–3.1 ms | 5 |
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
| Frame draw time (p50) | 3.1 ms | 2.9 ms–3.1 ms | 5 |
| Frame draw time (p95) | 3.3 ms | 3.2 ms–4.2 ms | 5 |
| Install size (`horon-tui`) | 4.9 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
