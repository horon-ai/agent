<p align="center">
  <img src="docs/assets/hero.svg" alt="horon agent: one agent with two equal modes, headless and TUI, over Chat Completions, Responses, Anthropic Messages and Gemini generateContent endpoints" width="720">
</p>

<p align="center">
  <strong>One agent, two equal modes. A program drives it, or you drive it from the terminal, over Chat Completions, Responses, Anthropic Messages or Gemini generateContent.</strong>
</p>

<p align="center">
  Language: <strong>English</strong> | <a href="README.ko.md">한국어</a>
</p>

<p align="center">
  <a href="https://github.com/horon-ai/agent/releases/latest"><img src="https://img.shields.io/github/v/release/horon-ai/agent?style=flat&amp;colorA=222222&amp;colorB=58A6FF" alt="Latest release"></a>
  <a href="https://github.com/horon-ai/agent/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/horon-ai/agent/ci.yml?branch=main&amp;style=flat&amp;colorA=222222&amp;colorB=3FB950" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/horon-ai/agent?style=flat&amp;colorA=222222&amp;colorB=58A6FF" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-DEA584?style=flat&amp;colorA=222222&amp;logo=rust&amp;logoColor=white" alt="Rust"></a>
</p>

horon agent is one agent with two equal modes on one engine. In headless mode a program drives a session: a one-shot command, an SDK, an editor over ACP, or a host program over stdio RPC. In TUI mode a person drives it from the terminal through the `horon-tui` client. Both modes attach to the same daemon and share its sessions, tools, MCP servers and configuration. They differ only in who is on the other end, and neither is a reduced form of the other.

Model calls speak one of four wires, picked by the profile's `type`: OpenAI Chat Completions, OpenAI Responses, Anthropic Messages or Gemini generateContent. One provider runtime posts directly to the configured endpoint.

Each release ships two binaries built from the same tag. The agent binary `horon-agent` serves headless mode and runs the daemon both modes attach to; it never draws a full-screen terminal UI. The TUI binary `horon-tui` is a separate package built on the agent SDK and published beside it as its own archive.

| Mode | Who drives it | Entry points |
| --- | --- | --- |
| Headless | A program | `run`, `serve`, `api-bridge`, `acp`, `rpc` |
| TUI | A person at a terminal | bare `horon`, which replaces itself with `horon-tui` |

## Numbers

![Benchmark summary: horon against seven tools](docs/performance/charts/00-summary.svg)

- Linux one-shot median 42.5 ms against jcode's 138.0 ms, and macOS 88.1 ms against 377.1 ms. Windows is the exception: 141.8 ms against 128.4 ms, inside overlapping ranges.
- One history page from a 100k-event session reads 18,554 B instead of 62,169,922 B and takes 5.9 ms instead of 26.2 s; a 200 MiB source hash peaks at 65,600 B instead of 268,435,520 B.
- Memory per additional session is still jcode's win: 151.0 KB against 98.1 KB on Linux.

![Session store: one request, before and after the plan](docs/performance/charts/record-01-session-store.svg)

