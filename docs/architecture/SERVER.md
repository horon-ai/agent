# Server structure

Language: **English** | [한국어](SERVER.ko.md)

## Overview

horon agent uses a **one server, many clients** structure. A single server process manages every session and all state. The clients are the
TypeScript and Rust SDKs. They attach to the server over a Unix socket and reconnect transparently when the connection drops or the server
re-execs. This repository contains no client implementation.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              SERVER (🔥 blazing)                              │
│                                                                             │
│  horon-agent serve                                                                │
│  ├── Unix socket:  /run/user/$UID/horon-agent.sock                                │
│  ├── Debug socket: /run/user/$UID/horon-agent-debug.sock                          │
│  ├── Registry:     ~/.horon/servers.json                                    │
│  ├── Provider (OpenAI-compatible, direct /chat/completions)                 │
│  ├── MCP pool (shared across sessions)                                      │
│  └── Sessions:                                                              │
│        ├── 🦊 fox   (active)  → "🔥 blazing 🦊 fox"                         │
│        ├── 🐻 bear  (active)  → "🔥 blazing 🐻 bear"                        │
│        └── 🦉 owl   (idle)    → "🔥 blazing 🦉 owl"                         │
└─────────────────────────────────────────────────────────────────────────────┘
         │              │              │
         ▼              ▼              ▼
    ┌─────────┐   ┌─────────┐   ┌─────────┐
    │ Client 1│   │ Client 2│   │ Client 3│
    │ 🦊 fox  │   │ 🐻 bear │   │ 🦉 owl  │
    └─────────┘   └─────────┘   └─────────┘
```

## Naming

```
SERVER = Adjective/Verb modifier          SESSIONS = Animal nouns
────────────────────────────              ────────────────────────
🔥 blazing   ❄️ frozen   ⚡ swift          🦊 fox    🐻 bear   🦉 owl
🌀 rising    🍂 falling  🌊 rushing        🌙 moon   ⭐ star   🔥 fire
✨ bright    🌑 dark     💫 spinning       🐺 wolf   🦁 lion   🐋 whale

Combined: "🔥 blazing 🦊 fox" = server + session
```

The server takes a random adjective or verb name at start (for example "blazing"). Each session takes an animal noun (for example "fox").
Together they form the natural phrase shown on screen: "🔥 blazing 🦊 fox".

The server name survives a reload through the registry (`~/.horon/servers.json`). When the server execs into a new binary on `/reload`, the
new process registers under a new name. Stale entries are cleaned up automatically.

## Lifecycle

```
  START                          CONNECT                     RELOAD
  ─────                          ───────                     ──────
  horon-agent (first run)              horon-agent (subsequent)          /reload
       │                              │                          │
       ├─▶ No server? Spawn daemon    ├─▶ Server exists?         ├─▶ Server execs into
       ├─▶ Wait for socket            │   Connect directly       │   new binary (same PID)
       ├─▶ Connect as client          │                          ├─▶ All clients disconnect
       └─▶ Create session             └─▶ Create/resume session  └─▶ Clients auto-reconnect
```

### Server start

Running `horon-agent` checks whether a server is already up.

1. **A server exists:** connect to it directly as a client
2. **No server:** start `horon-agent serve` as a detached daemon under `setsid`, wait for the socket, then connect

The server is fully detached from the client that started it through `setsid()`, so killing any client affects neither the server nor the
other clients.

For long-running deployments, `horon-agent serve --server-name <name>` or the `HORON_AGENT_SERVER_NAME` environment variable gives the daemon a
stable identity that clients can address. An optional `HORON_AGENT_SERVER_DISPLAY_NAME` environment variable exists for service managers that
prefer a display name. CLI input beats environment input. Names are normalized to a lowercase label that is safe for the registry, so
`mount-cloud/fabian` shows up as `mount-cloud-fabian`.

### Server shutdown

The server exits in these cases.
- **Idle deadline:** five minutes pass with no clients and no live headless swarm workers. The shared-server deadline is fixed at 300 seconds (`IDLE_TIMEOUT_SECS` in `crates/app-core/src/server.rs`) and `[server]` cannot change it. The watcher checks every 10 seconds, so shutdown can land slightly after five minutes.
- **Manual:** you kill the server process
- **Reload:** the server execs into a new binary (the socket path stays the same)

The debug control server turns the shared-server idle watcher off. Temporary servers use their own lifetime policy.

### Headless `run` sessions and the temporary server

`horon-agent run` uses no server at all. It builds an `Agent` in-process, sends one message, and exits.
`--socket <path>` only chooses which socket the tools dial, so in most runs nothing listens on that path.

One tool genuinely needs a server. `swarm` talks to the daemon over the socket because workers are headless sessions the server owns. A `run`
session used to fail these requests outright ("horon-agent server is not running"), which kept headless automation from using swarm. Now the
`run` command installs a spawner (`crate::server_spawn::register_on_demand_server_spawner`) and the first swarm request that cannot reach a
server starts one. On the shared socket it becomes an ordinary long-lived daemon; on a user-supplied `--socket` the server belongs to that run.

```
horon-agent run --socket /tmp/run.sock "spawn a worker that ..."
   │
   ├─▶ agent runs in-process; no server started
   ├─▶ swarm tool dials /tmp/run.sock → not found
   ├─▶ spawn `horon-agent serve --temporary-server --owner-pid <run pid>`
   │      on /tmp/run.sock, using the *current* executable
   └─▶ retry the request; the server exits within ~10s of the run
