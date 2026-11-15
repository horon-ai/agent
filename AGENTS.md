# AGENTS.md: horon `agent/`

This repository is horon agent: one agent with two equal modes. In headless
mode a program drives a session (`run`, the `serve` daemon behind the SDKs and
`api-bridge`, `acp` for editors, `rpc` for host programs). In TUI mode a person
drives one from the terminal through `horon-tui`. Both modes share one engine,
daemon, session store, tool set and configuration; they differ only in who is
on the other end, and neither is a reduced form of the other. Model calls speak
one of four wires, picked by the profile's `type`: OpenAI Chat Completions,
OpenAI Responses, Anthropic Messages or Gemini generateContent. Other documents
describe the code; this file governs it. When another document disagrees with
this file, this file wins.

## Modes, binaries and release units

|Unit|Source|What it serves|How it ships|
|---|---|---|---|
|`horon-agent`, the agent binary|root package (`src/`, `crates/*`)|headless mode, the daemon both modes attach to, and the plain `repl` line prompt|`horon-agent-<suffix>.tar.gz` on every release; every installation needs it|
|`horon-tui`, the TUI binary|`crates/tui`, a client of `horon-agent-sdk`|TUI mode|`horon-tui-<suffix>.tar.gz` beside the agent in the same release; optional, never on FreeBSD|
|`@horon-ai/horon` and six platform packages|`packages/npm`, `scripts/npm`|both binaries through one launcher|`publish-npm.yml`; dry run until `NPM_PUBLISH` is `true`; not published yet|
|SDKs|`crates/agent-sdk`, `packages/sdk/typescript`, `packages/sdk/python`|headless mode from code|TypeScript: `publish-typescript-sdk.yml` (same `NPM_PUBLISH` gate); Python: `publish-python-sdk.yml`, manual dispatch into the `pypi` environment; neither published yet|
|Container image|`scripts/release/Containerfile`|the daemon in headless mode|`ghcr.io/horon-ai/agent`, pushed by `release-tag.yml`|

- **One release.** Both binaries are built by the same `release.yml` run from
  the same tagged commit (`vMAJOR.MINOR.PATCH`), and `install-release` stages
  them into one `builds/versions/<version>/` directory that activates as a
  unit. There is no separate TUI release or channel. The version strings
  differ: `horon-agent --version` comes from the tag (`crates/build-meta`),
  `horon-tui --version` from the `crates/tui` package version. The npm version is the tag
  without its `v` (`docs/release/NPM.md`).
- **Terminology.** "Headless" names a mode, nothing else. Do not call the
  package, binary, engine or product "headless": say "the agent binary
  `horon-agent`" and "the TUI binary `horon-tui`". A session with no client
  attached (swarm workers, ambient resumes) is an *unattended session*. Code
  identifiers, file names and config values that still say `headless`
  (`check_headless_graph.py`, `is_headless`, `spawn_mode = "headless"`) keep
  their names until a separately approved rename.

## Fixed requirements

Every change in this tree keeps all three. A change that breaks one is not
done, regardless of what else it fixes.

1. **No terminal UI in the agent binary.** The build ships two binaries, and
   the split is the rule. `horon-agent` (`serve`, `run`, `repl`, `api-bridge`,
   `acp`, `rpc`) never opens a full-screen terminal UI, and its dependency
   graph stays free of `ratatui`, `crossterm`, `horon-tui`, and any
   `horon-agent-tui*` crate. TUI mode is the separate package and binary
   `horon-tui`, which talks to the daemon through `horon-agent-sdk` and never
   moves the terminal stack into `horon-agent`. Bare `horon` draws nothing
   itself: when stdin and stdout are terminals, `HORON_NO_TUI` is unset, and
   `horon-tui` sits beside the running binary, it replaces its own process with
   that client (`src/cli/tui_handoff.rs`); otherwise it stays in headless mode
   and prints why. `scripts/ci/check_headless_graph.py` enforces the graph rule
   in CI.
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
   `docs/performance/RESULTS.md`, which is the recorded baseline. Scope: the
   baseline and the jcode target below cover `horon-agent` workloads (headless
   mode), and those cells keep their definition unchanged now that a second
   binary exists. `horon-tui` is measured on its own, never mixed into a
   `horon-agent` cell and never compared against one: its suites time
   `horon-tui` against `horon-tui`, and a `horon-agent` number quoted for a
   change that only touches the TUI is not evidence.

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

