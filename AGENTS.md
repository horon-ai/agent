# AGENTS.md: horon `agent/`

This directory is horon's headless agent backend. The binary speaks the OpenAI
protocol only and never opens a terminal. Other documents describe the code;
this file governs it. When another document disagrees with this file, this file
wins.

## Fixed requirements

Every change in this tree keeps all three. A change that breaks one is not
done, regardless of what else it fixes.

1. **No TUI.** The binary is headless and never opens a terminal. The
   dependency graph stays free of `ratatui`, `crossterm`, and any
   `horon-agent-tui*` crate. Do not bring back the TUI or the crossterm login
   presenter.
2. **ACP v1 and v2.** `horon-agent acp` must serve Agent Client Protocol
   `protocolVersion` 1 and 2, answering `initialize` with the highest version
   both sides support. Current state: both are implemented — v2 negotiates in
   `src/cli/acp/v2/negotiate.rs`, v1 answers from `src/cli/acp/util.rs` and
   pins to `ACP_PROTOCOL_VERSION = 1`. Never drop or break v1 while adding v2.
   The stdio contract, editor setup, and the v2 differences are in
   `docs/protocol/ACP.md`.
3. **No measured performance regression.** On the same machine, build profile,
   and workload, a change must match or beat the recorded baseline. A change
   that makes a measured path slower than the baseline does not merge. How to
   measure: `docs/performance/BENCHMARK.md`; recorded numbers and their limits:
   `docs/performance/RESULTS.md`, which is the recorded baseline.

## Performance target: beat jcode

Performance work aims to beat jcode, the closest competitor, on every OS and
every cell of the comparison benchmark, by as wide a margin as the code allows.
Requirement 3 is the floor (never slower than our own baseline); this is the
direction (faster and smaller than jcode).

- **Measure** with the `compare` suite in GitHub Actions only, never on a
  developer machine: `gh workflow run benchmark.yml --ref <branch> -f os=all
  -f suites=compare -f competitors=jcode -f oneshot_runs=5 -f baseline_ref=dev`.
  The same job builds the dev baseline (`horon-base`) beside the candidate, so
  before, after and jcode share one runner.
- **Compare** per OS (Linux, macOS, Windows): one-shot time and peak RSS,
  skill and MCP call time, `--version`, install size, first-session and
  each-additional-session memory (idle and post-turn), and Linux fsync count.
  Use the median of at least 5 runs; a difference inside the min–max overlap
  is a tie, not a win.
- **Find the cause before changing code**: split a run with
  `request_offsets_ms`, `log_tail`, `thread_names` and `live_heap_bytes`
  (`docs/performance/BENCHMARK.md`), not by guessing.
- **Never** buy speed with durability or correctness: no dropped flush of an
  original (event log, blob, local state, runtime journal), no skipped
  validation, no weaker default, no benchmark-detecting branch, no change to
  the fixture or mock to favour horon. A flush may go only when a test proves
  a crash still loses nothing (precedent: the catalog dirty marker,
  `docs/protocol/SESSION_STORE.md`). A cell jcode keeps for such a reason is
  reported as a structural difference with that reason.

## What this build is

- One headless binary, `horon-agent`. Working subcommands: `serve` (agent
  daemon), `run` (send one message and exit), `api-bridge` (alias `api`;
  harness API on a Unix socket, Unix only), and `acp` (ACP adapter on stdio).
- One model path: the agent POSTs `/chat/completions` to the OpenAI-compatible
  endpoint of the profile named by `[provider] default_provider` or
  `--provider-profile`. A profile is a `[providers.<name>]` table with
  `type = "openai-compatible"`, `base_url`, `api_key_env` or `env_file`,
  `auth`, `default_model`, and optional `[[providers.<name>.models]]`. Nothing
  else serves models in this build: no second provider runtime, no router
  process, no plugin host. External tools come in over MCP, which stays.
- Core tool profile, the headless default (15 tools): `bash`, `read`, `write`,
  `edit`, `replace`, `apply_patch`, `agentgrep`, `ls`, `webfetch`, `websearch`,
  `skill_manage`, `mcp`, `monitor`, `undo`, `ask`. `skill_manage` and `mcp` stay
  registered. A `core` session sends seven of them eagerly (`read`, `write`,
  `edit`, `apply_patch`, `bash`, `ls`, `agentgrep`) plus `toolbox`, names the
  other eight in the system prompt, and the model reaches them through
  `toolbox` (`docs/runtime/TOOLS.md`).

## Configuration that must keep its default

