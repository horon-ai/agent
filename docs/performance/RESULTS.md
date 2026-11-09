# Performance: measured results and their limits

Language: **English** | [한국어](RESULTS.ko.md)

This document records the measurements the `agent-runtime-performance-2` plan left on baseline B0 (the commit immediately before the plan started). The plan landed 19 performance commits on `dev`, and the same range holds one `fix(cli)` commit unrelated to this plan that has no bearing on the numbers below. The measured surfaces are the session store's hashes and history pages, the SSE parser, replication export, the result cache's threshold decision, and the attach/detach soak. Every measurement was taken on this machine (Apple M1 Max, debug test profile) by alternating before and after on the same machine.

All raw artifacts live under `.omo/evidence/agent-runtime-performance-2/`, a path ignored by `.gitignore:26`. The plan asked for the key numbers to be copied into the PR bodies, but in practice most PR bodies were one line. So anyone who clones this repository today cannot see a single measurement from this work. This page moves that record somewhere tracked; every number is copied verbatim from the evidence files above.

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

<!-- bench-auto:start commit=7d0726ced806f3e3048e153ee1c33b70a337ff6b -->

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `7d0726ced806f3e3048e153ee1c33b70a337ff6b`
- 측정 시각: 2026-11-09 09:21 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/42586815039> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (7d0726c)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (7d0726c)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (7d0726c)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (7d0726c) | 3.4 ms | 110.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.2 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 8.2 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 37.1 ms | 435.0 MB |
| OpenCode | 1.19.5 | 458.8 ms | 613.4 MB |
| pi | 1.5.0 | 220.8 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 712.1 ms | 412.6 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 114.5 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 145.2 ms | 102.2 ms–808.8 ms | baseline | 38.6 MB | baseline | yes | 14 | — |
| jcode | 143.5 ms | 142.4 ms–151.9 ms | same | 41.6 MB | 1.08× more | yes | 5 | — |
| Claude Code | 442.7 ms | 417.8 ms–451.7 ms | 3.05× more | 204.6 MB | 5.31× more | yes | — | — |
| Codex CLI | 512.9 ms | 470.8 ms–530.6 ms | 3.53× more | 192.3 MB | 4.99× more | yes | 6 | — |
| OpenCode | 2.55 s | 2.52 s–2.82 s | 17.58× more | 567.1 MB | 14.70× more | yes | 4 | — |
| pi | 373.1 ms | 366.6 ms–377.3 ms | 2.57× more | 112.7 MB | 2.92× more | yes | — | — |
| oh-my-pi (omp) | 2.02 s | 1.92 s–2.32 s | 13.88× more | 394.6 MB | 10.23× more | yes | 71 | — |
| oh-my-openagent (omo) | 6.13 s | 6.09 s–6.49 s | 42.23× more | 670.6 MB | 17.39× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 51.8 ms | 51.1 ms–56.2 ms | baseline | 35.3 MB | baseline | skill-uri | yes | — |
| jcode | 56.4 ms | 53.7 ms–74.6 ms | 1.09× more | 41.6 MB | 1.18× more | skill-tool | yes | — |
| Claude Code | 426.2 ms | 422.4 ms–587.6 ms | 8.23× more | 207.4 MB | 5.87× more | skill-tool | yes | — |
| Codex CLI | 574.6 ms | 486.6 ms–748.3 ms | 11.10× more | 192.6 MB | 5.45× more | file-read | yes | — |
| OpenCode | 3.03 s | 3.03 s–3.31 s | 58.60× more | 692.1 MB | 19.59× more | skill-tool | yes | — |
| pi | 488.8 ms | 484.3 ms–505.0 ms | 9.44× more | 125.0 MB | 3.54× more | file-read | yes | — |
| oh-my-pi (omp) | 1.77 s | 1.76 s–1.79 s | 34.11× more | 341.4 MB | 9.66× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.71 s | 6.44 s–6.88 s | 129.61× more | 666.9 MB | 18.87× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 72.6 ms | 71.7 ms–76.9 ms | baseline | 49.4 MB | baseline | mcp-call | yes | 50.3 ms | — |
| jcode | 172.7 ms | 171.3 ms–174.9 ms | 2.38× more | 53.9 MB | 1.09× more | mcp-action | yes | 142.7 ms | — |
| Claude Code | 529.3 ms | 508.9 ms–537.4 ms | 7.29× more | 226.1 MB | 4.58× more | mcp-tool | yes | 387.4 ms | — |
| Codex CLI | 601.8 ms | 494.9 ms–651.0 ms | 8.29× more | 190.7 MB | 3.86× more | mcp-tool | yes | 426.3 ms | — |
| OpenCode | 2.86 s | 2.81 s–2.99 s | 39.45× more | 567.5 MB | 11.48× more | mcp-tool | yes | 2.57 s | — |
| pi | 531.7 ms | 524.6 ms–538.9 ms | 7.32× more | 140.6 MB | 2.84× more | mcp-tool | yes | 477.7 ms | — |
| oh-my-pi (omp) | 2.05 s | 2.03 s–2.07 s | 28.21× more | 393.2 MB | 7.96× more | mcp-route | yes | 1.69 s | — |
| oh-my-openagent (omo) | 6.89 s | 6.78 s–6.90 s | 94.86× more | 661.4 MB | 13.38× more | mcp-tool | yes | 3.49 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 36.2 MB | 169.4 KB | baseline | 0.92 | 1.00 | 43.1 MB | 481.0 KB |
| jcode | 43.2 MB | 26.9 KB | 6.29× less | 0.11 | — | 46.7 MB | 207.3 KB |
| Claude Code | 149.6 MB | 149.4 MB | 902.92× more | — | — | 184.6 MB | 180.9 MB |
| Codex CLI | 129.3 MB | 133.1 MB | 804.26× more | — | — | 161.7 MB | 164.7 MB |
| OpenCode | 243.8 MB | 1.0 MB | 6.05× more | — | — | 452.8 MB | 13.1 MB |
| pi | 107.6 MB | 108.4 MB | 655.37× more | — | — | 117.1 MB | 131.9 MB |
| oh-my-pi (omp) | 333.5 MB | 61.6 MB | 372.21× more | — | — | 363.3 MB | 357.1 MB |
| oh-my-openagent (omo) | 279.1 MB | 34.8 MB | 210.34× more | — | — | 616.1 MB | 588.1 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 43.1 MB | 45.8 MB | 47.4 MB |
| jcode | 46.7 MB | 47.6 MB | 48.5 MB |
| Claude Code | 184.6 MB | 920.5 MB | 1.8 GB |
| Codex CLI | 161.7 MB | 813.8 MB | 1.6 GB |
| OpenCode | 452.8 MB | 495.4 MB | 570.2 MB |
| pi | 117.1 MB | 627.5 MB | 1.3 GB |
| oh-my-pi (omp) | 363.3 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 616.1 MB | 2.9 GB | 5.8 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    4.61 ms
  Max:    4.98 ms
  Mean:   4.77 ms
  Median: 4.76 ms
  Stdev:  0.11 ms

