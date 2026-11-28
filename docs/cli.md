# Command line

`horon`, `hrn` and `horon-agent` are the same program. `horon --help` and `horon <command> --help` list every flag; this page says which command answers which need.

## Run the agent

| Need | Command | Notes |
| --- | --- | --- |
| Terminal client | `horon` | Hands off to `horon-tui`; see [overview.md](overview.md) for the conditions |
| One task, then exit | `horon run "<message>"` | Exits `1` on a guardrail stop, `length`, or an empty turn. Up to 6 follow-up turns continue unfinished todos (`HORON_AGENT_RUN_AUTO_POKE=0` disables). `--max-cost <USD>` stops at that spend, `--warn-cost <USD>` warns once |
| Line prompt | `horon repl` | No full-screen client |
| Daemon | `horon serve` | `--server-name <name>` gives it a stable identity |
| Daemon control | `horon server start\|reload\|stop\|promote` | `reload` keeps live sessions; `stop` needs `--force` |
| Editor | `horon acp` | [editors.md](editors.md) |
| Host program | `horon rpc` (or `--mode rpc`) | [rpc.md](rpc.md) |
| SDK socket | `horon api-bridge` (alias `api`) | [harness-api.md](harness-api.md) |
| Resume a session | `horon --resume <id>` | [sessions.md](sessions.md) |

Common global flags: `-m/--model <id>`, `--provider-profile <name>`, `-p/--provider <id>` (a built-in provider id, default `auto`), `--tool-profile <core|full|acp|minimal|none>`, `--isolated-profile <name>` (separate config, sessions, credentials and sockets), `--trust-workspace`, `-C/--cwd <dir>`, `--resume [<id>]`, `--continue` (latest session), `--quiet`. `horon run` adds `--role <role>=<model>`, `--prewalk <model|role:name>`, `--no-prewalk`, `--thinking`, `--max-cost`, `--warn-cost`.

## Providers and models

| Command | Purpose |
| --- | --- |
| `horon provider add <name> --type <type> --base-url <url> --model <id>` | Add a profile ([providers.md](providers.md)) |
| `horon provider current [--json]` | Requested and resolved selection; `"ready": true` when usable |
| `horon provider status [--json]` | Credential source per profile, without calling providers |
| `horon provider diagnose [--check]` | Check profiles; `--check` queries model lists |
| `horon provider failover` | Preview retry and failover order |
| `horon provider list` | Ids accepted by `-p` |
| `horon model list [--refresh] [--profile <name>]` | Model ids accepted by `-m` |
| `horon usage` / `horon stats [--from] [--through]` | Provider limits / stored local token usage |
| `horon tokens [TEXT\|--file PATH]` (alias `toks`) | Local token estimate |

## Sessions and unattended runs

| Command | Purpose |
| --- | --- |
| `horon session inspect\|doctor\|migrate\|rename\|archive\|restore\|delete\|export\|import\|recover-to` | Work on the event store ([sessions.md](sessions.md)) |
| `horon export <session> [--html] [--out FILE]` | Redacted Markdown or offline HTML transcript |
| `horon runtime-state show --session <id>` | Live inputs and which are undetermined |
| `horon runtime-state resolve-input --session <id> --input <id> --action retry\|cancel` | Settle an undetermined input |
| `horon approvals resolve --request-id <id> --tool-call-id <id> --verdict retry\|skip` | Answer a run that stopped for a person |

## Configuration, tools and extensions

| Command | Purpose |
| --- | --- |
| `horon config show\|check [--json]` | Effective configuration / validate `config.toml` (exit `1` on bad TOML, unknown keys, unknown default provider) |
| `horon mcp list\|test\|login\|logout <server>` | MCP servers and remote OAuth ([mcp.md](mcp.md)) |
| `horon skills ...` | List skills with provenance, install from an approved source ([skills.md](skills.md)) |
| `horon package install\|list\|verify\|disable\|remove` | Rust extension and plugin packages ([packages.md](packages.md)) |
| `horon memory list\|search\|export\|import\|stats` / `horon memory migrate` | Stored memories ([memory.md](memory.md)) |
| `horon preview <read\|agentgrep\|websearch> <json>` | Run one read-only tool without a model |
| `horon diagnostics <files...>` | Language-server diagnostics |
| `horon agents` | Named agent definitions ([swarm.md](swarm.md)) |
| `horon api-credentials issue\|rotate\|revoke\|list` | Tokens for `api-bridge --api-bind` |
| `horon volume ...` | Back up, restore, delete this user's data ([hosting.md](hosting.md)) |

## Maintenance

`horon install-release`, `horon update`, `horon version [--json]`, `horon changelog [VERSION]`, `horon completions bash|zsh|fish`, `horon images ...` (generated images), `horon stream-rules [TEXT]`, `horon compact-text <FILE>`, `horon debug <command>`, `horon cloud move|return|where|attach|doctor`, `horon pair`, `horon permissions`, `horon ambient status|log|trigger|stop`.
