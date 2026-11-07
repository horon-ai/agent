use serde::{Deserialize, Serialize};

mod display;
mod feature;
mod gateway;
mod irc;
mod power;
mod serde_lenient;
mod storage;
pub use {
    display::DisplayConfig,
    feature::FeatureConfig,
    gateway::GatewayConfig,
    irc::IrcConfig,
    power::PowerConfig,
    storage::{StorageConfig, StorageSyncMode},
};

/// Compaction mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompactionMode {
    /// Compact when context hits a fixed threshold (default)
    #[default]
    Reactive,
    /// Compact early based on predicted token growth rate
    Proactive,
    /// Compact based on semantic topic shifts and relevance scoring
    Semantic,
}

impl CompactionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reactive => "reactive",
            Self::Proactive => "proactive",
            Self::Semantic => "semantic",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "reactive" => Some(Self::Reactive),
            "proactive" => Some(Self::Proactive),
            "semantic" => Some(Self::Semantic),
            _ => None,
        }
    }
}

/// Which compaction engine owns a session.
///
/// Exactly one strategy owns compaction at a time. `structured` is horon's own
/// summarizer; the `openai_*` variants hand ownership to the provider's native
/// compaction so both never trigger for the same turn.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    /// horon summarizes and rewrites the transcript (default).
    #[default]
    Structured,
    /// [OI] native auto compaction owns the transcript.
    #[serde(rename = "openai_auto")]
    OpenAiAuto,
    /// [OI] native compaction is requested explicitly by the agent.
    #[serde(rename = "openai_explicit")]
    OpenAiExplicit,
}

impl CompactionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Structured => "structured",
            Self::OpenAiAuto => "openai_auto",
            Self::OpenAiExplicit => "openai_explicit",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "structured" => Some(Self::Structured),
            "openai_auto" | "openai-auto" => Some(Self::OpenAiAuto),
            "openai_explicit" | "openai-explicit" => Some(Self::OpenAiExplicit),
            _ => None,
        }
    }

    /// Whether the provider owns compaction under this strategy.
    pub fn is_native(self) -> bool {
        !matches!(self, Self::Structured)
    }
}

/// How to display the model's reasoning/thinking content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningDisplayMode {
    /// Never display reasoning content.
    #[default]
    Off,
    /// Keep every reasoning trace in the transcript (classic behavior).
    Full,
    /// Show only the *current* reasoning live; collapse it once the model
    /// commits an assistant message or tool call, then show the next one.
    Current,
}

impl ReasoningDisplayMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Full => "Full",
            Self::Current => "Current",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Current,
            Self::Current => Self::Full,
            Self::Full => Self::Off,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "off" | "none" | "false" | "0" | "no" => Some(Self::Off),
            "full" | "all" | "true" | "1" | "yes" | "on" => Some(Self::Full),
            "current" | "live" | "ephemeral" | "collapse" => Some(Self::Current),
            _ => None,
        }
    }
}

/// Update channel: how aggressively to receive updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// Only update from tagged GitHub Releases (default).
    #[default]
    Stable,
    /// Update from latest commit on main branch (bleeding edge).
    Main,
}

impl UpdateChannel {
    /// Parse a channel name, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" | "release" => Some(Self::Stable),
            "main" | "nightly" | "edge" => Some(Self::Main),
            _ => None,
        }
    }
}

/// Config deserialization is deliberately lenient: an unknown or removed
/// channel name (e.g. a stale `update_channel = "manual"` left in
/// config.toml) falls back to the default channel instead of failing the
/// entire config parse. A strict enum here once made the freshly exec'd
/// server die during the reload handoff, leaving the handoff marker stuck
/// in `starting` and clients re-requesting the reload forever (issue #349).
impl<'de> Deserialize<'de> for UpdateChannel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

impl std::fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stable => write!(f, "stable"),
            Self::Main => write!(f, "main"),
        }
    }
}

/// Cross-provider failover behavior when the same input would be resent elsewhere.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CrossProviderFailoverMode {
    /// Show a 3-second cancelable countdown, then resend on another provider.
    #[default]
    Countdown,
    /// Do not resend the prompt to another provider automatically.
    #[serde(alias = "off", alias = "false", alias = "disabled", alias = "none")]
    Manual,
}

impl CrossProviderFailoverMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Countdown => "countdown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "manual" | "off" | "false" | "disabled" | "none" => Some(Self::Manual),
            "countdown" | "auto" | "automatic" => Some(Self::Countdown),
            _ => None,
        }
    }
}

/// Compaction configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    /// Compaction mode: reactive (default), proactive, or semantic
    pub mode: CompactionMode,

    /// [proactive] Number of turns to look ahead when projecting token growth
    pub lookahead_turns: usize,

    /// [proactive] EWMA alpha for token growth smoothing (0.0-1.0, higher = more recency bias)
    pub ewma_alpha: f32,

    /// [proactive/semantic] Minimum context fill level before any proactive check fires (0.0-1.0)
    pub proactive_floor: f32,

    /// [proactive/semantic] Minimum number of token snapshots needed before proactive check
    pub min_samples: usize,

    /// [proactive/semantic] Number of stable turns (no growth) before suppressing proactive compact
    pub stall_window: usize,

    /// [proactive/semantic] Minimum turns between two compactions (cooldown)
    pub min_turns_between_compactions: usize,

    /// [semantic] Cosine similarity threshold below which a topic shift is detected (0.0-1.0)
    pub topic_shift_threshold: f32,

    /// [semantic] Cosine similarity above which a message is kept verbatim (0.0-1.0)
    pub relevance_keep_threshold: f32,

    /// [semantic] Number of recent turns to look at for building the "current goal" embedding
    pub goal_window_turns: usize,

    /// Hard cap on the token budget compaction measures against, regardless of
    /// the model's advertised context window. 0 = no cap (use the model window).
    ///
    /// Every turn re-sends the whole transcript, so on a 1M-window model the
    /// default 80%-of-window trigger lets a session reach ~800k tokens per
    /// request before anything folds. Set this to e.g. 200000 to compact earlier
    /// on large-window providers. This bounds the compaction trigger budget,
    /// not the final request size when recent messages cannot be compacted.
    pub max_context_tokens: usize,

    /// Which engine owns compaction: `structured` (default), `openai_auto`, or
    /// `openai_explicit`.
    ///
    /// Left unset, the strategy is derived from an explicitly-written
    /// `[provider] openai_native_compaction_mode`. An explicitly-set strategy
    /// always wins over that legacy key.
    pub strategy: Option<CompactionStrategy>,

    /// Headroom kept free below the compaction trigger. Raised to
    /// `min(window * 0.04, 49152)` when the window needs more room.
    pub reserve_tokens: usize,

    /// Token allowance for the verbatim recent tail. Lowered when the window
    /// cannot spare it (`window * (1 - ratio - 0.05)`, floor 1024).
    pub recent_tokens: usize,

    /// Run proactive and idle compaction jobs in the background. Turning this
    /// off leaves threshold (reactive) compaction and manual `/compact`
    /// available; it never blocks a provider request.
    pub background: bool,

    /// Model for structured compaction summaries. Unset or empty uses the
    /// session model. Ignored by provider-native strategies.
    pub summary_model: Option<String>,
    /// Replace stale tool results with a short marker on the provider request.
    ///
    /// The stored transcript is untouched: an older `read` superseded by a
    /// newer one, or a result that reported nothing, costs one line instead of
    /// its full text until the conversation is compacted. Off by default
    /// because it changes what the model sees.
    pub prune_tool_results: bool,
}

