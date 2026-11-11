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
# Location: ~/.horon/config.toml
#
# Environment variables override these settings.
# Run `/config` in horon-agent to see current settings.
#
# Environment-only settings:
#   HORON_AGENT_JOBS_ARTIFACT_RETENTION - "rotate" (default) reclaims the oldest
#       background task artifacts when the byte or file budget is full; "preserve"
#       refuses to start a new background task instead.
#   HORON_AGENT_WORKSPACE_TRUST - "trust" or "deny" for this process only. Project-local
#       MCP config (.horon/mcp.json, .mcp.json, .claude/mcp.json) is skipped
#       unless its current digest was trusted; --trust-workspace records that decision.

[display]
# Enable debug socket for external control/testing (default: false)
debug_socket = false

# Render emoji in server output (default: true).
# Set false here or set HORON_AGENT_NO_EMOJI=1 for ASCII fallbacks.
emoji = true

# Show thinking/reasoning content (default: true)
show_thinking = true

# How to display reasoning/thinking content: "off", "full", or "current".
#   off     - never show reasoning
#   full    - keep every reasoning trace in the transcript
#   current - show only the live reasoning; collapse it once the model commits
#             an assistant message or runs a tool, then show the next one
# When unset, falls back to show_thinking (true => full, false => off).
reasoning_display = "full"
# Display and hidden option summaries are intentionally minimal: the headless
# build has no terminal UI to configure.

[features]
# Check for and install updates during startup. Set to false for the persistent
# equivalent of passing --no-update on every invocation.
check_updates = true
# Memory: retrieval + extraction sidecar features
memory = true
# Swarm: multi-session coordination features
swarm = true
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
# Recognize per-turn effort keywords in user messages: `ultrathink` raises to
# the highest supported effort and `chillax` lowers to the lowest for that one
# turn. Code spans, fences, comments, tags, and URLs are ignored.
effort_keywords = true
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
# Local websearch engine: "duckduckgo", "bing", "searxng", or "custom".
# Search runs in this process against the provider you pick here; the model
# provider is never asked to search on its behalf.
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

# Custom HTTP JSON search: set engine = "custom" above.
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
# Summarise the branch a rewind throws away and append the report to the kept
# branch. Opt-in twice: this switch and `summarize: true` on the rewind request.
# rewind_summary = false
# Token allowance for that report. 1500 is horon's default, not a borrowed one;
# 0 also means "use the default", and values are clamped to 1..=4096.
# rewind_summary_tokens = 1500

[messages]
# How messages that arrive while a turn is running are delivered. Both queue
# modes default to `all`, which is what horon has always done; omp defaults to
# `one-at-a-time`. The first delivery point of a turn carries messages left over
# from the previous turn (`follow_up_mode`), later points carry messages that
# arrived during this turn (`steering_mode`). `one-at-a-time` leaves the rest
# pending, in order, so each message gets its own injection.
# follow_up_mode = "all"
# steering_mode = "all"
# Whether a queued message waits for the running tool call (`wait`, the timing
# horon has always had; only an urgent message cuts the round) or stops the
# remaining calls at the next safe point (`immediate`).
# interrupt_mode = "wait"

[tools]
# Controls which built-in tools are sent to the model.
# Profiles: "core" (headless default), "full", "acp", "minimal"/"lite", or "none".
# core is the headless default: bash, read, write, edit, replace, apply_patch,
# agentgrep, ls, webfetch, websearch, skill_manage, mcp, monitor, undo, and ask.
# Set "full" to expose every built-in tool.
# acp keeps the core coding tools plus batch, mcp, todo, and ask for generic
# ACP clients.
# minimal keeps core coding tools only: bash, read, write, edit, replace,
# apply_patch, agentgrep, and ls.
profile = "core"
# Explicit allow-list. When non-empty, only these tools are exposed.
# enabled = ["bash", "read", "write", "apply_patch", "agentgrep", "ls"]
# All built-in tools are exposed by the full profile.
# Use enabled = ["*"] to explicitly select the unrestricted full toolset.
# Hide selected tools after applying the profile/allow-list.
# disabled = ["mcp", "swarm"]
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
# into ~/.horon/managed-skills/. An authored skill of the same name always
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
# IRC bus: expose the `irc` tool so agents of the same daemon can send, wait
# for, and read each other's messages. Off by default because it opens a
# message path between sessions. Env override: HORON_AGENT_IRC.
irc = false
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
# mcp = "off"
# memory = "essential"

