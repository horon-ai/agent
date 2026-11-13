# The terminal client

Language: **English** | [한국어](TUI.ko.md)

`horon-tui` is the interactive client for the harness. It is a separate package and binary that talks to the daemon only through `horon-agent-sdk`, so the headless `horon-agent` graph stays free of terminal libraries and a client bug cannot take down the server.

## Running it

Bare `horon` (or `hrn`, or `horon-agent`) on a terminal starts the server when needed and then replaces itself with the `horon-tui` beside it. The replacement passes the working directory as `--cwd`, so a new session belongs to the directory the command was run from.

The handoff has three conditions, all checked before the process is replaced (`src/cli/tui_handoff.rs`):

- stdin and stdout must both be terminals. A pipe, a file or a closed stream keeps the headless path, because scripts and CI read its output.
- `HORON_NO_TUI` must be unset. Setting it to any value forces the headless path on a terminal.
- `horon-tui` must be a file next to the running executable. A launcher that is a symbolic link is retried at its real location, which matters on macOS.

When a condition fails, the command prints a sentence that names the condition (`stdin and stdout are not both terminals`, `HORON_NO_TUI is set`, or `no horon-tui beside this binary`) plus the command that installs the client, and exits with a non-zero code. A server it started before the check keeps running. Starting `horon-tui` directly skips the decision entirely.

A resumed session is handed over as well. `horon --resume <id>` passes `--session <id>` to the client, so the client attaches to that session instead of creating one. The socket is not passed on the command line: it is already exported as `HORON_AGENT_SOCKET`, and the client resolves the harness API socket itself.

The client takes its own arguments:

```text
horon-tui [--socket <path>] [--session <id>] [--cwd <dir>] [--config <file>]...
```

| Flag | Meaning |
| --- | --- |
| `--socket <path>` | Harness API socket instead of the resolved default |
| `--session <id>` | Attach to an existing session instead of creating one |
| `--cwd <dir>` | Working directory for a session this client creates |
| `--config <file>` | Extra settings file, repeatable |
| `-V`, `--version` | Print the version and exit |
| `-h`, `--help` | Print the usage and exit |

A lost bridge ends the client. The banner carries the sentence the event stream produced (for example `the event stream ended: ...`), the loop stops, and the process exits with code 1 so a wrapper sees the failure.

## User files

Five files shape the client. Four live under `$HORON_HOME` (`~/.horon` unless `HORON_HOME` is set) and one under the project.