|Setting|Default|Other values|
|---|---|---|
|Tool profile (`--tool-profile`, `HORON_AGENT_TOOL_PROFILE`, `[tools] profile`)|`core`|`full`, `acp`, `minimal`/`lite`, `none`|
|`[acp] profile`|`standard`|`extended`, `full`|
|`[tools] redo`, `hashline_edit`|`false`|`true`|
|`[tools] ask_policy`|`recommended`|`error`|
|`[tools] bash_pty`|`auto`|`off`, `always`|
|`[tools] irc`|`false`|`true`; `HORON_AGENT_IRC` overrides it for one process|
|`[irc] mailbox_cap`, `max_body_bytes`, `wait_max_secs`, `broadcast_concurrency`|`100`, `65536`, `300`, `16`|any value ≥ 1|
|`[features] crash_bundles`|`true`|`false`|
|`[features] turn_recovery`|`conservative`|`aggressive`, `off`|
|`[features] foreign_rules`, `stream_rules`|`false`|`true`|
|`[storage] sync`|`full`|`fast`; `HORON_AGENT_STORAGE_SYNC` overrides it for one process, and an unknown value falls back to `full` with one warning|
|`[features] pricing_refresh`|`false`|`true`; `HORON_AGENT_PRICING_REFRESH=1` or `0` overrides it for one process|
|`[server] idle_hibernation_secs`|`1800`|`0` disables the sweep; `HORON_AGENT_IDLE_HIBERNATION_SECS` overrides it for one process|
|Update check on `serve`|not run|`--no-update` skips it everywhere|

`[storage] sync` names a durability level, not a syscall: `full` is
power-loss durable (macOS `F_FULLFSYNC`) and `fast` is the cheapest
crash-durable flush (plain `fsync` on macOS, the same as `full` elsewhere).
Originals are confirmed with it and derived caches are rebuilt instead, which
`docs/architecture/CRATES.md` states as the boundary. `pricing_refresh` off
keeps a one-shot run off the network; a catalog already on disk is still read.

`[server] idle_hibernation_secs` is the idle window before the daemon releases
a session's derived caches. It drops the provider projection and the tool-output
index and writes nothing, so the next turn rebuilds both from resident state
without a disk read; `docs/performance/MEMORY.md` lists what is released and
what stays. `0` disables the sweep, and the daemon checks every ten seconds, so
the release lands up to one tick after the threshold passes.

Tools outside the core set (swarm, memory, session search, todo, irc) stay in
the tree but off.
`irc` is agent-to-agent messaging inside one daemon; `[tools] irc = true` adds
it to every non-empty allow list, and with the switch off it refuses to run
even under `full` (`docs/runtime/IRC.md`). Under `full`, the optional tools
load as deferred definitions behind `toolbox`
(`docs/runtime/TOOLS.md`). `[tools.load_mode]` overrides one tool's tier
(`essential`, `discoverable`, `off`). `ask = "essential"` sends a listed core
tool eagerly again, and `toolbox = "off"` turns listing off so all 15 core
tools are eager. Sessions under `acp`, `minimal`, and `none`, one with an
explicit `enabled` list, and a `core` session with an SDK `enabled` list send
every tool they allow eagerly and get no index. Under `full`, an SDK list keeps
deferring the non-core tools.

Two settings are environment-only. `HORON_AGENT_JOBS_ARTIFACT_RETENTION` is
`rotate` by default and reclaims the oldest background task artifacts when the
budget is full; `preserve` refuses to start a new task instead.
`HORON_AGENT_WORKSPACE_TRUST` accepts `trust` or `deny` for one process;
project-local MCP config is skipped unless its digest was trusted, and
`--trust-workspace` records the decision (`docs/safety/SECURITY.md`).

## Build, run, verify

```bash
cargo build -p horon-agent
./target/debug/horon-agent --version
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --workspace
```

Requirement 1 check. Each command must fail with "did not match any packages":

```bash
cargo tree -p horon-agent -i ratatui
cargo tree -p horon-agent -i crossterm
cargo tree -p horon-agent -i horon-agent-tui
```

Requirement 2 check: `cargo test -p horon-agent --lib acp` (ACP unit tests in
`src/cli/acp.rs`). Requirement 3 check: the structural gate and A/B runner in
`docs/performance/BENCHMARK.md`.

## CI warnings: zero is the completion condition

A task is not finished while any job of its CI run shows a warning. A green run
with warnings is not done: keep fixing until a run on the final commit shows none,
and do not merge or report success before then.

- **What counts.** Any warning in any job log of any workflow the change
  triggers (`CI`, `Structural performance`, `Release Recovery Validation`, ...):
  rustc, clippy and cargo `warning:`, rustup `warn:`, cargo-audit `Warning:`,
  script `warning:` lines, `cargo:warning=` build-script output, and `##[warning]`
  or `warning`-level annotations.
- **How to check.** When the run has finished, run
  `python3 scripts/ci/run_warnings.py <run-id>` (needs a signed-in `gh`; find the
  id with `gh run list --branch <branch>`). It prints every distinct warning with
  its job and step and exits 0 only when there are none. Paste that result, not
  the green check marks, as the evidence.
