# Horon documentation

These pages are compiled into the binary and describe the build that is running. Read one with `read horon://<page>` (for example `read horon://tools.md`), list them with `read horon://`, and search them with the `horon` tool. Nothing here needs network access.

The developer reference (internal design, CI, benchmarks, crate layout) lives on the documentation site at https://agent.horon.ai/reference; these pages cover what a user or an agent running inside Horon needs.

## Reading rules

- When a page and the behavior you observe disagree, the behavior wins: report the mismatch instead of following the page.
- Each page states what happens in each case; the `Read when` column below says which page answers which question.
- Commands are written as `horon <subcommand>`; `horon-agent` and `hrn` are the same program.

## Pages

| Page | Read when |
| --- | --- |
| [overview.md](overview.md) | You need the shape of Horon: modes, binaries, daemon, sessions |
| [install.md](install.md) | Installing, updating, reinstalling, or running on a server |
| [cli.md](cli.md) | Looking for a subcommand or a flag |
| [configuration.md](configuration.md) | Editing `config.toml`, data directories, environment variables, defaults |
| [providers.md](providers.md) | Connecting a model endpoint, keys, model roles, presets, model errors |
| [tools.md](tools.md) | Which tools exist, how a session loads them, `toolbox` |
| [editing.md](editing.md) | Changing files: `read`, `write`, `edit`, `apply_patch`, hashline and AST edits, undo |
| [search-and-web.md](search-and-web.md) | Finding code or text: `agentgrep`, `ast_grep`, `lsp`, web search and fetch, history search |
| [shell.md](shell.md) | Running commands: `bash`, background tasks, `monitor`, `eval`, `codemode`, `batch` |
| [read-addresses.md](read-addresses.md) | `read` of `skill://`, `horon://`, `memory://`, `artifact://`, `history://`, archives, databases, images |
| [skills.md](skills.md) | Finding, reading, installing, or writing a skill |
| [mcp.md](mcp.md) | Connecting MCP servers, workspace trust, calling server tools |
| [memory.md](memory.md) | Saving and recalling facts across sessions |
| [sessions.md](sessions.md) | Resuming, forking, exporting, archiving, deleting sessions |
| [turns.md](turns.md) | What a turn does: recovery, interrupts, compaction, effort, goals, spend caps, the loop guard |
| [tui.md](tui.md) | The terminal client: keys, slash commands, scrolling, selection, status line |
| [safety.md](safety.md) | Approvals, plan mode, destructive commands, secrets, workspace trust |
| [swarm.md](swarm.md) | Delegating work to sub-agents and task graphs |
| [irc.md](irc.md) | Messaging between agents in one daemon |
| [hooks.md](hooks.md) | Running your own commands on agent events |
| [packages.md](packages.md) | Installing or writing Rust extension and plugin packages |
| [editors.md](editors.md) | Using Horon from an editor over ACP |
| [rpc.md](rpc.md) | Driving Horon from a host program over stdio JSONL |
| [harness-api.md](harness-api.md) | The SDKs, `api-bridge`, and remote API credentials |
| [hosting.md](hosting.md) | Running one Horon per customer in a container |
| [troubleshooting.md](troubleshooting.md) | An error message or a stuck session |
