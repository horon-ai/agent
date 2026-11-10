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

<!-- bench-auto:start commit=5b83dc36c46688abc5dbadc0f02ca99520488a11 -->

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `5b83dc36c46688abc5dbadc0f02ca99520488a11`
- Measured at: 2026-11-10 00:39 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/42723443025> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (5b83dc3)` | Success |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (5b83dc3)` | Success |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (5b83dc3)` | Success |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (5b83dc3) | 3.4 ms | 110.7 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 7.8 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 6.8 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 32.9 ms | 435.0 MB |
| OpenCode | 1.19.5 | 386.7 ms | 613.4 MB |
| pi | 1.5.0 | 184.3 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 589.8 ms | 412.6 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 94.6 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 66.4 ms | 52.0 ms–302.5 ms | baseline | 39.1 MB | baseline | yes | 14 | — |
| jcode | 139.4 ms | 139.3 ms–143.3 ms | 2.10× more | 42.2 MB | 1.08× more | yes | 5 | — |
| Claude Code | 384.7 ms | 373.9 ms–394.4 ms | 5.79× more | 206.3 MB | 5.27× more | yes | — | — |
| Codex CLI | 1.29 s | 624.0 ms–1.57 s | 19.47× more | 202.3 MB | 5.17× more | yes | 6 | — |
| OpenCode | 2.18 s | 2.17 s–2.45 s | 32.89× more | 596.4 MB | 15.25× more | yes | 4 | — |
| pi | 304.0 ms | 301.7 ms–311.4 ms | 4.58× more | 114.1 MB | 2.92× more | yes | — | — |
| oh-my-pi (omp) | 1.50 s | 1.47 s–1.67 s | 22.64× more | 356.4 MB | 9.11× more | yes | 67 | — |
| oh-my-openagent (omo) | 5.12 s | 4.91 s–5.49 s | 77.11× more | 665.9 MB | 17.02× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 48.0 ms | 47.1 ms–54.6 ms | baseline | 34.3 MB | baseline | skill-uri | yes | — |
| jcode | 50.0 ms | 49.0 ms–50.6 ms | 1.04× more | 40.7 MB | 1.19× more | skill-tool | yes | — |
| Claude Code | 359.4 ms | 356.2 ms–370.8 ms | 7.48× more | 203.8 MB | 5.94× more | skill-tool | yes | — |
| Codex CLI | 1.10 s | 886.2 ms–1.64 s | 22.85× more | 202.0 MB | 5.88× more | file-read | yes | — |
| OpenCode | 2.65 s | 2.64 s–2.70 s | 55.17× more | 596.8 MB | 17.38× more | skill-tool | yes | — |
| pi | 412.2 ms | 411.6 ms–412.8 ms | 8.59× more | 125.7 MB | 3.66× more | file-read | yes | — |
| oh-my-pi (omp) | 1.49 s | 1.47 s–1.51 s | 31.07× more | 339.3 MB | 9.88× more | file-read | yes | — |
| oh-my-openagent (omo) | 5.64 s | 5.29 s–5.87 s | 117.38× more | 659.7 MB | 19.21× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 65.4 ms | 63.1 ms–65.4 ms | baseline | 43.7 MB | baseline | mcp-call | yes | 50.0 ms | — |
| jcode | 166.0 ms | 165.4 ms–166.8 ms | 2.54× more | 53.9 MB | 1.23× more | mcp-action | yes | 140.8 ms | — |
| Claude Code | 450.5 ms | 449.8 ms–457.0 ms | 6.89× more | 225.2 MB | 5.15× more | mcp-tool | yes | 325.7 ms | — |
| Codex CLI | 804.1 ms | 704.9 ms–910.1 ms | 12.30× more | 200.4 MB | 4.59× more | mcp-tool | yes | 284.9 ms | — |
| OpenCode | 2.39 s | 2.38 s–2.41 s | 36.53× more | 573.1 MB | 13.11× more | mcp-tool | yes | 2.20 s | — |
| pi | 449.8 ms | 441.5 ms–455.9 ms | 6.88× more | 139.5 MB | 3.19× more | mcp-tool | yes | 397.4 ms | — |
| oh-my-pi (omp) | 1.51 s | 1.46 s–1.51 s | 23.04× more | 353.2 MB | 8.08× more | mcp-route | yes | 1.41 s | — |
| oh-my-openagent (omo) | 5.63 s | 5.48 s–5.65 s | 86.05× more | 662.9 MB | 15.17× more | mcp-tool | yes | 2.81 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 36.5 MB | 138.1 KB | baseline | 0.44 | 1.00 | 43.7 MB | 417.9 KB |
| jcode | 43.2 MB | 50.8 KB | 2.72× less | 0.11 | — | 46.6 MB | 205.5 KB |
| Claude Code | 151.8 MB | 149.5 MB | 1,108× more | — | — | 187.3 MB | 180.7 MB |
| Codex CLI | 128.5 MB | 133.3 MB | 988.16× more | — | — | 162.3 MB | 164.9 MB |
| OpenCode | 243.0 MB | 543.0 KB | 3.93× more | — | — | 448.0 MB | 22.0 MB |
| pi | 106.8 MB | 108.6 MB | 805.44× more | — | — | 129.8 MB | 124.5 MB |
| oh-my-pi (omp) | 334.4 MB | 84.0 MB | 622.56× more | — | — | 356.6 MB | 357.1 MB |
| oh-my-openagent (omo) | 292.8 MB | 35.1 MB | 260.30× more | — | — | 585.4 MB | 606.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 43.7 MB | 46.3 MB | 47.5 MB |
| jcode | 46.6 MB | 47.6 MB | 48.4 MB |
| Claude Code | 187.3 MB | 922.1 MB | 1.8 GB |
| Codex CLI | 162.3 MB | 812.4 MB | 1.6 GB |
| OpenCode | 448.0 MB | 460.8 MB | 641.0 MB |
| pi | 129.8 MB | 636.9 MB | 1.2 GB |
| oh-my-pi (omp) | 356.6 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 585.4 MB | 3.0 GB | 5.9 GB |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.19 ms
  Max:    3.45 ms
  Mean:   3.29 ms
  Median: 3.27 ms
  Stdev:  0.09 ms

--version:
  Min:    3.01 ms
  Max:    3.13 ms
  Mean:   3.08 ms
  Median: 3.08 ms
  Stdev:  0.05 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.46 ms
  Max:    20.58 ms
  Mean:   20.50 ms
  Median: 20.48 ms
  Stdev:  0.04 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.47 ms
  Max:    20.67 ms
  Mean:   20.62 ms
  Median: 20.64 ms
  Stdev:  0.06 ms

Bridge stage: hello_ok:
  Min:    21.01 ms
  Max:    21.31 ms
  Mean:   21.18 ms
  Median: 21.17 ms
  Stdev:  0.08 ms

Bridge stage: no-op reply:
  Min:    21.36 ms
  Max:    21.66 ms
  Mean:   21.54 ms
  Median: 21.53 ms
  Stdev:  0.08 ms

Bridge total (socket -> no-op):
  Min:    21.36 ms
  Max:    21.66 ms
  Mean:   21.54 ms
  Median: 21.53 ms
  Stdev:  0.08 ms

============================================================
Summary:
  Binary load median:      ~3.3 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.5 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (53e241d3845a)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-m3x9sjsh
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.015/0.027    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.106    p95    0.119/0.121    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.181/0.182    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.940/7.829    p95    9.416/8.076    delta  -0.111 ms  control-no-change
  total store_read_bytes p50 23074  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23052..23088 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23056..23084 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.680/2.682    p95    2.763/2.719    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    4.643/4.654    p95    4.940/4.720    delta  +0.010 ms  control-no-change
  mcp_config_load   p50    0.150/0.148    p95    0.170/0.160    delta  -0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   10.448/10.219   p95   15.883/14.346   delta  -0.229 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23068..23080 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23084 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.015    p95    0.015/0.016    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.105    p95    0.135/0.121    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.175/0.178    delta  -0.000 ms  control-no-change
  attach_history    p50  292.287/290.663  p95  297.372/294.677  delta  -1.624 ms  control-no-change
  resume_ready      p50   52.916/53.140   p95   53.611/53.801   delta  +0.224 ms  control-no-change
  provider_dispatch p50    1.333/1.330    p95    1.352/1.348    delta  -0.002 ms  control-no-change
  session_save      p50    7.475/7.383    p95    8.316/7.992    delta  -0.092 ms  control-no-change
  total store_read_bytes p50 5311392  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23094 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23061..23078 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.677/2.667    p95    2.700/2.764    delta  -0.010 ms  control-no-change
  skill_request_overlay p50    4.636/4.675    p95    4.685/4.751    delta  +0.040 ms  control-no-change
  mcp_config_load   p50    0.148/0.148    p95    0.161/0.155    delta  -0.000 ms  control-no-change
  attach_history    p50  294.034/294.008  p95  300.292/296.932  delta  -0.026 ms  control-no-change
  resume_ready      p50   53.426/53.231   p95   54.374/54.209   delta  -0.195 ms  control-no-change
  provider_dispatch p50    1.311/1.311    p95    1.367/1.337    delta  +0.000 ms  control-no-change
  session_save      p50    7.345/7.481    p95    7.608/7.535    delta  +0.136 ms  control-no-change
  total store_read_bytes p50 5311394  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189183..4189191 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099125..1099133 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23082 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23058..23085 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.015    p95    0.033/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.105    p95    0.107/0.106    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.144    p95    0.145/0.156    delta  +0.000 ms  control-no-change
  attach_history    p50 3759.549/3723.891 p95 3778.085/3755.222 delta -35.658 ms  control-no-change
  resume_ready      p50  577.433/576.007  p95  589.691/594.669  delta  -1.426 ms  control-no-change
  provider_dispatch p50   14.360/14.270   p95   15.074/14.646   delta  -0.090 ms  control-no-change
  session_save      p50    7.758/7.702    p95    8.112/8.228    delta  -0.056 ms  control-no-change
  total store_read_bytes p50 53417606  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184284..42184289 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210238..11210243 (4 distinct values)
  SPREAD   a session_save store_read_bytes 23067..23089 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184287..42184289 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210243 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23057..23085 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.694/2.686    p95    2.760/2.776    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    4.633/4.671    p95    4.701/4.721    delta  +0.037 ms  control-no-change
  mcp_config_load   p50    0.148/0.148    p95    0.149/0.150    delta  +0.001 ms  control-no-change
  attach_history    p50 3769.200/3773.231 p95 3848.018/3803.927 delta  +4.031 ms  control-no-change
  resume_ready      p50  579.601/577.243  p95  600.012/584.939  delta  -2.358 ms  control-no-change
  provider_dispatch p50   14.866/14.494   p95   16.757/14.977   delta  -0.372 ms  control-no-change
  session_save      p50    7.739/7.765    p95    8.011/8.638    delta  +0.026 ms  control-no-change
  total store_read_bytes p50 53417603  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23065..23085 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184287..42184289 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210243 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23089 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (53e241d3845a)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-b9qc7_j3
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.015/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.105    p95    0.138/0.135    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.143    p95    0.162/0.145    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    8.191/8.110    p95    9.899/10.782   delta  -0.081 ms  control-no-change
  total store_read_bytes p50 23797  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23061, 23073, 23081, 23779, 23795, 23799, 24505, 24509, 24513, 24517]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23073, 23081, 23783, 23795, 23799, 24501, 24505, 24509]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23061..24517 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24509 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.818/2.814    p95    2.874/2.850    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    4.820/4.781    p95    5.054/4.839    delta  -0.039 ms  control-no-change
  mcp_config_load   p50    0.149/0.149    p95    0.150/0.171    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    9.620/9.673    p95   10.364/10.608   delta  +0.053 ms  control-no-change
  total store_read_bytes p50 23799  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23061, 23069, 23081, 23775, 23795, 23803, 24497, 24505, 24509, 24525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23059, 23069, 23077, 23781, 23795, 23799, 24495, 24511, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23061..24525 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23059..24517 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.018/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.117/0.133    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.144    p95    0.159/0.145    delta  +0.001 ms  control-no-change
  attach_history    p50  100.180/99.663   p95  101.359/102.761  delta  -0.517 ms  control-no-change
  resume_ready      p50   54.058/53.749   p95   54.965/58.935   delta  -0.309 ms  control-no-change
  provider_dispatch p50    1.367/1.358    p95    1.417/1.387    delta  -0.009 ms  control-no-change
  session_save      p50    8.225/8.458    p95    8.327/13.971   delta  +0.234 ms  control-drift
  total store_read_bytes p50 2841365  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23784, 23800, 24494, 24514, 25220, 25242, 25942, 25944, 26668, 26670]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23764, 23786, 24506, 24516, 25230, 25238, 25936, 25940, 26652, 26666]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23784..26670 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23764..26666 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.767/2.759    p95    2.822/3.525    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    4.708/4.694    p95    4.769/4.902    delta  -0.014 ms  control-no-change
  mcp_config_load   p50    0.148/0.153    p95    0.156/0.169    delta  +0.005 ms  control-drift
  attach_history    p50  100.004/98.988   p95  101.924/101.176  delta  -1.015 ms  control-no-change
  resume_ready      p50   53.366/53.490   p95   54.207/54.424   delta  +0.124 ms  control-no-change
  provider_dispatch p50    1.319/1.336    p95    1.363/1.360    delta  +0.016 ms  control-no-change
  session_save      p50    9.606/9.621    p95   11.144/10.927   delta  +0.015 ms  control-no-change
  total store_read_bytes p50 2841358  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23776, 23788, 24498, 24510, 25228, 25236, 25950, 26660, 26672]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23790, 23800, 24516, 24518, 25220, 25224, 25950, 25958, 26648, 26672]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23776..26672 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23790..26672 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.015/0.017    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.123/0.121    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.144    p95    0.150/0.160    delta  +0.001 ms  control-no-change
  attach_history    p50 1122.773/1120.722 p95 1139.095/1131.803 delta  -2.051 ms  control-no-change
  resume_ready      p50  615.954/629.026  p95  638.204/653.133  delta +13.072 ms  control-no-change
  provider_dispatch p50   15.492/16.500   p95   17.903/17.592   delta  +1.008 ms  control-no-change
  session_save      p50    8.133/8.227    p95    9.847/69.590   delta  +0.094 ms  control-no-change
  total store_read_bytes p50 28640377  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23790, 24508, 24524, 25226, 25230, 25952, 25956, 26658, 26662]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23780, 23800, 24514, 25226, 25232, 25944, 25950, 26658, 26664]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23790..26662 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23780..26664 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.713/2.710    p95    2.814/2.786    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    4.671/4.648    p95    4.784/4.734    delta  -0.022 ms  control-no-change
  mcp_config_load   p50    0.148/0.149    p95    0.161/0.164    delta  +0.000 ms  control-no-change
  attach_history    p50 1100.027/1100.661 p95 1127.828/1134.934 delta  +0.635 ms  control-no-change
  resume_ready      p50  612.823/596.618  p95  638.358/636.591  delta -16.205 ms  control-no-change
  provider_dispatch p50   15.062/15.021   p95   15.886/16.064   delta  -0.041 ms  control-no-change
  session_save      p50    9.491/9.586    p95   10.091/10.684   delta  +0.095 ms  control-no-change
  total store_read_bytes p50 28640389  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23788, 23798, 24510, 24520, 25238, 25242, 25940, 25948, 26658, 26660]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23786, 23798, 24516, 24520, 25234, 25942, 25948, 26654, 26664]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23788..26660 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23786..26664 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.2 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (10 lines @ offset) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 1.5 ms | 4.8 ms | 1.4 ms | 4.8 ms | 5 |
| write (10KB) | 1.9 ms | 2.0 ms | 1.7 ms | 2.0 ms | 5 |
| edit (simple replace) | 1.6 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| ls (scripts dir) | 0.8 ms | 0.9 ms | 0.6 ms | 0.9 ms | 5 |
| agentgrep (grep in tree) | 1.6 ms | 2.1 ms | 1.5 ms | 2.1 ms | 5 |
| agentgrep (find **/*.rs) | 1.2 ms | 1.2 ms | 1.1 ms | 1.2 ms | 5 |
| bash (true) | 1.7 ms | 1.8 ms | 1.7 ms | 1.8 ms | 5 |
| bash (echo hello) | 1.7 ms | 1.8 ms | 1.6 ms | 1.8 ms | 5 |
| bash (ls fixture tree) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 56.4 ms | 56.4 ms | 56.4 ms | 56.6 ms | 0 |
| 10 | 178.4 ms | 210.6 ms | 210.6 ms | 211.1 ms | 0 |
| 40 | 692.8 ms | 892.9 ms | 895.1 ms | 897.2 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 247.6 ms | 247.6 ms | 247.6 ms | 1 |
| leaf_touch | src/main.rs | 390.1 ms | 390.1 ms | 390.1 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 21.55 s | 21.55 s | 21.55 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 29.59 s | 29.59 s | 29.59 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (5b83dc3) | 13.7 ms | 93.1 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 17.6 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 28.4 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 104.6 ms | 346.5 MB |
| OpenCode | 1.19.5 | 628.1 ms | 344.6 MB |
| pi | 1.5.0 | 427.3 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 730.4 ms | 273.0 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 197.0 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 211.4 ms | 186.0 ms–802.2 ms | baseline | 31.4 MB | baseline | yes | — |
| jcode | 428.1 ms | 390.2 ms–716.3 ms | 2.02× more | 34.3 MB | 1.09× more | yes | — |
| Claude Code | 611.2 ms | 608.7 ms–1.21 s | 2.89× more | 197.1 MB | 6.28× more | yes | — |
| Codex CLI | 1.24 s | 1.12 s–3.10 s | 5.86× more | 144.6 MB | 4.61× more | yes | — |
| OpenCode | 4.32 s | 4.23 s–8.91 s | 20.42× more | 762.9 MB | 24.30× more | yes | — |
| pi | 592.2 ms | 519.0 ms–610.3 ms | 2.80× more | 123.3 MB | 3.93× more | yes | — |
| oh-my-pi (omp) | 2.10 s | 1.85 s–2.41 s | 9.94× more | 411.5 MB | 13.11× more | yes | — |
| oh-my-openagent (omo) | 9.14 s | 8.32 s–9.85 s | 43.25× more | 902.2 MB | 28.74× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 122.4 ms | 101.0 ms–131.7 ms | baseline | 22.7 MB | baseline | skill-uri | yes | — |
| jcode | 307.0 ms | 277.1 ms–342.9 ms | 2.51× more | 25.4 MB | 1.12× more | skill-tool | yes | — |
| Claude Code | 1.23 s | 1.13 s–1.27 s | 10.02× more | 203.2 MB | 8.96× more | skill-tool | yes | — |
| Codex CLI | 817.0 ms | 693.1 ms–825.2 ms | 6.68× more | 134.8 MB | 5.94× more | file-read | yes | — |
| OpenCode | 4.31 s | 4.22 s–4.77 s | 35.19× more | 714.1 MB | 31.47× more | skill-tool | yes | — |
| pi | 645.8 ms | 605.9 ms–664.5 ms | 5.28× more | 133.8 MB | 5.90× more | file-read | yes | — |
| oh-my-pi (omp) | 2.31 s | 2.06 s–2.55 s | 18.86× more | 400.7 MB | 17.66× more | file-read | yes | — |
| oh-my-openagent (omo) | 10.28 s | 10.16 s–12.81 s | 83.99× more | 886.7 MB | 39.08× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 157.1 ms | 125.5 ms–168.7 ms | baseline | 19.3 MB | baseline | mcp-call | yes | 111.6 ms | — |
| jcode | 420.4 ms | 354.2 ms–442.2 ms | 2.68× more | 52.7 MB | 2.73× more | mcp-action | yes | 387.2 ms | — |
| Claude Code | 1.18 s | 755.1 ms–1.37 s | 7.50× more | 219.4 MB | 11.36× more | mcp-tool | yes | 807.4 ms | — |
| Codex CLI | 1.05 s | 672.0 ms–1.24 s | 6.69× more | 142.4 MB | 7.37× more | mcp-tool | yes | 693.4 ms | — |
| OpenCode | 5.47 s | 5.39 s–6.24 s | 34.82× more | 693.5 MB | 35.91× more | mcp-tool | yes | 4.75 s | — |
| pi | 677.8 ms | 643.9 ms–776.2 ms | 4.31× more | 150.5 MB | 7.79× more | mcp-tool | yes | 600.1 ms | — |
| oh-my-pi (omp) | 2.08 s | 1.97 s–2.20 s | 13.21× more | 413.8 MB | 21.43× more | mcp-route | yes | 1.89 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.4 MB | 176.5 KB | baseline | 0.45 | 1.00 | 34.2 MB | 373.9 KB |
| jcode | 36.5 MB | 219.8 KB | 1.25× more | — | — | 40.2 MB | 313.6 KB |
| Claude Code | 156.8 MB | 134.4 MB | 779.91× more | — | — | 188.3 MB | 180.3 MB |
| Codex CLI | 88.4 MB | 81.3 MB | 471.53× more | — | — | 142.4 MB | 103.7 MB |
| OpenCode | 262.1 MB | 318.4 KB | 1.80× more | — | — | 548.9 MB | 13.2 MB |
| pi | 119.7 MB | 115.5 MB | 669.81× more | — | — | 149.2 MB | 147.9 MB |
| oh-my-pi (omp) | 375.4 MB | 136.3 MB | 790.65× more | — | — | 417.9 MB | 321.6 MB |
| oh-my-openagent (omo) | 59.7 MB | 43.5 MB | 252.30× more | — | — | 814.2 MB | 330.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.2 MB | 35.9 MB | 37.5 MB |
| jcode | 40.2 MB | 41.6 MB | 43.0 MB |
| Claude Code | 188.3 MB | 920.9 MB | 1.8 GB |
| Codex CLI | 142.4 MB | 536.3 MB | 1.0 GB |
| OpenCode | 548.9 MB | 597.8 MB | 667.5 MB |
| pi | 149.2 MB | 739.0 MB | 1.4 GB |
| oh-my-pi (omp) | 417.9 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 814.2 MB | 3.1 GB | 3.8 GB |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    9.12 ms
  Max:    21.09 ms
  Mean:   12.91 ms
  Median: 10.41 ms
  Stdev:  4.44 ms

--version:
  Min:    11.27 ms
  Max:    19.21 ms
  Mean:   14.71 ms
  Median: 14.17 ms
  Stdev:  2.67 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    26.34 ms
  Max:    99.09 ms
  Mean:   66.48 ms
  Median: 76.45 ms
  Stdev:  29.54 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    38.56 ms
  Max:    117.43 ms
  Mean:   74.67 ms
  Median: 70.68 ms
  Stdev:  24.36 ms

Bridge stage: hello_ok:
  Min:    40.26 ms
  Max:    118.29 ms
  Mean:   75.78 ms
  Median: 71.56 ms
  Stdev:  24.32 ms

Bridge stage: no-op reply:
  Min:    41.15 ms
  Max:    118.76 ms
  Mean:   76.41 ms
  Median: 72.22 ms
  Stdev:  24.17 ms

Bridge total (socket -> no-op):
  Min:    41.15 ms
  Max:    118.76 ms
  Mean:   76.41 ms
  Median: 72.22 ms
  Stdev:  24.17 ms

============================================================
Summary:
  Binary load median:      ~10.4 ms
  Server ready median:     ~76.4 ms
  Bridge total median:     ~72.2 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (998dc45c31cc)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-be5qwqmv
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.048/0.050    p95    0.097/0.125    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.248/0.350    p95    0.404/0.611    delta  +0.102 ms  control-no-change
  mcp_config_load   p50    0.295/0.418    p95    0.519/0.669    delta  +0.122 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   24.561/23.661   p95   32.555/60.806   delta  -0.901 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22962 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22962 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.867/4.484    p95   11.185/9.119    delta  -0.383 ms  control-no-change
  skill_request_overlay p50    7.177/6.400    p95   11.834/9.147    delta  -0.777 ms  control-no-change
  mcp_config_load   p50    0.291/0.301    p95    0.330/0.563    delta  +0.011 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   24.193/22.515   p95   34.521/27.138   delta  -1.678 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22954 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22962 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.032/0.039    p95    0.062/0.101    delta  +0.007 ms  control-no-change
  skill_request_overlay p50    0.267/0.313    p95    0.930/0.844    delta  +0.045 ms  control-no-change
  mcp_config_load   p50    0.314/0.306    p95    1.053/0.688    delta  -0.008 ms  control-no-change
  attach_history    p50  384.964/426.101  p95  564.226/735.251  delta +41.137 ms  control-no-change
  resume_ready      p50   75.235/87.870   p95  112.692/127.603  delta +12.635 ms  control-no-change
  provider_dispatch p50    1.492/1.428    p95   24.128/1.982    delta  -0.065 ms  control-no-change
  session_save      p50   25.765/41.612   p95  146.447/227.288  delta +15.847 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22971 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22964 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.577/4.732    p95    6.929/7.556    delta  +0.155 ms  control-no-change
  skill_request_overlay p50    5.939/5.690    p95   10.146/8.452    delta  -0.248 ms  control-no-change
  mcp_config_load   p50    0.282/0.299    p95    0.356/0.519    delta  +0.016 ms  control-no-change
  attach_history    p50  357.389/352.965  p95  549.469/404.598  delta  -4.424 ms  control-no-change
  resume_ready      p50   77.211/69.497   p95  106.706/97.922   delta  -7.714 ms  control-no-change
  provider_dispatch p50    1.409/1.270    p95    2.976/4.220    delta  -0.139 ms  control-no-change
  session_save      p50   23.168/26.213   p95   42.438/66.970   delta  +3.046 ms  control-no-change
  total store_read_bytes p50 5311304  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22931..22968 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22968 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.029    p95    0.118/0.065    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.221/0.233    p95    0.295/0.263    delta  +0.012 ms  control-no-change
  mcp_config_load   p50    0.254/0.261    p95    0.324/0.523    delta  +0.007 ms  control-no-change
  attach_history    p50 3896.423/3714.200 p95 4437.078/4159.849 delta -182.224 ms  control-no-change
  resume_ready      p50  828.682/726.905  p95 1034.275/819.001  delta -101.778 ms  control-no-change
  provider_dispatch p50   11.303/10.262   p95   16.053/18.471   delta  -1.041 ms  control-no-change
  session_save      p50   22.066/21.352   p95   54.186/41.620   delta  -0.713 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22967 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    5.217/4.744    p95   10.854/7.448    delta  -0.473 ms  control-no-change
  skill_request_overlay p50    6.028/6.625    p95   33.621/10.699   delta  +0.597 ms  control-no-change
  mcp_config_load   p50    0.287/0.300    p95    0.402/0.845    delta  +0.013 ms  control-no-change
  attach_history    p50 4125.040/4320.475 p95 5325.197/5805.214 delta +195.435 ms  control-no-change
  resume_ready      p50  855.608/775.218  p95 1019.985/1080.706 delta -80.389 ms  control-no-change
  provider_dispatch p50   12.539/11.434   p95   22.314/16.455   delta  -1.105 ms  control-no-change
  session_save      p50   28.032/29.740   p95  106.695/260.842  delta  +1.708 ms  control-no-change
  total store_read_bytes p50 53417511  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184299..42184303 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210250..11210254 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22934..22970 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184303 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210250..11210254 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22952..22974 (8 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (998dc45c31cc)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-wyjgv9n8
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.024/0.024    p95    0.033/0.029    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.216/0.213    p95    0.229/0.236    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.251/0.253    p95    0.264/0.267    delta  +0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   11.693/11.676   p95   13.265/13.959   delta  -0.017 ms  control-no-change
  total store_read_bytes p50 23669  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22935, 22947, 22951, 23657, 23665, 23673, 24363, 24371, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22947, 22951, 22955, 23653, 23665, 24375, 24383, 24391]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22935..24379 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22947..24391 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.024/4.024    p95    4.925/4.609    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    5.098/5.430    p95    5.772/16.932   delta  +0.332 ms  control-no-change
  mcp_config_load   p50    0.256/0.262    p95    0.437/0.292    delta  +0.006 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   13.232/14.848   p95   14.843/28.912   delta  +1.616 ms  control-no-change
  total store_read_bytes p50 23667  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22943, 22955, 23657, 23661, 23673, 24371, 24373, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22925, 22935, 22939, 23659, 23661, 23669, 24367, 24369, 24371, 24377]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22943..24383 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22925..24377 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.022/0.021    p95    0.025/0.028    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.210/0.197    p95    0.232/0.217    delta  -0.013 ms  control-no-change
  mcp_config_load   p50    0.237/0.227    p95    0.274/0.247    delta  -0.010 ms  control-no-change
  attach_history    p50   86.498/85.844   p95   91.692/90.597   delta  -0.653 ms  control-no-change
  resume_ready      p50   51.958/53.352   p95   55.338/58.092   delta  +1.394 ms  control-no-change
  provider_dispatch p50    0.925/0.938    p95    0.960/1.007    delta  +0.013 ms  control-no-change
  session_save      p50    9.987/10.002   p95   11.067/11.306   delta  +0.015 ms  control-no-change
  total store_read_bytes p50 2841256  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23672, 23678, 24372, 24394, 25094, 25102, 25790, 25808, 26512, 26516]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23674, 24378, 24388, 25092, 25098, 25806, 25812, 26526, 26532]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23672..26516 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23674..26532 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.497/3.650    p95    4.401/3.975    delta  +0.153 ms  control-no-change
  skill_request_overlay p50    4.410/4.598    p95    5.247/5.175    delta  +0.189 ms  control-no-change
  mcp_config_load   p50    0.227/0.235    p95    0.247/0.275    delta  +0.008 ms  control-no-change
  attach_history    p50   86.993/86.141   p95  102.284/93.271   delta  -0.852 ms  control-no-change
  resume_ready      p50   53.162/52.522   p95   57.335/56.077   delta  -0.640 ms  control-no-change
  provider_dispatch p50    0.984/0.957    p95    1.253/1.039    delta  -0.027 ms  control-no-change
  session_save      p50   11.939/11.888   p95   13.545/13.369   delta  -0.052 ms  control-no-change
  total store_read_bytes p50 2841249  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23669, 24375, 24389, 25091, 25095, 25809, 25813, 26519, 26531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23666, 23674, 24376, 24384, 25090, 25102, 25804, 25808, 26526, 26534]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23666..26534 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.025/0.024    p95    0.041/0.050    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.210/0.202    p95    0.251/0.238    delta  -0.007 ms  control-no-change
  mcp_config_load   p50    0.236/0.245    p95    0.251/0.318    delta  +0.009 ms  control-no-change
  attach_history    p50  936.566/927.053  p95 1004.908/995.131  delta  -9.514 ms  control-no-change
  resume_ready      p50  601.145/617.405  p95  828.440/638.096  delta +16.260 ms  control-no-change
  provider_dispatch p50    9.388/9.537    p95   11.298/10.070   delta  +0.150 ms  control-no-change
  session_save      p50   12.794/13.215   p95  112.505/31.308   delta  +0.422 ms  control-no-change
  total store_read_bytes p50 28640276  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23673, 23677, 24387, 25101, 25815, 25823, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23665, 23669, 24375, 24387, 25075, 25107, 25801, 25805, 26527, 26531]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23673..26529 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23665..26531 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.408/3.805    p95    4.146/4.015    delta  +0.397 ms  control-drift
  skill_request_overlay p50    4.554/4.888    p95    5.525/5.269    delta  +0.335 ms  control-no-change
  mcp_config_load   p50    0.248/0.247    p95    0.294/0.260    delta  -0.001 ms  control-no-change
  attach_history    p50  924.909/936.334  p95  968.093/1022.728 delta +11.425 ms  control-no-change
  resume_ready      p50  580.287/597.998  p95  605.926/665.264  delta +17.711 ms  control-no-change
  provider_dispatch p50    9.055/9.173    p95   10.863/10.875   delta  +0.117 ms  control-no-change
  session_save      p50   13.410/14.293   p95   17.412/17.043   delta  +0.883 ms  control-no-change
  total store_read_bytes p50 28640278  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23681, 24387, 24391, 25101, 25105, 25811, 25815, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23677, 23685, 24383, 24391, 25105, 25109, 25811, 25823, 26529, 26537]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26529 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23677..26537 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.6 ms | 0.2 ms | 0.6 ms | 5 |
| read (1000 lines) | 0.6 ms | 0.8 ms | 0.6 ms | 0.8 ms | 5 |
| read (10 lines @ offset) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| write (small) | 2.5 ms | 3.0 ms | 1.8 ms | 3.0 ms | 5 |
| write (10KB) | 2.4 ms | 3.0 ms | 2.3 ms | 3.0 ms | 5 |
| edit (simple replace) | 2.6 ms | 2.8 ms | 2.0 ms | 2.8 ms | 5 |
| ls (scripts dir) | 0.7 ms | 0.8 ms | 0.7 ms | 0.8 ms | 5 |
| agentgrep (grep in tree) | 4.8 ms | 6.3 ms | 3.7 ms | 6.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 2.1 ms | 1.5 ms | 2.1 ms | 5 |
| bash (true) | 7.4 ms | 10.0 ms | 5.4 ms | 10.0 ms | 5 |
| bash (echo hello) | 5.1 ms | 8.3 ms | 4.7 ms | 8.3 ms | 5 |
| bash (ls fixture tree) | 9.6 ms | 15.1 ms | 7.9 ms | 15.1 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 108.2 ms | 108.2 ms | 108.2 ms | 108.4 ms | 0 |
| 10 | 332.8 ms | 334.6 ms | 334.6 ms | 335.6 ms | 0 |
| 40 | 1.23 s | 1.23 s | 1.23 s | 1.23 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 993.1 ms | 993.1 ms | 993.1 ms | 1 |
| leaf_touch | src/main.rs | 532.4 ms | 532.4 ms | 532.4 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 55.24 s | 55.24 s | 55.24 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 55.79 s | 55.79 s | 55.79 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (5b83dc3) | 10.7 ms | 92.8 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 12.6 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 33.8 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 87.8 ms | 479.4 MB |
| OpenCode | 1.19.5 | 469.0 ms | 605.3 MB |
| pi | 1.5.0 | 240.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 584.1 ms | 291.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 173.2 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 349.7 ms | 309.1 ms–587.5 ms | baseline | 32.8 MB | baseline | yes | — |
| jcode | 120.8 ms | 117.8 ms–202.8 ms | 2.89× less | 31.8 MB | 1.03× less | yes | — |
| Claude Code | 1.09 s | 1.07 s–1.70 s | 3.13× more | 273.8 MB | 8.34× more | yes | — |
| Codex CLI | 3.50 s | 1.38 s–4.24 s | 10.00× more | 3.2 GB | 100.00× more | yes | — |
| OpenCode | 4.88 s | 4.61 s–6.79 s | 13.96× more | 828.9 MB | 25.25× more | yes | — |
| pi | 452.0 ms | 417.4 ms–496.6 ms | 1.29× more | 125.4 MB | 3.82× more | yes | — |
| oh-my-pi (omp) | 3.21 s | 2.69 s–3.65 s | 9.18× more | 399.2 MB | 12.16× more | yes | — |
| oh-my-openagent (omo) | 25.26 s | 17.12 s–27.30 s | 72.23× more | 544.0 MB | 16.57× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 207.5 ms | 193.3 ms–214.8 ms | baseline | 25.6 MB | baseline | skill-uri | yes | — |
| jcode | 182.4 ms | 179.1 ms–303.8 ms | 1.14× less | 23.8 MB | 1.08× less | skill-tool | yes | — |
| Claude Code | 1.36 s | 826.4 ms–2.05 s | 6.56× more | 237.0 MB | 9.26× more | skill-tool | yes | — |
| Codex CLI | 2.26 s | 2.01 s–2.40 s | 10.89× more | 265.0 MB | 10.36× more | file-read | yes | — |
| OpenCode | 6.18 s | 6.04 s–6.84 s | 29.79× more | 3.2 GB | 128.61× more | skill-tool | yes | — |
| pi | 591.9 ms | 553.8 ms–620.9 ms | 2.85× more | 97.8 MB | 3.82× more | file-read | yes | — |
| oh-my-pi (omp) | 3.40 s | 3.38 s–4.19 s | 16.38× more | 415.1 MB | 16.23× more | file-read | yes | — |
| oh-my-openagent (omo) | 28.88 s | 20.38 s–30.31 s | 139.21× more | 532.9 MB | 20.83× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 241.7 ms | 226.3 ms–258.9 ms | baseline | 37.2 MB | baseline | mcp-call | yes | 133.8 ms | — |
| jcode | 313.9 ms | 283.4 ms–322.3 ms | 1.30× more | 36.1 MB | 1.03× less | mcp-action | yes | 215.6 ms | — |
| Claude Code | 1.13 s | 1.03 s–1.14 s | 4.68× more | 273.2 MB | 7.35× more | mcp-tool | yes | 926.7 ms | — |
| Codex CLI | 850.5 ms | 769.4 ms–974.8 ms | 3.52× more | 177.7 MB | 4.78× more | mcp-tool | yes | 594.7 ms | — |
| OpenCode | 6.22 s | 5.95 s–6.23 s | 25.73× more | 3.2 GB | 89.12× more | mcp-tool | yes | 4.89 s | — |
| pi | 622.1 ms | 616.1 ms–677.5 ms | 2.57× more | 118.1 MB | 3.18× more | mcp-tool | yes | 504.7 ms | — |
| oh-my-pi (omp) | 3.88 s | 3.63 s–4.53 s | 16.06× more | 422.0 MB | 11.35× more | mcp-route | yes | 2.95 s | — |
| oh-my-openagent (omo) | 27.39 s | 26.89 s–30.35 s | 113.29× more | 558.4 MB | 15.02× more | mcp-tool | yes | 16.95 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.6 MB | 123.9 KB | baseline | — | — | 29.6 MB | 495.0 KB |
| jcode | 22.0 MB | 49.6 KB | 2.50× less | — | — | 26.9 MB | 301.3 KB |
| Claude Code | 154.3 MB | 175.0 MB | 1,447× more | — | — | 188.9 MB | 189.7 MB |
| Codex CLI | 127.8 MB | 98.0 MB | 810.09× more | — | — | 151.0 MB | 124.9 MB |
| OpenCode | 234.9 MB | 1.8 MB | 14.73× more | — | — | 433.7 MB | 18.2 MB |
| pi | 90.6 MB | 492.4 MB | 4,071× more | — | — | 100.8 MB | 512.5 MB |
| oh-my-pi (omp) | 321.7 MB | 556.3 MB | 4,599× more | — | — | 345.9 MB | 991.2 MB |
| oh-my-openagent (omo) | 144.9 MB | 138.2 MB | 1,142× more | — | — | 411.0 MB | 420.1 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.6 MB | 32.2 MB | 34.0 MB |
| jcode | 26.9 MB | 28.3 MB | 29.6 MB |
| Claude Code | 188.9 MB | 942.4 MB | 1.9 GB |
| Codex CLI | 151.0 MB | 648.6 MB | 1.2 GB |
| OpenCode | 433.7 MB | 460.2 MB | 594.0 MB |
| pi | 100.8 MB | 504.8 MB | 4.5 GB |
| oh-my-pi (omp) | 345.9 MB | 1.7 GB | 8.9 GB |
| oh-my-openagent (omo) | 411.0 MB | 2.0 GB | 3.7 GB |

**Errors recorded during measurement**

- oh-my-openagent (omo) @ 10 sessions: turn 1 session 0: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 1: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 2: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 3: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 4: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 5: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 6: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 7: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 8: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 9: timeout

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    14.06 ms
  Max:    14.54 ms
  Mean:   14.29 ms
  Median: 14.29 ms
  Stdev:  0.15 ms

--version:
  Min:    13.72 ms
  Max:    15.37 ms
  Mean:   14.09 ms
  Median: 13.99 ms
  Stdev:  0.48 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.28 ms
  Max:    27.41 ms
  Mean:   22.93 ms
  Median: 22.34 ms
  Stdev:  1.59 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~14.3 ms
  Server ready median:     ~22.3 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (c76f097b42b9)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-9dchmzwu
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.078/0.077    p95    0.089/0.093    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.569/0.597    p95    0.637/0.664    delta  +0.029 ms  control-no-change
  mcp_config_load   p50    0.703/0.691    p95    0.789/0.773    delta  -0.012 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   48.611/48.512   p95   51.166/66.735   delta  -0.099 ms  control-no-change
  total store_read_bytes p50 22954  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22930..22964 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22914..22968 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   10.541/10.526   p95   10.982/11.129   delta  -0.016 ms  control-no-change
  skill_request_overlay p50   30.014/29.454   p95   32.049/34.662   delta  -0.560 ms  control-no-change
  mcp_config_load   p50    0.737/0.710    p95    0.785/0.770    delta  -0.027 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   65.542/68.123   p95  156.199/95.640   delta  +2.581 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22922..22968 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22926..22964 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.079/0.074    p95    0.106/0.097    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.576/0.569    p95    0.918/0.704    delta  -0.007 ms  control-no-change
  mcp_config_load   p50    0.701/0.693    p95    0.944/0.893    delta  -0.008 ms  control-no-change
  attach_history    p50  324.518/324.149  p95  394.079/503.131  delta  -0.369 ms  control-no-change
  resume_ready      p50  103.914/104.072  p95  117.814/117.620  delta  +0.158 ms  control-no-change
  provider_dispatch p50    1.221/1.206    p95    1.527/1.278    delta  -0.015 ms  control-no-change
  session_save      p50   44.049/42.606   p95  303.738/48.094   delta  -1.444 ms  control-no-change
  total store_read_bytes p50 5311208  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189147..4189153 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099099..1099105 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22922..22971 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189150..4189153 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099105 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22920..22975 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   10.925/10.874   p95   11.595/11.473   delta  -0.052 ms  control-no-change
  skill_request_overlay p50   29.613/29.433   p95   31.995/37.172   delta  -0.180 ms  control-no-change
  mcp_config_load   p50    0.715/0.739    p95    0.767/0.838    delta  +0.024 ms  control-no-change
  attach_history    p50  309.108/315.074  p95  350.458/334.696  delta  +5.966 ms  control-no-change
  resume_ready      p50  100.355/99.582   p95  114.364/104.238  delta  -0.772 ms  control-no-change
  provider_dispatch p50    1.140/1.158    p95    1.234/1.233    delta  +0.017 ms  control-no-change
  session_save      p50   47.035/42.999   p95   72.969/74.672   delta  -4.036 ms  control-no-change
  total store_read_bytes p50 5311202  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22910..22976 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22907..22972 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.072/0.073    p95    0.092/0.096    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.563/0.597    p95    0.599/0.656    delta  +0.034 ms  control-no-change
  mcp_config_load   p50    0.682/0.686    p95    0.724/0.734    delta  +0.004 ms  control-no-change
  attach_history    p50 3612.211/3607.919 p95 3652.574/3716.060 delta  -4.292 ms  control-no-change
  resume_ready      p50 1070.304/1075.943 p95 1079.849/1104.042 delta  +5.638 ms  control-no-change
  provider_dispatch p50   14.225/13.510   p95   14.768/14.818   delta  -0.715 ms  control-no-change
  session_save      p50   42.535/42.635   p95   45.397/53.851   delta  +0.100 ms  control-no-change
  total store_read_bytes p50 53417430  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22935..22967 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22959..22975 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   10.848/10.922   p95   11.602/14.482   delta  +0.075 ms  control-no-change
  skill_request_overlay p50   29.426/30.088   p95   33.033/35.851   delta  +0.663 ms  control-no-change
  mcp_config_load   p50    0.718/0.746    p95    0.753/0.774    delta  +0.028 ms  control-no-change
  attach_history    p50 3700.463/3655.867 p95 3796.686/3849.272 delta -44.596 ms  control-no-change
  resume_ready      p50 1064.725/1086.031 p95 1088.508/1208.354 delta +21.305 ms  control-no-change
  provider_dispatch p50   13.835/14.012   p95   14.603/14.774   delta  +0.178 ms  control-no-change
  session_save      p50   40.765/41.806   p95  555.195/67.086   delta  +1.041 ms  control-no-change
  total store_read_bytes p50 53417434  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184247..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210210..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22937..22979 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184247..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210211..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22941..22991 (7 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (c76f097b42b9)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-6_l67sal
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.066/0.065    p95    0.077/0.088    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.528/0.518    p95    0.558/0.533    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.619/0.638    p95    0.695/0.686    delta  +0.019 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   30.041/28.386   p95   38.826/33.094   delta  -1.655 ms  control-no-change
  total store_read_bytes p50 23635  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22937, 22941, 22953, 22957, 23626, 23644, 24319, 24331, 24347]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22945, 22947, 22957, 23636, 23640, 23652, 24311, 24326, 24347, 24363]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22937..24347 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22945..24363 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   10.212/10.265   p95   10.660/10.833   delta  +0.053 ms  control-no-change
  skill_request_overlay p50   27.551/27.549   p95   28.077/33.541   delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.652/0.654    p95    0.762/0.678    delta  +0.002 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   36.772/37.202   p95   45.398/51.080   delta  +0.430 ms  control-no-change
  total store_read_bytes p50 23626  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22908, 22929, 22943, 22959, 23610, 23641, 23646, 23649, 24325, 24364]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22895, 22941, 22949, 23632, 23652, 23660, 24327, 24335, 24367]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22908..24364 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22895..24367 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.070/0.069    p95    0.091/0.080    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.520/0.523    p95    0.561/0.563    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    0.636/0.652    p95    0.691/0.697    delta  +0.016 ms  control-no-change
  attach_history    p50  135.834/136.793  p95  138.564/142.467  delta  +0.959 ms  control-no-change
  resume_ready      p50  100.546/100.113  p95  104.936/102.125  delta  -0.433 ms  control-no-change
  provider_dispatch p50    1.100/1.073    p95    1.196/1.172    delta  -0.027 ms  control-no-change
  session_save      p50   27.540/29.459   p95   29.292/33.954   delta  +1.919 ms  control-no-change
  total store_read_bytes p50 2841143  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23633, 23651, 24313, 24350, 25061, 25065, 25748, 25760, 26447, 26457]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23610, 23675, 24298, 24349, 25056, 25063, 25770, 25771, 26441, 26454]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23633..26457 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23610..26454 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   10.258/10.181   p95   10.809/13.673   delta  -0.077 ms  control-no-change
  skill_request_overlay p50   27.387/27.606   p95   28.016/38.490   delta  +0.220 ms  control-no-change
  mcp_config_load   p50    0.681/0.672    p95    0.721/1.226    delta  -0.009 ms  control-no-change
  attach_history    p50  134.286/137.671  p95  140.613/202.500  delta  +3.386 ms  control-no-change
  resume_ready      p50   99.409/101.451  p95  105.174/153.100  delta  +2.042 ms  control-no-change
  provider_dispatch p50    1.007/1.121    p95    1.152/1.244    delta  +0.113 ms  control-no-change
  session_save      p50   35.434/34.119   p95   56.844/41.158   delta  -1.316 ms  control-no-change
  total store_read_bytes p50 2841134  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23602, 23671, 24317, 24359, 25049, 25058, 25733, 25766, 26422, 26429]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23653, 23656, 24348, 24359, 25019, 25057, 25760, 25764, 26410, 26447]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23602..26429 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23653..26447 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.070/0.071    p95    0.125/0.100    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.534/0.565    p95    0.613/0.645    delta  +0.031 ms  control-no-change
  mcp_config_load   p50    0.705/0.699    p95    0.750/0.781    delta  -0.005 ms  control-no-change
  attach_history    p50 1510.527/1491.208 p95 1645.138/1599.523 delta -19.319 ms  control-no-change
  resume_ready      p50 1084.137/1067.250 p95 1098.133/1128.318 delta -16.887 ms  control-no-change
  provider_dispatch p50   13.625/13.071   p95   14.729/14.054   delta  -0.554 ms  control-no-change
  session_save      p50   30.468/31.221   p95   79.601/33.708   delta  +0.753 ms  control-no-change
  total store_read_bytes p50 28640142  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23666, 24353, 24361, 25026, 25064, 25723, 25779, 26448, 26474]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23653, 23660, 24326, 24330, 25046, 25051, 25749, 25750, 26438, 26465]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26474 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23653..26465 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   10.445/10.193   p95   10.749/10.866   delta  -0.252 ms  control-no-change
  skill_request_overlay p50   29.084/28.141   p95   31.219/30.086   delta  -0.943 ms  control-no-change
  mcp_config_load   p50    0.674/0.675    p95    0.750/0.700    delta  +0.001 ms  control-no-change
  attach_history    p50 1487.756/1489.256 p95 1544.717/1537.499 delta  +1.500 ms  control-no-change
  resume_ready      p50 1058.177/1073.504 p95 1075.562/1114.027 delta +15.327 ms  control-no-change
  provider_dispatch p50   13.755/14.243   p95   14.788/15.091   delta  +0.488 ms  control-no-change
  session_save      p50   28.931/28.367   p95   30.968/33.802   delta  -0.564 ms  control-no-change
  total store_read_bytes p50 28640134  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 23662, 24328, 24337, 25033, 25040, 25738, 25749, 26444, 26449]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23678, 23679, 24330, 24344, 25047, 25061, 25748, 25756, 26438, 26443]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26449 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23678..26443 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.6 ms | 2.7 ms | 2.1 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.6 ms | 2.7 ms | 2.1 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 15.0 ms | 58.5 ms | 10.4 ms | 58.5 ms | 5 |
| write (10KB) | 12.4 ms | 18.0 ms | 9.9 ms | 18.0 ms | 5 |
| edit (simple replace) | 9.9 ms | 73.0 ms | 9.9 ms | 73.0 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 5.2 ms | 5.3 ms | 4.8 ms | 5.3 ms | 5 |
| agentgrep (find **/*.rs) | 5.2 ms | 5.2 ms | 4.2 ms | 5.2 ms | 5 |
| bash (true) | 7.8 ms | 7.8 ms | 7.3 ms | 7.8 ms | 5 |
| bash (echo hello) | 7.8 ms | 10.0 ms | 7.3 ms | 10.0 ms | 5 |
| bash (ls fixture tree) | 10.4 ms | 10.5 ms | 9.9 ms | 10.5 ms | 5 |
| invalid tool (dispatch only) | 2.6 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 226.8 ms | 226.8 ms | 226.8 ms | 227.0 ms | 0 |
| 10 | 920.1 ms | 921.4 ms | 921.4 ms | 922.9 ms | 0 |
| 40 | 2.42 s | 3.33 s | 3.33 s | 3.34 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 545.1 ms | 545.1 ms | 545.1 ms | 1 |
| leaf_touch | src\main.rs | 750.1 ms | 750.1 ms | 750.1 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 23.92 s | 23.92 s | 23.92 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 31.64 s | 31.64 s | 31.64 s | 1 |

<!-- bench-auto:end -->