[skills]
# Skill collections beside horon-agent's own that are read too. horon-agent
# always reads ~/.horon/skills and, per workspace, .horon/skills,
# .claude/skills and .agents/skills. The switches below cover the directories
# other tools own; each one starts off, because reading another agent's skills
# puts its instructions in front of this agent.
#
# ~/.claude/skills and the skills of installed Claude Code plugins.
claude_user = false
# ~/.codex/skills.
codex_user = false

[mcp]
# MCP server configs beside horon-agent's own that are read too. horon-agent
# always reads ~/.horon/mcp.json and, in a trusted workspace,
# .horon/mcp.json, .mcp.json and .claude/mcp.json. When two sources name
# the same server the higher one wins: project, then horon-agent, then Claude
# Code, then Codex. A local stdio entry is kept over a remote one of the same
# name. The switches below start off, because loading another host's servers
# starts processes it reviewed, not this agent.
#
# ~/.claude.json (global servers and this project's entry) and ~/.claude/mcp.json.
# HORON_AGENT_DISABLE_CLAUDE_MCP turns this off even when it is true.
claude_user = false
# [mcp_servers.*] in ~/.codex/config.toml, read live and never copied.
codex_user = false

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
# Default model (optional; the selected profile's default applies when unset).
#   default_model = "gpt-5.6-luna"
# Default provider profile (optional). The value is either a built-in profile id
# (openrouter, deepseek, anthropic-api, openai-api, ...) or the name of a
# [providers.<name>] table below. `--provider-profile <name>` overrides this for
# one launch.
#   default_provider = "openrouter"
# A profile is one OpenAI-compatible endpoint:
#   [providers.myprofile]
#   type = "openai-compatible"
#   base_url = "https://api.example.com/v1"   # the /v1 prefix is part of it
#   api_key_env = "MYPROFILE_API_KEY"         # or api_key, or env_file
#   auth = "bearer"                           # bearer, header, or none
#   default_model = "my-model"
#   prompt_cache_key = true                   # lineage-stable cache key: true, false, or omitted
#                                             # (omitted sends it only to api.openai.com and openrouter.ai)
#   [[providers.myprofile.models]]
#   id = "my-model"
# Every model request posts to <base_url>/chat/completions with that profile's
# credential. Nothing else serves models in this build: no second provider
# runtime, no router process, no plugin host.
# OpenAI reasoning effort (none|minimal|low|medium|high|xhigh|max)
openai_reasoning_effort = "low"
# Reasoning effort for models that accept it (none|low|medium|high|xhigh|max).
# xhigh needs Opus 4.7/4.8 or Fable 5; max needs an output_config effort model (Opus/Sonnet 4.6+).
# Defaults to xhigh for Claude Opus 4.7/4.8 (high on older Opus) when unset; other models keep their own default.
# anthropic_reasoning_effort = "medium"
# OpenAI transport mode (auto|websocket|https)
# openai_transport = "auto"
# OpenAI service tier override (priority|flex|off)
# Defaults to `priority` for the OpenAI route (higher speed, higher usage).
# Set to "off" (or "standard") to disable.
openai_service_tier = "priority"
# Preserve provider-native reasoning/thinking for future-turn context when supported.
# Applies to reasoning replay on the OpenRouter and OpenAI-compatible routes. Display is separate.
preserve_reasoning_context = true
# Cross-provider failover when the same prompt would be resent elsewhere.
# countdown = 3-second countdown before retrying on another provider (default)
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
# Max seconds to wait for streaming data before timing out a request with no
# data received. Raise this for slow reasoning models (e.g. DeepSeek) that think
# silently for minutes before emitting tokens. Default: 180.
# Applies to every streaming request. Also overridable per-launch via
# HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS.
# This is the base budget: high reasoning efforts scale it up automatically
# (high 2x, xhigh 3x, max/swarm 4x) since they think silently for much longer.
# stream_idle_timeout_secs = 600
# Maximum attempts for transient 429/5xx/network failures, including the first
# request. Retries honor Retry-After and use capped exponential backoff.
# Env overrides: HORON_AGENT_MAX_RETRIES, HORON_AGENT_RETRY_BACKOFF_CAP_SECS.
# max_retries = 8
# retry_backoff_cap_secs = 30

