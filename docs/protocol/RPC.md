# stdio RPC mode

Language: **English** | [한국어](RPC.ko.md)

RPC mode lets a host program drive the agent as a child process: one JSON object per line on stdin, one frame per line on stdout. It is the surface to embed Horon in another program that owns the process, needs streaming output, cancellation, and session controls, but cannot speak ACP.

This page states the contract this build implements and, row by row, which parts of the published omp RPC reference it serves. Horon speaks the same wire protocol as omp so that a host written for omp can drive Horon; where it cannot, the support table says so instead of failing quietly.

## Running

```bash
horon-agent rpc                      # subcommand form
horon-agent --mode rpc               # flag form, same mode
horon-agent rpc --resume <session>   # attach to an existing session
```

Both forms reach the same adapter, and the subcommand wins when a host passes both. `--session-dir` is accepted and ignored: sessions live under the Horon home, and the adapter logs a warning rather than pretending to honour it.

stdout carries frames only. Diagnostics go to stderr through the logging subsystem, so a host can parse stdout without filtering. Closing stdin ends the process; the adapter exits after the turn in flight, if any.

The adapter starts the daemon when none is running, then subscribes and reads the session history, exactly as the ACP adapter does. Model calls go to the endpoint of the wire the active `[providers.<name>]` profile's `type` names, so that profile has to be ready before the host starts.

## What a turn looks like

A minimal exchange is two lines in and one line out for the acknowledgement, then the turn frames. The `prompt` response is written *before* the turn runs, so a host that waits for it never deadlocks on a long answer.

```json
{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1],"maxFrameBytes":1048576,"maxReassembledFrameBytes":67108864,"supportsBashExcludeFromContext":true}
{"id":"p1","type":"prompt","message":"Reply with only the word ok"}
{"id":"p1","type":"response","command":"prompt","success":true}
```

One complete text turn writes these frames in this order: `agent_start`, `turn_start`, the user `message_start` and `message_end`, the assistant `message_start`, its `message_update` frames, the assistant `message_end`, `turn_end`, `agent_end`, then `prompt_result` and `session_settled`. A captured single-text turn is 19 frames from `ready` to `session_settled`; the same capture with one tool call is 58. A host may also see a stray `extension_ui_request` before `agent_end`, which it should ignore.

Two rules hold for every prompt:

- Exactly one `prompt_result` and exactly one `session_settled` end a prompt, whether the turn completed, was aborted, or failed. `prompt_result` carries `status` (`completed`, `aborted`, or `error`) beside `agentInvoked` and `sessionSettled`.
- Tool executions arrive *after* the assistant `message_end`. The daemon streams a tool call's arguments before the response closes, but the host's step boundary puts the executions and their result messages after the message, so the adapter holds them back. That is the only reordering in this mode.

While a prompt runs the adapter keeps reading stdin, so `steer`, `follow_up`, `abort_and_prompt`, `abort`, `get_state`, and `negotiate_protocol` work mid-turn. Every other command is refused with `code: "busy"` instead of queued, because a queued `message` would interleave with the running turn.

Stdin is read by a dedicated task. A read cancelled by `select!` loses the bytes it had already consumed, which truncates a frame at the worst moment, so the reader hands whole lines over a channel instead.

## Commands

`Horon` cites the file and line that answers the command; `omp` cites the section of the published reference and what a host sees. "partial" means the row answers but a field or a variant is missing.

