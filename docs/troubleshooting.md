# Troubleshooting

Find the message or symptom, then follow the fix. Each row links to the page with the full rules.

## Starting and connecting

| Symptom | Cause | Fix |
| --- | --- | --- |
| `horon` prints a reason and exits `1` | Not a terminal, `HORON_NO_TUI` set, or no `horon-tui` beside the binary | Run in a terminal, unset the variable, or install the client ([install.md](install.md)) |
| `terminal too small` | Less than 20×6 cells | Enlarge the window |
| The client lost the server after an upgrade | The daemon was replaced | Close the client and run `horon` again; next time use `horon server reload` |
| `horon --version` shows an unexpected number | Development builds number from git history | Compare `readlink <home>/builds/current/horon-agent` with the installed version folder |

## Model calls

| Message | Fix |
| --- | --- |
| The call names a missing `[providers.<name>]` table | Add a profile ([providers.md](providers.md)) |
| `Unknown provider profile` | Match `default_provider` / `--provider-profile` to a table name |
| 401 / 403 / 404 / model not found | See the table in [providers.md](providers.md) |
| `an image in the conversation could not be processed` | A broken image is stored in the session; start a new session. New reads refuse corrupt PNG/JPEG before sending ([read-addresses.md](read-addresses.md)) |
| `Unsupported ... model` when spawning a child | Pass `model` as `<profile>:<id>` ([swarm.md](swarm.md)) |

## Tools

| Message or symptom | Fix |
| --- | --- |
| `Loop guard refused "<tool>": this exact input has been sent 4 times` | Change the arguments or the approach; the turn ends after two refusals ([turns.md](turns.md)) |
| `toolbox run needs args` | Put the tool's arguments inside `args`; from the next request that tool is also callable directly ([tools.md](tools.md)) |
| `old_string not found` / found several times | Re-read the file; give more context or `replace_all: true` ([editing.md](editing.md)) |
| `No files changed: the patch failed preflight.` | A hunk did not match; re-read and regenerate that hunk; nothing was written |
| A stale hashline anchor | `read` again with `hashline: true` and use the new anchors |
| `lsp` fails with a stderr tail | Install the language server and the project's toolchain ([search-and-web.md](search-and-web.md)) |
| A web search returns nothing | Try another `engine` before concluding |
| Output ends with an `artifact://` address | `read` that address with a line range |
| `ast_grep` finds nothing | Check the folder and file count the result names; pass `path` |
| A destructive command is refused | Narrow the target; protected paths are never deleted ([safety.md](safety.md)) |
| `plan_mode` refusal | Leave plan mode or get the plan approved |
| `IRC is unavailable` | The session is not hosted by a daemon ([irc.md](irc.md)) |
| `HOSTED_POLICY` | The hosting mode forbids it ([hosting.md](hosting.md)) |

## Sessions

| Symptom | Fix |
| --- | --- |
| `session is open` on delete | Detach first; the daemon releases it about 30 s after the client leaves ([sessions.md](sessions.md)) |
| Older messages are missing after resume | They were compacted; `read history://current` |
| `undo` has nothing to revert | Undo history does not survive a restart |
| A project MCP server is missing | Trust the workspace ([mcp.md](mcp.md)) |
| A run stopped waiting for a decision | `horon runtime-state show` / `horon approvals resolve` ([sessions.md](sessions.md)) |

## Gathering evidence

`horon provider diagnose --check`, `horon config check`, `horon session inspect <id>`, `horon mcp list`, and the daemon log under `<home>/logs/`. A crash leaves a local bundle under `<home>/crashes/`.
