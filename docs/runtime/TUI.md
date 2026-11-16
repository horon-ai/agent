# The terminal client

Language: **English** | [한국어](TUI.ko.md)

`horon-tui` is the TUI binary: TUI mode of horon agent, where a person drives a session from the terminal. Headless mode, where a program drives it, runs from the agent binary `horon-agent`; both modes share one engine and one daemon. `horon-tui` is a separate package and binary that talks to the daemon only through `horon-agent-sdk`, so the `horon-agent` graph stays free of terminal libraries and a client bug cannot take down the server.

## Running it

Bare `horon` (or `hrn`, or `horon-agent`) on a terminal starts the server when needed and then replaces itself with the `horon-tui` beside it. The replacement passes the working directory as `--cwd`, so a new session belongs to the directory the command was run from.

The handoff has three conditions, all checked before the process is replaced (`src/cli/tui_handoff.rs`):

- stdin and stdout must both be terminals. A pipe, a file or a closed stream keeps headless mode, because scripts and CI read its output.
- `HORON_NO_TUI` must be unset. Setting it to any value forces headless mode on a terminal.
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

A connection that never opened ends the client. The banner carries the sentence the event stream produced (for example `the event stream ended: ...`), the loop stops, and the process exits with code 1 so a wrapper sees the failure.

A connection lost while the client runs is handled differently: the screen stays up so the failure and the conversation can be read. The status line says `disconnected`, a submit is refused with one warning line and the text stays in the input line, nothing remaining in the queue is sent, and prompts for the turn that was running are withdrawn. Ctrl-C leaves, and the process still exits with code 1 because the session failed.

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
| `transcript.tool_output_lines` | number | `3` | `HORON_TUI_TRANSCRIPT_TOOL_OUTPUT_LINES` |
| `transcript.mouse_scroll` | boolean | `false` | `HORON_TUI_TRANSCRIPT_MOUSE_SCROLL` |
| `transcript.show_reasoning` | boolean | `false` | `HORON_TUI_TRANSCRIPT_SHOW_REASONING` |
| `transcript.markdown` | boolean | `true` | `HORON_TUI_TRANSCRIPT_MARKDOWN` |

The table is the settings registry (`crates/tui/src/settings/registry.rs`), twelve keys in its order, and it is what the settings screen draws.

`statusline.path_max` is the longest directory the `path` segment shows before it is shortened; `0` keeps it whole. A shortened path keeps whole segments from its end and starts with `…/`, so the marker stands for the directories left out; a path with no separator at all is cut inside its only name and marked with the ellipsis alone. `statusline.context_window` is the token count the `context_pct` segment divides by; `0` hides the segment.

`editor.submit_on_enter = false` disconnects the send key, so pressing it starts a new line in the draft instead of sending. `transcript.tool_output_lines` is how many rows of a finished tool call's output are kept under its line: `0` hides the preview, and a value above `40` is capped. `transcript.mouse_scroll = true` turns on the terminal's mouse capture and gives the wheel to the transcript; with the default `false` the client never captures the mouse and the terminal keeps its own text selection. `transcript.markdown = false` draws the model's text with its Markdown markers as typed.

