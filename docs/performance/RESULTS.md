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

<!-- bench-auto:start commit=6cf85db5d3ff82498b6bdf36438932e7f1ad389a -->

`.github/workflows/benchmark-docs.yml` rewrites this section automatically. Do not edit it by hand.

- Commit: `6cf85db5d3ff82498b6bdf36438932e7f1ad389a`
- Measured at: 2026-11-09 14:21 UTC
- Run: <https://github.com/horon-ai/agent/actions/runs/42625435781> (raw JSON is in the per-OS `benchmark-<os>-<run_id>` artifacts)

Each OS is measured separately on a different GitHub-hosted runner. The machines differ, so do not compare absolute values across OSes; read only the relative comparison within one OS (multiples of horon) and the trend between commits.

| OS | Runner | Binary | Status |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (6cf85db)` | Partial failure (`failure`) |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (6cf85db)` | Partial failure (`failure`) |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (6cf85db)` | Partial failure (`failure`) |

### Linux

The measurement job on this OS did not finish successfully. Any suite below without results is missing for that reason.

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (6cf85db) | 3.9 ms | 110.7 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.9 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 11.0 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 44.4 ms | 435.0 MB |
| OpenCode | 1.19.5 | 495.7 ms | 613.4 MB |
| pi | 1.5.0 | 271.4 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 742.0 ms | 412.6 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 129.0 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 60.0 ms | 56.9 ms–110.0 ms | baseline | 37.2 MB | baseline | yes | 14 | — |
| jcode | 147.7 ms | 145.5 ms–153.2 ms | 2.46× more | 38.6 MB | 1.04× more | yes | 5 | — |
| Claude Code | 444.7 ms | 442.0 ms–485.6 ms | 7.41× more | 205.3 MB | 5.52× more | yes | — | — |
| Codex CLI | 440.2 ms | 439.4 ms–527.2 ms | 7.34× more | 191.0 MB | 5.13× more | yes | 6 | — |
| OpenCode | 2.90 s | 2.85 s–3.11 s | 48.32× more | 569.3 MB | 15.29× more | yes | 4 | — |
| pi | 459.0 ms | 458.9 ms–467.7 ms | 7.65× more | 113.3 MB | 3.04× more | yes | — | — |
| oh-my-pi (omp) | 2.20 s | 1.95 s–2.33 s | 36.72× more | 395.7 MB | 10.63× more | yes | 68 | — |
| oh-my-openagent (omo) | 6.80 s | 6.49 s–6.99 s | 113.30× more | 660.7 MB | 17.75× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 55.5 ms | 55.0 ms–55.6 ms | baseline | 35.4 MB | baseline | skill-uri | yes | — |
| jcode | 57.4 ms | 56.6 ms–58.3 ms | 1.03× more | 40.7 MB | 1.15× more | skill-tool | yes | — |
| Claude Code | 465.2 ms | 451.2 ms–471.1 ms | 8.39× more | 207.0 MB | 5.85× more | skill-tool | yes | — |
| Codex CLI | 537.3 ms | 531.1 ms–544.8 ms | 9.69× more | 191.2 MB | 5.40× more | file-read | yes | — |
| OpenCode | 3.71 s | 3.67 s–3.78 s | 66.90× more | 591.0 MB | 16.69× more | skill-tool | yes | — |
| pi | 558.1 ms | 549.7 ms–569.1 ms | 10.06× more | 124.6 MB | 3.52× more | file-read | yes | — |
| oh-my-pi (omp) | 1.99 s | 1.85 s–2.20 s | 35.79× more | 375.0 MB | 10.59× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.85 s | 6.53 s–7.13 s | 123.45× more | 666.9 MB | 18.83× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 80.8 ms | 80.2 ms–86.8 ms | baseline | 49.4 MB | baseline | mcp-call | yes | 59.7 ms | — |
| jcode | 176.9 ms | 175.0 ms–180.5 ms | 2.19× more | 55.6 MB | 1.13× more | mcp-action | yes | 145.0 ms | — |
| Claude Code | 575.2 ms | 563.0 ms–579.2 ms | 7.12× more | 226.1 MB | 4.57× more | mcp-tool | yes | 421.7 ms | — |
| Codex CLI | 475.0 ms | 457.5 ms–492.0 ms | 5.88× more | 195.5 MB | 3.95× more | mcp-tool | yes | 300.6 ms | — |
| OpenCode | 3.38 s | 3.32 s–3.40 s | 41.84× more | 574.7 MB | 11.63× more | mcp-tool | yes | 3.05 s | — |
| pi | 614.2 ms | 608.1 ms–616.3 ms | 7.60× more | 139.2 MB | 2.82× more | mcp-tool | yes | 540.3 ms | — |
| oh-my-pi (omp) | 2.18 s | 1.93 s–2.25 s | 27.03× more | 388.9 MB | 7.87× more | mcp-route | yes | 1.83 s | — |
| oh-my-openagent (omo) | 7.11 s | 7.08 s–7.20 s | 87.97× more | 663.1 MB | 13.42× more | mcp-tool | yes | 3.50 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | — | — | baseline | — | — | — | — |
| jcode | 42.9 MB | 36.9 KB | — | 0.12 | — | 46.3 MB | 254.7 KB |
| Claude Code | 150.2 MB | 147.9 MB | — | — | — | 188.1 MB | 180.5 MB |
| Codex CLI | 128.8 MB | 135.1 MB | — | — | — | 160.8 MB | 164.0 MB |
| OpenCode | 244.0 MB | 519.2 KB | — | — | — | 454.1 MB | 5.0 MB |
| pi | 107.4 MB | 107.8 MB | — | — | — | 130.8 MB | 127.0 MB |
| oh-my-pi (omp) | 329.1 MB | 61.9 MB | — | — | — | 364.6 MB | 354.7 MB |
| oh-my-openagent (omo) | 365.4 MB | 28.2 MB | — | — | — | 584.5 MB | 587.5 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | — | — | — |
| jcode | 46.3 MB | 47.1 MB | 48.5 MB |
| Claude Code | 188.1 MB | 916.6 MB | 1.8 GB |
| Codex CLI | 160.8 MB | 809.7 MB | 1.6 GB |
| OpenCode | 454.1 MB | 477.8 MB | 499.2 MB |
| pi | 130.8 MB | 622.9 MB | 1.2 GB |
| oh-my-pi (omp) | 364.6 MB | 1.7 GB | 3.5 GB |
| oh-my-openagent (omo) | 584.5 MB | 3.0 GB | 5.7 GB |

**Errors recorded during measurement**

- horon @ 1 sessions: daemon socket not ready: Error: Unknown provider profile 'mock'. Add [providers.mock] to config.toml.
- horon @ 5 sessions: daemon socket not ready: Error: Unknown provider profile 'mock'. Add [providers.mock] to config.toml.
- horon @ 10 sessions: daemon socket not ready: Error: Unknown provider profile 'mock'. Add [providers.mock] to config.toml.

#### Startup

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.14 ms
  Max:    3.31 ms
  Mean:   3.23 ms
  Median: 3.23 ms
  Stdev:  0.06 ms

--version:
  Min:    3.00 ms
  Max:    3.13 ms
  Mean:   3.05 ms
  Median: 3.04 ms
  Stdev:  0.05 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.44 ms
  Max:    20.51 ms
  Mean:   20.47 ms
  Median: 20.47 ms
  Stdev:  0.02 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.52 ms
  Max:    20.67 ms
  Mean:   20.62 ms
  Median: 20.63 ms
  Stdev:  0.04 ms

Bridge stage: hello_ok:
  Min:    21.10 ms
  Max:    21.33 ms
  Mean:   21.19 ms
  Median: 21.17 ms
  Stdev:  0.07 ms

Bridge stage: no-op reply:
  Min:    21.47 ms
  Max:    21.70 ms
  Mean:   21.54 ms
  Median: 21.53 ms
  Stdev:  0.07 ms

Bridge total (socket -> no-op):
  Min:    21.47 ms
  Max:    21.70 ms
  Mean:   21.54 ms
  Median: 21.53 ms
  Stdev:  0.07 ms

============================================================
Summary:
  Binary load median:      ~3.2 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.5 ms
```

