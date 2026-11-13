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

The adapter starts the daemon when none is running, then subscribes and reads the session history, exactly as the ACP adapter does. Model calls go to the OpenAI-compatible endpoint of the active `[providers.<name>]` profile, so that profile has to be ready before the host starts.

## What a turn looks like

A minimal exchange is two lines in and one line out for the acknowledgement, then the turn frames. The `prompt` response is written *before* the turn runs, so a host that waits for it never deadlocks on a long answer.

```json
{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1],"maxFrameBytes":1048576,"maxReassembledFrameBytes":67108864}
{"id":"p1","type":"prompt","message":"Reply with only the word ok"}
{"id":"p1","type":"response","command":"prompt","success":true}
```

One complete text turn writes these frames in this order: `agent_start`, `turn_start`, the user `message_start` and `message_end`, the assistant `message_start`, its `message_update` frames, the assistant `message_end`, `turn_end`, `agent_end`, then `prompt_result` and `session_settled`. Measured on this build's reference host, a single-text turn is 18 frames and a turn with one tool call is 58.

Two rules hold for every prompt:

- Exactly one `prompt_result` and exactly one `session_settled` end a prompt, whether the turn completed, was aborted, or failed. `prompt_result` carries `status` (`completed`, `aborted`, or `error`) beside `agentInvoked` and `sessionSettled`.
- Tool executions arrive *after* the assistant `message_end`. The daemon streams a tool call's arguments before the response closes, but the host's step boundary puts the executions and their result messages after the message, so the adapter holds them back. That is the only reordering in this mode.

While a prompt runs the adapter keeps reading stdin, so `steer`, `follow_up`, `abort_and_prompt`, `abort`, `get_state`, and `negotiate_protocol` work mid-turn. Every other command is refused with `code: "busy"` instead of queued, because a queued `message` would interleave with the running turn.

Stdin is read by a dedicated task. A read cancelled by `select!` loses the bytes it had already consumed, which truncates a frame at the worst moment, so the reader hands whole lines over a channel instead.

## Commands

`Horon` cites the file and line that answers the command; `omp` cites the section of the published reference and what a host sees. "partial" means the row answers but a field or a variant is missing.