`transcript.show_reasoning = true` keeps the model's reasoning and draws it dimmed above the answer it precedes, committing one item per thought at a boundary (reasoning finished, a tool call, or the first answer delta) rather than one per streamed chunk. With the setting off, reasoning deltas are dropped when they arrive instead of being stored.

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
"app.interrupt" = ["ctrl+x"]
```

An action id is a dotted name, so it must be written as a quoted key. `"app.interrupt" = [...]` names the action; the unquoted `app.interrupt = [...]` is a nested table, which the client reports as unknown action `app` and ignores, leaving every default in place.

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

| Action | Keys | What it does |
| --- | --- | --- |
| `app.submit` | `enter` | Send the typed message |
| `app.cancel` | `ctrl+c` | Cancel the running turn, or leave when nothing is running |
| `app.interrupt` | `esc` | Interrupt the running turn and nothing else |
| `app.quit` | `ctrl+d` | Leave |
| `app.scroll_page_up` | `pageup` | Hold the transcript one page higher |
| `app.scroll_page_down` | `pagedown` | Move the transcript one page back towards the end |
| `app.scroll_up` | `shift+up` | Hold the transcript one row higher |
| `app.scroll_down` | `shift+down` | Move the transcript one row back towards the end |
| `app.scroll_bottom` | `ctrl+end` | Follow the end of the conversation again |
| `editor.backspace` | `backspace` | Delete the character before the cursor |
| `editor.delete` | `delete` | Delete the character at the cursor |
| `editor.left` | `left`, `ctrl+b` | Move the cursor one character left |
| `editor.right` | `right`, `ctrl+f` | Move the cursor one character right |
| `editor.home` | `home`, `ctrl+a` | Move the cursor to the start of its line |
| `editor.end` | `end`, `ctrl+e` | Move the cursor to the end of its line |
| `editor.word_left` | `alt+left` | Move to the start of the word before the cursor |
| `editor.word_right` | `alt+right` | Move past the word after the cursor |
| `editor.history_prev` | `up` | Step back through earlier drafts, then into the lines of the draft |
| `editor.history_next` | `down` | Step forward again, back to the draft being typed |
| `editor.newline` | `shift+enter`, `alt+enter`, `ctrl+j` | Start a new line without sending the draft |
| `editor.kill_to_start` | `ctrl+u` | Delete from the start of the line to the cursor |
| `editor.kill_to_end` | `ctrl+k` | Delete from the cursor to the end of the line |
| `editor.delete_word` | `ctrl+w` | Delete the word before the cursor |
| `editor.complete` | `tab` | Fill the popup's selected command, or complete the prefix |
| `prompt.allow` | `y` | Approve the parked tool call once |
| `prompt.allow_always` | `a` | Approve this call and every later call like it |
| `prompt.deny` | `n` | Deny the parked tool call |
| `prompt.dismiss` | `esc` | Close the prompt without answering; on an approval that answers it with a denial |
| `prompt.confirm` | `enter` | Confirm a multi-choice question |

The table is `ACTIONS` in `crates/tui/src/keys/bindings.rs`: twenty-nine actions, each with the scope it belongs to. `/help` lists the same ids in the same order with the keys in force, so a rebound key is the key the listing shows.

Most terminals send Shift-Enter as a plain Enter, which submits. `alt+enter` and `ctrl+j` are therefore the reliable newline keys, and `shift+enter` answers only where the terminal reports the shifted form.

`up` and `down` move between the lines of a draft first and reach the input history only at its first and last line, which is why the transcript view moved to `shift+up`, `shift+down` and the page keys. While a prompt is open it takes every key, so a letter meant for the prompt never reaches the input line. Tab completes only a bare prefix: once the line contains a space the argument position belongs to the user.

## The editor

The input line grows with the draft: one row per line, up to eight rows, and a taller draft scrolls inside its own pane. The conversation keeps at least three rows while the draft grows, so a long message never takes the whole screen.

`enter` sends and the newline keys start a new line. With `editor.submit_on_enter = false` the send key starts a new line instead and nothing is sent from the keyboard, which is one way to type a multi-line message without a paste.

A paste is taken as one block. The client turns on the terminal's bracketed-paste mode while it runs, so a three-line paste arrives as one `Paste` event and stays one draft instead of being sent as three messages. The mode is turned off on the normal exit, on the error path, and from the panic hook, because a client that dies with it on leaves the shell pasting escape codes into the next program. A terminal that does not support the mode answers nothing and the paste arrives as ordinary keystrokes.

The last hundred drafts are kept for `up` and `down`. A walk starts from the newest entry and stepping past it returns the draft that was on screen when the walk began, so `up` twice never loses what was being typed. One entry is kept per distinct message in a row, and editing the draft ends the walk: the next `down` does not replace the edit with the entry the walk started from.

The cursor is placed in display cells rather than bytes, so a wide character counts for two. Text before the cursor is dropped from the left once the draft is wider than the pane, leaving one cell free so the caret stays visible at the right edge.

## The transcript

A tool call is drawn as one head row and the rows under it. The head carries the call's name, a short argument summary and the outcome. The summary is the first interesting field of the call's own JSON, taken in the order `command`, `file_path`, `path`, `pattern`, `query`, `url`, `name`, `prompt`, cut to the space the row has left. The outcome is `◐ running`, `✓ done` or `✕ failed` with the elapsed time (`1.4s`, `2m 05s`).

Results are matched to calls by call id, so two calls that ran in parallel cannot swap their results, and a result whose start this client never saw gets a row of its own instead of being attached to another call's. A finished call keeps a preview of its output under the head: three rows by default, each cut to 160 cells, and `+N lines` for the rows the preview does not show. A failure puts its message on its own row instead of in the head, because the message is usually longer than the pane and would push the argument and the duration off the right edge.

A call the approval gate refused is drawn with `needs approval` rather than the daemon's whole sentence, because the prompt beside it already asks the question. Answering that prompt does not run the call: the decision is recorded and the model has to call the tool again, and the transcript says so.

The conversation can be read back. `pageup`, `pagedown`, `shift+up` and `shift+down` hold the view, and `ctrl+end` returns to the end. While the view is held the bottom row carries `↓ N rows below`, and it adds `· new output` once a row has arrived that the held view has not shown. Taking the view off the end sets the baseline for that count, so the first `↓ N rows below` counts from the row the user was looking at.

Assistant text is rendered as Markdown: headings, emphasis, inline code, lists, block quotes, fenced code and horizontal rules. A list is nested up to six levels, two columns or one tab per level, and the nesting is drawn from the indent the model wrote. Fenced code keeps its text as typed, and the opening fence's first word is drawn as a language label once the fence line is complete, so a half-received ` ```ru ` does not flash a label that then changes. A table, a link and any syntax the renderer does not claim stay as the source the model wrote, because dropping the text would lose what it said. Wrapped rows keep the indent of the row they continue, so a nested item still reads as nested after a break. A new turn starts one blank row below the previous one.

