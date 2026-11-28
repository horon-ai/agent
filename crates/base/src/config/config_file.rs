use super::*;
use crate::storage::horon_agent_dir;
use std::path::PathBuf;

#[path = "overlay.rs"]
mod overlay;

type TrustedExternalAuthKey = (
    Option<PathBuf>,
    Option<std::time::SystemTime>,
    Option<u64>,
    Option<String>,
);
type TrustedExternalAuthLists = std::sync::Arc<(Vec<String>, Vec<String>)>;
static TRUSTED_EXTERNAL_AUTH_MEMO: std::sync::Mutex<Option<(TrustedExternalAuthKey, TrustedExternalAuthLists)>> =
    std::sync::Mutex::new(None);

impl Config {
    /// Get the config file path
    pub fn path() -> Option<PathBuf> {
        horon_agent_dir().ok().map(|d| d.join("config.toml"))
    }

    /// Load config from file, with environment variable overrides
    pub fn load() -> Self {
        let mut config = Self::load_from_file().unwrap_or_default();
        config.apply_env_overrides();
        config
    }

    /// Load config from file, with environment variable overrides.
    ///
    /// Unlike [`Self::load`], this returns TOML/read errors to callers that need
    /// to distinguish a malformed config from an absent config.
    pub fn load_strict() -> anyhow::Result<Self> {
        let mut config = Self::load_from_file_strict()?.unwrap_or_default();
        config.apply_env_overrides();
        Ok(config)
    }