--version:
  Min:    4.30 ms
  Max:    4.57 ms
  Mean:   4.47 ms
  Median: 4.50 ms
  Stdev:  0.10 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.69 ms
  Max:    20.74 ms
  Mean:   20.71 ms
  Median: 20.71 ms
  Stdev:  0.02 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.73 ms
  Max:    20.89 ms
  Mean:   20.77 ms
  Median: 20.76 ms
  Stdev:  0.05 ms

Bridge stage: hello_ok:
  Min:    21.35 ms
  Max:    21.65 ms
  Mean:   21.51 ms
  Median: 21.51 ms
  Stdev:  0.10 ms

Bridge stage: no-op reply:
  Min:    21.84 ms
  Max:    22.11 ms
  Mean:   21.99 ms
  Median: 22.00 ms
  Stdev:  0.09 ms

Bridge total (socket -> no-op):
  Min:    21.84 ms
  Max:    22.11 ms
  Mean:   21.99 ms
  Median: 22.00 ms
  Stdev:  0.09 ms

============================================================
Summary:
  Binary load median:      ~4.8 ms
  Server ready median:     ~20.7 ms
  Bridge total median:     ~22.0 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (38adf43b7e62)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-ou530cvo
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.017/0.016    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.107/0.141    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.143    p95    0.176/0.160    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    8.604/8.865    p95   11.319/10.491   delta  +0.261 ms  control-no-change
  total store_read_bytes p50 23076  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23084 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23080 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.741/2.707    p95    2.823/2.782    delta  -0.034 ms  control-no-change
  skill_request_overlay p50    4.686/4.672    p95    5.023/4.712    delta  -0.015 ms  control-no-change
  mcp_config_load   p50    0.147/0.146    p95    0.161/0.154    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   11.195/10.875   p95   12.449/18.238   delta  -0.319 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23084 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23076 (4 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.015    p95    0.016/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.106/0.106    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.186/0.185    delta  +0.000 ms  control-no-change
  attach_history    p50  295.130/293.480  p95  298.161/301.931  delta  -1.650 ms  control-no-change
  resume_ready      p50   53.160/53.252   p95   53.393/55.390   delta  +0.091 ms  control-no-change
  provider_dispatch p50    1.329/1.330    p95    1.343/1.669    delta  +0.000 ms  control-no-change
  session_save      p50    7.741/7.783    p95    8.157/9.121    delta  +0.041 ms  control-no-change
  total store_read_bytes p50 5311390  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189185..4189189 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099128..1099132 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23065..23081 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189183..4189189 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099128..1099132 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23061..23085 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.722/2.702    p95    2.791/2.796    delta  -0.021 ms  control-no-change
  skill_request_overlay p50    4.709/4.735    p95    4.775/4.783    delta  +0.026 ms  control-no-change
  mcp_config_load   p50    0.147/0.147    p95    0.160/0.149    delta  -0.000 ms  control-no-change
  attach_history    p50  297.325/296.367  p95  303.250/300.368  delta  -0.958 ms  control-no-change
  resume_ready      p50   53.313/53.134   p95   54.189/53.747   delta  -0.179 ms  control-no-change
  provider_dispatch p50    1.344/1.340    p95    1.383/1.375    delta  -0.005 ms  control-no-change
  session_save      p50    7.722/7.707    p95    8.197/7.900    delta  -0.015 ms  control-no-change
  total store_read_bytes p50 5311392  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23054..23082 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189191 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099133 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23066..23086 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.016/0.015    p95    0.035/0.016    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.106/0.105    p95    0.108/0.119    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.143    p95    0.163/0.162    delta  -0.000 ms  control-no-change
  attach_history    p50 3761.642/3765.886 p95 3816.909/3840.970 delta  +4.244 ms  control-no-change
  resume_ready      p50  576.094/579.387  p95  586.687/581.664  delta  +3.293 ms  control-no-change
  provider_dispatch p50   14.552/14.830   p95   14.905/15.565   delta  +0.278 ms  control-no-change
  session_save      p50    7.999/8.028    p95    8.625/9.212    delta  +0.030 ms  control-no-change
  total store_read_bytes p50 53417609  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184281..42184289 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210235..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23059..23085 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23085 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.707/2.705    p95    2.757/2.843    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    4.672/4.668    p95    5.377/4.688    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.146/0.147    p95    0.195/0.154    delta  +0.001 ms  control-no-change
  attach_history    p50 3748.727/3752.105 p95 3769.283/3774.423 delta  +3.378 ms  control-no-change
  resume_ready      p50  575.855/575.244  p95  581.434/579.066  delta  -0.611 ms  control-no-change
  provider_dispatch p50   14.705/14.892   p95   14.960/15.791   delta  +0.187 ms  control-no-change
  session_save      p50    7.991/8.047    p95    8.189/8.868    delta  +0.056 ms  control-no-change
  total store_read_bytes p50 53417611  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23067..23089 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184281..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210235..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23063..23085 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (38adf43b7e62)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-jhr7ne20
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.034/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.104/0.104    p95    0.105/0.105    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.141/0.141    p95    0.158/0.173    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.704/7.699    p95    9.711/8.298    delta  -0.005 ms  control-no-change
  total store_read_bytes p50 23793  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23061, 23073, 23081, 23787, 23791, 23795, 24489, 24505, 24509]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23061, 23069, 23073, 23787, 23791, 23803, 24501, 24505, 24509, 24513]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23061..24509 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23061..24513 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.663/2.685    p95    2.704/2.765    delta  +0.021 ms  control-no-change
  skill_request_overlay p50    4.584/4.619    p95    4.636/4.680    delta  +0.036 ms  control-no-change
  mcp_config_load   p50    0.144/0.145    p95    0.151/0.156    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50    9.432/9.401    p95   10.883/9.747    delta  -0.030 ms  control-no-change
  total store_read_bytes p50 23792  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23069, 23073, 23075, 23077, 23789, 23795, 23797, 24503, 24505, 24513]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23057, 23065, 23081, 23781, 23783, 23795, 24497, 24499, 24505, 24513]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23069..24513 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23057..24513 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.014/0.014    p95    0.016/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.104/0.104    p95    0.148/0.105    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.141/0.141    p95    0.172/0.152    delta  -0.001 ms  control-no-change
  attach_history    p50   96.333/96.112   p95   97.233/96.591   delta  -0.222 ms  control-no-change
  resume_ready      p50   52.208/52.159   p95   52.960/52.304   delta  -0.050 ms  control-no-change
  provider_dispatch p50    1.326/1.314    p95    1.343/1.334    delta  -0.012 ms  control-no-change
  session_save      p50    8.122/8.127    p95    9.338/11.459   delta  +0.005 ms  control-no-change
  total store_read_bytes p50 2841360  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23779, 23795, 24499, 24505, 25221, 25235, 25945, 25955, 26659, 26661]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23773, 23801, 24507, 24519, 25209, 25213, 25225, 25937, 25959, 26649]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23779..26661 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23773..26649 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.735/2.709    p95    2.807/2.753    delta  -0.026 ms  control-no-change
  skill_request_overlay p50    4.650/4.621    p95    4.733/4.686    delta  -0.029 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.156/0.156    delta  -0.001 ms  control-no-change
  attach_history    p50   96.415/97.014   p95   98.342/100.464  delta  +0.599 ms  control-no-change
  resume_ready      p50   52.230/52.093   p95   52.658/52.521   delta  -0.137 ms  control-no-change
  provider_dispatch p50    1.321/1.318    p95    1.377/1.356    delta  -0.003 ms  control-no-change
  session_save      p50    9.010/9.150    p95    9.959/16.185   delta  +0.140 ms  control-no-change
  total store_read_bytes p50 2841364  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23782, 23788, 24498, 24504, 25230, 25928, 25940, 26666]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23769, 23795, 24503, 24509, 25221, 25229, 25951, 25955, 26649, 26661]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23782..26666 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23769..26661 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.037/0.016    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.104/0.104    p95    0.107/0.104    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.140/0.140    p95    0.158/0.172    delta  -0.000 ms  control-no-change
  attach_history    p50 1059.256/1058.931 p95 1066.858/1066.415 delta  -0.326 ms  control-no-change
  resume_ready      p50  569.971/568.144  p95  578.455/577.230  delta  -1.827 ms  control-no-change
  provider_dispatch p50   14.346/14.460   p95   14.449/14.889   delta  +0.114 ms  control-no-change
  session_save      p50    7.767/7.825    p95    7.891/9.487    delta  +0.057 ms  control-no-change
  total store_read_bytes p50 28640385  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23786, 23798, 24514, 24516, 25234, 25238, 25952, 25956, 26666, 26678]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23772, 23794, 24508, 24514, 25214, 25226, 25942, 25952, 26668, 26678]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23786..26678 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23772..26678 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.647/2.664    p95    2.764/2.717    delta  +0.017 ms  control-no-change
  skill_request_overlay p50    4.603/4.637    p95    4.799/4.709    delta  +0.034 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.151/0.156    delta  +0.001 ms  control-no-change
  attach_history    p50 1063.130/1065.083 p95 1081.297/1085.455 delta  +1.953 ms  control-no-change
  resume_ready      p50  571.914/574.140  p95  576.061/583.307  delta  +2.226 ms  control-no-change
  provider_dispatch p50   14.168/14.171   p95   14.551/14.680   delta  +0.003 ms  control-no-change
  session_save      p50    9.281/9.453    p95   10.255/10.225   delta  +0.172 ms  control-no-change
  total store_read_bytes p50 28640383  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23791, 24501, 24521, 25225, 25239, 25939, 25941, 26665, 26667]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23791, 23793, 24511, 24519, 25231, 25233, 25931, 25957, 26667, 26677]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23791..26667 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23791..26677 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.2 ms | 0.2 ms | 0.2 ms | 0.2 ms | 5 |
