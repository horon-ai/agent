# Turns

Language: **English** | [한국어](TURNS.ko.md)

This covers turn state and recovery, message injection during a running turn, structured runtime commands, and `--resume` behaviour.

## Turn recovery and turn state

This document explains what the agent does when a turn ends unexpectedly and who holds the material for that decision. The code is in `crates/agent-runtime/src/turn_recovery.rs` and `crates/agent-runtime/src/turn_state.rs`.

### TurnState holds the material

`TurnState` owns a turn's counters and outcome outside the agent loop. It has twelve fields (`crates/agent-runtime/src/turn_state.rs:83-96`).

| Field | What it counts |
| --- | --- |
| `context_limit_retries` | Retries after a context limit |
| `rate_limit_retries` | Retries after a rate limit |
| `incomplete_continuations` | Continuations after an incomplete response |
| `empty_post_tool_continuations` | Continuations after an empty response following tool results |
| `fable_guardrail_reconsiderations` | Guardrail reconsiderations |
| `sequential_single_tool_rounds` | Rounds that called tools one at a time |
| `batch_nudge_pending` | Whether a batch nudge is pending |
| `consecutive_malformed_tool_rounds` | Consecutive malformed tool rounds |
| `unparsed_tool_markup_nudges` | Nudges about unparsed tool markup |
| `auto_continuations` | Auto continuations used |
| `final_text` | The turn's final text |
| `outcome` | The turn outcome (`TurnOutcome`) |

`TurnState` holds no reference to `Agent` and touches no session, client event, or disk. That is why both turn loops (the sequential loop in `crates/app-core/src/agent/turn_loops.rs` and the streaming loop in `crates/app-core/src/agent/interrupts.rs`) share the same decision functions.

### Four classes of unexpected stop

`classify(stop_reason, final_text)` looks only at the stop reason and the final text and picks one of four classes (`turn_recovery.rs:52`).

| Class | Meaning |
| --- | --- |
| `CleanStop` | The turn ended normally. |
| `BudgetTruncated` | The provider reported stopping on a length or token limit. |
| `UnclosedStructure` | The final text left a code fence or list item open. |
| `SemanticPrematureStop` | The last sentence announced what would happen next, and nothing did. |

### Three recovery modes

`RecoveryMode` decides which classes are actually acted on (`turn_recovery.rs:7-15`, `:40-49`).

| Mode | Classes acted on |
| --- | --- |
| `off` | None |
| `conservative` (default) | `BudgetTruncated`, `UnclosedStructure` |
| `aggressive` | Every non-clean class, including the semantic guess |

The default is `conservative`. Only provable truncation and unclosed structure are continued; the semantic guess is used only when the operator chooses `aggressive`.

### At most two auto continuations per turn

Synthetic continuations are capped by `MAX_AUTO_CONTINUATIONS = 2` (`crates/agent-runtime/src/turn_recovery.rs:31`). `TurnState` never exceeds that value (`crates/agent-runtime/src/turn_state.rs:215-219`; `record_auto_continuation` clamps at the ceiling), and a new turn starts at 0 with `TurnState::new()`, so the budget resets (`crates/agent-runtime/src/turn_state.rs:82-100`, test `crates/agent-runtime/src/turn_state_tests.rs:18-26`). Once the limit is used up, recovery stops and the user is told (`crates/app-core/src/agent/turn_recovery_wiring.rs:136`).

### Soft interrupts survive a restart

A message that arrives during a turn is kept in a per-session store and restored when the session is loaded again (`crates/base/src/soft_interrupt_store.rs`). The restore point is `crates/app-core/src/agent/turn_execution.rs:1170`, and the path saved when a session closes is pinned by the test in `crates/app-core/src/agent_tests.rs:2016`. The injection mechanism itself is described in [`docs/runtime/TURNS.md`](TURNS.md).

### How to verify

```bash
cargo test -p horon-agent-agent-runtime turn_recovery
cargo test -p horon-agent-agent-runtime turn_state
cargo test -p horon-agent-base soft_interrupt_store
```

