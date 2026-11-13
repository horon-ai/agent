---
title: Getting started
description: horon agent is a headless coding agent in one Rust binary that talks to any OpenAI-compatible endpoint.
section: Start
order: 1
---

horon agent is a headless coding agent. One Rust binary, one OpenAI-compatible
endpoint, no terminal UI in the headless package. It runs a turn, calls tools,
and exits — or stays up as a daemon that SDKs and editors attach to over a Unix
socket.

## What it is

- **One model path.** Requests go straight to `/chat/completions` on the
  endpoint you configure. There is no router process and no plugin host.
- **Two binaries, one engine.** `horon-agent` is headless (`serve`, `run`,
  `repl`, `api-bridge`, `acp`). `horon-tui` is the separate interactive terminal
  client, built on the SDK.
- **Append-only sessions.** History is an event log on disk; archive, compaction
  and rewind append records and never delete an original.
- **ACP v1 and v2.** `horon acp` serves both protocol versions and answers
  `initialize` with the highest version both sides support.

## First turn

Install the CLI (see [Install](/install/)), then point the agent at an endpoint.
Any OpenAI-compatible server works; this example uses a local one with no key.

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

Check that a profile resolves, then run a turn:

```sh
horon provider current --json   # "ready": true when a profile resolves
horon run "summarize this repo"
```

With no profile configured, a model call stops before the network and names the
missing `[providers.<name>]` table. Nothing silently falls back to another path.

## The interactive client

Run bare `horon` (or `hrn`, or `horon-agent`) on a terminal and it starts the
server if needed, then replaces itself with `horon-tui`, the client that sits
beside the agent binary and owns the screen from then on. The handoff checks
three things first: standard input and standard output are both terminals,
`HORON_NO_TUI` is unset, and `horon-tui` exists next to the running executable.
A pipe or a redirect keeps the headless path, which is what scripts and CI want.
When a check fails the command names it, prints the one-line hint about
installing `horon-tui` next to the binary, and exits non-zero; a server it had
already started stays up.

```sh
horon                      # starts the server, then opens horon-tui
horon --resume <session>   # the same, attached to an existing session
HORON_NO_TUI=1 horon       # stay headless
```

The client ships with the installer as an optional archive. A release that does
not publish it, or a digest that does not match, prints one line and installs the
headless agent alone; a source build and FreeBSD never stage a released client.
Keys, themes, the status line and command files are documented in
`docs/runtime/TUI.md`.

## Where to go next

- [Configuration](/configuration/) — provider profiles, model roles, storage.
- [CLI](/cli/) — every subcommand and what it does.
- [Tools](/tools/) — the core tool set and how deferred loading works.
- [TypeScript SDK](/sdk-typescript/) and [Python SDK](/sdk-python/) — embed the
  engine in an application.