| Command | Horon | omp | Verdict |
| --- | --- | --- | --- |
| `negotiate_protocol` | `src/cli/rpc/adapter.rs:232` answers `{protocolVersion:1}` for any request | Transport versions, "negotiate_protocol" | partial — v1 only, no v2 chunking |
| `prompt` | `src/cli/rpc/adapter.rs:218`, turn driven at `src/cli/rpc/adapter.rs:349` | Commands table, `prompt` | yes |
| `steer` | `src/cli/rpc/adapter.rs:435` sends a soft interrupt with `urgent: false` | "Sending work while a turn is active" | yes |
| `follow_up` | Same arm, `urgent: true` | Same section | partial — steering and follow-up modes are fixed at `all`; `set_steering_mode` and `set_follow_up_mode` do not exist |
| `abort` | `src/cli/rpc/adapter.rs:237` while idle, `src/cli/rpc/adapter.rs:426` mid-turn | "Cancel safely" | yes — the turn ends on the daemon's interrupt, not on this response |
| `abort_and_prompt` | `src/cli/rpc/adapter.rs:395` | "Cancel safely" | yes — cancels the active turn, awaits settlement, then sends the replacement prompt |
| `get_state` | `src/cli/rpc/state.rs:108` | Commands table, `get_state` | yes |
| `set_model` | `src/cli/rpc/state.rs:238` | Commands table, `set_model` | yes — the response is the selected model; `provider` and `modelId` are read from one `modelId` field |
| `get_available_models` | `src/cli/rpc/state.rs:135` | Commands table, `get_available_models` | yes — unavailable routes are dropped so a host cannot select one that fails |
| `set_thinking_level` | `src/cli/rpc/state.rs:259` | Commands table, `set_thinking_level` | yes |
| `get_available_commands` | `src/cli/rpc/adapter.rs:253` | Commands table, `get_available_commands` | partial — two entries, `compact` and `model` |
| `set_todos` | `src/cli/rpc/state.rs:295` | Commands table, `set_todos` | yes — `blocked` and `blocker` are real statuses here, so they survive a round trip |
| `compact` | `src/cli/rpc/state.rs:314` | Commands table, `compact` | partial — `CompactionResult` carries `summary` only; `customInstructions` is ignored |
| `get_session_stats` | `src/cli/rpc/state.rs:333` | Commands table, `get_session_stats` | yes |
| `get_messages` | `src/cli/rpc/state.rs:364` | Commands table, `get_messages` | yes |
| `set_session_name` | `src/cli/rpc/state.rs:279` | Commands table, `set_session_name` | yes |
| `bash` | `src/cli/rpc/state.rs:539` enforces tool allow-list/disabled-list, plan mode, approval, interceptor, git guard, and destructive-command gates before running `sh -c` | Commands table, `bash` | partial — runs to completion before its response, so it is serialized like every other command |
| `abort_bash` | `src/cli/rpc/adapter.rs:323` | Commands table, `abort_bash` | partial — answered as unsupported because `bash` cannot be concurrent here |
| `new_session` | `src/cli/rpc/adapter.rs:241` | Commands table, `new_session` | no — one session per process; start another process |
| `switch_session`, `branch`, `get_branch_messages`, `handoff` | Not in `src/cli/rpc/command.rs:93` | Commands table, session rows | no |
| `get_last_assistant_text` | Absent | Commands table | no — `get_messages` returns the transcript |
| `get_messages_page` | Absent | Commands table, `get_messages_page` | no — no cursor paging; `get_messages` returns everything |
| `export_html` | Absent | Commands table | no |
| `set_host_tools`, `set_host_uri_schemes` | Absent | "Host tools and host URIs" | no — tools are resolved on the daemon side |
| `set_subagent_subscription`, `get_subagents`, `get_subagent_messages` | Absent | "Subagent frames" | no |
| `cycle_model`, `cycle_thinking_level` | Absent | Commands table, model rows | no |
| `set_steering_mode`, `set_follow_up_mode`, `set_interrupt_mode` | Absent | "Sending work while a turn is active" | no — fixed at `all`, `all`, and `immediate` |
| `set_auto_compaction`, `set_auto_retry`, `abort_retry` | Absent | "Errors and recovery" | no |
| `set_fast_mode` | Absent | Commands table | no |
| `get_login_providers`, `login` | Absent | Commands table, login rows | no — credentials are configured outside the session |

A line that is not a JSON object, or that names no `type`, is ignored: a host may interleave blank lines while it starts up, and a transport hiccup must not end the session. A line over one mebibyte is failed with `code: "frame_too_large"` and the connection stays open.

## Frames on stdout

`Horon` cites the writer; `omp` cites the frame table in the published reference.

