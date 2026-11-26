//! Configuration file support for horon-agent
//!
//! Config is loaded from `~/.horon/config.toml` (or `$HORON_HOME/config.toml`)
//! Environment variables override config file settings.

pub use horon_agent_config_types::model_roles::{ModelRole, ModelRoles, SessionModelPreset, resolve_role_model};
pub use horon_agent_config_types::parse_model_reference;
pub use horon_agent_config_types::rewind_summary;
pub use horon_agent_config_types::{
    AdvisorConfig, AgentsConfig, AmbientConfig, ApprovalMode, AuthConfig, AutoJudgeConfig, AutoReviewConfig,
    BashInterceptorMode, BashPatternApproval, BashPatternRule, CompactionConfig, CompactionMode, CompactionStrategy,
    CrossProviderFailoverMode, CustomWebSearchConfig, CustomWebSearchMethod, DisplayConfig, FeatureConfig,
    ForeignRulesMode, GatewayConfig, HookCommands, HooksConfig, IrcConfig, ModelReference, NamedProviderAuth,
    NamedProviderConfig, NamedProviderModelConfig, NamedProviderType, PowerConfig, PrewalkConfig, PrewalkConfigError,
    ProviderConfig, ReasoningDisplayMode, SafetyConfig, SecretsMode, SponsorsConfig, StorageConfig, StorageSyncMode,
    StreamRuleConfig, StreamRuleDelivery, StreamRuleRepeat, SwarmSpawnMode, TurnRecoveryMode, UpdateChannel,
    WebSearchConfig, WebSearchEngine,
};
pub use horon_agent_tool_types::AskPolicy;
pub use mcp::McpSourcesConfig;
pub use messages::{InterruptMode, MessageDeliveryMode, MessagesConfig};
use serde::{Deserialize, Serialize};
pub use skills::SkillsConfig;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

mod artifact_output;
pub use artifact_output::ArtifactOutputConfig;
mod tool_surface;
use tool_surface::is_core_reachable;
pub use tool_surface::{CORE_DISCOVERABLE_TOOLS, CORE_TOOLS, ESSENTIAL_TOOLS};

const CONFIG_CACHE_CHECK_INTERVAL: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_millis(500)
};

