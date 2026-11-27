# AGENTS.md: horon `agent/`

Language: **English** | [한국어](AGENTS.ko.md)

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
|`@horon-ai/horon` and six platform packages|`packages/npm`, `scripts/npm`|both binaries through one launcher|`publish-npm.yml`; publishes on each `main` release once `NPM_PUBLISH` is `true` (and again on `release: published`, which heals a run that arrived while the release was a draft), and fails the run with the recovery command instead of dry-running silently when the switch is off|
|SDKs|`crates/agent-sdk`, `packages/sdk/typescript`, `packages/sdk/python`|headless mode from code|TypeScript: `publish-typescript-sdk.yml`, gated by its own `SDK_NPM_PUBLISH`; Python: `publish-python-sdk.yml`, manual dispatch into the `pypi` environment; neither published yet|
|Container image|`scripts/release/Containerfile`|the daemon in headless mode|`ghcr.io/horon-ai/agent`, pushed by `release-tag.yml`|
|Hosted per-tenant image|`scripts/hosting/Containerfile`|one customer per container with a `/data` home volume|built from the same release archives; not published, started by the control plane (`packages/docs-site/reference/architecture/hosting.mdx`)|

- **One release.** Both binaries are built by the same `release.yml` run from
  the same tagged commit (`vMAJOR.MINOR.PATCH`, or a `-PRERELEASE` tag such as
  `v0.2.0-alpha.1` for a GitHub prerelease), and `install-release` stages
  them into one `builds/versions/<version>/` directory that activates as a
  unit. There is no separate TUI release or channel. The version strings
  differ: `horon-agent --version` comes from the tag (`crates/build-meta`),
  `horon-tui --version` from the `crates/tui` package version. The npm version is the tag
  without its `v` (`packages/docs-site/reference/release/npm.mdx`).
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
   `packages/docs-site/reference/protocol/acp.mdx`.
3. **No measured performance regression.** On the same machine, build profile,
   and workload, a change must match or beat the recorded baseline. A change
   that makes a measured path slower than the baseline does not merge. How to
   measure: `packages/docs-site/reference/performance/benchmark.mdx`; recorded numbers and their limits:
   `packages/docs-site/reference/performance/results.mdx`, which is the recorded baseline. Scope: the
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
  (`packages/docs-site/reference/performance/benchmark.mdx`), not by guessing.
- **Never** buy speed with durability or correctness: no dropped flush of an
  original (event log, blob, local state, runtime journal), no skipped
  validation, no weaker default, no benchmark-detecting branch, no change to
  the fixture or mock to favour horon. A flush may go only when a test proves
  a crash still loses nothing (precedent: the catalog dirty marker,
  `packages/docs-site/reference/protocol/session-store.mdx`). A cell jcode keeps for such a reason is
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

The user-installed Rust package implementation exercises one exception to item
3's graph limit: the internal data-contract crate `extension-types`
(`crates/extension-types`, package `horon-agent-extension-types`) draws only
from the existing workspace dependency set and adds no new external runtime
dependencies, while `horon-agent-extension-sdk` (`crates/extension-sdk`) and
`horon-package-example` (`tests/fixtures/package-example`) stay outside the
`horon-agent` binary graph. Keep the no-package performance and binary size
baselines, enforce the zero-overhead `packages` bounds in the structural gate
alongside a binary-size check when extending the surface, and keep ACP v1/v2,
the SDKs, the TUI dependency boundary, and stored sessions compatible.

Does not do: forced tool-call steps, streaming dry-run, a `report_issue` tool,
per-tool `family@rev` versions, a KDL model catalog, wholesale `anyhow` or
`SmallVec` replacement, or a cutover of external contracts (ACP v1, harness API
v1, stored sessions). Reasons and sources: `PHILOSOPHY.md`.

## What this build is