`transcript.markdown = false` turns the rendering off and draws the same text with its markers as typed.

## Turn state

One turn runs at a time. Enter during a running turn queues the text instead of sending it: the transcript shows it as a `queued: <text>` row, and the message is sent when the running turn ends, in the order the messages were typed. Cancelling the turn keeps the queue, because the queue belongs to the next turn rather than to the one that stopped.

`esc` interrupts the running turn and does nothing when the client is idle, so a stray key cannot end the session. `ctrl+c` cancels the turn when one is running and leaves when none is, and `ctrl+d` always leaves.

A failed send is not drawn as a message. The client takes the user row back, puts the text in the input line (or queues it, when the input line already holds something else), returns the status to `idle` and shows one warning line with the reason. The queue is held until the next turn ends or the user submits again, so a harness that keeps refusing cannot turn a refused send into a loop.

When the connection goes away the status line says `disconnected`, the queue is not sent, the prompts of the turn are withdrawn, and submitting is refused with the text left in the input line. Ctrl-C leaves from there.

## Prompts

An approval and a question are drawn above the input line and take every key until they are answered.

Approvals are a queue. A second call that needs approval while the first is on screen waits behind it, and the prompt on screen adds a line `N more prompt waiting`, singular as `1 more prompt waiting`, so a second approval is not read as a repeat of the first. The prompt shows the call's action and, when the transcript can pair it with the request id the refusal wrote, the arguments of that exact call; a prompt is never shown another call's command.

`y` approves once, `a` approves and asks for every later call like it, and `n` or `esc` denies. An approval outlives its turn: the request stays queued in the daemon with its own deadline, so a lost prompt would be the only way to answer it taken away. Approving does not run the call. The transcript records the decision as `you allowed <action>: the model has to call it again before it runs`, or `you allowed <action>: recorded once (the daemon keeps no always-rule), so the model has to call it again` for the `a` key, and `you denied <action>: the call will not run` for a denial, because only the model calling the tool again runs it.

The `ask` tool's questions are drawn one at a time with a `(1/2)` counter. A single-choice question is settled by the digit of its option; a multi-select one toggles digits and confirms with `enter`, and the chosen options carry `[x]`. A recommended option is marked `(recommended)`. `esc` leaves the question open: the tool has no deny path and resolves its own deadline, so closing the prompt would take away the only way to answer it. Answering every question adds `you answered the ask tool (N answers): the call resumes`, because the tool waits for the answer on this connection instead of being remembered for a retry.

Prompts are withdrawn when their turn is cancelled, when the turn ends (questions only), and when the connection is lost. The withdrawal leaves one notice line naming what was dropped, as in `the turn was cancelled; 2 prompts withdrawn`.

## The slash popup and `/help`

Typing `/` opens a list of the commands whose name starts with what has been typed: the same commands `/help` lists, in name order, with their descriptions. The list shows at most eight rows and moves its window with the selection; `up` and `down` move it, `tab` fills the selected command, and `esc` closes it. The popup is the topmost surface, so the first `esc` always closes the popup and only a second one interrupts the turn. While the popup is open the completion hint is not drawn, because the list already says what the hint would. A draft whose one match is exactly the name typed shows no popup: there is nothing left to choose.

`/help` prints the commands grouped by where they come from, with the description column aligned, then the keys. The key list is built from the key map in force rather than from the shipped defaults, so a rebound key shows its new binding and an action a file turned off shows as `(unbound)`.

