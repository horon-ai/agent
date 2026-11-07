use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

pub const OPENCODE_AUTH_JSON_SOURCE_ID: &str = "opencode_auth_json";
pub const PI_AUTH_JSON_SOURCE_ID: &str = "pi_auth_json";
pub const OPENCLAW_AUTH_JSON_SOURCE_ID: &str = "openclaw_auth_json";
pub const HERMES_AUTH_JSON_SOURCE_ID: &str = "hermes_auth_json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalAuthSource {
    OpenCode,
    Pi,
    OpenClaw,
    Hermes,
}

impl ExternalAuthSource {
    pub fn source_id(self) -> &'static str {
        match self {
            Self::OpenCode => OPENCODE_AUTH_JSON_SOURCE_ID,
            Self::Pi => PI_AUTH_JSON_SOURCE_ID,
            Self::OpenClaw => OPENCLAW_AUTH_JSON_SOURCE_ID,
            Self::Hermes => HERMES_AUTH_JSON_SOURCE_ID,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::OpenCode => "OpenCode auth.json",
            Self::Pi => "pi auth.json",
            Self::OpenClaw => "OpenClaw auth.json",
            Self::Hermes => "Hermes auth.json",
        }
    }

    pub fn path(self) -> Result<PathBuf> {
        match self {
            Self::OpenCode => crate::storage::user_home_path(".local/share/opencode/auth.json"),
            Self::Pi => crate::storage::user_home_path(".pi/agent/auth.json"),
            Self::OpenClaw => openclaw_auth_path(),
            Self::Hermes => crate::storage::user_home_path(".hermes/auth.json"),
        }
    }
}

/// Resolve OpenClaw's credential file. OpenClaw has moved its auth store over
/// time, so probe the known locations and return the first that exists:
///
///   1. `~/.openclaw/agent/auth.json` - the original pi-fork layout.
///   2. `~/.openclaw/agents/<agentId>/agent/auth-profiles.json` - the current
///      per-agent profile store (`main` is checked first, then any agent).
///   3. `~/.openclaw/agents/<agentId>/agent/auth.json` - per-agent legacy file.
///   4. `~/.openclaw/credentials/oauth.json` - legacy import-only OAuth file.
///
/// Falls back to the pi-fork path when nothing exists (so consent bookkeeping
/// always has a stable path to record).
fn openclaw_auth_path() -> Result<PathBuf> {
    let legacy = crate::storage::user_home_path(".openclaw/agent/auth.json")?;
    if legacy.is_file() {
        return Ok(legacy);
    }

    let agents_root = crate::storage::user_home_path(".openclaw/agents")?;
    let agent_dirs = || -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        // Check the default agent first so multi-agent installs resolve
        // deterministically to the main store.
        dirs.push(agents_root.join("main"));
        if let Ok(entries) = std::fs::read_dir(&agents_root) {
            let mut rest: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "main"))
                .collect();
            rest.sort();
            dirs.extend(rest);
        }
        dirs
    };
    for dir in agent_dirs() {
        let profiles = dir.join("agent/auth-profiles.json");
        if profiles.is_file() {
            return Ok(profiles);
        }
        let auth = dir.join("agent/auth.json");
        if auth.is_file() {
            return Ok(auth);
        }
    }

    let credentials = crate::storage::user_home_path(".openclaw/credentials/oauth.json")?;
    if credentials.is_file() {
        return Ok(credentials);
    }

    Ok(legacy)
}

const SOURCES: [ExternalAuthSource; 4] = [
    ExternalAuthSource::OpenCode,
    ExternalAuthSource::Pi,
    ExternalAuthSource::OpenClaw,
    ExternalAuthSource::Hermes,
];

pub fn trust_external_auth_source(source: ExternalAuthSource) -> Result<()> {
    crate::config::Config::allow_external_auth_source_for_path(source.source_id(), &source.path()?)?;
    super::AuthStatus::invalidate_cache();
    Ok(())
}

pub fn has_any_unconsented_external_auth() -> bool {
    SOURCES
        .into_iter()
        .filter(|source| source.path().map(|path| path.exists()).unwrap_or(false))
        .any(|source| !source_allowed(source) && source_has_supported_auth(source))
}

pub fn unconsented_sources() -> Vec<ExternalAuthSource> {
    detected_sources()
        .into_iter()
        .filter(|source| !source_allowed(*source))
        .collect()
}

/// Every shared auth file on this machine that holds a login horon-agent can use,
/// whether or not horon-agent already trusts it.
pub fn detected_sources() -> Vec<ExternalAuthSource> {
    SOURCES
        .into_iter()
        .filter(|source| source.path().map(|path| path.exists()).unwrap_or(false))
        .filter(|source| source_has_supported_auth(*source))
        .collect()
}