    /// Strict operator validation (`horon-agent config check`): parses the file,
    /// rejects unknown TOML keys across `Config`, `[hosting]`, `[harness_api]`,
    /// and `[providers.*]`, and verifies `[provider] default_provider` names a
    /// configured or built-in provider.
    pub fn check_strict() -> anyhow::Result<Self> {
        let path_display = Self::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "config.toml".to_string());
        let Some((mut config, document)) = Self::load_file_layer()? else {
            let mut config = Self::default();
            config.apply_env_overrides();
            Self::validate_default_provider(&config)?;
            return Ok(config);
        };
        let unknown = collect_unknown_config_keys(&document)?;
        if !unknown.is_empty() {
            anyhow::bail!("Unknown configuration key(s) in {path_display}: {}", unknown.join(", "));
        }
        config.apply_env_overrides();
        Self::validate_default_provider(&config)?;
        Ok(config)
    }

    fn validate_default_provider(config: &Self) -> anyhow::Result<()> {
        let Some(name) = config
            .provider
            .default_provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(());
        };
        let normalized = name.to_ascii_lowercase();
        let known = config.providers.contains_key(name)
            || config.providers.contains_key(&normalized)
            || crate::provider_catalog::openai_compatible_profile_by_id(&normalized).is_some()
            || matches!(normalized.as_str(), "openai-compatible" | "compat" | "custom");
        if !known {
            anyhow::bail!(
                "Unknown [provider] default_provider '{name}': configure [providers.{name}] or use a built-in OpenAI-compatible provider id"
            );
        }
        Ok(())
    }

    /// Load the on-disk config for a read-modify-write operation.
    ///
    /// Unlike [`Self::load`], this never converts a parse error into defaults.
    /// Saving those defaults would destroy the user's existing config. It also
    /// deliberately skips environment overrides so transient process settings
    /// are not baked into the file as a side effect of changing one preference.
    fn load_for_update() -> anyhow::Result<Self> {
        Ok(Self::load_from_file_strict()?.unwrap_or_default())
    }

    /// Load config from file only (no env overrides)
    fn load_from_file() -> Option<Self> {
        match Self::load_from_file_strict() {
            Ok(config) => config,
            Err(e) => {
                crate::logging::error(&format!("Failed to parse config file: {}", e));
                None
            }
        }
    }

    /// Load config from file only (no env overrides), preserving parse/read errors.
    pub(crate) fn load_from_file_strict() -> anyhow::Result<Option<Self>> {
        if overlay::active() {
            let Some(path) = Self::path() else {
                return Ok(None);
            };
            return overlay::load_config(&path).map(Some);
        }
        Ok(Self::read_file_config()?.map(|(_, _, config)| config))
    }

    /// The file-only config and the TOML document it came from.
    ///
    /// One read of the file yields both, so a caller that names the file as the
    /// source of a value is describing the same bytes the daemon applied. A file
    /// that does not parse is an error, and one that is absent is `None`; either
    /// way it supplied nothing.
    pub(crate) fn load_file_layer() -> anyhow::Result<Option<(Self, toml::Value)>> {
        if overlay::active() {
            let Some(path) = Self::path() else {
                return Ok(None);
            };
            return overlay::load(&path).map(Some);
        }
        let Some((path, content, config)) = Self::read_file_config()? else {
            return Ok(None);
        };
        // The same text the config came from, kept as a document so a caller can
        // ask which keys it declared. A failed second parse is impossible after
        // `Self` parsed, and still reported rather than swallowed.
        let document = toml::from_str::<toml::Value>(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file {}: {}", path.display(), e))?;
        Ok(Some((config, document)))
    }

    /// Read the file and parse it into the file-only config.
    ///
    /// The path and text come back with the config so a caller that also wants
    /// the document re-parses the same bytes instead of reading the file twice,
    /// and a caller that only wants the config never parses it a second time.
    fn read_file_config() -> anyhow::Result<Option<(PathBuf, String, Self)>> {
        let Some(path) = Self::path() else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read config file {}: {}", path.display(), e))?;
        let config = toml::from_str::<Self>(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file {}: {}", path.display(), e))?;
        config
            .prewalk
            .validate()
            .map_err(|e| anyhow::anyhow!("Invalid [prewalk] section in {}: {e}", path.display()))?;
        // An explicit discovery opt-out is authoritative. Older saves wrote the
        // default endpoint too, so table shape cannot establish user intent.
        Ok(Some((path, content, config)))
    }

    /// Save config to file
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("No config path"))?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Self::invalidate_cache();
        Ok(())
    }

    /// Mark the process-cached config as stale and notify dependent caches.
    pub fn invalidate_cache() {
        // Also drop the trust-list memo: its mtime key can miss two same-length
        // saves inside one filesystem timestamp tick.
        if let Ok(mut guard) = TRUSTED_EXTERNAL_AUTH_MEMO.lock() {
            *guard = None;
        }
        super::invalidate_config_cache();
    }

    /// Persist a worker-model default (`/agents global`). None clears it.
    pub fn set_agent_model_default(target: &str, model: Option<&str>) -> anyhow::Result<()> {
        let value = model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let mut cfg = Self::load_for_update()?;
        match target {
            "swarm" => cfg.agents.swarm_model = value,
            "review" => cfg.autoreview.model = value,
            "judge" => cfg.autojudge.model = value,
            "memory" => cfg.agents.memory_model = value,
            "ambient" => cfg.ambient.model = value,
            other => anyhow::bail!("Unknown agent model target: {other}"),
        }
        cfg.save()
    }

    /// Persist the Anthropic cache duration for clients and the shared daemon.
    pub fn set_anthropic_cache_ttl_1h(enabled: bool) -> anyhow::Result<()> {
        let mut cfg = Self::load_for_update()?;
        cfg.provider.anthropic_cache_ttl_1h = enabled;
        cfg.save()
    }

    /// Update just the default model and provider in the config file.
    /// This reloads, patches, and saves so it doesn't clobber other fields.
    pub fn set_default_model(model: Option<&str>, provider: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load_for_update()?;
        cfg.provider.default_model = model.map(|s| s.to_string());
        cfg.provider.default_provider = provider.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved default model: {}, provider: {}",
            model.unwrap_or("(none)"),
            provider.unwrap_or("(auto)")
        ));
        Ok(())
    }

    /// Update just the default provider in the config file.
    pub fn set_default_provider(provider: Option<&str>) -> anyhow::Result<()> {
        let cfg = Self::load_for_update()?;
        Self::set_default_model(cfg.provider.default_model.as_deref(), provider)
    }

    /// Update just the default model in the config file.
    pub fn set_default_model_only(model: Option<&str>) -> anyhow::Result<()> {
        let cfg = Self::load_for_update()?;
        Self::set_default_model(model, cfg.provider.default_provider.as_deref())
    }

    /// Update the persisted OpenAI reasoning effort preference.
    pub fn set_openai_reasoning_effort(value: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load_for_update()?;
        cfg.provider.openai_reasoning_effort = value.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved openai_reasoning_effort to config: {}",
            value.unwrap_or("(none)")
        ));
        Ok(())
    }

    /// Update the persisted Anthropic reasoning effort preference.
    pub fn set_anthropic_reasoning_effort(value: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load_for_update()?;
        cfg.provider.anthropic_reasoning_effort = value.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved anthropic_reasoning_effort to config: {}",
            value.unwrap_or("(none)")
        ));
        Ok(())
    }

    /// Update the persisted OpenAI transport preference.
    pub fn set_openai_transport(value: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load_for_update()?;
        cfg.provider.openai_transport = value.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved openai_transport to config: {}",
            value.unwrap_or("(none)")
        ));
        Ok(())
    }

    /// Update the persisted OpenAI service tier preference.
    pub fn set_openai_service_tier(value: Option<&str>) -> anyhow::Result<()> {
        let mut cfg = Self::load_for_update()?;
        cfg.provider.openai_service_tier = value.map(|s| s.to_string());
        cfg.save()?;
        crate::logging::info(&format!(
            "Saved openai_service_tier to config: {}",
            value.unwrap_or("(none)")
        ));
        Ok(())
    }

    /// One-time migration: flip a persisted legacy `swarm_spawn_mode =
    /// "visible"` to the current `"inline"` default.
    ///
    /// Historically `visible` was the default, and any full-config
    /// `Config::save()` (model switches, display toggles, ...) baked that
    /// then-default into the user's config.toml. When the default changed to
    /// `inline`, those users stayed pinned to `visible` forever. This rewrites
    /// exactly that one line (preserving the rest of the file byte-for-byte)
    /// and drops a marker so it runs at most once. A user who explicitly sets
    /// `visible` after the migration is never flipped again.
    ///
    /// Returns `true` when it rewrote the config. Best-effort: errors are
    /// logged and swallowed.
    pub fn migrate_legacy_swarm_spawn_mode_once() -> bool {
        let Ok(dir) = horon_agent_dir() else {
            return false;
        };
        let marker = dir.join("migrations").join("swarm-spawn-mode-inline");
        if marker.exists() {
            return false;
        }
        let write_marker = || {
            if let Some(parent) = marker.parent() {
                let _ = crate::storage::ensure_dir(parent);
            }
            let _ = horon_agent_platform::fs::write_owner_only(
                &marker,
                b"swarm_spawn_mode default migration: visible -> inline\n",
            );
        };

        let path = dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            // No config file (fresh install): nothing to migrate.
            write_marker();
            return false;
        };

        let mut changed = false;
        let migrated: Vec<String> = content
            .lines()
            .map(|line| {
                if changed {
                    return line.to_string();
                }
                let trimmed = line.trim_start();
                let Some(rest) = trimmed.strip_prefix("swarm_spawn_mode") else {
                    return line.to_string();
                };
                let Some(value) = rest.trim_start().strip_prefix('=') else {
                    return line.to_string();
                };
                let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
                if matches!(value, "visible" | "headed") {
                    changed = true;
                    let indent = &line[..line.len() - trimmed.len()];
                    format!("{indent}swarm_spawn_mode = \"inline\"")
                } else {
                    line.to_string()
                }
            })
            .collect();

        if !changed {
            write_marker();
            return false;
        }

        let mut new_content = migrated.join("\n");
        if content.ends_with('\n') {
            new_content.push('\n');
        }
        match std::fs::write(&path, new_content) {
            Ok(()) => {
                Self::invalidate_cache();
                write_marker();
                crate::logging::info("Migrated legacy swarm_spawn_mode \"visible\" to \"inline\" in config.toml");
                true
            }
            Err(err) => {
                crate::logging::warn(&format!("swarm_spawn_mode migration failed to write config: {err}"));
                false
            }
        }
    }

    fn normalize_external_auth_source_id(source_id: &str) -> String {
        source_id.trim().to_ascii_lowercase()
    }

    pub(crate) fn trusted_external_auth_path_entry(source_id: &str, path: &std::path::Path) -> anyhow::Result<String> {
        let source_id = Self::normalize_external_auth_source_id(source_id);
        if source_id.is_empty() {
            anyhow::bail!("External auth source id cannot be empty");
        }
        let canonical = crate::storage::validate_external_auth_file(path)?;
        Ok(format!(
            "{}|{}",
            source_id,
            canonical.to_string_lossy().to_ascii_lowercase()
        ))
    }

    pub fn external_auth_source_allowed(source_id: &str) -> bool {
        let source_id = Self::normalize_external_auth_source_id(source_id);
        if source_id.is_empty() {
            return false;
        }

        Self::trusted_external_auth_lists()
            .0
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&source_id))
    }

    pub fn external_auth_source_allowed_for_path(source_id: &str, path: &std::path::Path) -> bool {
        let Ok(entry) = Self::trusted_external_auth_path_entry(source_id, path) else {
            return false;
        };

        Self::trusted_external_auth_lists()
            .1
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&entry))
    }

    /// Fresh `(trusted_external_sources, trusted_external_source_paths)`.
    ///
    /// Trust checks must observe a just-saved decision immediately, so they
    /// cannot use the throttled [`config()`] snapshot. Re-parsing config.toml
    /// on every call was the alternative, and a single auth probe makes a dozen
    /// of these checks (~0.5ms of TOML parsing each, on the TUI's first frame).
    /// Memoize the two lists keyed on everything that can change them: the
    /// config path, the file's mtime and length, and the env override.
    fn trusted_external_auth_lists() -> TrustedExternalAuthLists {
        let path = Self::path();
        let metadata = path.as_ref().and_then(|path| std::fs::metadata(path).ok());
        let key: TrustedExternalAuthKey = (
            path,
            metadata.as_ref().and_then(|meta| meta.modified().ok()),
            metadata.as_ref().map(std::fs::Metadata::len),
            std::env::var("HORON_AGENT_TRUSTED_EXTERNAL_AUTH_SOURCES").ok(),
        );
        if let Ok(guard) = TRUSTED_EXTERNAL_AUTH_MEMO.lock()
            && let Some((cached_key, lists)) = guard.as_ref()
            && *cached_key == key
        {
            return lists.clone();
        }

        let cfg = Self::load();
        let lists = std::sync::Arc::new((
            cfg.auth.trusted_external_sources,
            cfg.auth.trusted_external_source_paths,
        ));
        if let Ok(mut guard) = TRUSTED_EXTERNAL_AUTH_MEMO.lock() {
            *guard = Some((key, lists.clone()));
        }
        lists
    }

    /// Startup-sensitive variant that uses the process-cached config snapshot.
    ///
    /// This avoids reloading config.toml repeatedly during cold-start probes.
    pub fn external_auth_source_allowed_for_path_cached(source_id: &str, path: &std::path::Path) -> bool {
        let Ok(entry) = Self::trusted_external_auth_path_entry(source_id, path) else {
            return false;
        };

        if config()
            .auth
            .trusted_external_source_paths
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&entry))
        {
            return true;
        }

        // The global config snapshot can be initialized before an auth flow saves
        // a new path-bound trust decision, or before tests switch HORON_HOME. Fall
        // back to a fresh read on cache misses so fast auth probes remain correct
        // without penalizing the common already-trusted path. The memo re-stats
        // config.toml, so it is as fresh as a full reload.
        Self::trusted_external_auth_lists()
            .1
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&entry))
    }

    pub fn allow_external_auth_source(source_id: &str) -> anyhow::Result<()> {
        let source_id = Self::normalize_external_auth_source_id(source_id);
        if source_id.is_empty() {
            anyhow::bail!("External auth source id cannot be empty");
        }

        let mut cfg = Self::load_for_update()?;
        if !cfg
            .auth
            .trusted_external_sources
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&source_id))
        {
            cfg.auth.trusted_external_sources.push(source_id.clone());
            cfg.auth.trusted_external_sources.sort();
            cfg.auth.trusted_external_sources.dedup();
            cfg.save()?;
        }

        crate::logging::info(&format!("Saved trusted external auth source to config: {}", source_id));
        Ok(())
    }

    pub fn allow_external_auth_source_for_path(source_id: &str, path: &std::path::Path) -> anyhow::Result<()> {
        let entry = Self::trusted_external_auth_path_entry(source_id, path)?;
        let mut cfg = Self::load_for_update()?;
        if !cfg
            .auth
            .trusted_external_source_paths
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(&entry))
        {
            cfg.auth.trusted_external_source_paths.push(entry.clone());
            cfg.auth.trusted_external_source_paths.sort();
            cfg.auth.trusted_external_source_paths.dedup();
            cfg.save()?;
        }
        crate::logging::info(&format!("Saved trusted external auth source path: {}", entry));
        Ok(())
    }

    pub fn revoke_external_auth_source_for_path(source_id: &str, path: &std::path::Path) -> anyhow::Result<()> {
        let entry = Self::trusted_external_auth_path_entry(source_id, path)?;
        let mut cfg = Self::load_for_update()?;
        let before = cfg.auth.trusted_external_source_paths.len();
        cfg.auth
            .trusted_external_source_paths
            .retain(|value| !value.trim().eq_ignore_ascii_case(&entry));
        if cfg.auth.trusted_external_source_paths.len() != before {
            cfg.save()?;
            crate::logging::info(&format!("Removed trusted external auth source path: {}", entry));
        }
        Ok(())
    }

    /// Remove a source-level (non-path) trust decision, e.g. for credentials
    /// that have no stable on-disk path (macOS Keychain items).
    pub fn revoke_external_auth_source(source_id: &str) -> anyhow::Result<()> {
        let source_id = Self::normalize_external_auth_source_id(source_id);
        if source_id.is_empty() {
            return Ok(());
        }
        let mut cfg = Self::load_for_update()?;
        let before = cfg.auth.trusted_external_sources.len();
        cfg.auth
            .trusted_external_sources
            .retain(|value| !value.trim().eq_ignore_ascii_case(&source_id));
        if cfg.auth.trusted_external_sources.len() != before {
            cfg.save()?;
            crate::logging::info(&format!("Removed trusted external auth source: {}", source_id));
        }
        Ok(())
    }
}

