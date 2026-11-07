//! Builders for the direct OpenAI-compatible provider runtimes.
//!
//! `horon-agent-provider-openai` is a normal dependency, so base builds every
//! runtime itself from the resolved `[providers.<name>]` profile or a built-in
//! `OpenAiCompatibleProfile`. There is no registry and no second runtime path.

use super::Provider;
use horon_agent_provider_openai::{ChatAuth, OpenAiCompatibleProvider};
use std::sync::{Arc, RwLock};

/// Everything the direct Chat Completions provider needs, after profile
/// resolution has produced an endpoint, credential and catalog.
pub struct DirectOpenAiCompatibleSpec {
    pub base_url: String,
    pub auth: ChatAuth,
    pub api_key: Option<String>,
    pub display_name: String,
    pub profile_id: String,
    pub default_model: String,
    pub models: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub extra_body: Option<serde_json::Value>,
}

/// Build the direct Chat Completions runtime for a resolved profile.
pub fn build_direct_openai_compatible_runtime(spec: DirectOpenAiCompatibleSpec) -> anyhow::Result<Arc<dyn Provider>> {
    let mut provider =
        OpenAiCompatibleProvider::new(spec.base_url, spec.auth, spec.api_key, spec.default_model, spec.models)?
            .with_display_name(spec.display_name)
            .with_profile_id(spec.profile_id);
    if !spec.headers.is_empty() {
        provider = provider.with_headers(spec.headers);
    }
    if let Some(extra_body) = spec.extra_body {
        provider = provider.with_extra_body(extra_body);
    }
    Ok(Arc::new(provider))
}

/// Build the runtime for one built-in `OpenAiCompatibleProfile`.
///
/// The profile's credential env var and env file are resolved here. A profile
/// that requires a key but finds none fails with an error naming the env var,
/// never the credential value.
pub fn build_openai_compatible_profile_runtime(
    profile: crate::provider_catalog::OpenAiCompatibleProfile,
) -> anyhow::Result<Arc<dyn Provider>> {
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    let api_key = crate::provider_catalog::load_api_key_from_env_or_config(&resolved.api_key_env, &resolved.env_file);

    let mut headers = Vec::new();
    let auth = if !resolved.requires_api_key {
        ChatAuth::None
    } else if crate::provider_catalog::api_base_is_anthropic(&resolved.api_base) {
        headers.push((
            "anthropic-version".to_string(),
            crate::provider_catalog::ANTHROPIC_VERSION_HEADER_VALUE.to_string(),
        ));
        ChatAuth::Header {
            header: "x-api-key".to_string(),
        }
    } else {
        ChatAuth::Bearer
    };

    if resolved.requires_api_key && api_key.is_none() {
        anyhow::bail!(
            "The {} OpenAI-compatible provider requires an API key, but {} is not set. \
             Set {} or add it (and the key) to {}.",
            resolved.display_name,
            resolved.api_key_env,
            resolved.api_key_env,
            resolved.env_file,
        );
    }

    let mut models = crate::provider_catalog::openai_compatible_profile_static_models(profile);
    let default_model = resolved
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToString::to_string)
        .or_else(|| models.first().cloned())
        .unwrap_or_default();
    if !default_model.is_empty() && !models.iter().any(|model| model == &default_model) {
        models.insert(0, default_model.clone());
    }

    build_direct_openai_compatible_runtime(DirectOpenAiCompatibleSpec {
        base_url: resolved.api_base,
        auth,
        api_key,
        display_name: resolved.display_name,
        profile_id: resolved.id,
        default_model,
        models,
        headers,
        extra_body: None,
    })
}

