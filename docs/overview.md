# Overview

Horon is a coding agent with two equal modes on one engine. In headless mode a program drives a session; in TUI mode a person does. Both use the same daemon, sessions, tools and configuration.

## Binaries

| Binary | Role | Needed |
| --- | --- | --- |
| `horon-agent` (also `horon`, `hrn`) | The agent: `serve`, `run`, `repl`, `api-bridge`, `acp`, `rpc` and maintenance commands. It never opens a full-screen UI. | Always |
| `horon-tui` | The terminal client, a client of the daemon through the SDK. | Only for TUI mode; not built for FreeBSD |

Both ship from the same release and are installed into the same version directory. `horon-agent --version` and `horon-tui --version` print different numbers for the same release; compare release tags, not strings.

## Which mode starts

| You run | What happens |
| --- | --- |
| `horon` on a terminal, `horon-tui` beside the binary, `HORON_NO_TUI` unset | Starts the daemon if none is up, then replaces itself with `horon-tui` |
| `horon` with a pipe, `HORON_NO_TUI=1`, or no `horon-tui` | Starts nothing, prints which condition failed and how to fix it, exits `1` |
| `horon run "<message>"` | Runs one message in-process (no daemon), runs tools until the turn ends, exits |
| `horon repl` | A plain line prompt, no full-screen client |
| `horon serve` | The daemon only, on a Unix socket (named pipe on Windows) |
| `horon acp` / `horon rpc` / `horon api-bridge` | Adapters for editors, host programs and SDKs (see [editors.md](editors.md), [rpc.md](rpc.md), [harness-api.md](harness-api.md)) |

## The daemon

- The first `horon` starts `horon serve` detached (`setsid`); later clients connect to it. Killing a client never stops the daemon or other clients.
- It exits five minutes after the last client leaves when no unattended swarm worker is alive, or on `SIGTERM`/`SIGINT` after draining in-flight saves.
- `horon server reload` hands live sessions to a freshly started binary at the same socket; use it after an upgrade. `horon server stop --force` stops it.
- A session idle for `[server] idle_hibernation_secs` (default `1800`) drops its derived caches; the next turn rebuilds them from memory.
- `horon run` needs no daemon. If its `swarm` tool needs one, the first swarm request starts it.

## Model path

Each request goes straight to the endpoint of the active provider profile, over the wire its `type` names: Chat Completions, the Responses API, Anthropic Messages, or Gemini generateContent. There is no router process. With no profile configured, a model call stops before the network and names the missing `[providers.<name>]` table. See [providers.md](providers.md).

## Sessions

A session is an append-only event log at `<home>/sessions/<id>/events.jsonl`, with large payloads stored by content hash beside it. Compaction, rewind and archive append records; nothing deletes an original except `horon session delete`. See [sessions.md](sessions.md).

## Data home

`<home>` is `~/.horon`, or `HORON_HOME` when set. It holds `config.toml`, sessions, memory, installed packages, logs and release versions. See [configuration.md](configuration.md).