| write (small) | 1.0 ms | 1.1 ms | 0.9 ms | 1.1 ms | 5 |
| write (10KB) | 1.1 ms | 1.2 ms | 1.0 ms | 1.2 ms | 5 |
| edit (simple replace) | 1.0 ms | 1.0 ms | 1.0 ms | 1.0 ms | 5 |
| ls (scripts dir) | 0.5 ms | 0.7 ms | 0.4 ms | 0.7 ms | 5 |
| agentgrep (grep in tree) | 1.2 ms | 1.3 ms | 1.1 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.0 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.5 ms | 1.5 ms | 1.5 ms | 1.5 ms | 5 |
| bash (echo hello) | 1.5 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| bash (ls fixture tree) | 2.6 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 41.2 ms | 41.2 ms | 41.2 ms | 41.3 ms | 0 |
| 10 | 131.1 ms | 192.8 ms | 192.8 ms | 193.1 ms | 0 |
| 40 | 619.7 ms | 663.0 ms | 663.9 ms | 664.3 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 228.8 ms | 228.8 ms | 228.8 ms | 1 |
| leaf_touch | src/main.rs | 353.5 ms | 353.5 ms | 353.5 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 17.11 s | 17.11 s | 17.11 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 23.72 s | 23.72 s | 23.72 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (7d0726c) | 14.0 ms | 92.9 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 14.0 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 28.9 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 81.9 ms | 346.5 MB |
| OpenCode | 1.19.5 | 533.8 ms | 344.6 MB |
| pi | 1.5.0 | 333.7 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 778.8 ms | 273.0 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 221.8 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 124.7 ms | 105.7 ms–568.6 ms | baseline | 28.0 MB | baseline | yes | — |
| jcode | 461.9 ms | 385.7 ms–713.5 ms | 3.70× more | 34.6 MB | 1.23× more | yes | — |
| Claude Code | 779.2 ms | 677.2 ms–2.62 s | 6.25× more | 192.5 MB | 6.87× more | yes | — |
| Codex CLI | 500.4 ms | 455.6 ms–1.49 s | 4.01× more | 143.2 MB | 5.11× more | yes | — |
| OpenCode | 4.28 s | 3.25 s–6.84 s | 34.35× more | 781.6 MB | 27.88× more | yes | — |
| pi | 592.9 ms | 565.7 ms–625.5 ms | 4.76× more | 118.8 MB | 4.24× more | yes | — |
| oh-my-pi (omp) | 2.24 s | 2.22 s–2.69 s | 18.00× more | 407.6 MB | 14.54× more | yes | — |
| oh-my-openagent (omo) | 10.91 s | 10.54 s–11.31 s | 87.47× more | 863.5 MB | 30.81× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 110.9 ms | 93.3 ms–156.1 ms | baseline | 22.5 MB | baseline | skill-uri | yes | — |
| jcode | 385.8 ms | 332.3 ms–574.0 ms | 3.48× more | 27.5 MB | 1.22× more | skill-tool | yes | — |
| Claude Code | 513.4 ms | 498.4 ms–555.7 ms | 4.63× more | 194.2 MB | 8.65× more | skill-tool | yes | — |
| Codex CLI | 551.6 ms | 497.8 ms–606.4 ms | 4.97× more | 142.9 MB | 6.36× more | file-read | yes | — |
| OpenCode | 4.02 s | 3.67 s–4.24 s | 36.28× more | 678.5 MB | 30.20× more | skill-tool | yes | — |
| pi | 718.9 ms | 710.3 ms–766.4 ms | 6.48× more | 132.2 MB | 5.89× more | file-read | yes | — |
| oh-my-pi (omp) | 2.34 s | 2.14 s–3.39 s | 21.07× more | 413.2 MB | 18.39× more | file-read | yes | — |
| oh-my-openagent (omo) | 10.38 s | 10.23 s–14.52 s | 93.56× more | 896.9 MB | 39.92× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 166.3 ms | 148.5 ms–215.2 ms | baseline | 31.2 MB | baseline | mcp-call | yes | 128.6 ms | — |
| jcode | 479.3 ms | 465.5 ms–514.6 ms | 2.88× more | 43.5 MB | 1.39× more | mcp-action | yes | 442.4 ms | — |
| Claude Code | 707.0 ms | 649.0 ms–773.6 ms | 4.25× more | 221.2 MB | 7.09× more | mcp-tool | yes | 554.2 ms | — |
| Codex CLI | 474.2 ms | 456.6 ms–520.1 ms | 2.85× more | 142.9 MB | 4.58× more | mcp-tool | yes | 300.5 ms | — |
| OpenCode | 4.05 s | 3.66 s–4.30 s | 24.34× more | 709.6 MB | 22.73× more | mcp-tool | yes | 3.29 s | — |
| pi | 749.8 ms | 698.9 ms–864.0 ms | 4.51× more | 147.6 MB | 4.73× more | mcp-tool | yes | 608.6 ms | — |
| oh-my-pi (omp) | 2.52 s | 2.30 s–2.69 s | 15.15× more | 408.3 MB | 13.08× more | mcp-route | yes | 2.17 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | no | — | failed: no tool that could reach the fixture was offered (one request, no tool call) |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.5 MB | 155.1 KB | baseline | 0.45 | 1.00 | 34.2 MB | 388.1 KB |
| jcode | 36.5 MB | 190.0 KB | 1.22× more | — | — | 40.1 MB | 324.9 KB |
| Claude Code | 157.5 MB | 151.8 MB | 1,002× more | — | — | 187.3 MB | 183.2 MB |
| Codex CLI | 113.4 MB | 82.8 MB | 546.24× more | — | — | 106.9 MB | 107.4 MB |
| OpenCode | 262.4 MB | 456.4 KB | 2.94× more | — | — | 575.6 MB | 11.6 MB |
| pi | 119.0 MB | 115.5 MB | 762.02× more | — | — | 148.5 MB | 148.0 MB |
| oh-my-pi (omp) | 369.9 MB | 93.9 MB | 619.62× more | — | — | 435.0 MB | 317.8 MB |
| oh-my-openagent (omo) | 57.6 MB | 41.6 MB | 274.49× more | — | — | 812.5 MB | 315.9 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.2 MB | 35.9 MB | 37.7 MB |
| jcode | 40.1 MB | 41.4 MB | 43.0 MB |
| Claude Code | 187.3 MB | 927.5 MB | 1.8 GB |
| Codex CLI | 106.9 MB | 534.2 MB | 1.0 GB |
| OpenCode | 575.6 MB | 599.3 MB | 678.1 MB |
| pi | 148.5 MB | 740.8 MB | 1.4 GB |
| oh-my-pi (omp) | 435.0 MB | 1.9 GB | 3.2 GB |
| oh-my-openagent (omo) | 812.5 MB | 3.1 GB | 3.6 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    8.65 ms
  Max:    16.71 ms
  Mean:   11.31 ms
  Median: 10.44 ms
  Stdev:  2.59 ms

