# Lifecycle hooks

Language: **English** | [한국어](HOOKS.ko.md)

horon-agent can run external commands at well-defined lifecycle points, so other
programs can observe or control agent behavior without forking horon-agent.
Lifecycle hooks tell an external command *what is happening* inside a session.

## Configuration

```toml
# ~/.horon/config.toml
[hooks]
turn_start    = "~/bin/horon-agent-turn-busy"       # observer
turn_end      = "~/bin/horon-agent-turn-notify"     # observer
session_start = ""                            # observer
session_end   = ""                            # observer
pre_tool      = "~/bin/horon-agent-tool-policy"     # gate
post_tool     = ""                            # observer
post_tool_feedback = "~/bin/horon-agent-vale"       # feeds output back to the model
pre_request   = "~/bin/horon-agent-request-policy"  # rewrites the outgoing messages
pre_tool_timeout_ms = 5000
post_tool_feedback_timeout_ms = 10000
pre_request_timeout_ms = 5000
```

The nine command keys (`pre_tool_transform`, `turn_start`, `turn_end`,
`session_start`, `session_end`, `pre_tool`, `post_tool`,
`post_tool_feedback`, `pre_request`) accept a command string or an array of
strings. Independent plugins can share one event without a dispatcher.

```toml
[hooks]
turn_end = ["~/bin/horon-agent-turn-notify", "~/bin/horon-agent-event-log"]
pre_tool = ["~/bin/horon-agent-tool-policy", "~/bin/horon-agent-audit-policy"]
```

Empty commands and whitespace-only commands are skipped. Observer commands are
spawned in declaration order but run independently, so their completion order is
not defined. Gate commands run sequentially in declaration order even after a
block has happened, and the first block result is returned. Transformers and the
feedback hook also run sequentially in declaration order, as described below.

Environment variable overrides always win, and an empty value turns off the
configured hook:
`HORON_AGENT_HOOK_TURN_START`, `HORON_AGENT_HOOK_TURN_END`,
`HORON_AGENT_HOOK_SESSION_START`, `HORON_AGENT_HOOK_SESSION_END`,
`HORON_AGENT_HOOK_PRE_TOOL`, `HORON_AGENT_HOOK_POST_TOOL`,
`HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS`,
`HORON_AGENT_HOOK_POST_TOOL_FEEDBACK`,
`HORON_AGENT_HOOK_POST_TOOL_FEEDBACK_TIMEOUT_MS`,
`HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM`,
`HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM_TIMEOUT_MS`,
`HORON_AGENT_HOOK_PRE_REQUEST`,
`HORON_AGENT_HOOK_PRE_REQUEST_TIMEOUT_MS`.
Command overrides accept a command string or a TOML string array. For example,
write `HORON_AGENT_HOOK_TURN_END='["notify-one", "notify-two"]'`.

## Common contract

- Hook command lines are parsed shell-style (quotes and backslash escapes work)
  but executed **directly**, without going through a shell. A leading `~/` in
  the program path is expanded.
- A hook runs in the session working directory when that directory is known.
- Every hook receives the following.

| Variable | Meaning |
| --- | --- |
| `HORON_AGENT_HOOK_EVENT` | `turn_start`, `turn_end`, `session_start`, `session_end`, `pre_tool`, `post_tool`, `post_tool_feedback`, `pre_tool_transform`, `pre_request` |
| `HORON_AGENT_HOOK_SESSION_ID` | the session the event belongs to |
| `HORON_AGENT_HOOK_CWD` | the session working directory |
| `HORON_AGENT_HOOK_PAYLOAD` | JSON object holding every field verbatim (at most 16 KB) |
| `HORON_AGENT_HOOKS_DISABLED` | always `1`. Suppresses hooks in nested horon-agent calls (recursion guard) |

## Observer hooks

`turn_start`, `turn_end`, `session_start`, `session_end`, and `post_tool` are
**observers**. They are spawned detached and the agent does not wait for their
results. The agent never waits for them to finish, and failures are only logged.