## Soft interrupts: seamless message injection

### Overview

Soft interrupts are implemented in this build. Adding a message to a running turn does not cancel the current generation. Instead of breaking the flow and restarting, the message is queued and attached to the conversation history at a safe point after the agent loop finishes executing tools. The implementation has three pieces: two protocol requests (`crates/protocol/src/wire.rs:175-197`), the per-session queue (`crates/agent-runtime/src/lib.rs:18-175`), and the injection points in the turn loop (`crates/app-core/src/agent/interrupts.rs:536-669`).

### Difference from a hard interrupt

| Item | Hard interrupt (`cancel`) | Soft interrupt |
|--------|-------------------------|---------------------|
| Cancels the generation | Yes | No |
| Tool results already executed | Discarded | Kept |
| Injection timing | A new request after cancellation finishes | The next safe point |
| Queue persistence | None | Written to a per-session file |

Hard interrupts are still needed. When the user wants an immediate stop, `cancel` is the tool. Soft interrupts are for "do not stop, take this into account".

### The queue and the in-flight set

`SoftInterruptQueue` manages the `pending` and `in_flight` vectors under one lock (`crates/agent-runtime/src/lib.rs:40-63`). `pending` holds messages that have not entered the conversation, and `in_flight` holds messages injected this turn that the turn has not finished.

- `transfer_to_in_flight` moves `pending` into `in_flight` as a whole. Both vectors change under the same lock, so no message can end up in neither (`crates/agent-runtime/src/lib.rs:113-120`).
- `commit` clears `in_flight` when the turn succeeds (`:124-131`).
- `restore` returns the messages of a failed, cancelled, or panicked turn to the front of `pending` (`:136-148`).

Each turn creates one `InFlightGuard` (`crates/app-core/src/agent/interrupts.rs:21-70`). On a normal exit or a restore path, `disarm` makes the drop a no-op. When a provider or tool layer panics, an armed guard drops, calls `restore`, and the injected messages return to the queue instead of disappearing (`:58-70`).

### Injection points

There are three injection points. The event carries the point's name as is (`crates/protocol/src/wire/events.rs:266-284`).

| Point | Location | Timing | Purpose |
|-------|----------|--------|----------|
| **B** | Turn completion | No tool request | Default: no tools remain, so ordering cannot break |
| **C** | Mid tool round | Urgent stop | Results are filled for skipped calls first, then injection happens |
| **D** | After all tools execute | Before the next provider call | Default: every pending interrupt is flushed here |

The point decisions are in `crates/app-core/src/agent/interrupts.rs:695` (B) and `:706` (D); C opens only when an urgent interrupt is pending. The streaming path uses the same points at `crates/app-core/src/agent/turn_streaming_mpsc.rs:1504` (B), `:1636` (D), and `:2231` (C).

#### Urgent handling at point C

When a message with the urgent flag set is pending, the tool round is not run to completion. The only decision function is `urgent_interrupt_pending` (`crates/app-core/src/agent/interrupts.rs:135-140`). Cutting a round first inserts a tool result with `is_error: true` for every call that has no result yet (`crates/app-core/src/agent/turn_streaming_mpsc.rs:2205-2227`). The content is `[Skipped: user interrupted]` (`:12`). The user message follows, and the event reports the skipped count as `tools_skipped`.

Non-urgent interrupts are never inserted between tools. All of them wait for point D (`crates/app-core/src/agent/turn_streaming_mpsc.rs:2104-2106`).

### Pairing tool calls with results

Model calls in this build go out over a single OpenAI-compatible Responses path. That request assembler keeps calls and results in order itself. It first scans which message holds each tool result (`crates/provider-openai/src/request.rs:184-193`), defers output while the result is still ahead to preserve call order (`:390-420`), and inserts a synthetic `[Error] ...` output for a call that never gets a result (`:436-441`). A result that attaches to no call is emitted as a user message (`:449-464`).