## Pickers

`/model` with no argument opens a filterable list of the models the harness reported, with the model in force marked `(current)` and the selected row carrying the input line's own marker (`❯` in the Unicode preset). Typing filters the list, the rows show `nothing matches that filter` when none is left, `enter` picks the row and `esc` closes the list. Picking sends the switch to the daemon, and the status line changes only when the daemon confirms it with the session's identity, so a refused switch leaves the old model showing.

`/model <name>` switches without the list.

`/resume` opens the sessions stored for the directory the client is working in, newest first. Each row carries the session's title (or its first user line, or its id) and the time it last changed, flush with the row's right edge so the times read down the list; a conversation too narrow for both keeps the name. The session in force is marked `(current)`. `/resume` is refused while a turn is running, because attaching elsewhere would leave that turn's events arriving for a session the client no longer shows, and it is refused when the connection is gone. Attaching clears the screen of the old session, replays the stored conversation as user and assistant messages, adds a notice naming the new session, and takes the model and provider from the new session's identity, so the status line keeps showing `provider/model`.

In a window too small for an overlay, both `/model`, `/resume` and the settings screen draw one line instead of the panel: `list hidden: window too small, esc closes` or `settings hidden: window too small, esc closes`. The overlay still owns every key, so `esc` closes it.

## The status line

The bottom line is assembled from named segments. A segment with no data draws nothing, and an unknown name is reported and skipped so the rest of the line still draws.

| Segment | Shows |
| --- | --- |
| `status` | What the turn is doing: `idle`, `running`, `requires_action` or `disconnected` |
| `model` | `provider/model`, or whichever of the two the daemon reported |
| `session` | The session id, shortened |
| `path` | The working directory, with the home directory shortened to `~` and long paths cut at a segment boundary |
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
| `/clear` | Clear the transcript pane |
| `/help` | List the commands, where each one comes from, and the keys in force |
| `/model [name]` | Pick a model from a list, or switch to the one named |
| `/quit` | Leave |
| `/resume` | Pick a session stored for this directory and replay it |
| `/settings` | Open the settings screen |

The table is the built-in command list (`Local` in `crates/tui/src/commands/mod.rs`): six commands the client answers itself, in name order.

The harness commands run in the attached runtime and never reach the model as a prompt. `/help` prints them in name order, six to a line:

```text
  /agents /auth /autojudge /autoreview /context /continue
  /fast /info /initiatives /judge /logout /memory
  /overnight /plan /productivity /refresh_model_list /remote /resume_all
  /review /server_reload /skills /subagent /subagent_model /swarm
  /transport
```

Command files appear in `/help` with their argument hint and description. With no command files at all, `/help` says where to put one.

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
- A missing `SHA256SUMS` entry, or a digest that does not match, prints one line and installs the agent binary alone.
- A source build (`--source-binary`) and FreeBSD never stage a released client; FreeBSD publishes `horon-agent` only.
- An existing version directory that already holds `horon-agent` only gains the client when the file is absent; the version is immutable otherwise.

`scripts/install/uninstall.sh` and `scripts/install/uninstall.ps1` list the `horon-tui` launcher in the install directory with the rest of the targets and remove it too, including one left behind by an older install.

## Known limits

- Windows has no PTY smoke test. The `tui` benchmark records the render and size metrics there and marks the PTY metrics with `pty unavailable on Windows` instead of failing the run.
- FreeBSD ships `horon-agent` only, so the client is not installed there and bare `horon` reports the missing sibling.
- On Linux and macOS the handoff is one process replacing another, so a client that exits takes the terminal command with it and the exit code is the client's. On Windows the launcher starts the client and exits with code 0 at once, so its exit code says nothing about the client.
- Killing the client with `SIGTERM` or `SIGHUP` does not restore the terminal modes. Bracketed paste is turned off on every exit the client itself runs, but a signal is not an exit it runs, so the shell can be left with the mode on; with `transcript.mouse_scroll = true` the mouse mode is left on the same way. The client installs no signal handler.
- A theme JSON file edited on disk is re-read by the theme loader on its one-second check, but the screen is not redrawn for it: an idle client keeps the picture it has until the next key, event or settings reload.
- A table and a link are shown as the source the model wrote, and a list indented by one or three spaces rounds down to a whole number of levels, because a level is two columns.
- `editor.submit_on_enter` is read by the send-key handling and `transcript.show_reasoning` by the transcript; both apply from the next keystroke and the next reasoning delta once changed. The other transcript settings and the status line take effect as soon as the file reloads, which redraws the screen.