impl CompactionConfig {
    /// The strategy that owns compaction for this config.
    ///
    /// An explicitly-set `strategy` wins. Otherwise only an explicitly-written
    /// `[provider] openai_native_compaction_mode` of `auto` or `explicit` selects
    /// the provider-native path; an absent key, an empty value, or an explicit
    /// `off` all mean structured compaction.
    pub fn resolved_strategy(&self, provider: &ProviderConfig) -> CompactionStrategy {
        if let Some(strategy) = self.strategy {
            return strategy;
        }
        match provider.explicit_native_compaction_mode() {
            Some("auto") => CompactionStrategy::OpenAiAuto,
            Some("explicit") => CompactionStrategy::OpenAiExplicit,
            _ => CompactionStrategy::Structured,
        }
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            mode: CompactionMode::Reactive,
            lookahead_turns: 15,
            ewma_alpha: 0.3,
            proactive_floor: 0.40,
            min_samples: 3,
            stall_window: 5,
            min_turns_between_compactions: 10,
            topic_shift_threshold: 0.45,
            relevance_keep_threshold: 0.65,
            goal_window_turns: 5,
            max_context_tokens: 0,
            strategy: None,
            reserve_tokens: 16_384,
            recent_tokens: 20_000,
            background: true,
            summary_model: None,
            prune_tool_results: false,
        }
    }
}

/// Which tool calls need the user's approval before they run.
///
/// The mode is a floor on friction, not a replacement for the command-risk
/// gate: a call the gate classifies as dangerous still asks in every mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalMode {
    /// No approval gate; the behavior every existing config already has.
    #[default]
    Off,
    /// Ask before a call that can mutate files.
    Write,
    /// Ask before any call that is not read-only.
    All,
}

impl ApprovalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Write => "write",
            Self::All => "all",
        }
    }

    /// Parse a mode name, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "write" => Some(Self::Write),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// Lenient like [`UpdateChannel`]: a stale `approval_mode` value in config.toml
/// falls back to `off` rather than discarding every other setting in the file.
impl<'de> Deserialize<'de> for ApprovalMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

/// How the `bash` tool reacts to a shell idiom another tool handles better.
///
/// The interceptor recognises `cat`/`head`/`tail`, `grep`/`rg`, `find`/`fd`,
/// in-place `sed`/`perl`/`awk`, shell redirection, `nohup`, and a trailing `&`.
/// A rule only fires when the dedicated tool it names is reachable under the
/// active allow-list, so a narrowed profile is never nudged toward a tool it
/// cannot call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BashInterceptorMode {
    /// Run the command as written; the behavior every existing config has.
    #[default]
    Off,
    /// Run the command and prefix its output with the note.
    Nudge,
    /// Refuse the call before anything is spawned.
    Block,
}

impl BashInterceptorMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Nudge => "nudge",
            Self::Block => "block",
        }
    }

    /// Parse a mode name, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "nudge" => Some(Self::Nudge),
            "block" => Some(Self::Block),
            _ => None,
        }
    }
}

/// Lenient like [`ApprovalMode`]: an unrecognized value falls back to `off`
/// rather than discarding every other setting in the file.
impl<'de> Deserialize<'de> for BashInterceptorMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

/// What one `[[tools.bash_patterns]]` rule decides for a matching command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BashPatternApproval {
    /// Run without asking. Only a single plain command can be allowed.
    Allow,
    /// Refuse the call.
    Deny,
    /// Ask the user before the call runs.
    Prompt,
}

/// One explicit approval rule for shell commands.
///
/// `pattern` is matched against the whole command and, for `deny`/`prompt`,
/// against each command segment. `*` matches any run of characters and every
/// other character is literal. Rules are evaluated in config order and the
/// first match wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashPatternRule {
    pub pattern: String,
    pub approval: BashPatternApproval,
}

/// How credentials that appear in text leaving the agent are handled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretsMode {
    /// Pass text through untouched; the behavior every existing config has.
    #[default]
    Off,
    /// Replace detected credentials with stable placeholders, restoring them
    /// for the tool that needs the real value.
    Obfuscate,
    /// Refuse the call that would carry a credential outward.
    Block,
}

impl SecretsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Obfuscate => "obfuscate",
            Self::Block => "block",
        }
    }

    /// Parse a mode name, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "obfuscate" => Some(Self::Obfuscate),
            "block" => Some(Self::Block),
            _ => None,
        }
    }
}