/// Build the runtime for a user-defined `[providers.<name>]` profile from
/// config.toml.
///
/// Only `type = "openai-compatible"` profiles are supported: the native
/// Anthropic wire format and the managed OpenRouter runtime are gone, so any
/// other type is a named configuration error rather than a silent fallback.
pub fn build_named_openai_compatible_runtime(
    profile_name: &str,
    profile: &crate::config::NamedProviderConfig,
) -> anyhow::Result<Arc<dyn Provider>> {
    if !matches!(
        profile.provider_type,
        crate::config::NamedProviderType::OpenAiCompatible
    ) {
        anyhow::bail!(
            "Provider profile '{}' has type {:?}; only \"openai-compatible\" profiles are supported. \
             Rewrite [providers.{}] with type = \"openai-compatible\".",
            profile_name,
            profile.provider_type,
            profile_name,
        );
    }

    let base_url = crate::provider_catalog::normalize_api_base(&profile.base_url).ok_or_else(|| {
        anyhow::anyhow!(
            "Provider profile '{}' has invalid base_url '{}'. Use an absolute https:// URL (or http://localhost).",
            profile_name,
            profile.base_url,
        )
    })?;

    let auth = match profile.auth {
        crate::config::NamedProviderAuth::Bearer => ChatAuth::Bearer,
        crate::config::NamedProviderAuth::Header => {
            let header = profile
                .auth_header
                .as_deref()
                .map(str::trim)
                .filter(|header| !header.is_empty())
                .unwrap_or("api-key");
            ChatAuth::Header {
                header: header.to_string(),
            }
        }
        crate::config::NamedProviderAuth::None => ChatAuth::None,
    };

    let api_key = named_profile_api_key(profile);
    let requires_api_key = profile
        .requires_api_key
        .unwrap_or_else(|| !crate::provider_catalog::api_base_uses_localhost(&base_url));
    if requires_api_key && !matches!(auth, ChatAuth::None) && api_key.is_none() {
        let env_name = profile
            .api_key_env
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        match env_name {
            Some(env_name) => anyhow::bail!(
                "Provider profile '{}' requires an API key, but {} is not set. \
                 Set {} or add {} to the profile's env_file.",
                profile_name,
                env_name,
                env_name,
                env_name,
            ),
            None => anyhow::bail!(
                "Provider profile '{}' requires an API key but no api_key_env is configured. \
                 Add api_key_env = \"YOUR_KEY_VAR\" to [providers.{}].",
                profile_name,
                profile_name,
            ),
        }
    }

    let mut models = profile
        .models
        .iter()
        .map(|model| model.id.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    let default_model = profile
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToString::to_string)
        .or_else(|| models.first().cloned())
        .unwrap_or_default();
    if !default_model.is_empty() && !models.iter().any(|model| model == &default_model) {
        models.insert(0, default_model.clone());
    }

    build_direct_openai_compatible_runtime(DirectOpenAiCompatibleSpec {
        base_url,
        auth,
        api_key,
        display_name: profile_name.to_string(),
        profile_id: profile_name.to_string(),
        default_model,
        models,
        headers: profile
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
        extra_body: profile.extra_body.clone(),
    })
}

fn named_profile_api_key(profile: &crate::config::NamedProviderConfig) -> Option<String> {
    if let Some(key) = profile.api_key.as_deref().map(str::trim).filter(|key| !key.is_empty()) {
        return Some(key.to_string());
    }
    let env_name = profile
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())?;
    if !crate::provider_catalog::is_safe_env_key_name(env_name) {
        return None;
    }
    let env_file = profile
        .env_file
        .as_deref()
        .map(str::trim)
        .filter(|file| !file.is_empty())
        .unwrap_or("providers.env");
    if !crate::provider_catalog::is_safe_env_file_name(env_file) {
        return None;
    }
    crate::provider_catalog::load_api_key_from_env_or_config(env_name, env_file)
}

/// Background OpenAI-compatible profile catalog refresh, registered by the
/// composition root. Base's route-building path calls this on a display cache
/// miss; when unregistered (e.g. minimal test binaries) the refresh is skipped
/// gracefully.
type ProfileCatalogRefresh =
    Arc<dyn Fn(crate::provider_catalog::OpenAiCompatibleProfile, &'static str) -> bool + Send + Sync>;

fn profile_catalog_refresh_slot() -> &'static RwLock<Option<ProfileCatalogRefresh>> {
    static SLOT: std::sync::LazyLock<RwLock<Option<ProfileCatalogRefresh>>> =
        std::sync::LazyLock::new(|| RwLock::new(None));
    &SLOT
}

/// Register the background profile-catalog refresh scheduler.
pub fn register_profile_catalog_refresh<F>(refresh: F)
where
    F: Fn(crate::provider_catalog::OpenAiCompatibleProfile, &'static str) -> bool + Send + Sync + 'static,
{
    *profile_catalog_refresh_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(refresh));
}

/// Schedule a background catalog refresh for a direct [OI]-compatible
/// profile. Returns false when no scheduler is registered or the refresh was
/// not started.
pub(crate) fn maybe_schedule_profile_catalog_refresh(
    profile: crate::provider_catalog::OpenAiCompatibleProfile,
    context: &'static str,
) -> bool {
    let refresh = profile_catalog_refresh_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    match refresh {
        Some(refresh) => refresh(profile, context),
        None => false,
    }
}
