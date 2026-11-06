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
   both sides support. Current state: only v1 is implemented
   (`src/cli/acp.rs`, `ACP_PROTOCOL_VERSION = 1`, the response pins to 1);
   v2 is an open gap, not a waiver. Never drop or break v1 while adding v2.
   The stdio contract, editor setup, and the v2 differences are in
   `docs/protocol/ACP.md`.
3. **No measured performance regression.** On the same machine, build profile,
   and workload, a change must match or beat the recorded baseline. A change
   that makes a measured path slower than the baseline does not merge. How to
   measure: `docs/performance/BENCHMARK.md`; recorded numbers and their limits:
   `docs/performance/RESULTS.md`, which is the recorded baseline.

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
|`[features] mermaid`|`false`|`true`; `HORON_AGENT_ENABLE_MERMAID` for one process|
|Update check on `serve`|not run|`--no-update` skips it everywhere|

Tools outside the core set (swarm, memory, panels, calendar, gmail,
session search, todo, irc) stay in the tree but off.
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

## Docs

`docs/` is grouped by topic into `architecture/`, `protocol/`, `runtime/`,
`provider/`, `safety/`, `performance/`, and `release/`. File names are
uppercase (`docs/runtime/TOOLS.md`). Extend an existing doc before adding a
new file. Docs are written in Korean.

Every Markdown file under `docs/`, including subfolders, is compiled into the
binary and served by the `horon_agent_docs` tool (`crates/app-core/build.rs`).
A moved or deleted doc must leave no stale path in code, scripts, or tests.
`docs/README.md` is the index; update it in the same change.
