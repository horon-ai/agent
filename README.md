# horon agent

The agent backend for horon. One binary, `horon-agent`, runs an agent over a Unix
socket, the harness API, or ACP on stdio. It has no terminal UI and speaks one
model protocol, OpenAI-compatible.

## Fixed requirements

These hold for every change; `AGENTS.md` carries the full contract.

- **No TUI.** The binary never opens a terminal; `ratatui`, `crossterm`, and
  `horon-agent-tui*` stay out of the dependency graph.
- **ACP v1 and v2.** `horon-agent acp` must serve both protocol versions.
  Today only v1 is implemented; v2 is an open gap. The adapter and the stdio
  contract are in [docs/protocol/ACP.md](docs/protocol/ACP.md).
- **No measured performance regression** against the recorded baseline on the
  same machine and workload. Measurement method:
  [`docs/performance/BENCHMARK.md`](docs/performance/BENCHMARK.md); recorded
  numbers: [`docs/performance/RESULTS.md`](docs/performance/RESULTS.md).

## Build

Needs a recent stable Rust toolchain (edition 2024).

```bash
cargo build -p horon-agent
./target/debug/horon-agent --version
```

## Install a release

Published installers fetch a release's assets into a directory and hand them to
the binary instead of unpacking the archive themselves:

```bash
horon-agent install-release --assets <dir> --agent-version <version>
```

The binary verifies `SHA256SUMS`, extracts the platform archive, stages an
immutable version directory, and activates the stable/current channel links. A
version directory that already holds different bytes is refused rather than
overwritten. `--source-binary <path>` installs a locally built executable
through the same path. Details:
[`docs/release/INSTALL.md`](docs/release/INSTALL.md).

## Run

```bash
./target/debug/horon-agent serve                      # agent daemon
./target/debug/horon-agent run "summarize this repo"  # one message, then exit
./target/debug/horon-agent api-bridge --api-socket /run/user/1000/horon-agent-api.sock
./target/debug/horon-agent acp                        # ACP v1 over stdio for editors, see docs/protocol/ACP.md
```

`HORON_AGENT_SOCKET` moves the daemon socket and `HORON_AGENT_API_SOCKET` the
harness API socket.

## Model endpoint: an OpenAI-compatible API, called directly

Every model request goes to an OpenAI-compatible API's `/chat/completions`
endpoint straight from the agent process. There is no router process or plugin
layer in between, and nothing listens on a port for the agent. Configure the
endpoint in `config.toml`; see
[`docs/provider/MODEL.md`](docs/provider/MODEL.md) for keys, errors and cleanup.

- A provider is a `[providers.<name>]` table with `type = "openai-compatible"`,
  `base_url`, `default_model`, `api_key_env` or `env_file`, and `auth`. Requests
  go to `base_url` plus `/chat/completions`.
- `[provider] default_provider = "<name>"` picks the startup profile;
  `--provider-profile <name>` and `--model <model-id>` override it for one
  launch. `horon-agent provider add` writes the same tables.
- Readiness is `horon-agent provider current --json`. `ready`, `resolved_provider`,
  `selected_model` and, when not ready, `unavailable_reason` come from
  `src/cli/commands/report_info.rs`.
- With no configured provider, a model call fails before it reaches the network,
  and the error names the `openai-compatible` profile to add.
- Built-in OpenAI-compatible profiles such as `openrouter`, `zai`, `kimi`, and
  `ollama` can be named in `[provider] default_provider`; profiles that also have
  a CLI choice, such as `zai`, `kimi`, and `ollama`, work with `--provider <id>`
  too. `openrouter` has no CLI choice and is selected through
  `default_provider`. Keys live in the environment variable or `env_file` each
  profile declares (`crates/provider-metadata/src/catalog.rs`).

## Sessions

History is an append-only event log at
`~/.horon-agent/sessions/<id>/events.jsonl`, with large payloads in
content-addressed blobs. Archive, compaction, and rewind append records and
never delete an original.

```bash
./target/debug/horon-agent session inspect <id>       # read-only layout and damage report
./target/debug/horon-agent session doctor             # validate, never repair
./target/debug/horon-agent session migrate --apply    # dry run without --apply
```

Format and wire contract: `docs/protocol/SESSION_STORE.md`.

## Tools

The default profile is `core`: file read/write/edit, shell, search, web fetch
and search, skills, MCP, `undo`, and `ask`. Swarm, memory, and the other
optional tools stay in the tree but off; enable them with
`--tool-profile`, `HORON_AGENT_TOOL_PROFILE`, or `[tools] profile` in
`~/.horon-agent/config.toml`.

Agents in the same daemon and swarm can message each other with the `irc` tool
(`send`, `wait`, `inbox`, `list`). It is off by default; set `irc = true` under
`[tools]` to turn it on. Behavior, limits and the `[irc]` settings are in
`docs/runtime/IRC.md`.

## Verify

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Docs

- `AGENTS.md`: working rules and the fixed requirements.
- `docs/README.md`: index of the remaining docs (Korean).
