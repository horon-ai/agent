# The terminal client

Language: **English** | [한국어](TUI.ko.md)

`horon-tui` is the TUI binary: TUI mode of horon agent, where a person drives a session from the terminal. Headless mode, where a program drives it, runs from the agent binary `horon-agent`; both modes share one engine and one daemon. `horon-tui` is a separate package and binary that talks to the daemon only through `horon-agent-sdk`, so the `horon-agent` graph stays free of terminal libraries and a client bug cannot take down the server.

## Running it

Bare `horon` (or `hrn`, or `horon-agent`) on a terminal starts the server when needed and then replaces itself with the `horon-tui` beside it. The replacement passes the working directory as `--cwd`, so a new session belongs to the directory the command was run from.

The handoff has three conditions, all checked before the process is replaced (`src/cli/tui_handoff.rs`):

- stdin and stdout must both be terminals. A pipe, a file or a closed stream keeps headless mode, because scripts and CI read its output.
- `HORON_NO_TUI` must be unset. Setting it to any value forces headless mode on a terminal.
- `horon-tui` must be a file next to the running executable. A launcher that is a symbolic link is retried at its real location, which matters on macOS.

When a condition fails, the command prints a sentence that names the condition (`stdin and stdout are not both terminals`, `HORON_NO_TUI is set`, or `no horon-tui beside this binary`), the one remedy that answers it, and the headless fallback, then exits with a non-zero code. Only a missing client is answered by installing: a pipe or `HORON_NO_TUI` happens with the client already in place, so the sentence names a terminal or the variable instead. A server it started before the check keeps running. Starting `horon-tui` directly skips the decision entirely.

A resumed session is handed over as well. `horon --resume <id>` passes `--session <id>` to the client, so the client attaches to that session instead of creating one. The socket is not passed on the command line: it is already exported as `HORON_AGENT_SOCKET`, and the client resolves the harness API socket itself.

`horon --continue` selects the most recently active saved session in the working directory and hands it to the client the same way.

The client reaches the daemon through the harness API bridge, and starts that bridge when no listening socket answers. The bridge serves the terminal that started it and ends with it: while a client is connected it keeps serving, and once none is it stops, as soon as the process that started it exits or after a minute without a client, releasing its single-instance lock and removing the socket file (`horon-agent-api.sock`). The next launch therefore starts a bridge from the installed release instead of reconnecting to one that stayed resident across an upgrade, and the daemon keeps running throughout. A socket file left by a bridge that was killed outright is not taken as a running bridge: `horon` and `horon-tui` check the socket with a connection, and when nothing answers they start the bridge again, which replaces the leftover file. No manual cleanup is needed.

The client takes its own arguments:

```text
horon-tui [--socket <path>] [--session <id>] [--cwd <dir>] [--config <file>]... [--gallery[=<scene>]]
           [--motion <mode>] [--spinner-interval <ms>] [--max-fps <n>] [--hide-thinking] [--no-title]
```

The three motion flags are per-launch overrides: they outrank every settings file and the environment, are shown as `command line` in the settings screen, and survive a reload of the files under them. A flag outside its range is an argument error, not a warning: `--max-fps 0` prints the expected range and exits with code 2.

| Flag | Meaning |
| --- | --- |
| `--socket <path>` | Harness API socket instead of the resolved default |
| `--session <id>` | Attach to an existing session instead of creating one |
| `--cwd <dir>` | Working directory for a session this client creates |
| `--config <file>` | Extra settings file, repeatable |
| `--gallery[=<scene>]` | Open an offline preview of the terminal renderer |
| `--motion <mode>` | `full`, `reduced` or `static` for this launch, over `motion.preset` |
| `--spinner-interval <ms>` | Spinner frame length for this launch, `40` to `2000` |
| `--max-fps <n>` | Stream frame limit for this launch, `1` to `240` |
| `--hide-thinking` | Start with the model's reasoning hidden for this session, whatever `transcript.show_reasoning` says; `ctrl+t` (default) toggles it again |
| `--no-title` | Leave the terminal window title alone for this launch |
| `-V`, `--version` | Print the version and exit |
| `-h`, `--help` | Print the usage and exit |

Run `horon-tui --gallery=unicode` to see how the renderer wraps wide characters, or start with `horon-tui --gallery` to see the first fixture. Use `n` and `p` or the arrow keys to change scenes and `q` to leave. The preview uses fixed data and requires no daemon or model connection.

A connection that never opened ends the client. The banner carries the sentence the event stream produced (for example `the event stream ended: ...`), the loop stops, and the process exits with code 1 so a wrapper sees the failure.

A connection lost while the client runs is handled differently: the screen stays up so the failure and the conversation can be read. The status line says `disconnected`, a submit is refused with one warning line and the text stays in the input line, nothing remaining in the queue is sent, and prompts for the turn that was running are withdrawn. Ctrl-C leaves, and the process still exits with code 1 because the session failed.

The client names the terminal window `horon · <label>`, so a stack of terminals says which one belongs to which session. The label is the session's own title, and the last component of the working directory for a session the daemon never titled; control characters are dropped and the label stops at eighty characters. The title the shell set is pushed onto the terminal's title stack first and comes back on every way out — the normal exit, the error path and the panic hook — because the shell still owns the window afterwards. The stack pair (`CSI 22 ; 2 t` to push, `CSI 23 ; 2 t` to pop) is an extension, so it is written only where the terminal keeps a stack: an xterm-like terminal (`TERM` other than `dumb`), Windows Terminal (`WT_SESSION`) and ConEmu (`ConEmuANSI`) restore the title, while a Windows console without either marker and any terminal that calls itself `dumb` get the title without the pair and keep `horon · <label>` after the client exits. Nothing is written when standard output is not a terminal.

Set `title.state = true` to append `running`, `waiting` or `idle` to the title on terminals with a title stack. The marker changes with the turn or the pending prompt. Exit restores the previous title.

Set `title.enabled = false` in `tui.toml` to leave the terminal title alone from the next launch. `--no-title` overrides the setting for one launch and also works in the offline gallery. Saved session names still appear in the session picker.

## User files

Six files shape the client. Five live under `$HORON_HOME` (`~/.horon` unless `HORON_HOME` is set) and one under the project.

