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

Four structural items were measured and then rejected. Each item carries the numbers a later attempt would have to beat to overturn that decision.

| Item | Decision | Numbers behind it |
| --- | --- | --- |
| Idle session eviction policy | not adopted | 0.999 fds and 0.998 threads per session, 0.41 MB RSS. The slope does not bend from 1 to 1,000 sessions. The `shared_context` entry and the debug job are 0 at all five population counts, and the soak returns the session count to 0 |
| `globalEvents` multiplexing | not adopted (protocol change) | 7.4 consumer-side fds and 3.0 threads per watched session. The daemon's own share is 1.0 connection per session, and the bridge attaches one session per connection |
| Narrowing the session map lock scope | not adopted | Removing an unrelated session waits for the whole import. 861 / 5,561 / 11,255 / 24,499 ms at 1 / 20 / 60 / 150 events, ratio 1.00 |
| Replacing the 25 ms interrupt poll timer with notification | not adopted | It would save a worst case of 29.7 ms (27.6 ms on average) at the cost of restructuring a wakeup once per watch-group round |

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

<!-- bench-auto:start commit=c4816f4a54c0622f768024fe57fe60476aafc661 -->

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `c4816f4a54c0622f768024fe57fe60476aafc661`
- Measured at: 2026-11-10 04:15 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/42750483292> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (c4816f4)` | Success |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (c4816f4)` | Success |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (c4816f4)` | Success |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (c4816f4) | 3.2 ms | 110.9 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 8.7 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 9.3 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 32.6 ms | 435.0 MB |
| OpenCode | 1.19.5 | 299.3 ms | 613.4 MB |
| pi | 1.5.0 | 144.2 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 439.0 ms | 412.6 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 78.3 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 80.6 ms | 43.1 ms–146.0 ms | baseline | 39.0 MB | baseline | yes | 14 | — |
| jcode | 138.6 ms | 137.9 ms–143.4 ms | 1.72× more | 40.0 MB | 1.03× more | yes | 5 | — |
| Claude Code | 368.7 ms | 361.1 ms–388.7 ms | 4.58× more | 204.8 MB | 5.25× more | yes | — | — |
| Codex CLI | 754.9 ms | 636.5 ms–1.44 s | 9.37× more | 201.1 MB | 5.15× more | yes | 6 | — |
| OpenCode | 1.99 s | 1.82 s–2.05 s | 24.68× more | 576.0 MB | 14.75× more | yes | 4 | — |
| pi | 243.1 ms | 238.2 ms–258.6 ms | 3.02× more | 113.9 MB | 2.92× more | yes | — | — |
| oh-my-pi (omp) | 1.14 s | 1.10 s–1.42 s | 14.21× more | 386.7 MB | 9.90× more | yes | 66 | — |
| oh-my-openagent (omo) | 4.11 s | 3.86 s–4.21 s | 51.04× more | 656.6 MB | 16.82× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 42.5 ms | 41.8 ms–44.1 ms | baseline | 36.9 MB | baseline | skill-uri | yes | — |
| jcode | 48.3 ms | 47.8 ms–55.8 ms | 1.14× more | 41.0 MB | 1.11× more | skill-tool | yes | — |
| Claude Code | 309.9 ms | 303.5 ms–322.7 ms | 7.29× more | 203.3 MB | 5.50× more | skill-tool | yes | — |
| Codex CLI | 928.7 ms | 682.8 ms–1.75 s | 21.86× more | 206.0 MB | 5.58× more | file-read | yes | — |
| OpenCode | 2.10 s | 2.05 s–2.14 s | 49.39× more | 604.7 MB | 16.37× more | skill-tool | yes | — |
| pi | 325.9 ms | 322.3 ms–334.9 ms | 7.67× more | 126.4 MB | 3.42× more | file-read | yes | — |
| oh-my-pi (omp) | 1.09 s | 1.07 s–1.18 s | 25.64× more | 348.2 MB | 9.42× more | file-read | yes | — |
| oh-my-openagent (omo) | 4.27 s | 4.18 s–4.42 s | 100.55× more | 672.9 MB | 18.22× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 66.1 ms | 61.1 ms–90.9 ms | baseline | 49.9 MB | baseline | mcp-call | yes | 48.9 ms | — |
| jcode | 165.4 ms | 164.3 ms–184.1 ms | 2.50× more | 55.5 MB | 1.11× more | mcp-action | yes | 140.5 ms | — |
| Claude Code | 393.0 ms | 386.4 ms–398.2 ms | 5.95× more | 227.6 MB | 4.56× more | mcp-tool | yes | 270.3 ms | — |
| Codex CLI | 979.8 ms | 670.0 ms–1.38 s | 14.82× more | 205.4 MB | 4.12× more | mcp-tool | yes | 215.7 ms | — |
| OpenCode | 1.90 s | 1.83 s–1.98 s | 28.78× more | 564.1 MB | 11.31× more | mcp-tool | yes | 1.72 s | — |
| pi | 349.9 ms | 339.6 ms–353.8 ms | 5.29× more | 138.2 MB | 2.77× more | mcp-tool | yes | 307.6 ms | — |
| oh-my-pi (omp) | 1.10 s | 1.08 s–1.13 s | 16.59× more | 349.0 MB | 7.00× more | mcp-route | yes | 1.02 s | — |
| oh-my-openagent (omo) | 4.32 s | 4.27 s–4.47 s | 65.30× more | 665.3 MB | 13.34× more | mcp-tool | yes | 2.31 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 36.3 MB | 164.0 KB | baseline | 0.79 | 1.00 | 43.3 MB | 501.6 KB |
| jcode | 42.8 MB | 82.0 KB | 2.00× less | 0.21 | — | 46.3 MB | 245.0 KB |
| Claude Code | 151.6 MB | 149.8 MB | 935.07× more | — | — | 187.6 MB | 180.7 MB |
| Codex CLI | 128.3 MB | 133.1 MB | 830.81× more | — | — | 162.0 MB | 164.6 MB |
| OpenCode | 245.3 MB | 0.0 B | — | — | — | 444.1 MB | 6.1 MB |
| pi | 108.1 MB | 109.3 MB | 682.30× more | — | — | 132.5 MB | 127.9 MB |
| oh-my-pi (omp) | 312.8 MB | 150.3 MB | 938.76× more | — | — | 365.8 MB | 353.4 MB |
| oh-my-openagent (omo) | 331.0 MB | 32.6 MB | 203.80× more | — | — | 581.6 MB | 583.8 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 43.3 MB | 45.8 MB | 47.7 MB |
| jcode | 46.3 MB | 47.7 MB | 48.5 MB |
| Claude Code | 187.6 MB | 913.8 MB | 1.8 GB |
| Codex CLI | 162.0 MB | 814.6 MB | 1.6 GB |
| OpenCode | 444.1 MB | 528.8 MB | 503.0 MB |
| pi | 132.5 MB | 657.3 MB | 1.3 GB |
| oh-my-pi (omp) | 365.8 MB | 1.9 GB | 3.5 GB |
| oh-my-openagent (omo) | 581.6 MB | 3.0 GB | 5.7 GB |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.92 ms
  Max:    4.33 ms
  Mean:   4.08 ms
  Median: 4.02 ms
  Stdev:  0.14 ms

--version:
  Min:    3.72 ms
  Max:    3.96 ms
  Mean:   3.86 ms
  Median: 3.88 ms
  Stdev:  0.10 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.57 ms
  Max:    20.70 ms
  Mean:   20.63 ms
  Median: 20.60 ms
  Stdev:  0.05 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.58 ms
  Max:    20.87 ms
  Mean:   20.78 ms
  Median: 20.79 ms
  Stdev:  0.08 ms

Bridge stage: hello_ok:
  Min:    21.22 ms
  Max:    21.72 ms
  Mean:   21.50 ms
  Median: 21.48 ms
  Stdev:  0.15 ms

Bridge stage: no-op reply:
  Min:    21.69 ms
  Max:    22.20 ms
  Mean:   21.96 ms
  Median: 21.97 ms
  Stdev:  0.14 ms

Bridge total (socket -> no-op):
  Min:    21.69 ms
  Max:    22.20 ms
  Mean:   21.96 ms
  Median: 21.97 ms
  Stdev:  0.14 ms

============================================================
Summary:
  Binary load median:      ~4.0 ms
  Server ready median:     ~20.6 ms
  Bridge total median:     ~22.0 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (44219deaf9a6)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-x9qe9ta0
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.007/0.007    p95    0.007/0.009    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.041/0.041    p95    0.041/0.042    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.057/0.058    p95    0.058/0.061    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  session_save      p50    7.321/6.896    p95  343.564/138.191  delta  -0.425 ms  control-no-change
  total store_read_bytes p50 23074  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23092 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23088 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.370/1.350    p95    1.597/1.460    delta  -0.020 ms  control-no-change
  skill_request_overlay p50    2.024/1.998    p95    2.440/2.155    delta  -0.026 ms  control-no-change
  mcp_config_load   p50    0.063/0.063    p95    0.084/0.066    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   14.842/20.301   p95   63.757/34.592   delta  +5.459 ms  control-no-change
  total store_read_bytes p50 23080  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23072..23084 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23088 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.006/0.006    p95    0.019/0.006    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.040/0.040    p95    0.059/0.052    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.057/0.057    p95    0.059/0.081    delta  +0.001 ms  control-no-change
  attach_history    p50  206.090/207.148  p95  218.661/229.062  delta  +1.058 ms  control-no-change
  resume_ready      p50   29.452/29.378   p95   30.502/30.067   delta  -0.074 ms  control-no-change
  provider_dispatch p50    1.034/1.039    p95    1.058/1.080    delta  +0.005 ms  control-no-change
  session_save      p50    7.192/6.996    p95   34.357/43.202   delta  -0.195 ms  control-no-change
  total store_read_bytes p50 5311407  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23070..23090 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23090 (5 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.333/1.328    p95    1.399/1.368    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    1.968/1.967    p95    2.098/2.052    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.062/0.062    p95    0.069/0.069    delta  -0.000 ms  control-no-change
  attach_history    p50  208.281/206.211  p95  221.462/216.850  delta  -2.070 ms  control-no-change
  resume_ready      p50   29.269/29.291   p95   30.639/30.810   delta  +0.022 ms  control-no-change
  provider_dispatch p50    1.026/1.016    p95    1.045/1.062    delta  -0.011 ms  control-no-change
  session_save      p50    6.351/6.323    p95   15.348/10.655   delta  -0.028 ms  control-no-change
  total store_read_bytes p50 5311407  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23070..23085 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23086 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.006/0.006    p95    0.006/0.008    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.041/0.041    p95    0.055/0.058    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.058/0.057    p95    0.058/0.094    delta  -0.000 ms  control-no-change
  attach_history    p50 2692.191/2690.230 p95 2742.301/2920.573 delta  -1.961 ms  control-no-change
  resume_ready      p50  375.178/372.537  p95  383.113/405.033  delta  -2.641 ms  control-no-change
  provider_dispatch p50   12.634/12.465   p95   13.324/13.197   delta  -0.169 ms  control-no-change
  session_save      p50    6.583/7.021    p95    7.599/19.558   delta  +0.438 ms  control-drift
  total store_read_bytes p50 53417620  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23060..23088 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23071..23093 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.349/1.346    p95    1.368/1.536    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    1.929/1.915    p95    1.975/2.051    delta  -0.015 ms  control-no-change
  mcp_config_load   p50    0.061/0.062    p95    0.068/0.075    delta  +0.001 ms  control-no-change
  attach_history    p50 2640.698/2683.307 p95 2718.121/2749.996 delta +42.609 ms  control-no-change
  resume_ready      p50  365.326/371.278  p95  371.531/377.389  delta  +5.952 ms  control-no-change
  provider_dispatch p50   12.531/12.484   p95   13.024/13.059   delta  -0.048 ms  control-no-change
  session_save      p50    6.732/6.675    p95    7.683/8.128    delta  -0.058 ms  control-no-change
  total store_read_bytes p50 53417622  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23075..23097 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184293 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210238..11210246 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23061..23089 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (44219deaf9a6)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-iwefw0tt
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.017/0.023    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.139/0.127    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.164/0.145    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    8.711/8.808    p95   10.162/10.414   delta  +0.097 ms  control-no-change
  total store_read_bytes p50 23797  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23073, 23077, 23787, 23795, 23799, 24505, 24509]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23073, 23081, 23791, 23807, 24501, 24505, 24517, 24521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23073..24509 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24521 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.713/2.744    p95    2.750/3.221    delta  +0.031 ms  control-no-change
  skill_request_overlay p50    4.718/4.714    p95    4.818/4.785    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.148/0.148    p95    0.169/0.157    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    9.488/9.519    p95   11.451/11.036   delta  +0.031 ms  control-no-change
  total store_read_bytes p50 23780  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23065, 23067, 23073, 23081, 23777, 23783, 23785, 24503, 24509, 24521]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23071, 23073, 23775, 23781, 23787, 24499, 24501, 24513, 24525]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23065..24521 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24525 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.017/0.015    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.114/0.126    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.144    p95    0.169/0.181    delta  +0.001 ms  control-no-change
  attach_history    p50   97.849/98.024   p95   98.748/99.232   delta  +0.174 ms  control-no-change
  resume_ready      p50   53.244/53.409   p95   54.064/53.853   delta  +0.165 ms  control-no-change
  provider_dispatch p50    1.371/1.355    p95    1.400/1.383    delta  -0.016 ms  control-no-change
  session_save      p50    8.321/8.160    p95    8.497/11.948   delta  -0.161 ms  control-no-change
  total store_read_bytes p50 2841357  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23787, 24517, 24521, 25223, 25227, 25945, 25957, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23791, 23803, 24493, 24509, 25231, 25235, 25945, 25957, 26651, 26663]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23787..26671 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23791..26663 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.717/2.776    p95    2.752/2.823    delta  +0.058 ms  control-drift
  skill_request_overlay p50    4.672/4.686    p95    4.722/4.733    delta  +0.014 ms  control-no-change
  mcp_config_load   p50    0.147/0.146    p95    0.160/0.148    delta  -0.001 ms  control-no-change
  attach_history    p50   97.745/98.220   p95   98.850/99.245   delta  +0.475 ms  control-no-change
  resume_ready      p50   53.191/53.032   p95   53.730/53.732   delta  -0.159 ms  control-no-change
  provider_dispatch p50    1.342/1.339    p95    1.433/1.358    delta  -0.003 ms  control-no-change
  session_save      p50    9.335/9.217    p95    9.592/9.565    delta  -0.118 ms  control-no-change
  total store_read_bytes p50 2841359  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23776, 23780, 24510, 25224, 25226, 25942, 25948, 26662, 26672]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23784, 23794, 24504, 24510, 25220, 25228, 25946, 25950, 26654, 26664]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23776..26672 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23784..26664 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.016/0.018    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.105    p95    0.107/0.126    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.184/0.172    delta  -0.000 ms  control-no-change
  attach_history    p50 1100.447/1102.167 p95 1108.749/1111.010 delta  +1.719 ms  control-no-change
  resume_ready      p50  587.641/589.668  p95  602.017/608.719  delta  +2.027 ms  control-no-change
  provider_dispatch p50   15.280/15.257   p95   16.361/15.441   delta  -0.023 ms  control-no-change
  session_save      p50    7.840/7.892    p95    8.375/8.110    delta  +0.052 ms  control-no-change
  total store_read_bytes p50 28640372  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23799, 23803, 24505, 24521, 25215, 25227, 25941, 25953, 26675, 26679]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23779, 23799, 24513, 24517, 25231, 25235, 25945, 25957, 26671, 26675]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23799..26679 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23779..26675 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.792/2.759    p95    2.831/3.423    delta  -0.034 ms  control-no-change
  skill_request_overlay p50    4.698/4.659    p95    4.720/4.762    delta  -0.039 ms  control-no-change
  mcp_config_load   p50    0.148/0.151    p95    0.156/0.167    delta  +0.002 ms  control-no-change
  attach_history    p50 1109.353/1108.745 p95 1114.250/1119.602 delta  -0.608 ms  control-no-change
  resume_ready      p50  609.405/609.813  p95  616.236/617.185  delta  +0.409 ms  control-no-change
  provider_dispatch p50   15.691/15.184   p95   16.234/16.871   delta  -0.507 ms  control-no-change
  session_save      p50    9.820/9.635    p95   10.326/10.952   delta  -0.184 ms  control-no-change
  total store_read_bytes p50 28640363  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23787, 23795, 24513, 24517, 25217, 25223, 25935, 25953, 26673, 26675]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23799, 23801, 24501, 24523, 25223, 25231, 25953, 25965, 26665, 26671]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23787..26675 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23799..26671 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.4 ms | 0.4 ms | 0.4 ms | 5 |
| read (10 lines @ offset) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.3 ms | 0.3 ms | 5 |
| write (small) | 1.0 ms | 1.3 ms | 0.9 ms | 1.3 ms | 5 |
| write (10KB) | 1.1 ms | 1.3 ms | 1.0 ms | 1.3 ms | 5 |
| edit (simple replace) | 1.0 ms | 1.1 ms | 1.0 ms | 1.1 ms | 5 |
| ls (scripts dir) | 0.7 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| agentgrep (grep in tree) | 1.6 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| agentgrep (find **/*.rs) | 1.2 ms | 1.2 ms | 1.2 ms | 1.2 ms | 5 |
| bash (true) | 1.6 ms | 1.7 ms | 1.5 ms | 1.7 ms | 5 |
| bash (echo hello) | 1.5 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| bash (ls fixture tree) | 2.6 ms | 2.6 ms | 2.5 ms | 2.6 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 70.8 ms | 70.8 ms | 70.8 ms | 73.8 ms | 0 |
| 10 | 226.9 ms | 282.2 ms | 282.2 ms | 282.5 ms | 0 |
| 40 | 1.14 s | 1.17 s | 1.17 s | 1.17 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 276.9 ms | 276.9 ms | 276.9 ms | 1 |
| leaf_touch | src/main.rs | 433.9 ms | 433.9 ms | 433.9 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 21.89 s | 21.89 s | 21.89 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 30.88 s | 30.88 s | 30.88 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (c4816f4) | 11.7 ms | 93.3 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.1 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 21.4 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 71.5 ms | 346.5 MB |
| OpenCode | 1.19.5 | 523.9 ms | 344.6 MB |
| pi | 1.5.0 | 299.6 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 522.8 ms | 273.0 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 163.8 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 95.1 ms | 80.9 ms–439.4 ms | baseline | 29.4 MB | baseline | yes | — |
| jcode | 428.4 ms | 387.4 ms–594.9 ms | 4.51× more | 34.1 MB | 1.16× more | yes | — |
| Claude Code | 665.4 ms | 459.2 ms–1.29 s | 7.00× more | 201.4 MB | 6.85× more | yes | — |
| Codex CLI | 635.2 ms | 593.1 ms–1.49 s | 6.68× more | 144.1 MB | 4.90× more | yes | — |
| OpenCode | 3.58 s | 2.80 s–4.06 s | 37.60× more | 677.7 MB | 23.07× more | yes | — |
| pi | 445.4 ms | 397.0 ms–526.4 ms | 4.68× more | 123.7 MB | 4.21× more | yes | — |
| oh-my-pi (omp) | 1.73 s | 1.21 s–2.12 s | 18.18× more | 416.0 MB | 14.16× more | yes | — |
| oh-my-openagent (omo) | 6.85 s | 6.42 s–10.53 s | 72.09× more | 842.4 MB | 28.68× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 87.5 ms | 70.8 ms–111.5 ms | baseline | 29.6 MB | baseline | skill-uri | yes | — |
| jcode | 327.8 ms | 310.6 ms–646.0 ms | 3.75× more | 29.2 MB | same | skill-tool | yes | — |
| Claude Code | 520.0 ms | 417.4 ms–678.9 ms | 5.95× more | 201.1 MB | 6.80× more | skill-tool | yes | — |
| Codex CLI | 641.0 ms | 552.0 ms–724.8 ms | 7.33× more | 144.2 MB | 4.88× more | file-read | yes | — |
| OpenCode | 4.91 s | 3.07 s–5.68 s | 56.09× more | 791.0 MB | 26.76× more | skill-tool | yes | — |
| pi | 726.5 ms | 596.6 ms–863.3 ms | 8.31× more | 139.6 MB | 4.72× more | file-read | yes | — |
| oh-my-pi (omp) | 1.33 s | 1.28 s–1.54 s | 15.21× more | 354.4 MB | 11.99× more | file-read | yes | — |
| oh-my-openagent (omo) | 8.43 s | 7.68 s–11.57 s | 96.36× more | 883.8 MB | 29.89× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 112.6 ms | 102.5 ms–160.8 ms | baseline | 44.5 MB | baseline | mcp-call | yes | 80.2 ms | — |
| jcode | 415.9 ms | 342.7 ms–529.3 ms | 3.69× more | 53.8 MB | 1.21× more | mcp-action | yes | 361.5 ms | — |
| Claude Code | 597.2 ms | 538.9 ms–788.0 ms | 5.30× more | 217.4 MB | 4.88× more | mcp-tool | yes | 443.7 ms | — |
| Codex CLI | 533.3 ms | 485.5 ms–640.7 ms | 4.74× more | 142.8 MB | 3.21× more | mcp-tool | yes | 274.7 ms | — |
| OpenCode | 3.27 s | 3.24 s–3.61 s | 29.07× more | 712.8 MB | 16.00× more | mcp-tool | yes | 2.87 s | — |
| pi | 527.6 ms | 491.8 ms–875.9 ms | 4.69× more | 132.2 MB | 2.97× more | mcp-tool | yes | 457.0 ms | — |
| oh-my-pi (omp) | 1.72 s | 1.49 s–2.17 s | 15.29× more | 407.7 MB | 9.15× more | mcp-route | yes | 1.61 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | no | — | failed: no tool that could reach the fixture was offered (one request, no tool call) |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.3 MB | 152.1 KB | baseline | 0.45 | 1.00 | 34.3 MB | 374.4 KB |
| jcode | 36.5 MB | 199.1 KB | 1.31× more | — | — | 40.1 MB | 315.1 KB |
| Claude Code | 159.5 MB | 151.0 MB | 1,017× more | — | — | 191.0 MB | 182.6 MB |
| Codex CLI | 114.9 MB | 82.1 MB | 552.70× more | — | — | 106.8 MB | 107.6 MB |
| OpenCode | 264.0 MB | 163.0 KB | 1.07× more | — | — | 554.8 MB | 12.5 MB |
| pi | 119.2 MB | 117.0 MB | 787.55× more | — | — | 148.7 MB | 148.2 MB |
| oh-my-pi (omp) | 394.8 MB | 199.6 MB | 1,344× more | — | — | 418.2 MB | 312.2 MB |
| oh-my-openagent (omo) | 57.0 MB | 45.2 MB | 304.52× more | — | — | 802.7 MB | 334.7 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.3 MB | 35.6 MB | 37.6 MB |
| jcode | 40.1 MB | 41.5 MB | 42.9 MB |
| Claude Code | 191.0 MB | 931.3 MB | 1.8 GB |
| Codex CLI | 106.8 MB | 535.8 MB | 1.0 GB |
| OpenCode | 554.8 MB | 586.8 MB | 665.9 MB |
| pi | 148.7 MB | 740.1 MB | 1.4 GB |
| oh-my-pi (omp) | 418.2 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 802.7 MB | 3.0 GB | 3.8 GB |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    12.10 ms
  Max:    17.74 ms
  Mean:   14.58 ms
  Median: 14.03 ms
  Stdev:  2.01 ms

--version:
  Min:    10.53 ms
  Max:    17.89 ms
  Mean:   14.12 ms
  Median: 13.29 ms
  Stdev:  2.38 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    35.00 ms
  Max:    94.70 ms
  Mean:   63.83 ms
  Median: 53.76 ms
  Stdev:  27.40 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    31.82 ms
  Max:    105.53 ms
  Mean:   70.86 ms
  Median: 73.77 ms
  Stdev:  22.18 ms

Bridge stage: hello_ok:
  Min:    33.12 ms
  Max:    112.73 ms
  Mean:   73.05 ms
  Median: 75.32 ms
  Stdev:  23.29 ms

Bridge stage: no-op reply:
  Min:    33.87 ms
  Max:    113.80 ms
  Mean:   73.84 ms
  Median: 76.02 ms
  Stdev:  23.29 ms

Bridge total (socket -> no-op):
  Min:    33.87 ms
  Max:    113.80 ms
  Mean:   73.84 ms
  Median: 76.02 ms
  Stdev:  23.29 ms

============================================================
Summary:
  Binary load median:      ~14.0 ms
  Server ready median:     ~53.8 ms
  Bridge total median:     ~76.0 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (a4fb3e02676f)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-d85g8l00
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.030/0.030    p95    0.180/0.066    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.235/0.308    p95    0.731/0.662    delta  +0.073 ms  control-no-change
  mcp_config_load   p50    0.303/0.446    p95    0.582/0.958    delta  +0.142 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.002    delta  -0.000 ms  control-no-change
  session_save      p50   27.089/28.222   p95   43.233/39.354   delta  +1.133 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22958 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22958 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    8.259/5.774    p95   15.971/9.958    delta  -2.485 ms  control-no-change
  skill_request_overlay p50    9.446/7.222    p95   11.116/9.368    delta  -2.224 ms  control-no-change
  mcp_config_load   p50    0.326/0.319    p95    0.625/0.834    delta  -0.007 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   28.918/27.520   p95   46.230/50.886   delta  -1.398 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22926..22958 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22958 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.027    p95    0.066/0.076    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.230/0.278    p95    0.613/0.716    delta  +0.048 ms  control-drift
  mcp_config_load   p50    0.267/0.301    p95    0.651/1.011    delta  +0.034 ms  control-no-change
  attach_history    p50  426.900/442.263  p95  492.035/575.916  delta +15.363 ms  control-no-change
  resume_ready      p50   94.459/106.522  p95  120.571/113.212  delta +12.062 ms  control-no-change
  provider_dispatch p50    1.125/1.365    p95    2.629/2.491    delta  +0.239 ms  control-no-change
  session_save      p50   25.959/38.651   p95   32.445/50.970   delta +12.692 ms  control-no-change
  total store_read_bytes p50 5311305  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22942..22964 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22968 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.234/4.597    p95    7.087/6.559    delta  +0.363 ms  control-no-change
  skill_request_overlay p50    5.624/6.028    p95    6.162/11.079   delta  +0.404 ms  control-no-change
  mcp_config_load   p50    0.271/0.311    p95    0.336/0.359    delta  +0.040 ms  control-no-change
  attach_history    p50  314.419/333.757  p95  403.051/440.497  delta +19.338 ms  control-no-change
  resume_ready      p50   72.071/65.606   p95  107.649/99.116   delta  -6.465 ms  control-no-change
  provider_dispatch p50    1.118/1.128    p95    1.363/1.930    delta  +0.010 ms  control-no-change
  session_save      p50   21.918/18.242   p95  113.898/30.142   delta  -3.676 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22948..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22964 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.025    p95    0.076/0.047    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.230/0.230    p95    0.872/0.439    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.273/0.259    p95    0.695/0.407    delta  -0.014 ms  control-no-change
  attach_history    p50 3672.129/3571.553 p95 4188.905/4023.877 delta -100.576 ms  control-no-change
  resume_ready      p50  692.850/700.269  p95  822.922/945.974  delta  +7.418 ms  control-no-change
  provider_dispatch p50    9.991/10.240   p95   17.252/14.875   delta  +0.249 ms  control-no-change
  session_save      p50   17.694/21.758   p95   31.913/44.338   delta  +4.063 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22951..22971 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22971 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.417/3.975    p95    5.712/6.750    delta  -0.442 ms  control-no-change
  skill_request_overlay p50    5.605/5.105    p95    6.232/8.471    delta  -0.499 ms  control-no-change
  mcp_config_load   p50    0.286/0.258    p95    0.370/0.578    delta  -0.028 ms  control-no-change
  attach_history    p50 3443.591/3124.801 p95 4255.540/4629.306 delta -318.790 ms  control-no-change
  resume_ready      p50  756.043/657.840  p95  976.180/753.499  delta -98.203 ms  control-no-change
  provider_dispatch p50   10.745/10.280   p95   22.135/12.137   delta  -0.465 ms  control-no-change
  session_save      p50   23.254/24.944   p95   45.133/26.920   delta  +1.690 ms  control-no-change
  total store_read_bytes p50 53417513  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184298..42184305 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210249..11210255 (5 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22973 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22971 (8 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (a4fb3e02676f)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-y4ivzl3b
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.029/0.025    p95    0.049/0.040    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.245/0.228    p95    0.283/0.291    delta  -0.016 ms  control-no-change
  mcp_config_load   p50    0.260/0.254    p95    0.321/0.351    delta  -0.006 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   11.109/11.208   p95   12.091/11.682   delta  +0.099 ms  control-no-change
  total store_read_bytes p50 23665  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22943, 22947, 23657, 23665, 24359, 24379, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22923, 22947, 22955, 23647, 23669, 24375, 24379, 24381]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22943..24383 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22923..24381 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    3.601/3.882    p95    6.741/6.002    delta  +0.281 ms  control-no-change
  skill_request_overlay p50    4.659/4.899    p95   12.554/6.899    delta  +0.240 ms  control-no-change
  mcp_config_load   p50    0.254/0.239    p95    0.517/0.275    delta  -0.015 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   11.463/11.099   p95   19.069/16.073   delta  -0.365 ms  control-no-change
  total store_read_bytes p50 23658  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22947, 22949, 22951, 23655, 23661, 23667, 24365, 24371, 24373]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22949, 22951, 22959, 23649, 23663, 23669, 24355, 24381, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24373 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22949..24383 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.020/0.020    p95    0.023/0.028    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.183/0.185    p95    0.197/0.196    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.212/0.211    p95    0.227/0.236    delta  -0.001 ms  control-no-change
  attach_history    p50   77.677/78.011   p95   82.263/80.169   delta  +0.334 ms  control-no-change
  resume_ready      p50   46.981/46.924   p95   47.454/48.944   delta  -0.057 ms  control-no-change
  provider_dispatch p50    0.859/0.848    p95    0.920/0.914    delta  -0.012 ms  control-no-change
  session_save      p50    9.874/9.531    p95   11.800/9.813    delta  -0.343 ms  control-no-change
  total store_read_bytes p50 2841242  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23674, 23678, 24370, 24372, 25094, 25098, 25806, 25808, 26518, 26528]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23662, 23672, 24362, 24388, 25096, 25102, 25814, 25820, 26520, 26528]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23674..26528 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23662..26528 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.107/3.168    p95    3.604/3.771    delta  +0.062 ms  control-no-change
  skill_request_overlay p50    4.066/4.052    p95    4.389/4.451    delta  -0.015 ms  control-no-change
  mcp_config_load   p50    0.213/0.212    p95    0.248/0.221    delta  -0.000 ms  control-no-change
  attach_history    p50   77.695/77.507   p95   84.878/80.830   delta  -0.187 ms  control-no-change
  resume_ready      p50   47.184/47.144   p95   52.143/51.393   delta  -0.040 ms  control-no-change
  provider_dispatch p50    0.847/0.850    p95    0.945/0.978    delta  +0.003 ms  control-no-change
  session_save      p50   10.905/10.822   p95   12.200/11.673   delta  -0.083 ms  control-no-change
  total store_read_bytes p50 2841256  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23666, 23674, 24380, 25098, 25804, 25808, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23660, 23678, 24382, 24388, 25090, 25100, 25802, 25812, 26520, 26534]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23666..26530 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23660..26534 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.025/0.024    p95    0.029/0.035    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.239/0.210    p95    0.254/0.249    delta  -0.029 ms  control-no-change
  mcp_config_load   p50    0.239/0.243    p95    0.299/0.272    delta  +0.004 ms  control-no-change
  attach_history    p50  914.275/934.292  p95 1024.560/983.557  delta +20.017 ms  control-no-change
  resume_ready      p50  553.032/539.024  p95  608.000/610.382  delta -14.008 ms  control-no-change
  provider_dispatch p50    9.196/9.426    p95   10.562/10.760   delta  +0.231 ms  control-no-change
  session_save      p50   11.730/14.214   p95   16.979/64.571   delta  +2.484 ms  control-no-change
  total store_read_bytes p50 28640281  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23660, 23668, 24382, 24390, 25100, 25116, 25810, 25820, 26524, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23662, 23684, 24364, 24386, 25096, 25102, 25808, 25822, 26522]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23660..26530 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23662..26522 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.347/3.386    p95    3.873/4.020    delta  +0.038 ms  control-no-change
  skill_request_overlay p50    4.286/4.286    p95    4.774/4.807    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.219/0.215    p95    0.263/0.237    delta  -0.003 ms  control-no-change
  attach_history    p50  859.983/834.441  p95  938.027/908.860  delta -25.541 ms  control-no-change
  resume_ready      p50  536.892/520.702  p95  593.211/564.401  delta -16.190 ms  control-no-change
  provider_dispatch p50    8.562/8.543    p95   14.255/9.735    delta  -0.019 ms  control-no-change
  session_save      p50   11.239/11.207   p95   22.873/14.444   delta  -0.032 ms  control-no-change
  total store_read_bytes p50 28640270  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23681, 24371, 24375, 25093, 25097, 25807, 25819, 26517, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23677, 23681, 24383, 24387, 25101, 25105, 25815, 25819, 26529, 26537]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23677..26537 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.4 ms | 0.7 ms | 0.3 ms | 0.7 ms | 5 |
| read (1000 lines) | 0.7 ms | 0.8 ms | 0.5 ms | 0.8 ms | 5 |
| read (10 lines @ offset) | 0.6 ms | 0.8 ms | 0.5 ms | 0.8 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.7 ms | 0.3 ms | 0.7 ms | 5 |
| write (small) | 1.8 ms | 2.4 ms | 1.6 ms | 2.4 ms | 5 |
| write (10KB) | 2.0 ms | 4.0 ms | 1.8 ms | 4.0 ms | 5 |
| edit (simple replace) | 1.7 ms | 2.0 ms | 1.6 ms | 2.0 ms | 5 |
| ls (scripts dir) | 0.9 ms | 2.0 ms | 0.8 ms | 2.0 ms | 5 |
| agentgrep (grep in tree) | 2.4 ms | 2.6 ms | 2.2 ms | 2.6 ms | 5 |
| agentgrep (find **/*.rs) | 1.5 ms | 1.9 ms | 1.4 ms | 1.9 ms | 5 |
| bash (true) | 4.5 ms | 5.8 ms | 3.9 ms | 5.8 ms | 5 |
| bash (echo hello) | 4.4 ms | 5.5 ms | 4.1 ms | 5.5 ms | 5 |
| bash (ls fixture tree) | 10.6 ms | 12.3 ms | 9.4 ms | 12.3 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 115.7 ms | 115.7 ms | 115.7 ms | 115.9 ms | 0 |
| 10 | 393.4 ms | 434.2 ms | 434.2 ms | 434.8 ms | 0 |
| 40 | 1.63 s | 1.64 s | 1.64 s | 1.64 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.56 s | 1.56 s | 1.56 s | 1 |
| leaf_touch | src/main.rs | 809.2 ms | 809.2 ms | 809.2 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 40.01 s | 40.01 s | 40.01 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 50.29 s | 50.29 s | 50.29 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (c4816f4) | 10.7 ms | 93.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 13.9 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 37.4 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 93.7 ms | 479.4 MB |
| OpenCode | 1.19.5 | 451.1 ms | 605.3 MB |
| pi | 1.5.0 | 238.3 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 574.6 ms | 291.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 217.9 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 184.0 ms | 171.8 ms–265.8 ms | baseline | 35.7 MB | baseline | yes | — |
| jcode | 126.1 ms | 115.0 ms–199.1 ms | 1.46× less | 32.8 MB | 1.09× less | yes | — |
| Claude Code | 1.14 s | 1.14 s–1.84 s | 6.20× more | 277.4 MB | 7.77× more | yes | — |
| Codex CLI | 1.45 s | 995.6 ms–5.21 s | 7.86× more | 266.2 MB | 7.45× more | yes | — |
| OpenCode | 4.39 s | 4.22 s–6.94 s | 23.87× more | 852.8 MB | 23.88× more | yes | — |
| pi | 409.3 ms | 404.6 ms–451.1 ms | 2.22× more | 122.6 MB | 3.43× more | yes | — |
| oh-my-pi (omp) | 2.56 s | 2.44 s–2.62 s | 13.92× more | 397.9 MB | 11.14× more | yes | — |
| oh-my-openagent (omo) | 19.46 s | 15.61 s–36.94 s | 105.78× more | 559.5 MB | 15.67× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 218.2 ms | 196.1 ms–292.1 ms | baseline | 24.7 MB | baseline | skill-uri | yes | — |
| jcode | 196.2 ms | 179.0 ms–223.0 ms | 1.11× less | 23.8 MB | 1.04× less | skill-tool | yes | — |
| Claude Code | 806.1 ms | 791.3 ms–885.8 ms | 3.69× more | 236.7 MB | 9.58× more | skill-tool | yes | — |
| Codex CLI | 1.71 s | 1.58 s–2.25 s | 7.84× more | 264.8 MB | 10.72× more | file-read | yes | — |
| OpenCode | 6.14 s | 5.99 s–6.24 s | 28.14× more | 746.4 MB | 30.20× more | skill-tool | yes | — |
| pi | 590.2 ms | 537.9 ms–638.8 ms | 2.71× more | 97.7 MB | 3.95× more | file-read | yes | — |
| oh-my-pi (omp) | 4.00 s | 2.81 s–4.26 s | 18.34× more | 395.4 MB | 16.00× more | file-read | yes | — |
| oh-my-openagent (omo) | 17.78 s | 15.25 s–20.58 s | 81.50× more | 538.1 MB | 21.77× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 271.2 ms | 261.8 ms–285.8 ms | baseline | 37.1 MB | baseline | mcp-call | yes | 167.1 ms | — |
| jcode | 312.7 ms | 298.9 ms–486.7 ms | 1.15× more | 36.3 MB | 1.02× less | mcp-action | yes | 219.7 ms | — |
| Claude Code | 974.1 ms | 942.0 ms–1.28 s | 3.59× more | 281.1 MB | 7.57× more | mcp-tool | yes | 837.7 ms | — |
| Codex CLI | 769.8 ms | 697.5 ms–2.43 s | 2.84× more | 178.1 MB | 4.80× more | mcp-tool | yes | 599.1 ms | — |
| OpenCode | 5.96 s | 5.84 s–6.52 s | 21.96× more | 769.1 MB | 20.73× more | mcp-tool | yes | 4.70 s | — |
| pi | 605.2 ms | 592.5 ms–675.1 ms | 2.23× more | 117.5 MB | 3.17× more | mcp-tool | yes | 492.2 ms | — |
| oh-my-pi (omp) | 3.50 s | 3.47 s–4.12 s | 12.89× more | 411.5 MB | 11.09× more | mcp-route | yes | 2.95 s | — |
| oh-my-openagent (omo) | 20.55 s | 19.28 s–28.64 s | 75.78× more | 565.8 MB | 15.25× more | mcp-tool | yes | 18.23 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.5 MB | 138.1 KB | baseline | — | — | 29.4 MB | 334.9 KB |
| jcode | 21.9 MB | 108.2 KB | 1.28× less | — | — | 27.2 MB | 390.7 KB |
| Claude Code | 153.8 MB | 169.2 MB | 1,254× more | — | — | 187.2 MB | 190.0 MB |
| Codex CLI | 125.4 MB | 97.7 MB | 724.77× more | — | — | 170.7 MB | 116.5 MB |
| OpenCode | 246.6 MB | 15.0 MB | 111.01× more | — | — | 427.5 MB | 20.5 MB |
| pi | 90.5 MB | 90.9 MB | 674.39× more | — | — | 101.5 MB | 100.9 MB |
| oh-my-pi (omp) | 335.6 MB | 138.0 MB | 1,023× more | — | — | 336.6 MB | 336.1 MB |
| oh-my-openagent (omo) | 149.3 MB | 130.5 MB | 967.65× more | — | — | 412.4 MB | 396.8 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.4 MB | 31.9 MB | 32.4 MB |
| jcode | 27.2 MB | 28.3 MB | 30.6 MB |
| Claude Code | 187.2 MB | 947.0 MB | 1.9 GB |
| Codex CLI | 170.7 MB | 656.5 MB | 1.2 GB |
| OpenCode | 427.5 MB | 487.8 MB | 610.5 MB |
| pi | 101.5 MB | 505.3 MB | 1009.7 MB |
| oh-my-pi (omp) | 336.6 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 412.4 MB | 2.1 GB | 3.9 GB |

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    14.80 ms
  Max:    16.54 ms
  Mean:   15.61 ms
  Median: 15.60 ms
  Stdev:  0.56 ms

--version:
  Min:    15.23 ms
  Max:    15.94 ms
  Mean:   15.51 ms
  Median: 15.51 ms
  Stdev:  0.20 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.41 ms
  Max:    33.27 ms
  Mean:   24.32 ms
  Median: 22.83 ms
  Stdev:  3.52 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~15.6 ms
  Server ready median:     ~22.8 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (4a72b41e2577)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-lszk6wys
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.067/0.067    p95    0.077/0.085    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.507/0.499    p95    0.530/0.543    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.643/0.644    p95    0.685/0.701    delta  +0.002 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   34.379/33.918   p95   43.387/39.045   delta  -0.461 ms  control-no-change
  total store_read_bytes p50 22938  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22916..22960 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22910..22964 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   10.220/10.443   p95   10.768/12.011   delta  +0.223 ms  control-no-change
  skill_request_overlay p50   28.234/28.154   p95   37.937/29.707   delta  -0.080 ms  control-no-change
  mcp_config_load   p50    0.678/0.707    p95    0.785/0.797    delta  +0.029 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   49.178/58.380   p95   61.563/73.100   delta  +9.203 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22916..22968 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22960 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.067/0.066    p95    0.079/0.085    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.534/0.508    p95    0.593/0.552    delta  -0.026 ms  control-no-change
  mcp_config_load   p50    0.666/0.651    p95    0.790/0.699    delta  -0.015 ms  control-no-change
  attach_history    p50  290.820/292.626  p95  373.524/310.934  delta  +1.806 ms  control-no-change
  resume_ready      p50   99.194/96.980   p95  106.547/103.483  delta  -2.214 ms  control-no-change
  provider_dispatch p50    1.073/1.072    p95    1.146/1.177    delta  -0.001 ms  control-no-change
  session_save      p50   34.452/30.414   p95   70.774/95.667   delta  -4.039 ms  control-no-change
  total store_read_bytes p50 5311210  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22931..22968 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22971 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   10.092/10.199   p95   10.562/10.573   delta  +0.107 ms  control-no-change
  skill_request_overlay p50   26.961/27.320   p95   28.749/30.230   delta  +0.359 ms  control-no-change
  mcp_config_load   p50    0.663/0.700    p95    0.712/0.897    delta  +0.037 ms  control-no-change
  attach_history    p50  297.411/295.088  p95  384.997/348.893  delta  -2.323 ms  control-no-change
  resume_ready      p50   96.922/98.081   p95  108.243/110.569  delta  +1.159 ms  control-no-change
  provider_dispatch p50    1.096/1.067    p95    1.199/1.115    delta  -0.030 ms  control-no-change
  session_save      p50   42.768/43.016   p95  306.740/52.669   delta  +0.247 ms  control-no-change
  total store_read_bytes p50 5311221  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189147..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099098..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22891..22980 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22968 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.069/0.069    p95    0.080/0.089    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.566/0.531    p95    0.650/0.556    delta  -0.035 ms  control-no-change
  mcp_config_load   p50    0.718/0.676    p95    0.741/0.738    delta  -0.042 ms  control-no-change
  attach_history    p50 3547.536/3522.837 p95 3769.186/3791.747 delta -24.699 ms  control-no-change
  resume_ready      p50 1048.851/1063.513 p95 1094.747/1098.757 delta +14.661 ms  control-no-change
  provider_dispatch p50   13.804/14.267   p95   25.782/15.139   delta  +0.464 ms  control-no-change
  session_save      p50   34.344/34.707   p95   38.304/45.260   delta  +0.363 ms  control-no-change
  total store_read_bytes p50 53417424  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22979 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22946..22975 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   10.427/10.490   p95   10.817/10.838   delta  +0.062 ms  control-no-change
  skill_request_overlay p50   28.926/29.373   p95   31.046/30.946   delta  +0.447 ms  control-no-change
  mcp_config_load   p50    0.724/0.724    p95    0.777/2.680    delta  -0.000 ms  control-no-change
  attach_history    p50 3543.094/3532.243 p95 3624.059/3621.893 delta -10.851 ms  control-no-change
  resume_ready      p50 1049.543/1044.002 p95 1076.622/1079.552 delta  -5.540 ms  control-no-change
  provider_dispatch p50   13.445/13.893   p95   14.813/14.706   delta  +0.448 ms  control-no-change
  session_save      p50   33.514/31.895   p95   54.376/38.446   delta  -1.619 ms  control-no-change
  total store_read_bytes p50 53417426  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22967 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184246..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210209..11210216 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22918..22971 (8 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (4a72b41e2577)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-t83zr_4g
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.069/0.068    p95    0.097/0.090    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.529/0.527    p95    0.563/0.635    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.619/0.641    p95    0.693/0.861    delta  +0.022 ms  control-no-change
  attach_history    p50    0.001/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   30.248/32.358   p95   34.693/46.060   delta  +2.110 ms  control-no-change
  total store_read_bytes p50 23628  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22911, 22933, 22947, 22956, 23618, 23638, 23660, 24290, 24325, 24339]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22934, 22935, 22953, 23630, 23644, 23645, 24325, 24334, 24336, 24345]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22911..24339 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22934..24345 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   10.566/10.971   p95   11.436/11.832   delta  +0.405 ms  control-no-change
  skill_request_overlay p50   27.035/27.923   p95   29.561/29.051   delta  +0.887 ms  control-no-change
  mcp_config_load   p50    0.707/0.733    p95    0.832/0.889    delta  +0.025 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.004/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   38.588/38.562   p95   54.848/50.638   delta  -0.026 ms  control-no-change
  total store_read_bytes p50 23636  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22913, 22926, 22946, 22949, 23635, 23637, 23653, 24318, 24344, 24352]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22909, 22923, 22931, 22949, 23594, 23640, 23642, 24329, 24355]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22913..24352 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22909..24355 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.068/0.075    p95    0.078/0.126    delta  +0.007 ms  control-drift
  skill_request_overlay p50    0.523/0.589    p95    0.553/0.697    delta  +0.067 ms  control-drift
  mcp_config_load   p50    0.678/0.701    p95    0.847/0.849    delta  +0.023 ms  control-no-change
  attach_history    p50  151.477/156.302  p95  159.313/171.470  delta  +4.825 ms  control-no-change
  resume_ready      p50  113.907/111.970  p95  122.618/120.870  delta  -1.937 ms  control-no-change
  provider_dispatch p50    1.409/1.391    p95    1.537/1.520    delta  -0.018 ms  control-no-change
  session_save      p50   32.021/31.156   p95   35.491/32.716   delta  -0.864 ms  control-no-change
  total store_read_bytes p50 2841132  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23645, 23655, 24340, 24350, 25047, 25057, 25734, 25744, 26425, 26463]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23637, 23675, 24348, 24366, 25031, 25063, 25742, 26457, 26465]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23645..26463 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23637..26465 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   10.753/10.632   p95   11.000/11.446   delta  -0.121 ms  control-no-change
  skill_request_overlay p50   27.375/27.522   p95   28.166/28.250   delta  +0.146 ms  control-no-change
  mcp_config_load   p50    0.696/0.754    p95    0.762/0.866    delta  +0.058 ms  control-no-change
  attach_history    p50  147.376/147.334  p95  154.373/152.532  delta  -0.042 ms  control-no-change
  resume_ready      p50  110.235/106.662  p95  116.071/110.702  delta  -3.572 ms  control-no-change
  provider_dispatch p50    1.360/1.351    p95    1.629/1.474    delta  -0.009 ms  control-no-change
  session_save      p50   38.001/39.814   p95   54.039/132.392  delta  +1.812 ms  control-no-change
  total store_read_bytes p50 2841111  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23663, 23675, 24330, 24346, 25049, 25746, 25764, 26423, 26449]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23634, 23679, 24334, 24346, 25039, 25065, 25752, 25776, 26459, 26471]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23663..26449 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23634..26471 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.066/0.066    p95    0.070/0.080    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.556/0.540    p95    0.600/0.591    delta  -0.017 ms  control-no-change
  mcp_config_load   p50    0.673/0.659    p95    0.751/0.684    delta  -0.014 ms  control-no-change
  attach_history    p50 1574.776/1578.216 p95 1615.811/1662.100 delta  +3.441 ms  control-no-change
  resume_ready      p50 1117.664/1128.107 p95 1171.363/1193.757 delta +10.443 ms  control-no-change
  provider_dispatch p50   14.172/14.160   p95   14.447/14.462   delta  -0.012 ms  control-no-change
  session_save      p50   31.753/31.683   p95   42.837/39.846   delta  -0.070 ms  control-no-change
  total store_read_bytes p50 28640150  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23638, 23654, 24357, 24365, 25031, 25076, 25768, 25775, 26447, 26470]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23640, 23676, 24347, 25023, 25060, 25715, 25766, 26442, 26453]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23638..26470 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23640..26453 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   10.447/10.269   p95   11.353/10.756   delta  -0.178 ms  control-no-change
  skill_request_overlay p50   26.993/27.014   p95   32.872/28.877   delta  +0.022 ms  control-no-change
  mcp_config_load   p50    0.733/0.715    p95    0.871/0.763    delta  -0.018 ms  control-no-change
  attach_history    p50 1556.900/1569.492 p95 1625.377/1608.596 delta +12.591 ms  control-no-change
  resume_ready      p50 1106.969/1112.658 p95 1129.908/1143.082 delta  +5.689 ms  control-no-change
  provider_dispatch p50   13.899/13.809   p95   14.493/14.083   delta  -0.091 ms  control-no-change
  session_save      p50   30.914/32.924   p95   33.379/44.431   delta  +2.010 ms  control-no-change
  total store_read_bytes p50 28640130  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23624, 23661, 24344, 24345, 25025, 25060, 25746, 25760, 26459, 26467]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23661, 24324, 24372, 25050, 25057, 25740, 25749, 26456, 26463]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23624..26467 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23661..26463 (9 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 7.9 ms | 10.0 ms | 7.9 ms | 10.0 ms | 5 |
| write (10KB) | 7.6 ms | 8.0 ms | 7.5 ms | 8.0 ms | 5 |
| edit (simple replace) | 7.4 ms | 9.5 ms | 7.4 ms | 9.5 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| agentgrep (grep in tree) | 7.4 ms | 7.5 ms | 5.3 ms | 7.5 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 5.3 ms | 4.8 ms | 5.3 ms | 5 |
| bash (true) | 10.5 ms | 10.5 ms | 10.4 ms | 10.5 ms | 5 |
| bash (echo hello) | 10.0 ms | 10.5 ms | 9.6 ms | 10.5 ms | 5 |
| bash (ls fixture tree) | 15.2 ms | 15.6 ms | 13.1 ms | 15.6 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 1.06 s | 1.06 s | 1.06 s | 1.06 s | 0 |
| 10 | 969.8 ms | 971.5 ms | 971.5 ms | 972.6 ms | 0 |
| 40 | 2.83 s | 2.85 s | 2.85 s | 2.86 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 646.7 ms | 646.7 ms | 646.7 ms | 1 |
| leaf_touch | src\main.rs | 890.1 ms | 890.1 ms | 890.1 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 30.88 s | 30.88 s | 30.88 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 46.13 s | 46.13 s | 46.13 s | 1 |

<!-- bench-auto:end -->