[prewalk]
# One-shot handoff to an execution model once planning is done. The session
# switches at the first successful `todo` write, or at plan approval when
# [tools] plan_mode is on, and runs on that model for the rest of the session.
# Off by default because the handoff trades plan-following quality for cost.
# enabled = false
# The execution model: a model id, or `role:<name>` resolved through the active
# profile's roles (`default`, `plan`, `aux`). Required when enabled: an enabled
# section without a usable value fails config validation and names this key.
# into = "role:aux"
# Applicability only: hand off just when the session currently runs this model.
# It never names a model to return to, and leaving it out applies to every
# session.
# from = "my-planner"

[server]
# Who executes autonomous wake requests from background completion/stall,
# swarm await completion, and communication delivery.
# "internal" starts or interrupts turns in the daemon (default).
# "external" emits typed wake_requested events for an operator to handle and
# never starts a turn or injects into a running turn.
# Env override: HORON_AGENT_WAKE_MODE
wake_mode = "internal"
# Release the derived caches of a session that has been idle this many seconds.
# Only the provider projection and the tool-output index are dropped; the saved
# transcript, the frozen prompt and tool surface, and rewind undo stay, so the
# next turn rebuilds those two caches with no disk read.
# 0 disables the sweep. Checked every ten seconds.
# Env override: HORON_AGENT_IDLE_HIBERNATION_SECS
idle_hibernation_secs = 1800

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
# How swarm-created agents are spawned. Every mode runs the worker in this
# process; horon-agent never opens a terminal window.
#   "inline"   - in-process, reported as a live gallery viewport (default)
#   "headless" - in-process with no gallery viewport
#   "auto"     - historical "try a window first" mode; resolves to the
#                configured mode, so an old config keeps working
# `visible`/`headed` from older configs still parse and mean "inline".
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
# Recall uses Jev typed Decisions directly, without embeddings or a sidecar LLM.
# Provider values: auto, openrouter, typesafe, aimlapi.
# auto prefers TypeSafe, then OpenRouter, then AI/ML API credentials.
# Env override: HORON_AGENT_MEMORY_JEV_PROVIDER
# memory_jev_provider = "auto"
# Minimum relevance probability (0.8..=1.0). Invalid values fail closed.
# memory_jev_threshold = 0.8
# BYOK: OPENROUTER_API_KEY, TYPESAFE_API_KEY, or AIMLAPI_API_KEY.
# No fallback after auth, billing, or network failure; no silent BYOK spend.
# Memories remain local; the query and candidate memories go to the selected provider.
# No keys? Local memory list/search/remember/forget still work.
#
# Optional text-generating extraction is separate from recall. Disable it to
# learn only through the main agent's explicit memory writes.
# (OpenAI defaults to gpt-5.6-luna with reasoning effort "none").
# Env overrides: HORON_AGENT_MEMORY_SIDECAR_ENABLED, HORON_AGENT_MEMORY_MODEL
# memory_sidecar_enabled = true
# memory_model = "gpt-5.6-luna"
#
# Cross-session consolidation is the second memory phase: after an extraction it
# merges memories that share a subject into one active entry (the rest are kept
# and marked superseded) and rewrites one bounded summary that is injected as
# memory guidance. A pass only runs once this many memories have been stored
# since the previous one, and the injected summary is capped at this many bytes.
# Both defaults are Horon defaults; omp caps its summary by tokens instead.
# memory_consolidation_min_new = 20
# memory_consolidation_summary_bytes = 4096
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

[hooks]
# Synchronous tool-input transformers. Each receives the tool input JSON on
# stdin and may print replacement JSON on stdout. Failures and empty output
# retain the original input. Transformers run before pre_tool policy gates.
# pre_tool_transform = ["~/.horon/plugins/rtk-transform"]
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
#
# Synchronous hook before each turn's model request. It receives
# {"session_id","turn","model","messages"} on stdin; printing {"messages":[...]}
# replaces the messages for that request only, and the stored transcript is
# never touched. Any failure keeps the request unchanged.
# pre_request = "~/bin/horon-agent-request-policy"
# pre_request_timeout_ms = 5000

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

[irc]
# Messages kept per agent mailbox before the oldest is dropped.
mailbox_cap = 100
# Largest accepted message body, in bytes.
max_body_bytes = 65536
# Longest a single `wait` call may block, in seconds.
wait_max_secs = 300
# Broadcast legs delivered concurrently.
broadcast_concurrency = 16