- **Fix the cause.** Do not hide a warning with `#[allow]`, `-A`, `--ignore`, a
  lowered log level or a renamed message. An external cause this repository cannot
  fix (an unmaintained transitive crate, a Rust toolchain defect) may be allowed at
  the narrowest scope, with the reason beside it and named in the report. Existing
  cases: the `audit_ignores` list in `scripts/ci/check_security.sh` (triage in
  `docs/safety/SECURITY.md`) and the `linker_messages` allow in
  `crates/app-core/src/lib.rs`.
- **Gates that keep it at zero.** `ci.yml` sets `CARGO_BUILD_WARNINGS=deny`, so a
  rustc or build-script warning in a workspace crate fails the build, and
  `scripts/ci/check_security.sh` runs `cargo audit --deny warnings`. A new
  advisory in the RustSec database can therefore turn CI red without a code
  change; triage it the same way. `notice`-level annotations (the macOS runner
  capacity notice) are GitHub's and are not warnings.

## CI gates by change type

`ci.yml` and `structural-perf.yml` start with a `Changed Paths` job
(`scripts/ci/classify_changes.py`) that classifies a pull request by its files.

- **Documentation only** (README, `docs/`, `AGENTS.md`, templates): runs
  `Documentation Checks` (links, UTF-8, English/Korean pairs) and
  `Documentation Corpus Test` (`docs/` is compiled into the binary, so the
  `horon_agent_docs` tests still run). The release build, the three-OS test
  matrix, the SDK jobs and the structural performance gate are skipped.
- **Anything else** (code, workflows, scripts, dependencies, lockfiles): every
  gate runs.
- **Pushes and manual dispatches always run everything.** Release evidence
  (`scripts/release/check_ci.py`) accepts only a fully successful push run.
- `CI Required` and `Structural performance required` always finish and fail
  only when a gate failed or was cancelled; use them as the required checks so
  a skipped gate never leaves a check pending.
- Every action is pinned to a commit with a `# vX` comment (Dependabot keeps them
  current); `release.yml`, `release-tag.yml` and the publish workflows are not
  part of this gating. `actionlint` must report nothing (`Workflow Lint`).
- **Superseded runs are cancelled outside `main`.** A newer run on the same ref
  cancels the older one on every ref except `main`, whose push runs are release
  evidence and always finish. On `dev` a push and a manual dispatch share the
  group and cancel each other; after a cancelled `dev` push run, use `Re-run all
  jobs` on that run. Never fast-forward `dev` to a `main` commit (the sync is the
  merge-commit PR): a cancelled `dev` run would then be the newest push run for a
  release commit, and `scripts/release/check_ci.py` rejects it.

## Release and branch sync: dev into main, then main back into dev

`dev` is where work lands; `main` is what ships. Merging into `main` publishes a
release, so do it only when the owner asks. The two branches differ by merge
commits and automatic `docs(perf)` commits, which is why a release is followed by
a sync back.

1. **Release.** Check the latest `dev` CI run with `scripts/ci/run_warnings.py`
   (zero warnings, all jobs green). Open a PR with base `main`, head `dev`, titled
   `Release: dev into main (vX.Y.Z)`, `Refs` the issues it carries, and list what
   it ships. Merge it with a merge commit, never squash. After the CI push run on
   `main` succeeds, `release-tag.yml` tags the merge commit, runs `release.yml`
   and pushes `ghcr.io/horon-ai/agent`; watch `gh run list --branch main` until
   the release run ends.
2. **Sync back.** When the release has finished, open a PR with base `dev`, head
   `main`, titled `Sync: main into dev`, and merge it with a merge commit so `dev`
   holds every commit `main` has. It needs a linked issue like any PR.
3. **Confirm.** `git rev-list --left-right --count origin/dev...origin/main` shows
   `0` commits missing on each side except the sync merge itself, and
   `git diff origin/dev origin/main` is empty.

## Docs

`docs/` is grouped by topic into `architecture/`, `protocol/`, `runtime/`,
`provider/`, `safety/`, `performance/`, and `release/`. File names are
uppercase (`docs/runtime/TOOLS.md`). Extend an existing doc before adding a
new file. Docs are bilingual and English is the default: `X.md` is the
canonical English text and `X.ko.md` is its Korean counterpart in the same
folder, as with `README.md` and `README.ko.md`. Change both in the same commit.
Each pair keeps the same headings, tables, code fences and links, and links
between docs stay inside their own language. `python3 scripts/ci/check_docs_pairs.py`
checks this and runs in the Quality Guardrails job.

Every English Markdown file under `docs/`, including subfolders, and the root
`README.md` are compiled into the binary and served by the `horon_agent_docs`
tool (`crates/app-core/build.rs`); `.ko.md` files are not bundled.
A moved or deleted doc must leave no stale path in code, scripts, or tests.
`docs/README.md` is the index; update it in the same change.