/// Lenient like [`ApprovalMode`]: an unrecognized value must not make the whole
/// config file unreadable.
impl<'de> Deserialize<'de> for SecretsMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NamedProviderType {
    #[serde(alias = "openai-compatible", alias = "openai_compatible")]
    #[default]
    OpenAiCompatible,
    #[serde(alias = "anthropic-compatible", alias = "anthropic_compatible")]
    AnthropicCompatible,
    OpenRouter,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NamedProviderAuth {
    #[serde(alias = "Bearer", alias = "BEARER")]
    #[default]
    Bearer,
    #[serde(alias = "Header", alias = "HEADER")]
    Header,
    #[serde(alias = "None", alias = "NONE")]
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct NamedProviderModelConfig {
    pub id: String,
    /// Explicitly enable or disable `/effort` for this model. When omitted,
    /// the provider-level setting and built-in model-family detection apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// Reasoning effort selected when this model becomes active. This overrides
    /// `[provider].openai_reasoning_effort` for this model only.
    #[serde(default, alias = "reasoning-effort", skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(
        default,
        alias = "context_limit",
        alias = "context-length",
        alias = "context-window",
        alias = "context_length",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_window: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct NamedProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: NamedProviderType,
    pub base_url: String,
    pub api: Option<String>,
    pub auth: NamedProviderAuth,
    pub auth_header: Option<String>,
    /// Extra HTTP headers sent with every request to this provider.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
    pub env_file: Option<String>,
    pub default_model: Option<String>,
    pub requires_api_key: Option<bool>,
    #[serde(default)]
    pub provider_routing: bool,
    #[serde(default)]
    pub model_catalog: bool,
    #[serde(default)]
    pub allow_provider_pinning: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<NamedProviderModelConfig>,
    /// Extra top-level JSON fields merged into every chat/completions request
    /// body sent to this provider. Lets users inject non-standard parameters
    /// some OpenAI-compatible backends require (e.g. NVIDIA NIM DeepSeek-V4
    /// needs `chat_template_kwargs = { thinking = true, reasoning_effort = "high" }`).
    /// Must be a JSON object; keys here override horon-agent-generated body fields.
    #[serde(default, alias = "extra-body", skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
    /// Send a lineage-stable `prompt_cache_key` with every request to this
    /// endpoint. Unset sends it only to hosts whose gateway documents the field
    /// (`api.openai.com`, `openrouter.ai`); `false` keeps strict-schema
    /// endpoints from rejecting an unknown field.
    #[serde(default, alias = "prompt-cache-key", skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<bool>,
    /// Whether this endpoint accepts the DeepSeek-style top-level
    /// `reasoning_effort` request field (`/effort` support). When unset, horon-agent
    /// auto-detects it from the active model id (DeepSeek-family models
    /// support it regardless of which gateway serves them). Set `false` to
    /// suppress auto-detection for strict-schema endpoints.
    #[serde(
        default,
        alias = "supports-reasoning-effort",
        alias = "reasoning_effort",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_reasoning_effort: Option<bool>,
    /// Disable model-name based reasoning detection for this profile. Explicit
    /// provider/model capability settings continue to work.
    #[serde(default, alias = "disable-reasoning-heuristics")]
    pub disable_reasoning_heuristics: bool,
}

/// Remembered trust decisions for external auth sources managed by other tools.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuthConfig {
    /// External auth source ids that the user has approved horon-agent to read/use.
    pub trusted_external_sources: Vec<String>,
    /// Path-bound approvals for external auth sources managed by other tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_external_source_paths: Vec<String>,
}

/// Agent-specific model defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    /// Optional default model override for spawned swarm/subagent sessions.
    ///
    /// Leave unset (or use `"inherit"` / `"coordinator"`) to have spawned swarm
    /// agents inherit the spawning coordinator's model. Set to a concrete model
    /// string to change the worker default. An explicit `model` in the swarm
    /// tool overrides this default for newly spawned workers.
    pub swarm_model: Option<String>,
    /// Optional default reasoning effort for spawned swarm/subagent sessions
    /// (`"low"`, `"medium"`, `"high"`, ...). Applied when a `swarm spawn`
    /// call does not pass an explicit `effort`. Leave unset to let workers
    /// inherit the provider-wide reasoning effort.
    pub swarm_effort: Option<String>,
    /// Root reasoning effort in light swarm mode. Unset or invalid means `max`.
    /// This does not change worker effort (`swarm_effort`).
    pub swarm_root_effort: Option<String>,
    /// Root reasoning effort in deep swarm mode. Unset or invalid means `max`.
    pub swarm_deep_root_effort: Option<String>,
    /// Default spawn mode for swarm-created agents. Every mode runs the worker
    /// in this process; the value only names how the coordinator reports it.
    pub swarm_spawn_mode: SwarmSpawnMode,
    /// Jev Decisions provider for recall: auto, openrouter, typesafe, aimlapi,
    /// or horon-agent. Auto uses a provider-specific BYOK credential before HoronAgent.
    #[serde(default = "default_memory_jev_provider")]
    pub memory_jev_provider: String,
    /// Minimum Jev relevance probability. Invalid values fail closed.
    #[serde(default = "default_memory_jev_threshold")]
    pub memory_jev_threshold: f32,
    /// Optional model override for memory extraction only, never recall.
    pub memory_model: Option<String>,
    /// Whether optional automatic memory extraction may use a text-generating
    /// sidecar. Recall always uses Jev and is independent of this setting.
    #[serde(default = "default_memory_sidecar_enabled")]
    pub memory_sidecar_enabled: bool,
    /// Legacy setting, retained for config compatibility. Jev recall ignores it.
    #[serde(default = "default_memory_rerank_cadence")]
    pub memory_rerank_cadence: usize,
    /// Legacy setting, retained for config compatibility. Jev recall ignores it.
    #[serde(default = "default_memory_rerank_votes")]
    pub memory_rerank_votes: usize,
    /// Legacy setting, retained for config compatibility. Jev recall ignores it.
    #[serde(default = "default_memory_rerank_min_agree")]
    pub memory_rerank_min_agree: usize,
    /// Legacy benchmark/debug embedding backend. Jev recall never uses it.
    #[serde(default = "default_memory_embedding_backend")]
    pub memory_embedding_backend: String,
    /// OpenAI embedding model name when `memory_embedding_backend = "openai"`.
    /// Unset = `text-embedding-3-small`. Env: `HORON_AGENT_MEMORY_EMBEDDING_MODEL`.
    #[serde(default)]
    pub memory_embedding_model: Option<String>,
    /// Optional override for the embeddings API base URL (no trailing slash),
    /// for OpenAI-compatible gateways. Unset = `https://api.openai.com/v1`.
    /// Env: `HORON_AGENT_MEMORY_EMBEDDING_BASE_URL`.
    #[serde(default)]
    pub memory_embedding_base_url: Option<String>,
    /// Optional override for the remote embedding dimensionality (vector-space
    /// metadata / sanity checks). Unset = inferred from the model name.
    #[serde(default)]
    pub memory_embedding_dim: Option<usize>,
    /// Maximum number of live swarm worker agents in one swarm. This is the RAM
    /// safety budget for both recursive ad hoc spawning and deep-mode `run_plan`
    /// parallelism. Completed/stopped workers do not consume slots. Light mode
    /// still uses a smaller fixed fan-out. `0` disables this configurable guard,
    /// leaving only the absolute `MAX_SWARM_MEMBERS` hard cap.
    /// Env override: `HORON_AGENT_SWARM_MAX_CONCURRENT_AGENTS`.
    #[serde(default = "default_swarm_max_concurrent_agents")]
    pub swarm_max_concurrent_agents: usize,
    /// Stalled-worker watchdog window in seconds. A running swarm worker with
    /// no streamed tokens, tool events, or task heartbeats for this long, and
    /// no tool currently executing, is flagged as stalled in swarm status and
    /// its coordinator is notified once. `0` disables the watchdog.
    /// Env override: `HORON_AGENT_SWARM_STALL_AFTER_SECS`.
    #[serde(default = "default_swarm_stall_after_secs")]
    pub swarm_stall_after_secs: u64,
    /// When the watchdog flags a worker as stalled, also queue a short nudge
    /// message into the worker's own session. Off by default.
    /// Env override: `HORON_AGENT_SWARM_STALL_NUDGE`.
    #[serde(default)]
    pub swarm_stall_nudge: bool,
}

