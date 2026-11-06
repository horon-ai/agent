//! `[features]` section of the config: runtime feature toggles.

use serde::{Deserialize, Serialize};

use crate::{ForeignRulesMode, TurnRecoveryMode, UpdateChannel};

/// Runtime feature toggles
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureConfig {
    /// Check for and install horon-agent updates during startup (default: true).
    /// Set this to false for the persistent equivalent of `--no-update`.
    pub check_updates: bool,
    /// Enable memory retrieval/extraction features (default: true)
    pub memory: bool,
    /// Enable swarm coordination features (default: true)
    pub swarm: bool,
    /// Default state of auto-poke (automatic follow-up when the model stops with
    /// incomplete todos). `/poke on` / `/poke off` still override this per session
    /// (default: true)
    pub auto_poke: bool,
    /// Import rule files written for other agent tools (`.cursor/rules/*.mdc`,
    /// `.cursorrules`, `.clinerules`, Copilot instructions, Windsurf rules,
    /// `GEMINI.md`) into the system prompt, read-only (default: false)
    pub foreign_rules: bool,
    /// How imported foreign rules reach the prompt: inline appends every rule
    /// body (default); rulebook inlines always-on rules and lists the rest so
    /// the read tool can pull one in when its globs match a touched file.
    pub foreign_rules_mode: ForeignRulesMode,
    /// Run the independent read-only tool calls of one model message concurrently
    /// instead of one at a time. Tools that write, run a shell command, or touch
    /// session state stay sequential (default: true)
    pub parallel_tools: bool,
    /// Detect a repeated identical tool call, warn once, refuse the next repeats,
    /// and end the turn after two refusals (default: true)
    pub loop_guard: bool,
    /// Inject timestamps into user messages and tool results sent to the model (default: true)
    pub message_timestamps: bool,
    /// Persist auto-recalled memory injections into normal session history instead of sending
    /// them as request-only ephemeral suffix messages (default: false)
    pub persist_memory_injections: bool,
    /// Surface an in-chat system message whenever a request misses the KV cache
    /// for a harness-caused (avoidable) reason: the system prompt, tool set, or
    /// message prefix changed without the conversation legitimately growing.
    /// These should essentially never happen, so the notice acts as a loud alarm
    /// that something in the harness silently invalidated the prefix cache
    /// (default: true).
    pub kv_cache_miss_notices: bool,
    /// Update channel: "stable" (releases only) or "main" (latest commits)
    pub update_channel: UpdateChannel,
    /// Write a redacted local crash bundle under `<agent dir>/crashes/` when the
    /// process panics. Bundles stay on disk and are never transmitted
    /// (default: true)
    pub crash_bundles: bool,
    /// Automatic continuation after a turn stops unexpectedly: `off`,
    /// `conservative` (default), or `aggressive`
    pub turn_recovery: TurnRecoveryMode,
    /// Enforce the operator-written `[[stream_rules]]` entries while a response
    /// streams: a matching pattern aborts the response, injects the rule body as
    /// a system reminder, and re-issues the request (default: false)
    pub stream_rules: bool,
    /// Fetch the models.dev pricing catalog in the background when the disk
    /// cache is missing or older than 24 hours. Off by default so a one-shot
    /// run never contacts an external site; a cached catalog on disk is still
    /// read. `HORON_AGENT_PRICING_REFRESH=1` enables it and
    /// `HORON_AGENT_PRICING_REFRESH=0` forces it off regardless of this value.
    pub pricing_refresh: bool,
}

impl Default for FeatureConfig {
    fn default() -> Self {
        Self {
            check_updates: true,
            memory: true,
            swarm: true,
            auto_poke: true,
            foreign_rules: false,
            foreign_rules_mode: ForeignRulesMode::Inline,
            parallel_tools: true,
            loop_guard: true,
            message_timestamps: true,
            persist_memory_injections: false,
            kv_cache_miss_notices: true,
            update_channel: UpdateChannel::default(),
            crash_bundles: true,
            turn_recovery: TurnRecoveryMode::default(),
            stream_rules: false,
            pricing_refresh: false,
        }
    }
}
