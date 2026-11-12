---
title: CLI
description: "Every horon subcommand: serve, run, repl, api-bridge, acp, sessions, providers, cloud move."
section: Using
order: 1
---

Bare `horon` on a terminal starts the server and then replaces itself with the
`horon-tui` client beside it. A pipe, `HORON_NO_TUI=1`, or a missing sibling keeps
the headless path.

## Core commands

| Command | What it does |
| --- | --- |
| `horon run "<message>"` | Send one message, run tools until the turn ends, exit. `--json` prints a machine-readable result, `--ndjson` streams events. |
| `horon repl` | Simple line-based REPL, no TUI. |
| `horon serve` | Background agent daemon on a Unix socket (`HORON_AGENT_SOCKET`). |
| `horon api-bridge` | Harness API on a Unix socket for SDK clients (`HORON_AGENT_API_SOCKET`, Unix only). |
| `horon acp` | ACP v1/v2 adapter on stdio for editors. |
| `horon server start\|reload\|stop\|promote` | Manage the daemon. Prefer `reload` after an upgrade: it hands live sessions to a freshly exec'd server instead of dropping them. `stop` requires `--force`. |

`api-bridge` listens on `$XDG_RUNTIME_DIR/horon-agent-api.sock` by default;
`--api-socket <path>` moves it, and `--stdio` serves one connection on
stdin/stdout for SSH clients. The global `--socket` selects the *internal daemon*
socket, which is a different socket.

## Providers and models

| Command | What it does |
| --- | --- |
| `horon provider list` | Provider ids you can pass to `-p/--provider`. |
| `horon provider current` | The currently requested and resolved provider selection. `--json` reports `"ready"`. |
| `horon provider add <name> <base-url> <model>` | Add an OpenAI-compatible profile. `--api-key-stdin` keeps the key out of shell history. |
| `horon model list` | Model names you can pass to `-m/--model`. |

## Sessions

`horon session` (alias `horon sessions`) works on the event store:
`inspect`, `doctor`, `migrate`, `export`, `import`, `export-legacy`,
`recover-to`, `rename`, `archive`, `restore`.

`horon export <session> [--out FILE] [--json]` prints a Markdown transcript.
Secrets are redacted and tool results over 4 KiB are truncated; the output matches
the `export_session_markdown` harness API reply.

## Unattended runs

| Command | What it does |
| --- | --- |
| `horon runtime-state show --session <id>` | List the live inputs of a session and which ones are undetermined. |
| `horon runtime-state resolve-input --session <id> --input <id> --action retry\|cancel` | Settle an input whose outcome is unknown. |
| `horon approvals resolve --request-id <id> --tool-call-id <id> --verdict retry\|skip` | Record a human verdict for a run that stopped waiting on a person. |

## Other commands

| Command | What it does |
| --- | --- |
| `horon install-release --assets <dir> --agent-version <version>` | Verify and install a downloaded release. `--source-binary <path>` installs a local build through the same path. |
| `horon update` | Update horon-agent to the latest version. |
| `horon version [--json]` | Build and version information. |
| `horon usage [--json]` | Usage limits for connected providers. |
| `horon mcp` | List configured MCP servers and manage remote-server OAuth credentials. |
| `horon memory` | Memory management. |
| `horon cloud move\|return\|where\|attach` | Move a live session to a cloud host and bring its work back. |
| `horon pair [--list] [--revoke <name>]` | Pairing codes for the iOS and web clients. |
| `horon permissions` / `horon ambient` | Review pending ambient permission requests. |
| `horon debug <command> [arg]` | Talk to a running daemon over the debug socket. |

## Editors (ACP)

Register `horon` (or `horon-agent`) with the argument `acp` as an external agent.
In Zed (`settings.json`):

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

JetBrains, Neovim and VS Code ACP plugins take the same two values. The `initialize`
response advertises `authMethods` with one method, `horon-agent-credentials`,
which checks whether the credentials of a `[providers.<name>]` profile can be used
to call models. Register a profile with `horon provider add`; this build has no
login command for the editor to run in a terminal.