#### Session loading (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (7e89e99427e8)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-4a9db7a5
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.011/0.011    p95    0.021/0.017    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.083/0.083    p95    0.095/0.091    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.110/0.109    p95    0.111/0.116    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    5.663/5.720    p95   21.222/5.945    delta  +0.057 ms  control-no-change
  total store_read_bytes p50 23078  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23072..23096 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23084 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.901/1.874    p95    1.964/1.920    delta  -0.027 ms  control-no-change
  skill_request_overlay p50    3.349/3.337    p95    3.490/3.423    delta  -0.012 ms  control-no-change
  mcp_config_load   p50    0.112/0.112    p95    0.116/0.118    delta  +0.000 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   12.103/13.634   p95   46.293/26.319   delta  +1.531 ms  control-no-change
  total store_read_bytes p50 23074  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23084 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23084 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.011/0.011    p95    0.011/0.011    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.085/0.083    p95    0.100/0.116    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.111/0.111    p95    0.122/0.114    delta  -0.000 ms  control-no-change
  attach_history    p50  167.293/168.445  p95  212.593/228.159  delta  +1.152 ms  control-no-change
  resume_ready      p50   33.792/32.958   p95   64.608/46.980   delta  -0.834 ms  control-no-change
  provider_dispatch p50    0.803/0.783    p95    2.012/0.800    delta  -0.020 ms  control-no-change
  session_save      p50    5.618/5.640    p95   19.479/15.515   delta  +0.022 ms  control-no-change
  total store_read_bytes p50 5311411  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23072..23094 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23046..23094 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.856/1.865    p95    1.940/1.887    delta  +0.009 ms  control-no-change
  skill_request_overlay p50    3.311/3.331    p95    3.441/3.384    delta  +0.019 ms  control-no-change
  mcp_config_load   p50    0.112/0.111    p95    0.125/0.116    delta  -0.001 ms  control-no-change
  attach_history    p50  170.200/172.830  p95  217.456/185.511  delta  +2.630 ms  control-no-change
  resume_ready      p50   32.619/32.464   p95   33.653/34.251   delta  -0.155 ms  control-no-change
  provider_dispatch p50    0.765/0.761    p95    0.795/0.794    delta  -0.004 ms  control-no-change
  session_save      p50    5.447/5.427    p95   11.110/131.229  delta  -0.020 ms  control-no-change
  total store_read_bytes p50 5311407  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23089 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23090 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.011/0.011    p95    0.012/0.011    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.082/0.082    p95    0.084/0.094    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.108/0.108    p95    0.116/0.117    delta  +0.000 ms  control-no-change
  attach_history    p50 2218.310/2228.931 p95 2248.057/2279.630 delta +10.621 ms  control-no-change
  resume_ready      p50  371.053/374.170  p95  397.530/383.145  delta  +3.117 ms  control-no-change
  provider_dispatch p50    8.941/9.069    p95    9.696/9.855    delta  +0.128 ms  control-no-change
  session_save      p50    5.740/5.790    p95    5.993/6.121    delta  +0.050 ms  control-no-change
  total store_read_bytes p50 53417614  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23059..23093 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 42184287..42184293 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210240..11210246 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23076..23093 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.902/1.883    p95    1.964/1.945    delta  -0.019 ms  control-no-change
  skill_request_overlay p50    3.321/3.281    p95    3.457/3.357    delta  -0.040 ms  control-no-change
  mcp_config_load   p50    0.112/0.111    p95    0.119/0.117    delta  -0.001 ms  control-no-change
  attach_history    p50 2195.658/2216.315 p95 2308.768/2271.552 delta +20.657 ms  control-no-change
  resume_ready      p50  370.534/368.514  p95  384.142/382.949  delta  -2.020 ms  control-no-change
  provider_dispatch p50    9.087/9.182    p95   10.373/9.962    delta  +0.095 ms  control-no-change
  session_save      p50    5.780/5.708    p95    6.350/6.335    delta  -0.072 ms  control-no-change
  total store_read_bytes p50 53417622  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23064..23089 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23069..23089 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (7e89e99427e8)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-nkvha8ph
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.034/0.018    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.108/0.105    p95    0.139/0.115    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.145/0.144    p95    0.147/0.159    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    9.296/10.531   p95   13.273/12.882   delta  +1.234 ms  control-no-change
  total store_read_bytes p50 23793  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23047, 23061, 23065, 23767, 23791, 23795, 24487, 24509, 24513]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23059, 23069, 23077, 23769, 23773, 23803, 24489, 24509, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23047..24513 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23059..24517 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.835/2.856    p95    2.908/3.407    delta  +0.021 ms  control-no-change
  skill_request_overlay p50    5.088/5.102    p95    5.199/6.495    delta  +0.014 ms  control-no-change
  mcp_config_load   p50    0.153/0.151    p95    0.164/0.208    delta  -0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   10.023/10.573   p95   11.133/12.433   delta  +0.550 ms  control-no-change
  total store_read_bytes p50 23796  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23055, 23069, 23085, 23787, 23793, 23799, 24507, 24509, 24513, 24517]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23061, 23067, 23069, 23771, 23795, 23801, 24493, 24501, 24503, 24509]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23055..24517 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23061..24509 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.015/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.152/0.146    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.145    p95    0.179/0.174    delta  +0.000 ms  control-no-change
  attach_history    p50  101.138/101.374  p95  103.502/102.723  delta  +0.236 ms  control-no-change
  resume_ready      p50   54.549/54.549   p95   55.028/55.491   delta  -0.000 ms  control-no-change
  provider_dispatch p50    1.363/1.373    p95    1.432/1.448    delta  +0.010 ms  control-no-change
  session_save      p50    8.336/8.399    p95   12.804/8.751    delta  +0.063 ms  control-no-change
  total store_read_bytes p50 2841349  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 23800, 24496, 24514, 25214, 25232, 25940, 25950, 26662, 26668]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23790, 23802, 24508, 24516, 25214, 25226, 25240, 25928, 25948, 26666]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26668 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23790..26666 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.843/2.811    p95    2.946/2.874    delta  -0.032 ms  control-drift
  skill_request_overlay p50    4.722/4.730    p95    4.779/4.759    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.150/0.151    p95    0.157/0.165    delta  +0.001 ms  control-no-change
  attach_history    p50  101.403/101.129  p95  101.842/102.730  delta  -0.274 ms  control-no-change
  resume_ready      p50   54.443/54.282   p95   55.860/54.709   delta  -0.161 ms  control-no-change
  provider_dispatch p50    1.339/1.351    p95    1.408/1.384    delta  +0.012 ms  control-no-change
  session_save      p50    9.249/9.419    p95   10.596/9.881    delta  +0.171 ms  control-no-change
  total store_read_bytes p50 2841352  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23787, 23803, 24505, 24521, 25213, 25227, 25931, 25945, 26669, 26675]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23778, 23792, 24508, 24522, 25224, 25230, 25946, 25948, 26662, 26670]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23787..26675 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23778..26670 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.033/0.016    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.107    p95    0.149/0.135    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.147/0.147    p95    0.208/0.175    delta  +0.000 ms  control-no-change
  attach_history    p50 1128.786/1131.339 p95 1150.560/1154.042 delta  +2.553 ms  control-no-change
  resume_ready      p50  645.968/632.305  p95  659.983/662.077  delta -13.663 ms  control-no-change
  provider_dispatch p50   17.060/16.224   p95   18.213/17.796   delta  -0.835 ms  control-no-change
  session_save      p50    8.318/8.116    p95    8.719/8.520    delta  -0.202 ms  control-no-change
  total store_read_bytes p50 28640379  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23795, 24501, 24513, 25225, 25231, 25937, 25941, 26667, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23777, 23795, 24513, 24515, 25229, 25239, 25941, 25945, 26667, 26671]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23795..26671 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23777..26671 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.818/2.817    p95    2.934/2.911    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    4.725/4.741    p95    4.796/4.873    delta  +0.015 ms  control-no-change
  mcp_config_load   p50    0.150/0.152    p95    0.158/0.167    delta  +0.002 ms  control-no-change
  attach_history    p50 1131.544/1137.417 p95 1156.666/1146.513 delta  +5.873 ms  control-no-change
  resume_ready      p50  660.755/652.646  p95  667.330/670.207  delta  -8.109 ms  control-drift
  provider_dispatch p50   16.660/16.721   p95   17.773/17.903   delta  +0.061 ms  control-no-change
  session_save      p50    9.596/9.898    p95    9.819/10.160   delta  +0.301 ms  control-no-change
  total store_read_bytes p50 28640382  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23795, 23811, 24509, 24513, 25231, 25945, 25961, 26655, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23789, 23797, 24499, 25233, 25237, 25943, 25947, 26651, 26665]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23795..26671 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23789..26665 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