pub fn source_provider_labels(source: ExternalAuthSource) -> Vec<&'static str> {
    if source_contains_supported_api_key(source).unwrap_or(false) {
        vec!["OpenAI-compatible API-key providers"]
    } else {
        Vec::new()
    }
}

pub fn preferred_unconsented_api_key_source() -> Option<ExternalAuthSource> {
    SOURCES
        .into_iter()
        .filter(|source| source.path().map(|path| path.exists()).unwrap_or(false))
        .find(|source| !source_allowed(*source) && source_contains_supported_api_key(*source).unwrap_or(false))
}

pub fn preferred_unconsented_api_key_source_for_env(env_key: &str) -> Option<ExternalAuthSource> {
    SOURCES
        .into_iter()
        .filter(|source| source.path().map(|path| path.exists()).unwrap_or(false))
        .find(|source| {
            !source_allowed(*source)
                && load_api_key_from_source(*source, env_key)
                    .map(|key| !key.trim().is_empty())
                    .unwrap_or(false)
        })
}

pub fn load_api_key_for_env(env_key: &str) -> Option<String> {
    for source in SOURCES {
        if !source_allowed(source) {
            continue;
        }
        if let Some(key) = load_api_key_from_source(source, env_key) {
            return Some(key);
        }
    }
    None
}

pub fn source_allowed(source: ExternalAuthSource) -> bool {
    let Ok(path) = source.path() else {
        return false;
    };

    if crate::config::Config::external_auth_source_allowed_for_path(source.source_id(), &path) {
        return true;
    }

    match source {
        ExternalAuthSource::OpenCode => {
            crate::config::Config::external_auth_source_allowed_for_path(source.source_id(), &path)
        }
        ExternalAuthSource::Pi | ExternalAuthSource::OpenClaw | ExternalAuthSource::Hermes => false,
    }
}

fn source_has_supported_auth(source: ExternalAuthSource) -> bool {
    source_contains_supported_api_key(source).unwrap_or(false)
}

fn source_contains_supported_api_key(source: ExternalAuthSource) -> Result<bool> {
    let auth = load_auth_map(source)?;
    Ok(auth.values().any(|entry| extract_api_key(source, entry).is_some()))
}

fn load_api_key_from_source(source: ExternalAuthSource, env_key: &str) -> Option<String> {
    let auth = load_auth_map(source).ok()?;
    for &provider_key in provider_keys_for_env(env_key) {
        if let Some(entry) = auth.get(provider_key)
            && let Some(key) = extract_api_key(source, entry)
            && !key.trim().is_empty()
        {
            return Some(key);
        }
    }
    None
}

fn load_auth_map(source: ExternalAuthSource) -> Result<HashMap<String, Value>> {
    let path = crate::storage::validate_external_auth_file(&source.path()?)?;
    let raw = std::fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    let value: Value = serde_json::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))?;
    match source {
        ExternalAuthSource::OpenCode | ExternalAuthSource::Pi => {
            // Flat `provider -> credential` maps.
            Ok(value
                .as_object()
                .map(|object| object.iter().map(|(key, value)| (key.clone(), value.clone())).collect())
                .unwrap_or_default())
        }
        ExternalAuthSource::OpenClaw => Ok(flatten_openclaw_auth_store(&value)),
        ExternalAuthSource::Hermes => Ok(flatten_hermes_auth_store(&value)),
    }
}

/// OpenClaw historically used the flat pi-style `provider -> credential` map,
/// but its current store is `auth-profiles.json`:
///
/// ```json
/// {
///   "version": 1,
///   "profiles": {
///     "openai:default": { "type": "oauth", "provider": "openai", "access": ..., "refresh": ..., "expires": ... },
///     "openrouter:default": { "type": "api_key", "provider": "openrouter", "key": "..." }
///   }
/// }
/// ```
///
/// Normalize both shapes to a flat `provider -> credential` map. Profile
/// entries keep the pi-style credential fields, so the shared extractors work
/// unchanged. When several profiles exist for one provider, the `<provider>:default`
/// profile wins; otherwise the first seen is kept.
fn flatten_openclaw_auth_store(value: &Value) -> HashMap<String, Value> {
    let Some(object) = value.as_object() else {
        return HashMap::new();
    };
    let Some(profiles) = object.get("profiles").and_then(Value::as_object) else {
        // Legacy flat map.
        return object.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
    };

    let mut map: HashMap<String, Value> = HashMap::new();
    for (profile_id, entry) in profiles {
        let provider = entry
            .get("provider")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| profile_id.split(':').next().map(ToOwned::to_owned));
        let Some(provider) = provider else { continue };
        let is_default = profile_id.ends_with(":default") || !profile_id.contains(':');
        if is_default || !map.contains_key(&provider) {
            map.insert(provider, entry.clone());
        }
    }
    map
}