| Frame | Horon | omp | Verdict |
| --- | --- | --- | --- |
| `ready` | `src/cli/rpc/frame.rs:32` | Other stdout frames, `ready` | partial — `supportedProtocolVersions` is `[1]`, not `[1,2]` |
| `response` | `src/cli/rpc/frame.rs:43`, `src/cli/rpc/frame.rs:53` | Response envelope | yes |
| `prompt_result` | `src/cli/rpc/frame.rs:82` | Other stdout frames | yes — always emitted, with `status` added |
| `session_settled` | `src/cli/rpc/frame.rs:92` | Not documented as a separate frame | yes |
| `agent_start`, `agent_end` | `src/cli/rpc/mapper.rs:82`, `src/cli/rpc/mapper.rs:169` | Agent lifecycle events | yes |
| `turn_start`, `turn_end` | `src/cli/rpc/mapper.rs:90`, `src/cli/rpc/mapper.rs:555` | Agent lifecycle events | yes |
| `message_start`, `message_end` | `src/cli/rpc/frame.rs:100`, `src/cli/rpc/frame.rs:104` | Agent lifecycle events | yes — `messageId` is added beside `message` |
| `message_update` | `src/cli/rpc/frame.rs:109` | Agent lifecycle events | yes — `text_*`, `thinking_*`, and `toolcall_*` events only; no `image_end` and no `done` or `error` event |
| `tool_execution_start`, `tool_execution_update`, `tool_execution_end` | `src/cli/rpc/mapper.rs:520`, `src/cli/rpc/mapper.rs:527`, `src/cli/rpc/mapper.rs:539` | Agent lifecycle events | yes — emitted after the assistant `message_end` |
| `available_commands_update` | `src/cli/rpc/adapter.rs:173` | Other stdout frames | partial — written once at startup, never updated |
| `session_info_update` | `src/cli/rpc/mapper.rs:141` | Other stdout frames | yes |
| `model_changed`, `thinking_level_changed` | `src/cli/rpc/mapper.rs:147`, `src/cli/rpc/mapper.rs:150` | Session events | yes |
| `notice` | `src/cli/rpc/frame.rs:96` | Session events, `notice` | partial — used for guardrail stops and compaction; `source` is absent |
| `rpc_frame_error` | `src/cli/rpc/frame.rs:73` | Other stdout frames | yes — a frame over one mebibyte is replaced, never truncated |
| `rpc_chunk` | Absent by design: `src/cli/rpc/frame.rs:18` pins `PROTOCOL_VERSION` to 1 | "Transport versions and large frames" | no — reassembling a chunked frame is v2 only |
| `command_output` | Absent | Other stdout frames | no — no locally handled slash command produces text |
| `config_update` | Absent | Other stdout frames | no — `model_changed` and `thinking_level_changed` carry the change instead |
| `extension_ui_request` | `src/cli/rpc/adapter.rs:164` sends one empty `setWidget` | "Extension UI sub-protocol" | partial — a single acknowledgement so a host that waits for it does not stall; no dialog, no `cancel`, no value responses |
| `extension_error` | Absent | Other stdout frames | no |
| `host_tool_call`, `host_tool_cancel`, `host_uri_request`, `host_uri_cancel` | Absent | "Host tools and host URIs" | no |
| `subagent_lifecycle`, `subagent_progress`, `subagent_event` | Absent | "Subagent frames" | no |
| `auto_compaction_start`, `auto_compaction_end`, `auto_retry_start`, `auto_retry_end` | Absent | Session events | no — a `notice` reports compaction |

## Differences a host must handle

- A host that negotiates version 2 is answered with version 1. It has to downgrade and accept unchunked frames; this build never emits `rpc_chunk`.
- A frame over one mebibyte is reported as `rpc_frame_error` with the frame it replaced, not delivered. Large tool output therefore reaches the host through its own `tool_execution_end`, not through a chunk.
- `prompt_result` is always written and always carries `sessionSettled: true`, so a host may end a turn on either `prompt_result` or `session_settled` without waiting for both.
- Nothing in this mode asks the host for a decision. Approvals and questions are resolved on the daemon side, so `extension_ui_request` never blocks a turn.

## Proposed host question capability

Status: design for #547. The behavior documented above is the current RPC v1 contract. The new capability reuses the typed `AskRequested` and `ask_response` contract from the [Harness API](HARNESS_API.md) for structured questions and choices. Approval decisions stay on the existing approval channel.

| Option | Consequence |
| --- | --- |
| Add dialogs to the legacy widget frame | Couples the question lifecycle to an existing compatibility frame and can surprise hosts. |
| Add an opt-in RPC v1 capability | Preserves the version and existing frames while allowing a host to render a bounded question. Selected. |
| Create a second UI adapter | Duplicates the turn and cancellation logic. |

A host sends `negotiate_protocol` with `capabilities: ["ask_v1"]` before its first prompt. The adapter echoes `ask_v1` only if it can forward typed questions and answers to the daemon. Legacy negotiations without the field retain the existing response shape. Latch the agreed capability per stdio connection before attaching the first turn, and keep it fixed through the connection lifetime. Missing capability uses the current daemon behavior. Older adapters can omit the field and the host then uses RPC v1 as it does today.