Not measured: this run did not produce `tools.md`.

#### Reload storm

Not measured: this run did not produce `reload.md`.

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 237.3 ms | 237.3 ms | 237.3 ms | 1 |
| leaf_touch | src/main.rs | 376.5 ms | 376.5 ms | 376.5 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 19.41 s | 19.41 s | 19.41 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 25.87 s | 25.87 s | 25.87 s | 1 |

### macOS

The measurement job on this OS did not finish successfully. Any suite below without results is missing for that reason.

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (6cf85db) | 10.7 ms | 93.1 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 11.9 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 35.1 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 116.1 ms | 346.5 MB |
| OpenCode | 1.19.5 | 589.2 ms | 344.6 MB |
| pi | 1.5.0 | 370.5 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 1.08 s | 273.0 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 221.3 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 101.3 ms | 89.3 ms–410.6 ms | baseline | 30.6 MB | baseline | yes | — |
| jcode | 387.1 ms | 357.2 ms–694.7 ms | 3.82× more | 34.0 MB | 1.11× more | yes | — |
| Claude Code | 579.1 ms | 548.7 ms–1.02 s | 5.72× more | 195.1 MB | 6.38× more | yes | — |
| Codex CLI | 623.4 ms | 496.4 ms–1.91 s | 6.15× more | 143.2 MB | 4.68× more | yes | — |
| OpenCode | 3.90 s | 3.44 s–5.96 s | 38.46× more | 717.9 MB | 23.48× more | yes | — |
| pi | 666.9 ms | 662.6 ms–748.1 ms | 6.58× more | 122.9 MB | 4.02× more | yes | — |
| oh-my-pi (omp) | 3.29 s | 2.40 s–3.36 s | 32.52× more | 426.8 MB | 13.96× more | yes | — |
| oh-my-openagent (omo) | 13.17 s | 10.76 s–13.25 s | 129.96× more | 894.4 MB | 29.25× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 108.9 ms | 107.1 ms–425.4 ms | baseline | 24.5 MB | baseline | skill-uri | yes | — |
| jcode | 356.5 ms | 339.7 ms–380.5 ms | 3.27× more | 36.3 MB | 1.48× more | skill-tool | yes | — |
| Claude Code | 470.0 ms | 462.7 ms–616.9 ms | 4.32× more | 183.2 MB | 7.47× more | skill-tool | yes | — |
| Codex CLI | 652.8 ms | 642.7 ms–1.07 s | 6.00× more | 142.3 MB | 5.80× more | file-read | yes | — |
| OpenCode | 5.54 s | 5.17 s–5.59 s | 50.92× more | 712.9 MB | 29.08× more | skill-tool | yes | — |
| pi | 893.9 ms | 735.0 ms–1.11 s | 8.21× more | 149.9 MB | 6.11× more | file-read | yes | — |
| oh-my-pi (omp) | 2.71 s | 2.23 s–3.48 s | 24.92× more | 433.2 MB | 17.67× more | file-read | yes | — |
| oh-my-openagent (omo) | 13.00 s | 12.66 s–13.77 s | 119.38× more | 898.8 MB | 36.66× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 122.2 ms | 121.3 ms–183.6 ms | baseline | 19.3 MB | baseline | mcp-call | yes | 92.3 ms | — |
| jcode | 410.8 ms | 377.4 ms–435.5 ms | 3.36× more | 52.3 MB | 2.71× more | mcp-action | yes | 385.6 ms | — |
| Claude Code | 646.5 ms | 538.0 ms–680.8 ms | 5.29× more | 220.8 MB | 11.44× more | mcp-tool | yes | 499.2 ms | — |
| Codex CLI | 449.2 ms | 427.3 ms–475.3 ms | 3.68× more | 142.6 MB | 7.39× more | mcp-tool | yes | 319.7 ms | — |
| OpenCode | 4.38 s | 4.09 s–4.59 s | 35.84× more | 709.2 MB | 36.75× more | mcp-tool | yes | 3.70 s | — |
| pi | 826.1 ms | 826.0 ms–892.9 ms | 6.76× more | 153.5 MB | 7.96× more | mcp-tool | yes | 742.1 ms | — |
| oh-my-pi (omp) | 2.46 s | 2.22 s–4.64 s | 20.13× more | 439.8 MB | 22.79× more | mcp-route | yes | 2.04 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | no | — | failed: no tool that could reach the fixture was offered (one request, no tool call) |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | — | — | baseline | — | — | — | — |
| jcode | 36.3 MB | 226.9 KB | — | — | — | 40.0 MB | 350.2 KB |
| Claude Code | 157.4 MB | 145.3 MB | — | — | — | 190.4 MB | 181.6 MB |
| Codex CLI | 88.3 MB | 84.3 MB | — | — | — | 106.8 MB | 107.8 MB |
| OpenCode | 261.9 MB | 771.3 KB | — | — | — | 561.7 MB | 8.6 MB |
| pi | 118.7 MB | 115.4 MB | — | — | — | 149.0 MB | 147.8 MB |
| oh-my-pi (omp) | 385.9 MB | 48.9 MB | — | — | — | 439.0 MB | 318.3 MB |
| oh-my-openagent (omo) | 57.6 MB | 42.0 MB | — | — | — | 821.2 MB | 333.8 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | — | — | — |
| jcode | 40.0 MB | 41.4 MB | 43.1 MB |
| Claude Code | 190.4 MB | 929.2 MB | 1.8 GB |
| Codex CLI | 106.8 MB | 534.3 MB | 1.1 GB |
| OpenCode | 561.7 MB | 604.0 MB | 639.6 MB |
| pi | 149.0 MB | 739.6 MB | 1.4 GB |
| oh-my-pi (omp) | 439.0 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 821.2 MB | 3.4 GB | 3.8 GB |

