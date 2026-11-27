# Hooks

Hooks run your own programs on agent events. Configure them under `[hooks]` in `config.toml`; each key takes a command or an array of commands.

```toml
[hooks]
turn_end = ["~/bin/notify-done", "~/bin/event-log"]
pre_tool = "~/bin/tool-policy"
post_tool_feedback = "~/bin/lint-changed-file"
pre_tool_timeout_ms = 5000
```

## Events

| Key | Runs | Can change behavior |
| --- | --- | --- |
| `session_start`, `session_end`, `turn_start`, `turn_end`, `post_tool` | After the event | No (observer; runs in the background) |
| `pre_tool` | Before every tool call, synchronously | Exit `0` allows, exit `2` blocks; its stderr (up to 2,000 chars) becomes the tool error the model sees |
| `pre_tool_transform` | Before a call, synchronously | Rewrites the tool input |
| `post_tool_feedback` | After every successful call | Its stdout (up to 8 KB) is appended to the result under `[post_tool_feedback]` |
| `pre_request` | Before each model request | Rewrites the outgoing messages |

Any other exit code, a timeout or a missing program fails open: the call proceeds and a warning is logged.

## What a hook receives

| Variable | Value |
| --- | --- |
| `HORON_AGENT_HOOK_EVENT` | The event name |
| `HORON_AGENT_HOOK_SESSION_ID` | The session |
| `HORON_AGENT_HOOK_CWD` | The session working directory (also the hook's cwd) |
| `HORON_AGENT_HOOK_PAYLOAD` | All fields as JSON (up to 16 KB) |
| `HORON_AGENT_HOOK_TOOL_NAME`, stdin | For tool hooks: the tool name and the full input JSON |
| `HORON_AGENT_HOOKS_DISABLED=1` | Set so a `horon` started by the hook runs no hooks |

Commands are split shell-style but run without a shell; a leading `~/` is expanded. Each key has an environment override such as `HORON_AGENT_HOOK_PRE_TOOL`; an empty value turns that hook off.

## Order

Observers start in declaration order and finish in any order. Gates, transformers and feedback hooks run one after another in declaration order; the first block wins.