- Two binaries. The agent binary, `horon-agent`, has these working
  subcommands: `serve` (agent daemon), `run` (send one message and exit),
  `repl` (plain line prompt, no full-screen UI), `api-bridge` (alias `api`;
  harness API on a local socket, a Unix socket or a Windows named pipe, plus an
  optional authenticated TCP listener with `--api-bind`), `api-credentials`
  (issue, rotate, revoke and list the bearer tokens that listener accepts),
  `acp` (ACP adapter on stdio), and
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
  and `x-goog-api-key` for Gemini; an explicit `auth` always wins. Model calls
  use one provider runtime and reach the endpoint directly. Keep routers and
  provider plugins out of that path. External tools come in over MCP and
  user-installed Rust packages (`packages/docs-site/reference/runtime/packages.mdx`). When the active
  profile names `roles.image`, the optional `generate_image` tool POSTs
  `/images/generations` and `/images/edits` on that same profile, with the same
  base URL, credential and `extra_body`. It is a route beside the wires, not a
  runtime; `[tools] generate_image` is `true` by default and the tool is registered only when the profile has an `image` role. The `image`
  role is therefore the one role whose model does not speak Chat Completions,
  it is served only by an `openai-compatible` or `openai-responses` profile,
  and it is the only role with no text fallback.
- User-installed Rust packages are implemented (`packages/docs-site/reference/runtime/packages.mdx`). An
  extension declares a Rust executable (`extension.toml`); a plugin bundles one
  `extension.toml` and declared resources (`plugin.toml`). Both use
  `horon package install|list|verify|disable|remove` or
  `$HORON_HOME/packages/inbox/<id>/` (reconciled by a running daemon).
  Installation is consent: `PackageStore` validates declarations and native
  targets, records the SHA-256 of `extension.toml`, applies a new catalog
  generation at the next turn boundary of an open session, and verifies that
  SHA-256 before spawning each child. Executable calls, package MCP stdio
  servers, and observers run in separate child processes under the user's OS
  account through the existing tool and hosting policy gates
  (`crates/base/src/packages/child_gate.rs`). Keep in-process plugin loading
  prohibited and preserve the fixed requirements.
- Hosting is opt-in through `HORON_AGENT_HOSTING_MODE` (`per-user-container`
  or `shared-worker`; absent means the operator's own home, and any other value
  is a startup error naming both). A hosted process carries
  `HORON_AGENT_TENANT_ID` as a validated audit label and directory name, never
  a session lookup key. A per-user container refuses to start unless
  `HORON_HOME` is an owner-only directory that is not `~/.horon`, the runtime
  directory is absolute, owner-only and outside the home, the TCP gateway is
  off and the tenant id is valid. A shared worker starts, but
  `HostingPolicy::allows_shared_children()` is false. A per-user container
  keeps its own MCP stdio servers and package children; a shared worker refuses
  every MCP stdio server, editor-supplied server, pool connection and remote
  connection through one gate (`crates/base/src/mcp/hosted.rs`), with one
  `HOSTED_POLICY` audit record per refusal. `[hosting]` in `config.toml` may
  only narrow the policy (`allow_shared_children`, `allowed_mcp_servers`), and a
  `mode` key there is ignored with one warning, because a tenant can write its
  own config (`packages/docs-site/reference/architecture/hosting.mdx`, `packages/docs-site/reference/safety/security.mdx`).
- Core tool profile, the default when a launch selects none: every built-in tool
  this build registers is reachable. The allow-list is `CORE_TOOLS` plus
  `CORE_DISCOVERABLE_TOOLS` plus `toolbox`, and `irc`, `checkpoint`, and `rewind`
  join through their own switches. A `core` session sends eleven of them eagerly
  (`read`, `write`, `edit`, `apply_patch`, `bash`, `ls`, `agentgrep`, `todo`,
  `bg`, `websearch`, `webfetch`) plus `toolbox`, and `eval` when a Python
  interpreter is on PATH. It names the rest in the system prompt, and the model
  reaches them through `toolbox` (`packages/docs-site/reference/runtime/tools.mdx`). `skill_manage` and
  `mcp` stay registered. An SDK-driven session keeps `session_search` and
  `swarm` off unless its `enabled` list names them. `horon-agent acp` is the
  exception to the default: with no explicit profile it applies
  `[acp] tool_profile`, which is `acp`.

## Repository layout and crate boundaries