const NAMED_PROVIDER_KNOWN_KEYS: &[&str] = &[
    "type",
    "base_url",
    "api",
    "auth",
    "auth_header",
    "headers",
    "api_key_env",
    "api_key",
    "env_file",
    "default_model",
    "requires_api_key",
    "provider_routing",
    "model_catalog",
    "allow_provider_pinning",
    "models",
    "extra_body",
    "extra-body",
    "prompt_cache_key",
    "prompt-cache-key",
    "session_header",
    "session-header",
    "stream_first_event_timeout_secs",
    "stream_idle_timeout_secs",
    "max_in_flight_requests",
    "supports_reasoning_effort",
    "supports-reasoning-effort",
    "reasoning_effort",
    "disable_reasoning_heuristics",
    "disable-reasoning-heuristics",
    "fallback_models",
    "fallback-models",
    "roles",
];

fn collect_unknown_config_keys(document: &toml::Value) -> anyhow::Result<Vec<String>> {
    use serde::Deserialize;
    use std::cell::RefCell;

    let Some(mut root_table) = document.as_table().cloned() else {
        anyhow::bail!("Configuration root must be a TOML table");
    };
    let unknown = RefCell::new(Vec::new());

    if let Some(hosting_val) = root_table.remove("hosting") {
        let de = StrictTomlDeserializer {
            inner: hosting_val,
            path: vec!["hosting".to_string()],
            unknown: &unknown,
        };
        horon_agent_config_types::HostingSection::deserialize(de)?;
    }
    if let Some(harness_val) = root_table.remove("harness_api") {
        let de = StrictTomlDeserializer {
            inner: harness_val,
            path: vec!["harness_api".to_string()],
            unknown: &unknown,
        };
        horon_agent_config_types::HarnessApiConfig::deserialize(de)?;
    }

    if let Some(providers_table) = root_table.get("providers").and_then(toml::Value::as_table) {
        for (provider_name, provider_val) in providers_table {
            if let Some(table) = provider_val.as_table() {
                for key in table.keys() {
                    if !NAMED_PROVIDER_KNOWN_KEYS.contains(&key.as_str()) {
                        unknown.borrow_mut().push(format!("providers.{provider_name}.{key}"));
                    }
                }
                if let Some(roles_val) = table.get("roles") {
                    let de = StrictTomlDeserializer {
                        inner: roles_val.clone(),
                        path: vec!["providers".to_string(), provider_name.clone(), "roles".to_string()],
                        unknown: &unknown,
                    };
                    horon_agent_config_types::model_roles::ModelRoles::deserialize(de)?;
                }
                if let Some(models_val) = table.get("models") {
                    let de = StrictTomlDeserializer {
                        inner: models_val.clone(),
                        path: vec!["providers".to_string(), provider_name.clone(), "models".to_string()],
                        unknown: &unknown,
                    };
                    Vec::<NamedProviderModelConfig>::deserialize(de)?;
                }
            }
        }
    }

    let de = StrictTomlDeserializer {
        inner: toml::Value::Table(root_table),
        path: Vec::new(),
        unknown: &unknown,
    };
    Config::deserialize(de)?;

    let mut keys = unknown.into_inner();
    keys.sort();
    keys.dedup();
    Ok(keys)
}