| Command | Horon | omp | Verdict |
| --- | --- | --- | --- |
| `negotiate_protocol` | `src/cli/rpc/adapter.rs:224` answers `{protocolVersion:1}` for any request | Transport versions, "negotiate_protocol" | partial — v1 only, no v2 chunking |
| `prompt` | `src/cli/rpc/adapter.rs:210`, turn driven at `src/cli/rpc/adapter.rs:338` | Commands table, `prompt` | yes |
| `steer` | `src/cli/rpc/adapter.rs:408` sends a soft interrupt with `urgent: false` | "Sending work while a turn is active" | yes |
| `follow_up` | Same arm, `urgent: true` | Same section | partial — steering and follow-up modes are fixed at `all`; `set_steering_mode` and `set_follow_up_mode` do not exist |
| `abort` | `src/cli/rpc/adapter.rs:229` while idle, `src/cli/rpc/adapter.rs:410` mid-turn | "Cancel safely" | yes — the turn ends on the daemon's interrupt, not on this response |
| `abort_and_prompt` | `src/cli/rpc/adapter.rs:419` | "Cancel safely" | partial — no replacement prompt is scheduled; the new text becomes a steer |
| `get_state` | `src/cli/rpc/state.rs:108` | Commands table, `get_state` | yes |
| `set_model` | `src/cli/rpc/state.rs:234` | Commands table, `set_model` | yes — the response is the selected model; `provider` and `modelId` are read from one `modelId` field |
| `get_available_models` | `src/cli/rpc/state.rs:135` | Commands table, `get_available_models` | yes — unavailable routes are dropped so a host cannot select one that fails |
| `set_thinking_level` | `src/cli/rpc/state.rs:255` | Commands table, `set_thinking_level` | yes |
| `get_available_commands` | `src/cli/rpc/adapter.rs:242` | Commands table, `get_available_commands` | partial — two entries, `compact` and `model` |
| `set_todos` | `src/cli/rpc/state.rs:291` | Commands table, `set_todos` | yes — `blocked` and `blocker` are real statuses here, so they survive a round trip |
| `compact` | `src/cli/rpc/state.rs:302` | Commands table, `compact` | partial — `CompactionResult` carries `summary` only; `customInstructions` is ignored |
| `get_session_stats` | `src/cli/rpc/state.rs:321` | Commands table, `get_session_stats` | yes |
| `get_messages` | `src/cli/rpc/state.rs:352` | Commands table, `get_messages` | yes |
| `set_session_name` | `src/cli/rpc/state.rs:275` | Commands table, `set_session_name` | yes |
| `bash` | `src/cli/rpc/state.rs:411` runs the command locally with `sh -c` | Commands table, `bash` | partial — runs to completion before its response, so it is serialized like every other command |
| `abort_bash` | `src/cli/rpc/adapter.rs:312` | Commands table, `abort_bash` | partial — answered as unsupported because `bash` cannot be concurrent here |
| `new_session` | `src/cli/rpc/adapter.rs:233` | Commands table, `new_session` | no — one session per process; start another process |
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
| `ready` | `src/cli/rpc/frame.rs:30` | Other stdout frames, `ready` | partial — `supportedProtocolVersions` is `[1]`, not `[1,2]` |
| `response` | `src/cli/rpc/frame.rs:42`, `src/cli/rpc/frame.rs:51` | Response envelope | yes |
| `prompt_result` | `src/cli/rpc/frame.rs:80` | Other stdout frames | yes — always emitted, with `status` added |
| `session_settled` | `src/cli/rpc/frame.rs:91` | Not documented as a separate frame | yes |
| `agent_start`, `agent_end` | `src/cli/rpc/mapper.rs:82`, `src/cli/rpc/mapper.rs:166` | Agent lifecycle events | yes |
| `turn_start`, `turn_end` | `src/cli/rpc/mapper.rs:90`, `src/cli/rpc/mapper.rs:552` | Agent lifecycle events | yes |
| `message_start`, `message_end` | `src/cli/rpc/frame.rs:99`, `src/cli/rpc/frame.rs:103` | Agent lifecycle events | yes — `messageId` is added beside `message` |
| `message_update` | `src/cli/rpc/frame.rs:107` | Agent lifecycle events | yes — `text_*`, `thinking_*`, and `toolcall_*` events only; no `image_end` and no `done` or `error` event |
| `tool_execution_start`, `tool_execution_update`, `tool_execution_end` | `src/cli/rpc/mapper.rs:520`, `src/cli/rpc/mapper.rs:527`, `src/cli/rpc/mapper.rs:536` | Agent lifecycle events | yes — emitted after the assistant `message_end` |
| `available_commands_update` | `src/cli/rpc/adapter.rs:165` | Other stdout frames | partial — written once at startup, never updated |
| `session_info_update` | `src/cli/rpc/mapper.rs:141` | Other stdout frames | yes |
| `model_changed`, `thinking_level_changed` | `src/cli/rpc/mapper.rs:147`, `src/cli/rpc/mapper.rs:150` | Session events | yes |
| `notice` | `src/cli/rpc/frame.rs:95` | Session events, `notice` | partial — used for guardrail stops and compaction; `source` is absent |
| `rpc_frame_error` | `src/cli/rpc/frame.rs:72` | Other stdout frames | yes — a frame over one mebibyte is replaced, never truncated |
| `rpc_chunk` | Absent by design: `src/cli/rpc/frame.rs:18` pins version 1 | "Transport versions and large frames" | no — reassembling a chunked frame is v2 only |
| `command_output` | Absent | Other stdout frames | no — no locally handled slash command produces text |
| `config_update` | Absent | Other stdout frames | no — `model_changed` and `thinking_level_changed` carry the change instead |
| `extension_ui_request` | `src/cli/rpc/adapter.rs:156` sends one empty `setWidget` | "Extension UI sub-protocol" | partial — a single acknowledgement so a host that waits for it does not stall; no dialog, no `cancel`, no value responses |
| `extension_error` | Absent | Other stdout frames | no |
| `host_tool_call`, `host_tool_cancel`, `host_uri_request`, `host_uri_cancel` | Absent | "Host tools and host URIs" | no |
| `subagent_lifecycle`, `subagent_progress`, `subagent_event` | Absent | "Subagent frames" | no |
| `auto_compaction_start`, `auto_compaction_end`, `auto_retry_start`, `auto_retry_end` | Absent | Session events | no — a `notice` reports compaction |

## Differences a host must handle

- A host that negotiates version 2 is answered with version 1. It has to downgrade and accept unchunked frames; this build never emits `rpc_chunk`.
- A frame over one mebibyte is reported as `rpc_frame_error` with the frame it replaced, not delivered. Large tool output therefore reaches the host through its own `tool_execution_end`, not through a chunk.
- `prompt_result` is always written and always carries `sessionSettled: true`, so a host may end a turn on either `prompt_result` or `session_settled` without waiting for both.
- Nothing in this mode asks the host for a decision. Approvals and questions are resolved on the daemon side, so `extension_ui_request` never blocks a turn.

## Verification

The adapter has an integration test that scripts the daemon over an in-process socket pair, so the real protocol path runs without a daemon or a model. It covers the startup handshake, version negotiation, an unknown command, non-JSON lines, `get_state`, one full prompt turn, `abort` while idle, an oversized frame, `set_todos` persistence, and the model catalog projection.

```bash
cargo test -p horon-agent --lib cli::rpc
python3 scripts/ci/check_docs_pairs.py
```

The evidence rows above name the file and line that answers each command or writes each frame, so a reviewer can check a row without running anything.