|Path|Holds|
|---|---|
|`src/`|the `horon-agent` package: agent loop, daemon, harness API bridge, ACP and RPC adapters, CLI dispatch|
|`crates/`|the workspace crates; `crates/tui` is the `horon-tui` package, `crates/agent-sdk` the Rust SDK|
|`tests/`, `crates/*/tests/`|integration targets|
|`docs/`|the agent-facing documentation, English only, flat topic pages; compiled into the agent binary and served at `horon://<page>` and by the `horon` tool|
|`packages/sdk/typescript`, `packages/sdk/python`|the TypeScript and Python SDKs|
|`packages/npm/horon`|the `@horon-ai/horon` launcher|
|`packages/docs-site`|the agent.horon.ai site, a Mintlify project, and the source of the developer reference: `reference/` (English), `ko/reference/` (Korean, same paths) and `images/reference/` are edited in place. Only `reference/philosophy`, `ko/reference/philosophy` and `reference/sdk/*` are generated, by `scripts/ci/sync_docs_site.py` from `PHILOSOPHY*.md` and the SDK and npm READMEs. Mintlify deploys only the `deploy/docs` branch, which `docs-site-deploy.yml` alone fast-forwards to `main` when the folder changed there (`packages/docs-site/README.md`)|
|`scripts/ci`, `scripts/perf`, `scripts/release`, `scripts/install`, `scripts/npm`|gates, benchmarks, release, installer and packaging scripts|

Boundaries, each with the gate that enforces it:

- `horon-agent` reaches no `ratatui`, `crossterm`, `horon-tui` or
  `horon-agent-tui*` package, dev-dependencies included
  (`scripts/ci/check_headless_graph.py`).
