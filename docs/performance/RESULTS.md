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

<!-- bench-auto:start commit=8724ad632e3798a4161d8158ff3728f2ad27307b -->

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `8724ad632e3798a4161d8158ff3728f2ad27307b`
- Measured at: 2026-11-11 00:22 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/42924574911> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (8724ad6)` | Success |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (8724ad6)` | Success |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (8724ad6)` | Success |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (8724ad6) | 3.6 ms | 112.4 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 8.5 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 8.6 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 39.9 ms | 435.0 MB |
| OpenCode | 1.19.5 | 463.2 ms | 613.4 MB |
| pi | 1.5.0 | 217.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 730.2 ms | 412.9 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 110.5 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 50.2 ms | 49.4 ms–247.6 ms | baseline | 47.0 MB | baseline | yes | 10 | — |
| jcode | 141.1 ms | 140.8 ms–149.2 ms | 2.81× more | 42.0 MB | 1.12× less | yes | 5 | — |
| Claude Code | 424.4 ms | 417.5 ms–451.4 ms | 8.45× more | 207.8 MB | 4.43× more | yes | — | — |
| Codex CLI | 622.5 ms | 540.9 ms–679.8 ms | 12.39× more | 199.3 MB | 4.25× more | yes | 6 | — |
| OpenCode | 2.78 s | 2.59 s–2.82 s | 55.35× more | 566.5 MB | 12.06× more | yes | 4 | — |
| pi | 355.9 ms | 354.8 ms–370.1 ms | 7.08× more | 114.3 MB | 2.43× more | yes | — | — |
| oh-my-pi (omp) | 1.95 s | 1.75 s–2.10 s | 38.90× more | 394.1 MB | 8.39× more | yes | 71 | — |
| oh-my-openagent (omo) | 6.26 s | 6.02 s–6.58 s | 124.71× more | 665.2 MB | 14.17× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 46.8 ms | 46.3 ms–49.2 ms | baseline | 35.8 MB | baseline | skill-uri | yes | — |
| jcode | 53.8 ms | 53.4 ms–55.7 ms | 1.15× more | 40.6 MB | 1.13× more | skill-tool | yes | — |
| Claude Code | 451.7 ms | 438.1 ms–455.1 ms | 9.65× more | 207.8 MB | 5.80× more | skill-tool | yes | — |
| Codex CLI | 795.8 ms | 637.5 ms–935.0 ms | 16.99× more | 200.5 MB | 5.60× more | file-read | yes | — |
| OpenCode | 3.06 s | 3.03 s–3.11 s | 65.26× more | 606.4 MB | 16.93× more | skill-tool | yes | — |
| pi | 490.9 ms | 489.2 ms–504.3 ms | 10.48× more | 125.6 MB | 3.51× more | file-read | yes | — |
| oh-my-pi (omp) | 1.76 s | 1.73 s–1.79 s | 37.53× more | 338.5 MB | 9.45× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.79 s | 6.36 s–6.90 s | 145.03× more | 671.6 MB | 18.74× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 68.9 ms | 65.2 ms–69.0 ms | baseline | 45.1 MB | baseline | mcp-call | yes | 53.5 ms | — |
| jcode | 174.0 ms | 172.6 ms–175.1 ms | 2.53× more | 53.9 MB | 1.20× more | mcp-action | yes | 142.4 ms | — |
| Claude Code | 544.8 ms | 537.0 ms–566.6 ms | 7.91× more | 224.7 MB | 4.98× more | mcp-tool | yes | 401.8 ms | — |
| Codex CLI | 650.9 ms | 608.8 ms–703.2 ms | 9.45× more | 196.5 MB | 4.36× more | mcp-tool | yes | 341.3 ms | — |
| OpenCode | 2.76 s | 2.72 s–2.79 s | 40.10× more | 565.7 MB | 12.55× more | mcp-tool | yes | 2.54 s | — |
| pi | 548.7 ms | 538.5 ms–553.2 ms | 7.97× more | 140.3 MB | 3.11× more | mcp-tool | yes | 490.6 ms | — |
| oh-my-pi (omp) | 1.84 s | 1.79 s–2.09 s | 26.71× more | 385.0 MB | 8.54× more | mcp-route | yes | 1.71 s | — |
| oh-my-openagent (omo) | 6.64 s | 6.60 s–6.75 s | 96.46× more | 665.8 MB | 14.77× more | mcp-tool | yes | 3.41 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 36.8 MB | 133.3 KB | baseline | 0.66 | 1.00 | 44.0 MB | 501.3 KB |
| jcode | 42.6 MB | 163.2 KB | 1.22× more | 0.00 | — | 46.1 MB | 299.5 KB |
| Claude Code | 150.6 MB | 150.0 MB | 1,152× more | — | — | 184.9 MB | 181.2 MB |
| Codex CLI | 129.7 MB | 132.7 MB | 1,020× more | — | — | 162.8 MB | 164.4 MB |
| OpenCode | 241.1 MB | 300.3 KB | 2.25× more | — | — | 453.9 MB | 20.8 MB |
| pi | 109.3 MB | 109.2 MB | 839.14× more | — | — | 119.2 MB | 132.2 MB |
| oh-my-pi (omp) | 347.5 MB | 56.1 MB | 430.87× more | — | — | 365.8 MB | 358.2 MB |
| oh-my-openagent (omo) | 297.2 MB | 34.0 MB | 260.85× more | — | — | 594.1 MB | 594.0 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.0 MB | 46.4 MB | 48.4 MB |
| jcode | 46.1 MB | 47.3 MB | 48.7 MB |
| Claude Code | 184.9 MB | 922.2 MB | 1.8 GB |
| Codex CLI | 162.8 MB | 814.3 MB | 1.6 GB |
| OpenCode | 453.9 MB | 490.3 MB | 637.5 MB |
| pi | 119.2 MB | 646.5 MB | 1.3 GB |
| oh-my-pi (omp) | 365.8 MB | 1.7 GB | 3.5 GB |
| oh-my-openagent (omo) | 594.1 MB | 3.0 GB | 5.8 GB |

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.40 ms
  Max:    3.82 ms
  Mean:   3.60 ms
  Median: 3.55 ms
  Stdev:  0.15 ms

--version:
  Min:    3.52 ms
  Max:    3.73 ms
  Mean:   3.61 ms
  Median: 3.62 ms
  Stdev:  0.06 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.60 ms
  Max:    20.71 ms
  Mean:   20.64 ms
  Median: 20.63 ms
  Stdev:  0.03 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.61 ms
  Max:    20.76 ms
  Mean:   20.70 ms
  Median: 20.70 ms
  Stdev:  0.05 ms

Bridge stage: hello_ok:
  Min:    21.29 ms
  Max:    21.61 ms
  Mean:   21.44 ms
  Median: 21.45 ms
  Stdev:  0.10 ms

Bridge stage: no-op reply:
  Min:    21.69 ms
  Max:    22.15 ms
  Mean:   21.86 ms
  Median: 21.85 ms
  Stdev:  0.13 ms

Bridge total (socket -> no-op):
  Min:    21.69 ms
  Max:    22.15 ms
  Mean:   21.86 ms
  Median: 21.85 ms
  Stdev:  0.13 ms

============================================================
Summary:
  Binary load median:      ~3.6 ms
  Server ready median:     ~20.6 ms
  Bridge total median:     ~21.8 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (e2dfdaafd454)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-jppxb6rr
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.008/0.008    p95    0.011/0.010    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.052/0.052    p95    0.079/0.053    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.073    p95    0.092/0.087    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    8.066/7.903    p95    8.833/8.501    delta  -0.163 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23068..23080 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23056..23080 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.759/1.756    p95    2.250/1.853    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    2.509/2.505    p95    2.882/2.579    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.081/0.080    p95    0.099/0.083    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50    9.327/9.190    p95   13.693/13.711   delta  -0.136 ms  control-no-change
  total store_read_bytes p50 23070  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23048..23080 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23056..23080 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.008/0.008    p95    0.011/0.010    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.051/0.052    p95    0.052/0.072    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.073/0.074    p95    0.092/0.105    delta  +0.000 ms  control-no-change
  attach_history    p50  258.518/257.634  p95  263.963/259.380  delta  -0.884 ms  control-no-change
  resume_ready      p50   38.507/38.359   p95   39.366/38.657   delta  -0.149 ms  control-no-change
  provider_dispatch p50    1.265/1.268    p95    1.311/1.318    delta  +0.003 ms  control-no-change
  session_save      p50    7.015/6.545    p95    7.945/7.304    delta  -0.471 ms  control-no-change
  total store_read_bytes p50 5311396  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23086 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189191 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099133 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23062..23082 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.789/1.798    p95    1.838/1.860    delta  +0.009 ms  control-no-change
  skill_request_overlay p50    2.519/2.519    p95    2.587/2.589    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.082/0.081    p95    0.088/0.082    delta  -0.001 ms  control-no-change
  attach_history    p50  261.380/260.308  p95  267.160/270.688  delta  -1.072 ms  control-no-change
  resume_ready      p50   38.578/38.644   p95   38.879/39.325   delta  +0.067 ms  control-no-change
  provider_dispatch p50    1.270/1.288    p95    1.334/1.306    delta  +0.018 ms  control-no-change
  session_save      p50    6.979/6.579    p95   11.316/7.237    delta  -0.400 ms  control-no-change
  total store_read_bytes p50 5311394  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23058..23086 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23082 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.008/0.008    p95    0.011/0.010    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.051/0.051    p95    0.056/0.052    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.072    p95    0.082/0.073    delta  -0.001 ms  control-no-change
  attach_history    p50 3473.513/3460.726 p95 3505.842/3489.637 delta -12.787 ms  control-no-change
  resume_ready      p50  466.320/461.400  p95  482.636/491.117  delta  -4.920 ms  control-no-change
  provider_dispatch p50   17.397/17.180   p95   18.007/22.208   delta  -0.217 ms  control-no-change
  session_save      p50    6.605/6.840    p95    7.432/7.447    delta  +0.235 ms  control-no-change
  total store_read_bytes p50 53417609  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23065..23085 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23089 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.738/1.753    p95    1.801/1.774    delta  +0.015 ms  control-no-change
  skill_request_overlay p50    2.419/2.428    p95    2.472/2.624    delta  +0.009 ms  control-no-change
  mcp_config_load   p50    0.079/0.079    p95    0.081/0.086    delta  -0.000 ms  control-no-change
  attach_history    p50 3461.760/3470.606 p95 3490.570/3506.083 delta  +8.846 ms  control-no-change
  resume_ready      p50  451.753/450.040  p95  473.030/493.964  delta  -1.714 ms  control-no-change
  provider_dispatch p50   16.457/16.466   p95   18.117/17.510   delta  +0.010 ms  control-no-change
  session_save      p50    6.971/6.656    p95    7.629/7.045    delta  -0.315 ms  control-no-change
  total store_read_bytes p50 53417610  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23069..23089 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23069..23089 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (e2dfdaafd454)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-kium_6ih
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.019/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.105    p95    0.140/0.130    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.163/0.172    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  session_save      p50    8.837/8.493    p95   13.709/11.308   delta  -0.345 ms  control-no-change
  total store_read_bytes p50 23785  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23057, 23069, 23071, 23783, 23787, 23799, 24497, 24505, 24513]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23063, 23065, 23073, 23775, 23787, 24493, 24501, 24503, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23057..24513 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23063..24517 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.694/2.720    p95    2.778/2.801    delta  +0.027 ms  control-no-change
  skill_request_overlay p50    4.663/4.705    p95    4.728/4.748    delta  +0.042 ms  control-no-change
  mcp_config_load   p50    0.147/0.147    p95    0.163/0.157    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    9.393/9.424    p95    9.817/10.689   delta  +0.031 ms  control-no-change
  total store_read_bytes p50 23799  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23061, 23065, 23069, 23791, 23795, 23803, 24485, 24501, 24509, 24525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23065, 23073, 23077, 23779, 23787, 23791, 24497, 24503, 24505, 24525]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23061..24525 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23065..24525 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.020/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.117/0.106    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.143    p95    0.162/0.148    delta  -0.000 ms  control-no-change
  attach_history    p50   98.213/98.442   p95  100.480/99.974   delta  +0.230 ms  control-no-change
  resume_ready      p50   53.085/53.148   p95   53.830/53.809   delta  +0.063 ms  control-no-change
  provider_dispatch p50    1.313/1.323    p95    1.354/1.352    delta  +0.010 ms  control-no-change
  session_save      p50    8.338/8.301    p95   13.279/8.916    delta  -0.037 ms  control-no-change
  total store_read_bytes p50 2841353  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23786, 23788, 24514, 24516, 25224, 25230, 25946, 25948, 26658, 26666]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23792, 23804, 24510, 25232, 25236, 25946, 25954, 26660, 26668]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23786..26666 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23792..26668 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.787/2.766    p95    2.878/2.865    delta  -0.021 ms  control-no-change
  skill_request_overlay p50    4.698/4.681    p95    4.735/4.762    delta  -0.016 ms  control-no-change
  mcp_config_load   p50    0.149/0.148    p95    0.166/0.154    delta  -0.001 ms  control-no-change
  attach_history    p50   98.968/98.908   p95  100.734/99.710   delta  -0.060 ms  control-no-change
  resume_ready      p50   53.421/53.428   p95   54.548/54.223   delta  +0.007 ms  control-no-change
  provider_dispatch p50    1.332/1.330    p95    1.369/1.345    delta  -0.001 ms  control-no-change
  session_save      p50    9.567/9.470    p95   10.910/11.357   delta  -0.097 ms  control-no-change
  total store_read_bytes p50 2841361  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23778, 23788, 24504, 24510, 25218, 25236, 25948, 25954, 26660, 26662]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23792, 23794, 24502, 24516, 25222, 25226, 25944, 26650, 26658]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23778..26662 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23792..26658 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.017/0.017    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.108    p95    0.119/0.127    delta  +0.003 ms  control-drift
  mcp_config_load   p50    0.144/0.149    p95    0.171/0.194    delta  +0.005 ms  control-drift
  attach_history    p50 1073.649/1080.340 p95 1089.247/1100.382 delta  +6.691 ms  control-no-change
  resume_ready      p50  576.587/575.282  p95  580.797/592.517  delta  -1.305 ms  control-no-change
  provider_dispatch p50   13.980/14.127   p95   14.379/14.621   delta  +0.147 ms  control-no-change
  session_save      p50    7.941/8.028    p95    8.233/8.329    delta  +0.087 ms  control-no-change
  total store_read_bytes p50 28640379  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23781, 23799, 24499, 24513, 25225, 25231, 25947, 25961, 26661, 26663]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23781, 23795, 24487, 24517, 25233, 25239, 25945, 25947, 26667, 26669]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23781..26663 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23781..26669 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.661/2.648    p95    2.809/2.716    delta  -0.013 ms  control-no-change
  skill_request_overlay p50    4.647/4.676    p95    4.810/4.737    delta  +0.030 ms  control-no-change
  mcp_config_load   p50    0.147/0.147    p95    0.170/0.160    delta  -0.000 ms  control-no-change
  attach_history    p50 1072.957/1070.380 p95 1088.144/1086.777 delta  -2.577 ms  control-no-change
  resume_ready      p50  574.240/575.223  p95  581.553/579.169  delta  +0.982 ms  control-no-change
  provider_dispatch p50   14.018/13.840   p95   14.178/14.217   delta  -0.178 ms  control-no-change
  session_save      p50    9.539/9.169    p95   12.167/9.747    delta  -0.370 ms  control-no-change
  total store_read_bytes p50 28640376  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23791, 23803, 24505, 24509, 25227, 25239, 25929, 25961, 26659, 26667]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23779, 23785, 24497, 24503, 25219, 25945, 25949, 26671, 26675]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23791..26667 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23779..26675 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 39.8 ms | 110.1 ms | 0.9 ms | 110.1 ms | 5 |
| write (10KB) | 1.0 ms | 1.1 ms | 1.0 ms | 1.1 ms | 5 |
| edit (simple replace) | 1.0 ms | 1.0 ms | 1.0 ms | 1.0 ms | 5 |
| ls (scripts dir) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| agentgrep (grep in tree) | 1.1 ms | 1.1 ms | 1.0 ms | 1.1 ms | 5 |
| agentgrep (find **/*.rs) | 0.9 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.5 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| bash (echo hello) | 1.4 ms | 1.5 ms | 1.4 ms | 1.5 ms | 5 |
| bash (ls fixture tree) | 2.5 ms | 2.5 ms | 2.4 ms | 2.5 ms | 5 |
| invalid tool (dispatch only) | 0.0 ms | 0.1 ms | 0.0 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 79.3 ms | 79.3 ms | 79.3 ms | 79.5 ms | 0 |
| 10 | 197.2 ms | 300.9 ms | 300.9 ms | 302.0 ms | 0 |
| 40 | 928.5 ms | 1.08 s | 1.08 s | 1.08 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 277.7 ms | 277.7 ms | 277.7 ms | 1 |
| leaf_touch | src/main.rs | 424.4 ms | 424.4 ms | 424.4 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 21.06 s | 21.06 s | 21.06 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 29.38 s | 29.38 s | 29.38 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (8724ad6) | 7.3 ms | 94.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.6 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 14.7 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 43.3 ms | 346.5 MB |
| OpenCode | 1.19.5 | 310.0 ms | 344.6 MB |
| pi | 1.5.0 | 209.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 530.2 ms | 273.3 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 143.6 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 61.3 ms | 60.0 ms–290.1 ms | baseline | 31.2 MB | baseline | yes | — |
| jcode | 301.2 ms | 292.2 ms–504.6 ms | 4.91× more | 34.0 MB | 1.09× more | yes | — |
| Claude Code | 315.4 ms | 290.0 ms–651.4 ms | 5.14× more | 195.1 MB | 6.24× more | yes | — |
| Codex CLI | 350.0 ms | 254.2 ms–2.12 s | 5.71× more | 142.5 MB | 4.56× more | yes | — |
| OpenCode | 2.11 s | 1.78 s–2.69 s | 34.40× more | 705.4 MB | 22.57× more | yes | — |
| pi | 677.8 ms | 491.0 ms–1.30 s | 11.05× more | 120.3 MB | 3.85× more | yes | — |
| oh-my-pi (omp) | 1.57 s | 1.42 s–1.72 s | 25.63× more | 406.9 MB | 13.02× more | yes | — |
| oh-my-openagent (omo) | 8.38 s | 8.20 s–8.66 s | 136.69× more | 856.6 MB | 27.41× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 56.2 ms | 52.4 ms–356.8 ms | baseline | 30.5 MB | baseline | skill-uri | yes | — |
| jcode | 202.5 ms | 188.6 ms–257.4 ms | 3.61× more | 31.3 MB | 1.03× more | skill-tool | yes | — |
| Claude Code | 295.6 ms | 265.2 ms–322.3 ms | 5.26× more | 191.5 MB | 6.29× more | skill-tool | yes | — |
| Codex CLI | 364.2 ms | 340.2 ms–425.2 ms | 6.48× more | 143.8 MB | 4.72× more | file-read | yes | — |
| OpenCode | 2.70 s | 2.49 s–2.98 s | 47.99× more | 691.7 MB | 22.71× more | skill-tool | yes | — |
| pi | 406.7 ms | 396.0 ms–454.6 ms | 7.24× more | 151.0 MB | 4.96× more | file-read | yes | — |
| oh-my-pi (omp) | 1.54 s | 1.31 s–1.76 s | 27.49× more | 403.9 MB | 13.26× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.84 s | 6.77 s–11.74 s | 121.80× more | 879.1 MB | 28.87× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 88.1 ms | 84.5 ms–95.5 ms | baseline | 19.3 MB | baseline | mcp-call | yes | 71.6 ms | — |
| jcode | 313.9 ms | 305.8 ms–321.5 ms | 3.56× more | 51.9 MB | 2.69× more | mcp-action | yes | 295.8 ms | — |
| Claude Code | 416.9 ms | 357.9 ms–517.5 ms | 4.73× more | 220.0 MB | 11.39× more | mcp-tool | yes | 312.7 ms | — |
| Codex CLI | 345.4 ms | 284.4 ms–421.9 ms | 3.92× more | 120.5 MB | 6.24× more | mcp-tool | yes | 197.8 ms | — |
| OpenCode | 2.47 s | 2.29 s–2.55 s | 28.00× more | 723.9 MB | 37.48× more | mcp-tool | yes | 2.09 s | — |
| pi | 418.2 ms | 404.9 ms–440.4 ms | 4.75× more | 137.4 MB | 7.11× more | mcp-tool | yes | 364.2 ms | — |
| oh-my-pi (omp) | 1.50 s | 1.45 s–1.74 s | 17.08× more | 378.5 MB | 19.60× more | mcp-route | yes | 1.43 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.7 MB | 157.2 KB | baseline | 0.78 | 1.00 | 34.7 MB | 383.7 KB |
| jcode | 36.3 MB | 229.5 KB | 1.46× more | — | — | 39.9 MB | 370.1 KB |
| Claude Code | 156.7 MB | 157.2 MB | 1,024× more | — | — | 188.5 MB | 185.1 MB |
| Codex CLI | 122.2 MB | 77.2 MB | 502.83× more | — | — | 106.7 MB | 107.4 MB |
| OpenCode | 263.1 MB | 126.4 KB | 1.24× less | — | — | 568.7 MB | 11.6 MB |
| pi | 117.6 MB | 115.9 MB | 754.71× more | — | — | 148.0 MB | 147.9 MB |
| oh-my-pi (omp) | 373.7 MB | 178.9 MB | 1,165× more | — | — | 442.8 MB | 310.8 MB |
| oh-my-openagent (omo) | 57.6 MB | 44.3 MB | 288.45× more | — | — | 816.1 MB | 326.7 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.7 MB | 36.4 MB | 38.1 MB |
| jcode | 39.9 MB | 41.3 MB | 43.2 MB |
| Claude Code | 188.5 MB | 932.8 MB | 1.8 GB |
| Codex CLI | 106.7 MB | 534.6 MB | 1.0 GB |
| OpenCode | 568.7 MB | 598.1 MB | 671.5 MB |
| pi | 148.0 MB | 739.4 MB | 1.4 GB |
| oh-my-pi (omp) | 442.8 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 816.1 MB | 3.0 GB | 3.7 GB |

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    10.14 ms
  Max:    20.53 ms
  Mean:   14.66 ms
  Median: 14.06 ms
  Stdev:  3.32 ms

--version:
  Min:    11.79 ms
  Max:    22.65 ms
  Mean:   16.50 ms
  Median: 16.34 ms
  Stdev:  3.69 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    28.75 ms
  Max:    103.16 ms
  Mean:   65.73 ms
  Median: 57.87 ms
  Stdev:  30.09 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    46.04 ms
  Max:    136.42 ms
  Mean:   89.60 ms
  Median: 91.45 ms
  Stdev:  31.98 ms

Bridge stage: hello_ok:
  Min:    48.48 ms
  Max:    137.71 ms
  Mean:   91.23 ms
  Median: 92.89 ms
  Stdev:  31.56 ms

Bridge stage: no-op reply:
  Min:    49.04 ms
  Max:    138.71 ms
  Mean:   92.09 ms
  Median: 93.73 ms
  Stdev:  31.50 ms

Bridge total (socket -> no-op):
  Min:    49.04 ms
  Max:    138.71 ms
  Mean:   92.09 ms
  Median: 93.73 ms
  Stdev:  31.50 ms

============================================================
Summary:
  Binary load median:      ~14.1 ms
  Server ready median:     ~57.9 ms
  Bridge total median:     ~93.7 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (5f58be645d59)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-cd5j6rkd
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.026/0.023    p95    0.036/0.028    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.247/0.221    p95    0.352/0.258    delta  -0.026 ms  control-no-change
  mcp_config_load   p50    0.283/0.262    p95    0.358/0.432    delta  -0.021 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   17.038/16.319   p95   43.440/28.905   delta  -0.719 ms  control-no-change
  total store_read_bytes p50 22950  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22954 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22958 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.419/4.230    p95    5.473/9.114    delta  -0.188 ms  control-no-change
  skill_request_overlay p50    5.731/5.261    p95    9.902/8.190    delta  -0.471 ms  control-no-change
  mcp_config_load   p50    0.274/0.270    p95    0.883/0.306    delta  -0.004 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   20.786/19.244   p95   52.394/31.830   delta  -1.542 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22930..22954 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22954 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.025    p95    0.042/0.034    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.233/0.242    p95    0.310/0.275    delta  +0.009 ms  control-no-change
  mcp_config_load   p50    0.258/0.267    p95    0.327/0.316    delta  +0.009 ms  control-no-change
  attach_history    p50  292.054/289.960  p95  356.914/341.367  delta  -2.094 ms  control-no-change
  resume_ready      p50   61.983/62.169   p95   73.554/84.288   delta  +0.187 ms  control-no-change
  provider_dispatch p50    1.075/1.105    p95    1.448/1.543    delta  +0.030 ms  control-no-change
  session_save      p50   13.992/17.097   p95   17.295/24.647   delta  +3.105 ms  control-no-change
  total store_read_bytes p50 5311300  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22938..22964 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22932..22968 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.055/4.457    p95    9.558/5.527    delta  +0.402 ms  control-no-change
  skill_request_overlay p50    5.258/5.684    p95    9.174/8.547    delta  +0.426 ms  control-no-change
  mcp_config_load   p50    0.272/0.284    p95    0.776/0.550    delta  +0.012 ms  control-no-change
  attach_history    p50  314.932/292.493  p95  414.941/424.108  delta -22.440 ms  control-no-change
  resume_ready      p50   67.200/66.436   p95   83.476/82.425   delta  -0.764 ms  control-no-change
  provider_dispatch p50    1.066/1.140    p95    1.573/2.169    delta  +0.074 ms  control-no-change
  session_save      p50   16.336/22.202   p95   26.719/39.106   delta  +5.866 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22936..22972 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22968 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.031/0.029    p95    0.038/0.061    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.237/0.244    p95    0.520/0.621    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.263/0.308    p95    0.650/0.451    delta  +0.045 ms  control-no-change
  attach_history    p50 4012.170/4223.070 p95 6245.852/5639.612 delta +210.900 ms  control-no-change
  resume_ready      p50  945.983/920.839  p95 1238.557/1206.673 delta -25.143 ms  control-no-change
  provider_dispatch p50   15.669/13.540   p95   21.579/37.960   delta  -2.129 ms  control-no-change
  session_save      p50   31.159/33.627   p95  164.394/90.924   delta  +2.468 ms  control-no-change
  total store_read_bytes p50 53417515  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22946..22963 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22949..22971 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.220/4.166    p95    5.270/5.074    delta  -0.054 ms  control-no-change
  skill_request_overlay p50    5.345/5.498    p95    7.663/9.194    delta  +0.153 ms  control-no-change
  mcp_config_load   p50    0.271/0.273    p95    0.527/0.547    delta  +0.002 ms  control-no-change
  attach_history    p50 3513.556/3242.818 p95 4541.435/3710.403 delta -270.738 ms  control-no-change
  resume_ready      p50  711.772/642.248  p95  986.116/715.815  delta -69.524 ms  control-no-change
  provider_dispatch p50   10.671/10.331   p95   16.505/16.918   delta  -0.340 ms  control-no-change
  session_save      p50   25.628/21.535   p95  103.860/60.054   delta  -4.093 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184299..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210250..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22945..22967 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184295..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210246..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22967 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (5f58be645d59)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-ew_27mcr
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.026    p95    0.111/0.103    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.237/0.247    p95    0.636/0.699    delta  +0.010 ms  control-no-change
  mcp_config_load   p50    0.257/0.309    p95    0.727/0.741    delta  +0.051 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   20.223/20.027   p95   24.050/44.871   delta  -0.197 ms  control-no-change
  total store_read_bytes p50 23673  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22947, 22955, 22959, 23653, 23669, 23677, 24367, 24375, 24381, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22943, 22951, 23661, 23665, 23673, 24367, 24375, 24379]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22947..24383 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22943..24379 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.598/4.358    p95    9.648/8.400    delta  -0.240 ms  control-no-change
  skill_request_overlay p50    5.984/5.520    p95   11.187/11.434   delta  -0.464 ms  control-no-change
  mcp_config_load   p50    0.300/0.272    p95    0.630/0.444    delta  -0.029 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   18.317/15.363   p95   23.887/27.984   delta  -2.954 ms  control-no-change
  total store_read_bytes p50 23659  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22945, 22949, 22951, 23655, 23663, 23671, 24373, 24377, 24381]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22963, 23665, 23669, 24367, 24375]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24381 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24375 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.048/0.030    p95    0.074/0.081    delta  -0.017 ms  control-no-change
  skill_request_overlay p50    0.269/0.237    p95    0.682/0.701    delta  -0.032 ms  control-no-change
  mcp_config_load   p50    0.280/0.270    p95    1.077/0.659    delta  -0.010 ms  control-no-change
  attach_history    p50  176.182/152.049  p95  229.577/187.450  delta -24.133 ms  control-no-change
  resume_ready      p50   98.123/117.206  p95  157.125/155.287  delta +19.083 ms  control-no-change
  provider_dispatch p50    1.325/1.880    p95    2.311/2.783    delta  +0.556 ms  control-no-change
  session_save      p50   28.396/27.739   p95  102.846/37.484   delta  -0.657 ms  control-no-change
  total store_read_bytes p50 2841255  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23678, 24384, 24388, 25096, 25098, 25810, 25820, 26520, 26526]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23668, 23682, 24386, 24388, 25090, 25096, 25810, 25820, 26520, 26528]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26526 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23668..26528 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    5.157/5.094    p95   12.440/8.027    delta  -0.063 ms  control-no-change
  skill_request_overlay p50    7.459/7.124    p95   32.618/12.509   delta  -0.335 ms  control-no-change
  mcp_config_load   p50    0.311/0.290    p95    0.501/0.531    delta  -0.021 ms  control-no-change
  attach_history    p50  142.978/128.705  p95  166.048/179.937  delta -14.273 ms  control-no-change
  resume_ready      p50   80.734/82.624   p95   97.812/90.858   delta  +1.890 ms  control-no-change
  provider_dispatch p50    1.163/1.169    p95    2.231/1.728    delta  +0.006 ms  control-no-change
  session_save      p50   20.968/21.591   p95   32.917/30.179   delta  +0.624 ms  control-no-change
  total store_read_bytes p50 2841257  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 24380, 24396, 25092, 25106, 25802, 25824, 26524, 26526]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 23678, 24376, 24378, 25090, 25796, 26514, 26534]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26526 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26534 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.029/0.034    p95    0.061/0.086    delta  +0.005 ms  control-no-change
  skill_request_overlay p50    0.227/0.260    p95    0.805/0.630    delta  +0.033 ms  control-no-change
  mcp_config_load   p50    0.299/0.536    p95    0.899/0.981    delta  +0.237 ms  control-drift
  attach_history    p50 1551.646/1753.833 p95 1940.151/2151.198 delta +202.187 ms  control-drift
  resume_ready      p50  949.660/1049.862 p95 1232.501/1161.326 delta +100.202 ms  control-no-change
  provider_dispatch p50   14.152/19.763   p95   19.985/22.433   delta  +5.610 ms  control-no-change
  session_save      p50   26.354/35.015   p95   34.867/95.601   delta  +8.660 ms  control-drift
  total store_read_bytes p50 28640268  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23677, 24375, 24385, 25089, 25097, 25807, 25827, 26525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23675, 23677, 24385, 24387, 25105, 25113, 25807, 25819, 26525, 26541]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26525 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23675..26541 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    5.286/6.783    p95   13.700/13.105   delta  +1.497 ms  control-no-change
  skill_request_overlay p50    7.874/7.527    p95   17.508/12.687   delta  -0.347 ms  control-no-change
  mcp_config_load   p50    0.349/0.281    p95    0.843/0.861    delta  -0.068 ms  control-no-change
  attach_history    p50 2096.630/2034.740 p95 3067.823/2488.227 delta -61.891 ms  control-no-change
  resume_ready      p50 1077.765/1115.195 p95 1595.925/1506.036 delta +37.430 ms  control-no-change
  provider_dispatch p50   16.252/18.067   p95   33.558/25.858   delta  +1.815 ms  control-no-change
  session_save      p50   38.721/47.172   p95   95.950/125.433  delta  +8.451 ms  control-no-change
  total store_read_bytes p50 28640278  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23659, 23673, 24383, 24385, 25101, 25105, 25807, 25811, 26517, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23685, 24387, 24395, 25097, 25099, 25809, 25823, 26523, 26529]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23659..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26529 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.5 ms | 0.6 ms | 0.4 ms | 0.6 ms | 5 |
| read (1000 lines) | 0.8 ms | 1.0 ms | 0.7 ms | 1.0 ms | 5 |
| read (10 lines @ offset) | 0.8 ms | 1.0 ms | 0.6 ms | 1.0 ms | 5 |
| read (repo Cargo.toml) | 0.6 ms | 0.7 ms | 0.4 ms | 0.7 ms | 5 |
| write (small) | 2.6 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| write (10KB) | 3.1 ms | 3.2 ms | 2.6 ms | 3.2 ms | 5 |
| edit (simple replace) | 2.4 ms | 2.9 ms | 1.8 ms | 2.9 ms | 5 |
| ls (scripts dir) | 1.0 ms | 1.8 ms | 0.9 ms | 1.8 ms | 5 |
| agentgrep (grep in tree) | 4.3 ms | 5.3 ms | 4.0 ms | 5.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 2.8 ms | 1.6 ms | 2.8 ms | 5 |
| bash (true) | 4.9 ms | 5.6 ms | 4.4 ms | 5.6 ms | 5 |
| bash (echo hello) | 6.5 ms | 7.9 ms | 5.7 ms | 7.9 ms | 5 |
| bash (ls fixture tree) | 8.8 ms | 10.9 ms | 8.8 ms | 10.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 90.8 ms | 90.8 ms | 90.8 ms | 90.9 ms | 0 |
| 10 | 273.1 ms | 310.2 ms | 310.2 ms | 310.8 ms | 0 |
| 40 | 1.14 s | 1.15 s | 1.16 s | 1.16 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.28 s | 1.28 s | 1.28 s | 1 |
| leaf_touch | src/main.rs | 568.9 ms | 568.9 ms | 568.9 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 27.23 s | 27.23 s | 27.23 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 34.58 s | 34.58 s | 34.58 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (8724ad6) | 12.1 ms | 94.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 13.9 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 36.7 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 88.4 ms | 479.4 MB |
| OpenCode | 1.19.5 | 518.0 ms | 605.3 MB |
| pi | 1.5.0 | 254.3 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 629.5 ms | 291.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 177.1 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 188.5 ms | 178.3 ms–267.0 ms | baseline | 35.3 MB | baseline | yes | — |
| jcode | 134.7 ms | 128.1 ms–327.4 ms | 1.40× less | 31.0 MB | 1.14× less | yes | — |
| Claude Code | 1.22 s | 1.15 s–2.02 s | 6.48× more | 273.2 MB | 7.75× more | yes | — |
| Codex CLI | 1.72 s | 1.54 s–2.16 s | 9.10× more | 258.1 MB | 7.32× more | yes | — |
| OpenCode | 4.53 s | 4.48 s–7.04 s | 24.05× more | 840.9 MB | 23.85× more | yes | — |
| pi | 454.0 ms | 427.6 ms–483.5 ms | 2.41× more | 125.2 MB | 3.55× more | yes | — |
| oh-my-pi (omp) | 3.04 s | 2.93 s–26.95 s | 16.14× more | 414.4 MB | 11.75× more | yes | — |
| oh-my-openagent (omo) | 22.07 s | 16.86 s–30.87 s | 117.07× more | 555.1 MB | 15.74× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 352.2 ms | 249.3 ms–440.0 ms | baseline | 24.9 MB | baseline | skill-uri | yes | — |
| jcode | 209.2 ms | 205.4 ms–239.2 ms | 1.68× less | 24.1 MB | 1.03× less | skill-tool | yes | — |
| Claude Code | 835.2 ms | 807.9 ms–871.0 ms | 2.37× more | 237.6 MB | 9.53× more | skill-tool | yes | — |
| Codex CLI | 2.14 s | 1.97 s–2.93 s | 6.07× more | 264.4 MB | 10.60× more | file-read | yes | — |
| OpenCode | 7.10 s | 6.86 s–7.93 s | 20.16× more | 749.4 MB | 30.05× more | skill-tool | yes | — |
| pi | 686.1 ms | 607.6 ms–914.4 ms | 1.95× more | 97.8 MB | 3.92× more | file-read | yes | — |
| oh-my-pi (omp) | 3.65 s | 3.11 s–4.25 s | 10.38× more | 406.5 MB | 16.30× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.20 s | 18.51 s–24.53 s | 60.18× more | 528.2 MB | 21.18× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 283.7 ms | 279.0 ms–1.02 s | baseline | 37.3 MB | baseline | mcp-call | yes | 197.2 ms | — |
| jcode | 345.1 ms | 342.6 ms–413.0 ms | 1.22× more | 37.1 MB | same | mcp-action | yes | 237.0 ms | — |
| Claude Code | 878.5 ms | 869.2 ms–1.22 s | 3.10× more | 270.4 MB | 7.25× more | mcp-tool | yes | 759.9 ms | — |
| Codex CLI | 2.22 s | 1.86 s–2.67 s | 7.83× more | 275.3 MB | 7.38× more | mcp-tool | yes | 892.0 ms | — |
| OpenCode | 6.96 s | 6.67 s–9.54 s | 24.52× more | 766.4 MB | 20.53× more | mcp-tool | yes | 5.32 s | — |
| pi | 691.4 ms | 682.5 ms–789.2 ms | 2.44× more | 118.4 MB | 3.17× more | mcp-tool | yes | 529.1 ms | — |
| oh-my-pi (omp) | 3.47 s | 3.40 s–3.85 s | 12.24× more | 417.5 MB | 11.18× more | mcp-route | yes | 2.74 s | — |
| oh-my-openagent (omo) | 22.11 s | 19.45 s–30.77 s | 77.92× more | 566.5 MB | 15.18× more | mcp-tool | yes | 18.82 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.6 MB | 145.2 KB | baseline | — | — | 29.7 MB | 400.4 KB |
| jcode | 22.0 MB | 77.5 KB | 1.87× less | — | — | 27.0 MB | 315.3 KB |
| Claude Code | 154.9 MB | 172.9 MB | 1,219× more | — | — | 187.4 MB | 188.5 MB |
| Codex CLI | 126.2 MB | 117.1 MB | 825.67× more | — | — | 165.8 MB | 117.4 MB |
| OpenCode | 245.8 MB | 13.1 MB | 92.69× more | — | — | 434.8 MB | 26.1 MB |
| pi | 90.8 MB | 90.8 MB | 639.96× more | — | — | 101.3 MB | 100.8 MB |
| oh-my-pi (omp) | 205.7 MB | 150.5 MB | 1,062× more | — | — | 338.7 MB | 338.6 MB |
| oh-my-openagent (omo) | 148.0 MB | 131.0 MB | 923.49× more | — | — | 411.2 MB | 459.9 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.7 MB | 32.2 MB | 33.3 MB |
| jcode | 27.0 MB | 28.4 MB | 29.8 MB |
| Claude Code | 187.4 MB | 950.5 MB | 1.8 GB |
| Codex CLI | 165.8 MB | 653.4 MB | 1.2 GB |
| OpenCode | 434.8 MB | 461.6 MB | 663.8 MB |
| pi | 101.3 MB | 505.8 MB | 1008.9 MB |
| oh-my-pi (omp) | 338.7 MB | 1.6 GB | 3.3 GB |
| oh-my-openagent (omo) | 411.2 MB | 2.2 GB | 3.9 GB |

**Errors recorded during measurement**

- oh-my-openagent (omo) @ 10 sessions: turn 2 session 8: Connection error.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 0: Connection error.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 8: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    15.84 ms
  Max:    16.32 ms
  Mean:   16.10 ms
  Median: 16.09 ms
  Stdev:  0.16 ms

--version:
  Min:    15.50 ms
  Max:    16.89 ms
  Mean:   15.97 ms
  Median: 15.84 ms
  Stdev:  0.49 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.58 ms
  Max:    27.65 ms
  Mean:   23.32 ms
  Median: 22.66 ms
  Stdev:  1.55 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~16.1 ms
  Server ready median:     ~22.7 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (1dec058e6ab5)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-_m835xem
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.076/0.071    p95    0.086/0.120    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.557/0.552    p95    0.604/0.974    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.696/0.673    p95    0.766/1.137    delta  -0.023 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.009/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   39.369/39.518   p95   48.990/52.151   delta  +0.149 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22904..22964 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22878..22964 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   10.661/10.674   p95   10.872/12.360   delta  +0.012 ms  control-no-change
  skill_request_overlay p50   29.745/29.774   p95   32.076/35.205   delta  +0.028 ms  control-no-change
  mcp_config_load   p50    0.719/0.709    p95    0.868/0.793    delta  -0.010 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.004    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.014/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   54.963/48.670   p95   62.029/57.922   delta  -6.294 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22894..22960 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22916..22956 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.070/0.070    p95    0.090/0.072    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.553/0.545    p95    0.581/0.608    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.666/0.698    p95    0.740/0.767    delta  +0.032 ms  control-no-change
  attach_history    p50  318.236/313.070  p95  330.835/330.552  delta  -5.167 ms  control-no-change
  resume_ready      p50  101.453/99.255   p95  106.598/103.824  delta  -2.197 ms  control-no-change
  provider_dispatch p50    1.194/1.200    p95    1.732/1.250    delta  +0.006 ms  control-no-change
  session_save      p50   35.138/35.398   p95   37.810/41.076   delta  +0.260 ms  control-no-change
  total store_read_bytes p50 5311215  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22940..22974 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22923..22976 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   11.000/10.812   p95   11.743/12.801   delta  -0.188 ms  control-no-change
  skill_request_overlay p50   29.564/29.931   p95   32.685/35.739   delta  +0.367 ms  control-no-change
  mcp_config_load   p50    0.733/0.726    p95    0.860/1.432    delta  -0.007 ms  control-no-change
  attach_history    p50  307.690/311.737  p95  324.830/321.862  delta  +4.047 ms  control-no-change
  resume_ready      p50   99.850/101.815  p95  103.708/107.870  delta  +1.964 ms  control-no-change
  provider_dispatch p50    1.181/1.144    p95    1.246/1.270    delta  -0.038 ms  control-no-change
  session_save      p50   37.991/38.569   p95   61.852/56.673   delta  +0.579 ms  control-no-change
  total store_read_bytes p50 5311213  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099104..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22931..22976 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22968 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.067/0.068    p95    0.083/0.091    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.524/0.540    p95    0.580/0.580    delta  +0.016 ms  control-no-change
  mcp_config_load   p50    0.683/0.678    p95    0.715/0.735    delta  -0.005 ms  control-no-change
  attach_history    p50 3504.310/3345.711 p95 3816.235/3703.461 delta -158.599 ms  control-no-change
  resume_ready      p50 1055.542/1022.610 p95 1092.154/1117.242 delta -32.932 ms  control-no-change
  provider_dispatch p50   12.876/12.905   p95   15.085/15.255   delta  +0.029 ms  control-no-change
  session_save      p50   33.115/33.970   p95  127.937/291.088  delta  +0.855 ms  control-no-change
  total store_read_bytes p50 53417430  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22935..22987 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22979 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    9.547/9.704    p95   10.698/10.576   delta  +0.157 ms  control-no-change
  skill_request_overlay p50   26.796/26.482   p95   30.348/29.370   delta  -0.314 ms  control-no-change
  mcp_config_load   p50    0.650/0.656    p95    0.732/0.708    delta  +0.006 ms  control-no-change
  attach_history    p50 3327.271/3294.592 p95 3528.120/3540.102 delta -32.679 ms  control-no-change
  resume_ready      p50 1005.018/1005.208 p95 1049.803/1043.810 delta  +0.189 ms  control-no-change
  provider_dispatch p50   12.537/12.349   p95   15.119/14.186   delta  -0.189 ms  control-no-change
  session_save      p50   32.948/32.543   p95   38.367/50.003   delta  -0.404 ms  control-no-change
  total store_read_bytes p50 53417438  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22959..22979 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210214..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22935..22979 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (1dec058e6ab5)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-am2hln65
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.119/0.122    p95    0.149/0.148    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.940/0.904    p95    1.001/0.961    delta  -0.036 ms  control-no-change
  mcp_config_load   p50    1.087/1.128    p95    1.144/1.270    delta  +0.041 ms  control-drift
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   38.041/38.131   p95   41.423/39.397   delta  +0.089 ms  control-no-change
  total store_read_bytes p50 23632  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22899, 22930, 22951, 23630, 23634, 23636, 23647, 24319, 24345]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22908, 22925, 22937, 23618, 23641, 23644, 24328, 24339, 24341, 24351]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22899..24345 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22908..24351 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   18.297/18.115   p95   20.303/19.471   delta  -0.181 ms  control-no-change
  skill_request_overlay p50   49.047/48.832   p95   58.425/58.300   delta  -0.215 ms  control-no-change
  mcp_config_load   p50    1.171/1.177    p95    1.802/1.233    delta  +0.006 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.007/0.003    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   41.541/41.497   p95   84.345/44.777   delta  -0.044 ms  control-no-change
  total store_read_bytes p50 23624  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22952, 22953, 22955, 23620, 23628, 23642, 23653, 24335, 24352]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22942, 22945, 22947, 22950, 23608, 23617, 23627, 23634, 24337, 24339]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24352 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22942..24339 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.117/0.117    p95    0.159/0.143    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.935/0.905    p95    0.951/1.021    delta  -0.029 ms  control-no-change
  mcp_config_load   p50    1.109/1.114    p95    1.191/1.199    delta  +0.005 ms  control-no-change
  attach_history    p50  252.310/250.506  p95  268.361/257.956  delta  -1.804 ms  control-no-change
  resume_ready      p50  186.938/191.066  p95  190.110/205.489  delta  +4.128 ms  control-no-change
  provider_dispatch p50    1.684/1.714    p95    1.762/1.793    delta  +0.030 ms  control-no-change
  session_save      p50   37.361/37.420   p95   38.425/41.932   delta  +0.060 ms  control-no-change
  total store_read_bytes p50 2841110  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23623, 23655, 24346, 24352, 25028, 25061, 25747, 25764, 26438, 26467]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23632, 23667, 24343, 24362, 25035, 25050, 25761, 25770, 26412, 26448]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23623..26467 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23632..26448 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   18.224/18.435   p95   24.341/19.116   delta  +0.211 ms  control-no-change
  skill_request_overlay p50   48.243/48.465   p95   51.008/55.036   delta  +0.221 ms  control-no-change
  mcp_config_load   p50    1.147/1.193    p95    1.483/1.507    delta  +0.046 ms  control-no-change
  attach_history    p50  249.051/245.683  p95  261.179/250.331  delta  -3.368 ms  control-no-change
  resume_ready      p50  187.457/191.208  p95  190.651/199.516  delta  +3.750 ms  control-no-change
  provider_dispatch p50    1.678/1.687    p95    1.749/1.948    delta  +0.009 ms  control-no-change
  session_save      p50   39.342/38.219   p95   42.505/57.633   delta  -1.123 ms  control-no-change
  total store_read_bytes p50 2841112  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23665, 24352, 24353, 25024, 25041, 25741, 25764, 26406, 26430]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23679, 24306, 24364, 25044, 25055, 25725, 25744, 26439, 26440]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26430 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26440 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.118/0.121    p95    0.179/0.180    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.955/0.960    p95    1.330/1.036    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    1.133/1.107    p95    1.592/1.147    delta  -0.025 ms  control-no-change
  attach_history    p50 2608.144/2598.797 p95 2851.005/2717.734 delta  -9.347 ms  control-no-change
  resume_ready      p50 1977.009/1981.329 p95 2640.800/2060.218 delta  +4.320 ms  control-no-change
  provider_dispatch p50   17.800/18.118   p95   19.561/22.160   delta  +0.318 ms  control-no-change
  session_save      p50   38.441/37.771   p95   86.490/215.086  delta  -0.669 ms  control-no-change
  total store_read_bytes p50 28640167  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23630, 23663, 24358, 24369, 25064, 25076, 25755, 25763, 26429, 26470]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23664, 24321, 24360, 25052, 25065, 25757, 25760, 26444, 26461]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23630..26470 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26461 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   18.077/18.325   p95   19.748/18.682   delta  +0.249 ms  control-no-change
  skill_request_overlay p50   48.910/48.454   p95   51.095/52.324   delta  -0.456 ms  control-no-change
  mcp_config_load   p50    1.194/1.223    p95    1.242/1.509    delta  +0.029 ms  control-no-change
  attach_history    p50 2610.457/2617.941 p95 2631.786/2629.029 delta  +7.484 ms  control-no-change
  resume_ready      p50 1981.307/1978.245 p95 1998.206/1990.525 delta  -3.062 ms  control-no-change
  provider_dispatch p50   17.887/17.910   p95   18.548/18.933   delta  +0.023 ms  control-no-change
  session_save      p50   36.887/36.656   p95   45.332/40.567   delta  -0.231 ms  control-no-change
  total store_read_bytes p50 28640153  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23621, 23664, 24359, 24362, 25047, 25065, 25744, 25756, 26455, 26475]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23652, 23678, 24371, 24377, 25051, 25061, 25752, 25764, 26431, 26451]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23621..26475 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23652..26451 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 8.0 ms | 10.5 ms | 7.9 ms | 10.5 ms | 5 |
| write (10KB) | 10.0 ms | 12.2 ms | 10.0 ms | 12.2 ms | 5 |
| edit (simple replace) | 10.0 ms | 10.0 ms | 7.4 ms | 10.0 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.6 ms | 8.0 ms | 7.4 ms | 8.0 ms | 5 |
| agentgrep (find **/*.rs) | 7.3 ms | 7.9 ms | 5.3 ms | 7.9 ms | 5 |
| bash (true) | 10.4 ms | 10.5 ms | 10.0 ms | 10.5 ms | 5 |
| bash (echo hello) | 10.5 ms | 10.6 ms | 9.9 ms | 10.6 ms | 5 |
| bash (ls fixture tree) | 13.0 ms | 14.6 ms | 13.0 ms | 14.6 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 154.0 ms | 154.0 ms | 154.0 ms | 154.1 ms | 0 |
| 10 | 473.2 ms | 476.2 ms | 476.2 ms | 477.0 ms | 0 |
| 40 | 1.40 s | 1.81 s | 1.81 s | 1.82 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 474.5 ms | 474.5 ms | 474.5 ms | 1 |
| leaf_touch | src\main.rs | 671.7 ms | 671.7 ms | 671.7 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 21.57 s | 21.57 s | 21.57 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 29.24 s | 29.24 s | 29.24 s | 1 |

<!-- bench-auto:end -->