So the injection rules cannot create an ordering violation. Point D comes after every result is already in history, and point C comes after the skipped calls have been filled.

### API surface

Clients send two requests.

```rust
// crates/protocol/src/wire.rs
SoftInterrupt {
    id: u64,
    content: String,
    images: Vec<(String, String)>,
    urgent: bool,
    client_id: Option<String>,
}

CancelSoftInterrupts {
    id: u64,
    client_ids: Option<Vec<String>>,
}
```

`client_id` is echoed in the injection event and is used to target a filtered cancel request (`crates/protocol/src/wire.rs:175-197`).

The server emits three events.

- `soft_interrupt_injected`: the injected content, display role, point, skipped tool count, and merged caller ids (`crates/protocol/src/wire/events.rs:266-284`).
- `soft_interrupts_cancelled`: reports cancelled ids and ids that were not in the queue separately (`:288-294`).
- `soft_interrupt_queue`: how many messages remain in the queue when the turn ends. It arrives immediately before the turn's termination event (`:309-312`).

### Persistence and restore

The queue is kept in a per-session file at `<agent data directory>/pending-soft-interrupts/<session-id>.json` (`crates/base/src/soft_interrupt_store.rs:69-75`).

- When the server queues a message it also appends it to disk (`crates/app-core/src/server/state.rs:1288-1298`).
- A cancel request removes only the entries matching `client_ids` from disk (`crates/app-core/src/server/client_lifecycle.rs:3821`). A full cancel empties the file (`:3851`).
- The agent rewrites the snapshot on every state change (`crates/app-core/src/agent.rs:1369`, `:1401` → `crates/app-core/src/agent/interrupts.rs:181-200`).

Restoring a session reads the on-disk queue into memory (`crates/app-core/src/agent/turn_execution.rs:1170` → `crates/app-core/src/agent/interrupts.rs:143-175`). That read is not destructive. The disk copy is not removed until the first turn commits successfully, so messages are injected exactly once across restarts and retries.

### Cancelled turns

Nothing is injected when the user cancels a turn. `inject_soft_interrupts` returns an empty list when a normal-termination signal is set and leaves the messages in the queue (`crates/app-core/src/agent/interrupts.rs:536-554`). That keeps the next prompt the user types from appearing in history as an unanswered message, and lets the client send it as the next turn.

### Relationship to stream rule injection

With `[features] stream_rules` on, a rule violation detected mid-stream injects a message over the same event channel. That path uses the point name `S` (`crates/app-core/src/agent/stream_rules.rs:42`, `crates/app-core/src/agent/turn_streaming_mpsc.rs:738-780`). It is separate from the user input queue, and when the switch is off the matcher is never created (`crates/app-core/src/agent/stream_rules.rs:83-90`).

### User experience

This build has no TUI. The feedback a client can use is three protocol events.

In the default mode (`urgent = false`), a client that sends `soft_interrupt` makes the daemon queue the message. On injection, `soft_interrupt_injected` arrives with the point name (`crates/protocol/src/wire/events.rs:266-284`). When the turn ends with messages still queued, `soft_interrupt_queue` reports how many remain, and that event comes immediately before the termination event (`crates/app-core/src/server/client_lifecycle.rs:3554-3561`). With zero remaining, no event is sent.

Messages are grouped by origin and joined with `\n\n`. Consecutive messages from the same origin enter in one injection, and a change of origin breaks the group at that point (`crates/app-core/src/agent/interrupts.rs:596-640`).

Urgent mode (`urgent = true`) means the remaining tools may be skipped at point C (`crates/protocol/src/wire.rs:183`). Skipped calls get `is_error` results first, and the event carries their count in `tools_skipped`. That handling is in `crates/app-core/src/agent/turn_streaming_mpsc.rs:2205-2227`.

### Edge cases

