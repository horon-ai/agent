use super::*;
use std::path::PathBuf;

impl Config {
    /// Create a default config file with comments
    pub fn create_default_config_file() -> anyhow::Result<PathBuf> {
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("No config path"))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&path, Self::default_config_file_contents())?;
        Ok(path)
    }

    /// The commented config template written by [`Self::create_default_config_file`].
    ///
    /// Exposed separately so tests can check that the template we ship actually
    /// parses and documents the options it claims to.
    pub fn default_config_file_contents() -> String {
        let default_content = r##"# horon-agent configuration file
# Location: ~/.horon-agent/config.toml
#
# Environment variables override these settings.
# Run `/config` in horon-agent to see current settings.
#
# Environment-only settings:
#   HORON_AGENT_JOBS_ARTIFACT_RETENTION - "rotate" (default) reclaims the oldest
#       background task artifacts when the byte or file budget is full; "preserve"
#       refuses to start a new background task instead.
#   HORON_AGENT_WORKSPACE_TRUST - "trust" or "deny" for this process only. Project-local
#       MCP config (.horon-agent/mcp.json, .mcp.json, .claude/mcp.json) is skipped
#       unless its current digest was trusted; --trust-workspace records that decision.

[keybindings]
# Scroll keys (vim-style by default)
# Supports: ctrl, alt, shift modifiers + any key
# Examples: "ctrl+k", "alt+j", "ctrl+shift+up", "pageup"
scroll_up = "ctrl+k"
scroll_down = "ctrl+j"
scroll_page_up = "alt+u"
scroll_page_down = "alt+d"

# Model switching
model_switch_next = "ctrl+tab"
model_switch_prev = "ctrl+shift+tab"

# Reasoning effort switching (OpenAI models)
# Defaults: cmd+right / cmd+left on macOS, alt+right / alt+left elsewhere.
# Alt/Option+Left/Right move by word in the input box.
effort_increase = "@EFFORT_INCREASE@"
effort_decrease = "@EFFORT_DECREASE@"

# Speed tier switching: Standard -> Fast -> Ultrafast (OpenAI), Fast on/off elsewhere.
# Ctrl/Cmd+Up/Down still recall prompt history.
speed_increase = "alt+up"
speed_decrease = "alt+down"

# Centered mode toggle key
centered_toggle = "alt+c"

# Jump between user prompts
# Ctrl+1..4 resizes the pinned side panel to 25/50/75/100%.
# Ctrl+5..9 jumps by recency rank (5 = 5th most recent).
scroll_prompt_up = "ctrl+["
scroll_prompt_down = "ctrl+]"

# Scroll bookmark toggle (stash position, jump to bottom, press again to return)
scroll_bookmark = "ctrl+g"

# Auto-poke toggle. Set "" to disable.
auto_poke_toggle = "ctrl+p"

# Optional fallback scroll bindings (useful on macOS terminals that forward Command)
# Leave unset by default; on macOS Cmd+K / Cmd+J move up / down by prompt instead.
scroll_up_fallback = ""
scroll_down_fallback = ""

# Workspace navigation (Niri-style)
# Comma-separate multiple bindings to add aliases.
workspace_left = "alt+h"
workspace_down = "alt+j"
workspace_up = "alt+k"
workspace_right = "alt+l"

# Pane / mode toggles
side_panel_toggle = "alt+m"
copy_selection_toggle = "alt+y"
diagram_pane_toggle = "alt+t"
diagram_pane_visibility_toggle = "alt+shift+m"
typing_scroll_lock_toggle = "alt+s"
# Cycle the diff display mode (unbound by default; /diff does the same).
diff_mode_cycle = ""
# Show the git commit log of the project you are in.
commit_log = "alt+g"
info_widget_toggle = "alt+i"
# Focus the inline swarm panel (list of agents this session manages). Press
# again to cycle agents. While focused: alt+↑/↓ select, alt+o pops the agent
# out to a new terminal, esc exits. Plain typing still goes to the chat input.
# Active only with agents.swarm_spawn_mode = "inline".
swarm_panel_focus = "alt+n"

# Spawn a fresh horon-agent session in a new terminal window, reusing the current
# session's working directory. Companion to the system-wide launch hotkeys.
# `horon-agent setup-hotkey` installs these three global launch hotkeys on macOS,
# Linux niri/Hyprland/sway/i3, and Windows. The Cmd modifier maps to Super on
# Linux and Alt on Windows. Windows also adds the physical Copilot key:
#   Cmd+;        new horon-agent in your home directory
#   Cmd+'        new horon-agent in your last project directory
#   Cmd+Shift+'  new horon-agent self-dev session (last horon-agent repo)
# Default: Cmd+Shift+; on macOS, Alt+Shift+; elsewhere. Set "" to disable.
# Note: some macOS terminals intercept Cmd combos; if so, pick another binding.
# new_terminal = "cmd+shift+;"

# Open the /resume session picker.
# Default: Cmd+B on macOS, Alt+R on Windows/Linux. Set "" to disable.
# open_resume = "cmd+b"

# Built-in voice input: press to record, press again to send the transcript.
# Esc cancels. Needs a Nari API key (NARI_API_KEY or ~/.config/horon-agent/nari.env).
# Default: Ctrl+Space. Set "" to disable.
# voice_input = "ctrl+space"

# /resume picker Enter behavior. Options: "current-terminal" or "new-terminal".
# By default Enter resumes in this terminal; Ctrl+Enter performs the alternate action.
session_picker_enter = "current-terminal"

[dictation]
# External speech-to-text command.
# The command should record/transcribe speech and print the final transcript to stdout.
# You can include any tool-specific flags here too, for example a grammar target.
# Examples:
# command = "~/.local/bin/my-whisper-script"
# command = "~/.local/bin/my-whisper-script --grammar-target code"
command = ""

# How to apply the transcript inside horon-agent: insert|append|replace|send
mode = "send"

# Optional in-app hotkey to trigger dictation. Set to "off" to disable.
# Example: "alt+;"
key = "off"

# Max seconds to wait for the dictation command to finish (0 = no timeout)
timeout_secs = 90

# Extra names or terms to help built-in voice transcription recognize them.
# HoronAgent's own product names are always included.
# vocabulary = ["Kubernetes", "Alice Zhang"]

# Microphone recorder for built-in voice input (keybindings.voice_input).
# Empty auto-detects pw-record, parecord, arecord, rec (SoX), or ffmpeg.
# A custom command must print raw mono 16 kHz signed 16-bit little-endian PCM.
# recorder = "arecord -q -t raw -f S16_LE -r 16000 -c 1 -"

# Copilot-key push-to-talk in `horon-agent companion` (Linux). List keyboard evdev
# nodes to watch, e.g. ["/dev/input/by-id/usb-Foo-event-kbd"]. Holding the key
# while a HoronAgent terminal is focused sends the transcript to that session, even
# without HoronAgent Desktop. Empty disables it.
# global_devices = []

[display]
# Diff display mode: "off", "inline" (default), "full-inline", or "file"
diff_mode = "inline"

# Center all content by default (default: false)
centered = false

# Pin read images to a side pane (default: true)
pin_images = true

# Pin the full session todo list to the top of the chat transcript while it
# scrolls, like the sticky previous-prompt preview (default: false)
# pin_todos = true

# Queue mode: wait until assistant is done before sending next message
queue_mode = false

# Automatically reload the remote server when a newer server binary is detected (default: true)
auto_server_reload = true

# Capture mouse events (enables scroll wheel; disables terminal text selection)
mouse_capture = true

# Enable debug socket for external control/testing (default: false)
debug_socket = false

# Render emoji in terminal-facing TUI and CLI output (default: true).
# Set false here or set HORON_AGENT_NO_EMOJI=1 for ASCII fallbacks.
emoji = true

# Usage percentage wording: "left" (default) or "used".
usage_display = "left"

# Show thinking/reasoning content (default: true)
show_thinking = true

# How to display reasoning/thinking content: "off", "full", or "current".
#   off     - never show reasoning
#   full    - keep every reasoning trace in the transcript
#   current - show only the live reasoning; collapse it once the model commits
#             an assistant message or runs a tool, then show the next one
# When unset, falls back to show_thinking (true => full, false => off).
reasoning_display = "full"

# Markdown spacing style: "compact" (chat/TUI) or "document" (docs-like)
# markdown_spacing = "compact"

# LaTeX rendering: "none" (show source), "unicode" (terminal text), or "image" (typeset PNG, default).
# Image mode uses `latex` + `dvipng`, or `pdflatex` + `pdftocairo` as a fallback.
# When neither toolchain is available, formulas fall back to Unicode.
latex_rendering = "image"

# Show idle animation before first prompt (default: false)
idle_animation = false

# Briefly animate a user prompt line when it enters the viewport (default: true)
prompt_entry_animation = true

# Render swarm/file-activity notifications in a compact single-line form
# instead of the full multi-line card with diff preview (default: false)
# compact_notifications = false

# Show the full agentgrep tool output inline in the transcript instead of just
# the one-line summary (default: false). Useful when you want to read search
# results directly in the chat.
# show_agentgrep_output = false

# Show up to the last three non-empty lines of bash output beneath the tool
# summary (default: false).
# show_bash_output = false

# Show the dimmed technical detail (command, file path, args) next to the
# model-provided intent on tool rows (default: false). When false, tool rows
# with an intent show just the intent; rows without an intent still show the
# technical detail.
# tool_call_details = false

# Occasionally surface a "learn this keybinding" nudge (in a distinct color)
# when you keep doing something the slow way (e.g. /resume) instead of using
# its configured shortcut. Set false to disable all such hints (default: true).
# keybinding_hints = true

# Active sessions manager: pressing Left arrow on an empty input opens a
# picker scoped to live (open) sessions, showing which are still working on a
# response and which are ready for input (default: false). The /active
# command is always available regardless of this setting.
# active_sessions_manager = false

# Include transcripts from other agent CLIs (Claude Code, Codex, Pi, OpenCode,
# Cursor) in the session picker so they can be resumed or imported
# (default: true). Set false to list only horon-agent's own sessions.
# external_sessions = true

# Open /resume filtered to sessions from the current project (same directory,
# subdirectories, and sibling git worktrees) when that project has sessions
# (default: false). Resume is global by default. Press `s` to cycle filters.
# resume_project_scope = false

# Disable specific animation variants by name.
# Examples: ["donut"] or ["donut", "orbit_rings"]
# Legacy aliases such as "three_rings" and "gyroscope" are still accepted.
# disabled_animations = []

# Performance tier: auto/full/reduced/minimal (default: auto)
# auto = detect system load, memory, terminal type, SSH, and apply extra caps for WSL/Windows Terminal
# full = all animations enabled
# reduced = skip idle animations, keep spinners
# minimal = disable all animations, slower redraw rate
# performance = "auto"

# Animation FPS (idle animation): 1-120 (default: 60)
# Runtime policy may cap this lower on slower environments such as WSL/Windows Terminal.
# animation_fps = 60

# Active redraw FPS (processing, streaming, spinners): 1-120 (default: 60)
# Runtime policy may cap this lower on slower environments such as WSL/Windows Terminal.
# redraw_fps = 60

# Label shown for the Alt/Option modifier in copy badges.
# Empty = auto ("⌥" on macOS, "Alt" elsewhere). Examples: "Option", "Alt", "⌥".
# copy_badge_alt_label = ""

# Color theme: "auto" (query the terminal background), "dark", or "light".
# theme = "auto"

# Per-role color overrides. Every color the TUI renders is configurable: the
# named roles below are substituted directly, and the ad hoc shades individual
# widgets use follow whichever role they belong to.
#
# Easiest path: run `/colors generate #8ab4f8` to derive a whole harmonious
# palette from one color you like, then `/colors harmony` to score it and see
# exactly what to fix. `/colors` lists every role, and `/colors export` prints
# this section for you.
#
# [display.colors]
# user = "#8ab4f8"
# ai = "#81c784"
# accent = "#ba8bff"
# success = "#64c864"
# warning = "#ffc864"
# error = "#ff6464"

[features]
# Check for and install updates during startup. Set to false for the persistent
# equivalent of passing --no-update on every invocation.
check_updates = true
# Memory: retrieval + extraction sidecar features
memory = true
# Swarm: multi-session coordination features
swarm = true
# Mermaid: render Mermaid code blocks and tell the model that diagrams are supported.
# Off by default: no horon client renders Mermaid, so the guidance is dead weight.
mermaid = false
# Auto-poke: automatically nudge the model to continue when it stops with
# incomplete todos. /poke on and /poke off still override this per session.
auto_poke = true
# Inject timestamps into user messages and tool results sent to the model
message_timestamps = true
# Persist memory injections into session history instead of sending them as request-only ephemeral context
persist_memory_injections = false
# Import rule files written for other agent tools (.cursor/rules/*.mdc, .cursorrules,
# .clinerules, Copilot instructions, Windsurf rules, GEMINI.md) into the system
# prompt, read-only. AGENTS.md and CLAUDE.md are skipped because they are injected
# natively.
foreign_rules = false
# How imported foreign rules reach the prompt. "inline" appends every rule body
# to the system prompt. "rulebook" inlines only always-on rules and lists the
# rest, so a rule is read only when its globs match a file the session touches.
# foreign_rules_mode = "rulebook"
# Show an in-chat warning when a request misses the KV cache for a harness-caused
# (avoidable) reason: system prompt, tool set, or message prefix changed. These
# should essentially never happen and indicate a prefix-cache bug.
kv_cache_miss_notices = true
# Write a redacted local crash bundle under <agent dir>/crashes/ when the process
# panics. Bundles stay on disk and are never transmitted.
crash_bundles = true
# Update channel: "stable" (releases only) or "main" (latest commits on push)
# Set to "main" for bleeding edge updates every time code is pushed
update_channel = "stable"
# Automatic continuation after a turn stops unexpectedly. "conservative"
# continues only provable truncation and unclosed structure, "aggressive" also
# continues a final sentence that announces work with nothing after it, and
# "off" disables the extra continuation entirely.
turn_recovery = "conservative"
# Enforce the operator-written [[stream_rules]] entries while a response streams:
# a matching pattern aborts the response, injects the rule body as a system
# reminder, and re-issues the request. Off by default.
stream_rules = false
# Each [[stream_rules]] entry names an id, a Rust regex pattern, and the body to
# inject. Optional keys: scope (["text"] default, "tool", "tool:<name>"), paths
# (globs matched against a tool call's file_path/path argument), repeat
# ("turn" default, "once", "after-gap"), repeat_gap (default 10 user turns), and
# delivery ("interrupt" default, "passive").
# [[stream_rules]]
# id = "no-unwrap"
# pattern = "unwrap\\(\\)"
# body = "Use the ? operator instead of unwrap()."
# scope = ["tool:write", "tool:edit"]
# repeat = "after-gap"
# repeat_gap = 20

[websearch]
# Use the model provider's own server-side search (Anthropic web_search on the
# first-party API, OpenAI Responses web_search) whenever the active
# provider/model supports it. Searches then run on the provider's side, so they
# work on servers where DuckDuckGo/Bing scraping is blocked and need no extra
# key. Providers without native search use the local engines below.
# Anthropic API keys are billed about $10 per 1,000 searches.
prefer_native = true
# Local websearch engine: "duckduckgo", "bing", "searxng", "custom", or "native"
# ("native" forces prefer_native on and otherwise runs fallback_engines).
engine = "duckduckgo"
# Keyless HTML engines to try if the preferred engine fails. Default falls back to Bing HTML.
fallback_engines = ["bing"]
# Bring your own Bing Search API key for primary Bing searches. Prefer using an env var.
# Fallback Bing searches intentionally use keyless HTML search.
# bing_api_key_env = "HORON_AGENT_BING_API_KEY"
# bing_api_key = ""
# Bing market/region, for example "en-US" or "zh-CN".
bing_market = "en-US"
# SearXNG instance for the "searxng" engine. On some hosts (commonly Linux),
# DuckDuckGo and Bing block scraped requests via TLS fingerprinting / IP
# reputation and return an anti-bot page with no results. Pointing at a SearXNG
# instance (self-hosted or trusted public) with the JSON format enabled avoids
# this. Configure here or via the HORON_AGENT_SEARXNG_URL environment variable, then
# set engine = "searxng" or add it to fallback_engines.
# searxng_url = "https://searx.example.org"
# Native search options (prefer_native):
# Max provider searches per request (Anthropic only). Caps spend. 0 = no cap.
# native_max_uses = 5
# Restrict results to these domains (Anthropic and OpenAI), or block domains
# (Anthropic only). Set one list, not both.
# native_allowed_domains = ["docs.rs", "developer.mozilla.org"]
# native_blocked_domains = ["example.com"]
# Anthropic tool version. "web_search_20260209" and newer are sent with
# allowed_callers = ["direct"].
# native_anthropic_tool_version = "web_search_20250305"

# Custom HTTP JSON search: set engine = "custom" and prefer_native = false above.
# Set fallback_engines = [] above if private queries must not reach Bing on failure.
# See docs/provider/INTEGRATIONS.md for GET/POST, authentication and JSON pointers.
# [websearch.custom]
# endpoint = "https://search.example.org/api"
# method = "GET"
# query_param = "q"
# count_param = "count" # optional; omit to send no count parameter
# params = { format = "json" }
# headers = { Accept = "application/json" }
# header_env = { Authorization = "SEARCH_AUTH" } # complete value, e.g. Bearer ...
# results_pointer = "/results"
# title_pointer = "/title"
# url_pointer = "/url"
# snippet_pointer = "/snippet"

[compaction]
# Model used for structured compaction summaries; empty keeps the session model.
# Env override: HORON_AGENT_COMPACTION_SUMMARY_MODEL.
# summary_model = ""

[tools]
# Controls which built-in tools are sent to the model.
# Profiles: "core" (headless default), "full", "acp", "minimal"/"lite", or "none".
# core is the headless default: bash, read, write, edit, replace, apply_patch,
# agentgrep, ls, webfetch, websearch, skill, mcp, undo, and ask. Set "full" to
# expose every built-in tool.
# acp keeps core coding tools plus batch for generic ACP clients.
# minimal keeps core coding tools only: bash, read, write, edit, multiedit,
# apply_patch, patch, agentgrep, glob, grep, and ls.
profile = "core"
# Explicit allow-list. When non-empty, only these tools are exposed.
# enabled = ["bash", "read", "write", "apply_patch", "agentgrep", "ls"]
# All built-in tools, including gmail, are exposed by the full profile.
# Use enabled = ["*"] to explicitly select the unrestricted full toolset.
# Hide selected tools after applying the profile/allow-list.
# disabled = ["browser", "gmail", "swarm"]
# Disable all built-in tools unless enabled is set.
disable_base_tools = false
# MCP tool exposure. "auto" never changes the cached tool list when MCP
# servers connect, reconnect, or register late, so the provider prompt cache
# survives: Claude and OpenAI (gpt-5.4+) load MCP tools natively as deferred
# definitions; other providers use fixed mcp_search/mcp_call tools and learn
# new tools' schemas from the transcript. "deferred" behaves like "auto".
# "eager" sends every server tool definition in the tool list; adding a
# server mid-session then invalidates the whole prompt cache.
# Env overrides: HORON_AGENT_MCP_TOOLS. mcp_tools_token_threshold is ignored and
# kept only so existing configs still parse.
mcp_tools = "auto"
mcp_tools_token_threshold = 8000
# Experimental, opt-in Codemode. Adds a `codemode` tool that runs
# model-written JavaScript in an embedded QuickJS sandbox. Scripts call any
# other tool (including MCP tools) via `await tools.<name>(args)`, chain and
# parallelize calls, and keep intermediate results out of the conversation.
# Env override: HORON_AGENT_CODEMODE=1.
codemode = false
# On by default: when `read` returns a file, append the `AGENTS.md` of the
# directories above it (up to the session working directory) so the rules of the
# component being read arrive with the file. Capped at 16 KiB per file and 32 KiB
# per read, injected once per session, and skipped when the read result is already
# 32 KiB or larger.
# Env override: HORON_AGENT_NESTED_AGENTS_MD=0.
nested_agents_md = true
# Answer a repeated read of an unchanged file with a one-line notice, and a
# changed file with a diff. Pass full=true on a read to print it again.
# Env override: HORON_AGENT_READ_DEDUP=0.
read_dedup = true
# Condense large, older tool results in one batch when the prompt cache is cold
# or compaction is near; originals stay readable via artifact://.
# Env override: HORON_AGENT_TOOL_RESULT_AGING=0.
tool_result_aging = true
# On by default: refuse to edit, replace, delete, or overwrite a file that a
# generator produces (`*.pb.go`, `*_pb2.py`, `// Code generated ... DO NOT EDIT`,
# `@generated`, and similar markers). Change the generator input and regenerate
# instead; set false to allow edits to generated files.
block_auto_generated = true
# How the `ask` tool resolves a question when no interactive client can answer
# it. "recommended" (default) picks the option the model recommended, or the
# first option when it named none, and says the choice was automatic. "error"
# refuses the call so the model continues on its own judgment. `ask` never
# blocks in either mode.
ask_policy = "recommended"
# Opt-in `redo`: expose the tool that re-applies the mutation the last `undo`
# removed. `undo` is always available; this only adds the reverse direction.
redo = false
# Opt-in `hashline_edit`: replaces line spans named by the anchors `read` emits
# with `hashline: true`. Every anchor is checked against the file before any
# write, so a stale anchor refuses the whole call.
hashline_edit = false
# Opt-in `ast_grep`/`ast_edit`: structural search and rewrite over tree-sitter
# grammars (rust, typescript, tsx, javascript, python, go). `ast_edit` previews
# a unified diff unless `expected_replacements` is given, so a preview never
# touches a file.
ast_tools = false
# Opt-in structural `read` summary: a whole-file read of a supported language
# with more than 400 lines keeps item signatures and elides long bodies, with a
# footer naming the ranges to read for the elided lines.
read_summary = false
# Opt-in fuzzy `edit`: when `old_string` matches exactly one location after
# whitespace and curly-quote normalization, apply the edit there and report the
# similarity. Off by default, so an edit either matches exactly or reports the
# closest match for the next attempt.
edit_fuzzy = false
# Opt-in `lsp`: adds an `lsp` tool that asks the project's language server for
# diagnostics, definitions, references, hover text, and symbols. The server is
# rust-analyzer for Rust, typescript-language-server for TypeScript/JavaScript,
# gopls for Go, or pyright for Python; it must be on PATH. A server starts on
# the first call and shuts down after ten idle minutes.
lsp = false
# Opt-in managed skills: let `skill_manage create`/`update` write skill files
# into ~/.horon-agent/managed-skills/. An authored skill of the same name always
# wins, and descriptions are sanitized before they reach the prompt.
managed_skills = false
# Opt-in `context_notes`: exposes a tool that keeps a small notes file per
# session (read it back with `read history://current`) and appends it to every
# request as "# Context Notes". Cleared when the conversation is cleared.
context_notes = false
# Terminal allocation for `bash`: "auto" (default) allocates one only for commands
# that need it, "off" never allocates, "always" always does. A call's own `pty`
# argument overrides this value.
bash_pty = "auto"
# Plan mode: while on, calls that mutate files or spawn processes are refused
# until a plan is approved. Off by default, so nothing changes for an existing
# config.
plan_mode = false
# Let `bash` start the next call in the directory the previous command ended in.
# On by default: an agent that must prefix every command with `cd` misreads its
# own working directory. Set false to pin every call to the session directory.
bash_session_cwd = true
# Give every `bash` child a non-interactive environment: pagers and editors
# resolve to no-ops, `CI=true` asks tools for plain output, and package
# managers stop prompting. Without it a command can hang waiting for a
# terminal answer. Set false to inherit the parent environment unchanged.
bash_non_interactive_env = true
# Opt-in output minimizer: a single, non-piped `cargo`/package-manager/pytest/go
# test/git status/tsc/eslint command has its progress lines dropped before the
# result reaches the model. The full output is saved as a session artifact and
# the result points at it. Off by default because it rewrites what the model
# reads.
# bash_minimizer = false
# How `bash` reacts to shell idioms a dedicated tool handles better: "off"
# (default) runs them as written, "nudge" runs the command and prefixes the
# output with the better tool, "block" refuses the call. Covers cat/head/tail,
# grep/rg, find/fd, in-place sed/perl/awk, shell redirection, nohup, and a
# trailing `&`; a rule only fires when its tool is reachable.
bash_interceptor = "off"
# Explicit approval rules for shell commands, checked before approval_mode.
# The first matching rule in config order wins. `pattern` is matched against the
# whole command, and against each command segment for deny/prompt; `*` matches
# anything and every other character is literal.
# [[tools.bash_patterns]]
# pattern = "git push *"
# approval = "deny"
# Refuse history-rewriting git commands (stash, reset --hard, branch moves) in a
# shared main checkout, where another session may hold uncommitted work. A
# linked worktree is never refused. Off by default.
git_guard = false
# Which tool calls ask for approval first: "off" (default) never asks,
# "write" asks before a call that can mutate files, "all" asks before anything
# that is not read-only.
approval_mode = "off"
# Opt-in auxiliary completions for side tasks that must not disturb the main
# conversation.
aux_completion = false
# How credentials found in outbound text are handled: "off" (default) leaves
# text untouched, "obfuscate" swaps in stable placeholders and restores them
# for the tool that needs the value, "block" refuses the call.
secrets_mode = "off"
# Command-risk classes that always require approval, whatever approval_mode
# says, so a dangerous class can never be auto-approved. "destructive-shell" is
# a command the classifier wants justified, "catastrophic-shell" is one it
# refuses outright. Default: empty, which leaves every class to approval_mode.
# Example: ["destructive-shell"]
dual_confirm_classes = []
# Per-tool load tiers for the unrestricted `full` profile. Out of the box, the
# core tools stay eager and every other enabled tool is sent as a provider-native
# deferred definition, so its schema stays out of the cached prompt prefix; a
# provider without deferred loading drops those definitions instead. "essential"
# forces one back into the cached prefix, "off" removes it, and a name this build
# does not have is ignored. Default: empty, so every tool keeps its profile tier.
# [tools.load_mode]
# gmail = "off"
# browser = "essential"

[acp]
# Agent Client Protocol adapter compatibility profile: standard, extended, or full.
# standard emits only spec-compatible ACP messages.
# extended/full additionally emit ignorable _horon_agent/* extension notifications.
profile = "standard"
# Tool profile requested when `horon-agent acp` starts the daemon itself.
# Existing daemons keep their current server-wide tool config.
tool_profile = "acp"
# Answer ACP protocol version 2 to an editor that asks for it. False answers
# every client exactly as version 1 does.
# protocol_v2 = true

[provider]
# Default model (optional, uses provider default if not set)
# Set via /model picker with Ctrl+B to save as default
# default_model = "claude-opus-5-5"
# Default provider (optional: claude|anthropic-api|openai|openai-api|copilot|openrouter|...)
# When set, this provider is preferred on startup if available.
#   claude        = Claude via OAuth/subscription (token in ~/.horon-agent/auth.json)
#   anthropic-api = Claude via direct Anthropic API key (ANTHROPIC_API_KEY env
#                   or ~/.config/horon-agent/anthropic.env). API-key mode does NOT fall
#                   back to OAuth; configure the key first.
# `claude` and `anthropic-api` are distinct providers with distinct credentials.
# default_provider = "copilot"
# OpenAI reasoning effort (none|minimal|low|medium|high|xhigh|max)
openai_reasoning_effort = "low"
# Anthropic reasoning effort for Claude reasoning models (none|low|medium|high|xhigh|max)
# xhigh needs Opus 4.7/4.8 or Fable 5; max needs an output_config effort model (Opus/Sonnet 4.6+).
# Defaults to xhigh for Claude Opus 4.7/4.8 (high on older Opus) when unset; other models keep their own default.
# anthropic_reasoning_effort = "medium"
# OpenAI transport mode (auto|websocket|https)
# openai_transport = "auto"
# OpenAI service tier override (priority|flex|off)
# Defaults to `priority` to match Codex /fast behavior for OpenAI OAuth
# (higher speed, higher usage). Set to "off" (or "standard") to disable.
openai_service_tier = "priority"
# Preserve provider-native reasoning/thinking for future-turn context when supported.
# Applies to OpenRouter, Anthropic, and OpenAI native reasoning replay. Display is separate.
preserve_reasoning_context = true
# Cross-provider failover when the same prompt would be resent elsewhere.
# countdown = 3-second countdown before retrying on another provider; press Esc to cancel (default)
# manual = show a notice and let you switch yourself
# cross_provider_failover = "manual"
# Try another account on the same provider before switching providers (default: true)
# same_provider_account_failover = false
cross_provider_failover = "countdown"
# Let an unattended run switch provider routes on its own when the provider
# refuses, instead of waiting for the interactive countdown (default: false)
# autonomous_failover = false
# Route switches allowed per execution chain; 0 disables switching (default: 3)
# autonomous_failover_max_attempts = 3
# Copilot premium mode: "normal" (default), "one" (first msg only), "zero" (all free)
# Set to "zero" if you have premium Copilot and want free requests
# copilot_premium = "zero"
# Only list these providers in the /model picker (issue #460). Entries match
# provider labels ("openai", "anthropic", "copilot", "openrouter", ...), route
# api methods ("claude-oauth", "openai-compatible:myprofile"), or bare
# openai-compatible profile ids ("myprofile"). The active model's routes always
# stay visible. Unset or empty = show everything.
# model_picker_providers = ["myprofile", "openrouter"]
# Max seconds to wait for streaming data before timing out a request with no
# data received. Raise this for slow reasoning models (e.g. DeepSeek) that think
# silently for minutes before emitting tokens. Default: 180.
# Applies to every streaming provider path (OpenAI native, Anthropic, Copilot,
# OpenRouter/OpenAI-compatible). The TUI's client-side stall guard also extends
# to match this value. Also overridable per-launch via HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS.
# This is the base budget: high reasoning efforts scale it up automatically
# (high 2x, xhigh 3x, max/swarm 4x) since they think silently for much longer.
# stream_idle_timeout_secs = 600
# Maximum attempts for transient 429/5xx/network failures, including the first
# request. Retries honor Retry-After and use capped exponential backoff.
# Env overrides: HORON_AGENT_MAX_RETRIES, HORON_AGENT_RETRY_BACKOFF_CAP_SECS.
# max_retries = 8
# retry_backoff_cap_secs = 30

[server]
# Who executes autonomous wake requests from background completion/stall,
# swarm await completion, and communication delivery.
# "internal" starts or interrupts turns in the daemon (default).
# "external" emits typed wake_requested events for an operator to handle and
# never starts a turn or injects into a running turn.
# Env override: HORON_AGENT_WAKE_MODE
wake_mode = "internal"

[agents]
# Swarm root settings and defaults for helper agents (workers, subagents, sidecars).
# All keys are optional; the values below are the built-in defaults.
#
# Default model for spawned swarm/subagent sessions.
# Leave unset (or "inherit"/"coordinator") so workers inherit the model of the
# session that spawned them. Set a concrete model to change the worker default.
# An explicit `model` in the swarm tool overrides this default for new workers.
# Env override: HORON_AGENT_SWARM_MODEL
# swarm_model = "inherit"
#
# Default reasoning effort for spawned swarm workers when the spawn call does
# not pass an explicit `effort` ("low", "medium", "high", ...). Leave unset so
# workers inherit the provider-wide reasoning effort.
# Env override: HORON_AGENT_SWARM_EFFORT
# swarm_effort = "medium"
#
# Root model reasoning while /effort swarm or /effort swarm-deep is selected.
# These are independent of worker swarm_effort. Supported levels:
# none|minimal|low|medium|high|xhigh|max. Unset/invalid = max (model maximum).
# Providers map unsupported levels to their supported range.
# Env overrides: HORON_AGENT_SWARM_ROOT_EFFORT, HORON_AGENT_SWARM_DEEP_ROOT_EFFORT
swarm_root_effort = "max"
swarm_deep_root_effort = "max"
#
# How swarm-created agents are spawned:
#   "inline"   - in-process (no window), shown as a live gallery viewport in the coordinator (default)
#   "visible"  - open a headed terminal window (alias: "headed")
#   "headless" - create the worker in-process with no terminal window
#   "auto"     - try visible first, fall back to headless if no window can open
# The swarm tool's per-call `spawn_mode` overrides this when set.
# Env override: HORON_AGENT_SWARM_SPAWN_MODE
swarm_spawn_mode = "inline"
#
# Max live swarm worker agents in one swarm. This RAM-safety budget applies to
# recursive ad hoc spawning and deep-mode run_plan parallelism. Completed/stopped
# workers free their slots. 0 disables this guard and leaves only the absolute
# per-swarm hard cap of 1000. Light mode uses a smaller fixed fan-out.
# Env override: HORON_AGENT_SWARM_MAX_CONCURRENT_AGENTS
swarm_max_concurrent_agents = 32
#
# Stalled-worker watchdog. A running worker with no streamed tokens, tool
# events, or task heartbeats for this many seconds (and no tool currently
# executing) is flagged as stalled in swarm status and its coordinator is
# notified once. 0 disables it. Set swarm_stall_nudge = true to also send the
# worker a short nudge message.
# Env overrides: HORON_AGENT_SWARM_STALL_AFTER_SECS, HORON_AGENT_SWARM_STALL_NUDGE
swarm_stall_after_secs = 600
# swarm_stall_nudge = false
#
# Max percentage (1-90) of the chat height the inline swarm gallery band may use.
# Unset = built-in default (40%). Lower values keep more transcript visible; set
# near the minimum to collapse the gallery to a thin strip.
# swarm_gallery_max_pct = 40
#
# Layout of the inline swarm strip above the status line:
#   "vertical"   - one agent per row (session icon + status + task), capped to
#                  a few rows with a "+N more" overflow marker (default)
#   "horizontal" - all agents packed as chips on a single row
# Env override: HORON_AGENT_SWARM_STRIP_LAYOUT
# swarm_strip_layout = "vertical"
#
# Recall uses Jev typed Decisions directly, without embeddings or a sidecar LLM.
# Provider values: auto, horon-agent, openrouter, typesafe, aimlapi.
# auto prefers HoronAgent, then OpenRouter, TypeSafe, AI/ML API credentials.
# Env override: HORON_AGENT_MEMORY_JEV_PROVIDER
# memory_jev_provider = "auto"
# Minimum relevance probability (0.8..=1.0). Invalid values fail closed.
# memory_jev_threshold = 0.8
# BYOK: OPENROUTER_API_KEY, TYPESAFE_API_KEY, or AIMLAPI_API_KEY.
# HoronAgent requires an eligible subscription and gateway memory_jev capability.
# With a HoronAgent login and an older gateway, explicitly select a BYOK provider.
# No fallback after entitlement, auth, billing, or network failure; no silent BYOK spend.
# Memories remain local; the query and candidate memories go to the selected provider.
# No keys? Local memory list/search/remember/forget still work.
#
# Optional text-generating extraction is separate from recall. Disable it to
# learn only through the main agent's explicit memory writes.
# (OpenAI defaults to gpt-5.6-luna with reasoning effort "none").
# Env overrides: HORON_AGENT_MEMORY_SIDECAR_ENABLED, HORON_AGENT_MEMORY_MODEL
# memory_sidecar_enabled = true
# memory_model = "gpt-5.6-luna"
# Legacy memory_rerank_* and memory_embedding_* settings are accepted for
# backwards compatibility, but have no effect on Jev recall.

[advisor]
# A second, independent model that watches this session and files narrow notes
# for the main agent. It reads the transcript and only read-only inspection
# tools, never starts a turn, and never acts on its own.
# (default: false — opt-in)
# enabled = false
# Model override for the advisor. Omit to inherit the session provider.
# model = ""
# Non-blocker notes one update may deliver; blockers are exempt. Clamped to 1-32.
max_notes_per_update = 4
# User turns after a blocker interrupt when nothing interrupts again.
immune_turns = 3

[terminal]
# Without a hook, clients inside tmux automatically use a right-side pane.
# Set HORON_AGENT_TERMINAL to force a supported terminal emulator instead.
# External command that takes over headed session spawns (swarm agents,
# resume-in-new-terminal, self-dev windows, restart restores).
#
# When set, horon-agent runs `<spawn_hook> <horon-agent-binary> <args...>` instead of
# opening a terminal emulator itself. The hook receives HORON_AGENT_SPAWN_* env vars
# describing the spawn so multiplexers/wrappers can decide where it appears:
#   HORON_AGENT_SPAWN_KIND        - "swarm-agent", "resume", "selfdev", "restart", ...
#   HORON_AGENT_SPAWN_SESSION_ID  - session the window will run
#   HORON_AGENT_SPAWN_TITLE       - suggested window/tab title
#   HORON_AGENT_SPAWN_CWD         - session working directory (also the hook's cwd)
#   HORON_AGENT_SPAWN_PROGRAM     - horon-agent binary path
#   HORON_AGENT_SPAWN_COMMAND     - full shell-escaped command line
#   HORON_AGENT_SPAWN_SWARM_ID / HORON_AGENT_SPAWN_COORDINATOR_SESSION_ID (swarm spawns)
# If the hook fails to start, horon-agent falls back to built-in terminal detection.
# Env override: HORON_AGENT_SPAWN_HOOK (set empty to disable a config hook).
#
# Examples:
#   spawn_hook = "tmux new-window"                # tmux window per agent
#   spawn_hook = "kitty @ launch --type=tab --"   # kitty tab per agent
#   spawn_hook = "~/bin/horon-agent-spawn-router"       # custom placement script
# spawn_hook = ""
#
# External command used to focus/raise an existing session window, replacing
# the built-in wmctrl/xdotool title search. Receives HORON_AGENT_FOCUS_SESSION_ID
# and HORON_AGENT_FOCUS_TITLE env vars. Pair with spawn_hook so the program that
# placed the window also brings it to the front.
# Env override: HORON_AGENT_FOCUS_HOOK (set empty to disable a config hook).
#
# Example:
#   focus_hook = "~/bin/horon-agent-focus-router"
# focus_hook = ""
#
# macOS only: terminal that the Cmd+; launch hotkey and in-app session spawns
# open horon-agent into. One of: ghostty, iterm2, wezterm, warp, alacritty, vscode,
# terminal (Apple Terminal). Preferred over the legacy
# ~/.horon-agent/preferred_terminal.json file. After changing this, re-run
# `horon-agent setup-hotkey` so the generated launcher script (Cmd+;) picks it up.
# preferred = "ghostty"

[notifications]
# Desktop notifications for interactive sessions (macOS Notification Center /
# Linux notify-send). Separate from [safety], which covers ambient-mode
# ntfy/email/channel notifications.
#
# Notify when an agent turn finishes. Fires only for long turns and, by
# default, only while the terminal window is unfocused. The notification is a
# compact summary: session name, duration, todo progress, and a snippet of the
# final assistant message.
# turn_complete = true
# Minimum turn duration (seconds) before notifying (default: 120)
# turn_complete_min_secs = 120
# Lower threshold (seconds) when the session has todos, since todos indicate
# task-style work worth reporting sooner (default: 30)
# turn_complete_todo_min_secs = 30
# Only notify while the terminal window is unfocused (default: true)
# turn_complete_only_when_unfocused = true
# macOS Notification Center sound played on completion (e.g. "Glass", "Ping",
# "Hero"). Empty string disables the sound. Ignored on non-macOS. (default: "Glass")
# turn_complete_sound = "Glass"

[hooks]
# Synchronous tool-input transformers. Each receives the tool input JSON on
# stdin and may print replacement JSON on stdout. Failures and empty output
# retain the original input. Transformers run before pre_tool policy gates.
# pre_tool_transform = ["~/.horon-agent/plugins/rtk-transform"]
# pre_tool_transform_timeout_ms = 500
# Lifecycle hooks: external commands horon-agent runs at well-defined points so other
# programs can observe or gate agent behavior. Commands are parsed shell-style
# (quotes work) but executed directly, with HORON_AGENT_HOOK_* env vars describing
# the event:
#   HORON_AGENT_HOOK_EVENT       - "turn_start", "turn_end", "session_start",
#                            "session_end", "pre_tool", "post_tool"
#   HORON_AGENT_HOOK_SESSION_ID  - the session the event belongs to
#   HORON_AGENT_HOOK_CWD         - session working directory (also the hook's cwd)
#   HORON_AGENT_HOOK_PAYLOAD     - JSON mirror of all fields
# Hook processes get HORON_AGENT_HOOKS_DISABLED=1 so nested horon-agent calls don't recurse.
#
# All hooks except pre_tool are observers: detached, fire-and-forget, failures
# only logged. Env overrides: HORON_AGENT_HOOK_TURN_START, HORON_AGENT_HOOK_TURN_END,
# HORON_AGENT_HOOK_SESSION_START, HORON_AGENT_HOOK_SESSION_END, HORON_AGENT_HOOK_PRE_TOOL,
# HORON_AGENT_HOOK_POST_TOOL (set empty to disable a config hook).
#
# Runs when an agent turn begins, before the model starts generating and before
# the first pre_tool. Lets integrations detect the agent is working during the
# think/stream window before any tool call. Extra fields: HORON_AGENT_HOOK_MODEL,
# HORON_AGENT_HOOK_SOURCE ("chat"/"resume"/"ambient").
# turn_start = "~/bin/horon-agent-turn-start"
#
# Runs when an agent turn completes. Extra fields: HORON_AGENT_HOOK_STATUS
# ("ok"/"error"), HORON_AGENT_HOOK_DURATION_MS, HORON_AGENT_HOOK_MODEL,
# HORON_AGENT_HOOK_LAST_ASSISTANT_TEXT (first 4000 chars), HORON_AGENT_HOOK_ERROR.
# turn_end = "~/bin/horon-agent-turn-notify"
#
# Runs when a session becomes active. Extra: HORON_AGENT_HOOK_SOURCE
# ("create"/"attach"/"resume").
# session_start = ""
#
# Runs when a session closes normally. Extra: HORON_AGENT_HOOK_SOURCE ("close").
# session_end = ""
#
# Gate hook before every tool call. Receives HORON_AGENT_HOOK_TOOL_NAME and the tool
# input JSON on stdin (truncated copy in HORON_AGENT_HOOK_TOOL_INPUT). Exit 0 allows
# the call; exit 2 blocks it and stderr is shown to the model as the error;
# any other outcome (other exits, timeout, missing binary) fails open.
# pre_tool = "~/bin/horon-agent-tool-policy"
#
# Max milliseconds to wait for pre_tool before failing open (default: 5000).
# pre_tool_timeout_ms = 5000
#
# Runs after each tool call. Extra fields: HORON_AGENT_HOOK_TOOL_NAME,
# HORON_AGENT_HOOK_TOOL_CALL_ID, HORON_AGENT_HOOK_TOOL_INPUT (tool input JSON, 16 KB cap),
# HORON_AGENT_HOOK_TOUCHED_PATHS (newline-separated absolute paths),
# HORON_AGENT_HOOK_STATUS, HORON_AGENT_HOOK_DURATION_MS, HORON_AGENT_HOOK_OUTPUT_BYTES,
# HORON_AGENT_HOOK_ERROR.
# post_tool = ""
#
# Synchronous hook after each successful tool call. Same fields as post_tool,
# full tool input JSON on stdin. Anything it prints to stdout is appended to
# the tool result the model sees, so linters can report findings inline.
# post_tool_feedback = "~/bin/horon-agent-vale"
# post_tool_feedback_timeout_ms = 10000

[ambient]
# Ambient mode: background agent that maintains your codebase
# Enable ambient mode (default: false)
enabled = false
# Provider override (default: auto-select based on available credentials)
# provider = "claude"
# Model override (default: provider's strongest)
# model = "claude-sonnet-4-20250514"
# Allow API key usage (default: false, only OAuth to avoid surprise costs)
allow_api_keys = false
# Daily token budget when using API keys (optional)
# api_daily_budget = 100000
# Minimum interval between cycles in minutes
min_interval_minutes = 5
# Maximum interval between cycles in minutes
max_interval_minutes = 120
# Pause ambient when user has active session
pause_on_active_session = true
# Enable proactive work (new features, refactoring) vs garden-only (lint, format, deps)
proactive_work = true
# Branch prefix for proactive work
work_branch_prefix = "ambient/"
# Show ambient cycle in a terminal window (default: true)
# visible = true
# Seconds an unattended run waits for a permission decision before it applies
# approval_timeout_secs (default: 1800)
# approval_timeout_secs = 1800
# Outcome when that wait elapses with no decision: "deny" (default), "approve",
# or "allow". The approval values ignore case and surrounding whitespace, and
# anything else, including a typo, reads as "deny".
# approval_default_on_timeout = "deny"
# Let a run with no attached client ask for permission instead of stalling
# (default: false)
# approval_for_clientless_runs = false

[gateway]
# Enable WebSocket gateway for iOS/web clients
enabled = false
# TCP port for gateway listener
port = 7643
# Bind address (0.0.0.0 for LAN/Tailscale reachability)
bind_addr = "0.0.0.0"

[power]
# Prevent automatic system sleep while any horon-agent session is actively working.
# Linux also blocks lid-switch suspend. On Windows and macOS see block_lid_close.
# The display may sleep.
# The guard is held only for as long as work is in flight. (default: true)
# Set HORON_AGENT_DISABLE_POWER_INHIBIT=1 to force-disable regardless of this setting.
prevent_sleep_while_streaming = true
# Also keep working when the lid closes on macOS and Windows while a session is
# working. Windows temporarily sets the power plan's lid close action to "Do
# nothing". macOS runs `sudo -n pmset -a disablesleep 1` and needs a passwordless
# sudoers rule for /usr/bin/pmset (skipped otherwise). Original settings are
# saved to ~/.horon-agent/lid_override.json and restored afterwards, even after a crash.
block_lid_close = true

[menubar]
# macOS only: show the horon-agent session count in the menu bar. Set to false to
# stop the menu bar helper (a running helper exits within a second).
# HORON_AGENT_NO_MENUBAR=1 also disables it.
enabled = true

[safety]
# Notification settings for ambient mode events

# ntfy.sh push notifications (free, phone app: https://ntfy.sh)
# ntfy_topic = "horon-agent-ambient-your-secret-topic"
# ntfy_server = "https://ntfy.sh"

# Desktop notifications via notify-send (default: true)
desktop_notifications = true

# Email notifications via SMTP
# email_enabled = false
# email_to = "you@example.com"
# email_from = "horon-agent@example.com"
# email_smtp_host = "smtp.gmail.com"
# email_smtp_port = 587
# Password via env: HORON_AGENT_SMTP_PASSWORD (preferred) or config below
# email_password = ""

# IMAP for email replies (reply to ambient emails to send directives)
# email_reply_enabled = false
# email_imap_host = "imap.gmail.com"
# email_imap_port = 993

# Telegram notifications via Bot API (free, https://telegram.org)
# telegram_enabled = false
# telegram_bot_token = ""  # From @BotFather (prefer HORON_AGENT_TELEGRAM_BOT_TOKEN env var)
# telegram_chat_id = ""    # Your user/chat ID
# telegram_reply_enabled = false  # Reply to bot messages to send directives

# Discord notifications via Bot API (https://discord.com/developers)
# discord_enabled = false
# discord_bot_token = ""     # From Discord Developer Portal (prefer HORON_AGENT_DISCORD_BOT_TOKEN env var)
# discord_channel_id = ""    # Channel ID to post in
# discord_bot_user_id = ""   # Bot's user ID (for filtering own messages)
# discord_reply_enabled = false  # Messages in channel become agent directives

# Jade cloud relay (outbound-only long polling, disabled by default).
# Prefer environment variables for secrets:
# HORON_AGENT_JADE_RELAY_API_BASE, HORON_AGENT_JADE_RELAY_TOKEN, HORON_AGENT_JADE_RELAY_TOKEN_ID,
# HORON_AGENT_JADE_RELAY_USER_ID, HORON_AGENT_JADE_RELAY_SESSION_ID.
# jade_relay_enabled = false
# jade_relay_reply_enabled = false   # Deliver cloud prompts to one configured live session.
# jade_relay_launch_enabled = false  # Allow cloud device commands to open headed local sessions.
# jade_relay_launch_working_dir = "" # Optional default cwd for launched sessions.

# [sponsors] # Legacy config section name retained for compatibility.
# Integration discovery (enabled by default; set enabled = false to opt out).
# When enabled, the agent gains a `discover_tools` tool listing third-party
# developer tools from HoronAgent's hosted integration directory. These providers
# have integrated with the agent to make setup and use seamless. Some providers
# may share revenue with HoronAgent when a referred user becomes a customer, but
# commercial relationships never influence recommendations.
# See https://horon-agent.sh/discovery-tools
# enabled = true
# endpoint = "https://api.horon-agent.sh/v1/discovery"
	"##;

        // Substitute platform-specific defaults from the keybinding registry.
        let p = horon_agent_config_types::KeybindingPlatform::current();
        let effort_increase = horon_agent_config_types::default_binding("effort_increase", p).unwrap_or("alt+right");
        let effort_decrease = horon_agent_config_types::default_binding("effort_decrease", p).unwrap_or("alt+left");
        default_content
            .replace("@EFFORT_INCREASE@", effort_increase)
            .replace("@EFFORT_DECREASE@", effort_decrease)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped template is a hand-maintained string, so a typo in it ships
    /// a config file that horon-agent itself cannot read. Parse it here.
    #[test]
    fn default_config_template_parses() {
        let template = Config::default_config_file_contents();
        let config = toml::from_str::<Config>(&template).expect("the shipped config template must parse");
        assert_eq!(config.tools.mcp_tools, McpToolsMode::Auto);
        assert_eq!(config.tools.mcp_tools_token_threshold, 8_000);
        assert!(
            config.display.show_thinking,
            "the shipped user config must request model reasoning"
        );
        assert_eq!(
            config.display.reasoning_display(),
            ReasoningDisplayMode::Full,
            "the shipped user config must keep the full reasoning trace visible"
        );
    }

    /// The template is written once and then read by the daemon, saved back by
    /// `/config`, and read again. Serializing the parsed template must not lose
    /// or reject any switch it documents.
    #[test]
    fn default_config_template_round_trips() {
        let template = Config::default_config_file_contents();
        let parsed = toml::from_str::<Config>(&template).expect("the shipped config template must parse");
        let written = toml::to_string_pretty(&parsed).expect("a parsed template must serialize");
        let reread = toml::from_str::<Config>(&written).expect("what we write must parse back");

        assert_eq!(reread.tools.plan_mode, parsed.tools.plan_mode);
        assert_eq!(reread.tools.approval_mode, parsed.tools.approval_mode);
        assert_eq!(reread.tools.bash_session_cwd, parsed.tools.bash_session_cwd);
        assert_eq!(
            reread.tools.bash_non_interactive_env,
            parsed.tools.bash_non_interactive_env
        );
        assert_eq!(reread.tools.bash_minimizer, parsed.tools.bash_minimizer);
        assert_eq!(reread.tools.aux_completion, parsed.tools.aux_completion);
        assert_eq!(reread.tools.secrets_mode, parsed.tools.secrets_mode);
        assert_eq!(reread.tools.dual_confirm_classes, parsed.tools.dual_confirm_classes);
        assert_eq!(reread.tools.bash_interceptor, parsed.tools.bash_interceptor);
        assert_eq!(reread.tools.bash_patterns, parsed.tools.bash_patterns);
        assert_eq!(reread.tools.git_guard, parsed.tools.git_guard);
    }

    /// Colors are only discoverable if the template mentions them, since most
    /// users read the generated config rather than the docs.
    #[test]
    fn default_config_template_documents_colors() {
        let template = Config::default_config_file_contents();
        assert!(
            template.contains("[display.colors]"),
            "the template should show how to configure colors"
        );
        assert!(
            template.contains("/colors"),
            "the template should point at the /colors command"
        );
    }

    /// Uncommenting the documented color example must actually work, which is
    /// the thing a user will literally do.
    #[test]
    fn documented_color_example_is_valid_when_uncommented() {
        let template = Config::default_config_file_contents();
        let start = template
            .find("# [display.colors]")
            .expect("template documents a colors section");
        let example: String = template[start..]
            .lines()
            .take_while(|line| line.starts_with("# ") || line == &"#")
            .map(|line| format!("{}\n", line.trim_start_matches("# ").trim_start_matches('#')))
            .collect();

        let parsed: Config = toml::from_str(&example).expect("uncommented example must parse");
        assert!(
            !parsed.display.colors.is_empty(),
            "the example should set some colors, got {:?}",
            parsed.display.colors
        );
        for (role, value) in &parsed.display.colors {
            assert!(
                horon_agent_config_types::DisplayConfig::default().colors.is_empty(),
                "colors should default to empty"
            );
            assert_eq!(value.len(), 7, "{role} example should be #rrggbb: {value}");
        }
    }
}