## Design principles

Short form of `PHILOSOPHY.md` (Korean: `PHILOSOPHY.ko.md`); this file keeps
contract priority, and the Fixed requirements above are unchanged.

Principles:

1. Beat jcode in every cell and never fall below the recorded baseline.
2. Support ACP v1 and v2.
3. Headless mode and TUI mode are equal modes on one engine. The agent binary
   has no terminal UI; TUI mode is the separate `horon-tui` package and binary
   on the SDK, released with the agent from the same tag.
4. The engine is usable from server code through the Rust, TypeScript and
   Python SDKs.
5. Claims are measured in CI, and durability is never traded for speed.

A proposed change fits when it:

1. reduces per-request tokens or per-turn CPU, fsync or memory;
2. is measurable by CI `compare` or the structural gate;
3. does not grow the `horon-agent` dependency graph or the binary;
4. does not break ACP v1/v2, the SDKs or stored sessions;
5. does not reintroduce removed surface under another name.

Does not do: forced tool-call steps, streaming dry-run, a `report_issue` tool,
per-tool `family@rev` versions, a KDL model catalog, wholesale `anyhow` or
`SmallVec` replacement, or a cutover of external contracts (ACP v1, harness API
v1, stored sessions). Reasons and sources: `PHILOSOPHY.md`.

## What this build is

- Two binaries. The agent binary, `horon-agent`, has these working
  subcommands: `serve` (agent daemon), `run` (send one message and exit),
  `repl` (plain line prompt, no full-screen UI), `api-bridge` (alias `api`;
  harness API on a Unix socket, Unix only), `acp` (ACP adapter on stdio), and
  `rpc` (stdio JSONL RPC adapter; `--mode rpc` selects the same adapter), plus
  maintenance commands such as `update` and `install-release`. The TUI binary,
  `horon-tui`, is a separate package and binary on `horon-agent-sdk`; it is
  built with its own cargo invocation (`-p horon-tui`) so its feature set never
  unifies with `horon-agent`. Bare `horon` on a terminal replaces itself with
  the sibling `horon-tui` once the server is up; a pipe, `HORON_NO_TUI`, or a
  missing sibling stays in headless mode, names the condition and prints the
  install hint.
- One model path: the agent POSTs to the endpoint of the profile named by
  `[provider] default_provider` or `--provider-profile`, and the profile's
  `type` picks the wire. There are four: `openai-compatible` (Chat Completions,
  `POST {base_url}/chat/completions`, the default), `openai-responses`
  (`POST {base_url}/responses`), `anthropic-compatible` (Anthropic Messages,
  `POST {base_url}/messages`) and `gemini-compatible` (Gemini generateContent,
  `POST {base_url}/models/{model}:streamGenerateContent?alt=sse`). A `base_url`
  that already ends with the wire's path is used as it is, and `open-router` is
  rejected at runtime by a message that lists the four values. A profile is a
  `[providers.<name>]` table with `type`, `base_url`, `api_key_env` or
  `env_file`, `auth`, `default_model`, and optional
  `[[providers.<name>.models]]`. Omitting `auth` applies the type default:
  `Authorization: Bearer` for the two OpenAI kinds, `x-api-key` for Anthropic
  and `x-goog-api-key` for Gemini; an explicit `auth` always wins. There is
  still one provider runtime, no router process, no plugin host. External tools
  come in over MCP, which stays. The one documented exception is the optional
  `generate_image` tool: when the active profile names `roles.image`, it POSTs
  `/images/generations` and `/images/edits` on that same profile, with the same
  base URL, credential and `extra_body`. It is a route beside the wires, not a
  runtime, and it is off unless `[tools] generate_image = true`. The `image`
  role is therefore the one role whose model does not speak Chat Completions,
  it is served only by an `openai-compatible` or `openai-responses` profile,
  and it is the only role with no text fallback.