- `horon-tui` is a client: its only workspace dependencies are
  `horon-agent-sdk` and the contract crate `horon-agent-tool-types`, never an
  engine crate, and it grows no engine of its own
  (`crates/tui/Cargo.toml`, `packages/docs-site/reference/architecture/crates.mdx`; reviewed, no gate
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
  `packages/docs-site/reference/architecture/crates.mdx`, not case by case.

## Configuration that must keep its default

|Setting|Default|Other values|
|---|---|---|
|Tool profile (`--tool-profile`, `HORON_AGENT_TOOL_PROFILE`, `[tools] profile`)|`core`|`full`, `acp`, `minimal`/`lite`, `none`|
|`[acp] profile`|`standard`|`extended`, `full`|
|`[tools] redo`, `hashline_edit`|`true`|`false` removes the tool|
|`[tools] ast_tools`, `lsp`, `context_notes`, `managed_skills`, `codemode`|`true`|`false` removes the tool|
|`[tools] ask_policy`|`recommended`|`error`|
|`[tools] bash_pty`|`auto`|`off`, `always`|
|`[tools] irc`, `checkpoint`|`true`|`false` removes the tool; `HORON_AGENT_IRC` overrides `irc` for one process|
|`[irc] mailbox_cap`, `max_body_bytes`, `wait_max_secs`, `broadcast_concurrency`|`100`, `65536`, `300`, `16`|any value ≥ 1|
|`[tools] eval`|`true`|`false` removes the tool; a cell needs Python 3.10 or later on PATH and asks for approval exactly like `bash`|
|`[features] crash_bundles`|`true`|`false`|
|`[features] turn_recovery`|`conservative`|`aggressive`, `off`|
|`[features] foreign_rules`, `stream_rules`, `repeat_guard`|`false`|`true`|
|`[features] effort_keywords`|`true`|`false`|
|`[prewalk] enabled`|`false`|`true`, which then requires `into` (a model id or `role:<name>`) that passes config validation|
|`[storage] sync`|`full`|`fast`; `HORON_AGENT_STORAGE_SYNC` overrides it for one process, and an unknown value falls back to `full` with one warning|
|`[features] pricing_refresh`|`false`|`true`; `HORON_AGENT_PRICING_REFRESH=1` or `0` overrides it for one process|
|`[server] idle_hibernation_secs`|`1800`|`0` disables the sweep; `HORON_AGENT_IDLE_HIBERNATION_SECS` overrides it for one process|
|`HORON_AGENT_HOSTING_MODE`|unset (no hosting)|`per-user-container`, `shared-worker`; any other value is a startup error|
|`HORON_AGENT_TENANT_ID`|unset|1 to 63 lowercase ASCII letters, digits and hyphens, starting and ending with a letter or digit; required by every hosted mode and used only as an audit label|
|`[hosting] allow_shared_children`|per mode (true only in a per-user container)|`false` narrows; `true` never widens; `[hosting] mode` is ignored with one warning|
|`[hosting] allowed_mcp_servers`|unset (every server reachable)|a list of exact MCP server names for a hosted process; an empty list refuses every server; narrowing only, never widens a mode, and a process without hosting is unchanged|
|`HORON_AGENT_API_BIND`, `[harness_api] bind`, `--api-bind`|unset (local socket only)|`IP:PORT` for the authenticated TCP harness API listener; the flag wins over the environment and both win over the file|
|`HORON_AGENT_API_INSECURE_BEHIND_PROXY`, `[harness_api] insecure_behind_proxy`, `--api-insecure-behind-proxy`|`false`|`true` only when a TLS-terminating proxy fronts a non-loopback bind, which also needs an issued credential (`horon-agent api-credentials issue`)|
|Update check on `serve`|not run|`--no-update` skips it everywhere|

`[storage] sync` names a durability level, not a syscall: `full` is
power-loss durable (macOS `F_FULLFSYNC`) and `fast` is the cheapest
crash-durable flush (plain `fsync` on macOS, the same as `full` elsewhere).
Originals are confirmed with it and derived caches are rebuilt instead, which
`packages/docs-site/reference/architecture/crates.mdx` states as the boundary. `pricing_refresh` off
keeps a one-shot run off the network; a catalog already on disk is still read.

`[server] idle_hibernation_secs` is the idle window before the daemon releases
a session's derived caches and any opt-in Python kernel. The provider
projection and tool-output index rebuild from resident state without a disk
read. The next Python cell starts with fresh state. `packages/docs-site/reference/performance/memory.mdx` lists
what is released and what stays.
`0` disables the sweep, and the daemon checks every ten seconds, so
the release lands up to one tick after the threshold passes.

Memory is stored in the per-user markdown wiki under `$HORON_HOME/memory` — one
markdown page per memory plus a SQLite index rebuilt from those pages — and the
pre-wiki `memory/*.json` files are a migration source only
(`packages/docs-site/reference/runtime/memory.mdx`). Project memory is keyed by the canonical project
directory, not by the caller's path spelling, and every `memory` action and the
prompt-injection path read and write that store.

Every built-in tool is reachable by default; a switch set to `false`,
`[tools] disabled`, or `[tools.load_mode] <name> = "off"` removes one.
`irc` is agent-to-agent messaging inside one daemon; `[tools] irc = true`, the
default, adds it to the `core` list (other lists reach it by naming it), and with the switch off it
refuses to run even under `full` (`packages/docs-site/reference/runtime/irc.mdx`). Under `full`, the
optional tools load as deferred definitions behind `toolbox`
(`packages/docs-site/reference/runtime/tools.mdx`). `[tools.load_mode]` overrides one tool's tier
(`essential`, `discoverable`, `off`). `ask = "essential"` sends a listed
tool eagerly again, and `toolbox = "off"` turns listing off so every reachable
tool is eager. Sessions under `acp`, `minimal`, and `none`, one with an
explicit `enabled` list, and a `core` session with an SDK `enabled` list send
every tool they allow eagerly and get no index. Under `full`, an SDK list keeps
deferring the non-core tools.

Two settings are environment-only. `HORON_AGENT_JOBS_ARTIFACT_RETENTION` is
`rotate` by default and reclaims the oldest background task artifacts when the
budget is full; `preserve` refuses to start a new task instead.
`HORON_AGENT_WORKSPACE_TRUST` accepts `trust` or `deny` for one process;
project-local MCP config is skipped unless its digest was trusted, and
`--trust-workspace` records the decision (`packages/docs-site/reference/safety/security.mdx`).

## Build, run, verify

### CI-first local verification (takes precedence for agent sessions)

GitHub Actions owns full verification for this repository. Where the reference
commands or table below conflict with this subsection, these rules win:

1. **GitHub CI runs full checks.** Never run workspace-wide `cargo clippy`,
   `cargo test`, `cargo doc`, cross-OS checks (`cargo xwin`, `justrust`), or full
   `scripts/ci/check_all.sh` locally; check PR CI (`scripts/ci/run_warnings.py`
   at 0 warnings).
2. **Scoped local checks.** Run only single-crate checks (`cargo check --locked
   -p <crate>`, one targeted test) or non-Cargo gates (`actionlint`,
   `check_docs_links.py`), plus minimal (1 to 2) scoped before/after
   measurements when an optimization needs local evidence.
3. **`build-lock` discipline.** Serialize heavy local Cargo work with
   `~/.local/bin/build-lock`; allow one waiter per session (no polling loops or
   duplicate `hc wait` waiters; clear stale duplicates). Skip waiting when load
   per core < 2, and defer to CI under high contention.
4. **PR-first loop.** Open or update the PR first; if CI fails, reproduce only
   the single failing test locally. Merge only when green with 0 warnings.
5. **Hard test bounds.** Run local tests with a hard timeout (e.g., 90s) and
   `-- --test-threads=1` so hung tests never hold `build-lock`.
6. **Adopted 2026-11-24** per owner directive after shared MacBook `build-lock`
   queues stalled concurrent sessions.

Reference commands and pre-push gate table (run by CI or selectively on a single
touched component per the rules above):

```bash
cargo build -p horon-agent
cargo build -p horon-tui
./target/debug/horon-agent --version
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --workspace
```

CI gates and optional manual commands by changed area:

|Change|Command|
|---|---|
|Anything|`scripts/ci/check_all.sh`: the Format and Quality Guardrails steps of `ci.yml`. `--skip-slow` leaves out `cargo check`, clippy and `cargo machete`; `--fix` formats and rebaselines the ratchets|
|Rust code|the `cargo` lines above, with the tests of every crate touched|
|`crates/tui`|`cargo clippy --locked --all-targets --all-features -p horon-tui -- -D warnings`|
|`docs/`, `packages/docs-site/`, README, `PHILOSOPHY*.md`|`python3 scripts/ci/check_docs_links.py`, `python3 scripts/ci/check_docs_pairs.py`, `python3 scripts/ci/sync_docs_site.py --check`, `python3 -m unittest scripts/ci/test_docs_install_claims.py`, and `cargo test -p horon-agent-app-core --lib horon_agent_docs` when `docs/` changed|
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
`packages/docs-site/reference/performance/benchmark.mdx`.

## Tests

- **A test guards behavior.** Keep a test only when it depends on intended
  behavior: it changes only when that behavior changes, and fails only when it
  regresses. Do not write, and delete when found, a test that depends on the
  past (it asserts that removed code stays gone), on nothing (no code change
  can make it fail: it tests the standard library, the type system, or a mock
  of the unit under test), or on the implementation (its expected value
  repeats the code's own logic, or it asserts private state, internal call
  order, or which helper ran), or that duplicates another test reaching the
  same code path and branch. When such a test also covers behavior callers
  rely on, rewrite it against that behavior with literal expected values and
  keep its name and module, because workflows and gates select tests by name.
  Delete production code that exists only for tests (`#[cfg(test)]` helpers,
  `*_for_tests` functions) when its tests go. Wire and storage contracts,
  refusals a user sees, durability, concurrency and security tests, gate
  contract tests, and source-scanning rule tests are behavior. Removing tests
  lowers each `--min-passed` and `min_passed` floor that selected them by
  the number of them that step counted as passed (an `#[ignore]` test, or one
  whose `cfg` excludes the step's runner, never counted), never below 1 (a
  removed test named in a `skipped` list leaves that list instead), and the
  pull request names the change (#419).
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
  the narrowest scope, with the reason beside it and named in the report. The
  `linker_messages` allowances in `crates/app-core/src/lib.rs`, `src/lib.rs`,
  and `src/main.rs` cover unoptimized macOS links. The dependency audit uses no advisory exceptions.
- **Gates that keep it at zero.** `ci.yml` sets `CARGO_BUILD_WARNINGS=deny`, so a
  rustc or build-script warning in a workspace crate fails the build, and
  `scripts/ci/check_security.sh` runs `cargo audit --deny warnings`. A new
  advisory in the RustSec database can therefore turn CI red without a code
  change; triage it the same way. `notice`-level annotations (the macOS runner
  capacity notice) are GitHub's and are not warnings.

## CI gates by change type

`ci.yml` and `structural-perf.yml` start with a `Changed Paths` job
(`scripts/ci/classify_changes.py`, emitting 16 boolean lanes) that classifies a
change by its files: a pull request against its base, and a first-attempt push
to `dev` against the head of the last successful `dev` push run of the same
workflow (two-dot diff).

- **Documentation only** (`README`, `PHILOSOPHY*.md`, `docs/`, `AGENTS.md`,
  templates, `packages/docs-site/`; `docs_only=true`): runs `Documentation
  Checks` (links, UTF-8, English/Korean pairs, install claim table
  consistency), `Documentation Corpus Test` (`docs/` is compiled into the
  binary, so the `horon_agent_docs` tests still run) and, when site pages
  changed, `Documentation Site` (`sync_docs_site.py --check`, Mintlify build
  and broken-links check). Release builds, the OS test matrix, SDK jobs,
  `Quality Guardrails` and the structural performance gate are skipped. A
  documentation change and the site pages it regenerates stay in this lane,
  on the pull request and on its `dev` push.
- **Scoped non-Rust lanes:**
  - `.github/workflows/*` (`workflows=true`, `rust=false`) runs
    `Workflow Lint` (`actionlint`), `Release Automation` and
    `PowerShell Syntax`, skipping Rust builds.
  - Standalone TypeScript SDK, Python SDK, npm launcher or install-script
    edits (`ts_sdk`, `py_sdk`, `npm_pkg`, `install_scripts` with `rust=false`)
    run their matching lane (`TypeScript SDK`, `Python SDK`, `Setup Friction Eval`,
    `PowerShell Syntax`, `Release Automation`) plus non-Rust `Quality Guardrails`
    checks, skipping `Build & Test`, Windows cross-checks and `structural-perf`.
- **Rust changes (`rust=true`):** runs `Quality Guardrails`, `Build & Test`
  (pull requests use `scripts/ci/workspace_impact.py` to compile and test only
  impacted crates and integration targets, falling back to the full workspace
  on manifests, lockfiles, shared infra or `ci:all-os`/`ci:full`),
  `windows-cross-check` (`cargo-xwin`), `windows-arm64-check` (`justrust`),
  `Release Automation` and `Structural performance gate` (unified `--no-run`
  compilation with `mold` on Linux).
- **Pull request OS and soak scoping:** portable Rust pull requests run
  `ubuntu-latest` plus the two Linux-hosted Windows cross-checks with
  `HORON_AGENT_SOAK_CYCLES=20`. `macos-latest`, native `windows-build-test` and
  `tui_smoke` run on pull requests only when matching platform/TUI paths, shared
  Linux-macOS `#[path]` sources, manifests, lockfiles or the
  `ci:all-os`/`ci:full` label trigger them. Any ambiguous diff or lookup failure
  resolves conservatively to `rust=true` and `full_matrix=true`.
- **Pushes to `dev`, `main`, manual dispatches and re-runs:** non-docs pushes to
  `dev` run all three OSes, the full crate/target suite and
  `HORON_AGENT_SOAK_CYCLES=200`. A `dev` push whose head is already in
  `origin/main`, a push to `main`, a manual dispatch, a `dev` re-run and any
  lookup or diff failure run everything. Release evidence
  (`scripts/release/check_ci.py`) accepts only a fully successful push run; a
  documentation-only `dev` run is not release evidence, and `Re-run all jobs` on
  that run produces a full run.
- **Release pull request already verified on `dev`:** a pull request into
  `main` whose head is the head of the last successful `dev` push run of the
  same workflow, and whose merge commit (`GITHUB_SHA`, the checked-out
  `refs/pull/N/merge`) differs from that head only in documentation, gets the
  documentation-only lanes (`docs_only=true`): that push run already ran every
  gate on the same code. A change `main` already received by cherry-pick does
  not count, because the merge commit is compared, not `main`'s history. A
  `ci:all-os`/`ci:full` label, another head, code the merge adds, a missing
  merge commit or any lookup or diff failure classifies it as an ordinary pull
  request. `package-compat.yml` and `package-windows.yml` do
  not run on pull requests into `main` at all; their `dev` and `main` push runs
  cover the same files.
- **Rust caches are saved only on `main`.** `main` is the default branch, so
  every branch and pull request restores its entries, while `main` cannot
  restore `dev` entries and two copies overflow the 10 GB cache quota. CI,
  structural-perf, package and soak caches save on a first-attempt `main` run;
  benchmark, loading A/B and Windows smoke caches save only when the run's ref
  is `main`. `tests/test_ci_workflow_guards.py` pins each `save-if`.
- `CI Required` and `Structural performance required` always finish and fail
  only when a gate failed or was cancelled; use them as the required checks so
  a skipped gate never leaves a check pending.
- Every action is pinned to a commit with a `# vX` comment (Dependabot keeps them
  current); `release.yml`, `release-tag.yml` and the publish workflows are not
  part of this gating. `actionlint` must report nothing (`Workflow Lint`).
- **Superseded runs are cancelled outside `main` and `dev`.** A newer run on
  the same ref cancels the older one on every other ref. On `main` a push run
  is release evidence, so a running one is never cancelled. On `dev` a newer
  run waits for the running one instead: a cancelled `dev` run leaves no green
  head, so the next push would be compared with an older one and run every gate
  again. GitHub replaces a run that is only pending with a newer one, so at most
  one waits, and it is classified against the run it waited for. A push and a
  manual dispatch on `dev` share the group. After a cancelled or failed `dev`
  push run, use `Re-run all jobs` on that run. Never fast-forward `dev` to a
  `main` commit (the sync is the merge-commit PR): a cancelled `dev` run would
  then be the newest push run for a release commit, and
  `scripts/release/check_ci.py` rejects it.

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
`packages/docs-site/reference/performance/results.mdx` as they are on `main`. The pull request has base
`main`, carries documentation only, and is squash-merged with `[skip ci]` in
the commit subject, so the push to `main` starts no CI run and
`release-tag.yml` creates no tag. It starts no documentation site deploy
either: run `gh workflow run docs-site-deploy.yml --ref main` when the refresh
touched `packages/docs-site/`. The next release and sync back reconcile the
two copies.

## Docs

There are two documentation sets with different readers.

|Set|Reader|Location|Language|Shape|
|---|---|---|---|---|
|Agent documentation|an agent running inside Horon, and its user|`docs/<topic>.md`, flat; `docs/README.md` is the index|English only (it is the bundled language)|case-oriented: which tool or command for which need, what happens in each case, defaults and fixes|
|Developer reference|maintainers|`packages/docs-site/reference/<area>/<page>.mdx` and `packages/docs-site/ko/reference/<area>/<page>.mdx`|English and Korean pairs|design, contracts, `path:line` pointers, CI, benchmarks|

Every Markdown file under `docs/` is compiled into the binary
(`crates/app-core/build.rs`) and served by the `horon` tool and
`read horon://<page>`, with `docs/` as the root (`docs/tools.md` is
`horon://tools.md`). It describes the running version without network access.
Keep pages short and factual; link to the developer reference for depth.

A developer reference page and its Korean pair change in the same commit and
keep the same headings, tables, code fences and links;
`python3 scripts/ci/check_docs_pairs.py` checks them together with the
`README` and `PHILOSOPHY` pairs, and `python3 scripts/ci/check_docs_links.py`
checks root and `docs/` links. Extend an existing page before adding one, and
add a new page to `packages/docs-site/docs.json`.

`packages/docs-site/reference/diagram/index.mdx` shows the hand-written SVG
diagrams under `packages/docs-site/images/reference/diagram/` (`NN-name.svg`
with English labels, `NN-name.ko.svg` with Korean labels). They follow the
style of `packages/docs-site/images/reference/assets/architecture.svg`:
self-contained, no script or external reference, light and dark colours
through `prefers-color-scheme`. A moved or deleted page must leave no stale
path in code, scripts, or tests.

**Docs follow the code.** A change that alters behaviour a document describes
updates that document, its Korean pair, and the diagram that draws it in the
same pull request, not in a follow-up. A change is not done while a document
still states the old behaviour. To find the pages:

1. Search for every changed file path, symbol, CLI flag, config key and
   default: `rg -n '<path-or-name>' README.md README.ko.md AGENTS.md docs/ packages/docs-site/`.
   A page that names it describes it.
2. Check the area table below for pages that describe the behaviour without
   naming the file.
3. Re-read each hit against the new code: counts (tools, fields, profiles),
   protocol minor versions, defaults, `path:line` pointers, and command
   output. Update it to the current tense; history goes in the pull request
   body, not in the document.

|Changed area|Documents to check|
|---|---|
|`src/cli/` commands, flags and dispatch, `src/main.rs`|`README.md`, `packages/docs-site/reference/release/install.mdx`, `packages/docs-site/images/reference/diagram/02-run-modes.svg`|
|`src/cli/tui_handoff.rs`, `crates/tui/`|`packages/docs-site/reference/runtime/tui.mdx`, `packages/docs-site/images/reference/diagram/02-run-modes.svg`|
|`src/cli/acp/`, `src/cli/acp.rs`|`packages/docs-site/reference/protocol/acp.mdx`, `packages/docs-site/images/reference/diagram/05-acp-negotiation.svg`|
|`src/cli/rpc/`|`packages/docs-site/reference/protocol/rpc.mdx`|
|`crates/harness-api*`, `crates/agent-sdk`, `packages/sdk/`, `src/cli/api_credentials.rs`, `crates/base/src/harness_api.rs`, `crates/config-types/src/harness_api.rs`|`packages/docs-site/reference/protocol/harness-api.mdx`|
|`crates/app-core/src/server/`|`packages/docs-site/reference/architecture/server.mdx`, `packages/docs-site/reference/runtime/turns.mdx`, `packages/docs-site/images/reference/diagram/03-turn-lifecycle.svg`|
|`crates/base/src/hosting.rs`, `crates/config-types/src/hosting.rs`, `crates/app-core/src/server/hosted_tenant.rs`, `crates/agent-sdk/src/launch_hosted.rs`, `scripts/hosting/`|`packages/docs-site/reference/architecture/hosting.mdx`|
|`crates/session-store/`, `crates/storage/`, `[storage] sync`|`packages/docs-site/reference/protocol/session-store.mdx`, `packages/docs-site/reference/architecture/crates.mdx`, `packages/docs-site/images/reference/diagram/06-session-storage.svg`|
|`crates/app-core/src/tool/`, `crates/tool-*`, `crates/edit-safety/`, tool profiles|`packages/docs-site/reference/runtime/tools.mdx`, `packages/docs-site/reference/runtime/tool-scheduling.mdx`, `packages/docs-site/images/reference/diagram/07-tool-loading.svg`, "What this build is" above|
|`crates/extension-*`, `crates/base/src/packages/`, `crates/app-core/src/extensions/`, `src/cli/packages.rs`|`packages/docs-site/reference/runtime/packages.mdx`, "What this build is" above|
|`crates/base/src/memory*`, `crates/memory-types/`, `horon memory migrate`, `horon volume`, `src/cli/volume.rs`|`packages/docs-site/reference/runtime/memory.mdx`, `packages/docs-site/reference/runtime/tools.mdx`, `packages/docs-site/reference/architecture/crates.mdx`, `packages/docs-site/reference/architecture/hosting.mdx`|
|`crates/provider-*`, `crates/base/src/provider*`, `crates/config-types/`|`packages/docs-site/reference/provider/model.mdx`, `packages/docs-site/images/reference/diagram/04-provider-wires.svg`, "What this build is" above|
|`crates/irc-core/`, `crates/app-core/src/server/irc/`|`packages/docs-site/reference/runtime/irc.mdx`|
|`crates/plan/`, `crates/swarm-core/`|`packages/docs-site/reference/runtime/swarm-task-graph.mdx`|
|approval, workspace trust, crash bundles, `scripts/ci/check_security.sh`|`packages/docs-site/reference/safety/safety.mdx`, `packages/docs-site/reference/safety/security.mdx`|
|`crates/platform*`, a crate added, removed or renamed in `Cargo.toml`|`packages/docs-site/reference/architecture/crates.mdx`, "Repository layout" above, `packages/docs-site/images/reference/diagram/01-system-overview.svg`|
|`.github/workflows/`, `scripts/ci/classify_changes.py`, `scripts/release/`|"CI gates by change type" and "Release and branch sync" above, `packages/docs-site/reference/release/`, `packages/docs-site/images/reference/diagram/08-branch-release-flow.svg`|
|`scripts/perf/`, `benchmark*.yml`, `scripts/perf/structural_budget.json`|`packages/docs-site/reference/performance/benchmark.mdx`, `packages/docs-site/reference/performance/memory.mdx`|
|`packages/npm/`, `publish-*.yml`|`packages/docs-site/reference/release/npm.mdx`, `packages/docs-site/reference/release/install.mdx`|
|a value in "Configuration that must keep its default"|that table and the page that documents the setting|

Each area also has an agent-facing page in `docs/` (the index in
`docs/README.md` names it); a user-visible change updates that page too. The
same rule covers `README.md`, this file, and the hand-written guides in
`packages/docs-site`. A code pull request that needs no document change says
so in its body with the reason.

**Wording.** Follow the terminology in "Modes, binaries and release units":
"headless" and "TUI" name the two modes, and the binaries are called the agent
binary and the TUI binary. The `{/* bench-charts */}` and `{/* bench-auto */}`
blocks of `packages/docs-site/reference/performance/results.mdx` (and its Korean
pair) are written by `scripts/perf/bench_docs.py` in `benchmark-docs.yml`;
change the script, not the block.

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
- **Two documentation sets.** `docs/` is the agent documentation and the only
  set compiled into the binary; `packages/docs-site/reference/` is the
  developer reference, edited in place. Only the philosophy and SDK/npm pages
  are generated: edit `PHILOSOPHY*.md` or the package README, run
  `python3 scripts/ci/sync_docs_site.py` and commit the result. A behaviour
  change usually touches both sets and any hand-written guide that repeats it.
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
