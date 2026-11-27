# Editors (ACP)

`horon acp` speaks the Agent Client Protocol on stdio, versions 1 and 2, and answers `initialize` with the highest version both sides support.

## Register Horon

Give the editor the command and the argument `acp`. Zed (`settings.json`):

```json
{
  "agent_servers": {
    "horon": { "type": "custom", "command": "/absolute/path/to/horon", "args": ["acp"] }
  }
}
```

JetBrains, Neovim and VS Code ACP plugins take the same two values. Authentication checks that a `[providers.<name>]` profile can call models; add one with `horon provider add` first ([providers.md](providers.md)). There is no login command for the editor to run.

## What the editor controls

| Option | Values |
| --- | --- |
| `mode` | `default` (follow `approval_mode`), `ask-commands` (ask before shell, network and file changes), `auto` (no asking) |
| `model` | Models the profile offers |
| `reasoning_effort` | The effort ladder |

Permission answers: `allow-once`, `allow-always` (this tool, this session), `reject-once`; version 2 adds `allow-path` and `allow-folder` scoped grants.

## Defaults for editor sessions

- Tool profile `acp` (`[acp] tool_profile`): every allowed tool is sent eagerly, no `toolbox` list.
- `[acp] profile = standard` controls the extra surfaces; `extended` and `full` add more.
- The editor's open buffers are read instead of the disk copy when the editor serves the file; foreground non-PTY `bash` calls can run in the editor's terminal.
- The editor may pass its own MCP servers per session.

## Limits

| Case | Result |
| --- | --- |
| A non-null `systemPrompt` | Rejected with `-32602`; omit it or send `null` |
| `logout` on a version 1 connection | Not supported |
| An endpoint without streaming | The answer appears all at once |
| `session/delete` on a busy or attached session | Refused; close it first |