1. **Several soft interrupts**: group them by origin and join with `\n\n` into one message.
2. **A soft interrupt while only text is streaming**: inject at point B and continue the loop.
3. **The turn was cancelled**: do not inject and leave the messages queued. The client sends them as the next turn (`crates/app-core/src/agent/interrupts.rs:536-554`).
4. **The turn failed or panicked**: `InFlightGuard` returns the injected messages to the front of the queue (`crates/app-core/src/agent/interrupts.rs:58-70`).
5. **The queue lock was poisoned**: skip the injection and log a warning (`crates/app-core/src/agent/interrupts.rs:556-565`).

### Tests

- `crates/agent-runtime/src/queue_tests.rs`: queue movement, commit, restore.
- `crates/app-core/src/agent/soft_interrupt_commit_tests.rs`: on-disk state before and after commit, retries, cancellation, panic restore.
- `crates/app-core/src/agent/turn_streaming_mpsc_interrupt_tests.rs`: filling results for skipped calls on an urgent stop, and the event.
- `crates/app-core/src/server/queue_tests.rs`: keeping the server queue and the disk copy in sync.

## Structured runtime commands

The harness API advertises this feature only when the connected daemon's safety-feature probe reports `runtime_commands`.
Older daemons return an explicit unsupported error.
This is an additive API (the current minor version is 15), available from both the Rust and TypeScript SDKs
(`crates/harness-api/src/lib.rs:53-55`).

```rust,no_run
use horon_agent_sdk::{HoronAgentClient, RuntimeCommand};
# fn example(client: &HoronAgentClient, session: &str) -> horon_agent_sdk::Result<()> {
let result = client.runtime_command(session, RuntimeCommand::Memory, vec!["status".into()])?;
println!("{}", result.message);
# Ok(()) }
```

```json
{"v":1,"id":7,"req":"runtime_command","session_id":"session-id","command":"memory","args":["status"]}
{"v":1,"reply_to":7,"ev":"runtime_command_result","session_id":"session-id","command":"memory","data":{"enabled":true},"message":"..."}
```

Rust events use the `RuntimeCommandResult { session_id, command, data, message }`
fields directly. The Rust SDK returns `RuntimeCommandOutput`. TypeScript exposes
`runtimeCommand(sessionId, command, args)` with the same fields. `data` is
command-specific and additive. `message` is suitable for display. Commands are
never turned into an ordinary agent prompt. State is read and changed on the
session's runtime host, including SSH and managed remote runtimes.

### Available actions

- `skills`: loaded global and project overlay skills, descriptions, paths, active
  skills, shared approval recommendations. Refreshes the global registry.
- `context`: the assembled static and dynamic system prompt, effective and current tools,
  active conversation messages, context window metadata. It consumes no pending memory,
  prewarms no provider, and runs no inference.
- `info`: the real provider and session identity plus token and cache counters.
  Cache expiry and content inspection are unavailable and say so.
- `memory`: toggle, explicit on/off/status, or schedule an `extract`. An extraction
  response means scheduled, not finished.
- `swarm`: status/on/off/toggle through native membership management.
- `agents [global] [swarm|review|judge|memory|ambient] [model|inherit]`: read defaults and
  session overrides, or keep the requested policy scope.
- `subagent_model`: view, set, or inherit the spawned worker policy.
- `autoreview`, `autojudge`: show status and keep explicit on/off/toggle.
- `plan`: read and change plan mode state. No argument or `status` returns the current
  state, `on`/`off` turn plan mode on and off, and `approve`/`reject` approve or reject the
  plan pending in the session. When configuration has plan mode off, `on` and `approve`
  answer with an error (`crates/app-core/src/plan_mode.rs`).