struct StrictTomlDeserializer<'a> {
    inner: toml::Value,
    path: Vec<String>,
    unknown: &'a std::cell::RefCell<Vec<String>>,
}

struct StrictMapAccess<'a> {
    iter: toml::map::IntoIter,
    pending: Option<(String, toml::Value)>,
    path: Vec<String>,
    unknown: &'a std::cell::RefCell<Vec<String>>,
}

impl<'de, 'a> serde::de::MapAccess<'de> for StrictMapAccess<'a> {
    type Error = toml::de::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        match self.iter.next() {
            Some((key, val)) => {
                let decoded = seed.deserialize(toml::Value::String(key.clone()))?;
                self.pending = Some((key, val));
                Ok(Some(decoded))
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        let (key, val) = self
            .pending
            .take()
            .ok_or_else(|| serde::de::Error::custom("next_value_seed called without key"))?;
        let mut path = self.path.clone();
        path.push(key);
        seed.deserialize(StrictTomlDeserializer {
            inner: val,
            path,
            unknown: self.unknown,
        })
    }
}

struct StrictSeqAccess<'a> {
    iter: std::iter::Enumerate<std::vec::IntoIter<toml::Value>>,
    path: Vec<String>,
    unknown: &'a std::cell::RefCell<Vec<String>>,
}