--version:
  Min:    7.99 ms
  Max:    12.16 ms
  Mean:   9.11 ms
  Median: 8.58 ms
  Stdev:  1.30 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    24.97 ms
  Max:    100.15 ms
  Mean:   58.84 ms
  Median: 65.20 ms
  Stdev:  27.67 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    34.98 ms
  Max:    84.15 ms
  Mean:   55.64 ms
  Median: 52.51 ms
  Stdev:  16.92 ms

Bridge stage: hello_ok:
  Min:    35.99 ms
  Max:    85.35 ms
  Mean:   56.81 ms
  Median: 53.63 ms
  Stdev:  17.18 ms

Bridge stage: no-op reply:
  Min:    36.69 ms
  Max:    85.90 ms
  Mean:   57.49 ms
  Median: 54.53 ms
  Stdev:  17.22 ms

Bridge total (socket -> no-op):
  Min:    36.69 ms
  Max:    85.90 ms
  Mean:   57.49 ms
  Median: 54.53 ms
  Stdev:  17.22 ms

============================================================
Summary:
  Binary load median:      ~10.4 ms
  Server ready median:     ~65.2 ms
  Bridge total median:     ~54.5 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (21a2eee196a1)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-m1mqsf1w
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.026/0.025    p95    0.098/0.030    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.248/0.230    p95    0.445/0.440    delta  -0.018 ms  control-no-change
  mcp_config_load   p50    0.273/0.261    p95    0.460/0.520    delta  -0.012 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   20.905/20.853   p95   38.790/35.210   delta  -0.052 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22958 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22954 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.567/4.384    p95    5.872/5.094    delta  -0.182 ms  control-no-change
  skill_request_overlay p50    5.611/5.620    p95    7.288/6.732    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.273/0.291    p95    0.546/0.444    delta  +0.019 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   20.786/20.785   p95  150.313/23.829   delta  -0.001 ms  control-no-change
  total store_read_bytes p50 22950  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22938..22962 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22966 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.031/0.031    p95    0.191/0.060    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.249/0.280    p95    0.744/0.510    delta  +0.031 ms  control-no-change
  mcp_config_load   p50    0.276/0.384    p95    0.356/0.730    delta  +0.108 ms  control-drift
  attach_history    p50  414.603/395.407  p95  526.314/538.517  delta -19.196 ms  control-no-change
  resume_ready      p50   89.099/78.907   p95  144.675/128.004  delta -10.192 ms  control-no-change
  provider_dispatch p50    1.260/1.249    p95    3.395/2.894    delta  -0.012 ms  control-no-change
  session_save      p50   22.547/22.727   p95   80.807/36.025   delta  +0.180 ms  control-no-change
  total store_read_bytes p50 5311298  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189201..4189205 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099142..1099144 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22939..22965 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189201..4189205 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099140..1099144 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22970 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    5.218/5.922    p95    8.679/10.125   delta  +0.705 ms  control-no-change
  skill_request_overlay p50    8.021/8.209    p95   13.065/13.036   delta  +0.188 ms  control-no-change
  mcp_config_load   p50    0.299/0.308    p95    0.706/0.731    delta  +0.009 ms  control-no-change
  attach_history    p50  421.469/415.380  p95  525.872/481.927  delta  -6.090 ms  control-no-change
  resume_ready      p50   85.487/91.337   p95  116.152/108.471  delta  +5.850 ms  control-no-change
  provider_dispatch p50    1.042/1.035    p95    2.663/1.965    delta  -0.007 ms  control-no-change
  session_save      p50   33.615/30.313   p95   60.270/40.760   delta  -3.302 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22952..22964 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189202..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099140..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22968 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.031/0.034    p95    0.075/0.085    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.237/0.247    p95    0.579/0.562    delta  +0.010 ms  control-no-change
  mcp_config_load   p50    0.271/0.296    p95    0.673/1.233    delta  +0.025 ms  control-no-change
  attach_history    p50 4419.265/4029.742 p95 4896.619/4696.117 delta -389.524 ms  control-no-change
  resume_ready      p50  976.880/807.793  p95 1154.583/1083.100 delta -169.088 ms  control-no-change
  provider_dispatch p50   13.986/13.020   p95   20.720/19.375   delta  -0.966 ms  control-no-change
  session_save      p50   29.915/26.338   p95   48.788/89.898   delta  -3.577 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22967 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22963 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.431/4.445    p95    9.833/5.730    delta  +0.014 ms  control-no-change
  skill_request_overlay p50    6.324/6.931    p95    9.009/10.016   delta  +0.607 ms  control-no-change
  mcp_config_load   p50    0.309/0.284    p95    0.349/0.711    delta  -0.025 ms  control-no-change
  attach_history    p50 4063.759/3829.454 p95 5229.736/5541.719 delta -234.306 ms  control-no-change
  resume_ready      p50  802.250/926.952  p95 1041.907/1139.913 delta +124.702 ms  control-no-change
  provider_dispatch p50   12.387/14.359   p95   17.812/21.631   delta  +1.973 ms  control-no-change
  session_save      p50   23.651/32.046   p95   54.690/56.621   delta  +8.395 ms  control-no-change
  total store_read_bytes p50 53417521  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184297..42184305 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210247..11210255 (4 distinct values)
  SPREAD   a session_save store_read_bytes 22951..22975 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210250..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22966 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (21a2eee196a1)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-120n472e
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.030/0.031    p95    0.171/0.100    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.252/0.263    p95    1.198/0.887    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.267/0.267    p95    0.852/0.822    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.003    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   26.595/27.208   p95   45.429/52.093   delta  +0.613 ms  control-no-change
  total store_read_bytes p50 23659  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22929, 22939, 22959, 23643, 23657, 23661, 24367, 24373, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22945, 22947, 22955, 23651, 23653, 23657, 24365, 24369, 24371, 24375]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22929..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22945..24375 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    6.144/5.394    p95   10.113/8.598    delta  -0.749 ms  control-no-change
  skill_request_overlay p50    7.810/7.286    p95   11.607/11.219   delta  -0.524 ms  control-no-change
  mcp_config_load   p50    0.374/0.341    p95    0.743/0.731    delta  -0.033 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   22.529/23.864   p95   24.198/52.258   delta  +1.335 ms  control-no-change
  total store_read_bytes p50 23660  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22951, 22955, 23659, 23661, 23667, 24371, 24373, 24375]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22943, 22947, 23657, 23673, 24373, 24375, 24379, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24375 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22943..24383 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.030/0.032    p95    0.109/0.124    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.311/0.245    p95    0.847/0.621    delta  -0.066 ms  control-no-change
  mcp_config_load   p50    0.309/0.423    p95    0.903/0.761    delta  +0.114 ms  control-no-change
  attach_history    p50  157.085/151.632  p95  193.768/169.966  delta  -5.453 ms  control-no-change
  resume_ready      p50   95.772/102.007  p95  129.133/109.834  delta  +6.235 ms  control-no-change
  provider_dispatch p50    1.624/1.187    p95    2.465/3.556    delta  -0.438 ms  control-no-change
  session_save      p50   21.382/23.814   p95   82.984/46.612   delta  +2.432 ms  control-no-change
  total store_read_bytes p50 2841252  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23668, 23670, 24374, 24388, 25092, 25096, 25794, 25806, 26524, 26536]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23648, 23682, 24382, 24388, 25102, 25110, 25808, 25812, 26522, 26526]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23668..26536 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23648..26526 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.675/5.184    p95    5.571/6.316    delta  +0.509 ms  control-no-change
  skill_request_overlay p50    6.007/6.548    p95    8.490/8.053    delta  +0.541 ms  control-no-change
  mcp_config_load   p50    0.285/0.302    p95    0.325/0.435    delta  +0.017 ms  control-no-change
  attach_history    p50  116.520/122.846  p95  269.779/136.930  delta  +6.325 ms  control-no-change
  resume_ready      p50   73.821/65.162   p95   90.779/82.493   delta  -8.659 ms  control-no-change
  provider_dispatch p50    1.185/1.042    p95    2.083/1.531    delta  -0.144 ms  control-no-change
  session_save      p50   21.278/20.559   p95   28.706/27.974   delta  -0.719 ms  control-no-change
  total store_read_bytes p50 2841253  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 24375, 25093, 25101, 25801, 25815, 26523, 26525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23667, 23671, 24371, 24385, 25099, 25105, 25803, 25805, 26515, 26521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26525 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23667..26521 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.032/0.029    p95    0.078/0.048    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.236/0.237    p95    0.485/0.483    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.260/0.267    p95    0.571/0.630    delta  +0.007 ms  control-no-change
  attach_history    p50 1332.204/1497.659 p95 1893.158/1991.260 delta +165.455 ms  control-no-change
  resume_ready      p50  853.466/923.522  p95  995.550/1035.089 delta +70.057 ms  control-no-change
  provider_dispatch p50   15.561/12.294   p95   18.628/17.552   delta  -3.267 ms  control-no-change
  session_save      p50   24.425/29.658   p95   86.839/104.750  delta  +5.233 ms  control-no-change
  total store_read_bytes p50 28640279  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23673, 23681, 24375, 24389, 25095, 25113, 25789, 25817, 26519, 26527]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23677, 23681, 24385, 24395, 25099, 25103, 25809, 25821, 26515, 26527]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23673..26527 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23677..26527 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.820/4.760    p95    7.928/8.378    delta  -0.060 ms  control-no-change
  skill_request_overlay p50    6.682/5.989    p95    9.995/8.883    delta  -0.693 ms  control-no-change
  mcp_config_load   p50    0.288/0.265    p95    0.486/0.439    delta  -0.022 ms  control-no-change
  attach_history    p50 1271.835/1243.592 p95 1429.288/1619.288 delta -28.242 ms  control-no-change
  resume_ready      p50  729.483/721.796  p95  827.481/850.027  delta  -7.688 ms  control-no-change
  provider_dispatch p50   10.538/11.176   p95   14.000/15.336   delta  +0.639 ms  control-no-change
  session_save      p50   21.156/21.492   p95   25.695/32.783   delta  +0.335 ms  control-no-change
  total store_read_bytes p50 28640269  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23673, 24385, 24387, 25087, 25101, 25811, 25829, 26529, 26531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23665, 23673, 24391, 24395, 25099, 25101, 25803, 25809, 26519, 26521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23665..26521 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.5 ms | 0.6 ms | 0.5 ms | 0.6 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.5 ms | 0.5 ms | 0.5 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 1.7 ms | 1.9 ms | 1.5 ms | 1.9 ms | 5 |