- `transport`: only the name remains, and there is nothing to switch. On the `Provider`
  trait `transport` is `None`, `set_transport` returns the error "This provider does not
  support transport switching", and `available_transports` defaults to an empty array
  (`crates/provider-core/src/lib.rs:437-447`). Implementations in this tree use those
  defaults (`crates/base/src/provider/mod.rs:1128-1147`). So a call with no argument returns
  `{"transport": null, "provider": ...}`, and passing `auto`, `https`, or `websocket`
  raises the error above (`crates/app-core/src/server/runtime_commands.rs:200-210`).
  `openai_transport` in the configuration file and the environment only stores a value,
  and no provider reads it (`crates/config-types/src/lib.rs:1341`,
  `crates/base/src/config/env_overrides.rs:476`).
- `fast`: the no-argument command toggles priority and off, and explicit on/off/flex/status
  are supported. Providers that cannot apply it return the real error.
- `refresh_model_list`: a real provider catalog refresh.
- `continue`, `resume_all`: native continuation of interrupted live sessions. Busy,
  completed, and new sessions are untouched.
- `review`, `judge`, `subagent`: real independent swarm worker runs using the session or
  global model policy. Returns structured spawned-session metadata. Supports `--model` and
  `--type`. The removed direct subagent tool stays unregistered. `--continue` is not
  available here and reports an explicit error.
- `overnight`: native supervised child runs plus session-scoped status/log/review/cancel
  control. `productivity`: real reports and image payloads from the runtime host.
  `initiatives`: an honest removed-feature explanation. It is not coming back.
- `remote`: native gateway status/on/off/pair/revoke/help. A configuration toggle does not
  pretend the listener changed instantly; it reports that a restart is required.
- `server_reload`: requests a non-forcing, state-preserving upgrade when a newer binary
  exists. The first response is acceptance of the request, not completion.
- `auth`, `logout`: runtime host authentication inspection and explicitly confirmed
  removal of provider credentials. Supported shared credential stores are the target.

The names above are set by the runtime command enum
(`crates/harness-api/src/runtime_commands.rs:7-33`).
Sending a name that is not in the enum answers with an error.
`cache`, `cache stats`, `cache extend`, `cache 1h`, and `cache 5m` are not in the enum.
This build has no runtime command that extends cache TTL and no client request that changes
premium mode.

`transfer_session` is a separate lifecycle operation, exposed as
`transferSession` / `transfer_session`. It creates a real runtime compaction child and
answers `SessionForked`. It does not attach the caller to the child.

### Limits and deployment

Most agent-backed reads and changes return a busy error rather than waiting behind an
arbitrarily long generation during an active model turn. Provider-specific settings return
an error when unsupported. Cloud UI settings, cache eviction and prewarming, and removed
initiative tracking are not falsely advertised as working. There is also no runtime command
that extends cache TTL. Provider-reported caching shows up only in `info`'s counters.

A new SDK or bridge does not upgrade a running daemon. Build the runtime and the bridge,
install the new runtime candidate, then use the existing graceful server upgrade workflow.
It preserves session state and does not kill the live daemon or the harness bridge. The
feature stays unusable until both sides run the new code.

## Model-requested context rollover

`context_notes` gives the model a small notes file. `new_context` lets it decide
that those notes are enough and start the next turn from a clean active context.
The tool is registered only while `[tools] context_notes` is on
(`crates/app-core/src/tool/mod.rs:1095`), because the rollover keeps exactly the
notes the dynamic system prompt carries.

The call itself only records a pending request keyed by session
(`crates/app-core/src/tool/new_context.rs`), so two calls in one turn queue one
rollover. A tool round cannot rebuild its own context: the assistant message that
called `new_context` and the results still to come belong to the same turn, and
moving the boundary there would orphan them.

Both turn loops apply the request once the turn's transcript is final, from the
turn settle step they share
(`crates/app-core/src/agent/interrupts.rs:364`), through
`Agent::apply_pending_context_rollover`
(`crates/app-core/src/agent/compaction.rs:49`). That records a compaction
boundary over every message before the newest user turn. No summary is generated:
the boundary carries a one-line marker (`compaction.rs:9`), with a text summary
the session already had left in front of it. A provider-native window is dropped
instead, because advancing the boundary past it would replay a payload the server
built for the earlier coverage. The next request is then the dynamic system
prompt (with the working notes), that user message, and the marker naming
`history://current`. Nothing is deleted: `history://current` still serves the full
transcript, including the tool results the rollover dropped.