fn default_swarm_stall_after_secs() -> u64 {
    600
}

fn default_swarm_max_concurrent_agents() -> usize {
    32
}

fn default_memory_embedding_backend() -> String {
    "local".to_string()
}

fn default_memory_jev_provider() -> String {
    "auto".to_string()
}

fn default_memory_jev_threshold() -> f32 {
    0.8
}

fn default_memory_sidecar_enabled() -> bool {
    true
}

fn default_memory_rerank_cadence() -> usize {
    3
}

fn default_memory_rerank_votes() -> usize {
    2
}

fn default_memory_rerank_min_agree() -> usize {
    2
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            swarm_model: None,
            swarm_effort: None,
            swarm_root_effort: None,
            swarm_deep_root_effort: None,
            swarm_spawn_mode: SwarmSpawnMode::default(),
            memory_jev_provider: default_memory_jev_provider(),
            memory_jev_threshold: default_memory_jev_threshold(),
            memory_model: None,
            memory_sidecar_enabled: default_memory_sidecar_enabled(),
            memory_rerank_cadence: default_memory_rerank_cadence(),
            memory_rerank_votes: default_memory_rerank_votes(),
            memory_rerank_min_agree: default_memory_rerank_min_agree(),
            memory_embedding_backend: default_memory_embedding_backend(),
            memory_embedding_model: None,
            memory_embedding_base_url: None,
            memory_embedding_dim: None,
            swarm_max_concurrent_agents: default_swarm_max_concurrent_agents(),
            swarm_stall_after_secs: default_swarm_stall_after_secs(),
            swarm_stall_nudge: false,
        }
    }
}

impl AgentsConfig {
    /// Resolve a swarm mode's root effort without allowing orchestration
    /// sentinels to recurse into another mode. Unknown values preserve the
    /// historical maximum-effort behavior without invalidating other settings.
    pub fn root_effort_for_swarm(&self, deep: bool) -> &'static str {
        let configured = if deep {
            self.swarm_deep_root_effort.as_deref()
        } else {
            self.swarm_root_effort.as_deref()
        };
        let value = configured.unwrap_or("max").trim();
        ["none", "minimal", "low", "medium", "high", "xhigh", "max"]
            .into_iter()
            .find(|level| level.eq_ignore_ascii_case(value))
            .unwrap_or("max")
    }
}

/// How swarm-created agents should be spawned.
///
/// Every mode runs the worker in this process; the agent never opens a terminal
/// window. The mode names how the coordinator surfaces the worker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SwarmSpawnMode {
    /// Create the worker in-process.
    Headless,
    /// Like headless, but the coordinator renders a live inline gallery
    /// viewport of each worker's streaming output.
    #[default]
    #[serde(alias = "visible", alias = "headed")]
    Inline,
    /// Historical "try visible first" mode. Resolves to the configured mode,
    /// which is in-process, so an old config keeps working unchanged.
    Auto,
}

impl SwarmSpawnMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            // `visible`/`headed` selected the removed terminal-window spawn;
            // they now mean in-process like `inline`.
            "visible" | "headed" => Some(Self::Inline),
            "headless" => Some(Self::Headless),
            "inline" => Some(Self::Inline),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    /// Canonical lowercase string for this mode (matches the config/env values).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::Inline => "inline",
            Self::Auto => "auto",
        }
    }
}

/// Lifecycle hooks: external commands horon-agent runs at well-defined points.
///
/// Hook commands are parsed shell-style (quotes work) but executed directly,
/// with `HORON_AGENT_HOOK_*` env vars describing the event (`HORON_AGENT_HOOK_EVENT`,
/// `HORON_AGENT_HOOK_SESSION_ID`, `HORON_AGENT_HOOK_CWD`, event-specific fields, and a
/// `HORON_AGENT_HOOK_PAYLOAD` JSON mirror). Hook processes get
/// `HORON_AGENT_HOOKS_DISABLED=1` so nested horon-agent invocations don't recurse.
///
/// All hooks except `pre_tool` are observers: detached, fire-and-forget,
/// failures only logged. `pre_tool` is a gate: horon-agent waits for it and exit
/// code 2 blocks the tool call (stderr becomes the error shown to the model);
/// exit 0 allows; anything else fails open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCommands(Vec<String>);

impl HookCommands {
    pub fn one(command: impl Into<String>) -> Self {
        Self(vec![command.into()])
    }

    pub fn many(commands: Vec<String>) -> Self {
        Self(commands)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    pub fn first(&self) -> Option<&str> {
        self.0.first().map(String::as_str)
    }
}

impl Serialize for HookCommands {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if let [command] = self.0.as_slice() {
            command.serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for HookCommands {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }

        Ok(match OneOrMany::deserialize(deserializer)? {
            OneOrMany::One(command) => Self::one(command),
            OneOrMany::Many(commands) => Self::many(commands),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Synchronous input transformers run before each tool call. Each receives
    /// the tool input JSON on stdin and may print replacement JSON on stdout.
    /// Empty stdout or any failure leaves the input unchanged.
    pub pre_tool_transform: Option<HookCommands>,
    /// Max milliseconds to wait for each input transformer (default: 500).
    pub pre_tool_transform_timeout_ms: u64,
    /// Runs when an agent turn begins (after the user message is added and
    /// before the model starts generating). Fires before the first `pre_tool`,
    /// so integrations can detect that the agent is actively working even while
    /// it is only thinking/streaming text. Fields: MODEL, SOURCE
    /// ("chat"/"resume"/"ambient"). Env override: HORON_AGENT_HOOK_TURN_START.
    pub turn_start: Option<HookCommands>,
    /// Runs when an agent turn completes.
    /// Fields: STATUS ("ok"/"error"), DURATION_MS, MODEL, LAST_ASSISTANT_TEXT.
    /// Env override: HORON_AGENT_HOOK_TURN_END.
    pub turn_end: Option<HookCommands>,
    /// Runs when a session becomes active (created or resumed).
    /// Fields: SOURCE ("create"/"resume").
    /// Env override: HORON_AGENT_HOOK_SESSION_START.
    pub session_start: Option<HookCommands>,
    /// Runs when a session closes normally.
    /// Env override: HORON_AGENT_HOOK_SESSION_END.
    pub session_end: Option<HookCommands>,
    /// Gate hook before each tool call. Receives TOOL_NAME and the tool input
    /// JSON on stdin (also truncated in TOOL_INPUT). Exit 0 allows, exit 2
    /// blocks (stderr is fed back to the model), anything else fails open.
    /// Env override: HORON_AGENT_HOOK_PRE_TOOL.
    pub pre_tool: Option<HookCommands>,
    /// Runs after each tool call completes.
    /// Fields: TOOL_NAME, TOOL_CALL_ID, TOOL_INPUT, TOUCHED_PATHS, STATUS
    /// ("ok"/"error"), DURATION_MS, OUTPUT_BYTES, ERROR.
    /// Env override: HORON_AGENT_HOOK_POST_TOOL.
    pub post_tool: Option<HookCommands>,
    /// Synchronous hook after each successful tool call. Same fields as
    /// post_tool, tool input JSON on stdin. Its stdout is appended to the tool
    /// result the model sees (e.g. linter output for edited files).
    /// Env override: HORON_AGENT_HOOK_POST_TOOL_FEEDBACK.
    pub post_tool_feedback: Option<HookCommands>,
    /// Max milliseconds to wait for each post_tool_feedback hook (default:
    /// 10000). Env override: HORON_AGENT_HOOK_POST_TOOL_FEEDBACK_TIMEOUT_MS.
    pub post_tool_feedback_timeout_ms: u64,
    /// Max milliseconds to wait for the pre_tool gate before failing open
    /// (default: 5000). Env override: HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS.
    pub pre_tool_timeout_ms: u64,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            pre_tool_transform: None,
            pre_tool_transform_timeout_ms: 500,
            turn_start: None,
            turn_end: None,
            session_start: None,
            session_end: None,
            pre_tool: None,
            post_tool: None,
            post_tool_feedback: None,
            post_tool_feedback_timeout_ms: 10_000,
            pre_tool_timeout_ms: 5000,
        }
    }
}

/// Automatic end-of-turn code review configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutoReviewConfig {
    /// Enable autoreview by default for new/resumed sessions (default: false)
    pub enabled: bool,
    /// Optional model override for autoreview reviewer sessions.
    pub model: Option<String>,
}

/// Integration discovery configuration (the `[sponsors]` section).
///
/// Integration discovery makes third-party developer tools discoverable to
/// the agent via the `integration_tools` tool backed by a hosted directory.
/// Some providers may share revenue with HoronAgent when a referred user
/// becomes a customer, but partnership status never influences
/// recommendations. See <https://horon-agent.sh/discovery-tools>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SponsorsConfig {
    /// Enable integration discovery. Enabled by default; set to false to opt
    /// out. When false, no discovery categories are added to the prompt, the
    /// `integration_tools` tool is not registered, and horon-agent never
    /// contacts the discovery endpoint.
    pub enabled: bool,
    /// Base URL of the discovery endpoint.
    pub endpoint: String,
}

impl Default for SponsorsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: "https://api.horon-agent.sh/v1/discovery".to_string(),
        }
    }
}

/// Automatic end-of-turn execution judging configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutoJudgeConfig {
    /// Enable autojudge by default for new/resumed sessions (default: false)
    pub enabled: bool,
    /// Optional model override for autojudge sessions.
    pub model: Option<String>,
}

/// How much automatic continuation the turn loops perform after a turn stops
/// in a way the harness did not expect (default: conservative).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TurnRecoveryMode {
    /// Never continue an unexpected stop automatically.
    Off,
    /// Act on provable truncation and unclosed structure only (default).
    #[default]
    Conservative,
    /// Act on every non-clean class, including the semantic guess.
    Aggressive,
}

impl TurnRecoveryMode {
    /// Parse a mode name, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" | "0" | "no" => Some(Self::Off),
            "conservative" | "default" | "on" | "true" | "1" | "yes" => Some(Self::Conservative),
            "aggressive" => Some(Self::Aggressive),
            _ => None,
        }
    }
}

/// An unknown or removed mode name (e.g. a stale `turn_recovery = "auto"` left
/// in config.toml) falls back to the default mode instead of failing the whole
/// config parse, matching `UpdateChannel`.
impl<'de> Deserialize<'de> for TurnRecoveryMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

/// How imported foreign rules reach the system prompt (default: inline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ForeignRulesMode {
    /// Append every rule body to the system prompt as before (default).
    #[default]
    Inline,
    /// Inline only always-on rules; list the rest for on-demand reads, so a
    /// rule enters the context only when its globs match a file the session
    /// touches.
    Rulebook,
}

impl ForeignRulesMode {
    /// Parse a mode name, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "inline" | "all" | "default" => Some(Self::Inline),
            "rulebook" | "context" | "on-demand" => Some(Self::Rulebook),
            _ => None,
        }
    }
}

/// An unknown mode name falls back to inline instead of failing the config
/// parse, matching `TurnRecoveryMode`.
impl<'de> Deserialize<'de> for ForeignRulesMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

/// How often a mid-stream rule may fire (default: every turn).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StreamRuleRepeat {
    /// Fire on every turn that matches (default).
    #[default]
    Turn,
    /// Fire at most once per session.
    Once,
    /// Fire at most once per `repeat_gap` user turns.
    AfterGap,
}

impl StreamRuleRepeat {
    /// Parse a repeat mode, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "turn" | "every-turn" | "default" => Some(Self::Turn),
            "once" | "session" => Some(Self::Once),
            "after-gap" | "gap" => Some(Self::AfterGap),
            _ => None,
        }
    }
}

/// An unknown repeat mode falls back to the default instead of failing the
/// config parse, matching `TurnRecoveryMode`.
impl<'de> Deserialize<'de> for StreamRuleRepeat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

/// How a matched rule reaches the model (default: interrupt).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StreamRuleDelivery {
    /// Abort the attempt and re-issue the request with the rule body, or block
    /// the tool call the rule matched (default).
    #[default]
    Interrupt,
    /// Let the work stand and carry the rule body as a passive system reminder.
    Passive,
}

impl StreamRuleDelivery {
    /// Parse a delivery mode, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "interrupt" | "default" => Some(Self::Interrupt),
            "passive" | "reminder" => Some(Self::Passive),
            _ => None,
        }
    }
}