| write (10KB) | 1.9 ms | 2.0 ms | 1.9 ms | 2.0 ms | 5 |
| edit (simple replace) | 1.5 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| ls (scripts dir) | 0.7 ms | 0.7 ms | 0.7 ms | 0.7 ms | 5 |
| agentgrep (grep in tree) | 2.4 ms | 2.4 ms | 2.2 ms | 2.4 ms | 5 |
| agentgrep (find **/*.rs) | 1.3 ms | 1.3 ms | 1.3 ms | 1.3 ms | 5 |
| bash (true) | 3.9 ms | 4.2 ms | 3.7 ms | 4.2 ms | 5 |
| bash (echo hello) | 3.9 ms | 4.2 ms | 3.7 ms | 4.2 ms | 5 |
| bash (ls fixture tree) | 7.0 ms | 7.5 ms | 6.5 ms | 7.5 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 70.7 ms | 70.7 ms | 70.7 ms | 70.9 ms | 0 |
| 10 | 330.2 ms | 335.5 ms | 335.5 ms | 336.1 ms | 0 |
| 40 | 1.33 s | 1.34 s | 1.34 s | 1.34 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 3.68 s | 3.68 s | 3.68 s | 1 |
| leaf_touch | src/main.rs | 1.01 s | 1.01 s | 1.01 s | 1 |
| core_touch | crates/app-core/src/lib.rs | 31.10 s | 31.10 s | 31.10 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 37.58 s | 37.58 s | 37.58 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (7d0726c) | 16.5 ms | 92.6 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 18.8 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 51.0 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 113.7 ms | 479.4 MB |
| OpenCode | 1.19.5 | 612.2 ms | 605.3 MB |
| pi | 1.5.0 | 327.4 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 792.9 ms | 291.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 218.4 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 289.5 ms | 241.2 ms–481.0 ms | baseline | 35.8 MB | baseline | yes | — |
| jcode | 184.1 ms | 160.5 ms–228.7 ms | 1.57× less | 22.8 MB | 1.57× less | yes | — |
| Claude Code | 1.56 s | 1.53 s–2.25 s | 5.38× more | 269.6 MB | 7.52× more | yes | — |
| Codex CLI | 1.56 s | 1.54 s–2.06 s | 5.39× more | 267.2 MB | 7.46× more | yes | — |
| OpenCode | 5.99 s | 5.74 s–9.85 s | 20.69× more | 772.9 MB | 21.56× more | yes | — |
| pi | 596.9 ms | 572.2 ms–623.4 ms | 2.06× more | 120.1 MB | 3.35× more | yes | — |
| oh-my-pi (omp) | 3.30 s | 2.97 s–3.51 s | 11.41× more | 402.7 MB | 11.23× more | yes | — |
| oh-my-openagent (omo) | 24.60 s | 20.84 s–36.40 s | 84.97× more | 545.6 MB | 15.22× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 246.3 ms | 239.6 ms–254.3 ms | baseline | 24.8 MB | baseline | skill-uri | yes | — |
| jcode | 236.8 ms | 204.7 ms–247.5 ms | 1.04× less | 22.8 MB | 1.09× less | skill-tool | yes | — |
| Claude Code | 1.11 s | 1.09 s–1.12 s | 4.52× more | 238.1 MB | 9.59× more | skill-tool | yes | — |
| Codex CLI | 1.98 s | 1.88 s–1.98 s | 8.03× more | 265.5 MB | 10.69× more | file-read | yes | — |
| OpenCode | 9.29 s | 8.85 s–9.35 s | 37.73× more | 768.1 MB | 30.94× more | skill-tool | yes | — |
| pi | 747.7 ms | 732.5 ms–750.0 ms | 3.04× more | 97.8 MB | 3.94× more | file-read | yes | — |
| oh-my-pi (omp) | 4.01 s | 3.89 s–4.19 s | 16.28× more | 404.4 MB | 16.29× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.44 s | 20.20 s–23.28 s | 87.05× more | 519.9 MB | 20.94× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 278.4 ms | 277.6 ms–287.5 ms | baseline | 37.0 MB | baseline | mcp-call | yes | 165.1 ms | — |
| jcode | 357.4 ms | 317.0 ms–373.8 ms | 1.28× more | 37.1 MB | same | mcp-action | yes | 236.4 ms | — |
| Claude Code | 1.17 s | 1.17 s–1.19 s | 4.20× more | 277.4 MB | 7.51× more | mcp-tool | yes | 992.5 ms | — |
| Codex CLI | 972.0 ms | 871.9 ms–1.94 s | 3.49× more | 179.9 MB | 4.87× more | mcp-tool | yes | 701.7 ms | — |
| OpenCode | 8.88 s | 8.84 s–9.55 s | 31.91× more | 755.3 MB | 20.44× more | mcp-tool | yes | 7.11 s | — |
| pi | 828.7 ms | 811.7 ms–829.5 ms | 2.98× more | 118.5 MB | 3.21× more | mcp-tool | yes | 680.6 ms | — |
| oh-my-pi (omp) | 4.07 s | 3.81 s–4.24 s | 14.62× more | 404.1 MB | 10.93× more | mcp-route | yes | 3.67 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.5 MB | 159.3 KB | baseline | — | — | 29.3 MB | 388.9 KB |
| jcode | 22.0 MB | 90.7 KB | 1.76× less | — | — | 26.4 MB | 412.9 KB |
| Claude Code | 154.7 MB | 151.9 MB | 976.19× more | — | — | 188.0 MB | 190.8 MB |
| Codex CLI | 124.6 MB | 96.9 MB | 623.04× more | — | — | 165.2 MB | 116.7 MB |
| OpenCode | 245.0 MB | 17.6 MB | 112.87× more | — | — | 428.5 MB | 25.6 MB |
| pi | 90.6 MB | 90.8 MB | 583.59× more | — | — | 100.8 MB | 100.9 MB |
| oh-my-pi (omp) | 303.7 MB | 107.3 MB | 689.62× more | — | — | 337.4 MB | 335.9 MB |
| oh-my-openagent (omo) | 142.2 MB | 131.9 MB | 847.89× more | — | — | 410.4 MB | 458.4 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.3 MB | 32.2 MB | 32.8 MB |
| jcode | 26.4 MB | 28.7 MB | 30.1 MB |
| Claude Code | 188.0 MB | 939.7 MB | 1.9 GB |
| Codex CLI | 165.2 MB | 609.7 MB | 1.2 GB |
| OpenCode | 428.5 MB | 449.0 MB | 652.7 MB |
| pi | 100.8 MB | 505.3 MB | 1009.2 MB |
| oh-my-pi (omp) | 337.4 MB | 1.6 GB | 3.3 GB |
| oh-my-openagent (omo) | 410.4 MB | 2.2 GB | 3.9 GB |

**Errors recorded during measurement**

- oh-my-openagent (omo) @ 10 sessions: turn 3 session 6: Connection error.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 8: Connection error.

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    14.46 ms
  Max:    15.31 ms
  Mean:   14.81 ms
  Median: 14.71 ms
  Stdev:  0.28 ms

--version:
  Min:    13.94 ms
  Max:    14.76 ms
  Mean:   14.27 ms
  Median: 14.20 ms
  Stdev:  0.25 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.71 ms
  Max:    45.40 ms
  Mean:   27.32 ms
  Median: 23.09 ms
  Stdev:  7.68 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~14.7 ms
  Server ready median:     ~23.1 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (934eebd7c4a3)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-510pkfze
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.079/0.079    p95    0.092/0.090    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.585/0.586    p95    0.632/0.679    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.779/0.753    p95    1.005/0.832    delta  -0.025 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   50.408/47.936   p95   66.467/66.429   delta  -2.472 ms  control-no-change
  total store_read_bytes p50 22940  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22906..22956 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22964 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   12.654/12.701   p95   14.076/15.432   delta  +0.047 ms  control-no-change
  skill_request_overlay p50   32.956/32.233   p95   38.304/32.536   delta  -0.723 ms  control-no-change
  mcp_config_load   p50    0.835/0.828    p95    0.891/0.872    delta  -0.007 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   72.638/58.340   p95  324.833/200.922  delta -14.298 ms  control-no-change
  total store_read_bytes p50 22932  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22952 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22964 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.079/0.079    p95    0.088/0.086    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.582/0.627    p95    0.638/0.696    delta  +0.046 ms  control-drift
  mcp_config_load   p50    0.783/0.768    p95    0.809/0.833    delta  -0.014 ms  control-no-change
  attach_history    p50  396.045/401.237  p95  407.501/408.785  delta  +5.192 ms  control-no-change
  resume_ready      p50  122.459/123.212  p95  145.076/126.153  delta  +0.753 ms  control-no-change
  provider_dispatch p50    1.387/1.438    p95    1.469/1.473    delta  +0.051 ms  control-no-change
  session_save      p50   47.496/46.066   p95  103.584/61.913   delta  -1.430 ms  control-no-change
  total store_read_bytes p50 5311205  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22904..22964 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22931..22968 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   12.147/11.566   p95   12.387/12.226   delta  -0.580 ms  control-drift
  skill_request_overlay p50   31.608/32.039   p95   36.877/36.051   delta  +0.431 ms  control-no-change
  mcp_config_load   p50    0.804/0.828    p95    0.841/0.879    delta  +0.024 ms  control-no-change
  attach_history    p50  397.043/397.387  p95  492.980/450.931  delta  +0.344 ms  control-no-change
  resume_ready      p50  127.676/121.642  p95  136.180/123.610  delta  -6.034 ms  control-no-change
  provider_dispatch p50    1.359/1.323    p95    1.424/1.753    delta  -0.036 ms  control-no-change
  session_save      p50   51.959/56.933   p95  103.042/76.933   delta  +4.974 ms  control-no-change
  total store_read_bytes p50 5311214  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22924..22976 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22939..22984 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.079/0.080    p95    0.092/0.111    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.650/0.649    p95    0.736/1.154    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.765/0.764    p95    0.890/1.331    delta  -0.000 ms  control-no-change
  attach_history    p50 4659.331/4674.706 p95 5210.793/4794.073 delta +15.375 ms  control-no-change
  resume_ready      p50 1310.327/1312.874 p95 1354.155/1334.292 delta  +2.548 ms  control-no-change
  provider_dispatch p50   15.001/14.988   p95   16.274/15.237   delta  -0.014 ms  control-no-change
  session_save      p50   48.040/47.150   p95   76.752/433.371  delta  -0.890 ms  control-no-change
  total store_read_bytes p50 53417435  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22920..22987 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22975 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   12.064/12.069   p95   12.626/15.236   delta  +0.005 ms  control-no-change
  skill_request_overlay p50   32.047/32.273   p95   33.310/36.356   delta  +0.226 ms  control-no-change
  mcp_config_load   p50    0.833/0.840    p95    0.927/1.041    delta  +0.007 ms  control-no-change
  attach_history    p50 4678.186/4674.549 p95 4719.880/4716.489 delta  -3.637 ms  control-no-change
  resume_ready      p50 1299.225/1292.846 p95 1323.220/1322.051 delta  -6.379 ms  control-no-change
  provider_dispatch p50   14.828/14.946   p95   15.209/17.105   delta  +0.118 ms  control-no-change
  session_save      p50   45.153/47.857   p95   61.944/56.583   delta  +2.704 ms  control-no-change
  total store_read_bytes p50 53417424  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22903..22987 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22967 (8 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (934eebd7c4a3)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-fnaikcyb
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.107/0.102    p95    0.136/0.126    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.756/0.733    p95    0.814/0.832    delta  -0.023 ms  control-no-change
  mcp_config_load   p50    0.880/0.902    p95    0.915/0.953    delta  +0.022 ms  control-no-change
  attach_history    p50    0.000/0.003    p95    0.003/0.008    delta  +0.003 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   38.898/39.156   p95  181.536/49.585   delta  +0.258 ms  control-no-change
  total store_read_bytes p50 23646  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22921, 22931, 22945, 23621, 23639, 23652, 24313, 24318, 24342, 24351]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22947, 22949, 22957, 23654, 23656, 23660, 24329, 24331, 24339]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22921..24351 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22947..24339 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   14.316/14.174   p95   14.767/17.158   delta  -0.142 ms  control-no-change
  skill_request_overlay p50   37.020/37.145   p95   46.263/40.141   delta  +0.125 ms  control-no-change
  mcp_config_load   p50    0.928/0.954    p95    1.282/1.113    delta  +0.026 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   49.797/48.192   p95  109.444/159.467  delta  -1.604 ms  control-no-change
  total store_read_bytes p50 23650  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22925, 22953, 22961, 23648, 23652, 24351, 24355, 24359]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22953, 22963, 22977, 23634, 23640, 23656, 24323, 24333, 24339, 24351]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22925..24359 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22953..24351 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.101/0.097    p95    0.162/0.154    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.763/0.775    p95    1.073/1.146    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.892/0.929    p95    0.964/1.329    delta  +0.038 ms  control-no-change
  attach_history    p50  196.026/197.667  p95  220.204/212.984  delta  +1.641 ms  control-no-change
  resume_ready      p50  152.684/150.372  p95  159.355/154.234  delta  -2.312 ms  control-no-change
  provider_dispatch p50    1.484/1.486    p95    1.634/1.558    delta  +0.002 ms  control-no-change
  session_save      p50   37.897/40.768   p95  333.350/220.663  delta  +2.870 ms  control-no-change
  total store_read_bytes p50 2841097  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23643, 23659, 24328, 24343, 25004, 25046, 25747, 25753, 26452, 26466]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23634, 23667, 24350, 24354, 25057, 25071, 25756, 25772, 26447, 26459]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23643..26466 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23634..26459 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.883/14.870   p95   15.334/15.266   delta  -0.013 ms  control-no-change
  skill_request_overlay p50   36.858/36.798   p95   38.158/37.430   delta  -0.060 ms  control-no-change
  mcp_config_load   p50    0.993/0.917    p95    1.069/0.974    delta  -0.077 ms  control-no-change
  attach_history    p50  195.850/196.627  p95  204.100/204.975  delta  +0.777 ms  control-no-change
  resume_ready      p50  149.585/148.859  p95  157.091/157.755  delta  -0.726 ms  control-no-change
  provider_dispatch p50    1.485/1.466    p95    1.528/1.494    delta  -0.019 ms  control-no-change
  session_save      p50   44.814/54.898   p95  196.447/112.017  delta +10.084 ms  control-no-change
  total store_read_bytes p50 2841130  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23667, 23675, 24321, 24329, 25047, 25054, 25745, 25750, 26433, 26444]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23655, 23665, 24336, 24344, 25029, 25053, 25691, 25754, 26448, 26457]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23667..26444 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23655..26457 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.099/0.097    p95    0.136/0.161    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.752/0.753    p95    0.831/0.820    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.898/0.933    p95    0.944/0.984    delta  +0.035 ms  control-no-change
  attach_history    p50 2103.316/2099.309 p95 2123.937/2122.366 delta  -4.007 ms  control-no-change
  resume_ready      p50 1567.250/1569.259 p95 1575.554/1585.703 delta  +2.009 ms  control-no-change
  provider_dispatch p50   15.933/15.832   p95   16.876/17.751   delta  -0.101 ms  control-no-change
  session_save      p50   39.080/39.184   p95   51.763/146.892  delta  +0.104 ms  control-no-change
  total store_read_bytes p50 28640162  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23650, 23666, 24333, 24351, 25054, 25076, 25735, 25759, 26446, 26447]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23630, 23670, 24353, 24373, 25051, 25076, 25728, 25731, 26435, 26454]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23650..26447 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23630..26454 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.766/14.562   p95   15.316/15.625   delta  -0.205 ms  control-no-change
  skill_request_overlay p50   37.043/37.013   p95   37.624/38.209   delta  -0.030 ms  control-no-change
  mcp_config_load   p50    0.926/0.976    p95    1.107/1.019    delta  +0.050 ms  control-no-change
  attach_history    p50 2083.063/2077.153 p95 2099.146/2097.535 delta  -5.910 ms  control-no-change
  resume_ready      p50 1552.093/1548.812 p95 1575.696/1572.338 delta  -3.280 ms  control-no-change
  provider_dispatch p50   15.646/15.556   p95   16.554/16.283   delta  -0.090 ms  control-no-change
  session_save      p50   38.269/38.625   p95   48.799/115.349  delta  +0.356 ms  control-no-change
  total store_read_bytes p50 28640152  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23654, 23665, 24317, 24352, 25050, 25059, 25734, 25750, 26432, 26457]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23647, 23650, 24342, 24344, 25034, 25073, 25756, 25763, 26434, 26451]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23654..26457 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23647..26451 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.3 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (1000 lines) | 2.8 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.8 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| write (small) | 24.6 ms | 127.0 ms | 15.2 ms | 127.0 ms | 5 |
| write (10KB) | 12.8 ms | 51.2 ms | 10.1 ms | 51.2 ms | 5 |
| edit (simple replace) | 10.5 ms | 18.0 ms | 10.1 ms | 18.0 ms | 5 |
| ls (scripts dir) | 2.8 ms | 5.6 ms | 2.7 ms | 5.6 ms | 5 |
| agentgrep (grep in tree) | 8.5 ms | 16.6 ms | 8.0 ms | 16.6 ms | 5 |
| agentgrep (find **/*.rs) | 7.6 ms | 10.2 ms | 7.0 ms | 10.2 ms | 5 |
| bash (true) | 12.8 ms | 13.3 ms | 11.4 ms | 13.3 ms | 5 |
| bash (echo hello) | 10.1 ms | 13.1 ms | 10.0 ms | 13.1 ms | 5 |
| bash (ls fixture tree) | 15.3 ms | 19.3 ms | 13.2 ms | 19.3 ms | 5 |
| invalid tool (dispatch only) | 2.8 ms | 3.0 ms | 2.2 ms | 3.0 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 235.8 ms | 235.8 ms | 235.8 ms | 236.0 ms | 0 |
| 10 | 659.3 ms | 706.4 ms | 706.4 ms | 708.6 ms | 0 |
| 40 | 2.31 s | 3.26 s | 3.45 s | 3.47 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 663.9 ms | 663.9 ms | 663.9 ms | 1 |
| leaf_touch | src\main.rs | 902.9 ms | 902.9 ms | 902.9 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 25.21 s | 25.21 s | 25.21 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 35.77 s | 35.77 s | 35.77 s | 1 |

<!-- bench-auto:end -->
