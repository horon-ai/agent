---
title: Architecture
description: One server, many clients; crate boundaries; the model path and the session store.
section: Build
order: 3
---

## One server, many clients

A single daemon process manages every session and all state. Clients attach to it
over a Unix socket and reconnect transparently when the connection drops or the
server re-execs.

```
┌──────────────────────────────────────────────┐
│  horon serve                                 │
│  ├── Unix socket    $XDG_RUNTIME_DIR/...     │
│  ├── Debug socket   ...-debug.sock           │
│  ├── Registry       ~/.horon/servers.json    │
│  ├── Provider       OpenAI-compatible        │
│  ├── MCP pool       shared across sessions   │
│  └── Sessions       append-only event logs   │
└──────────────────────────────────────────────┘
        │              │              │
        ▼              ▼              ▼
   TypeScript SDK   Python SDK    ACP / horon-tui
```

## The model path

Model requests leave through one path and nothing else serves models in this
build: no second provider runtime, no router process, no plugin host.

1. `[provider] default_provider` selects the startup profile; `--provider-profile
   <name>` selects a `[providers.<name>]` table instead.
2. The model comes from `--model <id>`, the profile's `default_model`, then
   `[provider] default_model`. If none is set the call ends in an error.
3. The request URL is `base_url` + `/chat/completions`; the API prefix such as
   `/v1` is part of `base_url`.
4. Tool definitions travel as `tools` in the same request body.

The one documented exception is the optional `generate_image` tool: when the active
profile names `roles.image`, it posts to `/images/generations` and `/images/edits`
on that same profile, with the same base URL, credential and `extra_body`. It is
off unless `[tools] generate_image = true`.

## Two binaries

- `horon-agent` — headless: `serve`, `run`, `repl`, `api-bridge`, `acp`. Its
  dependency graph holds no `ratatui`, `crossterm` or `horon-tui` crate.
- `horon-tui` — the interactive terminal client, a separate package on
  `horon-agent-sdk`. Bare `horon` on a terminal replaces itself with it.

One engine, two binaries. The split is enforced in CI by
`scripts/ci/check_headless_graph.py`, which walks every package reachable from
`horon-agent`.

## Crate boundaries

| Crate | Owns |
| --- | --- |
| `base` | Config, prompt assembly, provider selection, safety classification |
| `provider-core` | Credential modes, model selection and prefixes |
| `app-core` | The daemon, sessions, tool runtime, update path |
| `session-store` | Event log, blobs, catalogs, derived caches |
| `harness-api` | The versioned request and event surface |
| `harness-api-server` | The bridge and translation between the daemon and that surface |
| `agent-sdk` | The Rust client, launch and lifecycle |
| `tui` | The `horon-tui` terminal client; it depends on `agent-sdk`, never on the engine |

## Durability

Session content is append-only and every file is owner-only. `[storage] sync =
full` confirms originals with `F_FULLFSYNC` on macOS and `fsync` elsewhere;
`fast` is the cheapest crash-durable flush. Derived caches are never confirmed —
they are rebuilt from the originals, so a crash loses no content.

## Storage and sockets

The session store root is `$HORON_HOME/sessions`. The daemon socket is selected
with `HORON_AGENT_SOCKET`; the SDK socket with `HORON_AGENT_API_SOCKET`. Both are
owner-only, and the bridge translates between them so a client never speaks the
internal protocol directly.