const CONFIG_ENV_KEYS: &[&str] = &[
    "HOME",
    "HORON_AGENT_SWARM_STALL_AFTER_SECS",
    "HORON_AGENT_SWARM_STALL_NUDGE",
    "HORON_AGENT_ACP_PROFILE",
    "HORON_AGENT_ACP_PROTOCOL_V2",
    "HORON_AGENT_ACP_TOOL_PROFILE",
    "HORON_AGENT_AMBIENT_ENABLED",
    "HORON_AGENT_AMBIENT_MAX_INTERVAL",
    "HORON_AGENT_AMBIENT_MIN_INTERVAL",
    "HORON_AGENT_AMBIENT_MODEL",
    "HORON_AGENT_AMBIENT_PROACTIVE",
    "HORON_AGENT_AMBIENT_PROVIDER",
    "HORON_AGENT_AUTO_POKE",
    "HORON_AGENT_AUTOJUDGE_ENABLED",
    "HORON_AGENT_AUTOJUDGE_MODEL",
    "HORON_AGENT_AUTOJUDGE_ATTEMPT_TIMEOUT_MS",
    "HORON_AGENT_AUTOREVIEW_ENABLED",
    "HORON_AGENT_AUTOREVIEW_MODEL",
    "HORON_AGENT_AUTO_POKE",
    "HORON_AGENT_CHECK_UPDATES",
    "HORON_AGENT_BING_API_KEY",
    "HORON_AGENT_BING_API_KEY_ENV",
    "HORON_AGENT_BING_MARKET",
    "HORON_AGENT_GEMINI_FORCE_OAUTH",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_PROJECT_ID",
    "HORON_AGENT_WAKE_MODE",
    "HORON_AGENT_IDLE_HIBERNATION_SECS",
    "HORON_AGENT_CROSS_PROVIDER_FAILOVER",
    "HORON_AGENT_DEBUG_SOCKET",
    "HORON_AGENT_DEFAULT_REASONING_DISPLAY",
    "HORON_AGENT_DIFF_LINE_WRAP",
    "HORON_AGENT_DISABLE_BASE_TOOLS",
    "HORON_AGENT_DISABLED_TOOLS",
    "HORON_AGENT_GATEWAY_BIND_ADDR",
    "HORON_AGENT_GATEWAY_ENABLED",
    "HORON_AGENT_GATEWAY_PORT",
    "HORON_HOME",
    "HORON_AGENT_HOOK_PRE_TOOL",
    "HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM",
    "HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM_TIMEOUT_MS",
    "HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS",
    "HORON_AGENT_HOOK_POST_TOOL",
    "HORON_AGENT_HOOK_POST_TOOL_FEEDBACK",
    "HORON_AGENT_HOOK_POST_TOOL_FEEDBACK_TIMEOUT_MS",
    "HORON_AGENT_HOOK_PRE_REQUEST",
    "HORON_AGENT_HOOK_PRE_REQUEST_TIMEOUT_MS",
    "HORON_AGENT_HOOK_SESSION_END",
    "HORON_AGENT_HOOK_SESSION_START",
    "HORON_AGENT_HOOK_TURN_END",
    "HORON_AGENT_HOOK_TURN_START",
    "HORON_AGENT_IRC",
    "HORON_AGENT_KV_CACHE_MISS_NOTICES",
    "HORON_AGENT_MEMORY_EMBEDDING_BACKEND",
    "HORON_AGENT_MEMORY_EMBEDDING_BASE_URL",
    "HORON_AGENT_MEMORY_EMBEDDING_DIM",
    "HORON_AGENT_MEMORY_EMBEDDING_MODEL",
    "HORON_AGENT_MEMORY_ENABLED",
    "HORON_AGENT_MEMORY_JEV_PROVIDER",
    "HORON_AGENT_MEMORY_MODEL",
    "HORON_AGENT_MEMORY_SIDECAR_ENABLED",
    "HORON_AGENT_PERSIST_MEMORY_INJECTIONS",
    "HORON_AGENT_MESSAGE_TIMESTAMPS",
    "HORON_AGENT_MODEL",
    "HORON_AGENT_NO_EMOJI",
    "HORON_AGENT_NTFY_SERVER",
    "HORON_AGENT_NTFY_TOPIC",
    "HORON_AGENT_OPENAI_NATIVE_COMPACTION_MODE",
    "HORON_AGENT_OPENAI_NATIVE_COMPACTION_THRESHOLD_TOKENS",
    "HORON_AGENT_OPENAI_REASONING_EFFORT",
    "HORON_AGENT_OPENAI_SERVICE_TIER",
    "HORON_AGENT_OPENAI_TRANSPORT",
    "HORON_AGENT_ANTHROPIC_REASONING_EFFORT",
    "HORON_AGENT_PRESERVE_REASONING_CONTEXT",
    "HORON_AGENT_PREVENT_SLEEP_WHILE_STREAMING",
    "HORON_AGENT_PROVIDER",
    "HORON_AGENT_REASONING_DISPLAY",
    "HORON_AGENT_SAME_PROVIDER_ACCOUNT_FAILOVER",
    "HORON_AGENT_SEARXNG_URL",
    "HORON_AGENT_SHOW_THINKING",
    "HORON_AGENT_STORAGE_SYNC",
    "HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS",
    "HORON_AGENT_MAX_RETRIES",
    "HORON_AGENT_MCP_TOOLS",
    "HORON_AGENT_MCP_TOOLS_TOKEN_THRESHOLD",
    "HORON_AGENT_CODEMODE",
    "HORON_AGENT_NESTED_AGENTS_MD",
    "HORON_AGENT_READ_DEDUP",
    "HORON_AGENT_TOOL_RESULT_AGING",
    "HORON_AGENT_COMPACTION_SUMMARY_MODEL",
    "HORON_AGENT_RETRY_BACKOFF_CAP_SECS",
    "HORON_AGENT_SWARM_ENABLED",
    "HORON_AGENT_SWARM_EFFORT",
    "HORON_AGENT_SWARM_ROOT_EFFORT",
    "HORON_AGENT_SWARM_DEEP_ROOT_EFFORT",
    "HORON_AGENT_SWARM_MODEL",
    "HORON_AGENT_SWARM_MAX_CONCURRENT_AGENTS",
    "HORON_AGENT_SWARM_SPAWN_MODE",
    "HORON_AGENT_TOOL_PROFILE",
    "HORON_AGENT_TOOLS",
    "HORON_AGENT_TRUSTED_EXTERNAL_AUTH_SOURCES",
    "HORON_AGENT_UPDATE_CHANNEL",
    "HORON_AGENT_WEBSEARCH_ENGINE",
    "HORON_AGENT_WEBSEARCH_FALLBACK_ENGINES",
    "XDG_CONFIG_HOME",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigCacheFingerprint {
    path: Option<PathBuf>,
    modified: Option<SystemTime>,
    len: Option<u64>,
    env: Vec<(String, String)>,
}

impl ConfigCacheFingerprint {
    fn current() -> Self {
        let path = Config::path();
        let metadata = path.as_ref().and_then(|path| std::fs::metadata(path).ok());
        Self {
            path,
            modified: metadata.as_ref().and_then(|metadata| metadata.modified().ok()),
            len: metadata.as_ref().map(std::fs::Metadata::len),
            env: config_env_fingerprint(),
        }
    }
}

struct ConfigCache {
    config: &'static Config,
    fingerprint: ConfigCacheFingerprint,
    last_checked: Instant,
    force_reload: bool,
}

static CONFIG_CACHE: LazyLock<RwLock<ConfigCache>> = LazyLock::new(|| {
    let config = leak_config(Config::load_counted());
    // Fingerprint after the load so it describes the state the loaded config
    // was resolved from.
    let fingerprint = ConfigCacheFingerprint::current();
    // Seed the global context-limit cache from named provider configs on first
    // load so every codepath (TUI info widget, compaction budget, model
    // switching) sees user-configured `context_window` values from the start.
    // Read from the loaded config directly to avoid recursing into config(),
    // which would deadlock on the still-initializing CONFIG_CACHE.
    populate_context_limits_from_config_ref(config);
    RwLock::new(ConfigCache {
        config,
        fingerprint,
        last_checked: Instant::now(),
        force_reload: false,
    })
});

fn leak_config(config: Config) -> &'static Config {
    Box::leak(Box::new(config))
}

/// Seed the global context-limit cache from a config reference directly.
///
/// Used during CONFIG_CACHE initialization (where calling config() would
/// deadlock) and shares its logic with
/// `crate::provider::populate_context_limits_from_config`.
fn populate_context_limits_from_config_ref(cfg: &Config) {
    crate::provider::populate_context_limits_from_config_value(cfg);
}

