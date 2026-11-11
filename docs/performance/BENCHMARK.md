# Performance measurement

Language: **English** | [한국어](BENCHMARK.ko.md)

horon's performance is read through three devices. Each asks a different question.

| Device | What it asks | When it runs | Failure condition | Where the result lands |
| --- | --- | --- | --- | --- |
| [Benchmarks](#benchmarks) | How fast and how much, and how does it compare with other agents | Manual runs, and automatically after a `main` code change | Script errors only (never fails on a number) | Job summary, artifacts, the automatic section of `RESULTS.md` |
| [Structural gate](#structural-gate) | Are the queues, buffers, and resources still inside their bounds | `dev` and `main` PRs and pushes | A counter crosses its bound | CI result |
| [A/B measurement](#ab-measurement) | How much did this change actually save | By hand when making a performance change | The runner decides | PR body, `RESULTS.md` |

Measured results and their limits are in [`RESULTS.md`](RESULTS.md).

## Benchmarks

`.github/workflows/benchmark.yml` owns this. It measures Linux, macOS, and Windows **separately per OS** on GitHub-hosted runners.

| OS key | Runner | Memory metric | Notes |
| --- | --- | --- | --- |
| `linux` | `ubuntu-24.04` | RSS, PSS | Also counts `fsync` calls (`strace`) |
| `macos` | `macos-latest` | RSS | |
| `windows` | `windows-latest` | Working Set | The daemon connection is a named pipe, and harness bridge startup is not measured (`api-bridge` is Unix-only) |

Each OS calls `.github/workflows/benchmark-os.yml` once, and inside it the work splits like this. Calling it per OS means one OS's measurement starts as soon as that OS's build finishes, without waiting for the slowest OS build.

| Job | Count | What it does |
| --- | --- | --- |
| `build` | 1 per OS | Builds the `horon-agent` and `loading_probe` release binaries once and uploads them as the `bench-bin-<os>-<run_id>` artifact (tar, kept 1 day). With a `baseline_ref` and `compare` selected it also builds that commit separately into `baseline/` inside the same tar. Both builds use the values `release.yml` uses for shipped binaries (`CARGO_PROFILE_RELEASE_OPT_LEVEL=3`, `CARGO_PROFILE_RELEASE_DEBUG=0`, `CARGO_INCREMENTAL=0`). The repository's `[profile.release]` is for local rebuilds (`opt-level = 1`, incremental), and the competing tools run their own shipped release builds. |
| `measure` | 1 per OS × measurement unit | Downloads the binaries and measures exactly one unit. The units are `compare`, `startup`, `loading-cold`, `loading-warm`, `tools`, `reload`, and `tui`. With a baseline build, `compare` measures it as the `horon-base` row, alternating inside the same job. |
| `compile` | 1 per OS | Rebuilds from source itself, so it runs separately and does not share a machine with the other measurements. |
| `merge` | 1 per OS | Collects the per-unit results `bench-part-<os>-<unit>-<run_id>` into the same `benchmark-<os>-<run_id>` artifact as before (`scripts/perf/bench_merge.py`). |

The comparison rows in one table (horon, each competing tool, each event count) are measured together on one machine. Those rows therefore share CPU, disk, and load. Do not compare absolute values across groups or across OSes, because the machines differ. One unit failing still lets the other units and OSes run. That OS's `status.txt` is recorded as not `success`, and whatever was measured stays in the artifact.

### Running it

```bash
gh workflow run benchmark.yml --ref dev                                   # three OSes, all groups
gh workflow run benchmark.yml --ref dev -f os=windows                     # Windows only
gh workflow run benchmark.yml --ref dev -f os=linux,macos -f suites=compare,tools
gh workflow run benchmark.yml --ref dev -f competitors=claude_code,codex
gh workflow run benchmark.yml --ref dev -f suites=compare -f competitors=omp,omo
```

| Input | Default | Meaning |
| --- | --- | --- |
| `os` | `all` | Comma-separated choice of `linux`, `macos`, `windows`. The jobs above start for each chosen OS. |
| `suites` | `all` | Comma-separated choice of `compare`, `startup`, `loading`, `tools`, `reload`, `tui`, `compile`. |
| `competitors` | `jcode,claude_code,codex,opencode,pi,omp,omo` | Tools compared with horon. `omp` (oh-my-pi) and `omo` (oh-my-openagent) are included. A tool that failed to install or lacks its runtime stays as `skipped`. |
| `sessions` | `1 5 10` | Concurrent session counts for the session memory comparison |
| `turns` | `3` | Turns sent per session |
| `oneshot_runs` | `5` | How many samples each build takes for the one-shot, skill and MCP cells; `compare.md` names the count on its Host line. Five is the floor for a median; raise it to about 20 when the median has to separate two close builds |
| `baseline_ref` | empty | When set, builds that commit separately inside the same job and measures it alternately as the `horon-base` row. That makes a before-and-after comparison on the same runner |
| `features` | empty | Cargo features added to the **candidate** release build only (`--features <value>`); the `horon-base` build never gets them, so the two rows stay comparable. Empty leaves the build command byte-identical to a run without this input, and the artifact then holds an empty `features.txt` line. The value must match `^[a-z0-9_,-]*$`. The `build` job keys its rust-cache entry on it, so a feature build never reuses the default artifacts |

An unknown OS or group name fails the run immediately (`scripts/perf/bench_select.py`). Results stay for 30 days as the per-OS `merge` job summary and the `benchmark-<os>-<run_id>` artifact (JSON, Markdown). Intermediate artifact names do not start with `benchmark-`, so they do not mix into the `benchmark-docs.yml` download.

### Allocator A/B

The shipped binary builds without the `jemalloc` feature, so glibc is the Linux allocator and the default is not changed by a benchmark run. To compare the two on one runner, give the candidate the feature and let the same job build the baseline without it:

```bash
gh workflow run benchmark.yml --ref <branch> -f os=linux -f suites=compare \
  -f competitors=jcode -f oneshot_runs=20 -f baseline_ref=dev -f features=jemalloc
```

Read the added-session memory (idle and right after a turn), first-session RSS, one-shot time and install size in both horon rows. A cell where jemalloc's min–max range is worse than the baseline's does not justify a default change. The `features` input only exists on the candidate build; the `horon-base` row is always the shipped feature set, which is what makes the pair a measurement of the feature alone. The input is not restricted by OS; the allocator A/B is defined for `os=linux` only, and on a target where the C toolchain cannot build the allocator cargo fails the build job.

There are no secrets and no API charges. Every measurement that needs a model attaches to `scripts/perf/mock_llm_server.py` (a local fake model server).

### Suites

| Group | Script | What it measures |
| --- | --- | --- |
| `compare` | `bench_compare.py`, `install_competitors.sh` | `--version` time, one-question latency and peak memory, memory by concurrent session count, install size, skill invocation time, and MCP tool call time and tool round trip for horon and the competing tools (`jcode`, `claude_code`, `codex`, `opencode`, `pi`, `omp`, `omo`). Linux also counts `fsync` calls. |
| `startup` | `bench_startup.py` | `--help` and `--version` load, `serve` socket readiness, and per-stage harness bridge readiness |
| `loading` | `bench_loading.py` | Session loading of a fixed-seed fixture, COLD and WARM |
| `tools` | `bench_tools.py` | Latency of 14 cases of core tools (read, write, edit, ls, agentgrep, bash) over the debug socket |
| `reload` | `bench_reload_storm.py` | Latency until `history` arrives when reopening N sessions (1, 10, 40) at once |
| `tui` | `bench_tui.py` | The interactive client `horon-tui`: first frame (bare `horon-agent` and a direct launch), the handoff between them, PTY input echo, idle and post-turn RSS, one drawn frame from the `render_bench` example, and install size |
| `compile` | `bench_compile.py` | `cargo check` with no change, and rebuild time after editing `src/main.rs`, `crates/app-core`, or build-meta |

### Automatic measurement

`.github/workflows/benchmark-docs.yml` starts when CI for a push to `main` succeeds. If an earlier step blocks, later steps do not run.

1. **Check for code changes.** It looks at whether anything under `src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, or `scripts/perf` changed since the last measuring commit (the `<!-- bench-auto:start commit=... -->` in `RESULTS.md`). With no record, or a commit missing from history, it compares against the parent commit. If nothing changed, it stops.
2. **Wait for the structural gate.** It waits up to 80 minutes for the `structural-perf.yml` push run on the same commit to succeed.
3. **Benchmark.** It dispatches `benchmark.yml` with `os=all` and `suites=all`. The three OSes run separately.
4. **Update the documents.** It downloads every per-OS artifact, rewrites only the `bench-auto` section of `RESULTS.md` and `RESULTS.ko.md` with `scripts/perf/bench_docs.py render`, and pushes to `main` and `dev` with a `docs(perf): record benchmark for <sha>` commit.

The document section holds an OS summary table (runner, binary, status) and a per-OS section in Linux, macOS, Windows order. A failed OS is marked "partial failure" and keeps what it measured. An OS with no results still keeps its section, marked "no results". Text outside the section stays byte-for-byte identical.

The docs commit is pushed with `GITHUB_TOKEN`, so that push does not start the workflow again. A docs-only commit is filtered out at step 1. `workflow_run` only runs workflow files from the default branch, so this file must be on `main` to work.

The numbers in the automatic section are a trend record, not a regression criterion. Do not edit them by hand. The next measurement overwrites them.

### Running locally

Every script uses only the Python standard library and runs on all three OSes. It uses a temporary HOME and runtime folder, so it never touches the real `~/.horon`.

```bash
cargo build --release --locked -p horon-agent
python3 scripts/perf/bench_tools.py --binary ./target/release/horon-agent --markdown-out tools.md
python3 scripts/perf/bench_reload_storm.py --binary ./target/release/horon-agent --markdown-out reload.md
python3 scripts/perf/bench_startup.py ./target/release/horon-agent --runs 10
```

To check only that the suites still start, run the smoke. It runs the tools, reload, and compare (horon only, `oneshot` and `sessions`) suites once each and exits 1 if any of them crashed, skipped, or recorded an error. A suite exits 0 even when its daemon never came up, so the smoke reads each JSON report rather than the exit code. The `Build & Test` job in `ci.yml` runs it on every code pull request against the binary it just built, because the benchmark workflow itself never runs on a pull request.

```bash
python3 scripts/perf/bench_smoke.py --binary ./target/release/horon-agent
```

The competitor comparison needs the tools installed first. A tool not given with `--bin name=path` is looked up on PATH.

```bash
bash scripts/perf/install_competitors.sh /tmp/bench-tools jcode,claude_code,codex,opencode,pi,omp,omo
python3 scripts/perf/bench_compare.py --bin horon=./target/release/horon-agent $(cat /tmp/bench-tools/bins.args) \
  --install-failures /tmp/bench-tools/failures.txt \
  --sessions 1 5 10 --turns 3 --json-out compare.json --markdown-out compare.md
```

`install_competitors.sh` installs pinned versions. jcode is the GitHub release `v0.99.1` (`.exe` on Windows), and the rest come from `npm install --prefix` as `@anthropic-ai/claude-code@2.1.317`, `@openai/codex@0.166.0`, `opencode-ai@1.19.5`, `@earendil-works/pi-coding-agent@1.5.0`, `@oh-my-pi/pi-coding-agent@18.18.0`, and `omo-ai@5.1.64`. Change versions with `JCODE_VERSION`, `CLAUDE_VERSION`, `CODEX_VERSION`, `OPENCODE_VERSION`, `PI_VERSION`, `OMP_VERSION`, and `OMO_VERSION`. On Windows it runs under Git Bash, and npm tools are recorded as `.cmd` executables.

pi ships under the `@earendil-works` name from 1.x and bundles an MCP client (Node 22.19 or newer). 0.73.1 (`@mariozechner/pi-coding-agent`) lacked an MCP client and could not measure the MCP row, so the pin moved to 1.5.0, which means every pi row from this version on measures a different version than earlier results. The MCP measurement puts the fixture server into `$PI_CODING_AGENT_DIR/mcp.json` with `"exposure": "direct"`. The default, `codemode`, does not tell the model about the tool, so the fake model could not call `mcp__bench__bench_echo`. The one-shot, skill, and session measurements turn MCP off with `--no-mcp` to match the other tools.

The jcode download handles transient errors (5xx). It retries the release download URL up to five times and, if that still fails, fetches the same file through the GitHub release API asset id. Either way it is used only when it matches the value in the release's `SHA256SUMS`. A tool that fails to install is recorded as one `name<TAB>reason` line in `<dest>/failures.txt`, and `bench_compare.py --install-failures` reads that file and writes it into the `Skipped:` line of `compare.md` as `install failed: <reason>`. If even one tool went unmeasured, `compare.md` starts with a `> **Warning: N tool(s) were not measured**` line, and on GitHub Actions each tool leaves a `::warning` annotation and a step summary line, visible right on the run page. An item that lacks a measurement feature, such as `syscalls`, is not a tool and does not warn.

### Metrics for the competitor comparison

| Metric | Meaning |
| --- | --- |
| `version_ms` | Statistics over N `--version` runs (5 by default) |
| `oneshot_ms` | Latency of one headless question (startup, one `ls` tool call, final response), the peak process tree memory during it, and whether the tool round-tripped |
| `fsync_calls` | Number of `fsync` and `fdatasync` calls one question makes when `--syscalls` is on. Counted by running the same number of extra `strace`-wrapped runs separately from the timed runs. Measured on Linux only when `strace` exists, otherwise `skipped` records the reason |
| `sessions_memory` | Process tree memory after N concurrent sessions × K turns, both idle and right after a turn. It reports the first-session value and the per-session increment (least-squares slope) |
| `install_size_bytes` | Executable size. For npm tools, the package plus installed dependencies |
| `skill_ms` | Time for one headless question to find the fixture skill and read its body. The `Via` column names the path the tool used (`skill-tool`/`skill-uri`/`file-read`) |
| `mcp_ms` | Time for one headless question to start the fixture stdio MCP server and call `bench_echo` once |
| `mcp_ready_ms` | Time from process start to the first request whose tool list shows `bench_echo` (MCP server start plus tool registration). Measured as when the fake model saw that request |
| `tool_round_trip` | `true` only when a later request actually saw the sentinel from the skill body or the nonce the MCP server returned |
| `request_offsets_ms` | Per run record (JSON), the time from process start to each model request arriving. The gaps split startup, tool round trip, and shutdown per OS |
| `thread_names` | For each session count in the daemon session measurement, the daemon process's threads counted by name right after a turn (Linux `/proc/<pid>/task/*/comm`). Shows which threads grow with session count. `null` on other OSes |
| `live_heap_bytes` | In the daemon session measurement, the bytes malloc areas actually hold while idle and right after a turn (the "All zones" total from macOS `heap(1)`). Read beside the RSS slope, it separates live data from memory the allocator keeps. The after-turn value is taken after the RSS sample; `null` on other OSes |
| `post_turn_rss_anon_bytes`, `post_turn_rss_file_bytes` | The daemon tree RSS right after a turn split into anonymous memory (heap, stack) and file mappings (executable, libraries) (Linux `/proc/<pid>/status` `RssAnon`, `RssFile`). Shows which side the per-session increment comes from. `null` on other OSes |
| `post_turn_allocator_live_bytes`, `post_purge_rss_bytes` | Measured on the horon daemon only. The first is what the allocator reports as live (glibc `mallinfo2`), the second is the tree RSS after `allocator:purge` returns freed pages. RSS minus post-return RSS is what the allocator kept after freeing, and post-return RSS minus live bytes is what neither explains (stack, mappings, fragmentation). The return changes the process, so it is sampled last after every other sample. An OS with no return mechanism (macOS, the Windows system allocator) records the reason in `diagnostic_error` |
| `log_tail`, `exit_local_time` | Per horon run record, the log lines this run wrote (up to 40 from the turn completion line) and the local time the process ended. Shows per OS which stage the time from the last model request to exit goes to. Empty list for other tools |

Multipliers keep horon at 1.0×. Each tool attaches to the fake model differently. horon and jcode use `[providers.mock]` in a temporary HOME plus a debug socket session on one daemon, Claude Code uses `ANTHROPIC_BASE_URL` and a resident `stream-json` process, Codex uses `model_providers` and `app-server`, OpenCode uses `baseURL` in `opencode.json` and `serve`, and pi, omp, and omo use `models.json` (`providers.mock`, `openai-completions`) under a temporary HOME with the session handed to `--mode rpc`.

Session implementations differ per tool (a session inside the daemon versus a process per session), so compare the per-session increment rather than absolute values. `strace` stops the tool on every system call, so a wrapped run's time grows with the call count. `oneshot_ms` therefore comes only from runs without `strace`, and wrapped runs serve only to count `fsync_calls`.

### omp and omo

The two tools use the same `models.json` and RPC contract as pi, so they share the harness, but their run modes and defaults differ and these are matched separately.

| Tool | Report name | npm package | Pinned version | Runtime | Mock wiring |
| --- | --- | --- | --- | --- | --- |
| `omp` | `oh-my-pi (omp)` | `@oh-my-pi/pi-coding-agent` | `18.18.0` (`OMP_VERSION`) | Bun 1.4.4 or newer (`engines`) | `providers.mock` in `models.json` under a temporary HOME, `-p --no-session --no-title --model mock/mock-model --no-lsp` |
| `omo` | `oh-my-openagent (omo)` | `omo-ai` | `5.1.64` (`OMO_VERSION`) | Node 24 or newer, or Bun | The same `models.json`, `-p --no-session --model mock/mock-model --tools bash,read` |

- `omp` ships its npm CLI as a Bun script, so the compare job installs Bun first with `oven-sh/setup-bun`. Without Bun the installer records `omp` as `skipped`, and without Node or Bun it records `omo` as `skipped`, while the job continues.
- If Bun cannot be fetched from GitHub releases, the installer installs `bun` from the npm registry instead (`BUN_VERSION`, default 1.4.4).
- `omo`'s default tool list has no shell tool (`bash`), and `--tools` is an allowlist, so `bash` and `read` must be named for the mock model to call tools.
- `omp` run without `--no-title` sends one more model request for title generation, which changes the one-shot request count. `--no-lsp` blocks the language server download.
- Both tools cache the model list and base URL in the agent directory (`agent.db`, `models.db` for omp), so each measurement gets a fresh agent directory. Session measurements create one per session.
- Telemetry, version checks, and network outside the mock server are turned off with `PI_OFFLINE=1`, `PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0`, `DISABLE_TELEMETRY=1`, and `DO_NOT_TRACK=1`. `OMP_*`, `OMO_*`, `SENPI_*`, `PI_CONFIG_DIR`, and API keys from the parent environment are removed and home and config paths point only under the temporary directory.
- Both tools are in the default comparison list (the `competitors` default in `benchmark.yml` and `DEFAULT_TOOLS` in `bench_compare.py`). They were added after `version_ms`, `oneshot`, and `sessions_memory` were all measured on Linux, macOS, and Windows. `benchmark-os.yml` sets the measured set by passing the input list as `--tools`, so `-f competitors=jcode,pi` also works to leave them out.

### Skill invocation and MCP tool calls

The `compare` group picks scenarios with `--scenarios`. The default is `oneshot,sessions,skill,mcp`; a subset means the missing scenarios do not run and no table appears for them in the report. An unknown name fails the run immediately.

- `skill`: the prompt starts with `[bench:skill]`. Seeing that marker, the fake model calls the skill-specific tool (`Skill`, `skill`, `skills.read`, `skill_manage`) when one exists; otherwise it passes `skill://bench-skill` to a read tool whose description mentions `skill://`; otherwise it opens the `SKILL.md` absolute path the prompt supplies with a read or shell tool. The `via` field in the request record and the `Via` column in the report record which path it took.
- `mcp`: the prompt starts with `[bench:mcp]`. The fake model looks for a tool whose name ends in `bench_echo` and calls it once with `{"text": "bench-nonce-N"}`. If that tool sits inside a Responses namespace, the function name is `bench_echo` and the namespace goes in a separate `namespace` field (the `function_call` shape Codex receives). When the tool list has no server tool, each agent follows the path its docs define: the fixed `mcp_call` tool (`mcp-call`), the management tool `mcp` whose `action` enum includes `call` (`mcp-action`), loading the prompt-listed deferred tool `mcp__<server>__bench_echo` first through `ToolSearch`'s `select:<name>` (`tool-search`), or, for a tool like omp that lists only an `xd://...bench_echo` route in the prompt, calling that route with the `write` tool (`mcp-route`).
- If a tool was called but no sentinel or nonce came back, the last 400 characters of the final tool result go into `result_excerpt` in the run record. That value exists to check sandbox denials and shell errors without rerunning them.
- Round trips are not assumed. `tool_round_trip` becomes `true` only when a later request sees `BENCH-SKILL-SENTINEL` from the `SKILL.md` body for skills, or the nonce the model passed for MCP. A run that returns only `mock-ok` without calling a tool stays `false`.
- Both scenarios measure from process start to the last response, so skill discovery, MCP server start, and tool registration land in the number. `mcp_ready_ms` shows only the server-ready part of it.

The fixtures live in the repository.

- `scripts/perf/fixtures/bench_skill/SKILL.md`: frontmatter `name: bench-skill` and one sentinel line in the body.
- `scripts/perf/fixtures/bench_mcp_server.py`: a dependency-free stdio JSON-RPC server. It answers `initialize`, `tools/list` (tool `bench_echo`), and `tools/call` (echoing the `text` it receives), rejects unknown methods with `-32601` and unknown tools with `-32602`, and exits with code 0 when standard input closes. Each agent starts it with its own config; the harness does not start it.

Config surfaces differ per tool, so the harness places things like this. The `Via` column in the report says which path was actually taken.

| Tool | Skill fixture location and invocation | MCP config |
| --- | --- | --- |
| `horon` | Reads the workspace `.horon/skills/bench-skill/SKILL.md` with the `read` tool (`file-read`) | `servers` in `<HORON_HOME>/mcp.json`. Because the tool list is fixed by a cache, the server tool is called through the fixed `mcp_call` tool (`server: bench`, `tool: bench_echo`) (`mcp-call`). This scenario alone sets `HORON_AGENT_RUN_MCP=1` |
| `jcode` | Invokes the fixture under `<JCODE_HOME>/skills` with `skill_manage` (`skill-tool`) | `mcpServers` in `<JCODE_HOME>/mcp.json` and the workspace `.jcode/mcp.json`. OpenAI-compatible providers get no MCP tools in the list (the default behavior of `[tools] mcp_tools = "auto"`), so it calls `mcp` with `action: "call"`, `server: bench`, `tool: bench_echo` (`mcp-action`) |
| `claude_code` | Invokes the fixture under `CLAUDE_CONFIG_DIR/skills` with the `Skill` tool (`skill-tool`) | `--mcp-config <file> --strict-mcp-config`. `--mcp-config` servers connect in the background by default, so the first request goes out without knowing the tool; it starts with the documented `MCP_CONNECTION_NONBLOCKING=0` so startup waits for the server. Tool search keeps its default, so the prompt lists `mcp__bench__bench_echo` as a deferred tool and the fake model loads the schema with `ToolSearch` (`select:mcp__bench__bench_echo`) before calling the tool. That costs one more model request |
| `codex` | `cat`s the fixture under the workspace `.agents/skills` with the shell tool (`file-read`). Codex finds that location as project skills and puts them in the prompt. The sandbox stays on and is tuned to work on the runner. The Linux runner first follows the Ubuntu 24.04 procedure from the Codex docs (install bubblewrap, load the AppArmor `bwrap-userns-restrict` profile). Otherwise every command fails with `bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`. On Windows every Codex run gets `-c windows.sandbox="unelevated"`: without it a fresh runner has no prepared sandbox, `-s read-only` commands are rejected as `rejected: blocked by policy`, and `unelevated` is the documented way to run without administrator setup. Windows measurement folders are also created with `os.mkdir` rather than `tempfile.mkdtemp` so they inherit the `%TEMP%` ACL. On CPython 3.12.4 and newer, `mkdtemp`'s 0o700 becomes a protected ACL that allows only SYSTEM, Administrators, and the owner, and on an administrator runner the owner is the Administrators group, so the sandbox token without administrator rights cannot read under it and gets `Access to the path ... is denied` | `[mcp_servers.bench]` and `tools.bench_echo.approval_mode = "approve"` in `$CODEX_HOME/config.toml`. The server appears as the namespaced tool `mcp__bench`, called with `name: bench_echo`, `namespace: mcp__bench` (`mcp-tool`) |
| `opencode` | Invokes the fixture under `XDG_CONFIG_HOME/opencode/skills` with the `skill` tool (`skill-tool`) | `mcp.bench` in `opencode.json` (`type: local`) |
| `pi` | Reads `SKILL.md` from the prompt's skill list with the `read` tool (`file-read`) | `mcpServers` in `$PI_CODING_AGENT_DIR/mcp.json`, `"exposure": "direct"`. Calls `mcp__bench__bench_echo` directly (`mcp-tool`) |
| `omp` | Reads the fixture under the agent directory `<agent>/skills` with `read` (`file-read`) | `mcpServers` in `<agent>/mcp.json`. Calls the `xd://...bench_echo` route from the prompt with `write` (`mcp-route`) |
| `omo` | Reads the fixture under the agent directory `<agent>/skills` with `read` (`file-read`) | `mcpServers` in `<agent>/mcp.json`, function `mcp_bench_bench_echo`. `--tools` is an allowlist and MCP tool names can differ per build, so this scenario alone omits `--tools` and uses the default tool set (including connected server tools) |

Each scenario gets its own home and workspace under `<scenario>/run<N>` beneath the tool root. Reusing the same home lets a previous run's MCP schema cache or skill hints skip preparation work. Skill and MCP fixtures also live only in that scenario's home, so they cannot leak into another scenario's numbers. The whole root is deleted when the run ends.

### Measurement limits

- The model is fake, so real model latency and token cost are not included. What is measured is the agent's own startup, memory, and tool round trip cost.
- Absolute values on hosted runners differ per machine and are not a baseline. Read only relative comparisons and trends within the same run and OS.
- Memory metrics differ per OS. Do not compare Linux PSS, macOS RSS, and Windows Working Set as the same number.
- The Windows bash tool runs through `cmd.exe`. `bash (true)` becomes `exit 0` and `bash (ls fixture tree)` becomes `dir /b tree` to do the same work.
- Tool latency is debug socket round trip plus `Agent::execute_tool` time. Model turns, approvals, and prompt assembly are not included.
- Repeating the reload storm at one level leaves the earlier opened sessions in the daemon and makes the second one slower. Read absolute values with `--iterations 1`.
- The compile benchmark breaks the cargo fingerprint through mtime. With hash-based fingerprints it produces the same value as no change.
- macOS temporary folder paths are long enough to exceed the Unix socket path limit of 104 bytes. The scripts use a short folder under `/tmp`.

### Contract for a new measurement script

It accepts `--json-out` and `--markdown-out`. A measurement failure records `null` and `error` rather than 0. A missing tool or binary records the reason in `skipped` and exits with code 0. Statistics are median, p95, minimum, maximum, and sample count. Daemon connection, process termination, and memory use `ipc_connect`, `new_group_kwargs`, `terminate_pgroup`, and `process_tree_rss_bytes` from `bench_common.py`, so the same code runs on all three OSes.

## Structural gate

`.github/workflows/structural-perf.yml` owns this. It runs on `dev` and `main` PRs and pushes and on manual dispatch. It uses no release build, no secrets, and no external network, only `cargo test` in the debug profile.

The gate asks "is the shape still bounded", not "is it fast". It therefore reads counters, not time. Latency and memory differ per machine, so using them as bounds would fail normal changes or pass real regressions. Counters read the same on any machine.

| Item | Counters | Bound |
| --- | --- | --- |
| Queue depth and buffer | `queue_depth_high_water`, `queue_bytes_high_water`, `queue_bytes_buffered` | At or below queue depth, at or below the cap, 0 after commit |
| Pending requests | `pending_requests`, `pending_requests_high_water` | 1 or more while pending, 0 after the response, at or below the cap |
| Page ranges | `history_page_replayed`, `history_page_flattened`, `history_index_builds`, `warm_read_bytes` | A warm page replays 0, builds no index, flattens only a page's worth, and reads at most a tenth of the log |
| Streaming hash buffer | `large_request`, `small_request`, `buffer_bytes` | All three equal and at or below `HASH_STREAM_BUFFER` |
| NDJSON frame cap | `max_frame_bytes`, `accepted_frame_bytes`, `rejected_frame_bytes` | The cap equals the budget value, accepted frames are at or below it, rejected ones exceed it |
| Resource return | `fd_retention`, `thread_retention`, `non_returning` | At or below budget after a 20-cycle soak, 0 failed returns |
| IRC mailbox | `mailbox_high_water`, `mailbox_dropped`, `broadcast_body_allocations`, `waiters_live_after_churn`, `broadcast_inflight_high_water` | At or below the 100 cap, 900 drops for 1000 arrivals, one body allocation per broadcast, 0 waiters after cleanup, at most 16 concurrent broadcasts |
| Skill catalog and workspace skills | `skill_catalog_file_reads`, `skill_catalog_bytes`, `skill_warmup_file_reads`, `skill_unchanged_request_file_reads`, `skill_unchanged_request_dir_reads` | In a 200-skill catalog at most one file read and 8 KiB per skill (reading a 32 KiB body whole fails), 0 file and folder reads across 9 unchanged workspace requests, 1 or more warmup reads |
| MCP user config | `mcp_config_warmup_reads`, `mcp_config_user_rereads` | 1 or more warmup reads, 0 rereads of unchanged user config after warmup |
| Request prefix | `tools_bytes`, `tool_count` | The `core` profile's tool catalog is at least 1 byte and at or below the committed ceiling, and the tool count equals the eager definitions it was built from |

The mock model server records `body_bytes`, `tools_bytes`, and `tool_count` for every request it answers (`GET /__stats`), so a bench run reports the same per-request shape the `prefix` row bounds.

An existing test in the repository prints each value as one `STRUCTURAL_PERF section=<name> k=v ...` line, and the gate reads that line. Because the counters are process-global, each measurement runs a single test with `--test-threads=1 --exact`.

```bash
python3 scripts/perf/check_structural.py                 # everything
python3 scripts/perf/check_structural.py --only page     # queue, pending, page, hash, frame, soak, context, irc, skill, mcp_config, hooks, prefix
python3 scripts/perf/check_structural.py --print-plan    # only show the cargo commands that would run
python3 -m unittest -v scripts/perf/test_check_structural.py   # the gate's contract tests
```

IRC throughput is measured by a separate example rather than a time budget. Change the message count with `IRC_THROUGHPUT_MESSAGES` (default 100000).

```bash
cargo run --locked --release -p horon-agent-irc-core --example irc_throughput
```

**Raising a bound.** Bounds are committed in `scripts/perf/structural_budget.json`. This keeps a regression from raising its own ceiling. When raising one, write the reason in `notes` and do it in a separate commit. The contract test checks `notes`.

## A/B measurement

The structural gate answers only "is it bounded". "How much did it save" is answered by alternating two binaries on the same machine.

**1. Build both binaries with the same profile.** Build the comparison baseline (B0, usually `dev` right before the change) in a separate worktree. The runner rejects a pair with different profiles or feature sets.

```bash
cargo build --release -p horon-agent-base --example perf_counters_surface
git worktree add ../agent-b0 <baseline-commit>
cd ../agent-b0 && cargo build --release -p horon-agent-base --example perf_counters_surface
```

**2. Measure alternately.**

```bash
python3 scripts/perf/ab_perf.py \
  --baseline ../agent-b0/target/release/examples/perf_counters_surface \
  --candidate target/release/examples/perf_counters_surface \
  --work-id "11 - seek history pages instead of replaying the session" \
  --workload '["{binary}"]' --warmup 3 --iterations 10 --require-counters \
  --correctness-cmd "cargo test --locked -p horon-agent-harness-api-server" \
  --rollback-commit <baseline-commit> --emit .omo/evidence/ab-report.json
```

- Putting the same binary on both sides gives a control run (A/A). A difference inside the noise band is recorded as `control-no-change`, outside it as `control-drift`. Since the two binaries are identical, a difference outside the band is runner noise, and the control run never produces `adopt-pending-correctness` or `reject-regression`. That is the evidence for "no false improvement".
- p99 is reported only with 20 or more samples. With fewer it keeps the key and records `null` plus the reason.
- A metric the workload does not report becomes `null` rather than 0 and appears in `counts.missing`.

**3. Fill in the report.** Write the result in the shape of `scripts/perf/report_template.json`. The contract test checks the required keys.

| Item | Keys |
| --- | --- |
| Work, commits | `work_id`, `baseline.git_hash`, `candidate.git_hash`, `rollback_commit` |
| Environment | `machine.machine`, `machine.system`, `toolchain.rustc`, `baseline.profile`, `baseline.features`, `fixtures[].sha256` |
| Repetition | `iterations.measured_per_side`, `iterations.warmup`, `counts.failure`, `counts.timeout`, `counts.missing` |
| Time | `timing.baseline.median_ms`, `p95_ms`, `p99_ms` |
| Resources | `resources.baseline.cpu_user_ms`, `cpu_system_ms`, `read_bytes`, `write_bytes`, `allocated_peak_bytes`, `retained_bytes`, `rss_bytes`, `pss_bytes`, `queue_depth_high_water`, `fd_count`, `thread_count`, `task_high_water` |
| Judgment | `correctness.result`, `interpretation`, `adoption_decision` |

`allocated_peak_bytes` and `task_high_water` are filled by the soak report, not the runner. Keep measurement JSON under `.omo/` and do not commit it. Move the numbers the judgment needs into the PR body and `RESULTS.md`.

### Skill and MCP config loading A/B

The loading path is measured with the `loading_probe` example and `scripts/perf/bench_loading.py`. The fixture `config.toml` turns on `[mcp] claude_user = true`, so both binaries are compared with the same `~/.claude.json` in play. `warm` mode is the A/B mode, and `--compare` rejects a `cold` report or a pair with mixed modes.

```bash
cargo build --release --locked -p horon-agent-base --example loading_probe
python3 scripts/perf/bench_loading.py --binary target/release/examples/loading_probe --mode warm \
  --events 0 --skills 0,200,1000 --skill-body-bytes 8192 --mcp-config-bytes 300000 \
  --warmup 3 --iterations 30 --emit candidate-warm.json
python3 scripts/perf/bench_loading.py --binary target/release/examples/loading_probe --mode warm \
  --events 0 --skills 0,200,1000 --skill-body-bytes 0 --mcp-config-bytes 0 \
  --warmup 3 --iterations 30 --emit candidate-warm-small.json
python3 scripts/perf/bench_loading.py --compare baseline-warm.json candidate-warm.json
```

The baseline comes from running the same command with a `loading_probe` built in a worktree of the baseline commit. Counters decide first. It fails if `skill_bytes_read` in `skill_catalog`, `skill_file_reads` in `skill_request_overlay`, or `mcp_config_reads` in `mcp_config_load` grows above the baseline. Time counts as a regression only when the same pair, measured alternately again, exceeds the baseline's p95 range.

The judging measurement runs in `.github/workflows/loading-ab.yml`, not on a development machine. That workflow checks out the baseline and candidate commits inside one `ubuntu-24.04` job, builds both as release, measures the two conditions above alternately with the candidate's `bench_loading.py` (an 8 KiB body and a 300 KB `~/.claude.json`, both 0), then runs `--compare`. A "Check counters" step then verifies three counter conditions: the skill catalog with bodies reads less than the baseline, workspace skills are not read again on repeated requests, and MCP config with `~/.claude.json` reads less than the baseline. Breaking any one fails the job. `bench_tools.py` also runs against both binaries, and the result stays for 30 days as the `loading-ab-<run_id>` artifact.

```bash
gh workflow run loading-ab.yml --ref dev -f base_ref=23007c9b -f head_ref=dev
```

The `base_ref` default `23007c9b` is the commit where the loading counters first landed. An older commit has a different report `schema_version`, so `--compare` refuses the comparison and the job fails. Because it is a `workflow_dispatch` workflow, the file must be on the default branch (`main`) to run.

## The goal is to beat jcode

The direction of performance work is to beat the closest competing tool, jcode, in every comparison cell on every OS. Fixed requirement 3 in AGENTS.md (never slower than the baseline) is the floor, and this section is the goal above it.

**Where and with which command.** Measure only on GitHub Actions. The development machine runs other builds alongside and its numbers are not trustworthy.

```bash
gh workflow run benchmark.yml --ref <branch> -f os=all -f suites=compare \
  -f competitors=jcode -f oneshot_runs=5 -f baseline_ref=dev
```

The same job builds the `dev` baseline (`horon-base`) and the candidate together and measures them alternately, so before, after, and jcode share one runner.

**The cells compared.** Per OS: one-question latency and peak memory, skill invocation time, MCP call time, `--version`, install size, first-session and per-session memory growth (idle and right after a turn), and `fsync` counts on Linux. Samples are medians of 5 or more runs, and if the min–max ranges of the two values overlap the cell counts as a tie, not an advantage.

**Find causes by measuring.** Split startup, tool round trip, and shutdown with `request_offsets_ms`, find the shutdown stages with `log_tail`, find growing threads with `thread_names`, and separate live data from allocator retention with `live_heap_bytes`. Do not change code on a guess.

**Optimizations we do not make.**

- Getting faster by dropping the flush of an original (event log, blobs, local state files, runtime journal). The only exception is when a test proves nothing is lost after a crash (the `.catalog.dirty` marker, [`../protocol/SESSION_STORE.md`](../protocol/SESSION_STORE.md)).
- Speed bought with skipped validation, weakened defaults, or removed features.
- Branches that detect benchmarks, or fixtures and a fake model tuned to favor horon.

If a cell still favors jcode for one of these reasons, do not fix it; report it as a structural difference with the reason (for example, horon performs original durability flushes that jcode does not).

## What we do not do

- Do not set numeric thresholds on latency or memory. Decide them separately once the B0 distribution is fixed.
- Do not make a release binary or an account a gate condition.
- Do not recreate the startup budget of `scripts/test/fast.sh` or the gates of the existing benchmark scripts.
- Do not drop the page cache, claim a p99 from too few samples, or compare memory metrics across OSes as the same number.

## Appendix: mapping to jcode's benchmarks

jcode's (`1jehuang/jcode`, commit `c725fa3` from 2026-11-05) `scripts/` benchmarks were judged like this.

| jcode | horon | Verdict |
| --- | --- | --- |
| `benchmark_tools.sh` | `bench_tools.py` | Ported. Attaches to the socket from Python without socat. |
| `bench_reload_storm.py` | `bench_reload_storm.py` | Ported. Sessions are built with `fixtures/gen_event_log.py`. |
| `bench_compile.sh`, `bench_selfdev_checkpoints.sh`, `bench_selfdev_build.sh` | `bench_compile.py` | Ported. The `cargo clean` cold mode was dropped because it wipes the CI cache. |
| `bench_headless_memory.py`, `bench_memory_cli.py`, `bench_jcode_maki.py` | `bench_compare.py` | Ported as the competitor comparison. Measured on the headless path. |
| `bench_startup.py`, `check_startup_budget.sh`, `memory_regression_gate.sh`, `analyze_runtime_memory_log.py` | `bench_startup.py`, `check_startup.sh`, `check_memory.sh`, `analyze_memory_log.py` | Already present |
| `jcode_memory_snapshot.py`, `repro_idle_session_rss.py` | `bench_common.py`, `bench_reload_storm.py` | Other scripts measure the same axis. |
| TUI screen measurements (`bench_startup_visible_ready.py`, `bench_resume_picker.py`, `startup_frames.sh`, and others) | None | Excluded. horon is headless. |
| `bench_resume_suite.py` | None | Excluded. The reload storm measures resume latency. |
| Ones that need a real model, a paid API, or an external harness (`benchmark_swarm.py`, `run_terminal_bench_*`, `benchmark_discovery*.py`, `benchmark_browser_handoff.*`, and others) | None | Excluded |
| `analyze_root_crate.py`, `analyze_openai_ws_cache.py` | None | Excluded. Those log lines do not exist. |