/// An unknown delivery mode falls back to the default instead of failing the
/// config parse, matching `TurnRecoveryMode`.
impl<'de> Deserialize<'de> for StreamRuleDelivery {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

/// One operator-written mid-stream rule.
///
/// `pattern` is a Rust `regex` expression matched against the assistant's own
/// streaming text; `body` is injected as a system reminder when it matches.
/// `id` names the rule in the transcript entry and in the logs, so two rules
/// cannot be confused after the fact.
///
/// A rule with no `scope` watches assistant text. `tool` watches every finalized
/// tool call, and `tool:<name>` watches one; `paths` narrows a tool rule to
/// calls whose `file_path`/`path` argument matches one of its globs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamRuleConfig {
    /// Stable identifier for this rule.
    pub id: String,
    /// Rust `regex` pattern matched against the rolling lookback window or the
    /// compact JSON of a tool call's input.
    pub pattern: String,
    /// Text injected as a system reminder when the pattern matches.
    pub body: String,
    /// `text` (default when empty), `tool`, or `tool:<name>`.
    pub scope: Vec<String>,
    /// Globs matched against a tool call's `file_path`/`path` argument. Empty
    /// matches any path.
    pub paths: Vec<String>,
    /// How often the rule may fire.
    pub repeat: StreamRuleRepeat,
    /// User turns between fires for `repeat = "after-gap"`.
    pub repeat_gap: u32,
    /// Whether a match interrupts the work or arrives as a passive reminder.
    pub delivery: StreamRuleDelivery,
}

impl Default for StreamRuleConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            pattern: String::new(),
            body: String::new(),
            scope: Vec::new(),
            paths: Vec::new(),
            repeat: StreamRuleRepeat::Turn,
            repeat_gap: 10,
            delivery: StreamRuleDelivery::Interrupt,
        }
    }
}

/// `[advisor]`: a second, independent model that watches the session and files
/// narrow notes for the main agent to weigh.
///
/// Off by default. The advisor reads the transcript and only the read-only
/// inspection tools; it never starts a turn of its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvisorConfig {
    /// Whether the advisor runs for this session. Opt-in (default: false).
    pub enabled: bool,
    /// Model override for the advisor. `None` inherits the session provider.
    pub model: Option<String>,
    /// Non-blocker notes one update may deliver. Blockers are exempt.
    pub max_notes_per_update: u32,
    /// User turns after a blocker interrupt during which no further interrupt
    /// is sent and every note rides the reminder slot instead.
    pub immune_turns: u32,
}

impl Default for AdvisorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: None,
            max_notes_per_update: 4,
            immune_turns: 3,
        }
    }
}

impl AdvisorConfig {
    /// Upper bound on the notes a single update may deliver, clamped to the
    /// documented range. Zero would silently mute the advisor, so it floors at
    /// one instead of honoring it.
    pub fn note_budget(&self) -> u32 {
        self.max_notes_per_update.clamp(1, 32)
    }
}

mod web_search;
pub use web_search::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// Default model to use (e.g. "claude-opus-4-8", "copilot:claude-opus-4.6")
    pub default_model: Option<String>,
    /// Default provider to use (claude|openai|copilot|openrouter)
    pub default_provider: Option<String>,
    /// Reasoning effort for OpenAI Responses API (none|minimal|low|medium|high|xhigh|max)
    pub openai_reasoning_effort: Option<String>,
    /// Reasoning effort for Anthropic Messages API output_config (none|low|medium|high|xhigh; max aliases to strongest supported)
    pub anthropic_reasoning_effort: Option<String>,
    /// Request one-hour Anthropic prompt caching instead of five minutes.
    pub anthropic_cache_ttl_1h: bool,
    /// OpenAI transport mode (auto|websocket|https)
    pub openai_transport: Option<String>,
    /// OpenAI service tier override (priority|flex)
    pub openai_service_tier: Option<String>,
    /// OpenAI native compaction mode: "auto", "explicit", or "off".
    ///
    /// Only a value written in the config file (or set by an environment
    /// override) counts. An absent key resolves to `"off"`, so an existing
    /// config file that never mentions this setting keeps horon's structured
    /// compaction instead of silently handing ownership to the provider.
    #[serde(
        default = "native_compaction_mode_absent",
        deserialize_with = "de_native_compaction_mode"
    )]
    pub openai_native_compaction_mode: String,
    /// Token threshold at which OpenAI auto native compaction should trigger.
    pub openai_native_compaction_threshold_tokens: usize,
    /// Preserve provider-native reasoning/thinking items for future-turn context when supported.
    pub preserve_reasoning_context: bool,
    /// How to handle cross-provider failover when the same input would be resent elsewhere.
    pub cross_provider_failover: CrossProviderFailoverMode,
    /// Whether horon-agent should automatically try another account on the same provider
    /// before falling back to a different provider.
    pub same_provider_account_failover: bool,
    /// Pin the `gemini` provider to Code Assist OAuth even when a Gemini
    /// Developer API key (`gemini.env` / `GEMINI_API_KEY`) is present. Without
    /// this an API key silently wins and every turn bills per token on the
    /// key's project. `HORON_AGENT_GEMINI_FORCE_OAUTH` overrides this value.
    pub gemini_force_oauth: bool,
    /// Google Cloud project for Gemini Code Assist OAuth. Workspace accounts
    /// require one; without it every turn fails with "requires setting
    /// GOOGLE_CLOUD_PROJECT". `GOOGLE_CLOUD_PROJECT` (or its legacy `_ID`
    /// alias) overrides this value. Config values are never exported to env.
    pub gemini_project: Option<String>,
    /// Max seconds to wait for streaming data before timing out a request with
    /// no data received. Base budget only: high reasoning efforts scale it up
    /// automatically (see `horon_agent_base::provider::stream_idle_timeout_for_effort`).
    /// Default: 180. Overridable via `HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS`.
    pub stream_idle_timeout_secs: u64,
    /// Maximum request attempts for transient provider errors, including the
    /// initial attempt. Default: 8. Overridable via `HORON_AGENT_MAX_RETRIES`.
    pub max_retries: u32,
    /// Maximum exponential-backoff delay between transient-error retries.
    /// Default: 30 seconds. Overridable via `HORON_AGENT_RETRY_BACKOFF_CAP_SECS`.
    pub retry_backoff_cap_secs: u64,
    /// Let an unattended run switch to another auto-switch account on its own
    /// when the provider refuses the request, instead of waiting for the
    /// interactive countdown. Off by default: an attended session keeps the
    /// countdown and the human's cancel. Default: false.
    pub autonomous_failover: bool,
    /// Automatic route switches allowed per execution chain before the run gives
    /// up with a final error. `0` disables switching without changing
    /// `autonomous_failover`. Default: 3.
    #[serde(default = "default_autonomous_failover_max_attempts")]
    pub autonomous_failover_max_attempts: u32,
}