/// Get the global config instance.
///
/// The returned reference is backed by a reloadable process cache. Calls check
/// the config file path/metadata and relevant environment overrides on a short
/// throttle, not every frame. When those inputs change, the next checked call
/// reloads config.toml and invalidates dependent auth/model caches. Older
/// references remain valid for the duration of any in-flight operation.
pub fn config() -> &'static Config {
    let now = Instant::now();
    if let Ok(cache) = CONFIG_CACHE.read()
        && !cache.force_reload
        && now.duration_since(cache.last_checked) < CONFIG_CACHE_CHECK_INTERVAL
    {
        return cache.config;
    }

    let mut reload_reason = None;
    let config = {
        let mut cache = CONFIG_CACHE.write().unwrap_or_else(|poisoned| poisoned.into_inner());

        let now = Instant::now();
        if !cache.force_reload && now.duration_since(cache.last_checked) < CONFIG_CACHE_CHECK_INTERVAL {
            return cache.config;
        }

        let fingerprint = ConfigCacheFingerprint::current();
        cache.last_checked = now;
        if cache.force_reload || cache.fingerprint != fingerprint {
            reload_reason = Some(change_report::describe_config_reload(
                cache.force_reload,
                &cache.fingerprint,
                &fingerprint,
            ));
            cache.config = leak_config(Config::load_counted());
            // Re-fingerprint after the load so the next check compares against
            // the state this reload actually observed.
            cache.fingerprint = ConfigCacheFingerprint::current();
            cache.force_reload = false;
        }
        cache.config
    };

    if let Some(reason) = reload_reason {
        crate::logging::info(&format!("CONFIG_RELOAD {}", reason));
        // A config reload can change config-derived system prompt sections
        // (feature toggles, sponsors, ...), which legitimately invalidates the
        // KV cache prefix of warm sessions. Document it so a subsequent
        // harness-attributed cache miss is surfaced with this cause instead of
        // as an unexplained prompt mutation.
        crate::cache_invalidation::record("config reload", &reason);
        notify_config_reloaded();
        // Re-seed the global context-limit cache so user edits to named
        // provider `context_window` values take effect without a restart.
        crate::provider::populate_context_limits_from_config();
    }

    config
}

fn config_env_fingerprint() -> Vec<(String, String)> {
    let mut values = std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.to_string_lossy().to_string();
            if CONFIG_ENV_KEYS.contains(&key.as_str()) {
                Some((key, value.to_string_lossy().to_string()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(&right.0));
    values
}

pub fn invalidate_config_cache() {
    let mut cache = CONFIG_CACHE.write().unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.force_reload = true;
    drop(cache);
    notify_config_reloaded();
}

fn notify_config_reloaded() {
    CONFIG_RELOAD_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for listener in CONFIG_RELOAD_LISTENERS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
    {
        listener();
    }
}

/// Monotonic counter bumped every time the config cache reloads.
///
/// Callers that snapshot config-derived state (e.g. the TUI's parsed
/// keybindings) can poll this cheaply and re-derive their snapshot when the
/// generation changes, giving instant hot-reload of config edits without a
/// restart.
static CONFIG_RELOAD_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Current config reload generation. Increments after every cache reload.
pub fn config_reload_generation() -> u64 {
    CONFIG_RELOAD_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Listeners invoked after the config cache reloads.
///
/// Config is a foundational module, so instead of reaching up into higher-level
/// subsystems (auth cache, event bus) on reload, those subsystems register a
/// reaction here at startup. This keeps config free of upward dependencies and
/// breaks the config -> auth / config -> bus cycle edges.
/// Type of a config reload listener callback.
type ConfigReloadListener = fn();

static CONFIG_RELOAD_LISTENERS: LazyLock<RwLock<Vec<ConfigReloadListener>>> = LazyLock::new(|| RwLock::new(Vec::new()));

/// Register a callback to run after the config cache reloads.
///
/// Callbacks must be cheap and non-blocking; they run on whichever thread
/// triggers the reload. Intended to be called once per subsystem during
/// process startup.
pub fn on_config_reloaded(listener: fn()) {
    CONFIG_RELOAD_LISTENERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(listener);
}

/// Main configuration struct
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Daemon behavior for autonomous wake requests.
    pub server: ServerConfig,

    /// Display/UI configuration
    pub display: DisplayConfig,

    /// Feature toggles
    pub features: FeatureConfig,

    /// Web search tool configuration
    pub websearch: WebSearchConfig,

    /// Built-in tool exposure configuration
    pub tools: ToolConfig,

    /// Agent Client Protocol adapter configuration
    pub acp: AcpConfig,

    /// Auth trust / consent configuration
    pub auth: AuthConfig,

    /// Provider configuration
    pub provider: ProviderConfig,

    /// Named provider profiles, keyed by profile name.
    pub providers: BTreeMap<String, NamedProviderConfig>,

    /// Named sets of session model and effort overrides.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub model_presets: BTreeMap<String, SessionModelPreset>,

    /// Agent-specific model defaults
    pub agents: AgentsConfig,

    /// Operator-written mid-stream rules, matched against the assistant's own
    /// streaming text while `[features] stream_rules` is on. An empty list is
    /// the default and is not written back to the config file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stream_rules: Vec<StreamRuleConfig>,

    /// Lifecycle hooks (external commands at turn/session/tool boundaries)
    pub hooks: HooksConfig,

    /// Ambient mode configuration
    pub ambient: AmbientConfig,

    /// Safety / notification configuration
    pub safety: SafetyConfig,

    /// WebSocket gateway configuration (for iOS/web clients)
    pub gateway: GatewayConfig,

    /// Limits for the in-process agent IRC mailbox bus.
    pub irc: IrcConfig,

    /// How messages that arrive while the agent is busy are delivered.
    pub messages: MessagesConfig,

    /// Advisor: a second, independent model that files narrow notes.
    pub advisor: AdvisorConfig,

    /// Compaction configuration
    pub compaction: CompactionConfig,

    /// Power-management configuration (prevent sleep while streaming)
    pub power: PowerConfig,

    /// Durability level for writes that have to survive a crash.
    pub storage: StorageConfig,

    /// Auto-review configuration
    pub autoreview: AutoReviewConfig,

    /// Auto-judge configuration
    pub autojudge: AutoJudgeConfig,

    /// Partner discovery configuration. Skipped when it matches the shipped
    /// default so saving config never bakes today's default into the file (see
    /// [`sponsors_is_default`]).
    #[serde(skip_serializing_if = "sponsors_is_default")]
    pub sponsors: SponsorsConfig,

    /// `[desktop.*]` tables owned by HoronAgent Desktop (voice, workspace,
    /// appearance, ...). The CLI never interprets them, but it must round-trip
    /// them verbatim so a CLI settings save never wipes Desktop preferences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop: Option<toml::Table>,

    /// `[skills]`: which collections beside horon-agent's own are read.
    pub skills: SkillsConfig,

    /// `[mcp]`: which MCP configs beside horon-agent's own are read.
    pub mcp: McpSourcesConfig,

    /// `[prewalk]`: a one-shot handoff to an execution model after planning.
    pub prewalk: PrewalkConfig,
}

/// Warned once per process, so a typo costs one line rather than one per write.
static STORAGE_SYNC_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Config {
    /// The durability level every original session write is confirmed with.
    ///
    /// `HORON_AGENT_STORAGE_SYNC` overrides `[storage] sync` from the file. An
    /// unrecognized value in either place falls back to `full` and is reported
    /// once per process instead of failing the config parse.
    pub fn storage_sync_mode(&self) -> horon_agent_platform::SyncMode {
        let (mode, warning) = self.storage_sync_resolution();
        if let Some(warning) = warning
            && !STORAGE_SYNC_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            crate::logging::warn(&warning);
        }
        mode
    }

    /// The resolved level and the warning an unrecognized value deserves.
    ///
    /// Split from [`Config::storage_sync_mode`] so the fallback can be tested
    /// without depending on the process-wide "already warned" state.
    pub(crate) fn storage_sync_resolution(&self) -> (horon_agent_platform::SyncMode, Option<String>) {
        let env = std::env::var_os("HORON_AGENT_STORAGE_SYNC");
        let env = env.as_deref().and_then(|value| value.to_str());
        let (mode, warning) = self.storage.resolve(env);
        let mode = match mode {
            StorageSyncMode::Full => horon_agent_platform::SyncMode::Full,
            StorageSyncMode::Fast => horon_agent_platform::SyncMode::Fast,
        };
        (mode, warning)
    }
}

/// Controls who owns autonomous wake execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WakeMode {
    /// The daemon starts idle turns and interrupts running turns itself.
    #[default]
    Internal,
    /// The daemon emits a wake request and leaves turn scheduling to its operator.
    External,
}