/// Hermes persists credentials in a nested store:
///
/// ```json
/// {
///   "version": 1,
///   "active_provider": "anthropic",
///   "credential_pool": { "<provider>": [ { "auth_type": ..., "access_token": ... }, ... ] },
///   "providers": { "<provider>": { ...singleton state... } }
/// }
/// ```
///
/// Normalize it to a flat `provider -> representative credential` map so the
/// shared extraction logic can treat it like the other sources. The highest
/// priority (first) credential-pool entry wins; legacy `providers.<id>` blocks
/// are used only when the pool has no entry for that provider.
fn flatten_hermes_auth_store(value: &Value) -> HashMap<String, Value> {
    let mut map: HashMap<String, Value> = HashMap::new();

    if let Some(providers) = value.get("providers").and_then(Value::as_object) {
        for (provider, state) in providers {
            map.insert(provider.clone(), state.clone());
        }
    }

    if let Some(pool) = value.get("credential_pool").and_then(Value::as_object) {
        for (provider, entries) in pool {
            if let Some(first) = entries.as_array().and_then(|entries| entries.first()) {
                // Credential-pool entries are authoritative over legacy blocks.
                map.insert(provider.clone(), first.clone());
            }
        }
    }

    map
}

fn extract_api_key(source: ExternalAuthSource, entry: &Value) -> Option<String> {
    let object = entry.as_object()?;
    match source {
        ExternalAuthSource::OpenCode => {
            if object.get("type")?.as_str()? != "api" {
                return None;
            }
            object
                .get("key")?
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        }
        ExternalAuthSource::Pi | ExternalAuthSource::OpenClaw => {
            if object.get("type")?.as_str()? != "api_key" {
                return None;
            }
            resolve_pi_api_key_value(object.get("key")?.as_str()?)
        }
        ExternalAuthSource::Hermes => {
            // Hermes stores API keys as credential-pool entries whose
            // `auth_type` is `api_key` and whose literal key lives in
            // `access_token`.
            if object.get("auth_type")?.as_str()? != "api_key" {
                return None;
            }
            object
                .get("access_token")?
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        }
    }
}

fn resolve_pi_api_key_value(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('!') {
        return None;
    }

    if let Ok(value) = std::env::var(raw) {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }

    Some(raw.to_string())
}

fn provider_keys_for_env(env_key: &str) -> &'static [&'static str] {
    match env_key {
        "ANTHROPIC_API_KEY" => &["anthropic", "claude"],
        "AZURE_OPENAI_API_KEY" => &["azure-openai-responses", "azure", "azure-openai"],
        "OPENAI_API_KEY" => &["openai", "openai-api"],
        "GEMINI_API_KEY" => &["google", "gemini"],
        "MISTRAL_API_KEY" => &["mistral"],
        "GROQ_API_KEY" => &["groq"],
        "CEREBRAS_API_KEY" => &["cerebras"],
        "BELVEDIR_API_KEY" => &["belvedir"],
        "XAI_API_KEY" => &["xai"],
        "OPENROUTER_API_KEY" => &["openrouter"],
        "CONIFER_API_KEY" => &["conifer"],
        "AI_GATEWAY_API_KEY" => &["vercel-ai-gateway"],
        "ZHIPU_API_KEY" | "ZAI_API_KEY" => &["zai"],
        "OPENCODE_API_KEY" => &["opencode"],
        "OPENCODE_GO_API_KEY" => &["opencode-go", "opencode"],
        "HF_TOKEN" => &["huggingface"],
        "KIMI_API_KEY" => &["kimi-coding", "kimi", "moonshot"],
        "MINIMAX_API_KEY" => &["minimax"],
        "MINIMAX_CN_API_KEY" => &["minimax-cn"],
        "NEBIUS_API_KEY" => &["nebius"],
        "SCALEWAY_API_KEY" => &["scaleway"],
        "STACKIT_API_KEY" => &["stackit"],
        "TOGETHER_API_KEY" => &["togetherai", "together-ai", "together"],
        "DEEPINFRA_API_KEY" => &["deepinfra"],
        "FIREWORKS_API_KEY" => &["fireworks"],
        "NOVITA_API_KEY" => &["novita", "novita-ai", "novita.ai"],
        "CHUTES_API_KEY" => &["chutes"],
        "BASETEN_API_KEY" => &["baseten"],
        "CORTECS_API_KEY" => &["cortecs"],
        "COMTEGRA_API_KEY" => &["comtegra", "cgc"],
        "DEEPSEEK_API_KEY" => &["deepseek"],
        "FIRMWARE_API_KEY" => &["firmware"],
        "MOONSHOT_API_KEY" => &["moonshotai", "moonshot"],
        "PERPLEXITY_API_KEY" => &["perplexity"],
        "BAILIAN_CODING_PLAN_API_KEY" => &["alibaba-coding-plan", "bailian"],
        _ => &[],
    }
}

#[cfg(test)]
#[path = "external_tests.rs"]
mod external_tests;
