# Tools

## How a session gets its tools

A session resolves one load tier per tool at start and keeps it until the session ends.

| Tier | Meaning |
| --- | --- |
| `essential` | Schema sent in every request; call it directly |
| `discoverable` | Named in the system prompt's `# Additional tools` list; reach it through `toolbox` |
| `off` | Not available |

The default `core` profile reaches every built-in tool. It sends these eleven eagerly: `read`, `write`, `edit`, `apply_patch`, `bash`, `ls`, `agentgrep`, `todo`, `bg`, `websearch`, `webfetch` (plus `eval` when Python is on `PATH`, plus `toolbox`). Everything else is listed by name.

| Profile | What the model gets |
| --- | --- |
| `core` (default) | Eleven eager tools, `toolbox`, and a list of the rest |
| `full` | Every built-in tool; non-core ones as deferred definitions |
| `acp` | Default for `horon acp`; every allowed tool eager, no list |
| `minimal` / `lite` | A small eager set |
| `none` | No tools |

An explicit allow-list (`[tools] enabled`, an SDK `enabled` list) also sends every allowed tool eagerly with no list.

## Using `toolbox`

| Action | Use |
| --- | --- |
| `{"action":"list"}` | Names and one-line descriptions of the listed tools |
| `{"action":"describe","tool":"ast_grep"}` | Full schema and a call example |
| `{"action":"run","tool":"ast_grep","args":{"pattern":"foo($A)","lang":"python"}}` | Run it; the tool's own arguments go inside `args` |

| Case | What happens |
| --- | --- |
| Arguments written beside `action` instead of inside `args` | Read as `args` |
| `args` sent as a JSON string | Parsed as an object |
| `args` missing, the tool has no required field | Runs with `{}` |
| `args` missing, the tool needs fields | Refused with the required fields, an example and the schema; from the next request that tool is sent directly with its own schema |
| The same failing call repeated | The loop guard refuses it after a few identical tries ([turns.md](turns.md)) |

The listed set is fixed at the first request and survives `/clear`; only the promotion above changes it.

## Built-in tools

| Tool | Purpose | Page |
| --- | --- | --- |
| `read`, `write`, `edit`, `replace`, `apply_patch`, `hashline_edit`, `ast_edit`, `undo`, `redo`, `ls` | Read and change files | [editing.md](editing.md) |
| `agentgrep`, `ast_grep`, `lsp`, `websearch`, `webfetch`, `session_search`, `conversation_search` | Find code, text, web pages, past work | [search-and-web.md](search-and-web.md) |
| `bash`, `bg`, `monitor`, `eval`, `codemode`, `batch` | Run commands and programs | [shell.md](shell.md) |
| `todo`, `goal`, `context_notes`, `new_context`, `checkpoint`, `rewind` | Track work and manage context | [turns.md](turns.md) |
| `ask` | Ask the user a question with options | [safety.md](safety.md) |
| `skill_manage` | Create and edit skills | [skills.md](skills.md) |
| `mcp`, `mcp_search`, `mcp_call` | MCP servers and their tools | [mcp.md](mcp.md) |
| `memory` | Durable memories | [memory.md](memory.md) |
| `swarm` | Sub-agents and task graphs | [swarm.md](swarm.md) |
| `irc` | Messages between agents of one daemon | [irc.md](irc.md) |
| `generate_image` | Images through the profile's `image` role (registered only when the profile has one; `[tools] generate_image = false` removes it) | [providers.md](providers.md) |
| `horon` | Search these docs; read one with `read horon://<page>` | [read-addresses.md](read-addresses.md) |

## Arguments the tools forgive

- An empty string for an optional argument (`""` or spaces) counts as omitted: `bash` `name`/`notify_pattern`, `agentgrep` `path`/`glob`/`type`/`file`, `hashline_edit` `move_to`/`end_anchor`, `edit` shorthand next to `edits`.
- Malformed arguments with an obvious fix are repaired before the call runs, and the repair is recorded in the result. Some alternate field names are accepted outright: `ast_grep` takes `path` or `paths`, one string or an array.
- Old tool names still resolve: `horon_agent_docs` and `documentation` run `horon`.

## Changing the tool set

`[tools] disabled = ["name"]` removes a tool. `[tools.load_mode] name = "essential"|"discoverable"|"off"` overrides its tier. `toolbox = "off"` turns listing off so every reachable tool is eager. Each optional tool also has its own switch ([configuration.md](configuration.md)).