The adapter maps one live `AskRequested` to `ask_requested` with an opaque `askId` and the typed `questions` payload. It accepts `ask_response` with a normal command `id`, the same `askId` and the typed `answers`. Apply the existing `ask` shape and size limits, the RPC frame ceiling and one advertised pending question batch per connection. If a request cannot be represented within those limits, let the existing daemon fallback settle it. During a turn the adapter continues reading stdin, including `abort` and matching `ask_response`. Malformed or stale IDs return a correlated error and leave other pending state alone. Accept a reply only for the current connection's pending batch. When the daemon accepts an answer, report success and retire the batch. A repeated answer gets a settled error with no second effect.

The daemon owns the existing answer deadline. The adapter never restarts it. A daemon timeout, host EOF, abort or turn settlement retires the pending ID. For a capable connection still open, emit `ask_closed` with the ID and reason so the host can dismiss its UI. Keep each turn's prompt acknowledgment, terminal frame ordering and settlement count. The host renders text and choices as data and the adapter runs no in-process host code. The ACP v1 and v2 adapters remain independent.

### Implementation boundaries

1. In `src/cli/rpc/`, extend negotiation and frame validation. Freeze legacy startup, negotiation and terminal frame fixtures before exposing a new frame.
2. Translate the typed question and answer, enforce pending-ID scope, and tie retirement to daemon settlement. Keep prompt payload and timeout policy with the existing `ask` contract.
3. Add subprocess host fixtures for negotiated, legacy and silent hosts. Run `cargo test --locked -p horon-agent --lib cli::` and include the focused cases in GitHub Actions.

| Fixture action | Expected observation |
| --- | --- |
| Negotiate `ask_v1`, start one turn, then answer its question once | One question frame, one successful correlated answer and one `prompt_result` followed by one `session_settled`. |
| Send stale and duplicate `ask_response` IDs | The stale answer does not affect the pending question and the duplicate has no second effect. |
| Use a v1 host with no capability | The startup and turn frames match the existing fixture and no new question frame appears. |
| Let the existing daemon deadline elapse without an answer | The daemon's existing fallback runs once. The host observes question closure and terminal frames exactly once. |

Use a fixture provider, controlled daemon clock and captured JSONL. Drive the clock to just before and then exactly to the daemon deadline. Check that the question remains pending at the first point and settles at the second. No network or terminal UI is required for the RPC subprocess fixture.

## Opt-in A2UI v1.0 surface capability (`a2ui_v1`)

Stdio RPC hosts can opt into A2UI v1.0 rendering before their first prompt without affecting unnegotiated hosts (`src/cli/rpc/command.rs`, `src/cli/rpc/frame.rs`).

| Frame / field | Direction | Purpose |
| --- | --- | --- |
| `negotiate_protocol` (`capabilities: ["a2ui_v1"]`, optional `a2ui`) | Host -> Agent | Latches `a2ui_v1` and optional renderer capabilities (`version`, `supportedCatalogIds`, `acceptsInlineCatalogs`) for the connection |
| `a2ui_update` | Agent -> Host | Streams one validated `agent_to_renderer` envelope (`toolCallId`, `sequence`, `envelope`, optional `fallbackText`) |
| `message_end` / `get_messages` (`type: "a2ui"` part) | Agent -> Host | Preserves assistant A2UI content parts (`envelopes`, optional `fallbackText`) alongside text blocks |
| `a2ui_event` | Host -> Agent | Submits a validated `action`, `callAgentFunction`, or `error` `envelope` (and optional `dataModel`) back to the active session |

- Connections that omit `a2ui_v1` never unlock the `a2ui` tool, never emit `a2ui_update`, and reject `a2ui_event` with an error response.

### One-shot `horon run` flags

`horon-agent run` accepts `--a2ui` and repeatable `--a2ui-catalog <uri>` (`src/cli/commands/a2ui_run.rs`). Plain `horon run --a2ui` renders readable `[A2UI surface: ...]` fallback cards to stdout, `--json` adds `type: "a2ui"` blocks to the JSON response, and `--ndjson` emits `type: "a2ui"` stream frames (`sequence`, `envelope`, `fallback_text`). Running `horon run` without `--a2ui` keeps the default tool set and prompt prefix byte-for-byte unchanged.

## Verification

The adapter has an integration test that scripts the daemon over an in-process socket pair, so the real protocol path runs without a daemon or a model. It covers the startup handshake, version negotiation, an unknown command, non-JSON lines, `get_state`, one full prompt turn, `abort` while idle, an oversized frame, `set_todos` persistence, and the model catalog projection.

```bash
cargo test -p horon-agent --lib cli::rpc
python3 scripts/ci/check_docs_pairs.py
```

The evidence rows above name the file and line that answers each command or writes each frame, so a reviewer can check a row without running anything.