- Core tool profile, the default when a launch selects none (18 tools): `bash`, `read`, `write`,
  `edit`, `replace`, `apply_patch`, `agentgrep`, `ls`, `webfetch`, `websearch`,
  `skill_manage`, `mcp`, `monitor`, `undo`, `ask`, `todo`, `session_search`, `swarm`.
  `skill_manage` and `mcp` stay registered. A `core` session sends seven of them
  eagerly (`read`, `write`, `edit`, `apply_patch`, `bash`, `ls`, `agentgrep`)
  plus `toolbox`, names the other eleven in the system prompt, and the model
  reaches them through `toolbox` (`docs/runtime/TOOLS.md`). An SDK-driven
  session keeps `session_search` and `swarm` off unless its `enabled` list names
  them. `horon-agent acp` is the exception to the default: with no explicit
  profile it applies `[acp] tool_profile`, which is `acp`.

## Repository layout and crate boundaries

|Path|Holds|
|---|---|
|`src/`|the `horon-agent` package: agent loop, daemon, harness API bridge, ACP and RPC adapters, CLI dispatch|
|`crates/`|the workspace crates; `crates/tui` is the `horon-tui` package, `crates/agent-sdk` the Rust SDK|
|`tests/`, `crates/*/tests/`|integration targets|
|`docs/`|behaviour documents in English/Korean pairs; the English files are compiled into the agent binary|
|`packages/sdk/typescript`, `packages/sdk/python`|the TypeScript and Python SDKs|
|`packages/npm/horon`|the `@horon-ai/horon` launcher|
|`packages/docs-site`|the agent.horon.ai site: a Mintlify project. `reference/` and `images/reference/` are generated from `docs/` by `scripts/ci/sync_docs_site.py`; every other page is hand-written|
|`scripts/ci`, `scripts/perf`, `scripts/release`, `scripts/install`, `scripts/npm`|gates, benchmarks, release, installer and packaging scripts|

Boundaries, each with the gate that enforces it:

- `horon-agent` reaches no `ratatui`, `crossterm`, `horon-tui` or
  `horon-agent-tui*` package, dev-dependencies included
  (`scripts/ci/check_headless_graph.py`).
- `horon-tui` is a client: its only workspace dependencies are
  `horon-agent-sdk` and the contract crate `horon-agent-tool-types`, never an
  engine crate, and it grows no engine of its own
  (`crates/tui/Cargo.toml`, `docs/architecture/CRATES.md`; reviewed, no gate
  yet).
- A `horon-agent-*-types` crate is a data contract. It never depends on the
  root package, `horon-agent-core`, or the runtime and provider crates listed
  in `scripts/ci/check_dependency_boundaries.py`.
- OS-specific `cfg` lines live in the five `crates/platform*` crates. Every
  line elsewhere is counted per file, with a reason, in
  `scripts/ci/platform_cfg_allowlist.json` (`scripts/ci/check_platform_cfg.py`).
- The Rust and TypeScript SDK surfaces stay in parity
  (`cargo test --locked -p horon-agent-sdk parity`).
- Whether a type or helper leaves the root package is decided by the rules in
  `docs/architecture/CRATES.md`, not case by case.

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
|`[features] effort_keywords`|`true`|`false`|
|`[prewalk] enabled`|`false`|`true`, which then requires `into` (a model id or `role:<name>`) that passes config validation|
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

