# horon-agent-sdk (Python)

The Python SDK for the horon-agent harness API. It connects to horon-agent over
its Unix socket (a named pipe on Windows), speaks protocol v1, and exposes the
same capability surface as the Rust and TypeScript SDKs.

```python
import asyncio

from horon_agent_sdk import HoronAgentClient


async def main() -> None:
    client = await HoronAgentClient.launch()
    try:
        session = await client.create_session()
        turn = await client.run(session["session_id"], "say hi")
        print(turn.text)
    finally:
        await client.close()


asyncio.run(main())
```

## Install

```bash
pip install horon-agent-sdk
# or
uv add horon-agent-sdk
```

Platform wheels carry the horon-agent runtime for that platform. The pure wheel
and the sdist do not, so `launch()` falls back to `horon-agent` on `PATH`.

## Requirements

- Python 3.10 or newer, on Linux, macOS, or Windows.
- The runtime dependency is `jsonschema`, used only by structured output and
  session-tool input validation.
- `launch()` needs a horon-agent binary: the one bundled in a platform wheel,
  one on `PATH`, or an explicit path.

## Two ways to use horon-agent

`HoronAgentClient.launch()` starts a **private instance**: its own state
directory, its own sessions, its own socket. Use it when your application embeds
horon-agent as an engine. `close()` stops the daemon and removes the temporary
state directory.

`HoronAgentClient.connect()` attaches to the horon-agent **already running** on
the machine. Use it for an editor plugin, a dashboard, or a hotkey tool that
automates the user's own sessions. Anything done there is visible in their
terminal.

## Quick start

```python
client = await HoronAgentClient.launch()
session = await client.create_session()  # working_dir=None -> runtime default
await client.send_message(session["session_id"], "hello")
```

### Full system prompt override

```python
from horon_agent_sdk import CreateSessionOptions

session = await client.create_session_with_options(
    CreateSessionOptions(
        working_dir="/project",
        system_prompt="Review precisely.\nKeep Unicode: λ",
    )
)
```

`system_prompt=None` keeps normal prompt assembly, and `""` overrides it with no
text at all. Both are persisted by the runtime and cannot change later.

### Controlling tools

Session tools are modelled as `SdkTool` definitions whose callbacks never cross
the wire. `create_session_with_options` configures them before the session is
returned; `configure_tools` replaces the policy of an idle session.

```python
from horon_agent_sdk import SdkTool, SessionToolsOptions, ToolResult

tool = SdkTool(
    name="lookup",
    description="Look up a value",
    parameters={
        "type": "object",
        "properties": {"value": {"type": "string"}},
        "required": ["value"],
        "additionalProperties": False,
    },
    execute=lambda tool_input, context: f"found {tool_input['value']}",
    timeout_ms=10_000,
)

session = await client.create_session_with_options(
    CreateSessionOptions(
        working_dir="/project",
        tools=SessionToolsOptions(enabled=[], custom=[tool]),
    )
)
```

`enabled=None` inherits configured defaults, `[]` exposes no built-in or MCP
tools, and `disabled` entries win over everything else. Callbacks are
session-scoped, run once per `call_id`, validate their input against
`parameters`, and report exceptions as tool errors rather than ending the turn.
`context.cancel_event` (and `context.cancelled`) is set on cancellation,
disconnect, or timeout; cooperate with it to stop side effects.

Low-level clients that handle `tool_call` events themselves call
`submit_tool_result(session_id, call_id, result)`.

### Assistant messages and final answers

`run()` returns a `TurnResult`:

- `text` is every assistant text delta in the turn, including narration.
- `final_text` is the last completed message, or the whole-turn text on bridges
  without message framing.
- `messages` are completed `AssistantTextMessage` frames, with `message_id`
  where the bridge supplies one.
- `reasoning`, `tool_calls`, `usage`, `stop_reason`, and `stop_message` carry
  the rest of the turn.

## Structured output

`run_structured()` wraps a turn in a JSON-only contract and validates the final
message against a JSON Schema. Invalid JSON or a schema violation triggers a
bounded corrective retry that includes the validation errors and the previous
response.

```python
from horon_agent_sdk import StructuredOutputError

schema = {
    "type": "object",
    "additionalProperties": False,
    "required": ["summary", "count"],
    "properties": {
        "summary": {"type": "string"},
        "count": {"type": "integer", "minimum": 0},
    },
}

result = await client.run_structured("session", "Summarize the work", schema, max_retries=2)
print(result.data["summary"])

try:
    await client.run_structured("session", "Summarize again", schema, max_retries=0)
except StructuredOutputError as error:
    print(error.validation_errors, error.last_text, len(error.attempts))
```

This is an SDK-level contract: no special bridge or model feature is required.

## Streaming