impl WakeMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "internal" => Some(Self::Internal),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}

/// Agent Client Protocol adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AcpConfig {
    /// Client compatibility profile: "standard" (default), "extended", or "full".
    pub profile: String,
    /// Tool profile to request when `horon-agent acp` starts a daemon itself.
    pub tool_profile: String,
    /// Answer ACP protocol version 2 to a client that asks for it. Off answers
    /// every client exactly as protocol version 1 does.
    #[serde(default = "acp_protocol_v2_default")]
    pub protocol_v2: bool,
}

fn acp_protocol_v2_default() -> bool {
    true
}

impl Default for AcpConfig {
    fn default() -> Self {
        Self {
            profile: "standard".to_string(),
            tool_profile: "acp".to_string(),
            protocol_v2: acp_protocol_v2_default(),
        }
    }
}

/// Controls how MCP server tools are exposed to the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpToolsMode {
    /// Never change the cached tool list for MCP changes. Providers with
    /// native deferred loading (Claude, OpenAI gpt-5.4+) get MCP tools as
    /// deferred definitions; others get the fixed `mcp_search`/`mcp_call`
    /// surface, with new tools described in the transcript.
    #[default]
    Auto,
    /// Always expose every MCP server tool as a top-level tool definition.
    /// Adding a server mid-session then invalidates the prompt cache.
    Eager,
    /// Same as `Auto` (kept for existing configs).
    Deferred,
}

impl McpToolsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Eager => "eager",
            Self::Deferred => "deferred",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "eager" => Some(Self::Eager),
            "deferred" => Some(Self::Deferred),
            _ => None,
        }
    }
}

/// How the `bash` tool provides stdio to the commands it runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BashPtyMode {
    /// Allocate a terminal only for commands that need one (default).
    #[default]
    Auto,
    /// Never allocate a terminal; always use pipes.
    Off,
    /// Always allocate a terminal. Platforms without a POSIX terminal return
    /// an explicit unsupported error instead of silently using pipes.
    Always,
}

impl BashPtyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Always => "always",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "off" => Some(Self::Off),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

/// Tool profile applied by a headless launch that selects no profile.
///
/// The `horon-agent` CLI applies this when neither `--tool-profile`, the
/// `HORON_AGENT_TOOL_PROFILE` environment variable, nor `[tools] profile` in
/// config.toml selects one, so a plain headless `serve`/`run` exposes the core
/// surface: [`CORE_TOOLS`], [`CORE_DISCOVERABLE_TOOLS`], and the dispatcher.
/// Any of those three still overrides it, so the set can be widened back to
/// `full`.
pub const CORE_TOOL_PROFILE: &str = "core";

/// Default lifetime of a scoped `path`/`folder` approval grant, in days.
///
/// A grant is the user answering "stop asking about this file" once. Seven days
/// keeps that answer useful for a work week without turning it into a permanent
/// policy the user has forgotten they gave; see `[tools] approval_grant_ttl_days`
/// for the knob and `0` to disable grants.
pub const DEFAULT_APPROVAL_GRANT_TTL_DAYS: u64 = 7;

/// Tool joined into a non-empty allow-list when
/// `ambient.approval_for_clientless_runs` is on.
///
/// It is deliberately not part of [`CORE_TOOLS`] or
/// [`CORE_DISCOVERABLE_TOOLS`]: this tool only becomes reachable when the
/// operator opts into clientless approvals.
pub const CLIENTLESS_APPROVAL_TOOL: &str = "request_permission";