| File | Scope | Purpose |
| --- | --- | --- |
| `$HORON_HOME/tui.toml` | Every project | Settings, including the [status line](#the-status-line) |
| `$HORON_HOME/themes/<name>.json` | Every project | A colour theme and its glyph preset |
| `$HORON_HOME/keybindings.toml` | Every project | Remapped keys |
| `$HORON_HOME/commands/<name>.md` | Every project | A slash command |
| `<cwd>/.horon/commands/<name>.md` | One project | A slash command for that project |

All five are read at startup and re-read while the client runs: the settings files are checked once a second, and the theme and key files follow the same cadence.

### Settings: `tui.toml`

The settings of one process are the registry defaults, then the global file, then the project file, then every `--config` file in the order given, then the environment. A higher layer wins per key; a key a layer does not name keeps the value from below. The settings screen shows the winning layer for every key, as `default`, `global <path>`, `project <path>`, `config <path>` or `env <name>`.

| Key | Type | Default | Environment variable |
| --- | --- | --- | --- |
| `theme` | text | `dark` | `HORON_TUI_THEME` |
| `symbols` | `unicode`, `nerd`, `ascii` | `unicode` | `HORON_TUI_SYMBOLS` |
| `statusline.preset` | `default`, `minimal`, `custom` | `default` | `HORON_TUI_STATUSLINE_PRESET` |
| `statusline.left` | array of segment names | `["model"]` | `HORON_TUI_STATUSLINE_LEFT` |
| `statusline.right` | array of segment names | `["status"]` | `HORON_TUI_STATUSLINE_RIGHT` |
| `statusline.path_max` | number | `40` | `HORON_TUI_STATUSLINE_PATH_MAX` |
| `statusline.context_window` | number | `0` | `HORON_TUI_STATUSLINE_CONTEXT_WINDOW` |
| `editor.submit_on_enter` | boolean | `true` | `HORON_TUI_EDITOR_SUBMIT_ON_ENTER` |
| `transcript.show_reasoning` | boolean | `false` | `HORON_TUI_TRANSCRIPT_SHOW_REASONING` |

`statusline.path_max` is the longest directory the `path` segment shows before it is shortened from the left; `0` keeps it whole. `statusline.context_window` is the token count the `context_pct` segment divides by; `0` hides the segment. `editor.submit_on_enter = false` disconnects the send key, so pressing it leaves the draft in place instead of sending; the editor is single-line, so the key inserts nothing rather than a newline. `transcript.show_reasoning = true` keeps the model's reasoning and draws it dimmed above the answer it precedes, committing one item per thought at a boundary (reasoning finished, a tool call, or the first answer delta) rather than one per streamed chunk. With the setting off, reasoning deltas are dropped when they arrive instead of being buffered for a setting that may never be turned on.

The project file is `<cwd>/.horon/tui.toml`. It is skipped when the working directory cannot be read. The file is read again a second after it changes in size or modification time, so a hand edit applies without a restart.

```toml
# ~/.horon/tui.toml
theme = "dark"
symbols = "nerd"

[statusline]
preset = "custom"
left = ["status", "model", "path"]
right = ["tokens", "context_pct", "time"]
path_max = 28
context_window = 200000
```

A key with the wrong shape is reported and skipped, and the rest of the file still applies. An unknown key is reported the same way.

### A theme in `$HORON_HOME/themes/`

A theme is one JSON object with a `name`, a `colors` table, and optional `vars` and `symbols` tables. `vars` names colours once and `colors` refers to them by name or writes a colour directly (`#rrggbb`). A broken file keeps the previous theme and produces a warning instead of leaving the interface unstyled.

```json
{
  "name": "mine",
  "vars": { "fg": "#c0caf5", "blue": "#7aa2f7" },
  "colors": {
    "accent": "blue",
    "border": "#3b4261",
    "muted": "#565f89",
    "text": "fg",
    "success": "#9ece6a",
    "warning": "#e0af68",
    "error": "#f7768e",
    "user_text": "#7dcfff",
    "assistant_text": "fg",
    "tool_title": "blue",
    "tool_running": "#e0af68",
    "tool_done": "#9ece6a",
    "tool_error": "#f7768e",
    "notice": "#565f89",
    "prompt_border": "blue",
    "prompt_text": "fg",
    "selected_bg": "#283457",
    "status_bg": "#1f2335",
    "status_text": "fg",
    "status_accent": "blue"
  },
  "symbols": { "preset": "unicode" }
}
```

Name the file after the theme (`themes/mine.json` for `name = "mine"`) and select it with `theme = "mine"`. The built-in names `dark` and `light` are looked up first, so a user file cannot shadow them. The `symbols.preset` in a theme applies unless `symbols` is set by a layer above the defaults; the three presets are `unicode`, `nerd` (private-use Nerd Font glyphs) and `ascii`.

### Key bindings: `keybindings.toml`

The file maps an action id to the keys it answers. An entry replaces every default key of that action, and an empty array turns the action off.

```toml
# ~/.horon/keybindings.toml
"app.submit" = ["enter", "shift+enter"]
"editor.complete" = []
"app.cancel" = ["ctrl+c", "esc"]
```

A key is written with the modifiers `ctrl`, `alt` and `shift` in any order before the key name (`Control+J` and `ctrl+j` are the same key). Character keys carry their shift in the character, so `A` and `shift+a` are one binding; every other key keeps shift, which makes `shift+enter` distinct from `enter`. A key the terminal reports with extra modifiers still answers a binding with the same key: `alt+left` moves the cursor like `left`.

Mistakes are warnings, not refusals. An unknown action id, a value that is not an array, a key name that cannot be read, and two actions in one scope claiming one key each produce a line in the transcript. The first action in declaration order keeps a contested key.

### Command files

A markdown file under `.horon/commands` (the project) or `$HORON_HOME/commands` (every project) becomes a slash command named after the file. A project file wins over a user file with the same name. A built-in command with that name shadows the file command entirely, and `/help` marks it `(shadowed by the built-in)`; a file wins over a harness command with the same name.

The optional block between two `---` lines holds `key: value` lines; only `description` and `argument-hint` are read. Without a description, the first body line is shown in `/help`.

```markdown
---
description: Review a diff against the project rules
argument-hint: <path>
---

Read $1 and review it against the rules in AGENTS.md. Extra context: $ARGUMENTS
```

`/review src/main.rs` replaces `$1` with `src/main.rs` and `$ARGUMENTS` with the whole argument string; `$2` through `$9` are the remaining words. Substitution runs once, so argument text is never scanned for another placeholder. An empty substitution result is an error rather than an empty message.

## Default keys

| Action | Keys | Scope |
| --- | --- | --- |
| `app.submit` | `enter` | Send the typed message |
| `app.cancel` | `ctrl+c` | Cancel the running turn, or leave when nothing is running |
| `app.quit` | `ctrl+d` | Leave |
| `editor.backspace` | `backspace` | Delete the character before the cursor |
| `editor.delete` | `delete` | Delete the character at the cursor |
| `editor.left` | `left` | Move the cursor one character left |
| `editor.right` | `right` | Move the cursor one character right |
| `editor.home` | `home` | Move the cursor to the start of the line |
| `editor.end` | `end` | Move the cursor to the end of the line |
| `editor.complete` | `tab` | Complete the slash command being typed |
| `prompt.allow` | `y` | Approve a parked tool call once |
| `prompt.allow_always` | `a` | Approve a parked tool call and every later call like it |
| `prompt.deny` | `n` | Deny a parked tool call |
| `prompt.dismiss` | `esc` | Ignore the prompt; the turn stays parked |
| `prompt.confirm` | `enter` | Confirm a multi-choice question |

While a prompt is open it takes every key, so a letter meant for the prompt never reaches the input line. Tab completes only a bare prefix: once the line contains a space the argument position belongs to the user.

## The status line

The bottom line is assembled from named segments. A segment with no data draws nothing, and an unknown name is reported and skipped so the rest of the line still draws.

| Segment | Shows |
| --- | --- |
| `status` | What the turn is doing: `idle`, `running` or `requires_action` |
| `model` | `provider/model`, or whichever of the two the daemon reported |
| `session` | The session id, shortened |
| `path` | The working directory, with the home directory shortened to `~` |
| `tokens` | Input and output tokens of the last request, as `↑in ↓out` |
| `context_pct` | Share of `statusline.context_window` used; hidden when the setting is `0` |
| `time` | Local wall-clock time as `HH:MM` |
| `pending` | A parked approval or question |

`statusline.preset` picks an arrangement:

| Preset | Left | Right |
| --- | --- | --- |
| `default` | `status`, `model`, `path` | `tokens`, `context_pct`, `time` |
| `minimal` | `model` | `status` |
| `custom` | Whatever `statusline.left` names | Whatever `statusline.right` names |

When the line does not fit the terminal, the least important segment is dropped first. Priority runs from `time` (dropped first), `session`, `tokens`, `context_pct`, `path`, `pending`, `status`, to `model` (dropped last). The last remaining segment stays even when it is wider than the pane, because a clipped model is more useful than an empty line.

## Commands

A line starting with `/` is resolved in this order: a built-in command, a command file, then a harness command. An unknown name is an error that points at `/help`, and a line starting with `//` is sent as the literal message with one slash removed.

| Command | Effect |
| --- | --- |
| `/quit` | Leave |
| `/help` | List the commands, where each one comes from, and its summary |
| `/clear` | Clear the transcript pane |
| `/model [name]` | List the models, or switch to the one named |
| `/settings` | Open the settings screen |

The harness commands run in the attached runtime and never reach the model as a prompt. `/help` prints them in name order, six to a line:

```text
  /agents /auth /autojudge /autoreview /context /continue
  /fast /info /initiatives /judge /logout /memory
  /overnight /plan /productivity /refresh_model_list /remote /resume_all
  /review /server_reload /skills /subagent /subagent_model /swarm
  /transport
```

Command files appear in `/help` with their argument hint and description. With no command files at all, `/help` says where to put one.

## The screen

The transcript is wrapped to the pane width before the visible rows are chosen, so a wrapped line occupies as many rows as its cells need. The newest rows stay in view: the pane keeps the tail of the wrapped output rather than the top of it. A row breaks after its last whitespace character, so a word is never split while the row has somewhere else to break; a row with no whitespace breaks at the cell limit, and a wide character that does not fit starts the next row. Styling survives the break because each cell carries the style of the span it came from.

The input is one line. Text before the cursor is dropped from the left once the line is wider than the pane, leaving one cell free so the caret stays visible at the right edge. The cursor column is measured in display cells, not bytes, so wide characters count for two.

## The settings screen

`/settings` is generated from the settings registry: one row per key, grouped into the Appearance, Status line, Editor and Transcript tabs, with the label and description the registry declares. Each row also shows the layer the value came from.

| Key | Effect |
| --- | --- |
| `up`, `down` | Move between rows |
| `left`, `tab` | Move to the previous tab |
| `right`, `shift+tab` | Move to the next tab |
| `enter` | Open the editor or the pick list of the row, or confirm |
| `esc` | Close the editor or pick list, then the screen |

A value that the key's type rejects is shown as an inline error and nothing is written. A save goes to the global `tui.toml` only; the project and `--config` layers are never rewritten.

The file is parsed and edited in place, so comments, key order and layout already in the file survive the save, and a key changed by hand since the client started keeps its value. A file that cannot be parsed is refused with `cannot parse <path>: ...; refusing to overwrite it`, because overwriting it would lose the user's work. A successful save is confirmed in the transcript with the path written to, and an environment variable that outranks the file is still shown as the winning layer afterwards.

## Installing the client

The release publishes the client as its own archive beside the agent: `horon-tui-<platform suffix>.tar.gz`, with the suffix the agent asset uses (`horon-tui-linux-x86_64.tar.gz`, `horon-tui-macos-aarch64.tar.gz`, `horon-tui-windows-x86_64.tar.gz`, and so on). Nothing else changes about the install.

`scripts/install/install.sh` and `scripts/install/install.ps1` fetch it next to the agent archive and check it against `SHA256SUMS`. `horon install-release` stages it into `builds/versions/<version>/` beside `horon-agent`, so a version directory holds both binaries and activation swaps them together.

The archive is optional at every step, and a copy that cannot be verified never blocks the agent:

- A release that publishes no archive for the platform installs `horon-agent` alone with one line saying so.
- A missing `SHA256SUMS` entry, or a digest that does not match, prints one line and installs the headless agent alone.
- A source build (`--source-binary`) and FreeBSD never stage a released client; FreeBSD publishes `horon-agent` only.
- An existing version directory that already holds `horon-agent` only gains the client when the file is absent; the version is immutable otherwise.

`scripts/install/uninstall.sh` and `scripts/install/uninstall.ps1` list the `horon-tui` launcher in the install directory with the rest of the targets and remove it too, including one left behind by an older install.

## Known limits

- Windows has no PTY smoke test. The `tui` benchmark records the render and size metrics there and marks the PTY metrics with `pty unavailable on Windows` instead of failing the run.
- FreeBSD ships `horon-agent` only, so the client is not installed there and bare `horon` reports the missing sibling.
- On Linux and macOS the handoff is one process replacing another, so a client that exits takes the terminal command with it and the exit code is the client's. On Windows the launcher starts the client and exits with code 0 at once, so its exit code says nothing about the client.
- `editor.submit_on_enter` is read by the send-key handling and `transcript.show_reasoning` by the transcript. Both apply from the next keystroke and the next reasoning delta once changed in the settings screen.