impl<'de, 'a> serde::de::SeqAccess<'de> for StrictSeqAccess<'a> {
    type Error = toml::de::Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: serde::de::DeserializeSeed<'de>,
    {
        match self.iter.next() {
            Some((idx, val)) => {
                let mut path = self.path.clone();
                path.push(format!("[{idx}]"));
                seed.deserialize(StrictTomlDeserializer {
                    inner: val,
                    path,
                    unknown: self.unknown,
                })
                .map(Some)
            }
            None => Ok(None),
        }
    }
}

impl<'de, 'a> serde::Deserializer<'de> for StrictTomlDeserializer<'a> {
    type Error = toml::de::Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_any(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        if !self.path.is_empty() {
            self.unknown.borrow_mut().push(self.path.join("."));
        }
        visitor.visit_unit()
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_bool(visitor)
    }

    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_i8(visitor)
    }

    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_i16(visitor)
    }

    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_i32(visitor)
    }

    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_i64(visitor)
    }

    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_u8(visitor)
    }

    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_u16(visitor)
    }

    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_u32(visitor)
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_u64(visitor)
    }

    fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_f32(visitor)
    }

    fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_f64(visitor)
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_char(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_str(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_string(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_bytes(visitor)
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_byte_buf(visitor)
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_some(self)
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_unit(visitor)
    }

    fn deserialize_unit_struct<V>(self, name: &'static str, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V>(self, _name: &'static str, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        match self.inner {
            toml::Value::Array(items) => visitor.visit_seq(StrictSeqAccess {
                iter: items.into_iter().enumerate(),
                path: self.path,
                unknown: self.unknown,
            }),
            other => other.deserialize_seq(visitor),
        }
    }

    fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        match self.inner {
            toml::Value::Array(items) => visitor.visit_seq(StrictSeqAccess {
                iter: items.into_iter().enumerate(),
                path: self.path,
                unknown: self.unknown,
            }),
            other => other.deserialize_tuple(len, visitor),
        }
    }

    fn deserialize_tuple_struct<V>(self, name: &'static str, len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        match self.inner {
            toml::Value::Array(items) => visitor.visit_seq(StrictSeqAccess {
                iter: items.into_iter().enumerate(),
                path: self.path,
                unknown: self.unknown,
            }),
            other => other.deserialize_tuple_struct(name, len, visitor),
        }
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        match self.inner {
            toml::Value::Table(table) => visitor.visit_map(StrictMapAccess {
                iter: table.into_iter(),
                pending: None,
                path: self.path,
                unknown: self.unknown,
            }),
            other => other.deserialize_map(visitor),
        }
    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        match self.inner {
            toml::Value::Table(table) => visitor.visit_map(StrictMapAccess {
                iter: table.into_iter(),
                pending: None,
                path: self.path,
                unknown: self.unknown,
            }),
            other => other.deserialize_struct(name, fields, visitor),
        }
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_enum(name, variants, visitor)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.inner.deserialize_identifier(visitor)
    }
}