/// Tool joined into a non-empty allow-list when `[tools] plan_mode` is on.
///
/// Like [`CLIENTLESS_APPROVAL_TOOL`] it stays outside the default surface, and
/// this tool only becomes reachable when the operator turns plan mode on.
pub const PLAN_TOOL: &str = "submit_plan";

/// Tool joined into a non-empty allow-list when `[tools] irc` is on.
///
/// Like [`PLAN_TOOL`] it stays outside [`CORE_TOOLS`] and
/// [`CORE_DISCOVERABLE_TOOLS`], because the switch is the only thing that
/// decides whether this tool is reachable at all. It only becomes reachable when
/// the operator turns the IRC bus on.
pub const IRC_TOOL: &str = "irc";

/// Tools joined into a non-empty allow-list when `[tools] checkpoint` is on.
///
/// Like [`IRC_TOOL`] they stay outside [`CORE_TOOLS`] and
/// [`CORE_DISCOVERABLE_TOOLS`], because the switch is the only thing that
/// decides whether the pair is reachable. Both tools share one switch because a
/// checkpoint nothing can rewind to, or a rewind with no checkpoint, is useless
/// on its own.
pub const CHECKPOINT_TOOLS: &[&str] = &["checkpoint", "rewind"];

/// The always-visible dispatcher for built-in tools that are not carried
/// eagerly.
///
/// Deliberately outside [`CORE_TOOLS`]: the `core` allow-list joins it and the
/// `acp` list never does. It is registered under `full` and `core`, and
/// [`ToolLoadTiers::tier`] pins its tier to [`LoadMode::Essential`] so the one
/// tool that reaches the deferred surface is never itself deferred. It only
/// loads a schema.
pub const TOOLBOX_TOOL: &str = "toolbox";

/// How one built-in tool enters the model's tool list.
///
/// `Discoverable` reuses the provider-native deferred loading that already backs
/// MCP tools: the definition travels in the request's tool catalog but stays
/// outside the cached system-prompt prefix, so it costs no schema bytes on the
/// cached prefix and adding or removing it never invalidates the prompt cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoadMode {
    /// Sent eagerly and callable without a discovery step. Every
    /// [`ESSENTIAL_TOOLS`] entry is essential in every profile, and so is any
    /// entry of the default surface outside a `core`-listed session.
    #[default]
    Essential,
    /// Sent deferred: the schema reaches the model once a tool reference names
    /// it. Providers without native deferred loading drop it instead.
    Discoverable,
    /// Not sent at all.
    Off,
}

impl LoadMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Essential => "essential",
            Self::Discoverable => "discoverable",
            Self::Off => "off",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "essential" => Some(Self::Essential),
            "discoverable" => Some(Self::Discoverable),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// Tools that are the discovery path for a deferred surface, so they must stay
/// callable without a prior reference. `mcp_search` loads MCP schemas from its
/// own results; deferring it would leave nothing able to reference it. The MCP
/// exposure pass may still defer `mcp_call` as the stable deferred placeholder.
const ALWAYS_EAGER_TOOLS: &[&str] = &["mcp_search", "mcp_call"];

/// Load tiers resolved for one session.
///
/// The session resolves this once, when it starts, so a configuration reload
/// cannot rewrite the tool list's deferred set mid-session and force a prompt
/// cache miss. [`ESSENTIAL_TOOLS`], [`ALWAYS_EAGER_TOOLS`] and [`TOOLBOX_TOOL`]
/// are always [`LoadMode::Essential`]. In a `core`-listed session every other
/// reachable tool — [`CORE_TOOLS`], [`CORE_DISCOVERABLE_TOOLS`], and the
/// switch-joined `irc`/`checkpoint`/`rewind` — is [`LoadMode::Discoverable`]:
/// that session advertises them by name and reaches them through the dispatcher
/// instead of paying for their schemas in the cached prefix. Every other tool
/// takes the session default unless [`ToolConfig::load_mode`] names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLoadTiers {
    /// Tier for an enabled tool that is neither core nor explicitly listed.
    default: LoadMode,
    overrides: BTreeMap<String, LoadMode>,
    /// Whether this session lists the core tools it does not send eagerly.
    ///
    /// The caller that knows the session's allow-list and profile resolves it,
    /// never this struct. The definition list and the dispatcher's frozen tiers
    /// both read this one flag.
    core_listed: bool,
}

impl ToolLoadTiers {
    /// Tier for one tool name. Unknown names never appear here, so an
    /// override for a tool this build does not have is inert.
    pub fn tier(&self, name: &str) -> LoadMode {
        let normalized = normalize_tool_name(name);
        if let Some(mode) = self.overrides.get(&normalized) {
            // `toolbox` is the discovery path for the deferred surface, so an
            // explicit entry may remove it but can never defer it: deferring
            // the dispatcher would leave nothing able to reference it.
            if normalized == TOOLBOX_TOOL && *mode == LoadMode::Discoverable {
                return LoadMode::Essential;
            }
            return *mode;
        }
        if ESSENTIAL_TOOLS.contains(&normalized.as_str())
            || ALWAYS_EAGER_TOOLS.contains(&normalized.as_str())
            || normalized == TOOLBOX_TOOL
        {
            return LoadMode::Essential;
        }
        if CORE_TOOLS.contains(&normalized.as_str()) {
            return if self.core_listed {
                LoadMode::Discoverable
            } else {
                LoadMode::Essential
            };
        }
        // The rest of the open surface is only ever reachable through the
        // dispatcher, so it is listed exactly where that dispatcher is: a
        // `core`-listed session. Outside one it keeps its session default, which
        // is what `full` already deferred before the surface opened.
        if self.core_listed && is_core_reachable(&normalized) {
            return LoadMode::Discoverable;
        }
        self.default
    }

    /// Whether this session lists the core tools it does not send eagerly.
    pub fn core_listed(&self) -> bool {
        self.core_listed
    }

