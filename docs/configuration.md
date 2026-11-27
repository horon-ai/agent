# Configuration

## Where things live

| Item | Location |
| --- | --- |
| Data home | `~/.horon`, or `HORON_HOME` |
| Main config | `<home>/config.toml` |
| MCP servers | `<home>/mcp.json`, plus project `.horon/mcp.json`, `.mcp.json`, `.claude/mcp.json` ([mcp.md](mcp.md)) |
| Sessions | `<home>/sessions/<id>/` |
| Memory | `<home>/memory/` ([memory.md](memory.md)) |
| Skills | `<home>/skills/`, project `.horon/skills/` ([skills.md](skills.md)) |
| Packages | `<home>/packages/` ([packages.md](packages.md)) |
| Terminal client files | see [tui.md](tui.md) |
| Daemon sockets | the runtime directory: `HORON_AGENT_RUNTIME_DIR`, else `XDG_RUNTIME_DIR`, else the per-user temp directory (`$TMPDIR` on macOS); named pipes on Windows |

`--isolated-profile <name>` keeps config, sessions, credentials and daemon sockets in a separate local profile.

## Validate before launch

`horon config check [--json]` exits `1` on malformed TOML, an unknown key, or a `[provider] default_provider` that names no profile. `horon config show` prints the effective configuration. A configuration change is picked up by the next session; a running session keeps its tool tiers until it ends.

## Defaults that matter

| Setting | Default | Other values |
| --- | --- | --- |
| Tool profile (`--tool-profile`, `HORON_AGENT_TOOL_PROFILE`, `[tools] profile`) | `core` | `full`, `acp`, `minimal`/`lite`, `none` |
| `[acp] profile` | `standard` | `extended`, `full` |
| `[tools] redo`, `hashline_edit`, `codemode`, `lsp`, `ast_tools`, `context_notes`, `checkpoint` | `true` | `false` removes the tool |
| `[tools] irc` | `true` | `false`; `HORON_AGENT_IRC=0` removes it for one process |
| `[tools] eval` | needs Python 3.10+ on `PATH`; each cell is approved like `bash` | `[tools] disabled = ["eval"]` |
| `[tools] ask_policy` | `recommended` (an unanswered question takes the recommended option) | `error` (refuse; the model decides alone) |
| `[tools] bash_pty` | `auto` | `off`, `always` |
| `[features] crash_bundles` | `true` | `false` |
| `[features] turn_recovery` | `conservative` | `aggressive`, `off` |
| `[features] effort_keywords` | `true` | `false` |
| `[features] foreign_rules`, `stream_rules`, `repeat_guard` | `false` | `true` |
| `[features] pricing_refresh` | `false` | `true`; `HORON_AGENT_PRICING_REFRESH=1\|0` per process |
| `[prewalk] enabled` | `false` | `true`, which needs `into` (a model id or `role:<name>`) |
| `[storage] sync` | `full` (power-loss durable) | `fast` (crash durable); `HORON_AGENT_STORAGE_SYNC` per process |
| `[server] idle_hibernation_secs` | `1800` | `0` disables |

## Turning one tool off or forcing it eager

```toml
[tools]
disabled = ["generate_image"]

[tools.load_mode]
memory = "essential"   # send its schema in every request
batch = "off"          # never send it
```

See [tools.md](tools.md) for tiers.

## Environment-only settings

| Variable | Effect |
| --- | --- |
| `HORON_HOME` | Data home |
| `HORON_NO_TUI` | Bare `horon` stays headless |
| `HORON_AGENT_WORKSPACE_TRUST=trust\|deny` | Trust decision for project-local MCP config for one process |
| `HORON_AGENT_JOBS_ARTIFACT_RETENTION=rotate\|preserve` | `rotate` (default) reclaims the oldest background-task artifacts when full; `preserve` refuses new tasks instead |
| `HORON_AGENT_HOSTING_MODE`, `HORON_AGENT_TENANT_ID` | Hosted modes ([hosting.md](hosting.md)) |
| `HORON_AGENT_NO_AUTO_UPDATE` | Keeps the agent's own updater off (set by the npm launcher) |
| `HORON_AGENT_RUN_AUTO_POKE=0` | `horon run` does not continue unfinished todos |
| `HORON_AGENT_API_BIND`, `HORON_AGENT_API_INSECURE_BEHIND_PROXY` | TCP listener of `api-bridge` ([harness-api.md](harness-api.md)) |

Provider keys and profiles are covered in [providers.md](providers.md).