Tools outside the core set (memory, irc) stay in the tree but off.
`irc` is agent-to-agent messaging inside one daemon; `[tools] irc = true` adds
it to every non-empty allow list, and with the switch off it refuses to run
even under `full` (`docs/runtime/IRC.md`). Under `full`, the optional tools
load as deferred definitions behind `toolbox`
(`docs/runtime/TOOLS.md`). `[tools.load_mode]` overrides one tool's tier
(`essential`, `discoverable`, `off`). `ask = "essential"` sends a listed core
tool eagerly again, and `toolbox = "off"` turns listing off so all 18 core
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
cargo build -p horon-tui
./target/debug/horon-agent --version
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --workspace
```

Run before pushing, by what the change touches:

|Change|Command|
|---|---|
|Anything|`scripts/ci/check_all.sh`: the Format and Quality Guardrails steps of `ci.yml`. `--skip-slow` leaves out `cargo check`, clippy and `cargo machete`; `--fix` formats and rebaselines the ratchets|
|Rust code|the `cargo` lines above, with the tests of every crate touched|
|`crates/tui`|`cargo clippy --locked --all-targets --all-features -p horon-tui -- -D warnings`|
|`docs/`, README, `PHILOSOPHY*.md`|`python3 scripts/ci/check_docs_links.py`, `python3 scripts/ci/check_docs_pairs.py`, `python3 -m unittest scripts/ci/test_docs_install_claims.py`|
|`.github/workflows`|`actionlint`|
|After the push|`python3 scripts/ci/run_warnings.py <run-id>` on the finished run (next section)|

Requirement 1 check. Each `cargo tree` command must print "did not match any
packages", and the graph gate must exit 0:

```bash
cargo tree -p horon-agent -i ratatui
cargo tree -p horon-agent -i crossterm
cargo tree -p horon-agent -i horon-tui
cargo tree -p horon-agent -i horon-agent-tui
python3 scripts/ci/check_headless_graph.py
```

The `cargo tree` lines cover those names only; `check_headless_graph.py` walks
every package reachable from `horon-agent` and fails on the `horon-agent-tui*`
prefix too.

Requirement 2 check: `cargo test -p horon-agent --lib acp` (ACP unit tests in
`src/cli/acp.rs`). Requirement 3 check: the structural gate and A/B runner in
`docs/performance/BENCHMARK.md`.

## Tests

- **A test counts only when CI runs it.** An integration target (`tests/*.rs`,
  `crates/*/tests/*.rs`) is listed in `scripts/ci/integration_targets.json`
  with its `min_passed` count, or in `scripts/ci/test_target_allowlist.json`
  with the reason it does not run (`scripts/ci/check_test_targets.py`). A lib
  test module must be selected by a cohort filter in `ci.yml`
  (`scripts/ci/check_lib_test_coverage.py`). A test that only compiles is not
  evidence.
- **A skip is written down.** A case left out of an integration target is
  named in that target's `skipped` list with a `skip_reason`.
- **Isolation.** Point `HORON_HOME` at a temporary directory and never read or
  write the real `~/.horon`. Tests that change process environment serialize
  themselves, as `crates/storage/src/active_pids.rs` does.
- **Size.** A test file over 1,200 lines is tracked in
  `scripts/ci/test_size_budget.json`: a tracked file may not grow and a new one
  fails. Put a large new suite in its own file.
- **Gates are tested too.** A gate script ships with its contract tests
  (`scripts/ci/test_*.py`), and CI runs them.
- **Performance and durability.** A performance claim is proven by the
  structural gate (counters, `HORON_AGENT_PERF_COUNTERS=1`) or the Actions
  benchmark, never by a timing run on a developer machine. A removed flush
  needs the crash test named under "Performance target".

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
  `docs/safety/SECURITY.md`) and the `linker_messages` allows for unoptimized
  macOS links in `crates/app-core/src/lib.rs` and `src/main.rs`.
- **Gates that keep it at zero.** `ci.yml` sets `CARGO_BUILD_WARNINGS=deny`, so a
  rustc or build-script warning in a workspace crate fails the build, and
  `scripts/ci/check_security.sh` runs `cargo audit --deny warnings`. A new
  advisory in the RustSec database can therefore turn CI red without a code
  change; triage it the same way. `notice`-level annotations (the macOS runner
  capacity notice) are GitHub's and are not warnings.

## CI gates by change type

`ci.yml` and `structural-perf.yml` start with a `Changed Paths` job
(`scripts/ci/classify_changes.py`) that classifies a change by its files: a pull
request against its base, and a first-attempt push to `dev` against the head of
the last successful `dev` push run of the same workflow (two-dot diff).

- **Documentation only** (README, `PHILOSOPHY*.md`, `docs/`, `AGENTS.md`, templates): runs
  `Documentation Checks` (links, UTF-8, English/Korean pairs) and
  `Documentation Corpus Test` (`docs/` is compiled into the binary, so the
  `horon_agent_docs` tests still run). The release build, the three-OS test
  matrix, the SDK jobs and the structural performance gate are skipped.
- **Anything else** (code, workflows, scripts, dependencies, lockfiles): every
  gate runs.
- **Site pages count as code.** `packages/docs-site/` is not in the documentation
  list of `classify_changes.py`, so a change to a site page runs every gate.
- **Pushes to `main`, manual dispatches and re-runs of a `dev` push always run
  everything.** A `dev` push whose head is already in `origin/main`, and any
  lookup or diff failure, also runs everything. Release evidence
  (`scripts/release/check_ci.py`) accepts only a fully successful push run; a
  documentation-only `dev` run is not release evidence, and `Re-run all jobs` on
  that run produces a full run.
- `CI Required` and `Structural performance required` always finish and fail
  only when a gate failed or was cancelled; use them as the required checks so
  a skipped gate never leaves a check pending.
- Every action is pinned to a commit with a `# vX` comment (Dependabot keeps them
  current); `release.yml`, `release-tag.yml` and the publish workflows are not
  part of this gating. `actionlint` must report nothing (`Workflow Lint`).
- **Superseded runs are cancelled outside `main`.** A newer run on the same ref
  cancels the older one on every ref except `main`, whose push runs are release
  evidence: a running one is never cancelled, though GitHub can still replace a
  run that is only pending. On `dev` a push and a manual dispatch share the
  group and cancel each other; after a cancelled `dev` push run, use `Re-run all
  jobs` on that run. Never fast-forward `dev` to a `main` commit (the sync is the
  merge-commit PR): a cancelled `dev` run would then be the newest push run for a
  release commit, and `scripts/release/check_ci.py` rejects it.

## Working rules: worktree, branch, commit, pull request

- **Worktree.** Change files in a git worktree at `.hyper/worktree/<name>`
  (`.hyper/` is ignored), cut from `dev`, one task per worktree. Run every
  command and edit with that worktree as the working directory. Never edit
  another session's worktree. Remove the worktree and the branch once the merge
  is confirmed.
- **Branch.** `<type>/<slug>`, with the type of the commit (`feat/`, `fix/`,
  `perf/`, `docs/`). The base and the pull request target are `dev`. `main`
  changes only through the release pull request below, or through the
  documentation refresh of `main` described there.
- **Commit.** Conventional Commits: `type(scope): summary`. A commit an agent
  writes ends with the trailer
  `Co-authored-by: Horon AI Bot <353694987+horon-ai-bot[bot]@users.noreply.github.com>`.
- **Pull request.** It must reference an existing issue of this repository
  (`require-issue.yml`; a pull request number does not count, and only
  Dependabot is exempt). The body lists what changed and the evidence for it,
  and either the documents updated for the change or one line saying why no
  document describes it (see "Docs follow the code"). Squash-merge into `dev`
  after every job is green and `run_warnings.py` prints no warning. Never
  force-push.

## Release and branch sync: dev into main, then main back into dev

`dev` is where work lands; `main` is what ships. Merging into `main` publishes a
release, so do it only when the owner asks. The two branches differ by merge
commits and automatic `docs(perf)` commits, which is why a release is followed by
a sync back.

1. **Release.** Check the newest `dev` CI run in which the code gates ran with
   `scripts/ci/run_warnings.py` (zero warnings, all jobs green, not cancelled);
   if the latest `dev` run is documentation-only, check the newest one before it
   that ran the code gates, or use `Re-run all jobs` on the latest. Open a PR with
   base `main`, head `dev`, titled `Release: dev into main (vX.Y.Z)`, `Refs` the
   issues it carries, and list what it ships. Merge it with a merge commit,
   never squash. After the CI push run on `main` succeeds, `release-tag.yml`
   tags the merge commit, runs `release.yml` and pushes
   `ghcr.io/horon-ai/agent`; watch `gh run list --branch main` until the release
   run ends.
2. **Sync back.** When the release has finished, open a PR with base `dev`, head
   `main`, titled `Sync: main into dev`, and merge it with a merge commit so `dev`
   holds every commit `main` has. It needs a linked issue like any PR.
3. **Confirm.** `git rev-list --left-right --count origin/dev...origin/main` shows
   `0` commits missing on each side except the sync merge itself, and
   `git diff origin/dev origin/main` is empty.

**Documentation refresh of `main`.** Between releases the documents on `main`
can lag the code `main` ships. They are refreshed without a release, as in
#327 and #328: land the refresh on `dev` first, then cherry-pick it onto a
`docs/` branch cut from `origin/main`, re-check every changed statement,
pointer and diagram against `main`'s code, and keep `main`'s values where
`dev` has changes `main` does not. Leave the generated blocks of
`docs/performance/RESULTS.md` as they are on `main`. The pull request has base
`main`, carries documentation only, and is squash-merged with `[skip ci]` in
the commit subject, so the push to `main` starts no CI run and
`release-tag.yml` creates no tag. The next release and sync back reconcile the
two copies.

## Docs

`docs/` is grouped by topic into `architecture/`, `protocol/`, `runtime/`,
`provider/`, `safety/`, `performance/`, `release/`, and `diagram/`. File names
are uppercase (`docs/runtime/TOOLS.md`). Extend an existing doc before adding a
new file. Docs are bilingual and English is the default: `X.md` is the
canonical English text and `X.ko.md` is its Korean counterpart in the same
folder, as with `README.md` and `README.ko.md`. Change both in the same commit.
Each pair keeps the same headings, tables, code fences and links, and links
between docs stay inside their own language. `python3 scripts/ci/check_docs_pairs.py`
checks this and runs in the Quality Guardrails job.

`docs/diagram/` holds hand-written SVG diagrams of the code on the branch they
are on, each as `NN-name.svg` with English labels and `NN-name.ko.svg` with
Korean labels, and `docs/diagram/README.md` (with its Korean pair) shows them on
one page with the files each one is drawn from. They follow the style of
`docs/assets/architecture.svg`: self-contained, no script or external
reference, light and dark colours through `prefers-color-scheme`.

Every English Markdown file under `docs/`, including subfolders, and the root
`README.md` are compiled into the binary and served by the `horon_agent_docs`
tool (`crates/app-core/build.rs`); `.ko.md` files are not bundled. The root
`PHILOSOPHY.md` pair is checked like the READMEs but is not bundled.
A moved or deleted doc must leave no stale path in code, scripts, or tests.
`docs/README.md` is the index; update it in the same change.

**Docs follow the code.** A change that alters behaviour a document describes
updates that document, its Korean pair, and the diagram that draws it in the
same pull request, not in a follow-up. A change is not done while a document
still states the old behaviour. To find the pages:

1. Search for every changed file path, symbol, CLI flag, config key and
   default: `rg -n '<path-or-name>' README.md README.ko.md AGENTS.md docs/`.
   A page that names it describes it.
2. Check the area table below for pages that describe the behaviour without
   naming the file.
3. Re-read each hit against the new code: counts (tools, fields, profiles),
   protocol minor versions, defaults, `path:line` pointers, and command
   output. Update it to the current tense; history goes in the pull request
   body, not in the document.

|Changed area|Documents to check|
|---|---|
|`src/cli/` commands, flags and dispatch, `src/main.rs`|`README.md`, `docs/release/INSTALL.md`, `docs/diagram/02-run-modes`|
|`src/cli/tui_handoff.rs`, `crates/tui/`|`docs/runtime/TUI.md`, `docs/diagram/02-run-modes`|
|`src/cli/acp/`, `src/cli/acp.rs`|`docs/protocol/ACP.md`, `docs/diagram/05-acp-negotiation`|
|`src/cli/rpc/`|`docs/protocol/RPC.md`|
|`crates/harness-api*`, `crates/agent-sdk`, `packages/sdk/`|`docs/protocol/HARNESS_API.md`|
|`crates/app-core/src/server/`|`docs/architecture/SERVER.md`, `docs/runtime/TURNS.md`, `docs/diagram/03-turn-lifecycle`|
|`crates/session-store/`, `crates/storage/`, `[storage] sync`|`docs/protocol/SESSION_STORE.md`, `docs/architecture/CRATES.md`, `docs/diagram/06-session-storage`|
|`crates/app-core/src/tool/`, `crates/tool-*`, `crates/edit-safety/`, tool profiles|`docs/runtime/TOOLS.md`, `docs/runtime/TOOL_SCHEDULING.md`, `docs/diagram/07-tool-loading`, "What this build is" above|
|`crates/provider-*`, `crates/base/src/provider*`, `crates/config-types/`|`docs/provider/MODEL.md`, `docs/diagram/04-provider-wires`, "What this build is" above|
|`crates/irc-core/`, `crates/app-core/src/server/irc/`|`docs/runtime/IRC.md`|
|`crates/plan/`, `crates/swarm-core/`|`docs/runtime/SWARM_TASK_GRAPH.md`|
|approval, workspace trust, crash bundles, `scripts/ci/check_security.sh`|`docs/safety/SAFETY.md`, `docs/safety/SECURITY.md`|
|`crates/platform*`, a crate added, removed or renamed in `Cargo.toml`|`docs/architecture/CRATES.md`, "Repository layout" above, `docs/diagram/01-system-overview`|
|`.github/workflows/`, `scripts/ci/classify_changes.py`, `scripts/release/`|"CI gates by change type" and "Release and branch sync" above, `docs/release/`, `docs/diagram/08-branch-release-flow`|
|`scripts/perf/`, `benchmark*.yml`, `scripts/perf/structural_budget.json`|`docs/performance/BENCHMARK.md`, `docs/performance/MEMORY.md`|
|`packages/npm/`, `publish-*.yml`|`docs/release/NPM.md`, `docs/release/INSTALL.md`|
|a value in "Configuration that must keep its default"|that table and the page that documents the setting|

The same rule covers `README.md`, this file, and `packages/docs-site` (see
"Two documentation sets" below). A code pull request that needs no document
change says so in its body with the reason.

**Wording.** Follow the terminology in "Modes, binaries and release units":
"headless" and "TUI" name the two modes, and the binaries are called the agent
binary and the TUI binary. The `<!-- bench-charts -->` and `<!-- bench-auto -->`
blocks of `docs/performance/RESULTS.md` are written by
`scripts/perf/bench_docs.py` in `benchmark-docs.yml`; change the script, not the
block. A statement that also appears on a `packages/docs-site` page is changed there
in the same pull request.

## Not without the owner's explicit request

- **Releasing.** Merging into `main`, pushing a tag, or dispatching
  `release.yml` or `release-tag.yml`. A `main` merge publishes a release.
- **Publishing.** The only paths to npm and PyPI are `publish-npm.yml`,
  `publish-typescript-sdk.yml` and `publish-python-sdk.yml`. Never publish from
  a machine (`npm publish`, a manual PyPI upload), never set `NPM_PUBLISH`, and
  never dispatch a publish workflow with `dry_run=false`.
- **Renaming.** Code identifiers, CLI flags, config keys, file names and CI job
  names do not change for wording reasons. ACP v1, harness API v1 and stored
  sessions are external contracts.
- **Rewriting history.** No force-push, and no deleting a branch or worktree
  that another session owns.
- **Lifting a limit.** A ratchet's `--update` and a performance budget change
  are for growth the change intends; name it in the pull request, and never use
  it to make a failing gate pass.

## Common pitfalls

- **Wrong checkout.** A relative path used from the main checkout edits the
  main checkout, not the worktree. Check the path before every edit.
- **Two documentation sets.** `docs/` is compiled into the binary and is the
  source of truth for the site's Reference tab: edit `docs/`, run
  `python3 scripts/ci/sync_docs_site.py` and commit the result, never edit
  `packages/docs-site/reference/`. The site's other pages are hand-written, so a
  claim fixed in `docs/` stays wrong in a guide until the guide is edited too.
- **Renamed heading.** It breaks `#anchor` links in both languages;
  `check_docs_pairs.py` reports them.
- **Stale `Cargo.lock`.** It passes every job that does not pass `--locked` and
  fails the ones that do. `cargo metadata --locked --format-version 1` catches
  it locally.
- **Version strings.** `horon-agent --version` and `horon-tui --version` print
  different values for the same release; compare release tags, not strings.
- **`headless` in a name.** `check_headless_graph.py`, `is_headless` and
  `spawn_mode = "headless"` are identifiers, not the product description.
- **GitHub API budget.** `gh run watch` polls every 3 seconds by default and
  every session shares one hourly limit. Use `--interval 60`.
- **Documentation-only `dev` run.** It is green without building anything and
  is not release evidence (see "CI gates by change type").