### `turn_start`

Fires before model generation and the first tool call. The targets are the `chat`
entry point that uses the streaming turn loop and the one-shot `run` path. Extra
fields are `HORON_AGENT_HOOK_MODEL` and `HORON_AGENT_HOOK_SOURCE` (`chat` or
`run`).

The current implementation does not send separate `resume` or `ambient` source
values. Those names in the stale config comment are not a runtime guarantee.

### `turn_end`

Fires when an agent turn ends (the streaming turn path covers desktop, swarm
worker, and headless sessions, and a one-shot `horon-agent run` counts here too).

Extra fields are `HORON_AGENT_HOOK_STATUS` (`ok`/`error`),
`HORON_AGENT_HOOK_DURATION_MS`, `HORON_AGENT_HOOK_MODEL`,
`HORON_AGENT_HOOK_LAST_ASSISTANT_TEXT` (first 4000 characters),
`HORON_AGENT_HOOK_PROMPT_TOKENS`, `HORON_AGENT_HOOK_INPUT_TOKENS`,
`HORON_AGENT_HOOK_OUTPUT_TOKENS`,
`HORON_AGENT_HOOK_CACHE_READ_TOKENS`,
`HORON_AGENT_HOOK_CACHE_WRITE_TOKENS`, and on failure
`HORON_AGENT_HOOK_ERROR`.

The token fields are summed from the assistant messages of the current turn
(`crates/app-core/src/agent/turn_execution.rs:256-270`). `PROMPT_TOKENS` is the
whole prompt size the provider reported, and it is filled only when present.
`INPUT_TOKENS` is the raw input count the provider reported. These values are not
normalized across providers. This build sends model calls through a single
OpenAI-compatible endpoint (`crates/base/src/provider/openai.rs:16`), so whether
cached input is included in `INPUT_TOKENS` follows how that upstream reports it.
The cache fields are omitted when no assistant message in the turn reports that
metric.

### `session_start` / `session_end`

`session_start` fires when an agent session becomes active, and
`HORON_AGENT_HOOK_SOURCE` is one of `create` (a completely new session),
`attach` (binding an existing session object), or `resume` (restoring by ID).
`session_end` fires on a normal close (`HORON_AGENT_HOOK_SOURCE=close`).

### `post_tool`

Fires after every tool call. Extra fields are below.

| Variable | Meaning |
| --- | --- |
| `HORON_AGENT_HOOK_TOOL_NAME` | the resolved tool name |
| `HORON_AGENT_HOOK_TOOL_CALL_ID` | the provider tool call id |
| `HORON_AGENT_HOOK_TOOL_INPUT` | tool input JSON (at most 16 KB) |
| `HORON_AGENT_HOOK_TOUCHED_PATHS` | newline-separated absolute paths the input names (`file_path`, `path`, `paths`, `apply_patch` headers, ...) |
| `HORON_AGENT_HOOK_STATUS` | `ok` / `error` |
| `HORON_AGENT_HOOK_DURATION_MS` | tool execution time |
| `HORON_AGENT_HOOK_OUTPUT_BYTES` | output size (on success) |
| `HORON_AGENT_HOOK_ERROR` | error message (on failure) |

`HORON_AGENT_HOOK_TOUCHED_PATHS` is derived from the tool input, so it lists the
paths the tool was told to read or write. Files a `bash` command happens to change
are not tracked.

## Feedback hook: `post_tool_feedback`

`post_tool_feedback` runs **synchronously after every successful tool call** and
may add text to the result the model sees. It receives the same environment fields
as `post_tool` and the full tool input JSON on standard input.

- Trimmed **standard output** is appended to the tool result under a
  `[post_tool_feedback]` marker (at most 8 KB per hook). Empty standard output
  adds nothing.
- The exit code is ignored, because linters usually exit non-zero when they report
  findings. Standard error is discarded.
- A timeout (`post_tool_feedback_timeout_ms`, 10 seconds by default) and a spawn
  failure add nothing and are only logged.