    /// The operator's own tier for one tool, or `None` when the profile decides.
    ///
    /// Same normalization and the same `toolbox` coercion as [`Self::tier`], so
    /// a caller that suppresses a profile default can still honor the entries an
    /// operator wrote down.
    pub fn explicit_tier(&self, name: &str) -> Option<LoadMode> {
        let normalized = normalize_tool_name(name);
        let mode = *self.overrides.get(&normalized)?;
        if normalized == TOOLBOX_TOOL && mode == LoadMode::Discoverable {
            return Some(LoadMode::Essential);
        }
        Some(mode)
    }

    /// Tier applied to an enabled tool outside [`CORE_TOOLS`] with no override.
    pub fn default_tier(&self) -> LoadMode {
        self.default
    }

    /// Force one tool eager in this session, past the session default.
    ///
    /// A child-only tool installed after the session started (the `yield`
    /// submitter of a schema-spawned child) has no configuration entry, and
    /// under the `full` profile the default is `Discoverable`: the model would
    /// have to discover a tool whose whole purpose is to be called at
    /// completion. Only the in-memory session snapshot changes; the user's
    /// configuration is untouched.
    pub fn force_essential(&mut self, name: &str) {
        self.overrides.insert(normalize_tool_name(name), LoadMode::Essential);
    }

    /// Explicit per-tool entries, keyed by normalized tool name.
    pub fn overrides(&self) -> &BTreeMap<String, LoadMode> {
        &self.overrides
    }
}