The boundary never lands on a tool-result message (`compaction.rs:17`). The call
that produced a result sits before the boundary, so a boundary on the result
would replay as an orphan pair and the session projection would reject the
transcript.

The tool declares a write effect, so plan mode refuses it like any other
mutation and approval treats it as one.

Tests: `crates/app-core/src/agent/context_rollover_tests.rs` covers the rebuilt
request, the stored transcript, and one rollover per request;
`crates/app-core/src/tool/new_context_tests.rs` covers the plan-mode refusal, the
registration gate, and one pending request per turn.

## Handoff compaction

A handoff compaction writes a generated handoff document as the boundary instead
of a model summary. The document describes what the session was doing, what it
decided and what is left, so a successor session can continue from it
(`crates/app-core/src/agent/handoff_compaction.rs`).

### Trigger

Three callers can ask for it. The harness API takes the existing `compact`
request with `method: "handoff"`; the reply is the outcome, not an
acknowledgement. An overnight supervisor runs it once at its handoff-ready point,
thirty minutes or a quarter of the run before the target wake time, so the
coordinator turns after it read the document instead of the night's transcript.
ACP has no compaction command in this build, so an ACP client cannot trigger one.

### The token-budget tail

The recent-token budget (`[compaction] recent_tokens`, 20,000 by default) stays
verbatim. The document covers everything before that tail and the outcome reports
how many messages it covered. When every message fits inside the budget there is
nothing to cover, and the request reports that instead of writing an empty
document.

### The document is not validated

Generation is one completion. Its text is committed as-is: a missing heading or
an omitted section is not a failure, and the boundary still keeps the document
plus the verbatim tail. Nothing re-reads the document to grade it.

### Failure and cancellation

A provider error or a cancellation commits nothing: the message count, the
transcript and the previous boundary are exactly what they were. Both are
reported to the caller as outcomes, and an overnight run logs them and continues
with its remaining turns.

Tests: `crates/app-core/src/agent/handoff_compaction_tests.rs` covers the
committed boundary, the uncovered tail, the provider error and the cancellation;
`crates/app-core/src/overnight_handoff_tests.rs` covers the supervisor pass and
its failure path; `crates/harness-api-server/src/translate_handoff_tests.rs`
covers the request field and the reported outcomes.

## `--resume` behaviour

This build has no interactive session picker. Session resume is a single global `--resume`
flag (`src/cli/args.rs:63-68`). Passing `--resume` with no value means the daemon does not
start and exits with an error. The text is "the interactive session picker is unavailable in
this headless build; pass an explicit session id to --resume" (`src/cli/dispatch.rs:97-101`).

### Default behaviour

- `--resume <id>` takes a session id and starts with that session. When the value is not a
  full id, it is first resolved from the local store (`src/cli/dispatch.rs:696-736`).
- If resolution fails, the raw id is passed to the server so the server can resolve it again
  (`src/cli/dispatch.rs:721`).
- `--resume` can be combined with other global flags. The session resumes in this process, and
  the connected client decides which display shows it.

### Displaying a resumed session

This build resumes a session inside the daemon or the `run` process and opens no terminal
window. Which window or pane shows it is the client's business. The old TUI's session
picker and the launcher that opened runs in a new terminal or multiplexer pane are not in
this tree. So `session_picker_enter` or `[terminal]` keys left in an old config file are
not read, and there is no window-selecting flag such as the old `--fresh-spawn`.
`crates/terminal-launch` keeps only the client terminal environment snapshot and hook
command parsing.

### Saved sessions

`/save` and `/unsave` are client slash commands, and this build has no such client. The
save label attached to a session survives as the `save_label` field in harness API
responses (`crates/harness-api-server/src/translate.rs:255`, `:883`).