[gateway]
# Enable the WebSocket gateway for remote daemon clients
enabled = false
# TCP port for gateway listener
port = 7643
# Bind address (0.0.0.0 for LAN/Tailscale reachability)
bind_addr = "0.0.0.0"

[power]
# Prevent automatic system sleep while any horon-agent session is actively working.
# Linux also blocks lid-switch suspend.
# The display may sleep.
# The guard is held only for as long as work is in flight. (default: true)
# Set HORON_AGENT_DISABLE_POWER_INHIBIT=1 to force-disable regardless of this setting.
prevent_sleep_while_streaming = true

[safety]
# Notification settings for ambient mode events

# ntfy.sh push notifications (free, phone app: https://ntfy.sh)
# ntfy_topic = "horon-agent-ambient-your-secret-topic"
# ntfy_server = "https://ntfy.sh"

# Desktop notifications via notify-send (default: true)
desktop_notifications = true

# [sponsors]
# Integration discovery (enabled by default; set enabled = false to opt out).
# When enabled, the agent gains an `integration_tools` tool listing third-party
# developer tools from HoronAgent's hosted integration directory. These providers
# have integrated with the agent to make setup and use seamless. Some providers
# may share revenue with HoronAgent when a referred user becomes a customer, but
# commercial relationships never influence recommendations.
# See https://horon-agent.sh/discovery-tools
# enabled = true
# endpoint = "https://api.horon-agent.sh/v1/discovery"
	"##;
        default_content.to_string()
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

    /// The shipped template documents the `[messages]` table and its defaults.
    #[test]
    fn default_config_template_documents_message_modes() {
        let template = Config::default_config_file_contents();
        for key in [
            "[messages]",
            "follow_up_mode = \"all\"",
            "steering_mode = \"all\"",
            "interrupt_mode = \"wait\"",
        ] {
            assert!(template.contains(key), "the template should document `{key}`");
        }

        let parsed = toml::from_str::<Config>(&template).expect("the shipped template must parse");
        assert_eq!(parsed.messages, Config::default().messages);
    }

    /// The shipped template documents the `[prewalk]` table, stays off, and
    /// parses to the built-in default so a fresh install never hands off.
    #[test]
    fn default_config_template_documents_prewalk_off_by_default() {
        let template = Config::default_config_file_contents();
        for key in [
            "[prewalk]",
            "# enabled = false",
            "# into = \"role:aux\"",
            "# from = \"my-planner\"",
        ] {
            assert!(template.contains(key), "the template should document `{key}`");
        }

        let parsed = toml::from_str::<Config>(&template).expect("the shipped template must parse");
        assert_eq!(parsed.prewalk, Config::default().prewalk);
        assert!(!parsed.prewalk.is_active(), "a fresh install must not hand off");
        parsed.prewalk.validate().expect("the shipped section is valid");
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
        assert_eq!(
            reread.server.idle_hibernation_secs, parsed.server.idle_hibernation_secs,
            "the idle hibernation window must survive a write and reread"
        );
    }

    /// The shipped template names the hibernation window, and the built-in
    /// default is the documented thirty minutes.
    #[test]
    fn idle_hibernation_defaults_to_thirty_minutes_and_keeps_zero() {
        let template = Config::default_config_file_contents();
        assert!(
            template.contains("idle_hibernation_secs = 1800"),
            "the template must show the documented default"
        );
        let config = toml::from_str::<Config>(&template).expect("the shipped config template must parse");
        assert_eq!(config.server.idle_hibernation_secs, 1800);
        assert_eq!(
            super::super::DEFAULT_IDLE_HIBERNATION_SECS,
            1800,
            "the constant and the template must agree"
        );

        // A config file that predates the key gets the default, not a disable.
        let legacy: Config = toml::from_str("[server]\nwake_mode = \"external\"\n").expect("legacy server table");
        assert_eq!(legacy.server.idle_hibernation_secs, 1800);
        assert_eq!(legacy.server.wake_mode, WakeMode::External);

        // Zero is a real value: it turns the sweep off, so it must round-trip.
        let disabled: Config = toml::from_str("[server]\nidle_hibernation_secs = 0\n").expect("zero parses");
        assert_eq!(disabled.server.idle_hibernation_secs, 0);
        let written = toml::to_string_pretty(&disabled).expect("a zero window must serialize");
        let reread: Config = toml::from_str(&written).expect("a zero window must parse back");
        assert_eq!(reread.server.idle_hibernation_secs, 0);
    }
}
