---
title: Editors (ACP)
description: Serve ACP v1 and v2 over stdio to Zed, JetBrains, Neovim and VS Code, with the capability and error contract.
section: Using
order: 4
---

`horon acp` is an Agent Client Protocol adapter on stdio, backed by the same
daemon as every other command. It serves protocol versions 1 and 2 and answers
`initialize` with the highest version both sides support.

## Registering the agent

Two values: the command and the argument `acp`.

```json
{
  "agent_servers": {
    "horon agent": {
      "type": "custom",
      "command": "/absolute/path/to/horon",
      "args": ["acp"]
    }
  }
}
```

That is Zed's `settings.json`. The ACP plugins for JetBrains, Neovim and VS Code
take the same two values; the setting key differs per plugin.

## Authentication

| id | Offered when | What it does |
| --- | --- | --- |
| `horon-agent-credentials` | Always | Checks whether the credentials of a `[providers.<name>]` profile can be used to call models |

Register a profile with `horon provider add`. Even when the client advertises
`auth.terminal`, there is only one method: this build has no login command for the
editor to run in a terminal.

`authenticate` succeeds with `{}` when the daemon reports a usable model and
returns `-32000` when it does not; `session/new` returns `-32000` in the same
situation. An unknown `methodId` is `-32602`. `logout` is not advertised because
there is no way to erase credentials.

## Advertised capabilities

| Item | Value |
| --- | --- |
| `loadSession` | `true` |
| `promptCapabilities` | `image: true`, `embeddedContext: true`, `audio: false` |
| `mcpCapabilities` | `http: true`, `sse: true` (stdio is always supported) |
| `sessionCapabilities` | `close`, `resume`, `list`, `delete`, `additionalDirectories` |

A method that calls an unadvertised capability is refused with `-32601`; so is an
unknown request starting with `_`. Unknown notifications are ignored, and
notifications never get a response. Any request sent before `initialize` is
`-32600`, and a `protocolVersion` that is missing or outside the integer range
0–65535 is `-32602`.

## Version 2

v2 is additive for clients that negotiate it: `session/prompt` acknowledges with a
`messageId` and progress arrives as `state_update` notifications, `session/load`
becomes `session/resume` with `replayFrom`, `authenticate`/`logout` become
`auth/login`/`auth/logout`, `session/set_mode` becomes
`session/set_config_option`, and capability markers are objects instead of
booleans. v1 behaves exactly as before.