/// Controls which tools are sent to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolConfig {
    /// Tool profile: "core" (headless default), "full", "acp",
    /// "minimal"/"lite", or "none".
    pub profile: String,
    /// Explicit allow-list. When set, only these tools are exposed.
    /// Use "*" or "all" to expose all tools without an allow-list.
    pub enabled: Vec<String>,
    /// Tools to remove after applying profile/enabled.
    pub disabled: Vec<String>,
    /// Disable all built-in tools unless `enabled` is provided.
    pub disable_base_tools: bool,
    /// MCP tool exposure mode: auto (default), eager, or deferred.
    pub mcp_tools: McpToolsMode,
    /// Ignored. `auto` used to switch MCP exposure at this token estimate,
    /// which changed the cached tool list mid-session. Kept so existing
    /// configs and the env override still parse.
    #[serde(
        alias = "mcp_tools_threshold",
        alias = "mcp_tools_auto_threshold",
        alias = "mcp_tools_auto_threshold_tokens"
    )]
    pub mcp_tools_token_threshold: usize,
    /// Codemode: expose a `codemode` tool that runs model-written
    /// JavaScript in an embedded QuickJS sandbox able to call every other
    /// tool (including MCP tools). On by default; `[tools] codemode = false`
    /// removes it.
    pub codemode: bool,
    /// `redo`: expose the tool that re-applies the mutation the last
    /// `undo` removed. On by default; `undo` is always available.
    pub redo: bool,
    /// On by default: append the `AGENTS.md` files in a read file's ancestor directories.
    pub nested_agents_md: bool,
    /// On by default: notice unchanged reads and diff changed whole-file re-reads. `full: true` always prints.
    pub read_dedup: bool,
    /// Maximum lines per default read page, clamped to 1–5,000.
    pub read_default_limit: usize,
    pub tool_result_aging: bool,
    pub output_column_cap: usize,
    pub artifact_output: ArtifactOutputConfig,
    /// On by default: refuse to edit, replace, delete, or overwrite a file that
    /// a generator produces. A generated file is regenerated from its source,
    /// so an edit to it is silently lost.
    pub block_auto_generated: bool,
    /// Terminal allocation for the `bash` tool: `auto` (default), `off`, or
    /// `always`. A call's own `pty` argument overrides it.
    pub bash_pty: BashPtyMode,
    /// `hashline_edit`: expose the tool that replaces line spans named
    /// by the anchors `read` emits with `hashline: true`. On by default.
    pub hashline_edit: bool,
    /// `ast_grep`/`ast_edit`: structural search and rewrite over
    /// tree-sitter grammars. On by default.
    pub ast_tools: bool,
    /// Opt-in outlines for large source files and Markdown documents.
    pub read_summary: bool,
    /// Opt-in fuzzy `edit`: accept an `old_string` that matches exactly one
    /// location after whitespace and quote normalization. Off by default.
    pub edit_fuzzy: bool,
    pub enforce_seen_line_guard: bool,
    /// `lsp`: expose the tool that asks the project's language server
    /// for diagnostics, definitions, references, hover text, and symbols. On by
    /// default; the server process starts on the first call and shuts down
    /// after its idle window.
    pub lsp: bool,
    /// Managed skills: let `skill_manage create`/`update` write skill
    /// files into `~/.horon/managed-skills/`. On by default;
    /// `[tools] managed_skills = false` stops a managed skill from being
    /// authored.
    pub managed_skills: bool,
    /// `context_notes`: expose the tool that keeps a small notes file
    /// per session and read it back into every request's dynamic prompt.
    /// On by default.
    pub context_notes: bool,
    /// How `ask` resolves a question when no client answers: `recommended`
    /// (default) picks it and says so; `error` refuses the call. An attached
    /// `supports_ask_requests` client is asked first, up to 300 s, then policy.
    pub ask_policy: AskPolicy,
    /// Per-tool load tiers that override the profile default, as
    /// `[tools.load_mode]` in config.toml. `off` removes an enabled tool from
    /// the list; an unknown name is inert.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub load_mode: BTreeMap<String, LoadMode>,
    /// Plan mode: while it is on, mutating and process-spawning calls are
    /// refused until a plan is approved. Off by default so nothing changes for
    /// an existing config.
    pub plan_mode: bool,
    /// `checkpoint`/`rewind` pair: let the model mark a point of the
    /// top-level conversation and later fold the exploration after it into one
    /// written report. On by default; `[tools] checkpoint = false` removes both
    /// tools, because a checkpoint nothing can rewind to, or a rewind with no
    /// checkpoint, is useless on its own.
    pub checkpoint: bool,
    /// `irc`: expose the tool that sends and receives messages between
    /// agents of the same daemon. On by default; `[tools] irc = false` or
    /// `HORON_AGENT_IRC=0` removes it.
    pub irc: bool,
    /// Which calls ask before they run. `yolo` auto-approves ordinary calls.
    /// See [`ApprovalMode`] for the modes and their restrictions.
    pub approval_mode: ApprovalMode,
    /// Start the next `bash` call where the last command ended. On by default.
    /// Set to `false` to pin every call to the session's working directory.
    pub bash_session_cwd: bool,
    /// Disable pagers, editors, and package-manager prompts for `bash` children.
    /// Defaults to enabled and includes `CI=true` for plain output.
    /// Set to `false` to inherit the parent environment unchanged.
    pub bash_non_interactive_env: bool,
    /// Load an approved direnv environment for local shell commands.
    pub bash_direnv: bool,
    /// Opt-in output minimizer for `bash`: a single, non-piped command that is
    /// a known build or package-manager invocation has its progress lines
    /// dropped before the result reaches the model, and the full output is
    /// saved as an artifact. Off by default because it rewrites what the model
    /// reads.
    pub bash_minimizer: bool,
    /// How `bash` reacts to shell idioms a dedicated tool handles better:
    /// `off` (default) runs everything as written, `nudge` runs the command
    /// and prefixes the output with the note, `block` refuses the call. See
    /// [`BashInterceptorMode`].
    pub bash_interceptor: BashInterceptorMode,
    /// Explicit approval rules for shell commands, checked before
    /// `approval_mode`. The first matching rule in config order wins; see
    /// [`BashPatternRule`]. Empty by default, which changes nothing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bash_patterns: Vec<BashPatternRule>,
    /// Refuse history-rewriting `git` commands (`stash`, `reset --hard`,
    /// branch moves) in a shared main checkout, where another session may
    /// hold uncommitted work. Off by default.
    pub git_guard: bool,
    /// Opt-in auxiliary completions: a separate small completion path for
    /// side tasks that must not disturb the main conversation.
    pub aux_completion: bool,
    /// `generate_image` on profiles with an `image` model role. On by default;
    /// it still needs the role, so a build without one never registers it.
    pub generate_image: bool,
    /// Python cells for `eval`. On by default; a cell asks for approval exactly
    /// like `bash` and only runs when Python 3.10 or later is on PATH.
    pub eval: bool,
    /// How credentials in outbound text are handled. `off` (default) leaves
    /// text untouched; see [`SecretsMode`].
    pub secrets_mode: SecretsMode,
    /// Command-risk classes that always require approval, whatever
    /// `approval_mode` says. The class names come from
    /// [`crate::approval::danger_class`]: `destructive-shell` for a `Confirm`
    /// verdict and `catastrophic-shell` for a `Catastrophic` one. Empty by
    /// default.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dual_confirm_classes: Vec<String>,
    /// Paths that always require approval, in every mode including `off`.
    /// Each entry is a glob matched against the path a call names, in its raw,
    /// resolved and canonical forms; `*` matches across separators, so a
    /// pattern that is too broad asks more often rather than fewer. Empty by
    /// default. An unparseable pattern restricts everything, because a rule
    /// that cannot be read must not silently stop asking.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restricted_paths: Vec<String>,
    /// How long a scoped path or folder grant stays usable, in days. `0`
    /// disables grants: none is written and none is consulted. Applies only to
    /// grants a client issued by scope; it has no effect on `approval_mode`.
    pub approval_grant_ttl_days: u64,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            profile: String::new(),
            enabled: Vec::new(),
            disabled: Vec::new(),
            disable_base_tools: false,
            mcp_tools: McpToolsMode::Auto,
            mcp_tools_token_threshold: 8_000,
            codemode: true,
            redo: true,
            nested_agents_md: true,
            read_dedup: true,
            read_default_limit: 5000,
            tool_result_aging: true,
            output_column_cap: 0,
            artifact_output: ArtifactOutputConfig::default(),
            block_auto_generated: true,
            bash_pty: BashPtyMode::Auto,
            hashline_edit: true,
            ast_tools: true,
            read_summary: false,
            edit_fuzzy: false,
            enforce_seen_line_guard: false,
            lsp: true,
            managed_skills: true,
            context_notes: true,
            ask_policy: AskPolicy::Recommended,
            load_mode: BTreeMap::new(),
            plan_mode: false,
            checkpoint: true,
            irc: true,
            approval_mode: ApprovalMode::default(),
            bash_session_cwd: true,
            bash_non_interactive_env: true,
            bash_direnv: false,
            bash_minimizer: false,
            bash_interceptor: BashInterceptorMode::Off,
            bash_patterns: Vec::new(),
            git_guard: false,
            aux_completion: false,
            generate_image: true,
            eval: true,
            secrets_mode: SecretsMode::Off,
            dual_confirm_classes: Vec::new(),
            restricted_paths: Vec::new(),
            approval_grant_ttl_days: DEFAULT_APPROVAL_GRANT_TTL_DAYS,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolSelection {
    pub allowed_tools: Option<HashSet<String>>,
    pub disabled_tools: HashSet<String>,
}

impl ToolConfig {
    pub fn selection(&self) -> ToolSelection {
        self.selection_with_ambient(&config().ambient)
    }

    pub fn allowed_tools(&self) -> Option<HashSet<String>> {
        self.selection().allowed_tools
    }