Every number above is a measured value with its run recorded in [docs/performance/RESULTS.md](docs/performance/RESULTS.md); the method and its limits are in [docs/performance/BENCHMARK.md](docs/performance/BENCHMARK.md). [Install](#install) takes one command.

## Install

**macOS · Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/install.sh | bash
```

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/install.ps1 | iex
```

**Container (Linux, amd64 and arm64)**

```sh
docker run --rm ghcr.io/horon-ai/agent:latest --version
```

**From source** (Rust 1.99 or newer, edition 2024)

```sh
cargo build --release -p horon-agent
./target/release/horon-agent --version
```

The installer downloads the release archive along with `SHA256SUMS` and hands both to `horon install-release`, which verifies the checksum, stages an immutable version directory and switches the `stable` and `current` links. A version directory that already holds different bytes is refused, not overwritten ([details](docs/release/INSTALL.md)).

It links three launchers: `horon`, the command every example here uses; `hrn`, the same command under a shorter name; and `horon-agent`, the executable's own name. All three run the same file. Data lives in `~/.horon`, which `HORON_HOME` overrides; releases up to v0.1.1 use `~/.horon-agent`.

Release assets cover Linux (x86_64, aarch64), macOS (aarch64, x86_64), Windows (x86_64, aarch64) and FreeBSD (x86_64).

## Quick start

Point the agent at an endpoint. The profile's `type` picks the wire; this one is a local Chat Completions server and needs no key.

```toml
# ~/.horon/config.toml
[provider]
default_provider = "local"

[providers.local]
type = "openai-compatible"
base_url = "http://127.0.0.1:11434/v1"
auth = "none"
default_model = "my-model"
```

For a hosted endpoint, use `auth = "bearer"` and `api_key_env = "MY_API_KEY"` instead. Then check readiness and run a turn:

```sh
horon provider current --json   # "ready": true once a profile resolves
horon run "summarize this repo"
```

<p align="center">
  <img src="docs/assets/terminal-run.svg" alt="Terminal transcript: provider current reports ready, then horon run executes an ls tool call and the model answers mock-ok" width="860">
</p>

<sub>Captured from a real run of a `dev` build against the repository's deterministic mock model server (`scripts/perf/mock_llm_server.py`), with an isolated home directory. The model reply is the mock's fixed text.</sub>

With no profile configured, a model call stops before the network and names the missing `[providers.<name>]` table. [docs/provider/MODEL.md](docs/provider/MODEL.md) lists every key: `headers`, `extra_body`, `context_window`, and built-in profiles such as `openrouter`, `zai`, `kimi` and `ollama`.

## Ways to run it

Headless mode covers every subcommand below except bare `horon`, which opens TUI mode. `horon repl` is a plain line prompt served by the agent binary itself, for someone who wants no full-screen client.

| Command | What it does |
| --- | --- |
| `horon` (no subcommand) | On a terminal, starts the server and replaces itself with the `horon-tui` client beside it, with keybindings, user files and a settings screen ([docs](docs/runtime/TUI.md)). A pipe, `HORON_NO_TUI=1` or a missing sibling prints a sentence naming the condition and the install command, then exits non-zero. |
| `horon run "<message>"` | Sends one message, runs tools until the turn ends, then exits. |
| `horon repl` | One line at a time in the terminal, without the full-screen client. |
| `horon serve` | Background agent daemon on a Unix socket (`HORON_AGENT_SOCKET`). |
| `horon api-bridge` | Harness API on a local socket for SDK clients (`HORON_AGENT_API_SOCKET`): a Unix socket, or a named pipe on Windows. |
| `horon acp` | ACP v1 and v2 adapter on stdio, for editors. |
| `horon rpc` | stdio JSONL RPC adapter for a host program that owns the process: one JSON command per line in, one frame per line out ([docs](docs/protocol/RPC.md)). `--mode rpc` selects the same adapter. |

`horon acp` answers `initialize` with the highest protocol version both sides support, so v1 clients and v2 clients both work.

### Terminal client

`horon-tui` is TUI mode: a transcript pane that scrolls back and renders the model's Markdown, a multi-line editor that grows to eight rows, an approval prompt that parks a tool call and takes `y`, `a`, `n` or `esc`, model and session pickers, and a settings screen behind `/settings`. It is a separate package (`crates/tui`) on `horon-agent-sdk` and attaches to the same daemon and sessions as every other client, which is what keeps terminal libraries out of the agent binary.

It starts when you run bare `horon`, `hrn` or `horon-agent` on a terminal. The command starts the server if none is running, then replaces itself with the `horon-tui` file beside the agent, handing over the working directory so the new session belongs to the directory you ran it from, and the session id when you passed `--resume <id>`. Three conditions are checked first: stdin and stdout are both terminals, `HORON_NO_TUI` is unset, and `horon-tui` sits next to the running executable. A pipe or a redirect prints that sentence instead, so scripts and CI get the reason rather than a client. When a condition fails, the command names it, prints the install hint for the interactive client, and exits non-zero.

The installer fetches `horon-tui-<platform suffix>.tar.gz` beside the agent archive and verifies it against `SHA256SUMS`. That archive is optional: a release that does not publish one, a digest that does not match, a source build and FreeBSD all install or keep the agent alone with one warning line. User files are `$HORON_HOME/tui.toml`, `themes/<name>.json`, `keybindings.toml`, `commands/<name>.md` and the project's `.horon/commands/<name>.md`; every key is in [docs/runtime/TUI.md](docs/runtime/TUI.md).

### Editors (ACP)

Register `horon` (or `horon-agent`) with the argument `acp` as an external agent. In Zed (`settings.json`):

```json
{
  "agent_servers": {
    "horon agent": {
      "type": "custom",
      "command": "/absolute/path/to/horon",
      "args": ["acp"]
    }
  }
}
```

JetBrains and other ACP clients take the same two values. [docs/protocol/ACP.md](docs/protocol/ACP.md) covers the stdio contract and the v2 differences.

### Host programs (RPC)

A program that owns the process can drive the agent instead of handing it a terminal. `horon rpc`, or `horon --mode rpc`, reads one JSON command per line on stdin and writes one frame per line on stdout, with streaming turns, cancellation and session controls; the subcommand wins when a host passes both. stdout carries frames only, diagnostics go to stderr, and closing stdin ends the process. The mode speaks protocol version 1, so a frame over one mebibyte becomes an error instead of being chunked. This is neither the terminal client nor ACP: `horon-tui` attaches over the harness API, and RPC serves a host that cannot speak ACP ([docs](docs/protocol/RPC.md)).

### SDKs

| Language | Package name | Location |
| --- | --- | --- |
| TypeScript | `@horon-ai/agent-sdk` | [`packages/sdk/typescript`](packages/sdk/typescript) |
| Python | `horon-agent-sdk` | [`packages/sdk/python`](packages/sdk/python) |

Both talk the harness API (protocol v1) over the `api-bridge` socket and can start their own daemon with `launch()`. Neither package is on npm or PyPI yet, so install from a source checkout; each SDK README has the steps.

## Features

<details>
<summary>All features</summary>

| Feature | What it does |
| --- | --- |
| A core tool set of 18 | `bash`, `read`, `write`, `edit`, `replace`, `apply_patch`, `agentgrep`, `ls`, `webfetch`, `websearch`, `skill_manage`, `mcp`, `monitor`, `undo`, `ask`, `todo`, `session_search` and `swarm`. A core session sends seven of them up front plus `toolbox`, names the other eleven in the system prompt, and reaches them through `toolbox`, which keeps the first request small. Profiles: `core` (default), `full`, `acp`, `minimal`, `none` ([docs](docs/runtime/TOOLS.md)). |
| Append-only sessions | History is an event log at `<home>/sessions/<id>/events.jsonl`, with large payloads in content-addressed blobs. Archive, compaction and rewind append records and never delete an original. The `horon session` subcommands inspect, doctor, export, import, migrate, recover-to, archive and restore that store; `delete <id> --yes` is the one destructive command, and it refuses a session a running daemon has open ([docs](docs/protocol/SESSION_STORE.md)). |
| Edit safety | A journal backs `undo` for file edits, and `redo` and hashline anchors are opt-in ([docs](docs/runtime/TOOLS.md)). |
| Goal budgets | A continuation goal can carry `budget_tokens`. When the session's spend reaches it, the goal moves to `budget_limited`, gets one wrap-up turn that tells the model to record what it has, and stops; raising the budget and setting the goal back to `active` is an operator decision ([docs](docs/runtime/TURNS.md)). |
| MCP and skills | `horon mcp` lists configured MCP servers and manages remote-server OAuth credentials; skills are read through `skill://` and managed with `skill_manage`. |
| Unattended runs | Turn recovery, `horon runtime-state` for durable pending inputs and `horon approvals` for a run that stopped waiting on a person ([docs](docs/safety/SAFETY.md)). |
| Agent-to-agent messaging | An opt-in `irc` mailbox bus (`[tools] irc = true`) lets agents in one daemon `send`, `wait`, `inbox` and `list` ([docs](docs/runtime/IRC.md)). |
| Workspace trust | Project-local MCP config is skipped until its digest is trusted, and `--trust-workspace` records the decision ([docs](docs/safety/SECURITY.md)). |
| TLS without OpenSSL | rustls with the union of bundled webpki roots and the OS trust store; the Linux binary needs only glibc, libm and libgcc_s ([docs](docs/release/INSTALL.md)). |

</details>

## Architecture

<p align="center">
  <img src="docs/assets/architecture.svg" alt="Architecture: SDKs and editors over ACP reach the agent daemon through the harness API and the ACP adapter, and the terminal client horon-tui attaches the same way while a bare run hosts the agent in-process; the daemon stores sessions in an event log and calls the profile's endpoint directly" width="860">
</p>

Crate boundaries and platform adapters are in [docs/architecture/CRATES.md](docs/architecture/CRATES.md); the daemon, sessions and sockets are in [docs/architecture/SERVER.md](docs/architecture/SERVER.md). CI keeps terminal libraries out of the agent binary: `scripts/ci/check_headless_graph.py` fails if `ratatui`, `crossterm`, `horon-tui` or any `horon-agent-tui*` crate becomes reachable from `horon-agent`.

## Benchmarks

`.github/workflows/benchmark-docs.yml` runs a one-shot turn on GitHub-hosted runners at commit `729624b` (measured 2026-11-18) against seven other agents, with a deterministic mock model and exactly one `ls` tool call per turn. jcode v0.99.1 is the closest competitor and the one compared below. Each OS runs on a different machine, so compare inside a row and never across rows.

<p align="center">
  <img src="docs/performance/charts/03-oneshot-time.svg" alt="Bar chart of median one-shot time per OS for eight agents. Linux: horon 42.5 ms, jcode 138 ms. macOS: horon 88.1 ms, jcode 377 ms. Windows: horon 142 ms, jcode 128 ms" width="780">
</p>

| Linux (`ubuntu-24.04`) | horon | jcode |
| --- | --- | --- |
| One-shot turn, median | 42.5 ms | 138.0 ms |
| Skill invocation, median | 38.0 ms | 53.1 ms |
| MCP tool call, median | 58.8 ms | 162.9 ms |
| Peak tree RSS, one-shot | 39.4 MB | 42.2 MB |
| Memory per additional session, idle after 3 turns | 151.0 KB | 98.1 KB |
| First model request, body / tool catalog bytes | 3,756 / 2,368 | 3,292 / 1,433 |
| Install size | 110.6 MB | 163.4 MB |

horon took all three Linux time rows and the one-shot peak memory, and macOS followed the same shape: one-shot 88.1 ms against 377.1 ms, skill 64.0 ms against 357.6 ms, MCP 115.1 ms against 486.4 ms. **Windows is the exception.** jcode had the lower median on the one-shot (128.4 ms against 141.8 ms) and the skill run (172.6 ms against 220.0 ms). In both cells the min–max ranges overlap, so neither counts as a clear win; horon kept the MCP call (211.0 ms against 318.3 ms). jcode also used less memory per additional session on Linux and Windows (98.1 KB against 151.0 KB, 63.8 KB against 122.8 KB), less peak memory on the Windows one-shot (26.9 MB against 36.1 MB), and a smaller first request on every OS.

These are single-commit CI samples, not a guarantee. The full tables for eight tools, the TUI suite, the measurement method and what the numbers do not say are in [docs/performance/RESULTS.md](docs/performance/RESULTS.md) and [docs/performance/BENCHMARK.md](docs/performance/BENCHMARK.md).

## Configuration defaults

<details>
<summary>All defaults</summary>

| Setting | Default | Other values |
| --- | --- | --- |
| Tool profile (`--tool-profile`, `[tools] profile`) | `core` | `full`, `acp`, `minimal`, `none` |
| `[acp] profile` | `standard` | `extended`, `full` |
| `[tools] ask_policy` | `recommended` | `error` |
| `[tools] bash_pty` | `auto` | `off`, `always` |
| `[features] turn_recovery` | `conservative` | `aggressive`, `off` |
| `[storage] sync` | `full` (power-loss durable) | `fast` |
| `[server] idle_hibernation_secs` | `1800` | `0` disables |
| Update check on `serve` | not run | `--no-update` skips it everywhere |

</details>

[`AGENTS.md`](AGENTS.md) carries the complete set of defaults that are part of the contract.

## Documentation

The docs are written in English (default) and Korean (`.ko.md` next to each file, as in `docs/README.ko.md`). The English files are compiled into the binary, where the `horon_agent_docs` tool serves them. [agent.horon.ai](https://agent.horon.ai) is the Mintlify site in [packages/docs-site](packages/docs-site). Its Reference tab is generated from `docs/`, `PHILOSOPHY.md` and the SDK and npm READMEs by `scripts/ci/sync_docs_site.py`; its guide pages are written by hand in English, so a behavior change that a guide repeats updates that guide too.

| Topic | Read |
| --- | --- |
| Documentation site (`agent.horon.ai`) | [https://agent.horon.ai](https://agent.horon.ai) |
| Documentation site source | [packages/docs-site](packages/docs-site) |
| Index and fixed requirements | [docs/README.md](docs/README.md) |
| Design principles | [PHILOSOPHY.md](PHILOSOPHY.md) |
| Terminal client and its user files | [docs/runtime/TUI.md](docs/runtime/TUI.md) |
| Model endpoints and API keys | [docs/provider/MODEL.md](docs/provider/MODEL.md) |
| Editors and ACP | [docs/protocol/ACP.md](docs/protocol/ACP.md) |
| Host programs over stdio RPC | [docs/protocol/RPC.md](docs/protocol/RPC.md) |
| Harness API for SDKs | [docs/protocol/HARNESS_API.md](docs/protocol/HARNESS_API.md) |
| Tools, approvals, edit safety | [docs/runtime/TOOLS.md](docs/runtime/TOOLS.md) |
| Turns, recovery, `--resume` | [docs/runtime/TURNS.md](docs/runtime/TURNS.md) |
| Release install and Linux servers | [docs/release/INSTALL.md](docs/release/INSTALL.md) |
| Performance method and results | [docs/performance/BENCHMARK.md](docs/performance/BENCHMARK.md) |

## Contributing

Read [`AGENTS.md`](AGENTS.md) first; it is the governing contract. Three requirements hold for every change: no terminal UI in the agent binary `horon-agent`, with TUI mode living in the TUI binary `horon-tui`; ACP v1 and v2 both served; and no measured performance regression against the recorded baseline.

- Branch from `dev` and open the pull request against `dev`; `main` changes through release pull requests. Every pull request must link a real issue in this repository (`Closes #123`); the `Require Linked Issue` check enforces it.
- Run the checks before pushing. `scripts/ci/check_all.sh` runs the Format and Quality Guardrails steps of CI, and the Rust checks are:

  ```sh
  cargo fmt --all -- --check
  cargo clippy --locked --all-targets --all-features -- -D warnings
  cargo test --workspace
  ```

- Performance claims come from the benchmark workflows on GitHub Actions, never from a developer machine.

## License

[MIT](LICENSE). Copyright 2025 Jeremy Huang and 2026 horon-ai.