- Several hooks run in declaration order, and each hook receives what the previous
  hook left as `HORON_AGENT_HOOK_OUTPUT` (at most 8 KB). The appended text is joined
  into one block.

### The envelope that replaces the result

When standard output is a JSON object carrying at least one of `content`,
`is_error`, or `note`, the hook decides the result the model sees instead of
appending text.

| Key | Type | Effect |
| --- | --- | --- |
| `content` | string | replaces the result text entirely (at most 8 KB) |
| `is_error` | boolean | marks the result as an error (`tool_result.is_error`) |
| `note` | string | appended after the result as a separate `<system-reminder>` block |

For example, redact a secret and tell the model.

```bash
#!/bin/sh
# ~/bin/horon-agent-redact
cat >/dev/null
printf '%s' '{"content":"(output redacted)","note":"the raw output held a credential"}'
```

```toml
[hooks]
post_tool_feedback = "~/bin/horon-agent-redact"
```

A JSON object without those three keys, and output that is not JSON, is appended
as before. This hook only changes the result the model sees. File state and the
undo journal are unaffected.

Keep it fast and filter early. This hook runs on every tool call, reads included.

```bash
#!/usr/bin/env bash
# ~/bin/horon-agent-vale: lint prose the agent just wrote
case "$HORON_AGENT_HOOK_TOOL_NAME" in
  write|edit|multiedit|apply_patch) ;;
  *) exit 0 ;;
esac
while IFS= read -r path; do
  case "$path" in
    *.md|*.mdx|*.txt|*.rst) [ -f "$path" ] && vale --output=line "$path" ;;
  esac
done <<<"$HORON_AGENT_HOOK_TOUCHED_PATHS"
exit 0
```

## Gate hook: `pre_tool`

`pre_tool` runs **synchronously before every tool call** and can block the call.

- The hook receives `HORON_AGENT_HOOK_TOOL_NAME` and the full tool input JSON on
  **standard input** (and a copy truncated at 16 KB as
  `HORON_AGENT_HOOK_TOOL_INPUT`).
- **Exit code 0**: allow the call.
- **Exit code 2**: block the call. The hook's standard error (trimmed, at most 2000
  characters) comes back to the model as the tool error, so the model can react.
- **Anything else fails open** and logs a warning. That covers other exit codes, a
  timeout (`pre_tool_timeout_ms`, 5 seconds by default), a missing binary, and a
  spawn error.

Failing open is deliberate. A broken policy script should degrade to "no policy"
instead of breaking every session. If you need fail-closed behavior, make the hook
itself robust. The trust boundary is the hook, not horon-agent.

## Tool input transformers: `pre_tool_transform`

A transformer is a synchronous external plugin that can validate and replace a
tool's JSON input before it runs. Configure one or more, in order.

```toml
[hooks]
pre_tool_transform = ["~/.horon/plugins/rtk-transform"]
pre_tool_transform_timeout_ms = 500
```

The plugin in this example is an external program run over standard input and
output, unrelated to the removed native plugin system. Hooks still work by
spawning a separate process like this.

Each transformer receives the current full tool input JSON on standard input and
`HORON_AGENT_HOOK_TOOL_NAME`, `HORON_AGENT_HOOK_SESSION_ID`, and
`HORON_AGENT_HOOK_CWD` in the environment. It may print a replacement JSON object
on standard output. Empty standard output, invalid JSON, a non-zero exit code, a
spawn error, or a timeout leaves the input unchanged. This fail-open contract makes
an optional integration safe to switch on.

Transformers compose in configuration order and run before the `pre_tool` policy
gate, so a policy evaluates the final transformed input. Keep plugins small and
tool-specific. For example, the RTK plugin should change `bash.command` only when
`rtk rewrite` returns a replacement string.

### Example policy script