/// Canonical default for `autonomous_failover_max_attempts`.
///
/// The durable failover budget in `horon-agent-app-core` spends attempts against
/// this same number, so the persisted counter and the configured cap cannot
/// drift apart.
pub const DEFAULT_AUTONOMOUS_FAILOVER_MAX_ATTEMPTS: u32 = 3;

fn default_autonomous_failover_max_attempts() -> u32 {
    DEFAULT_AUTONOMOUS_FAILOVER_MAX_ATTEMPTS
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            default_model: None,
            default_provider: None,
            openai_reasoning_effort: Some("low".to_string()),
            anthropic_reasoning_effort: None,
            anthropic_cache_ttl_1h: true,
            openai_transport: None,
            openai_service_tier: Some("priority".to_string()),
            openai_native_compaction_mode: "off".to_string(),
            openai_native_compaction_threshold_tokens: 200_000,
            preserve_reasoning_context: true,
            cross_provider_failover: CrossProviderFailoverMode::Countdown,
            same_provider_account_failover: true,
            gemini_force_oauth: false,
            gemini_project: None,
            stream_idle_timeout_secs: 180,
            max_retries: 8,
            retry_backoff_cap_secs: 30,
            autonomous_failover: false,
            autonomous_failover_max_attempts: DEFAULT_AUTONOMOUS_FAILOVER_MAX_ATTEMPTS,
        }
    }
}

impl ProviderConfig {
    /// The explicitly-configured [OI] native compaction mode, if the operator
    /// actually asked for one.
    ///
    /// Returns `None` for `off` (including the absent-key default) and for
    /// anything unrecognized, so an implicit default can never select the
    /// provider-native path.
    pub fn explicit_native_compaction_mode(&self) -> Option<&'static str> {
        match self.openai_native_compaction_mode.trim().to_ascii_lowercase().as_str() {
            "auto" => Some("auto"),
            "explicit" | "manual" => Some("explicit"),
            _ => None,
        }
    }
}

/// Absent `[provider] openai_native_compaction_mode` means structured compaction.
fn native_compaction_mode_absent() -> String {
    "off".to_string()
}

/// Normalize a written mode; unknown values fall back to `off` (structured)
/// instead of silently enabling provider-native compaction.
fn de_native_compaction_mode<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    let normalized = match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => "auto",
        "explicit" | "manual" => "explicit",
        _ => "off",
    };
    Ok(normalized.to_string())
}

/// Ambient mode configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AmbientConfig {
    /// Enable ambient mode (default: false)
    pub enabled: bool,
    /// Provider override (default: auto-select)
    pub provider: Option<String>,
    /// Model override (default: provider's strongest)
    pub model: Option<String>,
    /// Allow API key usage (default: false, only OAuth)
    pub allow_api_keys: bool,
    /// Daily token budget when using API keys
    pub api_daily_budget: Option<u64>,
    /// Minimum interval between cycles in minutes (default: 5)
    pub min_interval_minutes: u32,
    /// Maximum interval between cycles in minutes (default: 120)
    pub max_interval_minutes: u32,
    /// Pause ambient when user has active session (default: true)
    pub pause_on_active_session: bool,
    /// Enable proactive work vs garden-only (default: true)
    pub proactive_work: bool,
    /// Proactive work branch prefix (default: "ambient/")
    pub work_branch_prefix: String,
    /// How long an unattended run waits for a human decision on a permission
    /// request before it applies `approval_default_on_timeout`, in seconds
    /// (default: 1800).
    #[serde(default = "default_approval_timeout_secs")]
    pub approval_timeout_secs: u64,
    /// What to do when `approval_timeout_secs` elapses with no decision.
    /// `"deny"` fails closed; `"approve"` runs the gated action. Any other
    /// value is treated as `"deny"`. Default: `"deny"`.
    #[serde(default = "default_approval_default_on_timeout")]
    pub approval_default_on_timeout: String,
    /// Let `request_permission` run in a session that was never registered as
    /// an ambient session, which is what an unattended run without an attached
    /// client looks like. Off by default, so interactive sessions keep the
    /// ambient-only gate and its error message.
    #[serde(default)]
    pub approval_for_clientless_runs: bool,
}

fn default_approval_timeout_secs() -> u64 {
    1800
}

fn default_approval_default_on_timeout() -> String {
    "deny".to_string()
}

impl Default for AmbientConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: None,
            model: None,
            allow_api_keys: false,
            api_daily_budget: None,
            min_interval_minutes: 5,
            max_interval_minutes: 120,
            pause_on_active_session: true,
            proactive_work: true,
            work_branch_prefix: "ambient/".to_string(),
            approval_timeout_secs: default_approval_timeout_secs(),
            approval_default_on_timeout: default_approval_default_on_timeout(),
            approval_for_clientless_runs: false,
        }
    }
}

/// Safety system & notification configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    /// ntfy.sh topic name (required for push notifications)
    pub ntfy_topic: Option<String>,
    /// ntfy.sh server URL (default: https://ntfy.sh)
    pub ntfy_server: String,
    /// Enable desktop notifications via notify-send (default: true)
    pub desktop_notifications: bool,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            ntfy_topic: None,
            ntfy_server: "https://ntfy.sh".to_string(),
            desktop_notifications: true,
        }
    }
}

#[cfg(test)]
mod ambient_approval_defaults_tests {
    use super::*;