**Errors recorded during measurement**

- horon @ 1 sessions: daemon socket not ready: Error: Unknown provider profile 'mock'. Add [providers.mock] to config.toml.
- horon @ 5 sessions: daemon socket not ready: Error: Unknown provider profile 'mock'. Add [providers.mock] to config.toml.
- horon @ 10 sessions: daemon socket not ready: Error: Unknown provider profile 'mock'. Add [providers.mock] to config.toml.

#### Startup

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    9.22 ms
  Max:    17.87 ms
  Mean:   12.60 ms
  Median: 11.56 ms
  Stdev:  3.37 ms

--version:
  Min:    11.23 ms
  Max:    23.56 ms
  Mean:   16.93 ms
  Median: 17.86 ms
  Stdev:  4.14 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    28.23 ms
  Max:    100.57 ms
  Mean:   68.63 ms
  Median: 70.92 ms
  Stdev:  27.40 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    45.00 ms
  Max:    123.74 ms
  Mean:   89.61 ms
  Median: 94.61 ms
  Stdev:  24.16 ms

Bridge stage: hello_ok:
  Min:    45.67 ms
  Max:    125.88 ms
  Mean:   90.93 ms
  Median: 95.72 ms
  Stdev:  24.47 ms

Bridge stage: no-op reply:
  Min:    46.08 ms
  Max:    126.99 ms
  Mean:   91.94 ms
  Median: 96.42 ms
  Stdev:  24.89 ms

Bridge total (socket -> no-op):
  Min:    46.08 ms
  Max:    126.99 ms
  Mean:   91.94 ms
  Median: 96.42 ms
  Stdev:  24.89 ms

============================================================
Summary:
  Binary load median:      ~11.6 ms
  Server ready median:     ~70.9 ms
  Bridge total median:     ~96.4 ms
