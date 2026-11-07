use super::*;

impl Config {
    /// Apply environment variable overrides
    #[expect(
        clippy::collapsible_if,
        reason = "Environment override parsing is intentionally explicit and grouped by config area"
    )]
    pub(crate) fn apply_env_overrides(&mut self) {
        // Server/operator behavior
        if let Ok(v) = std::env::var("HORON_AGENT_WAKE_MODE")
            && let Some(parsed) = WakeMode::parse(&v)
        {
            self.server.wake_mode = parsed;
        }
        // Zero is meaningful here: it disables the sweep, so unlike the stream
        // idle timeout it must not be filtered out as "no value".
        if let Ok(v) = std::env::var("HORON_AGENT_IDLE_HIBERNATION_SECS")
            && let Ok(parsed) = v.trim().parse::<u64>()
        {
            self.server.idle_hibernation_secs = parsed;
        }

        // Tools
        if let Ok(v) = std::env::var("HORON_AGENT_TOOL_PROFILE") {
            self.tools.profile = v;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_TOOLS") {
            self.tools.enabled = parse_env_list(&v);
        }
        if let Ok(v) = std::env::var("HORON_AGENT_DISABLED_TOOLS") {
            self.tools.disabled = parse_env_list(&v);
        }
        if let Ok(v) = std::env::var("HORON_AGENT_DISABLE_BASE_TOOLS")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.tools.disable_base_tools = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MCP_TOOLS")
            && let Some(mode) = crate::config::McpToolsMode::parse(&v)
        {
            self.tools.mcp_tools = mode;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MCP_TOOLS_TOKEN_THRESHOLD")
            && let Ok(parsed) = v.trim().parse::<usize>()
        {
            self.tools.mcp_tools_token_threshold = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_CODEMODE")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.tools.codemode = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_NESTED_AGENTS_MD")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.tools.nested_agents_md = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_READ_DEDUP")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.tools.read_dedup = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_TOOL_RESULT_AGING")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.tools.tool_result_aging = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_IRC")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.tools.irc = parsed;
        }

        // ACP adapter
        if let Ok(v) = std::env::var("HORON_AGENT_ACP_PROFILE") {
            let trimmed = v.trim().to_ascii_lowercase();
            if matches!(trimmed.as_str(), "standard" | "extended" | "full") {
                self.acp.profile = trimmed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_ACP_TOOL_PROFILE") {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                self.acp.tool_profile = trimmed.to_string();
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_ACP_PROTOCOL_V2")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.acp.protocol_v2 = parsed;
        }

        // Display
        if let Ok(v) = std::env::var("HORON_AGENT_DEBUG_SOCKET")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.display.debug_socket = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_NO_EMOJI")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.display.emoji = !parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_SHOW_THINKING")
            && let Some(parsed) = parse_env_bool(&v)
        {
            self.display.show_thinking = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_REASONING_DISPLAY")
            && let Some(mode) = crate::config::ReasoningDisplayMode::parse(&v)
        {
            self.display.set_reasoning_display(mode);
        }
        // A front-end default, applied only when the user has not made an
        // explicit choice. The desktop uses this so its transcript shows live
        // thinking out of the box without silently overriding config.
        if !self.display.has_explicit_reasoning_display()
            && let Ok(v) = std::env::var("HORON_AGENT_DEFAULT_REASONING_DISPLAY")
            && let Some(mode) = crate::config::ReasoningDisplayMode::parse(&v)
        {
            self.display.set_reasoning_display(mode);
        }

        // Features
        if let Ok(v) = std::env::var("HORON_AGENT_MEMORY_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.features.memory = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_SWARM_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.features.swarm = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_CHECK_UPDATES") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.features.check_updates = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_AUTO_POKE") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.features.auto_poke = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MESSAGE_TIMESTAMPS") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.features.message_timestamps = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.features.persist_memory_injections = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_KV_CACHE_MISS_NOTICES") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.features.kv_cache_miss_notices = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_UPDATE_CHANNEL")
            && let Some(channel) = UpdateChannel::parse(&v)
        {
            self.features.update_channel = channel;
        }

        // Agents (spawned helper sessions)
        if let Ok(v) = std::env::var("HORON_AGENT_SWARM_MODEL") {
            let trimmed = v.trim();
            self.agents.swarm_model = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        if let Ok(v) = std::env::var("HORON_AGENT_SWARM_EFFORT") {
            let trimmed = v.trim();
            self.agents.swarm_effort = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        for (key, target) in [
            ("HORON_AGENT_SWARM_ROOT_EFFORT", &mut self.agents.swarm_root_effort),
            (
                "HORON_AGENT_SWARM_DEEP_ROOT_EFFORT",
                &mut self.agents.swarm_deep_root_effort,
            ),
        ] {
            if let Ok(value) = std::env::var(key) {
                let value = value.trim();
                *target = (!value.is_empty()).then(|| value.to_string());
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_SWARM_SPAWN_MODE") {
            if let Some(parsed) = SwarmSpawnMode::parse(&v) {
                self.agents.swarm_spawn_mode = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_SWARM_MAX_CONCURRENT_AGENTS") {
            if let Ok(parsed) = v.trim().parse::<usize>() {
                self.agents.swarm_max_concurrent_agents = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_SWARM_STALL_AFTER_SECS") {
            if let Ok(parsed) = v.trim().parse::<u64>() {
                self.agents.swarm_stall_after_secs = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_SWARM_STALL_NUDGE") {
            match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => self.agents.swarm_stall_nudge = true,
                "0" | "false" | "no" | "off" => self.agents.swarm_stall_nudge = false,
                _ => {}
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MEMORY_JEV_PROVIDER") {
            self.agents.memory_jev_provider = v.trim().to_ascii_lowercase();
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MEMORY_MODEL") {
            let trimmed = v.trim();
            self.agents.memory_model = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MEMORY_SIDECAR_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.agents.memory_sidecar_enabled = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MEMORY_EMBEDDING_BACKEND") {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                self.agents.memory_embedding_backend = trimmed.to_string();
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MEMORY_EMBEDDING_MODEL") {
            let trimmed = v.trim();
            self.agents.memory_embedding_model = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MEMORY_EMBEDDING_BASE_URL") {
            let trimmed = v.trim();
            self.agents.memory_embedding_base_url = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MEMORY_EMBEDDING_DIM") {
            if let Ok(parsed) = v.trim().parse::<usize>() {
                self.agents.memory_embedding_dim = Some(parsed);
            }
        }

        // Lifecycle hooks. Empty env values disable config-file hooks.
        fn hook_env_override(slot: &mut Option<HookCommands>, key: &str) {
            if let Ok(v) = std::env::var(key) {
                let trimmed = v.trim();
                *slot = if trimmed.is_empty() {
                    None
                } else if trimmed.starts_with('[') {
                    #[derive(serde::Deserialize)]
                    struct HookOverride {
                        commands: HookCommands,
                    }

                    toml::from_str::<HookOverride>(&format!("commands = {trimmed}"))
                        .map(|parsed| parsed.commands)
                        .ok()
                        .or_else(|| Some(HookCommands::one(trimmed)))
                } else {
                    Some(HookCommands::one(trimmed))
                };
            }
        }
        hook_env_override(&mut self.hooks.turn_start, "HORON_AGENT_HOOK_TURN_START");
        hook_env_override(&mut self.hooks.turn_end, "HORON_AGENT_HOOK_TURN_END");
        hook_env_override(&mut self.hooks.session_start, "HORON_AGENT_HOOK_SESSION_START");
        hook_env_override(&mut self.hooks.session_end, "HORON_AGENT_HOOK_SESSION_END");
        hook_env_override(&mut self.hooks.pre_tool, "HORON_AGENT_HOOK_PRE_TOOL");
        hook_env_override(
            &mut self.hooks.pre_tool_transform,
            "HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM",
        );
        if let Ok(v) = std::env::var("HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM_TIMEOUT_MS")
            && let Ok(parsed) = v.trim().parse::<u64>()
        {
            self.hooks.pre_tool_transform_timeout_ms = parsed;
        }
        hook_env_override(&mut self.hooks.post_tool, "HORON_AGENT_HOOK_POST_TOOL");
        hook_env_override(
            &mut self.hooks.post_tool_feedback,
            "HORON_AGENT_HOOK_POST_TOOL_FEEDBACK",
        );
        if let Ok(v) = std::env::var("HORON_AGENT_HOOK_POST_TOOL_FEEDBACK_TIMEOUT_MS")
            && let Ok(parsed) = v.trim().parse::<u64>()
        {
            self.hooks.post_tool_feedback_timeout_ms = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS") {
            if let Ok(parsed) = v.trim().parse::<u64>() {
                self.hooks.pre_tool_timeout_ms = parsed;
            }
        }

        // Web search
        if let Ok(v) = std::env::var("HORON_AGENT_WEBSEARCH_ENGINE")
            && let Some(engine) = WebSearchEngine::parse(&v)
        {
            self.websearch.engine = engine;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_WEBSEARCH_FALLBACK_ENGINES") {
            let engines = parse_env_list(&v)
                .into_iter()
                .filter_map(|item| WebSearchEngine::parse(&item))
                .collect::<Vec<_>>();
            if !engines.is_empty() {
                self.websearch.fallback_engines = engines;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_BING_API_KEY")
            && !v.trim().is_empty()
        {
            self.websearch.bing_api_key = Some(v);
        }
        if let Ok(v) = std::env::var("HORON_AGENT_BING_API_KEY_ENV")
            && !v.trim().is_empty()
        {
            self.websearch.bing_api_key_env = v;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_BING_MARKET")
            && !v.trim().is_empty()
        {
            self.websearch.bing_market = v;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_SEARXNG_URL")
            && !v.trim().is_empty()
        {
            self.websearch.searxng_url = Some(v);
        }

        if let Ok(v) = std::env::var("HORON_AGENT_TRUSTED_EXTERNAL_AUTH_SOURCES") {
            let mut source_ids = Vec::new();
            let mut source_paths = Vec::new();
            for value in parse_env_list(&v) {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.contains('|') {
                    source_paths.push(trimmed.to_ascii_lowercase());
                } else {
                    source_ids.push(trimmed.to_ascii_lowercase());
                }
            }
            self.auth.trusted_external_sources = source_ids;
            self.auth.trusted_external_source_paths = source_paths;
        }

        // Autoreview
        if let Ok(v) = std::env::var("HORON_AGENT_AUTOREVIEW_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.autoreview.enabled = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_AUTOREVIEW_MODEL") {
            let trimmed = v.trim();
            self.autoreview.model = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }

        // Autojudge
        if let Ok(v) = std::env::var("HORON_AGENT_AUTOJUDGE_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.autojudge.enabled = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_AUTOJUDGE_MODEL") {
            let trimmed = v.trim();
            self.autojudge.model = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }

        // Ambient
        if let Ok(v) = std::env::var("HORON_AGENT_AMBIENT_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.ambient.enabled = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_AMBIENT_PROVIDER") {
            self.ambient.provider = Some(v);
        }
        if let Ok(v) = std::env::var("HORON_AGENT_AMBIENT_MODEL") {
            self.ambient.model = Some(v);
        }
        if let Ok(v) = std::env::var("HORON_AGENT_AMBIENT_MIN_INTERVAL") {
            if let Ok(parsed) = v.trim().parse::<u32>() {
                self.ambient.min_interval_minutes = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_AMBIENT_MAX_INTERVAL") {
            if let Ok(parsed) = v.trim().parse::<u32>() {
                self.ambient.max_interval_minutes = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_AMBIENT_PROACTIVE") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.ambient.proactive_work = parsed;
            }
        }

        // Safety / notifications
        if let Ok(v) = std::env::var("HORON_AGENT_NTFY_TOPIC") {
            self.safety.ntfy_topic = Some(v);
        }
        if let Ok(v) = std::env::var("HORON_AGENT_NTFY_SERVER") {
            self.safety.ntfy_server = v;
        }

        // Gateway (iOS/web)
        if let Ok(v) = std::env::var("HORON_AGENT_GATEWAY_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.gateway.enabled = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_GATEWAY_PORT") {
            if let Ok(parsed) = v.trim().parse::<u16>() {
                self.gateway.port = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_GATEWAY_BIND_ADDR") {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                self.gateway.bind_addr = trimmed.to_string();
            }
        }

        // Power management
        if let Ok(v) = std::env::var("HORON_AGENT_PREVENT_SLEEP_WHILE_STREAMING") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.power.prevent_sleep_while_streaming = parsed;
            }
        }

        // Provider
        if let Ok(v) = std::env::var("HORON_AGENT_MODEL") {
            self.provider.default_model = Some(v);
        }
        if let Ok(v) = std::env::var("HORON_AGENT_PROVIDER") {
            let trimmed = v.trim().to_lowercase();
            if !trimmed.is_empty() {
                self.provider.default_provider = Some(trimmed);
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_OPENAI_REASONING_EFFORT") {
            let trimmed = v.trim().to_string();
            if !trimmed.is_empty() {
                self.provider.openai_reasoning_effort = Some(trimmed);
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_ANTHROPIC_REASONING_EFFORT") {
            let trimmed = v.trim().to_string();
            if !trimmed.is_empty() {
                self.provider.anthropic_reasoning_effort = Some(trimmed);
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_OPENAI_TRANSPORT") {
            let trimmed = v.trim().to_string();
            if !trimmed.is_empty() {
                self.provider.openai_transport = Some(trimmed);
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_OPENAI_SERVICE_TIER") {
            let trimmed = v.trim().to_string();
            if !trimmed.is_empty() {
                self.provider.openai_service_tier = Some(trimmed);
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_OPENAI_NATIVE_COMPACTION_MODE") {
            let trimmed = v.trim().to_ascii_lowercase();
            if !trimmed.is_empty() {
                self.provider.openai_native_compaction_mode = trimmed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_OPENAI_NATIVE_COMPACTION_THRESHOLD_TOKENS") {
            if let Ok(parsed) = v.trim().parse::<usize>() {
                if parsed > 0 {
                    self.provider.openai_native_compaction_threshold_tokens = parsed;
                }
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_COMPACTION_SUMMARY_MODEL") {
            let trimmed = v.trim().to_string();
            self.compaction.summary_model = (!trimmed.is_empty()).then_some(trimmed);
        }
        if let Ok(v) = std::env::var("HORON_AGENT_PRESERVE_REASONING_CONTEXT") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.provider.preserve_reasoning_context = parsed;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_CROSS_PROVIDER_FAILOVER") {
            if let Some(mode) = CrossProviderFailoverMode::parse(&v) {
                self.provider.cross_provider_failover = mode;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_SAME_PROVIDER_ACCOUNT_FAILOVER") {
            if let Some(enabled) = parse_env_bool(&v) {
                self.provider.same_provider_account_failover = enabled;
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_STREAM_IDLE_TIMEOUT_SECS") {
            if let Ok(parsed) = v.trim().parse::<u64>() {
                if parsed > 0 {
                    self.provider.stream_idle_timeout_secs = parsed;
                }
            }
        }
        if let Ok(v) = std::env::var("HORON_AGENT_MAX_RETRIES")
            && let Ok(parsed) = v.trim().parse::<u32>()
            && parsed > 0
        {
            self.provider.max_retries = parsed;
        }
        if let Ok(v) = std::env::var("HORON_AGENT_RETRY_BACKOFF_CAP_SECS")
            && let Ok(parsed) = v.trim().parse::<u64>()
            && parsed > 0
        {
            self.provider.retry_backoff_cap_secs = parsed;
        }

        // Explicit environment overrides win, but never export config values:
        // self-written env would mask subsequent config edits/removals.
        if let Ok(v) = std::env::var("HORON_AGENT_GEMINI_FORCE_OAUTH") {
            self.provider.gemini_force_oauth = parse_env_bool(&v).unwrap_or(false);
        }

        if let Ok(v) = std::env::var("GOOGLE_CLOUD_PROJECT").or_else(|_| std::env::var("GOOGLE_CLOUD_PROJECT_ID")) {
            let v = v.trim();
            if !v.is_empty() {
                self.provider.gemini_project = Some(v.to_string());
            }
        }
    }
}

fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_env_list(raw: &str) -> Vec<String> {
    raw.split([',', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Number of times the process config cache resolved `Config`. Test-only
/// observability for the startup-path tests.
#[cfg(any(test, feature = "test-support"))]
static CONFIG_LOAD_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Config {
    /// Load `Config` and count the load when test observability is compiled.
    ///
    /// Every population and reload of the process config cache goes through
    /// here, so the counter is the number of `Config::load` calls the process
    /// made for its global config.
    pub(crate) fn load_counted() -> Self {
        #[cfg(any(test, feature = "test-support"))]
        CONFIG_LOAD_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self::load()
    }

    /// Number of global `Config::load` calls so far. Test-only observability.
    #[cfg(any(test, feature = "test-support"))]
    pub fn config_load_count() -> u64 {
        CONFIG_LOAD_COUNT.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Apply a tool profile to the loaded config without re-reading the file.
    ///
    /// A headless launch that selects no profile applies
    /// [`CORE_TOOL_PROFILE`] by setting `HORON_AGENT_TOOL_PROFILE`.
    /// Invalidating the cache afterwards made the next `config()` parse the
    /// file a second time, so this patches the loaded value instead and keeps
    /// the launch at one `Config::load`.
    ///
    /// The stored environment fingerprint is refreshed from the current
    /// environment: the caller sets the variable right before this call, and an
    /// unsynchronized fingerprint would make the next `config()` treat it as an
    /// external change and reload anyway.
    pub fn set_loaded_tool_profile(profile: &str) {
        let mut cache = super::CONFIG_CACHE
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.config.tools.profile != profile {
            // The global config is handed out as `&'static Config`; publish a
            // new leaked copy instead of mutating a reference readers hold.
            let mut patched = cache.config.clone();
            patched.tools.profile = profile.to_string();
            cache.config = super::leak_config(patched);
        }
        cache.fingerprint = super::ConfigCacheFingerprint::current();
        cache.last_checked = std::time::Instant::now();
    }
}

#[cfg(test)]
mod idle_hibernation_env_tests {
    use super::*;
    use std::ffi::OsString;

    /// Isolated home plus a restored environment, so a real `HORON_AGENT_HOME`
    /// or a leftover override cannot leak into the parse.
    struct EnvCase {
        _lock: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
        previous: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvCase {
        fn new() -> Self {
            let lock = crate::storage::lock_test_env();
            let dir = tempfile::tempdir().expect("temp home");
            let keys = ["HORON_AGENT_HOME", "HORON_AGENT_IDLE_HIBERNATION_SECS"];
            let previous = keys
                .into_iter()
                .map(|key| {
                    let value = std::env::var_os(key);
                    (key, value)
                })
                .collect();
            crate::env::set_var("HORON_AGENT_HOME", dir.path());
            crate::env::remove_var("HORON_AGENT_IDLE_HIBERNATION_SECS");
            Self {
                _lock: lock,
                _dir: dir,
                previous,
            }
        }
    }

    impl Drop for EnvCase {
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..) {
                match value {
                    Some(value) => crate::env::set_var(key, value),
                    None => crate::env::remove_var(key),
                }
            }
            Config::invalidate_cache();
        }
    }

    fn load_with(value: Option<&str>) -> u64 {
        let _case = EnvCase::new();
        if let Some(value) = value {
            crate::env::set_var("HORON_AGENT_IDLE_HIBERNATION_SECS", value);
        }
        Config::load_strict()
            .expect("an isolated load must succeed")
            .server
            .idle_hibernation_secs
    }

    #[test]
    fn idle_hibernation_env_overrides_and_keeps_zero() {
        assert_eq!(load_with(None), 1800, "absent env keeps the default");
        assert_eq!(load_with(Some("1800")), 1800);
        // Zero disables the sweep, so the usual `parsed > 0` filter must not
        // swallow it the way the stream idle timeout does.
        assert_eq!(load_with(Some(" 0 ")), 0, "zero is a real value and trims");
        // A negative value cannot be a `u64` window; it must not fall back to
        // zero, which would silently disable a feature the operator wanted.
        assert_eq!(load_with(Some("-1")), 1800, "a negative value is ignored");
        assert_eq!(load_with(Some("invalid")), 1800, "junk is ignored");
    }
}