`events()` is an async iterator; `events(session_id)` filters to one session.
Unknown event kinds are delivered too, so always keep a fallback branch.

```python
stream = client.events(session["session_id"])
await client.send_message(session["session_id"], "hello")
try:
    async for event in stream:
        if event["ev"] == "text_delta":
            print(event["text"], end="", flush=True)
        elif event["ev"] == "turn_done":
            break
finally:
    await stream.aclose()
```

Leaving the loop does not close an async iterator, so call `aclose()` (or use
`run()`, which does it). The buffer is bounded in events and bytes: ordinary
frames are dropped past the budget and the iterator raises
`event_buffer_overflow`; control frames (approvals, asks, cancellations, turn
terminals, tool outcomes) get an extra quarter and a full reserve closes the
stream rather than dropping one. `stream.buffered` reports the current backlog.

### All-session events

```python
stop = asyncio.Event()
stream = client.global_events(GlobalEventsOptions(signal=stop, discovery_interval_ms=1_000))
try:
    async for event in stream:
        ...
finally:
    stop.set()
    await stream.aclose()
```

The bridge has one attachment per connection, so this stream discovers persisted
sessions, attaches one child client to each, and re-scans for new sessions.
Delivery starts at attach: events emitted before a child attaches, or during a
reconnect, cannot be recovered by protocol v1. Custom transports are refused
because they cannot be cloned into per-session connections.

## API surface

Every method mirrors the Rust SDK's name exactly:

- Sessions: `list_sessions`, `list_sessions_limited`, `archive_session`,
  `restore_session`, `set_retention_policy`, `create_session`,
  `create_session_with_options`, `attach_session`, `fork_session`,
  `transfer_session`, `detach_session`, `rename_session`, `set_session_saved`,
  `clear`, `rewind`, `rewind_undo`, `compact`.
- Messages: `send_message`, `send_message_with_reminder`,
  `send_system_reminder`, `send_context_message`, `cancel`, `soft_interrupt`,
  `soft_interrupt_with_images`, `soft_interrupt_with_id`,
  `cancel_soft_interrupts`, `cancel_soft_interrupts_by_id`,
  `cancel_soft_interrupt`, `background_tool`, `background_tool_result`.
- Interaction: `respond_to_permission`, `respond_to_ask`,
  `respond_to_ask_with_id`.
- Reads: `get_history`, `get_history_with_images`, `peek_session`,
  `list_models`, `get_runtime_info`, `runtime_command`, `read_file`,
  `find_files`, `search_text`, `file_status`, `todo_state`,
  `ack_todo_follow_up`.
- Tools: `configure_tools`, `list_tools`, `submit_tool_result`.
- Runtime: `get_runtime_state`, `apply_control`, `get_control_receipt`,
  `get_snapshot`, `snapshot_with_buffered_events`.
- Replication: `list_session_heads`, `select_session_head`,
  `get_history_page`, `export_session_events`, `import_session_events`,
  `export_session_blob`, `import_session_blob`, `session_blob_status`,
  `export_session_record_blob`, `import_session_record_blob`,
  `session_record_blob_status`.
- Credentials: `set_api_key`, `clear_api_key`, `notify_auth_changed`,
  `invalidate_usage`, `set_model`, `set_reasoning_effort`.
- Turns and transport: `run`, `run_structured`, `events`, `global_events`,
  `request`, `notify`, `supports`, `ping`, `close`.

Capabilities: `client.supports("session_tools")` answers whether the connected
server advertises a capability. Methods that require one raise `unsupported`
before anything reaches the wire.

## Binary resolution order

`launch()` resolves the runtime in this order:

1. `LaunchOptions(binary=...)`.
2. `bundled_horon_agent_binary()` — the runtime inside a platform wheel at
   `horon_agent_sdk/_bin/horon-agent` (`horon-agent.exe` on Windows).
3. `horon-agent` on `PATH`.

If all three fail, the error is `horon_agent_not_found` and names the three
fixes: reinstall a platform wheel, install horon-agent, or pass `binary`.

## Configuration

- `HORON_AGENT_API_SOCKET` overrides the API socket path; otherwise it is
  `horon-agent-api.sock` below the runtime directory.
- The runtime directory is `HORON_AGENT_RUNTIME_DIR`, then `XDG_RUNTIME_DIR`,
  then `TMPDIR` on macOS, then a per-user temporary directory. The derivation
  matches the Rust side exactly, including the Windows pipe name.
- `HORON_HOME` names the state directory; `launch()` overrides it for its
  private instance.

## Errors

SDK and protocol failures raise `HarnessError`; its stable `code` is the value to
branch on, and the message is diagnostic text that can change. OS errors from
the platform can still surface separately.

### Launch and connection errors

