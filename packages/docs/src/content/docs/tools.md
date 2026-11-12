---
title: Tools
description: The 18-tool core set, three loading tiers, deferred loading through toolbox, and edit safety.
section: Using
order: 3
---

## The core tool set

The `core` profile allows eighteen tools: `bash`, `read`, `write`, `edit`,
`replace`, `apply_patch`, `agentgrep`, `ls`, `webfetch`, `websearch`,
`skill_manage`, `mcp`, `monitor`, `undo`, `ask`, `todo`, `session_search` and
`swarm`.

Seven of them are sent eagerly with schemas in every session (`read`, `write`,
`edit`, `apply_patch`, `bash`, `ls`, `agentgrep`), because almost every coding
turn touches them. The other eleven are named in a system-prompt index and reached
through `toolbox`, which keeps the first request small and leaves the prompt cache
stable when a tool is added or removed.

Profiles: `core` (default), `full`, `acp`, `minimal`/`lite`, `none`. Choose one
with `--tool-profile`, `HORON_AGENT_TOOL_PROFILE`, or `[tools] profile`.

## Three loading tiers

| Tier | Meaning |
| --- | --- |
| `essential` | Sent eagerly and callable with no discovery step. The default. |
| `discoverable` | Sent as a deferred definition. The schema reaches the model once a tool reference names it; providers without native deferred loading drop the definition instead, and a `core` session names it in the index. |
| `off` | Not sent at all. |

Override one tool with `[tools.load_mode]`:

```toml
[tools.load_mode]
batch = "off"
memory = "essential"
ask = "essential"
```

`toolbox = "off"`, or adding `toolbox` to `[tools] disabled`, turns listing mode
off and puts all eighteen core tools back on the eager surface. An explicit
`discoverable` entry for `toolbox` is promoted to `essential`, because listing
would otherwise create a tool with no way to reach it.

Sessions that are not listing sessions send every core tool eagerly and get no
index: `acp`, `minimal`/`lite`, `none`, an explicit allow-list, and an SDK
`enabled` list. A session fixes its tiers once at start; reloading configuration
never moves a tool between tiers mid-session.

## The listing index

A listing `core` session's system prompt gains an `# Additional tools` section:
one guidance sentence on calling a listed tool through `toolbox run`, then one
`- <name>: <description>` row per tool. A row carries only the first sentence of
the tool description, capped at 60 characters; the model fetches parameters with
`toolbox describe`.

The index freezes when the first tool list is locked and does not change
afterwards. `/clear` keeps it because it is the same session; only a session
restore rebuilds it.

## Edit safety

- A journal backs `undo` for file edits; `redo` and hashline anchors are opt-in
  (`[tools] redo`, `[tools] hashline_edit`).
- `apply_patch` is the multi-file patch tool; `replace` is the single-string edit.
- Tool output over the condense threshold is summarized rather than dropped, and
  `read` deduplicates content it has already returned in the session.

## MCP and skills

`horon mcp` lists configured MCP servers and manages remote-server OAuth
credentials. Everything external comes in over MCP. Project-local MCP config is
skipped until its digest is trusted (`--trust-workspace` records the decision).

Skills are read through `skill://` and managed with `skill_manage`; the workspace
convention is `.horon/skills/<name>/SKILL.md`.
