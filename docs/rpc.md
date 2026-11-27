# Host programs (RPC)

`horon rpc` (or `horon --mode rpc`) lets a program that owns the process drive one session: one JSON command per line on stdin, one frame per line on stdout. Use it instead of scraping terminal output.

## Commands

| Command | Use |
| --- | --- |
| `negotiate_protocol` | Answers `{protocolVersion: 1}` |
| `prompt` | Start a turn |
| `steer` / `follow_up` | Add a message to the running turn (soft interrupt) |
| `abort` / `abort_and_prompt` | Cancel, or cancel then send a replacement prompt |
| `get_state`, `get_session_stats`, `get_messages` | Read the session |
| `get_available_models`, `set_model`, `set_thinking_level` | Model and effort |
| `set_todos`, `set_session_name`, `compact` | Change session state |
| `bash` | Run a shell command through the same gates the tool uses (allow list, plan mode, approval, destructive gate) |
| `get_available_commands` | Lists `compact` and `model` |

## Not available

| Missing | Instead |
| --- | --- |
| `new_session`, `switch_session`, `branch`, `handoff` | One session per process: start another process |
| `abort_bash` | `bash` is not concurrent here |
| `get_messages_page`, `get_last_assistant_text` | `get_messages` returns the whole transcript |

For several sessions, remote access or a typed client, use the SDKs over `api-bridge` ([harness-api.md](harness-api.md)).