| Code | Cause | Recovery |
| --- | --- | --- |
| `horon_agent_not_found` | `launch()` could not execute horon-agent. | Install a platform wheel or horon-agent, put it on `PATH`, or pass `binary`. |
| `startup_failed` | The private instance exited before opening its API socket. Its stderr is in the message. | Fix the reported configuration, credential, or binary error before retrying. |
| `startup_timeout` | No API socket within `startup_timeout_ms`. | Raise the timeout on a slow machine; check stderr and runtime-directory permissions. |
| `invalid_instance_home` | The instance home or a credential path is unsafe (same directory, symlink, file, traversal). | Choose a separate real directory. |
| `connect_failed` | The bridge is absent, dead, or listening elsewhere. | Run `horon api-bridge`; check `socket_path` or `HORON_AGENT_API_SOCKET`. |
| `handshake_failed` | The peer replied with an invalid frame during negotiation. | Confirm the socket is a harness socket and upgrade both sides. |
| `unsupported_version` | Client and bridge share no protocol major version. | Upgrade the older side. |

### Request and transport errors

| Code | Cause | Recovery |
| --- | --- | --- |
| `disconnected` | The socket closed or a write failed with work in flight. | Reconnect; retry only idempotent reads. |
| `timeout` | No reply within `request_timeout_ms` (30 s by default). | Check daemon health; treat the outcome as unknown before repeating a mutation. |
| `unexpected_reply` | The reply was valid protocol data but not the required event kind. | Upgrade both sides and report the versions. |
| `unknown_request` | The bridge does not implement that request tag. | Upgrade horon-agent, or stop using the newer method. |
| `unknown_session` | The session is gone, unavailable, or the connection is not attached. | Refresh `list_sessions()`, use the right instance, attach where required. |
| `invalid_request` | Arguments or state violate the contract. | Correct the input; the message names the constraint. |
| `invalid_option` | A client-only option is out of range. | Correct the option, such as `max_buffered_events`. |
| `unsupported` | The server does not advertise the required capability. | Update the daemon and bridge. |
| `busy` | Another tool configuration is in flight for this session. | Await the previous one. |
| `too_many_requests` | `max_pending_requests` requests await replies. | Await replies or raise the limit deliberately; the refused request never reached the wire. |
| `transport_backpressure` | The transport reported a full buffer and provides no drain signal. | Give it `on_drain`, or return `None` from `write` if it never blocks. |
| `internal` | The bridge or daemon failed unexpectedly. | Preserve logs, retry once if safe, then report. |

### Streaming and structured-output errors

| Code | Cause | Recovery |
| --- | --- | --- |
| `unsupported_transport` | `global_events()` was called on a custom transport. | Use a native socket client, or consume per-session `events()`. |
| `event_buffer_overflow` | A consumer fell behind its bounded buffer. | Consume faster or raise the buffer deliberately; recreate the iterator. |
| `snapshot_marker_missing` | `snapshot_with_buffered_events()` buffered past its ceiling without seeing the marker that bounds the view. | Reconnect before trusting the snapshot; a busy session is not a substitute for the marker. |
| `concurrent_next` | Two callers awaited the same `global_events()` iterator at once. | Give the iterator one consumer and fan out in the application. |
| `structured_schema_invalid` | The JSON Schema passed to `run_structured()` is invalid. | Fix the schema; do not retry unchanged. |
| `structured_output_invalid` | The retry budget ran out without schema-valid JSON. | Catch `StructuredOutputError` and inspect `validation_errors`, `last_text`, `attempts`. |

Unknown future server codes stay readable as `HarnessError.code`; keep a default
branch instead of assuming this table is exhaustive.

## Stability

This package is generally available and follows semver against protocol v1.

- Protocol v1 is stable: the handshake negotiates a major version, and a server
  that cannot speak v1 is rejected with `unsupported_version` rather than
  half-working.
- Additive protocol changes ship in minor releases; existing frames keep their
  shape.
- Drift is checked mechanically. `cargo test -p horon-agent-sdk parity` requires
  every shared capability to exist in the Rust, TypeScript, and Python clients,
  and a Python test compares this package's tag sets with
  `sdk/typescript/src/protocol.ts`.

Compatibility: CPython 3.10+, asyncio only (no threads required). Linux, macOS,
and Windows are wired up; Linux and macOS are exercised end to end.

## Forward compatibility

The harness may add events at any time within protocol v1. Frames are
dictionaries: read `event["ev"]` and keep a fallback branch for kinds added
after your version. `is_known_event(frame)` tells you whether this SDK knows a
tag, without hiding fields the server added.

## Releasing

See [RELEASING.md](RELEASING.md).

## Development

```bash
cd sdk/python
uv sync
uv run ruff check .
uv run ruff format --check .
uv run mypy src
uv run pytest
```

The tests run against an in-process mock harness that speaks real NDJSON frames,
so no daemon or credentials are needed.
