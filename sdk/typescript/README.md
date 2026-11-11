# @horon-ai/agent-sdk

TypeScript SDK for the **horon-agent harness API** (protocol v1) — the stable,
versioned boundary between the horon-agent agent runtime and any client.

It mirrors `crates/harness-api` and talks NDJSON over the harness API
Unix socket. Schema drift is guarded from both sides: a Rust test fails if a
variant is added without mirroring it here, and a Node test fails if the tag
sets diverge.

Full documentation: **[horon-agent.sh/sdk](https://horon-agent.sh/sdk)**

## Install

```bash
npm install @horon-ai/agent-sdk
```

From a source checkout:

```bash
cd sdk/typescript
npm install
npm run build
```

## Requirements

Node 20 or newer. The SDK installs the correct horon-agent runtime for supported
macOS, Linux, and Windows architectures as an optional platform package, so a
separate horon-agent installation is not normally required. If optional dependencies
are disabled, `launch()` falls back to `horon-agent` on `PATH`; `binary` can also
select a specific executable.

macOS and Linux are exercised end to end in CI. Windows builds and is wired up
(the bridge listens on a named pipe rather than a Unix socket, and the SDK
resolves the same pipe name), but it has no live end-to-end coverage yet, so
treat it as untested rather than unsupported and please report what breaks.

`launch()` needs nothing else: it starts its own daemon and bridge. `connect()`
needs a bridge already running, which the user starts once and leaves running.
The bridge ships in the runtime package, so no Rust toolchain is needed. To use
`connect()` with the user's global horon-agent, start its bridge:

```bash
horon api-bridge
```

It starts the horon-agent server if one is not already up, then exposes the API
socket (`$XDG_RUNTIME_DIR/horon-agent-api.sock`) and translates onto the internal
daemon socket. The socket is owner-only, matching the daemon socket it fronts.

Use `--api-socket <path>` to listen elsewhere, and set `HORON_AGENT_API_SOCKET` to
the same path in your client. (The global `--socket` selects the _internal
daemon_ socket, which is a different thing.)

## Two ways to use horon-agent

**Embed horon-agent as an agent engine** (`launch`). Starts a private instance with
its own state, sessions, and sockets. It cannot see or disturb the horon-agent the
user runs in their terminal, and `close()` shuts it down. This is the default
for applications. Private instances never auto-update: they run the horon-agent binary that launched them.

```ts
const client = await HoronAgentClient.launch({ workingDir: process.cwd() });
const session = await client.createSession();
console.log((await client.run(session.session_id, "hello")).text);
await client.close(); // stops the instance
```

Provider logins are inherited from the user by default, since an instance with
no credentials cannot reach a model. Pass `inheritLogins: false` to start empty
and supply your own. Pass `horonAgentHome` to keep sessions across runs instead of
using a temporary directory.

Inheritance shares only recognized credential **files**, never whole config or
tool directories. This keeps rotating OAuth tokens coherent without exposing
unrelated transcripts and state, and instance cleanup cannot recurse into the
user's credential directories. Temporary homes are owner-only and cleanup is
restricted to SDK-created temp paths. The launched process still runs as the
current OS user and can spend those accounts' quota, so disable inheritance
when running untrusted application code (`inheritLogins: false`).

**Automate the user's own horon-agent** (`connect`). Attaches to the horon-agent already
running on the machine, sharing its live sessions. This is what an editor
plugin or a status dashboard wants. Anything it does is visible in the user's
terminal, and it needs a bridge already running (`horon api-bridge`).

## Quick start

Swap `launch` for `connect` to drive the user's own horon-agent instead of a private
instance; everything after that line is identical.

A complete runnable application is available in
[`examples/demo-app`](https://github.com/horon-ai/agent/tree/dev/sdk/typescript/examples/demo-app).

```ts
import { HoronAgentClient } from "@horon-ai/agent-sdk";

const client = await HoronAgentClient.launch({ workingDir: process.cwd() });

const session = await client.createSession(process.cwd());
const turn = await client.run(session.session_id, "What files are in src/?", {
  autoApprove: true,
  onEvent: (event) => {
    if (event.ev === "text_delta") process.stdout.write(event.text);
  },
});

console.log(
  "\ntools:",
  turn.toolCalls.map((call) => call.name),
);
console.log("tokens:", turn.usage);
client.close();
```

### Full system prompt override

```ts
const session = await client.createSession({
  workingDir: process.cwd(),
  systemPrompt: "You are a concise code reviewer.",
});
```

`systemPrompt` is sent as `system_prompt` and replaces the **entire assembled
system prompt**, not just its base text. Default instructions and assembled
instruction/context additions are not appended. The override is immutable after
session creation and is persisted by the runtime for resume. Omit it (or use
`undefined`) to keep normal prompt assembly. An empty string explicitly replaces
the system prompt with an empty prompt. Existing `createSession()` and
`createSession("/path")` calls retain their normal behavior.

### Controlling tools

Use `createSession({ workingDir, tools })` or `configureTools(sessionId, tools)`
to control the tools exposed to one session. This requires a runtime and API
bridge advertising `session_tools`. Existing `createSession("/path")` calls
continue to work unchanged.

```ts
const session = await client.createSession({
  workingDir: process.cwd(),
  tools: {
    enabled: ["read", "agentgrep"],
    disabled: ["bash"],
    custom: [
      {
        name: "lookup_ticket",
        description: "Look up a ticket in the application's ticket store",
        parameters: {
          type: "object",
          properties: { id: { type: "string" } },
          required: ["id"],
          additionalProperties: false,
        },
        execute: async (input, { signal }) => {
          // Your application implements this function. Honor signal for cancellation.
          return JSON.stringify(await lookupTicket(String(input.id), { signal }));
        },
      },
    ],
  },
});

console.log(await client.listTools(session.session_id));
const turn = await client.run(session.session_id, "Summarize ticket ABC-123");

// Replace the policy between turns. No tools at all:
await client.configureTools(session.session_id, { enabled: [] });
// Restore configured defaults and remove all custom callbacks:
await client.configureTools(session.session_id, {});
```

- `enabled` selects built-in/MCP tools. Omitted or `null` inherits configured
  defaults. An empty array exposes no built-in/MCP tools.
- `custom` adds tools regardless of `enabled`. A custom tool with the same name
  replaces that tool only in this session. `disabled` wins over both lists.
- Configuration replaces the previous SDK policy, not a patch. It is accepted
  only while the session is idle. Await it before starting another turn.
- `execute` receives schema-validated input and returns a string or
  `{ output: string, error?: string }`. Thrown errors become tool errors sent back
  to the model. The SDK never serializes the callback function.
- Callbacks have a 60-second deadline. Set `timeoutMs` on a custom tool to shorten
  it. Cancellation and disconnect abort the supplied signal. Callbacks must
  cooperate with that signal to stop external work, and synchronous blocking
  callbacks cannot be forcibly interrupted by JavaScript timers.
- Custom callbacks belong to the registering connection. Keep it open throughout
  the turn. Policies are in-memory, not saved in the transcript. Reconfigure
  before sending a message after reconnecting, reloading a session, restarting
  the daemon, or forking a session. These controls are not an OS sandbox:
  an enabled shell or application callback can still perform arbitrary work.
  Automatic recovery after a daemon restart uses the daemon's default policy,
  not the previous SDK selection. Do not treat these live-session controls as
  a persistent security boundary. Prefer a private ephemeral `HoronAgentClient.launch()`
  instance for embedding rather than a shared or automatically resumed session.
- If a configuration acknowledgement times out, the SDK closes its connection
  rather than risk executing old callbacks against an uncertain new policy.

For manual dispatch, send a wire-level `configure_tools` request via
`client.request`, consume `tool_call` events, and call
`client.submitToolResult(sessionId, callId, result)`. Do not manually answer calls
that already have an `execute` callback.

### Web search

The built-in `websearch` tool searches locally with the engines configured
under `[websearch]` in `config.toml`. It needs no SDK code:

- `enabled` and `disabled` govern it like any other tool.
- A `custom` tool named `websearch` replaces search entirely. It is called
  through your callback.

### Assistant messages and final answers

`turn.text` is the concatenation of **all** assistant text in the turn, including
intermediate narration before tools. This behavior is preserved for compatibility.
Use `turn.finalText` to forward only the last completed assistant message, or
`turn.messages` to retain each completed message separately:

```ts
const turn = await client.run(session.session_id, "Investigate the failure");
console.log(turn.finalText);
// turn.messages: [{ messageId?: string, text: string }, ...]
```

Framing-capable bridges attach `message_id` to `text_delta` and emit `text_done`
with the same id when that message ends. Reasoning may interleave within one
message and is **not** a text boundary. These ids correlate a live stream, not
persisted history entries, and should be scoped to the connection and session.
`text_replace` replaces the text for its `message_id`, including a previously
completed message. An empty replacement retracts discarded retry output. Streaming
clients should apply these corrections, and wait for `turn_done` before publishing
an irreversible final answer. The SDK applies them to `text`, `messages`, and
`finalText` automatically.
With older bridges, `messages` is empty and `finalText` falls back to whole-turn
`text`. Exact message boundaries cannot be reconstructed from that older stream.

To verify message framing and concurrent history reads against a real provider
in a private instance, run the opt-in acceptance check from the repository root:

```sh
HORON_AGENT_SDK_TEST_MODEL="your-model-id" node sdk/typescript/test/live-text-framing.mjs ./target/selfdev/horon-agent
```

This uses your existing provider login and quota, runs one harmless bash tool,
and cleans up its private instance. It does not restart the shared daemon.

## Structured output

`runStructured()` asks the model for JSON, validates the response with Ajv, and
sends bounded corrective retries when the response is not valid JSON or does not
match your JSON Schema. It returns the normal turn metadata plus validated
`data` and an `attempts` audit trail. On framing-capable bridges it validates
`finalText`, so intermediate narration does not contaminate the JSON answer.

```ts
const result = await client.runStructured<{ summary: string; count: number }>(
  session.session_id,
  "Summarize the current changes",
  {
    schema: {
      type: "object",
      additionalProperties: false,
      required: ["summary", "count"],
      properties: {
        summary: { type: "string" },
        count: { type: "integer", minimum: 0 },
      },
    },
    maxRetries: 2, // default
  },
);

console.log(result.data.summary);
```

If all attempts fail validation, the promise rejects with
`StructuredOutputError`. Its `validationErrors`, `lastText`, and `attempts`
fields are stable for logging or user-facing diagnostics.

## Streaming

`run()` is the batch convenience path. For live UIs, iterate events directly:

```ts
const session = await client.createSession();
await client.sendMessage(session.session_id, "hello");

for await (const event of client.events(session.session_id)) {
  switch (event.ev) {
    case "text_delta":
      process.stdout.write(event.text);
      break;
    case "tool_start":
      console.log("\n[tool]", event.name);
      break;
    case "permission_request":
      await client.respondToPermission(session.session_id, event.request_id, "allow");
      break;
    case "turn_done":
      return;
  }
}
```

Per-kind listeners work too: `client.on("token_usage", handler)`.

Protocol `error` frames arrive on the `harness_error` channel, not `error`.
Node treats an unlistened `error` event as a fatal throw, so the plain channel
is reserved for transport faults.

### All-session events

`globalEvents()` is the process-wide stream for dashboards and integrations. The
bridge attaches one session per connection, so the SDK discovers every persisted
session, opens one child connection for each, and fans their streams into one
bounded iterator. Discovery repeats to include sessions created later.

```ts
const stop = new AbortController();
for await (const event of client.globalEvents({ signal: stop.signal })) {
  if ("session_id" in event) console.log(event.session_id, event.ev);
}
```

Delivery is **at-least-from-attach**, not historical replay. Protocol v1 cannot
recover events emitted before a child attaches or during an unexpected
disconnect and reattach. Per-session order is preserved, but there is no total
ordering across sessions. `return()`, aborting the signal, or closing the parent
closes all children. The iterator fails with `event_buffer_overflow` rather than
silently dropping events if its bounded queue fills. A custom `Transport` is
rejected with `unsupported_transport` because it cannot be cloned safely into
independent child connections. Set `discoveryIntervalMs: 0` for one initial
discovery pass only.

## API surface

| Method | Purpose |
| --- | --- |
| `HoronAgentClient.launch(options)` | Start a private instance and connect to it |
| `HoronAgentClient.connect(options)` | Attach to the horon-agent already running on this machine |
| `listSessions({ includeArchived? })` | Every persisted session, optionally including archived sessions |
| `archiveSession(id)` / `restoreSession(id)` | Reversibly hide or restore a session |
| `setRetentionPolicy(days?)` | Auto-archive inactive sessions, or disable retention |
| `createSession(workingDirOrOptions?)` | Create and attach, optionally overriding the full system prompt |
| `attachSession(id)` / `detachSession(id)` | Subscribe / unsubscribe |
| `sendMessage(id, content, images?)` | Send a user message (awaits `message_accepted`) |
| `run(id, content, options?)` | Send and collect one full turn |
| `runStructured(id, content, options)` | Send, validate JSON Schema output, and retry corrections |
| `events(sessionId?)` | Async iterator over stream events |
| `globalEvents(options?)` | Bounded fan-in stream over all persisted and newly created sessions |
| `cancel(id)` / `softInterrupt(id, content, urgent?)` | Interrupt a turn |
| `getHistory(id)` / `peekSession(id, limit?)` | Read a transcript (peek works unattached) |
| `clear(id)` / `rewind(id, index)` | Edit history |
| `respondToPermission(id, requestId, decision)` | Answer a permission prompt |
| `listModels(id)` / `setModel(id, model)` | List and choose the session's model |
| `getRuntimeInfo(id)` | Provider, model route, protocol, capability, and health metadata |
| `setApiKey(provider, key)` / `clearApiKey(provider)` | Atomically provision or remove owner-only API-key files |
| `readFile(id, path, maxBytes?)` | Read bounded UTF-8 text under the session root |
| `findFiles(id, query, limit?)` | Find rooted files by path substring |
| `searchText(id, query, options?)` | Bounded rooted literal text search |
| `fileStatus(id, path)` | Read safe rooted file metadata |
| `setReasoningEffort(id, effort)` | Set the cost/quality dial |
| `compact(id)` | Schedule transcript compaction to free context |
| `renameSession(id, title?)` | Set a session title, or clear it |
| `rewindUndo(id)` | Restore what the last `rewind` removed |
| `cancelSoftInterrupts(id)` | Retract queued soft interrupts |
| `softInterruptWithId(id, content, { clientId, images?, urgent? })` | Queue a mid-turn message you can track and retract |
| `cancelSoftInterruptsById(id, clientIds)` / `cancelSoftInterrupt(id, clientId)` | Retract specific queued messages, with an exact "too late?" answer |
| `backgroundTool(id)` / `backgroundToolResult(id)` | Move the running tool call to the background, optionally reporting what moved |
| `listSessionHeads(id)` / `selectSessionHead(id, headEventId)` | Durable heads and the host-local selection (`session_event_store_v1`) |
| `getHistoryPage(id, { headEventId?, beforeEventId?, limit? })` | One bounded page of history on a fixed head (`session_history_cursor_v1`) |
| `exportSessionEvents(id, { heads?, knownIds?, cursor? })` / `importSessionEvents(page, retryCursor?)` | Exchange one bounded page of original events (`session_replication_v1`) |
| `exportSessionBlob(...)` / `importSessionBlob(chunk)` / `sessionBlobStatus(...)` | Move original blobs in verified 1 MiB chunks (`session_replication_v1`) |
| `exportSessionRecordBlob(...)` / `importSessionRecordBlob(chunk)` / `sessionRecordBlobStatus(...)` | Move a whole canonical record the page carried by reference |
| `ping()` | Liveness |

## Models

A client that cannot enumerate models cannot offer a picker, so the catalog is
first-class. It is served from the push the daemon sends on attach, meaning
opening a picker costs no round trip:

```ts
const { models, current } = await client.listModels(session.session_id);
await client.setModel(session.session_id, "claude-opus-5");
```

An unknown model, or one the provider refuses, rejects with `invalid_request`
rather than silently leaving the session where it was. When the model changes,
every client attached to that session receives a `model_info` event, so a UI
that did not make the change still updates.

`setReasoningEffort(id, effort)` sets how much the model deliberates. The
accepted values are per-provider (typically `minimal` through `max`), so this
takes a string and reports what the provider says instead of guessing at a
union that would go stale.

`getRuntimeInfo(id)` adds the active provider/model, every available model route,
the negotiated protocol version, advertised capability strings, and a live ping.
API-key provisioning accepts the supported provider aliases, normalizes Gemini
aliases to `gemini`, supports the horon-agent subscription key, writes owner-only files
atomically, and asks the daemon to reload credentials. OAuth tokens are not part
of this API.

File methods are rooted at the persisted session working directory. Absolute
paths, `..`, and symlink escapes are rejected. Directory walks do not follow
symlinks and both file count and byte scanning are bounded. `readFile()` accepts
UTF-8 text only and reports when its byte limit truncated the result.

## Session archive and retention

Archiving never deletes a transcript. It removes the session from the default
`listSessions()` result and records an archive timestamp in owner-only state.
Pass `includeArchived: true` to display and restore archived sessions.
`setRetentionPolicy(days)` applies the same reversible archive operation to
inactive persisted sessions when sessions are listed. Omit `days` to disable
automatic retention.

## Session event store, heads, and replication

The daemon keeps each session as an append-only event log. Four capabilities gate
the surface that reads and exchanges it; check them with `client.supports(...)`
before calling, because a missing capability answers `unsupported` naming it:

| capability | gates |
| --- | --- |
| `session_event_store_v1` | `listSessionHeads`, `selectSessionHead` |
| `session_replication_v1` | `exportSessionEvents`, `importSessionEvents`, the blob methods, and the whole-record blob pair |
| `session_history_cursor_v1` | `getHistoryPage` |
| `structured_compaction_v1` | nothing in this SDK; read it with `supports()` before interpreting a `compacted` record's structured summary fields |

A session may retain several heads after a rewind or a divergent import. The
selected head is host-local: it is never replicated. When `selected_head` is
absent, execution is refused until the user picks one — surface the choice
rather than picking silently, because it decides what the next turn continues
from. `selectSessionHead` answers `busy` while another process owns the writer.

`getHistory` is unchanged and still returns the whole visible transcript.
`getHistoryPage` is the bounded read for a long session:

```ts
let page = await client.getHistoryPage(id, { limit: 50 });
const head = page.head;                       // pin it while the user scrolls
render(page.messages);                        // oldest first inside the page
while (page.next_cursor) {
  page = await client.getHistoryPage(id, { headEventId: head, beforeEventId: page.next_cursor, limit: 50 });
  prepend(page.messages);
}
```

Two instances exchange originals only — never checkpoints, indices, PIDs, or the
local head selection:

```ts
const page = await client.exportSessionEvents(id);
const result = await client.importSessionEvents(page as SessionEventPage);
// result.accepted_ids: durable successes, including verified retransmissions
// result.retry_cursor: send it back after a partial import
// result.conflict_heads: diverged heads, all of them kept
```

Resending an import is idempotent. `canonical_json` is opaque text and must be
passed through untouched: a `u64` above 2^53, `1.0`, and `-0.0` do not survive a
JavaScript number round trip, and re-serializing silently changes the event's
identity. Blobs are verified by SHA-256 and size before publication; an
interrupted transfer resumes from `sessionBlobStatus`.

Storage format, limits, and legacy migration: `docs/protocol/SESSION_STORE.md`.

## Long sessions

`compact(id)` summarizes the transcript so far, freeing context. It is
asynchronous: the daemon summarizes at the next safe point rather than
interrupting a turn, so it resolving means the request was accepted, not that
the transcript has already shrunk. Read the history afterwards for the result.

It is refused below about 10% context usage, on the grounds that there is
nothing worth compacting yet, and the rejection carries the current usage. So
treat `invalid_request` here as information for the user rather than an error
to retry.

Which engine owns compaction is `strategy` under `[compaction]`: `structured` (the
default), `openai_auto`, or `openai_explicit`. A config that only writes the
older `[provider] openai_native_compaction_mode` still selects a native strategy
for `auto`/`explicit`; an absent key, an empty value, or `off` means structured.
The structured path records `compacted` events whose payload names the covered
original range (`covered_through_event_id`, `source_prefix_digest`,
`first_kept_event_id`), so a summary can never be applied to a range it did not
actually read. A compaction that cannot proceed appends a `compaction_blocked`
record with a typed reason instead of reporting success.

```ts
await client.renameSession(id, "nightly refactor"); // omit the title to clear it
await client.rewind(id, 4);
await client.rewindUndo(id); // rewind is reversible
await client.cancelSoftInterrupts(id); // retract what is queued
```

## Messages sent mid-turn

A message sent while the agent is working joins the running turn at the next
safe point. Give it a `clientId` to follow it from there (needs the
`soft_interrupt_ids` capability):

```ts
await client.softInterruptWithId(id, "also update the changelog", { clientId: "m-42" });

client.on("event", (frame) => {
  if (frame.ev === "soft_interrupt_injected" && frame.client_ids.includes("m-42")) {
    // The model has it now. Everything it writes from here on was written
    // having seen the message, so the next text answers it.
  }
  if (frame.ev === "turn_done" && frame.pending_soft_interrupts) {
    // Messages still queued when the turn ended. They were never seen.
  }
});

// Take it back while it is still queued. `false` means it was already delivered.
const retracted = await client.cancelSoftInterrupt(id, "m-42");
```

`soft_interrupt_injected` arrives after the last `tool_done` of the batch and
before the model's first output that sees the message. Several queued messages
are delivered together as one user message, so `client_ids` can list more than
one id. Retraction is decided against delivery atomically: an id is never
reported both cancelled and delivered. `session_status` after attaching carries
`pending_soft_interrupts` so a reconnecting client can reconcile its own queue.

`backgroundToolResult(id)` moves the running tool to the background so a
queued message can be read sooner, and reports `{ moved, toolCallId, toolName }`.

## Instance lifecycle

A launched instance owns a daemon and a state directory, and both are cleaned
up for you:

- `close()` stops the daemon and removes an ephemeral home. It waits for the
  process to actually be gone, so the directory cannot be recreated behind the
  delete. Expect it to take a few seconds.
- If your process exits without calling `close()`, including after an uncaught
  exception, the instance is still reaped. Without this a server that restarts
  would accumulate one daemon and one temp directory per restart.
- `SIGKILL` is the one case nothing can cover, since no handler runs.

### `launch()` options

| Option             | Effect                                                                                                                                                          |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `workingDir`       | Working directory for sessions. Defaults to `process.cwd()`.                                                                                                    |
| `horonAgentHome`   | Keep state at a fixed path across runs. Defaults to a temporary directory that is removed on `close()`. See the note below.                                     |
| `inheritLogins`    | Inherit the user's provider logins. Defaults to `true`.                                                                                                         |
| `binary`           | Path to the horon-agent binary. Defaults to `horon-agent` on `PATH`.                                                                                            |
| `env`              | Extra environment variables for the instance.                                                                                                                   |
| `swarmModel`       | Operator-enforced model for all swarm workers. Use `inherit` to keep the coordinator model and auth route. Takes precedence over `env.HORON_AGENT_SWARM_MODEL`. |
| `wakeMode`         | `internal` (daemon-owned wakes) or `external` (emit `wake_requested` for the operator). Takes precedence over `env.HORON_AGENT_WAKE_MODE`.                      |
| `startupTimeoutMs` | How long to wait for the instance to come up. Defaults to 30000.                                                                                                |
| `cleanupTimeoutMs` | How long `close()` spends removing an ephemeral home. Defaults to 30000.                                                                                        |
| `inheritStderr`    | Forward the instance's stderr to your process. Defaults to `false`.                                                                                             |

A fixed `horonAgentHome` persists transcripts on disk. `listSessions()` discovers
those records even on a fresh, unattached connection, so a restarted process can
rebuild its complete session index without keeping a separate id registry.

## Configuration

| Env var                   | Effect                                                        |
| ------------------------- | ------------------------------------------------------------- |
| `HORON_AGENT_API_SOCKET`  | Override the API socket path                                  |
| `HORON_AGENT_WAKE_MODE`   | Autonomous wake ownership: `internal` (default) or `external` |
| `HORON_AGENT_RUNTIME_DIR` | Override the runtime directory                                |
| `XDG_RUNTIME_DIR`         | Default runtime directory on Linux                            |

Or pass `socketPath` to `connect()`.

## Errors

SDK and protocol failures reject with a `HarnessError`. Its stable `code` is
the value to branch on; the message is diagnostic text and can change. Normal
JavaScript errors (for example, an OS filesystem error) can still surface from
the platform.

```ts
import { HarnessError, StructuredOutputError } from "@horon-ai/agent-sdk";

try {
  await client.run(sessionId, prompt);
} catch (error) {
  if (error instanceof StructuredOutputError) {
    console.error(error.validationErrors, error.lastText, error.attempts);
  } else if (error instanceof HarnessError) {
    switch (error.code) {
      case "unknown_session":
        // Refresh listSessions(), then ask the user to choose another session.
        break;
      case "disconnected":
      case "timeout":
        // Reconnect and retry only if the operation is safe to repeat.
        break;
      default:
        console.error(error.code, error.message);
    }
  } else {
    throw error;
  }
}
```

### Launch and connection errors

| Code                    | Cause                                                                                                                     | Recovery                                                                                                             |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `horon_agent_not_found` | `launch()` could not execute horon-agent.                                                                                 | Install horon-agent, put it on `PATH`, or pass `binary` with an absolute path.                                       |
| `startup_failed`        | The private instance exited before opening its API socket. Its stderr is included in the message.                         | Display/log the message; fix the reported configuration, credential, or binary error before retrying.                |
| `startup_timeout`       | The private instance did not open its API socket within `startupTimeoutMs`.                                               | Increase the timeout on a slow machine; otherwise inspect stderr and ensure the runtime directory is writable.       |
| `invalid_instance_home` | `horonAgentHome`, its credential paths, or the source login home is unsafe (same directory, symlink, file, or traversal). | Choose a separate real directory. Do not point a private instance at the user's live horon-agent home.               |
| `connect_failed`        | The bridge is absent, dead, or listening at another socket path.                                                          | Run `horon api-bridge`; verify `socketPath` or `HORON_AGENT_API_SOCKET`. The message names the attempted path. |
| `handshake_failed`      | The peer replied with an invalid frame during protocol negotiation.                                                       | Confirm the socket is a horon-agent harness socket and upgrade horon-agent/SDK together.                             |
| `unsupported_version`   | Client and bridge do not share a protocol major version.                                                                  | Upgrade the older side. Do not retry unchanged versions.                                                             |

### Request and transport errors

| Code | Cause | Recovery |
| --- | --- | --- |
| `disconnected` | The socket closed or a write failed while work was in flight. | Reconnect. Retry only idempotent reads, or first verify whether a mutating request took effect. |
| `timeout` | No correlated reply arrived within `requestTimeoutMs` (30 seconds by default). | Check daemon health and raise the timeout for legitimately slow requests. Treat outcome as unknown before repeating mutations. |
| `unexpected_reply` | A reply was valid protocol data but not the event kind required by that SDK method. | Upgrade both sides and report the server/client versions with the error. |
| `unknown_request` | The bridge does not implement that request tag. | Upgrade horon-agent, or stop using that newer SDK method with this bridge. |
| `unknown_session` | The session no longer exists, is not available to this instance, or the connection is not attached where attachment is required. | Refresh `listSessions()`, use the right private/shared instance, and attach when the method requires it. |
| `invalid_request` | Arguments or current state violate the operation's contract (for example an invalid model, retry count, path, or compaction request). | Correct the caller input. The message contains the rejected constraint; do not blindly retry. |
| `invalid_option` | A client-only option is outside its allowed range. | Correct the named option, such as `discoveryIntervalMs` or `maxBufferedEvents`. |
| `unsupported` | The runtime does not advertise the capability that method needs (for example `session_tools`, or one of the four session-store capabilities). | Update both the horon-agent daemon and API bridge before using that surface. |
| `busy` | Another tool configuration request is in flight for this session. | Await the previous configuration before submitting another. |
| `too_many_requests` | `maxPendingRequests` requests are already awaiting a reply, so the new request was refused instead of growing the pending map. | Await replies, cancel work, or raise `maxPendingRequests` deliberately. The request never reached the wire, so repeating it is safe. |
| `transport_backpressure` | The transport reported a full write buffer but provides no `onDrain` signal, so the client cannot honour its backpressure. | Give the transport an `onDrain` listener, or return `void` from `write` if it never blocks. |
| `internal` | The bridge or daemon failed unexpectedly while handling a valid request. | Preserve the message and horon-agent logs, retry once if safe, then report it if reproducible. |

### Streaming and structured-output errors

| Code                        | Cause                                                                                                            | Recovery                                                                                                                                                  |
| --------------------------- | ---------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `unsupported_transport`     | `globalEvents()` was called on a custom transport, which cannot be cloned into per-session connections.          | Use a native socket client, or consume individual `events()` streams yourself.                                                                            |
| `event_buffer_overflow`     | A `globalEvents()` consumer fell behind `maxBufferedEvents`; the SDK fails rather than dropping events silently. | Consume faster, reduce work in the loop, or deliberately increase the bounded buffer. Recreate the iterator afterward.                                    |
| `concurrent_next`           | Two callers invoked `next()` concurrently on the same global event iterator.                                     | Give the iterator one consumer and fan out events inside the application.                                                                                 |
| `structured_schema_invalid` | The JSON Schema passed to `runStructured()` is invalid.                                                          | Fix the schema; this is deterministic and should not be retried unchanged.                                                                                |
| `structured_output_invalid` | The model exhausted the structured-output retry budget without producing schema-valid JSON.                      | Catch `StructuredOutputError` and inspect `validationErrors`, `lastText`, and `attempts`; revise the prompt/schema or increase `maxRetries` deliberately. |

Protocol error frames that are not replies to a pending request are emitted on
`harness_error`. Transport failures are emitted on `error` and also close the
client. Always register an `error` listener when using EventEmitter-style
listeners because Node treats an unhandled `error` event as fatal.

Unknown future server codes remain accessible as `HarnessError.code`; keep a
default branch and show the diagnostic message instead of assuming this table
is exhaustive forever.

## Stability

This package is generally available and follows semver against the protocol
it speaks.

- **Protocol v1 is stable.** The handshake negotiates a major version, and a
  server that cannot speak v1 is rejected with `unsupported_version` rather
  than half-working. A breaking protocol change bumps to v2 and to a new SDK
  major.
- **Additive changes are minor releases.** New events, new request fields, and
  new methods arrive in minors. Existing frames keep their shape.
- **The API covers what real clients need.** A test diffs the API against every
  request the terminal app makes and fails on an untriaged gap, so the surface
  cannot quietly fall behind the app it mirrors.
- **Drift is checked mechanically, in both directions.** A Rust test reads
  `src/protocol.ts` and fails if a variant _or a field_ is missing here; a Node
  test reads the Rust enums and fails if the tag sets diverge. Neither side can
  land a schema change alone.
- **The tarball is tested as a tarball.** `scripts/sdk/test_package.sh` packs
  it, installs it into a throwaway project, and imports it as ESM, as CJS, and
  through `tsc`.

Compatibility: Node 20+, ESM and CJS. Linux and macOS are covered end to end in
CI; Windows builds and is wired up but is not yet exercised live.

## Forward compatibility

The harness may add events at any time within protocol v1. `events()` and
`run()` are typed as `ApiEvent`, the union of kinds this SDK knows, so
`switch (event.ev)` narrows each case; always keep a `default` branch for kinds
added after your version. A frame of unknown kind is delivered as
`UnknownApiEvent` (`{ ev: string; [key: string]: unknown }`); use
`isKnownEvent(frame)` to narrow `AnyApiEvent` when you handle raw frames.

`UnknownApiEvent` is deliberately not a member of `ApiEvent`: a member with
`ev: string` widens the discriminant, and TypeScript then refuses to narrow any
case, typing every field as `unknown`.

## Releasing

See [RELEASING.md](RELEASING.md). `bash scripts/sdk/publish_preflight.sh` runs
every gate and reports what is left.

## Development

```bash
npm run check   # typecheck + build + tests (mock harness, no daemon needed)
```

`test/schema-parity.test.ts` reads the Rust enums directly, and
`crates/harness-api`'s `typescript_sdk_lists_every_variant` test reads
this package. Adding a variant on either side without the other fails CI.