#[cfg(test)]
mod issue_1056_tests {
    use super::Config;

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            crate::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => crate::env::set_var(self.key, value),
                None => crate::env::remove_var(self.key),
            }
            Config::invalidate_cache();
        }
    }

    #[test]
    fn effort_update_preserves_profile_with_capitalized_bearer_auth() {
        let _lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HORON_HOME", home.path());
        let path = home.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[provider]
openai_reasoning_effort = "low"

[providers.mistral]
type = "openai-compatible"
base_url = "https://api.mistral.ai/v1"
auth = "Bearer"
api_key_env = "MISTRAL_API_KEY"
disable_reasoning_heuristics = true

[[providers.mistral.models]]
id = "mistral-medium-latest"
reasoning = true
reasoning_effort = "max"
"#,
        )
        .unwrap();

        Config::set_openai_reasoning_effort(Some("high")).unwrap();

        let saved = std::fs::read_to_string(path).unwrap();
        assert!(saved.contains("[providers.mistral]"));
        assert!(saved.contains("mistral-medium-latest"));
        let parsed = Config::load_strict().unwrap();
        assert_eq!(parsed.provider.openai_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(parsed.providers["mistral"].models.len(), 1);
    }

    #[test]
    fn effort_update_refuses_to_overwrite_malformed_config() {
        let _lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HORON_HOME", home.path());
        let path = home.path().join("config.toml");
        let original = "[providers.broken]\nauth = \"invalid-auth-mode\"\n";
        std::fs::write(&path, original).unwrap();

        let error =
            Config::set_openai_reasoning_effort(Some("high")).expect_err("a malformed config must block mutation");

        assert!(error.to_string().contains("Failed to parse config file"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}

#[cfg(test)]
#[path = "strict_tests.rs"]
mod strict_tests;