```

#### Session loading (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (6117ff9a287e)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-upbjjo9x
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.026/0.024    p95    0.037/0.048    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.243/0.238    p95    0.725/0.347    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.269/0.267    p95    0.376/0.301    delta  -0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   20.296/15.957   p95   29.806/33.132   delta  -4.340 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22930..22954 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22926..22966 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.638/5.163    p95    8.983/7.900    delta  +0.525 ms  control-no-change
  skill_request_overlay p50    5.568/6.815    p95   10.689/10.431   delta  +1.247 ms  control-no-change
  mcp_config_load   p50    0.278/0.273    p95    0.870/0.585    delta  -0.005 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   25.341/30.017   p95   31.494/52.734   delta  +4.677 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22926..22954 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22958 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.031/0.024    p95    0.048/0.051    delta  -0.006 ms  control-no-change
  skill_request_overlay p50    0.233/0.230    p95    0.444/0.343    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.271/0.274    p95    0.565/1.390    delta  +0.003 ms  control-no-change
  attach_history    p50  357.077/378.151  p95  397.536/428.015  delta +21.073 ms  control-no-change
  resume_ready      p50   89.226/91.348   p95  101.435/124.586  delta  +2.122 ms  control-no-change
  provider_dispatch p50    1.025/1.067    p95    1.259/2.051    delta  +0.042 ms  control-no-change
  session_save      p50   22.921/25.439   p95   44.386/49.299   delta  +2.518 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22936..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22962 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    5.548/5.497    p95    9.793/10.194   delta  -0.051 ms  control-no-change
  skill_request_overlay p50    5.455/6.726    p95   11.303/19.689   delta  +1.270 ms  control-drift
  mcp_config_load   p50    0.274/0.294    p95    0.668/0.801    delta  +0.019 ms  control-no-change
  attach_history    p50  374.725/382.804  p95  440.836/501.373  delta  +8.079 ms  control-no-change
  resume_ready      p50   80.950/84.462   p95  107.136/112.520  delta  +3.512 ms  control-no-change
  provider_dispatch p50    1.085/1.104    p95    3.110/1.731    delta  +0.019 ms  control-no-change
  session_save      p50   26.317/24.794   p95   42.524/39.627   delta  -1.523 ms  control-no-change
  total store_read_bytes p50 5311304  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22940..22960 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189199..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099137..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22968 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.035/0.028    p95    0.212/0.089    delta  -0.007 ms  control-no-change
  skill_request_overlay p50    0.374/0.220    p95    0.813/0.450    delta  -0.153 ms  control-no-change
  mcp_config_load   p50    0.325/0.254    p95    1.054/0.335    delta  -0.071 ms  control-no-change
  attach_history    p50 4614.364/4713.953 p95 5515.694/5591.135 delta +99.589 ms  control-no-change
  resume_ready      p50  962.927/984.008  p95 1145.844/1093.615 delta +21.081 ms  control-no-change
  provider_dispatch p50   15.049/11.779   p95   18.432/15.760   delta  -3.270 ms  control-no-change
  session_save      p50   27.903/27.330   p95   43.211/35.821   delta  -0.572 ms  control-no-change
  total store_read_bytes p50 53417518  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22967 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22967 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.307/5.495    p95   10.974/11.983   delta  +1.188 ms  control-drift
  skill_request_overlay p50    5.268/6.525    p95    7.065/16.429   delta  +1.257 ms  control-drift
  mcp_config_load   p50    0.263/0.276    p95    0.282/1.467    delta  +0.013 ms  control-no-change
  attach_history    p50 4446.496/4909.344 p95 6270.295/6294.073 delta +462.848 ms  control-no-change
  resume_ready      p50  895.449/947.006  p95 1200.879/1153.878 delta +51.557 ms  control-no-change
  provider_dispatch p50   14.564/14.151   p95   21.975/27.165   delta  -0.414 ms  control-no-change
  session_save      p50   32.676/38.158   p95   74.758/115.567  delta  +5.483 ms  control-no-change
  total store_read_bytes p50 53417521  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210253..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22955..22974 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22961 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (6117ff9a287e)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-nsxizcuv
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.024/0.024    p95    0.028/0.038    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.215/0.216    p95    0.230/0.339    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.249/0.258    p95    0.264/0.395    delta  +0.008 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   12.339/12.332   p95   15.190/14.740   delta  -0.007 ms  control-no-change
  total store_read_bytes p50 23646  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22935, 22947, 22951, 23643, 23649, 23661, 24371, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22947, 22951, 23661, 23669, 23673, 24371, 24373, 24375, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22935..24379 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24383 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    3.962/4.101    p95    8.402/4.648    delta  +0.139 ms  control-no-change
  skill_request_overlay p50    5.165/5.186    p95   11.101/5.678    delta  +0.021 ms  control-no-change
  mcp_config_load   p50    0.269/0.257    p95    0.691/0.309    delta  -0.012 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   13.747/14.499   p95   18.373/20.819   delta  +0.751 ms  control-no-change
  total store_read_bytes p50 23662  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22943, 22945, 22951, 23649, 23657, 23667, 24363, 24371, 24373]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22941, 22947, 23655, 23665, 23669, 24363, 24365, 24375, 24377]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22943..24373 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22941..24377 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.041/0.034    p95    0.172/0.174    delta  -0.007 ms  control-no-change
  skill_request_overlay p50    0.305/0.278    p95    1.149/0.614    delta  -0.027 ms  control-no-change
  mcp_config_load   p50    0.305/0.285    p95    1.302/0.450    delta  -0.020 ms  control-no-change
  attach_history    p50  139.900/129.187  p95  174.206/193.388  delta -10.713 ms  control-no-change
  resume_ready      p50  110.554/103.877  p95  140.992/119.239  delta  -6.677 ms  control-no-change
  provider_dispatch p50    1.141/1.213    p95    1.720/2.051    delta  +0.072 ms  control-no-change
  session_save      p50   25.233/19.980   p95   93.755/53.504   delta  -5.253 ms  control-no-change
  total store_read_bytes p50 2841254  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23682, 24380, 24392, 25094, 25098, 25816, 26518, 26526]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23674, 23682, 24380, 25098, 25106, 25800, 25816, 26514, 26526]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26526 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23674..26526 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    5.510/4.288    p95   10.356/4.952    delta  -1.222 ms  control-no-change
  skill_request_overlay p50    7.636/5.625    p95   13.461/7.981    delta  -2.011 ms  control-no-change
  mcp_config_load   p50    0.286/0.276    p95    0.752/0.436    delta  -0.009 ms  control-no-change
  attach_history    p50  119.065/118.042  p95  145.377/155.671  delta  -1.023 ms  control-no-change
  resume_ready      p50   75.448/76.223   p95   91.322/106.451  delta  +0.775 ms  control-no-change
  provider_dispatch p50    1.081/1.117    p95    1.629/1.372    delta  +0.036 ms  control-no-change
  session_save      p50   22.010/20.689   p95   31.679/27.381   delta  -1.321 ms  control-no-change
  total store_read_bytes p50 2841262  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23666, 24376, 24392, 25102, 25106, 25808, 25812, 26526, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23662, 24380, 24384, 25094, 25808, 25820, 26518, 26530]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23666..26530 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23662..26530 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.032/0.035    p95    0.068/0.106    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.250/0.279    p95    0.544/1.084    delta  +0.028 ms  control-no-change
  mcp_config_load   p50    0.316/0.523    p95    0.508/1.120    delta  +0.207 ms  control-drift
  attach_history    p50 1665.558/1556.439 p95 2020.801/2301.058 delta -109.120 ms  control-no-change
  resume_ready      p50  976.291/1006.772 p95 1271.643/1092.845 delta +30.481 ms  control-no-change
  provider_dispatch p50   15.915/16.791   p95   21.670/33.080   delta  +0.876 ms  control-no-change
  session_save      p50   28.624/29.527   p95   89.648/117.706  delta  +0.903 ms  control-no-change
  total store_read_bytes p50 28640279  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23673, 23677, 24383, 24385, 25103, 25105, 25807, 25815, 26525, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23649, 23673, 24383, 24387, 25089, 25101, 25811, 25819, 26517, 26529]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23673..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23649..26529 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.300/4.172    p95    9.417/7.297    delta  -0.128 ms  control-no-change
  skill_request_overlay p50    5.726/5.694    p95    9.768/8.273    delta  -0.032 ms  control-no-change
  mcp_config_load   p50    0.292/0.276    p95    0.586/0.572    delta  -0.016 ms  control-no-change
  attach_history    p50 1369.521/1273.467 p95 1670.667/1511.364 delta -96.054 ms  control-no-change
  resume_ready      p50  834.055/785.275  p95 1031.323/957.902  delta -48.780 ms  control-no-change
  provider_dispatch p50   12.608/12.267   p95   16.821/16.355   delta  -0.341 ms  control-no-change
  session_save      p50   25.558/30.137   p95   39.084/87.266   delta  +4.579 ms  control-no-change
  total store_read_bytes p50 28640273  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 23668, 24382, 24386, 25096, 25104, 25798, 25814, 26524, 26532]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23658, 23660, 24386, 24388, 25088, 25112, 25802, 25806, 26518, 26536]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26532 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23658..26536 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

Not measured: this run did not produce `tools.md`.

#### Reload storm

Not measured: this run did not produce `reload.md`.

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.58 s | 1.58 s | 1.58 s | 1 |
| leaf_touch | src/main.rs | 1.05 s | 1.05 s | 1.05 s | 1 |
| core_touch | crates/app-core/src/lib.rs | 42.40 s | 42.40 s | 42.40 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 60.00 s | 60.00 s | 60.00 s | 1 |

### Windows

The measurement job on this OS did not finish successfully. Any suite below without results is missing for that reason.

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (6cf85db) | 11.0 ms | 92.8 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 15.8 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 36.7 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 88.2 ms | 479.4 MB |
| OpenCode | 1.19.5 | 465.4 ms | 605.3 MB |
| pi | 1.5.0 | 242.6 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 636.1 ms | 291.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 173.5 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 290.7 ms | 209.3 ms–424.2 ms | baseline | 30.4 MB | baseline | yes | — |
| jcode | 1.75 s | 1.59 s–3.27 s | 6.04× more | 28.2 MB | 1.08× less | yes | — |
| Claude Code | 1.24 s | 1.16 s–1.72 s | 4.26× more | 3.0 GB | 101.94× more | yes | — |
| Codex CLI | 2.51 s | 885.6 ms–3.64 s | 8.63× more | 263.9 MB | 8.67× more | yes | — |
| OpenCode | 4.62 s | 4.56 s–6.55 s | 15.88× more | 857.3 MB | 28.16× more | yes | — |
| pi | 421.5 ms | 419.1 ms–464.4 ms | 1.45× more | 120.1 MB | 3.95× more | yes | — |
| oh-my-pi (omp) | 2.89 s | 2.40 s–3.06 s | 9.95× more | 405.2 MB | 13.31× more | yes | — |
| oh-my-openagent (omo) | 21.66 s | 21.10 s–29.77 s | 74.52× more | 546.6 MB | 17.95× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 2.55 s | 1.21 s–3.66 s | baseline | 24.8 MB | baseline | skill-uri | yes | — |
| jcode | 621.9 ms | 424.6 ms–657.4 ms | 4.10× less | 24.1 MB | 1.03× less | skill-tool | yes | — |
| Claude Code | 826.1 ms | 811.9 ms–1.18 s | 3.09× less | 237.4 MB | 9.56× more | skill-tool | yes | — |
| Codex CLI | 3.24 s | 2.36 s–3.78 s | 1.27× more | 254.5 MB | 10.25× more | file-read | yes | — |
| OpenCode | 6.06 s | 5.83 s–6.74 s | 2.38× more | 747.8 MB | 30.12× more | skill-tool | yes | — |
| pi | 567.1 ms | 557.5 ms–611.3 ms | 4.49× less | 97.7 MB | 3.94× more | file-read | yes | — |
| oh-my-pi (omp) | 2.68 s | 2.56 s–3.61 s | 1.05× more | 397.8 MB | 16.02× more | file-read | yes | — |
| oh-my-openagent (omo) | 18.96 s | 18.91 s–30.31 s | 7.44× more | 522.7 MB | 21.06× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 3.46 s | 3.06 s–3.58 s | baseline | 37.3 MB | baseline | mcp-call | yes | 2.22 s | — |
| jcode | 499.5 ms | 393.8 ms–649.3 ms | 6.93× less | 36.5 MB | 1.02× less | mcp-action | yes | 277.5 ms | — |
| Claude Code | 891.6 ms | 874.7 ms–1.11 s | 3.88× less | 2.9 GB | 79.89× more | mcp-tool | yes | 763.8 ms | — |
| Codex CLI | 963.4 ms | 914.1 ms–1.08 s | 3.59× less | 177.3 MB | 4.76× more | mcp-tool | yes | 700.4 ms | — |
| OpenCode | 5.67 s | 5.64 s–5.84 s | 1.64× more | 772.5 MB | 20.72× more | mcp-tool | yes | 4.40 s | — |
| pi | 621.3 ms | 620.1 ms–671.1 ms | 5.57× less | 117.7 MB | 3.16× more | mcp-tool | yes | 503.4 ms | — |
| oh-my-pi (omp) | 3.46 s | 3.35 s–4.02 s | same | 415.1 MB | 11.14× more | mcp-route | yes | 2.57 s | — |
| oh-my-openagent (omo) | 28.76 s | 16.46 s–35.53 s | 8.31× more | 566.8 MB | 15.20× more | mcp-tool | yes | 23.22 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | — | — | baseline | — | — | — | — |
| jcode | 21.8 MB | 105.8 KB | — | — | — | 26.8 MB | 362.0 KB |
| Claude Code | 154.1 MB | 141.9 MB | — | — | — | 188.0 MB | 160.2 MB |
| Codex CLI | 136.1 MB | 97.2 MB | — | — | — | 163.5 MB | 122.1 MB |
| OpenCode | 246.6 MB | 0.0 B | — | — | — | 435.3 MB | 22.9 MB |
| pi | 90.6 MB | 90.6 MB | — | — | — | 101.0 MB | 100.8 MB |
| oh-my-pi (omp) | 332.2 MB | 133.9 MB | — | — | — | 341.3 MB | 334.6 MB |
| oh-my-openagent (omo) | 139.4 MB | 133.7 MB | — | — | — | 413.0 MB | 409.0 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | — | — | — |
| jcode | 26.8 MB | 28.4 MB | 30.0 MB |
| Claude Code | 188.0 MB | 4.4 GB | 1.8 GB |
| Codex CLI | 163.5 MB | 648.1 MB | 1.2 GB |
| OpenCode | 435.3 MB | 453.0 MB | 636.1 MB |
| pi | 101.0 MB | 504.0 MB | 1008.5 MB |
| oh-my-pi (omp) | 341.3 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 413.0 MB | 2.0 GB | 3.9 GB |

**Errors recorded during measurement**

- horon @ 1 sessions: daemon socket not ready: Error: Unknown provider profile 'mock'. Add [providers.mock] to config.toml.
- horon @ 5 sessions: daemon socket not ready: Error: Unknown provider profile 'mock'. Add [providers.mock] to config.toml.
- horon @ 10 sessions: daemon socket not ready: Error: Unknown provider profile 'mock'. Add [providers.mock] to config.toml.
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
- oh-my-openagent (omo) @ 10 sessions: turn 2 session 2: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 2 session 4: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 2 session 9: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 2: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 4: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 9: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.

#### Startup

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    15.00 ms
  Max:    21.72 ms
  Mean:   17.94 ms
  Median: 17.71 ms
  Stdev:  2.26 ms

--version:
  Min:    14.18 ms
  Max:    15.59 ms
  Mean:   14.76 ms
  Median: 14.64 ms
  Stdev:  0.50 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.37 ms
  Max:    27.91 ms
  Mean:   23.15 ms
  Median: 22.62 ms
  Stdev:  1.69 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~17.7 ms
  Server ready median:     ~22.6 ms
  Bridge total median:     n/a
```