    /// A config file written before the approval-wait keys existed must keep
    /// parsing, with a real timeout and a fail-closed default. `#[serde(default)]`
    /// on a `u64` would give 0 and on a `String` an empty value, so both fields
    /// name an explicit default function.
    #[test]
    fn legacy_ambient_section_gets_explicit_approval_defaults() {
        let legacy: AmbientConfig = serde_json::from_str(r#"{"enabled": true, "min_interval_minutes": 7}"#)
            .expect("legacy ambient section must parse");
        assert!(legacy.enabled);
        assert_eq!(legacy.min_interval_minutes, 7);
        assert_eq!(legacy.approval_timeout_secs, 1800);
        assert_eq!(legacy.approval_default_on_timeout, "deny");
        assert!(
            !legacy.approval_default_on_timeout.is_empty(),
            "an empty string would be read as an unknown timeout policy"
        );

        // An unknown extra key from a newer writer must not break parsing.
        let with_extra: AmbientConfig = serde_json::from_str(
            r#"{"approval_timeout_secs": 60, "approval_default_on_timeout": "approve", "future_key": [1, 2]}"#,
        )
        .expect("unknown keys must be ignored");
        assert_eq!(with_extra.approval_timeout_secs, 60);
        assert_eq!(with_extra.approval_default_on_timeout, "approve");
    }

    #[test]
    fn ambient_default_impl_matches_the_serde_defaults() {
        let defaults = AmbientConfig::default();
        assert_eq!(defaults.approval_timeout_secs, 1800);
        assert_eq!(defaults.approval_default_on_timeout, "deny");

        let parsed: AmbientConfig = serde_json::from_str("{}").expect("empty object must parse");
        assert_eq!(parsed.approval_timeout_secs, defaults.approval_timeout_secs);
        assert_eq!(parsed.approval_default_on_timeout, defaults.approval_default_on_timeout);
    }

    #[test]
    fn approval_fields_round_trip() {
        let config = AmbientConfig {
            approval_timeout_secs: 45,
            approval_default_on_timeout: "approve".to_string(),
            ..AmbientConfig::default()
        };
        let encoded = serde_json::to_string(&config).expect("serialize ambient config");
        let decoded: AmbientConfig = serde_json::from_str(&encoded).expect("parse ambient config");
        assert_eq!(decoded.approval_timeout_secs, 45);
        assert_eq!(decoded.approval_default_on_timeout, "approve");
    }

    #[test]
    fn clientless_approval_gate_is_off_unless_written() {
        let defaults = AmbientConfig::default();
        assert!(!defaults.approval_for_clientless_runs);

        let parsed: AmbientConfig = serde_json::from_str("{\"enabled\": true}").expect("legacy section must parse");
        assert!(
            !parsed.approval_for_clientless_runs,
            "the ambient-only gate must stay closed for configs written before this key"
        );

        let enabled: AmbientConfig =
            serde_json::from_str("{\"approval_for_clientless_runs\": true}").expect("flag must parse");
        assert!(enabled.approval_for_clientless_runs);
    }
}

#[cfg(test)]
mod compaction_strategy_tests {
    use super::*;

    #[test]
    fn compaction_strategy_defaults_to_structured() {
        assert_eq!(CompactionStrategy::default(), CompactionStrategy::Structured);
        assert_eq!(CompactionConfig::default().strategy, None);
        assert_eq!(
            CompactionConfig::default().resolved_strategy(&ProviderConfig::default()),
            CompactionStrategy::Structured
        );
        assert_eq!(CompactionStrategy::default().as_str(), "structured");
    }

    #[test]
    fn absent_native_mode_is_not_treated_as_explicit() {
        // An old config file has no key at all: that must stay structured.
        let provider: ProviderConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(provider.openai_native_compaction_mode, "off");
        assert_eq!(provider.explicit_native_compaction_mode(), None);

        let provider: ProviderConfig = serde_json::from_str(r#"{"openai_native_compaction_mode":"auto"}"#).unwrap();
        assert_eq!(provider.explicit_native_compaction_mode(), Some("auto"));
        assert_eq!(provider.openai_native_compaction_mode, "auto");

        // An empty string carries no setting; it must not enable native compaction.
        let provider: ProviderConfig = serde_json::from_str(r#"{"openai_native_compaction_mode":""}"#).unwrap();
        assert_eq!(provider.openai_native_compaction_mode, "off");
        assert_eq!(provider.explicit_native_compaction_mode(), None);
    }

    #[test]
    fn strategy_round_trips_through_serde() {
        for (text, expected) in [
            ("structured", CompactionStrategy::Structured),
            ("openai_auto", CompactionStrategy::OpenAiAuto),
            ("openai_explicit", CompactionStrategy::OpenAiExplicit),
        ] {
            let json = format!("\"{text}\"");
            assert_eq!(serde_json::from_str::<CompactionStrategy>(&json).unwrap(), expected);
            assert_eq!(CompactionStrategy::parse(text), Some(expected));
        }
    }

    #[test]
    fn explicit_strategy_wins_over_legacy_native_mode() {
        let provider = ProviderConfig {
            openai_native_compaction_mode: "auto".to_string(),
            ..Default::default()
        };
        // Direct assignment (the env-override path) is still an explicit choice.
        assert_eq!(provider.explicit_native_compaction_mode(), Some("auto"));

        let mut compaction = CompactionConfig {
            strategy: Some(CompactionStrategy::Structured),
            ..Default::default()
        };
        assert_eq!(compaction.resolved_strategy(&provider), CompactionStrategy::Structured);

        compaction.strategy = Some(CompactionStrategy::OpenAiExplicit);
        assert_eq!(
            compaction.resolved_strategy(&provider),
            CompactionStrategy::OpenAiExplicit
        );
    }

    #[test]
    fn provider_defaults_and_budget_defaults_match_policy() {
        let provider = ProviderConfig::default();
        assert_eq!(provider.openai_native_compaction_mode, "off");
        assert_eq!(provider.explicit_native_compaction_mode(), None);
        assert_eq!(provider.openai_native_compaction_threshold_tokens, 200_000);

        let compaction = CompactionConfig::default();
        assert_eq!(compaction.reserve_tokens, 16_384);
        assert_eq!(compaction.recent_tokens, 20_000);
    }
}

#[cfg(test)]
mod reasoning_display_defaults_tests {
    use super::*;

    #[test]
    fn explicit_reasoning_display_is_distinguishable_from_the_legacy_fallback() {
        // Front-ends (the desktop) apply their own default only when the user
        // has not chosen one, so this flag must not be true just because
        // `show_thinking` happens to be set.
        let mut display = DisplayConfig {
            reasoning_display: None,
            show_thinking: true,
            ..DisplayConfig::default()
        };
        assert!(!display.has_explicit_reasoning_display());
        assert_eq!(display.reasoning_display(), ReasoningDisplayMode::Full);

        display.set_reasoning_display(ReasoningDisplayMode::Current);
        assert!(display.has_explicit_reasoning_display());
        assert_eq!(display.reasoning_display(), ReasoningDisplayMode::Current);
        assert!(
            display.show_thinking,
            "any active display mode must keep reasoning requested from the provider"
        );

        display.set_reasoning_display(ReasoningDisplayMode::Off);
        assert!(display.has_explicit_reasoning_display());
        assert!(!display.show_thinking);
    }
}

#[cfg(test)]
#[path = "autonomous_failover_tests.rs"]
mod autonomous_failover_defaults_tests;