| File | Scope | Purpose |
| --- | --- | --- |
| `$HORON_HOME/tui.toml` | Every project | Settings, including the [status line](#the-status-line) |
| `$HORON_HOME/themes/<name>.json` | Every project | A colour theme and its glyph preset |
| `$HORON_HOME/keybindings.toml` | Every project | Remapped keys |
| `$HORON_HOME/commands/<name>.md` | Every project | A slash command |
| `$HORON_HOME/tui-notes.json` | Every project | Local notes on transcript messages, written by `/note` |
| `<cwd>/.horon/commands/<name>.md` | One project | A slash command for that project |

The five configuration files are read at startup and re-read while the client runs: the settings files are checked once a second, and the theme and key files follow the same cadence. `tui-notes.json` is read once at startup and written by `/note`; it is created readable by the owner only (`0600`), and a file found with wider permissions is narrowed on the next read.

`/info` shows the daemon's `approval mode` for TUI calls. It defaults to `yolo`; change `[tools] approval_mode` in the daemon's `config.toml`.

### Settings: `tui.toml`

The settings of one process are the registry defaults, then the global file, then the project file, then every `--config` file in the order given, then the environment. A higher layer wins per key; a key a layer does not name keeps the value from below. The settings screen shows the winning layer for every key, as `default`, `global <path>`, `project <path>`, `config <path>` or `env <name>`.

| Key | Type | Default | Environment variable |
| --- | --- | --- | --- |
| `theme` | text | `dark` | `HORON_TUI_THEME` |
| `symbols` | `unicode`, `nerd`, `ascii` | `unicode` | `HORON_TUI_SYMBOLS` |
| `motion.preset` | `full`, `reduced`, `static` | `full` | `HORON_TUI_MOTION_PRESET` |
| `motion.spinner_interval` | number | `50` | `HORON_TUI_MOTION_SPINNER_INTERVAL` |
| `motion.max_fps` | number | `30` | `HORON_TUI_MOTION_MAX_FPS` |
| `appearance.changes` | `theme`, `accessible` | `theme` | `HORON_TUI_CHANGES` |
| `title.enabled` | boolean | `true` | `HORON_TUI_TITLE_ENABLED` |
| `title.state` | boolean | `false` | `HORON_TUI_TITLE_STATE` |
| `statusline.preset` | `default`, `minimal`, `custom` | `default` | `HORON_TUI_STATUSLINE_PRESET` |
| `statusline.left` | array of segment names | `["model"]` | `HORON_TUI_STATUSLINE_LEFT` |
| `statusline.right` | array of segment names | `["status"]` | `HORON_TUI_STATUSLINE_RIGHT` |
| `statusline.path_max` | number | `40` | `HORON_TUI_STATUSLINE_PATH_MAX` |
| `statusline.context_window` | number | `0` | `HORON_TUI_STATUSLINE_CONTEXT_WINDOW` |
| `statusline.separator` | `preset`, `dot`, `bar` | `preset` | `HORON_TUI_STATUSLINE_SEPARATOR` |
| `statusline.background` | `theme`, `terminal` | `theme` | `HORON_TUI_STATUSLINE_BACKGROUND` |
| `terminal.progress` | boolean | `false` | `HORON_TUI_TERMINAL_PROGRESS` |
| `terminal.program_status` | boolean | `false` | `HORON_TUI_TERMINAL_PROGRAM_STATUS` |
| `editor.submit_on_enter` | boolean | `true` | `HORON_TUI_EDITOR_SUBMIT_ON_ENTER` |
| `editor.suggestions` | boolean | `false` | `HORON_TUI_EDITOR_SUGGESTIONS` |
| `transcript.tool_output_lines` | number | `3` | `HORON_TUI_TRANSCRIPT_TOOL_OUTPUT_LINES` |
| `transcript.show_tool_activity` | boolean | `true` | `HORON_TUI_TRANSCRIPT_SHOW_TOOL_ACTIVITY` |
| `transcript.mouse_scroll` | boolean | `true` | `HORON_TUI_TRANSCRIPT_MOUSE_SCROLL` |
| `transcript.show_reasoning` | boolean | `false` | `HORON_TUI_TRANSCRIPT_SHOW_REASONING` |
| `transcript.markdown` | boolean | `true` | `HORON_TUI_TRANSCRIPT_MARKDOWN` |
| `transcript.turn_usage` | boolean | `false` | `HORON_TUI_TRANSCRIPT_TURN_USAGE` |
| `transcript.visual_blocks` | boolean | `true` | `HORON_TUI_TRANSCRIPT_VISUAL_BLOCKS` |
| `models.shortlist` | array of model IDs | `[]` | `HORON_TUI_MODELS_SHORTLIST` |

The table follows the settings registry (`crates/tui/src/settings/registry.rs`), which also supplies the settings screen. The opt-in `usage` and `cost` segments are chosen with `statusline.left` and `statusline.right` rather than by a key of their own, so the registry gains no key for them.

`statusline.path_max` is the longest directory the `path` segment shows before it is shortened; `0` keeps it whole. A shortened path keeps whole segments from its end and starts with `…/`, so the marker stands for the directories left out; a path with no separator at all is cut inside its only name and marked with the ellipsis alone. `statusline.context_window` is a manual override of the token count the `context_pct` segment divides by. Left at `0` the segment divides by the window the runtime reported for the route in force, so switching models refreshes the denominator without a restart; a value above zero wins over the report, for a user who knows their route better than the daemon's catalog does. Neither known hides the segment.

`statusline.separator` can keep the glyph preset, draw a middle dot (`·`), or draw a bar (`|`). Set `statusline.background = "terminal"` to use the terminal's own background beneath the status line.

`editor.submit_on_enter = false` disconnects the send key, so pressing it starts a new line in the draft instead of sending. `transcript.tool_output_lines` is how many rows of a finished tool call's output are kept under its line: `0` hides the preview, and a value above `40` is capped. `transcript.mouse_scroll` is on by default: the client captures the mouse and the wheel scrolls the transcript. Turning it off releases the capture, so the terminal keeps its own text selection and the wheel does what the terminal does; most terminals also bypass a captured mouse while a selection modifier is held, such as Shift-drag. `transcript.markdown = false` draws the model's text with its Markdown markers as typed.

`motion.preset` controls the indicator beside `running`. `full` cycles its glyphs, `reduced` holds the first glyph and `static` hides the indicator. `motion.spinner_interval` sets the minimum time a glyph stays visible, from `40` to `2000` milliseconds. `motion.max_fps` caps automatic redraws due to incoming text or spinner ticks in all presets, from `1` to `240` frames per second. At `max_fps = 4`, automatic redraws are at least 250 milliseconds apart. Input, resize and other events that need immediate feedback can redraw sooner. Out-of-range values are rejected with a warning that names the default in use.

Set `terminal.progress = true` to report progress to terminals that support OSC 9;4. The indicator shows a percentage when available and an indeterminate state otherwise. Set `terminal.program_status = true` to send OSC 7501 updates on terminals that support that protocol. Horon sends changes as they occur and clears its signals on exit.

`appearance.changes = "accessible"` gives additions and deletions distinct colours in tool diffs and preserves their `+` and `-` marks. It also adjusts positive and negative status colours. Select `theme` to restore the theme's own colours.

`transcript.show_reasoning` is the thinking-block control, labelled `Show thinking` in the settings screen: `true` keeps the model's reasoning and draws it dimmed above the answer it precedes, committing one item per thought at a boundary (reasoning finished, a tool call, or the first answer delta) rather than one per streamed chunk. With the setting off, reasoning deltas are dropped when they arrive instead of being stored, so turning it off hides the thinking blocks of every later turn.

`ctrl+t` (`app.toggle_thinking`, the default binding) flips the same switch for the rest of the process without writing the file, and `--hide-thinking` starts it off hidden that way. The session choice outranks the setting: an unrelated reload or a theme change keeps it, while a change of `transcript.show_reasoning` itself is the newer choice and ends it. Hiding drops the thought still arriving but keeps the thoughts already committed on screen; showing begins at the next delta, because the deltas dropped meanwhile were never kept and are not recovered. Neither direction touches the reasoning effort the model is asked for. Each toggle leaves one notice in the transcript saying which way it went.

The project file is `<cwd>/.horon/tui.toml`. It is skipped when the working directory cannot be read. The file is read again a second after it changes in size or modification time, so a hand edit applies without a restart.

```toml
# ~/.horon/tui.toml
theme = "dark"
symbols = "nerd"

[motion]
preset = "reduced"
spinner_interval = 120
max_fps = 20

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
| `app.interrupt` | `esc` | Send the queued messages into the running turn; with nothing queued, interrupt it. No action when idle |
| `app.quit` | `ctrl+d` | Delete at the cursor while the draft has text, or leave when the draft is empty and the editor is idle |
| `app.scroll_page_up` | `pageup` | Hold the transcript one page higher |
| `app.scroll_page_down` | `pagedown` | Move the transcript one page back towards the end |
| `app.scroll_up` | `shift+up` | Hold the transcript one row higher |
| `app.scroll_down` | `shift+down` | Move the transcript one row back towards the end |
| `app.scroll_bottom` | `ctrl+end` | Follow the end of the conversation again |
| `app.expand_tool` | `ctrl+o` | Expand or fold the selected retained tool result |
| `app.toggle_tool_activity` | `alt+t` | Show or hide completed tool activity for this session |
| `app.copy_code` | `alt+c` | Copy the newest code block in the latest answer |
| `app.copy_last` | `alt+l` | Copy the latest answer |
| `app.copy_draft` | `alt+d` | Copy the whole draft |
| `app.copy_line` | `alt+k` | Copy the line under the cursor |
| `app.cycle_effort` | `shift+tab` | Step to the next reasoning effort, wrapping at the last one |
| `app.pick_model` | `alt+p` | Pick the model this session uses |
| `app.cycle_model` | `ctrl+p` | Move forward through the model shortlist |
| `app.cycle_model_back` | `ctrl+alt+p` | Move backward through the model shortlist |
| `app.repaint` | `ctrl+l` | Clear and redraw the terminal while keeping the draft, selection and scroll position |
| `app.toggle_thinking` | `ctrl+t` | Show or hide the model's reasoning for this session |
| `editor.backspace` | `backspace` | Delete the grapheme cluster before the cursor |
| `editor.delete` | `delete` | Delete the grapheme cluster at the cursor |
| `editor.left` | `left`, `ctrl+b` | Move the cursor one grapheme cluster left |
| `editor.right` | `right`, `ctrl+f` | Move the cursor one grapheme cluster right |
| `editor.home` | `home`, `ctrl+a` | Move the cursor to the start of its line |
| `editor.end` | `end`, `ctrl+e` | Move the cursor to the end of its line |
| `editor.word_left` | `alt+left` | Move to the start of the word before the cursor |
| `editor.word_right` | `alt+right` | Move past the word after the cursor |
| `editor.history_prev` | `up` | Step back through earlier drafts, then into the lines of the draft |
| `editor.history_next` | `down` | Step forward again, back to the draft being typed |
| `editor.newline` | `shift+enter`, `alt+enter`, `ctrl+j` | Start a new line without sending the draft |
| `editor.kill_to_start` | `ctrl+u` | Delete from the start of the line to the cursor |
| `editor.kill_to_end` | `ctrl+k` | Delete from the cursor to the end of the line |
| `editor.delete_word` | `ctrl+w`, `alt+backspace` | Delete the word before the cursor |
| `editor.complete` | `tab` | Fill the popup's selected command, or complete the command or workspace-path prefix |
| `editor.accept_suggestion` | `ctrl+right` | Add the visible suggestion to the draft |
| `editor.history_search` | `ctrl+r` | Search the drafts sent before this one |
| `editor.external` | `ctrl+g` | Edit the draft in `VISUAL` or `EDITOR` |
| `prompt.allow` | `y` | Approve the parked tool call once |
| `prompt.allow_always` | `a` | Approve this call and every later call like it |
| `prompt.deny` | `n` | Deny the parked tool call |
| `prompt.dismiss` | `esc` | Deny the parked call; a question stays open |
| `prompt.confirm` | `enter` | Confirm a multi-choice question |

The table is `ACTIONS` in `crates/tui/src/keys/bindings.rs`, each action with the scope it belongs to.

Most terminals send Shift-Enter as a plain Enter, which submits. `alt+enter` and `ctrl+j` are therefore the reliable newline keys, and `shift+enter` answers only where the terminal reports the shifted form.

`up` and `down` move between draft lines and enter history at the first or last line. `shift+up`, `shift+down` and the page keys scroll the transcript. `shift+tab` steps the reasoning effort. Plain Tab completes a slash command name or a local path. An open prompt receives its own input before the composer handles keys.

## The editor

With the default local endpoint, Tab completes paths relative to the session's working directory. Add `@` to insert a file mention. Names with spaces are quoted, and repeated Tab cycles matching filenames. Slash commands take priority. An explicit socket disables this local lookup.

With `editor.suggestions = true`, the composer can show a continuation from messages you already sent in the current session. Ctrl+Right adds it to the draft and Esc dismisses it. Editing the prefix withdraws a stale suggestion. The suggestion provider runs locally and sends no request.

Press Ctrl+G to edit the draft with `VISUAL`, or `EDITOR` when `VISUAL` is unset. The command accepts quoted arguments without shell expansion. Horon hands the editor a private temporary file and reads it back as UTF-8 after a successful exit. A failed edit leaves the draft intact. The edited text stays in the composer until you send it.

The input line grows with the draft: one row per line, up to eight rows, and a taller draft scrolls inside its own pane. The conversation keeps at least three rows while the draft grows, so a long message never takes the whole screen.

`enter` sends and the newline keys start a new line. With `editor.submit_on_enter = false` the send key starts a new line instead and nothing is sent from the keyboard, which is one way to type a multi-line message without a paste.

A draft whose last character is a single `\` is not sent: the backslash comes off, a newline takes its place, and the next `enter` sends the finished message. The draft stays in the editor, so the escape continues the line in place. Two backslashes at the end are sent as typed.

A paste is taken as one block. The client turns on the terminal's bracketed-paste mode while it runs, so a three-line paste arrives as one `Paste` event and stays one draft instead of being sent as three messages. The mode is turned off on the normal exit, on the error path, and from the panic hook, because a client that dies with it on leaves the shell pasting escape codes into the next program. A terminal that does not support the mode answers nothing and the paste arrives as ordinary keystrokes.

The last hundred drafts are kept for `up` and `down`. A walk starts from the newest entry and stepping past it returns the draft that was on screen when the walk began, so `up` twice never loses what was being typed. One entry is kept per distinct message in a row, and editing the draft ends the walk: the next `down` does not replace the edit with the entry the walk started from.

`ctrl+r` opens those drafts as a list, newest first, for reaching a draft that is too many `up` presses back. Each row is the draft's first line, with its line count when it has more than one, and the whole draft in the preview pane. The list is the same picker the model and session commands use, so typing filters it and `enter` puts the chosen draft back in the editor without sending it; `esc` closes the list and keeps the draft that was there. The drafts are the client's own, so the list is complete the moment it opens.

The cursor moves between grapheme clusters, so a flag emoji or a syllable made of combining characters stays together. The editor measures these clusters in display cells. With mouse capture enabled, clicking in the editor moves the cursor to the cluster at that cell. When a line exceeds the pane, the editor scrolls horizontally to keep the caret visible.

## The transcript

A tool call is drawn as one head row and the rows under it. The head carries the call's name, a short argument summary and the outcome. The summary is the first interesting field of the call's own JSON, taken in the order `command`, `file_path`, `path`, `pattern`, `query`, `url`, `name`, `prompt`, cut to the space the row has left. The outcome is `◐ running`, `✓ done` or `✕ failed` with the elapsed time (`1.4s`, `2m 05s`); a call the approval gate refused and nobody has decided yet is `◐ needs approval` with no time, because it neither ran nor failed.

Results are matched to calls by call id, so two calls that ran in parallel cannot swap their results, and a result whose start this client never saw gets a row of its own instead of being attached to another call's. A finished call keeps a preview of its output under the head: three rows by default, each cut to 160 cells, and `+N lines` for the rows the preview does not show. A failure puts its message on its own row instead of in the head, because the message is usually longer than the pane and would push the argument and the duration off the right edge.

The marker of the newest call whose preview hides something names the key that shows the rest: `+N lines · ctrl+o expands`, where the key is the one the map in force gives `app.expand_tool`. The key draws that call's whole output and puts the preview back on the next press, and it moves the view to the end of the conversation, where the block it changed usually is. With nothing kept to show it says `no tool output to expand`. The whole text is kept for the sixteen most recent cut calls, at most 64 KiB of each; longer output stops on a character boundary and its last kept line reads `… output truncated`. A call cut only at the right edge of a row counts as cut and carries `cut` instead of a row count, and a call whose text has fallen out of the window stops naming the key rather than promising a view that is gone.

A call the approval gate refused is drawn with one short label rather than the daemon's whole sentence, because the prompt beside it already asks the question, and the label follows the request: while the decision is pending the head row says `◐ needs approval`; once it is answered the row states the answer, `• allowed`, `• allowed always`, `✕ denied` for `n` or `esc`, or `✕ not answered` for a prompt that went away unanswered (a lost connection, a cancelled turn). None of those three says `failed`, because the call never ran: the gate held it and the model has to issue it again. Answering that prompt does not run the call: the decision is recorded and the model has to call the tool again, and the transcript says so.

A `bash` job with `notify_pattern` displays live process-output notices in the launching session's transcript. Each notice identifies the job and its stdout or stderr stream.

Set `transcript.turn_usage = true` to show elapsed time after each turn you start in this client. The footer also shows total input and output tokens when the provider reports them.

The client marks avoidable prompt cache misses after the request that caused them. A marker shows the cause and the approximate number of tokens resent, without prompt text or cache identifiers. A model or provider switch, expiry, or a documented invalidation leaves no marker. Set `[features] kv_cache_miss_notices = false` in the daemon's `config.toml` to turn off the reports.

The conversation can be read back. `pageup`, `pagedown`, `shift+up` and `shift+down` hold the view, `ctrl+end` returns to the end, and the mouse wheel moves three rows a notch while `transcript.mouse_scroll` holds the capture, which is the default. While the view is held the bottom row carries `↓ N rows below`, and it adds `· new output` once a row has arrived that the held view has not shown. Taking the view off the end sets the baseline for that count, so the first `↓ N rows below` counts from the row the user was looking at.

Scroll to an older retained tool result and press Ctrl+O to expand or fold it. Alt+T hides or restores completed tool rows. Running calls and pending approvals remain visible.

Assistant text is rendered as Markdown: headings, emphasis, inline code, lists, block quotes, fenced code and horizontal rules. A list is nested up to six levels, two columns or one tab per level, and the nesting is drawn from the indent the model wrote. Fenced code keeps its text as typed, and the opening fence's first word is drawn as a language label once the fence line is complete, so a half-received ` ```ru ` does not flash a label that then changes. A table, a link and any syntax the renderer does not claim stay as the source the model wrote, because dropping the text would lose what it said. Wrapped rows keep the indent of the row they continue, so a nested item still reads as nested after a break. A new turn starts one blank row below the previous one.

A `diagram` fence can draw a one-line flow such as `load -> parse -> render`. It accepts up to eight labels, with at most 24 characters and 24 display cells per label. The source must fit within 4 KiB and the rendered flow must fit the pane. Incomplete, malformed and oversized flows remain readable as source. Set `transcript.visual_blocks = false` to show every flow as source.

`transcript.markdown = false` turns the rendering off and draws the same text with its markers as typed.

Every message can carry one short note of the reader's own, drawn under it as `note: …` in the accent colour. `/note <text>` notes the newest message. `/note add [<address>] <text>` notes the addressed message or the newest one. `/note edit <address> <text>` replaces an existing note and refuses a message that has none. `/note remove [<address>]` deletes a note. `/note` lists the notes of the session with their address and a quote of the message. An address is `u<n>` or `a<n>`, the message's position among user or assistant messages in this session. Each note stores the session id and, when available, the stored message id. If either side lacks that id, as with live messages and older daemons, the client matches whitespace-normalised text and its occurrence number among identical messages. The client refuses an absent address when adding a note. A note whose message is gone can still be removed by its original address. The client uses the separate answer from a history reply when present and recognises the daemon's reasoning markup in older replies. `transcript.show_reasoning` continues to control reasoning visibility.

Notes are the reader's own file: no command sends them, they never join the conversation, and they are not part of a session record. `/note prune` asks the harness about every session that holds a note and drops the notes of ids it no longer knows; a session that only answers because it is archived keeps its notes. A save reads the file again under a lock and merges only the sessions this client changed, so a second `horon-tui` writing at the same moment keeps its own. A file this build cannot read is moved to `tui-notes.json.corrupt` instead of being overwritten, and a file from a newer schema is reported and left untouched.

## Session todos

The `Todos` panel shows the attached session's saved tasks, starting with the active item and then blocked items. A label shows each item's group and a blocked item includes its reason. The heading counts completed items and reports how many open items are outside the visible rows. On a short terminal the panel yields space to the conversation and input.

The client loads the list when you attach and reads it again after a saved change. Switching sessions clears the old list. A daemon or bridge without live todo support leaves the panel hidden.

## Turn state

One turn runs at a time. Enter during a running turn queues the text instead of sending it: the transcript shows it as a `queued: <text>` row, and the message is sent when the running turn ends, in the order the messages were typed. Cancelling the turn keeps the queue, because the queue belongs to the next turn rather than to the one that stopped.

`esc` sends the queued messages into the running turn instead of stopping it: each `queued: <text>` row becomes the message the model received, in the order the messages were typed, and a notice says `sent into the running turn`. With nothing queued there is nothing to hand over, so `esc` interrupts the turn; it does nothing when the client is idle, so a stray key cannot end the session. One steer waits at a time, so a second `esc` while the answer to the first is still outstanding interrupts instead of stacking messages the client could not tell apart later. A refusal is one warning line, `could not send the queued messages into the turn` followed by the harness's own sentence, and the messages go back in front of the queue in their own order. A steer still unanswered when the client moves to another session does not hold the new session's `esc`, so the first `esc` there steers again; its answer is still handled when it arrives, where an acceptance changes nothing on screen and a refusal is one warning with the harness's wording and puts nothing back, because that text belongs to the session the client left. `ctrl+c` cancels the turn when one is running and leaves when none is, and `ctrl+d` always leaves.

A turn that ends while the daemon still holds something sent into it says `the turn ended before reading everything sent into it; the next turn carries the rest`. The notice names no count and no key, because what the daemon counts is every item it holds for the session: a steer of several messages is one item, and a background result or another client's message is one each.

A model chosen with `/model` and an effort chosen with `/effort`, `shift+tab` or the ladder list are not sent during a turn, because the daemon answers those requests only after the turn releases the agent. They leave when the turn ends, in one order: the model, then the effort, then the oldest queued message. Until then the client says `the turn is still running; the model changes to <name> once it ends` or `the turn is still running; the reasoning effort changes to <level> once it ends`. `/new`, `/switch` and `/resume` wait for the same reasons: during a turn they answer `the turn is still running; /<name> works once it ends`, while a held change is still being applied they answer `a held model or effort change is still being applied; /<name> works in a moment`, and with the connection gone a warning says there is no session to change to.

A failed send is not drawn as a message. The client takes the user row back, puts the text in the input line (or queues it, when the input line already holds something else), returns the status to `idle` and shows one warning line with the reason. The queue is held until the next turn ends or the user submits again, so a harness that keeps refusing cannot turn a refused send into a loop.

When the connection goes away the status line says `disconnected`, the queue is not sent, the prompts of the turn are withdrawn, and submitting is refused with the text left in the input line. Ctrl-C leaves from there.

## Prompts

An approval and a question are drawn above the input line and take every key until they are answered.

Approvals are a queue. A second call that needs approval while the first is on screen waits behind it, and the prompt on screen adds a line `N more prompt waiting`, singular as `1 more prompt waiting`, so a second approval is not read as a repeat of the first. The prompt shows the call's action and, when the transcript can pair it with the request id the refusal wrote, the arguments of that exact call; a prompt is never shown another call's command.

`y` approves once, `a` approves and asks for every later call like it, and `n` or `esc` denies. An approval outlives its turn: the request stays queued in the daemon with its own deadline, so a lost prompt would be the only way to answer it taken away. Approving does not run the call. The transcript records the decision as `you allowed <action>: the model has to call it again before it runs`, or `you allowed <action>: recorded once (the daemon keeps no always-rule), so the model has to call it again` for the `a` key, and `you denied <action>: the call will not run` for a denial, because only the model calling the tool again runs it.

The `ask` tool's questions are drawn one at a time with a `(1/2)` counter. A single-choice question is settled by the digit of its option; a multi-select one toggles digits and confirms with `enter`, and the chosen options carry `[x]`. A recommended option is marked `(recommended)`. `esc` leaves the question open: the tool has no deny path and resolves its own deadline, so closing the prompt would take away the only way to answer it. Answering every question adds `you answered the ask tool (N answers): the call resumes`, because the tool waits for the answer on this connection instead of being remembered for a retry.

Prompts are withdrawn when their turn is cancelled, when the turn ends (questions only), and when the connection is lost. The withdrawal leaves one notice line naming what was dropped, as in `the turn was cancelled; 2 prompts withdrawn`.

## The slash popup and `/help`

Typing `/` opens a list of the commands the typed letters match: the same commands `/help` lists, skills included, ranked best match first, with their descriptions. The filter is a subsequence match, so `/mdl` finds `model` and `/rvw` finds `review`, and a name that still starts with the typed letters ranks above one the letters only resemble. The selected row is marked with the input line's own marker in the left column, and the characters the letters matched in the name are underlined on top of the row style. The list shows at most eight rows and moves its window with the selection; `up` and `down` move it, `tab` fills the selected command, and `esc` closes it. The popup is the topmost surface, so the first `esc` always closes the popup and only a second one acts on the turn. While the popup is open the completion hint is not drawn, because the list already says what the hint would. The list closes itself only once a row's name is exactly what was typed and no other matching row's name starts with it: `resume` and `resume_all` still leave a choice, while a row the letters only resemble is no choice at all and does not keep a finished name open.

`/help` prints the commands grouped by where they come from, with the description column aligned, then the keys. The key list is built from the key map in force rather than from the shipped defaults, so a rebound key shows its new binding and an action a file turned off shows as `(unbound)`. `/help <command>` prints one command instead of the whole listing: its name with the argument hint the command declares, its description, the group it comes from, and, for a command file the built-in shadows, the note that the built-in runs rather than the file. A leading slash is accepted, so `/help /model` reads the same as `/help model`, and a name nothing answers to is reported as an unknown command that points at `/help`. `/keys` prints the key half alone: the keys in force and what each action does, in the order `ACTIONS` declares, with an unbound action marked `(unbound)`.

## Pickers

The model, effort, session, skill, link and draft lists share a picker. It shows a title, a filter line, one row per choice and the selected row's position. Enter chooses a row, Esc closes the list, the arrow or page keys move, and Backspace edits the filter. Text typed while the picker is open goes into its filter.

The filter is the subsequence match the slash popup uses, over a row's name and its second column and, for a session, over the conversation lines its preview carries. Those lines are searchable and a preview's header line is not, because the header names the directory and the time every row of one directory shares. Matches are ranked, and rows that rank the same keep the order the list gave them, so an empty filter shows the list in its own order. A filter that leaves nothing says `nothing matches that filter`.

The panel grows by a preview pane for the selected row: a rule line and up to eight lines of that row's own text, cut with `…` where the panel is shorter than the text. Its height comes from the row carrying the longest preview rather than from the row the selection is on, because a panel that grew and shrank as the selection moved would make the list jump under the arrow keys. A screen too short for three list rows and the preview together keeps the list and drops the preview first.

The model list has the model in force marked `(current)` and the selected row carrying the input line's own marker (`❯` in the Unicode preset). Typing filters the list, `enter` picks the row and `esc` closes it. Its preview names the model and the provider, says whether the row is the model in force, and says what `enter` does. Picking sends the switch to the daemon, and the status line changes only when the daemon confirms it with the session's identity, so a refused switch leaves the old model showing.

`/model <name>` switches without the list.

The effort list is the ladder the client offers, `none`, `minimal`, `low`, `medium`, `high`, `xhigh` and `max`, with the level in force marked `(current)` and one preview line per row saying what the level asks of the model. The cursor and the preview open on the marked row, so `enter` confirms the level in force instead of dropping to `none`; a session that reports no level, or one the ladder does not hold, opens on the first row. The rows are the client's own, so the list is complete the moment it opens.

Set `models.shortlist` to the model IDs you want to cycle. Ctrl+P moves forward and Ctrl+Alt+P moves backward, wrapping at either end. The client asks the daemon for its model catalog before cycling and skips unlisted IDs.

`/switch` and `/resume` open the stored sessions under different titles, newest first. Each row carries the session's title (or its first user line, or its id) and the time it last changed, flush with the row's right edge so the times read down the list; a conversation too narrow for both keeps the name. The session in force is marked `(current)`. The preview opens with the directory and the time and follows with the newest six messages as `you: …` and `assistant: …` lines, taken from the newest twenty stored messages; an injected `<system-reminder>` message is left out, because the user never wrote it. A session the daemon cannot read back shows its title, or its id when it has none, and a head-only preview.

In the session picker, Ctrl+P pins or unpins the selected row. Pinned sessions appear first, with newest sessions first within each group. `/pin [id]` and `/unpin [id]` act on the attached session when the ID is omitted. Pins are saved locally for the connected daemon and survive a client restart.

Choosing a row, or typing `/switch <id>` or `/resume <id>`, attaches at once, and the daemon also accepts a unique prefix of six or more characters. Attaching clears the screen of the old session, replays the stored conversation as user and assistant messages, adds `started a new session <id>` or `resumed <id>`, and takes the model, provider, effort and title from the new session's identity, so the status line keeps showing `provider/model`. `/new` asks for a session in this directory and moves to it the same way; the session that was left stays stored, and the client drops the events of every session it has left, so neither conversation's rows reach the other. A session change waits while a turn runs or a model or effort change is still being applied, and a new session left without a message cannot be resumed, because the daemon does not keep it.

In `/switch` or `/resume`, select a saved session and press Ctrl+A to archive it. Leave the current session before archiving it. `/archived` lists archived sessions in the current directory. Press Enter on a row to restore it to the recent list.

`/skills` lists the skills the harness reported for this session: the name, the harness's description in the second column, and a preview naming the file the skill was read from and the tools it may use, ending with the line that says `enter` puts the invocation in the draft. Choosing a row writes `/skill:<name>` and a trailing space into the editor without sending it, so the request after it is the user's to type.

`/links` lists HTTP and HTTPS links from the eight most recent assistant answers, newest first. Choose a row with Enter to open the displayed URL in the system browser. Esc closes the list. The picker skips malformed URLs, local files and links with embedded credentials.

`/mcp` lists servers from the daemon's trusted configuration for the attached workspace. `/mcp probe <server>` opens a separate connection to the named enabled server and reports its transport and advertised capabilities. The probe reads the tool list without calling a tool. An untrusted or disabled server cannot be probed from the TUI.

In a window too small for an overlay, the lists and the settings screen draw one line instead of the panel: `list hidden: window too small, esc closes` or `settings hidden: window too small, esc closes`. The overlay still owns every key, so `esc` closes it.

## The status line

The bottom line is assembled from named segments. A segment with no data draws nothing, and an unknown name is reported and skipped so the rest of the line still draws.

| Segment | Shows |
| --- | --- |
| `status` | What the turn is doing: the client's own `idle`, `running`, `requires_action` or `disconnected`, or the session status the daemon reported |
| `model` | `provider/model` with the reasoning effort after it, or whichever of the two the daemon reported |
| `session` | The session id, shortened |
| `path` | The working directory, with the home directory shortened to `~` and long paths cut at a segment boundary |
| `tokens` | Input and output tokens of the last request, as `↑in ↓out` |
| `context_pct` | Share of the context window used, where `statusline.context_window` overrides the window the runtime reports; hidden when neither is known |
| `usage` | Tokens the whole session has spent, as `Σ↑in ↓out`; opt-in, and only the metrics the runtime recorded are drawn |
| `cost` | The daemon's own spend estimate as `$0.00`; opt-in, and hidden when the daemon publishes none |
| `time` | Local wall-clock time as `HH:MM` |
| `pending` | A parked approval or question |

`status` is the one segment whose words are not all the client's own. It writes `idle`, `running`, `requires_action` while a prompt waits, and `disconnected` when the connection is gone; it draws the daemon's `generating` as `running` and its `cancelled` restatement of a stopped turn as `idle`; and it shows any other non-empty session status as the daemon sent it, so a word this page does not list can appear there. `attached`, right after the client attaches to a session, is one of those.

`statusline.preset` picks an arrangement:

| Preset | Left | Right |
| --- | --- | --- |
| `default` | `status`, `model`, `path` | `tokens`, `context_pct`, `time` |
| `minimal` | `model` | `status` |
| `custom` | Whatever `statusline.left` names | Whatever `statusline.right` names |

The `model` segment carries the reasoning effort the daemon reported for the model, joined by the preset's separator (`·` unless the preset draws another of its own): `provider/model · high`. A missing or blank effort draws nothing, and an effort with no model name beside it is not drawn at all. The client asks the harness what the session is running before the first keystroke, so the first frame already names the model and its effort; a report the daemon refuses leaves both unknown and prints one warning, `could not read the runtime info: <error>`, and the identity event that follows every attach or new session fills the same two fields.

`usage` and `cost` are opt-in: no preset draws them, and only a custom `statusline.left` or `statusline.right` array can. Both read what the runtime reported, so a metric nobody recorded draws nothing rather than a zero: a provider that omitted cache accounting gets `Σ↑in ↓out` with no cache column, and a daemon without a price table publishes no estimate, which is why `cost` is hidden on this build. Neither one causes a request per frame: the report is read when the client moves to a session and again when the model moves.

When the line does not fit the terminal, the least important segment is dropped first. Priority runs from `time` (dropped first), `session`, `tokens`, `cost`, `context_pct`, `usage`, `path`, `pending`, `status`, to `model` (dropped last). The last remaining segment stays even when it is wider than the pane, because a clipped model is more useful than an empty line, and what it gives up first is the effort, so the cut lands inside the model name instead of after a separator that names nothing.

## Commands

A line starting with `/` is resolved in this order: a built-in command, a `skill:` address, a command file, a harness command, then a bare skill name. A built-in owns its name outright, so a command file with that name is listed with the built-in winning and never runs. An unknown name is an error that points at `/help`, and a line starting with `//` is sent as the literal message with one slash removed.

| Command | Effect |
| --- | --- |
| `/archived` | Restore a session from this directory's archived list |
| `/artifact [id]` | List retained artifacts for this local session or open a private copy |
| `/clear` | Clear only the transcript pane |
| `/clear-context` | Start a fresh context for the model in the current session and keep the saved conversation |
| `/compact` | Ask the configured model to summarize older context in place and keep the saved conversation |
| `/copy` | Choose which text to copy |
| `/effort [level]` | Set the reasoning effort, or pick one from the ladder |
| `/exit` | Leave, the same as `/quit` |
| `/help [command]` | List the commands, where each one comes from, and the keys in force, or describe one command |
| `/jobs` | Inspect the current session's background work and cancel a selected terminal job |
| `/keys` | List the keys in force and what each does |
| `/links` | List links from recent assistant answers and open the selected URL |
| `/mcp [probe <server>]` | List trusted MCP configuration or explicitly test a named connection |
| `/model [name]` | Pick a model from a list, or switch to the one named |
| `/new` | Start a new session in this directory |
| `/note [add\|edit\|remove\|list\|prune] [address] [text]` | Keep, list or remove a local note on a transcript message |
| `/pin [id]` | Pin the attached or named session to the top of the session list |
| `/queue` | Edit, remove or reorder waiting messages |
| `/quit` | Leave |
| `/record [path\|stop\|status]` | Start, stop or inspect a local transcript recording |
| `/rename <title>` | Save a title for the attached session and update the window title after the daemon confirms |
| `/replay [path\|stop]` | Replay a recording locally or return to the live session |
| `/resume [id]` | Replay a stored session, or pick one from the list |
| `/settings` | Open the settings screen |
| `/skills` | List the skills the harness reports, and put one in the draft |
| `/swarm` | Browse the attached session's direct children and return to a parent |
| `/switch [id]` | Move to another stored session, or pick one from the list |
| `/unpin [id]` | Remove a session from the pinned group |
| `/voice` | Dictate to the draft or play text on this device |

Use `/voice consent` in the attached session before recording. Press Ctrl+Space or run `/voice` to start or stop. `/voice cancel` discards the recording and `/voice forget` withdraws consent. Recognized text goes into the draft and only goes to the model if you submit it. `/voice say <text>` and `/voice read` play text on this device.

Dictation needs a local `whisper-cli` executable and a `ggml-*.bin` model under `$HORON_HOME/models/`. On macOS it uses `ffmpeg` for capture and `say` for playback. On Linux it uses `arecord` and `espeak-ng` or `espeak`. `/voice status` lists missing requirements. Install any required programs or models yourself. Text input works on systems without these programs.

The table is the built-in command list (`Local` in `crates/tui/src/commands/mod.rs`): twenty-nine commands the client answers itself, in name order.

In `/queue`, Enter moves the selected message into the composer, Delete removes it, and Alt+Up or Alt+Down moves it in the queue. A message already being sent is outside the list. The edited draft can be sent again when ready.

`/jobs` lists the background work this session is still running, read from the daemon each time rather than replayed from the transcript, so a job that started or ended on another client shows up as it stands. Each row shows the command, the job kind and its newest output. Delete cancels the selected background terminal; the session and every other job keep running, and Esc closes the list without cancelling anything. A tool-call job is listed but cannot be cancelled from this list, and the list says so instead of sending a request the daemon would refuse. Switching sessions clears the rows, because a job's call id only means something in the session that started it.

`/artifact` lists retained artifacts for the current local session. Enter `/artifact <id>` or paste the `artifact://<id>` pointer to edit a temporary copy with `VISUAL` or `EDITOR`. The retained file stays unchanged and the temporary copy is removed on return. An explicit socket or remote bridge disables local artifact access.

Copy uses an OSC 52 clipboard request. Set `HORON_TUI_OSC52=0` to turn it off, or set `HORON_TUI_OSC52=1` if your terminal forwards these requests through a multiplexer or SSH. When the terminal cannot accept the request, the text stays available for manual selection.

Enter `!command` to run a shell command in the TUI process's working directory. The transcript shows the local directory before the command starts. `!!command` leaves only the directory and exit status in the transcript. Both forms keep the command and output out of the model conversation. The client runs one local command at a time and stops it after ten seconds. Retained output is limited to 200 lines and 32 KiB.

`/record` starts a private local recording of submitted messages, completed replies and tool outcomes. `stop` ends the recording. The file omits unsent drafts, approval answers, reasoning and tool output, and has a 2 MiB limit. `/replay` opens a recording without submitting any recorded input: the playback is local only, so nothing it shows is sent to the harness. Space pauses, the arrow keys seek, `+` and `-` change speed and Esc returns to the live conversation.

The harness commands run in the attached runtime and never reach the model as a prompt. `/help` draws one row per command with its description in one aligned column: the built-in commands first, then the harness names in name order, then the command files, then the skills the harness reported, then the keys. The transcript draws the whole listing as a single notice, so its first row carries the notice's bullet and every later row is padded by two cells; the excerpt below is the listing without that pad.

```text
built-in commands:
  /archived            list archived sessions in this directory; Enter restores one
  /artifact            open a retained artifact of this session by id in $VISUAL or $EDITOR, as a private copy
  /clear               clear only the transcript pane
  /clear-context       start fresh model context and keep the saved conversation
  /compact             summarize older model context and keep the saved conversation
  /copy                copy a recent answer or code block to the terminal clipboard, or list them
  /effort              set the reasoning effort, or pick one from the ladder
  /exit                leave, the same as /quit
  /help                list the commands and the keys in force
  /jobs                list the background work this session is running, and cancel a terminal
  /keys                list the keys in force and what each does
  /links               list the http(s) links in recent answers and open one
  /mcp                 list trusted MCP servers or connect to one with /mcp probe <server>
  /model               pick a model, or switch with /model <name>
  /new                 start a new session in this directory
  /note                note, list or remove a local note on a transcript message
  /pin                 keep a session at the top of the list, or pin the attached one
  /queue               edit, remove or reorder the messages waiting for the turn to end
  /quit                leave
  /record              record the conversation to a local file, or stop (stop, status)
  /rename              save a title for the current session
  /replay              play a recorded conversation (path, stop), or the newest one
  /resume              replay a stored session, or pick one from the list
  /settings            open the settings screen
  /skills              list the skills the harness reports, and put one in the draft
  /swarm               list this session's swarm children and the parent it returns to, and attach to one
  /switch              move to another stored session, or pick one from the list
  /unpin               take a session out of the pinned group
  /voice               dictate to the draft or play text on this device
harness commands:
  /agents              list the agent models, and switch one for this session or every session
```

Which harness names appear, and how many, is whatever the attached build answers to: the list comes from the daemon on the other end of the socket, not from this page.

Command files appear in `/help` with their argument hint and description. With no command files at all, `/help` says where to put one.

`/switch` moves to another stored session, and a model choice is `/model`, with the name or with the list `alt+p` opens as well. omp's `/switch` changes the model instead; here the two are separate commands and `/switch` never touches the model of the session it leaves.

`/skills` lists what the harness reported for this session. A listed name answers as `/skill:<name>` and, where no built-in, harness command or command file already owns that name, as its bare name too. The line expands before it is sent: to `Read skill://<name> and follow its instructions.` with nothing after the name, and to `Read skill://<name> and follow its instructions for this request: <text>` when words follow it. The harness API has no call that activates a skill, so a chosen skill is sent as that read request and works as far as the session can read the address. omp loads the skill text itself and sends it; this client asks for it instead.

`/effort` takes one level from the ladder — `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, in any case — and refuses any other name before a request leaves the client, listing the ladder in the warning. With no level it opens the ladder list, and `shift+tab` walks the same ladder from the level in force and wraps past the last one. The ladder is the client's list and not the provider's, so which levels exist is the provider's decision: a level it does not have comes back as an error and the status line keeps the level it had.

`/keys` prints the key half of `/help` and nothing else, and a listing taller than the pane is drawn from its first row rather than its end until the next send or `ctrl+end`.

`/context` is the one harness command the client draws as a measured panel instead of printing. The daemon answers with the next request's material and an estimate per part, and the client keeps the numbers and drops the text: one header row reads `context · ~8.7k of 200k tokens (4%)`, then one row per part the request carries — `system prompt`, `tools` with how many the request sends and how many of them are deferred, `messages` with how many it holds, and `free` — each with a bar scaled to the whole window, so the section that grew is the bar that grew. A part that costs anything always shows at least one cell, and an estimate that reaches 80% of the window adds `near the limit` in the warning colour. The numbers are estimates from the daemon's own estimator over the prompt, the tool catalog and the visible conversation, not the provider's usage report. A window the build cannot name drops the percentage and the `free` row, and a pane too narrow for a bar keeps the rows and drops the bars. A daemon older than the breakdown answers with its message, and that message is what the client shows.

Every other harness command prints the message the daemon wrote, except one whose message is its own payload serialized as JSON — `/auth` and `/swarm` are the common ones. That answer is drawn from the `data` beside it as a labelled block: the command's title in the accent colour, then `label  value` rows aligned per nesting level, lists under a `key · count` heading with one named row per element, `yes`/`no` for switches, `—` for an absent value, and a note last. A string under a secret-shaped key (`api_key`, `*_token`, `*_secret`, `password`) is drawn as `— hidden`, while a variable's name such as `api_key_env` is not a secret and stays readable. A long value wraps under its own value column. A command that answers in prose — `/skills`, `/plan`, `/remote pair` — keeps the words it wrote, and no payload is ever drawn as JSON.

## The settings screen

`/settings` is generated from the settings registry: one row per key, grouped into the Appearance, Status line, Editor and Transcript tabs, with the label and description the registry declares. Each row also shows the layer the value came from.

A fifth tab, `Daemon`, is not built from the registry: it lists the daemon values this client may only read, which the daemon reports through `get_effective_settings` on the harness API. Each row shows the label, the value in force and where it came from (`daemon default`, `cli <FLAG>`, `env <NAME>`, or the config file path). Nothing on this tab is editable and `enter` there changes nothing, because the daemon owns those values; `r` asks for them again. Opening the screen asks once, and the tab says `reading the daemon settings` until the answer lands. A daemon or bridge that predates the request cannot report them, and the tab then says so while every local tab keeps working. A read that fails settles the tab instead of leaving it on that sentence: it says the settings could not be read and keeps the reason on the footer.

Press `/` to search all tabs by setting label, key or description. The results list shows the tab for each match. Press Enter to keep the filter and return to the rows. Searching and moving among results leave the values alone. A filter covers only the registry's rows: it moves the screen to the tab of the match it lands on, and no daemon row is ever listed.

| Key | Effect |
| --- | --- |
| `/` | Search settings across tabs or edit the current query |
| `up`, `down` | Move between rows |
| `left`, `tab` | Move to the previous tab |
| `right`, `shift+tab` | Move to the next tab |
| `enter` | Open the editor or the pick list of the row, or confirm |
| `r` | On the `Daemon` tab, read the daemon's values again |
| `esc` | Close the editor or pick list, then clear the search, then close the screen |

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

- FreeBSD ships `horon-agent` only, so the client is not installed there and bare `horon` reports the missing sibling.
- On Linux and macOS the handoff is one process replacing another, so a client that exits takes the terminal command with it and the exit code is the client's. On Windows the launcher starts the client and exits with code 0 at once, so its exit code says nothing about the client.
- `SIGTERM`, `SIGHUP` and `SIGQUIT` restore the terminal through the same path as `/quit` — mouse capture and bracketed paste off, the shell's title back — and then re-raise, so a wrapper still sees the client die by the signal (143 for `SIGTERM`). `SIGKILL` cannot be caught: a client killed that way leaves both modes on. Windows installs no handler, because its console is torn down with the process.
- A theme JSON file edited on disk is re-read by the theme loader on its one-second check, but the screen is not redrawn for it: an idle client keeps the picture it has until the next key, event or settings reload.
- A table and a link are shown as the source the model wrote, and a list indented by one or three spaces rounds down to a whole number of levels, because a level is two columns.
- While a prompt is open the keyboard belongs to it: slash commands cannot be typed, and a letter that is a prompt key answers the prompt instead (an `a` is "allow always", not the start of `/clear`).
- A draft that ends in a single `\` is not sent as typed: the backslash continues the line, and `\\` is sent as two backslashes.
- The effort ladder is the client's, so a provider that does not have a level refuses it, and the status line then keeps the level it had.
- The effort is stored on the server every time it changes, and a change made during a turn waits for that turn to end, so a level chosen mid-turn is not in force until then.
- The client asks the harness for the session's skills after every connect, every attach and every new session, and the daemon reads the global skill directories again for each of those answers.
- A turn the daemon starts by itself sends this client no start event, so the `esc` delivery marker does not move: messages sent with `esc` stay drawn as sent into the turn that ended.
- Starting with `--session <id>` attaches without drawing the stored conversation; `/resume <id>` replays it.
- With the popup closed, Tab completes built-in, harness and command-file names and can complete local workspace paths. Choose a skill from the popup or `/skills`.
- Only the newest kept tool call expands. Expanding moves the view to the end of the conversation, a copy longer than 64 KiB is cut with `… output truncated`, and the `ctrl+o expands` label is left out when the marker cannot hold it on one row, which happens below 29 columns for a three-digit row count.
- A second `/new` typed before the first answer arrives reports its session as resumed, because the client holds one flag for the session it is starting.
- The session list asks the daemon once or twice for every session it shows, so a directory with many sessions delays the list and the events behind it while they load.
- The session preview reads the newest twenty stored messages, leaves out injected `<system-reminder>` messages, and shows its head line alone for a session the daemon cannot read back.
- A new session left without a message cannot be resumed, because the daemon does not keep it.
- A skill is sent as a request to read `skill://<name>`, so it works only where the session can read that address.
- `esc` has no hint on screen of its own: `/keys` is where the key and what it does are written.
- `editor.submit_on_enter` is read by the send-key handling and `transcript.show_reasoning` by the transcript; both apply from the next keystroke and the next reasoning delta once changed. A session choice made with `--hide-thinking` or `ctrl+t` outranks that setting until the setting value itself changes, so editing another key in `tui.toml` does not put the setting back. The other transcript settings and the status line take effect as soon as the file reloads, which redraws the screen.