#### Session loading (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (f39f78eb20cc)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-iodwnpbh
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.100/0.100    p95    0.164/0.107    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.759/0.713    p95    0.883/0.762    delta  -0.046 ms  control-no-change
  mcp_config_load   p50    0.882/0.919    p95    0.985/0.943    delta  +0.037 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   56.665/62.868   p95   73.944/339.310  delta  +6.203 ms  control-no-change
  total store_read_bytes p50 22956  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22886..22968 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22914..22956 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   14.399/14.680   p95   14.843/15.519   delta  +0.281 ms  control-no-change
  skill_request_overlay p50   37.134/39.266   p95   39.534/50.746   delta  +2.132 ms  control-drift
  mcp_config_load   p50    0.975/0.990    p95    1.063/1.324    delta  +0.015 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-drift
  session_save      p50  125.763/113.649  p95  295.130/340.754  delta -12.115 ms  control-no-change
  total store_read_bytes p50 22941  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22882..22968 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22866..22964 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.099/0.100    p95    0.158/0.161    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.808/0.764    p95    0.862/0.866    delta  -0.044 ms  control-no-change
  mcp_config_load   p50    0.932/0.937    p95    1.006/1.013    delta  +0.005 ms  control-no-change
  attach_history    p50  435.040/431.848  p95  493.206/724.051  delta  -3.192 ms  control-no-change
  resume_ready      p50  154.115/152.807  p95  176.703/159.266  delta  -1.308 ms  control-no-change
  provider_dispatch p50    1.517/1.515    p95    1.738/1.542    delta  -0.002 ms  control-no-change
  session_save      p50   52.522/49.568   p95  203.654/134.016  delta  -2.954 ms  control-no-change
  total store_read_bytes p50 5311217  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22948..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22920..22980 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.043/15.039   p95   15.376/15.339   delta  -0.004 ms  control-no-change
  skill_request_overlay p50   37.268/37.462   p95   38.982/39.435   delta  +0.194 ms  control-no-change
  mcp_config_load   p50    0.988/1.028    p95    1.066/1.092    delta  +0.040 ms  control-no-change
  attach_history    p50  432.110/428.230  p95  445.287/527.338  delta  -3.879 ms  control-no-change
  resume_ready      p50  149.327/151.015  p95  155.731/156.623  delta  +1.688 ms  control-no-change
  provider_dispatch p50    1.481/1.440    p95    1.574/1.557    delta  -0.041 ms  control-no-change
  session_save      p50   69.679/96.146   p95  126.770/279.448  delta +26.468 ms  control-no-change
  total store_read_bytes p50 5311211  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22934..22976 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22931..22970 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.103/0.098    p95    0.151/0.153    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.768/0.756    p95    0.833/0.823    delta  -0.012 ms  control-no-change
  mcp_config_load   p50    0.982/0.959    p95    1.039/1.074    delta  -0.022 ms  control-no-change
  attach_history    p50 4945.055/4938.268 p95 5012.945/5007.267 delta  -6.787 ms  control-no-change
  resume_ready      p50 1585.196/1586.952 p95 1615.816/1596.836 delta  +1.756 ms  control-no-change
  provider_dispatch p50   17.849/18.069   p95   18.961/19.478   delta  +0.220 ms  control-no-change
  session_save      p50   49.445/51.250   p95   56.276/62.710   delta  +1.804 ms  control-no-change
  total store_read_bytes p50 53417424  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22935..22987 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22924..22975 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.646/14.639   p95   14.882/15.650   delta  -0.007 ms  control-no-change
  skill_request_overlay p50   37.051/37.197   p95   39.741/41.516   delta  +0.146 ms  control-no-change
  mcp_config_load   p50    0.963/0.985    p95    1.253/1.381    delta  +0.022 ms  control-no-change
  attach_history    p50 4867.480/4865.844 p95 4924.048/4957.762 delta  -1.636 ms  control-no-change
  resume_ready      p50 1550.092/1547.950 p95 1572.632/1555.197 delta  -2.142 ms  control-no-change
  provider_dispatch p50   16.698/16.155   p95   17.829/17.449   delta  -0.543 ms  control-no-change
  session_save      p50   49.625/48.523   p95   87.387/141.501  delta  -1.102 ms  control-no-change
  total store_read_bytes p50 53417426  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210214..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22975 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22935..22971 (6 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Session loading (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (f39f78eb20cc)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-cb0cdads
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.163/0.162    p95    0.250/0.193    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    1.308/1.228    p95    1.667/1.416    delta  -0.079 ms  control-no-change
  mcp_config_load   p50    1.475/1.462    p95    1.849/1.696    delta  -0.013 ms  control-no-change
  attach_history    p50    0.000/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   54.435/55.269   p95  122.481/124.442  delta  +0.834 ms  control-no-change
  total store_read_bytes p50 23650  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22928, 22950, 22953, 22965, 23648, 23651, 23653, 24322, 24329, 24359]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22896, 22925, 22955, 22959, 23620, 23631, 23668, 24304, 24331, 24335]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22928..24359 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22896..24335 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   24.826/24.864   p95   32.771/27.759   delta  +0.038 ms  control-no-change
  skill_request_overlay p50   58.670/58.774   p95   63.918/69.948   delta  +0.105 ms  control-no-change
  mcp_config_load   p50    1.568/1.539    p95    1.633/2.211    delta  -0.029 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.006    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   53.466/55.401   p95   61.334/58.903   delta  +1.935 ms  control-no-change
  total store_read_bytes p50 23622  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22945, 22954, 22961, 22963, 23622, 23623, 23655, 23676, 24317, 24355]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22914, 22939, 22957, 22965, 23627, 23630, 23654, 24338, 24345, 24347]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22945..24355 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22914..24347 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.190/0.159    p95    0.325/0.288    delta  -0.031 ms  control-no-change
  skill_request_overlay p50    1.268/1.288    p95    1.561/1.407    delta  +0.020 ms  control-no-change
  mcp_config_load   p50    1.444/1.448    p95    1.770/1.573    delta  +0.004 ms  control-no-change
  attach_history    p50  306.659/308.591  p95  312.323/326.202  delta  +1.932 ms  control-no-change
  resume_ready      p50  242.836/241.115  p95  249.825/250.453  delta  -1.721 ms  control-no-change
  provider_dispatch p50    1.901/1.922    p95    2.541/2.023    delta  +0.021 ms  control-no-change
  session_save      p50   51.071/46.703   p95   53.535/53.944   delta  -4.368 ms  control-no-change
  total store_read_bytes p50 2841129  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23642, 23674, 24309, 24357, 25038, 25064, 25745, 25763, 26427, 26454]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23634, 23655, 24359, 24366, 25019, 25050, 25720, 25751, 26433, 26459]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23642..26454 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23634..26459 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   25.643/25.489   p95   27.293/28.498   delta  -0.154 ms  control-no-change
  skill_request_overlay p50   58.350/58.227   p95   61.636/65.882   delta  -0.123 ms  control-no-change
  mcp_config_load   p50    1.560/1.533    p95    1.849/1.670    delta  -0.026 ms  control-no-change
  attach_history    p50  304.061/307.801  p95  316.394/319.224  delta  +3.739 ms  control-no-change
  resume_ready      p50  243.690/243.036  p95  257.952/247.940  delta  -0.654 ms  control-no-change
  provider_dispatch p50    1.964/1.996    p95    2.281/2.061    delta  +0.033 ms  control-no-change
  session_save      p50   52.657/50.755   p95   67.504/58.720   delta  -1.902 ms  control-no-change
  total store_read_bytes p50 2841104  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23646, 23650, 24317, 24367, 25024, 25732, 25733, 26440, 26441]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23667, 24329, 24362, 25061, 25067, 25741, 25744, 26388, 26450]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23646..26441 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26450 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.166/0.187    p95    0.210/0.239    delta  +0.021 ms  control-no-change
  skill_request_overlay p50    1.274/1.276    p95    1.534/1.638    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    1.513/1.450    p95    1.647/1.784    delta  -0.062 ms  control-no-change
  attach_history    p50 3166.362/3172.272 p95 3202.474/3230.979 delta  +5.910 ms  control-no-change
  resume_ready      p50 2511.143/2509.813 p95 2531.431/2542.674 delta  -1.330 ms  control-no-change
  provider_dispatch p50   20.429/22.115   p95   22.997/22.546   delta  +1.686 ms  control-no-change
  session_save      p50   46.998/48.641   p95   55.217/53.575   delta  +1.643 ms  control-no-change
  total store_read_bytes p50 28640151  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23645, 23670, 24337, 24362, 25032, 25076, 25735, 25741, 26440, 26448]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23644, 23648, 24355, 24357, 25052, 25078, 25724, 25735, 26453, 26454]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23645..26448 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23644..26454 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   24.075/24.217   p95   28.533/36.789   delta  +0.142 ms  control-no-change
  skill_request_overlay p50   59.522/59.872   p95   66.361/98.029   delta  +0.350 ms  control-no-change
  mcp_config_load   p50    1.527/1.571    p95    1.901/2.282    delta  +0.044 ms  control-no-change
  attach_history    p50 3173.614/3192.699 p95 3303.494/3449.323 delta +19.085 ms  control-no-change
  resume_ready      p50 2520.449/2534.371 p95 2564.039/2889.773 delta +13.922 ms  control-no-change
  provider_dispatch p50   22.175/22.779   p95   23.535/28.500   delta  +0.604 ms  control-no-change
  session_save      p50   51.777/51.420   p95   58.073/209.630  delta  -0.357 ms  control-no-change
  total store_read_bytes p50 28640126  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23615, 23662, 24342, 24373, 25004, 25069, 25709, 25728, 26398, 26425]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23642, 23662, 24337, 24349, 25048, 25072, 25743, 25775, 26452, 26458]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23615..26425 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23642..26458 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

Not measured: this run did not produce `tools.md`.

#### Reload storm

Not measured: this run did not produce `reload.md`.

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 691.4 ms | 691.4 ms | 691.4 ms | 1 |
| leaf_touch | src\main.rs | 979.1 ms | 979.1 ms | 979.1 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 27.28 s | 27.28 s | 27.28 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 38.20 s | 38.20 s | 38.20 s | 1 |

<!-- bench-auto:end -->