```bash
#!/usr/bin/env bash
# ~/bin/horon-agent-tool-policy
# stdin: tool input JSON. Env: HORON_AGENT_HOOK_TOOL_NAME, HORON_AGENT_HOOK_SESSION_ID...
input=$(cat)

case "$HORON_AGENT_HOOK_TOOL_NAME" in
  bash)
    if grep -qE 'rm -rf /([^a-zA-Z]|$)|mkfs|dd if=' <<<"$input"; then
      echo "blocked: destructive shell command" >&2
      exit 2
    fi
    ;;
  write|edit)
    if grep -q '"file_path":"/etc/' <<<"$input"; then
      echo "blocked: writes to /etc are not allowed" >&2
      exit 2
    fi
    ;;
esac
exit 0
```

## Request hook: `pre_request`

`pre_request` runs **synchronously before every model request of an agent turn**
and can change the messages that request sends. The stored transcript is unchanged.
Auxiliary calls such as compaction and consultation handle their own prompt rather
than the conversation, so they do not go through this hook.

- Standard input carries `{"session_id": ..., "turn": ..., "model": ..., "messages": [...]}`.
  `messages` has the same shape as a `/chat/completions` request body, and when a
  system prompt exists, `{"role":"system","content":"..."}` is the first entry.
- Empty standard output means the request goes out unchanged.
- When standard output is `{"messages": [...]}`, the messages are replaced for that
  request only. The messages the request was built from and the stored transcript
  stay as they are.
- The system prompt entry can be neither dropped nor changed. If the first entry
  differs from the request's system prompt, the whole replacement is rejected and
  the original messages are used.
- Invalid JSON, a non-zero exit code, a timeout (`pre_request_timeout_ms`, 5 seconds
  by default), and a spawn failure keep the original messages and log one warning
  (fail open).
- Several commands run in declaration order, and each command receives the messages
  the previous command left.

```bash
#!/bin/bash
# ~/bin/horon-agent-request-policy: mask a path in everything the model reads
python3 -c '
import json, sys
payload = json.load(sys.stdin)
for message in payload["messages"]:
    for block in message.get("content", []):
        if block.get("type") == "text":
            block["text"] = block["text"].replace("/Users/alice", "/home/user")
json.dump({"messages": payload["messages"]}, sys.stdout)
'
```

```toml
[hooks]
pre_request = "~/bin/horon-agent-request-policy"
pre_request_timeout_ms = 5000
```

A request path with no hook configured spawns no process and copies no message. It
checks whether a hook is configured first and builds the payload only when needed.

## Example: tmux status and a desktop notification at turn end

```bash
#!/usr/bin/env bash
# ~/bin/horon-agent-turn-notify
if [ "$HORON_AGENT_HOOK_STATUS" = ok ]; then icon=✅; else icon=❌; fi
tmux display-message "horon-agent $icon ${HORON_AGENT_HOOK_SESSION_ID:0:12}" 2>/dev/null
notify-send "horon-agent turn $HORON_AGENT_HOOK_STATUS" \
  "${HORON_AGENT_HOOK_LAST_ASSISTANT_TEXT:0:120}" 2>/dev/null
exit 0
```

## Example: log every hook activity to a JSON event log

Wire several hooks to one script and branch on `HORON_AGENT_HOOK_EVENT`.

```bash
#!/usr/bin/env bash
# ~/bin/horon-agent-event-log
echo "$HORON_AGENT_HOOK_PAYLOAD" >> ~/.local/state/horon-agent-events.jsonl
```

```toml
[hooks]
turn_end      = "~/bin/horon-agent-event-log"
session_start = "~/bin/horon-agent-event-log"
session_end   = "~/bin/horon-agent-event-log"
post_tool     = "~/bin/horon-agent-event-log"
```

## Design notes

- Hook lookup is configuration-based and re-resolved whenever configuration is
  read. You can add or change hooks without restarting horon-agent.
- Hot paths (`pre_tool`/`post_tool`/`pre_request`) check whether a hook is
  configured before building a payload, so an unset hook costs almost nothing.
- The recursion guard (`HORON_AGENT_HOOKS_DISABLED=1`) lets a hook call
  `horon-agent` CLI commands without triggering hooks again in the nested process.