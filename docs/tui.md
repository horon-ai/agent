# The terminal client

`horon` on a terminal opens `horon-tui`, a client of the daemon. Closing it leaves the daemon and the session running.

## User files

| File | Purpose |
| --- | --- |
| `<home>/tui.toml` | Settings (`theme`, `symbols`, status line, `transcript.mouse_scroll`, ...) |
| `<home>/themes/<name>.json` | Colour themes |
| `<home>/keybindings.toml` | Remapped keys |
| `<home>/commands/<name>.md`, `<cwd>/.horon/commands/<name>.md` | Your own slash commands (`$1`..`$9`, `$ARGUMENTS`) |

## Keys

| Keys | Action |
| --- | --- |
| `enter` / `shift+enter`, `alt+enter`, `ctrl+j` | Send / new line |
| `esc` | Send queued messages into the running turn; with none queued, interrupt it |
| `ctrl+c` | Copy the mouse selection when there is one; otherwise cancel the turn, or leave when idle |
| `ctrl+d` | Leave when the draft is empty |
| `alt+r` | Continue the last failed turn |
| `shift+tab` | Next reasoning effort for this session (the status line changes at once) |
| `alt+p` / `ctrl+p` | Pick a model / cycle the shortlist |
| `alt+m` | Plan mode (read-only) |
| `ctrl+t` | Show or hide reasoning |
| `ctrl+o` | Expand or fold the selected tool result |
| `alt+c`, `alt+l`, `alt+d`, `alt+k` | Copy newest code block, latest answer, draft, current line |
| `pageup`, `pagedown`, `shift+up`, `shift+down` | Scroll the transcript |
| `cmd+up`/`ctrl+home`, `cmd+down`/`ctrl+end` | Jump to the first message / back to the end |
| `alt+left`/`alt+b`, `alt+right`/`alt+f` | Move by word |
| `ctrl+r` / `ctrl+g` / `ctrl+v` | Search sent drafts / edit in `$EDITOR` / attach clipboard image |
| `tab` | Take the slash-menu selection or complete a path |
| `y` / `a` / `n` | Approve once / always / deny a parked tool call |

`/keys` lists the keys in force.

## Slash commands

| Need | Command |
| --- | --- |
| Change model | `/model [name]` or `/switch [name]` |
| Effort | `/effort [level]` |
| Session details panel (id, folder, model, messages, tokens) | `/session` (`esc` closes it) |
| Delete this session and start a new one | `/session delete` |
| Other sessions | `/resume [id]`, `/new`, `/fork`, `/archived`, `/rename <title>`, `/pin`, `/unpin` |
| Context | `/compact`, `/clear-context`, `/clear` (pane only) |
| Retry a failed turn | `/retry` |
| Background work, children, todos | `/jobs`, `/swarm`, `/todo` |
| A side question that does not touch the session | `/side <question>` |
| Tokens | `/stats` |
| Review and commit | `/diff [staged\|unstaged]`, `/commit <message>` |
| Export | `/export <path>` (offline HTML) |
| Skills, tools, MCP | `/skills`, `/tools`, `/mcp` |
| Help | `/help [command]`, `/settings` |
| Leave | `/quit`, `/exit` |

A skill runs with `/<name> <task>` or `/skill:<name> <task>`.

## Transcript behavior

- Tool calls are cards titled by their intent; a skill read shows as `✓ Read skill://<name>`. Card text keeps two blank columns from each edge, and blocks are separated by one blank row.
- Tabs in tool output become spaces and other control characters are dropped, so card edges stay aligned.
- The wheel moves one row per event; events that arrive before the next frame are drawn together.
- A left-button drag selects conversation text. The selection follows the text when you scroll, dragging past the top or bottom edge scrolls that way, and `ctrl+c` (or `cmd+c` where the terminal passes it) copies it, including rows scrolled off screen. Releasing the button copies nothing.
- The status line shows model, effort, context use (`8%|1M`), tokens and the session's average output speed (`tok/s`).

## Limits

The client needs at least 20×6 cells. It exits `1` on a pipe. Restored sessions redraw their recent history; older compacted parts are read with `history://current`.