    /// Resolve this configuration's load tiers for a session whose allow-list
    /// is `allowed_tools`; `None` means the unrestricted `full` surface.
    ///
    /// Only two things turn on the discoverable tier: an explicit `full`
    /// profile, and `core_listed`, which the caller sets for the untouched
    /// default `core` profile whose dispatcher stays reachable. Every other
    /// case — `acp`, `minimal`, `none`, an explicit `enabled` list, and an
    /// unset profile — keeps every tool it lets through eager. A tool that
    /// survived a restricted allow-list was enabled deliberately, so the `acp`
    /// twelve-tool list and the ambient `request_permission` join must never
    /// shrink, and this resolver never adds a tool to any allow-list.
    pub fn load_tiers(&self, allowed_tools: Option<&HashSet<String>>, core_listed: bool) -> ToolLoadTiers {
        let overrides: BTreeMap<String, LoadMode> = self
            .load_mode
            .iter()
            .map(|(name, mode)| (normalize_tool_name(name), *mode))
            .filter(|(name, _)| !name.is_empty())
            .collect();
        let full = self.profile.trim().eq_ignore_ascii_case("full");
        let default = if full && allowed_tools.is_none() {
            LoadMode::Discoverable
        } else {
            LoadMode::Essential
        };
        ToolLoadTiers {
            default,
            overrides,
            core_listed,
        }
    }

    /// Whether this configuration leaves the `toolbox` dispatcher reachable.
    ///
    /// A `core`-listed session reaches the tools it does not send eagerly
    /// through that dispatcher, so an operator who removes it must get every
    /// core tool back eagerly instead of stranding them. Both ways of removing
    /// a tool count: `[tools.load_mode] toolbox = "off"` and a
    /// `[tools] disabled` entry.
    pub fn toolbox_reachable(&self) -> bool {
        let named_off = self
            .load_mode
            .iter()
            .any(|(name, mode)| *mode == LoadMode::Off && normalize_tool_name(name) == TOOLBOX_TOOL);
        let disabled = self
            .disabled
            .iter()
            .any(|name| normalize_tool_name(name) == TOOLBOX_TOOL);
        !named_off && !disabled
    }

    pub fn apply_to_allowed_set(&self, allowed: &mut HashSet<String>) {
        let selection = self.selection();
        if let Some(global_allowed) = selection.allowed_tools {
            allowed.retain(|name| global_allowed.contains(name));
        }
        for disabled in selection.disabled_tools {
            allowed.remove(&disabled);
        }
    }

    fn base_allowed_tools(&self) -> Option<HashSet<String>> {
        let (explicit, enables_all_tools) = self.normalized_enabled_tools();

        let profile = self.profile.trim().to_ascii_lowercase();
        if enables_all_tools {
            None
        } else if !explicit.is_empty() {
            Some(explicit)
        } else if self.disable_base_tools || matches!(profile.as_str(), "none" | "off" | "disabled") {
            Some(HashSet::new())
        } else if matches!(profile.as_str(), "acp") {
            Some(
                [
                    "bash",
                    "read",
                    "write",
                    "edit",
                    "replace",
                    "apply_patch",
                    "agentgrep",
                    "ls",
                    "batch",
                    "mcp",
                    // The editor renders the agent's plan from the todo list and
                    // answers the ask tool's questions in its own UI, so both
                    // belong to this profile and to no wider one.
                    "ask",
                    "todo",
                ]
                .into_iter()
                .map(|name| name.to_string())
                .collect(),
            )
        } else if matches!(profile.as_str(), "minimal" | "lite" | "small") {
            Some(
                [
                    "bash",
                    "read",
                    "write",
                    "edit",
                    "replace",
                    "apply_patch",
                    "agentgrep",
                    "ls",
                ]
                .into_iter()
                .map(|name| name.to_string())
                .collect(),
            )
        } else if profile == CORE_TOOL_PROFILE {
            // The default surface is open: every built-in this build can
            // construct joins the list, and the dispatcher comes along so the
            // tools this session lists instead of sending stay reachable.
            // `irc`, `checkpoint`, and `rewind` are not here: their switches
            // join them in `selection_with_ambient`, so turning a switch off
            // still removes its tool. Every other profile keeps its own list.
            Some(
                CORE_TOOLS
                    .iter()
                    .chain(CORE_DISCOVERABLE_TOOLS)
                    .copied()
                    .chain(std::iter::once(TOOLBOX_TOOL))
                    .map(str::to_string)
                    .collect(),
            )
        } else {
            None
        }
    }

    fn normalized_enabled_tools(&self) -> (HashSet<String>, bool) {
        let mut enabled = HashSet::new();
        let mut enables_all_tools = false;

        for name in &self.enabled {
            let normalized = normalize_tool_name(name);
            if normalized.is_empty() {
                continue;
            }
            if normalized == "*" || normalized.eq_ignore_ascii_case("all") {
                enables_all_tools = true;
            } else {
                enabled.insert(normalized);
            }
        }

        (enabled, enables_all_tools)
    }
}

fn normalize_tool_name(name: &str) -> String {
    let trimmed = name.trim().trim_matches('"');
    horon_agent_tool_types::resolve_tool_name(trimmed).to_string()
}

pub mod change_report;
mod config_file;
mod default_file;
mod env_overrides;
mod mcp;
mod messages;
mod server;
mod skills;
mod sponsors_default;
use sponsors_default::sponsors_is_default;
mod tool_selection;

pub use self::server::{DEFAULT_IDLE_HIBERNATION_SECS, ServerConfig};

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "config/checkpoint_switch_tests.rs"]
mod checkpoint_switch_tests;

#[cfg(test)]
#[path = "config/moved_tests.rs"]
mod moved_tests;

#[cfg(test)]
#[path = "config_channel_compat_tests.rs"]
mod channel_compat;

#[cfg(test)]
#[path = "config_tool_tiers_tests.rs"]
mod tool_tiers;

#[cfg(test)]
#[path = "config_display_tests.rs"]
mod display_tests;

#[cfg(test)]
#[path = "config/storage_tests.rs"]
mod storage_tests;

#[cfg(test)]
mod tool_profile_tests;