```

Properties worth knowing:

- **Lazy.** A run that never uses `swarm` pays nothing. `horon-agent run` start stays as it was.
- **Follows the socket.** A temporary server belongs to the run only on a user-supplied `--socket`. Without `--socket` the run prepares the ordinary shared daemon. Putting a temporary server on the shared path would hold the socket every later client uses and then vanish when the run ends.
- **Owned.** The temporary lifetime watcher shuts the server down when `--owner-pid` disappears, so a one-off run leaves no daemon behind. The socket, debug socket, daemon lock, build-hash sidecar, and metadata files are all removed. Workers live only as long as the run that spawned them. A `run` that spawns a worker and exits without waiting loses the worker by design.
- **Current binary.** The owning path starts `std::env::current_exe()` instead of the shared-server update candidate, so a locally built binary exercised through `--socket` answers (see "Verifying a change at runtime" in AGENTS.md).
- **`run` only.** Interactive and client runs do not install the on-demand spawner. They already prepare a daemon through their own start path, and a spawn triggered deep inside a tool would race that logic.

Swarm workers are headless sessions running inside this process regardless of the `spawn_mode` on the `spawn` request. The values are
`headless`, `inline` (the default), and `auto`, and all three create the same in-process session and open no terminal window. Old config or
environment values `visible` and `headed` read as `inline` (`SwarmSpawnMode` at `crates/config-types/src/lib.rs:795`).

### Session ownership marker files (`active_pids`)

`~/.horon/active_pids/<session_id>` holds the PID of the process that owns the session. In server mode that is the daemon PID, so several
sessions can share one PID. Despite the directory name, "active" means process ownership, not that a client is attached or that the model is
generating. A session the daemon holds can stay registered after its client disconnects.

The marker file is removed when the session is closed or deleted. It can also outlive the process, so a marker file alone does not prove the
owner is alive. `active_session_ids()` lists marker file names without checking PID liveness, while `session_presence()` filters dead owners.
These files support lifetime and crash recovery; they do not track display. A separate `streaming_pids` marker file tracks model response
generation.

### Remote client working directory

By default a client sends its current working directory when it subscribes and the server uses that as the session working directory. A socket
forwarding wrapper for a remote daemon can separate the client path from the server path with `--remote-working-dir`.

```bash
horon-agent --socket /tmp/horon-agent.sock -C /local/checkout --remote-working-dir /remote/checkout
```

The `-C` path must exist on the client. `--remote-working-dir` must be an absolute path on the server.

### Client reconnection

Clients carry a reconnect loop. When the connection drops (server reload, network trouble), they proceed in this order.

1. The client shows "Connection lost - reconnecting..."
2. It retries with exponential backoff (1s, 2s, 4s... up to 30s)
3. Once reconnected it continues the same session (session state lives on disk)
4. If the server re-execed, the client can re-exec itself too when a newer client binary exists

### Hot reload (`/reload`)

1. The client sends `Request::Reload` to the server
2. The server sends a `Reloading` event to the requesting client
3. The server calls `exec()` on the new binary with the `serve` arguments
4. The new server process starts on the same socket
5. Every client reconnects automatically
6. The initiating client also re-execs if its own binary is stale

## Socket paths

```
/run/user/$UID/
├── horon-agent.sock              # Main communication socket
├── horon-agent-debug.sock        # Debug/testing socket
└── horon-agent-daemon.lock       # "one daemon on horon-agent.sock" flock
```

Changing the path with `--socket` or `HORON_AGENT_SOCKET` also gives the side lock file its own place (`<socket>.daemon.lock`), so a separate
server can hold its socket while the shared daemon keeps running. A daemon attaches to one *socket*, not to the runtime directory. The spawn
lock was always socket-scoped; the daemon lock now matches.

## Reload

`/reload` and `horon-agent server reload` exec the server into a new binary and clients reconnect. The reload checkpoint is the `shared-server`
channel and uses the same path for every session. A session interrupted mid-reload continues with a `reload_recovery` directive.

## Key behaviors

| Situation | Behavior |
|----------|----------|
| First `horon-agent` run | Starts the server daemon and connects |
| Later `horon-agent` runs | Connect to the existing server |
| Kill one client | The server and the other clients are unaffected |
| `/reload` | The server execs into the new binary and clients reconnect |
| Close every client | The shared server exits on the idle deadline after five minutes without live headless swarm workers (unless debug control is on) |
| Resume a session | `horon-agent --resume fox` reconnects to the existing session |
| `horon-agent run` | The agent runs in-process and uses no server unless a tool needs one |
| `horon-agent run` + `swarm` | Prepares the shared daemon, or a temporary server owned by that run when `--socket` is user-supplied |