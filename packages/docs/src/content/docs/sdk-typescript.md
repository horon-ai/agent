---
title: TypeScript SDK
description: "@horon-ai/agent-sdk: launch a private agent instance or attach to the user's daemon, over protocol v1."
section: Build
order: 1
---

`@horon-ai/agent-sdk` is the TypeScript client for the harness API (protocol v1)
— the versioned boundary between the agent runtime and any client. It talks
NDJSON over the harness API socket and mirrors `crates/harness-api`; schema drift
is guarded from both sides.

## Install

Neither SDK is on npm yet, so install from a source checkout:

```sh
cd packages/sdk/typescript
npm install
npm run build
```

The packages and the publish workflow are in place; the registry names are
reserved and the release procedure is in `packages/sdk/typescript/RELEASING.md`.

Node 20 or newer. The SDK installs the runtime for supported macOS, Linux and
Windows architectures as an optional platform package, so a separate horon-agent
installation is not normally required. If optional dependencies are disabled,
`launch()` falls back to `horon-agent` on `PATH`; `binary` selects a specific
executable.

## Two ways to use it

**Embed the engine** (`launch`). Starts a private instance with its own state,
sessions and sockets. It cannot see or disturb the agent the user runs in their
terminal, and `close()` shuts it down. This is the default for applications, and
private instances never auto-update.

**Automate the user's own agent** (`connect`). Attaches to the daemon already
running on the machine. Use it for an editor plugin, a dashboard, or a hotkey
tool; anything done there is visible in the user's terminal. Start the bridge
once:

```sh
horon api-bridge
```

That starts the server if needed and exposes `$XDG_RUNTIME_DIR/horon-agent-api.sock`.
The socket is owner-only. Use `--api-socket <path>` to listen elsewhere and set
`HORON_AGENT_API_SOCKET` to the same path in the client.

## First run

```ts
import { HoronAgentClient } from "@horon-ai/agent-sdk";

const client = await HoronAgentClient.launch({ workingDir: process.cwd() });
const session = await client.createSession();
console.log((await client.run(session.session_id, "hello")).text);
await client.close(); // stops the instance
```

Streaming and tool calls:

```ts
const session = await client.createSession(process.cwd());
const turn = await client.run(session.session_id, "What files are in src/?", {
  autoApprove: true,
  onEvent: (event) => {
    if (event.ev === "text_delta") process.stdout.write(event.text);
  },
});

console.log("\ntools:", turn.toolCalls.map((call) => call.name));
console.log("tokens:", turn.usage);
client.close();
```

## Logins and state

Provider logins are inherited from the user by default, because an instance with
no credentials cannot reach a model. `inheritLogins: false` starts empty.
`horonAgentHome` keeps sessions across runs instead of using a temporary
directory.

Inheritance shares only recognized credential **files**, never whole config or
tool directories. That keeps rotating OAuth tokens coherent without exposing
unrelated transcripts, and instance cleanup cannot recurse into the user's
credential directories. The launched process still runs as the current OS user and
can spend those accounts' quota, so disable inheritance when running untrusted
application code.

## System prompt

`systemPrompt` is sent as `system_prompt` and replaces the **entire assembled
system prompt**, not just its base text. The override is immutable after session
creation and persisted by the runtime for resume. Omit it to keep normal
assembly; an empty string explicitly replaces the prompt with an empty one.

## Durable runtime

The bridge advertises `durable_runtime_v1`, `monitor_v1`, `snapshot_marker_v1` and
`ask_receipts_v1`. The SDK rejects a request before writing it to the socket when
the matching capability is absent, so an old server never receives an unknown
request. Corresponding methods include `getRuntimeState`, `applyControl`,
`getControlReceipt`, `getSnapshot` and `respondToAskWithId`.
