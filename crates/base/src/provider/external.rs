//! Builders for the direct OpenAI-compatible provider runtimes.
//!
//! `horon-agent-provider-openai` is a normal dependency, so base builds every
//! runtime itself from the resolved `[providers.<name>]` profile or a built-in
//! `OpenAiCompatibleProfile`. There is no registry and no second runtime path.

use super::Provider;
use horon_agent_provider_openai::{ChatAuth, OpenAiCompatibleProvider, WireProtocol};
use std::collections::HashMap;
use std::num::NonZeroU16;
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::Semaphore;

/// All runtimes and image calls for one configured profile share this queue.
/// The limit is pinned on first use and a daemon restart applies a new limit.
fn request_slots(profile_name: &str, limit: Option<NonZeroU16>) -> Option<Arc<Semaphore>> {
    let limit = usize::from(limit?.get());
    static SLOTS: LazyLock<Mutex<HashMap<String, Arc<Semaphore>>>> = LazyLock::new(Mutex::default);
    let mut profiles = SLOTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(slots) = profiles.get(profile_name) {
        return Some(Arc::clone(slots));
    }
    let slots = Arc::new(Semaphore::new(limit));
    profiles.insert(profile_name.to_owned(), Arc::clone(&slots));
    Some(slots)
}

/// Everything one direct provider runtime needs, after profile resolution has
/// produced an endpoint, credential, wire and catalog.
pub struct DirectOpenAiCompatibleSpec {
    pub base_url: String,
    pub auth: ChatAuth,
    /// Request and response shape this endpoint speaks.
    pub wire: WireProtocol,
    pub api_key: Option<String>,
    pub display_name: String,
    pub profile_id: String,
    pub default_model: String,
    pub models: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub extra_body: Option<serde_json::Value>,
    /// Explicit `prompt_cache_key` policy; `None` keeps the per-host default.
    pub prompt_cache_key: Option<bool>,
    /// Configured legacy `prompt_cache_retention` override. The model-support
    /// policy is applied per request, so a model switch re-evaluates it.
    pub prompt_cache_retention: Option<String>,
    /// Reasoning effort each model declares in `[[providers.<name>.models]]
    /// reasoning_effort`. The runtime applies the active model's level as soon
    /// as it is selected, so a configured effort reaches the first request
    /// instead of waiting for an explicit `/effort`.
    pub model_reasoning_efforts: std::collections::BTreeMap<String, String>,
    pub stream_first_event_timeout: Option<Duration>,
    pub stream_idle_timeout: Option<Duration>,
    pub request_slots: Option<Arc<Semaphore>>,
}

/// Build the direct provider runtime for a resolved profile.
pub fn build_direct_openai_compatible_runtime(spec: DirectOpenAiCompatibleSpec) -> anyhow::Result<Arc<dyn Provider>> {
    Ok(Arc::new(build_direct_runtime(spec)?))
}

fn build_direct_runtime(spec: DirectOpenAiCompatibleSpec) -> anyhow::Result<OpenAiCompatibleProvider> {
    let configured_retention = spec.prompt_cache_retention;
    let model_reasoning_efforts = spec.model_reasoning_efforts;
    let mut provider =
        OpenAiCompatibleProvider::new(spec.base_url, spec.auth, spec.api_key, spec.default_model, spec.models)?
            .with_wire(spec.wire)
            .with_model_reasoning_efforts(model_reasoning_efforts)
            .with_display_name(spec.display_name)
            .with_profile_id(spec.profile_id)
            .with_prompt_cache_key_policy(spec.prompt_cache_key)
            .with_stream_timeouts(spec.stream_first_event_timeout, spec.stream_idle_timeout)
            .with_request_slots(spec.request_slots)
            .with_prompt_cache_retention_resolver(Arc::new(move |model: &str| {
                super::openai::effective_prompt_cache_retention(model, configured_retention.as_deref())
                    .map(str::to_owned)
            }));
    if !spec.headers.is_empty() {
        provider = provider.with_headers(spec.headers);
    }
    if let Some(extra_body) = spec.extra_body {
        provider = provider.with_extra_body(extra_body);
    }
    Ok(provider)
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
        wire: WireProtocol::ChatCompletions,
        api_key,
        display_name: resolved.display_name,
        profile_id: resolved.id,
        default_model,
        models,
        headers,
        extra_body: None,
        prompt_cache_key: None,
        prompt_cache_retention: super::openai::prompt_cache_retention_from_env(),
        // A built-in catalog profile names no per-model effort.
        model_reasoning_efforts: std::collections::BTreeMap::new(),
        stream_first_event_timeout: None,
        stream_idle_timeout: None,
        request_slots: None,
    })
}

/// Build the runtime for a user-defined `[providers.<name>]` profile from
/// config.toml.
///
/// Each supported `type` names one wire; the profile's endpoint, credential and
/// catalog resolve once and the wire decides only the request and response
/// shape. The managed OpenRouter runtime is gone, so `open-router` is a named
/// configuration error that lists the supported values rather than a silent
/// fallback.
pub fn build_named_openai_compatible_runtime(
    profile_name: &str,
    profile: &crate::config::NamedProviderConfig,
) -> anyhow::Result<Arc<dyn Provider>> {
    Ok(Arc::new(build_named_runtime(profile_name, profile)?))
}

/// `None` indicates a wire that does not use the OpenAI model-list format.
pub async fn fetch_named_openai_models(
    profile_name: &str,
    profile: &crate::config::NamedProviderConfig,
) -> anyhow::Result<Option<Vec<String>>> {
    if !matches!(
        named_provider_wire(&profile.provider_type),
        Some(WireProtocol::ChatCompletions | WireProtocol::Responses)
    ) {
        return Ok(None);
    }
    let runtime = build_named_runtime(profile_name, profile)
        .map_err(|_| anyhow::anyhow!("profile endpoint or authentication is invalid"))?;
    runtime.fetch_models().await.map(Some)
}

fn build_named_runtime(
    profile_name: &str,
    profile: &crate::config::NamedProviderConfig,
) -> anyhow::Result<OpenAiCompatibleProvider> {
    let Some(wire) = named_provider_wire(&profile.provider_type) else {
        anyhow::bail!(
            "Provider profile '{}' has type \"{}\", which is not supported. \
             Rewrite [providers.{}] with type = \"openai-compatible\", \"openai-responses\", \
             \"anthropic-compatible\" or \"gemini-compatible\".",
            profile_name,
            named_provider_type_name(&profile.provider_type),
            profile_name,
        );
    };

    let base_url = crate::provider_catalog::normalize_api_base(&profile.base_url).ok_or_else(|| {
        anyhow::anyhow!(
            "Provider profile '{}' has invalid base_url '{}'. Use an absolute https:// URL (or http://localhost).",
            profile_name,
            profile.base_url,
        )
    })?;

    if matches!(wire, WireProtocol::GeminiGenerateContent) && base_url.contains(['?', '#']) {
        anyhow::bail!(
            "Provider profile '{}' has a base_url carrying a query string or fragment. \
             A gemini-compatible base_url must not carry a query string: the wire appends \
             \"/models/<model>:streamGenerateContent?alt=sse\" to it, so the key travels through \
             api_key_env or env_file instead.",
            profile_name,
        );
    }

    let auth = profile_chat_auth(profile);

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

    let mut headers = profile
        .headers
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    // The Messages API refuses a request without a version header. The profile
    // may name its own; the default fills in only when it does not.
    if matches!(wire, WireProtocol::AnthropicMessages)
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("anthropic-version"))
    {
        headers.push((
            "anthropic-version".to_string(),
            crate::provider_catalog::ANTHROPIC_VERSION_HEADER_VALUE.to_string(),
        ));
    }

    build_direct_runtime(DirectOpenAiCompatibleSpec {
        base_url,
        auth,
        wire,
        api_key,
        display_name: profile_name.to_string(),
        profile_id: profile_name.to_string(),
        default_model,
        models,
        headers,
        extra_body: profile.extra_body.clone(),
        prompt_cache_key: profile.prompt_cache_key,
        prompt_cache_retention: super::openai::prompt_cache_retention_from_env(),
        model_reasoning_efforts: profile
            .models
            .iter()
            .filter_map(|model| {
                let id = model.id.trim();
                let effort = model.reasoning_effort.as_deref().map(str::trim)?;
                (!id.is_empty() && !effort.is_empty()).then(|| (id.to_string(), effort.to_string()))
            })
            .collect(),
        stream_first_event_timeout: profile
            .stream_first_event_timeout_secs
            .map(|seconds| Duration::from_secs(seconds.get().into())),
        stream_idle_timeout: profile
            .stream_idle_timeout_secs
            .map(|seconds| Duration::from_secs(seconds.get().into())),
        request_slots: request_slots(profile_name, profile.max_in_flight_requests),
    })
}

/// The wire a supported `NamedProviderType` speaks, or `None` for a type this
/// build cannot run.
fn named_provider_wire(provider_type: &crate::config::NamedProviderType) -> Option<WireProtocol> {
    match provider_type {
        crate::config::NamedProviderType::OpenAiCompatible => Some(WireProtocol::ChatCompletions),
        crate::config::NamedProviderType::OpenAiResponses => Some(WireProtocol::Responses),
        crate::config::NamedProviderType::AnthropicCompatible => Some(WireProtocol::AnthropicMessages),
        crate::config::NamedProviderType::GeminiCompatible => Some(WireProtocol::GeminiGenerateContent),
        crate::config::NamedProviderType::OpenRouter => None,
    }
}

/// The config.toml `type` value of a [`NamedProviderType`], matching its serde name.
fn named_provider_type_name(provider_type: &crate::config::NamedProviderType) -> &'static str {
    match provider_type {
        crate::config::NamedProviderType::OpenAiCompatible => "openai-compatible",
        crate::config::NamedProviderType::OpenAiResponses => "openai-responses",
        crate::config::NamedProviderType::AnthropicCompatible => "anthropic-compatible",
        crate::config::NamedProviderType::GeminiCompatible => "gemini-compatible",
        crate::config::NamedProviderType::OpenRouter => "open-router",
    }
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

/// The endpoint and credential the `image` role's caller posts to.
///
/// Image generation is the one documented exception to "one model path", and it
/// stays an endpoint rather than a runtime: the tool posts to
/// `/images/generations` on the active profile's existing base URL with the
/// existing credential and `extra_body`. Composition rather than a second
/// [`Provider`] keeps the failure modes identical to a chat turn (the same key,
/// the same host, the same proxy) and keeps the runtime count at one.
#[derive(Debug, Clone)]
pub struct ImageEndpoint {
    /// Profile name, for messages and logs.
    pub profile: String,
    /// Endpoint root, without a `/chat/completions` or `/images/…` suffix.
    pub base_url: String,
    /// How the profile presents its credential.
    pub auth: ChatAuth,
    /// The credential, when the profile has one.
    pub api_key: Option<String>,
    /// Profile-level body fields, merged into an image request the same way a
    /// chat request merges them.
    pub extra_body: Option<serde_json::Value>,
    pub(crate) request_slots: Option<Arc<Semaphore>>,
}

/// Resolve the endpoint the `image` role posts to.
///
/// `Ok(None)` means the profile exists but names no `image` model, which is the
/// off state: the caller reports that image generation is not configured rather
/// than making a request that cannot succeed. An `image` model with no usable
/// profile is an error, because that is a configuration mistake the user wants
/// to hear about.
pub fn image_endpoint(profile_name: &str) -> anyhow::Result<Option<ImageEndpoint>> {
    let config = crate::config::config();
    let Some(profile) = config.providers.get(profile_name) else {
        anyhow::bail!("Provider profile '{profile_name}' is not configured.");
    };
    // `/images/generations` exists only on the OpenAI-shaped endpoints; the
    // Anthropic and Gemini wires have no image route at all, so naming one here
    // would send an image request to a URL that cannot answer it.
    if !matches!(
        profile.provider_type,
        crate::config::NamedProviderType::OpenAiCompatible | crate::config::NamedProviderType::OpenAiResponses
    ) {
        anyhow::bail!(
            "Provider profile '{profile_name}' has type \"{}\"; image generation needs an \
             \"openai-compatible\" or \"openai-responses\" profile.",
            named_provider_type_name(&profile.provider_type)
        );
    }
    let model = profile
        .model_roles
        .roles
        .image
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    if model.is_none() {
        return Ok(None);
    }
    let base_url = crate::provider_catalog::normalize_api_base(&profile.base_url).ok_or_else(|| {
        anyhow::anyhow!(
            "Provider profile '{profile_name}' has invalid base_url '{}'. Use an absolute https:// URL (or http://localhost).",
            profile.base_url,
        )
    })?;
    let auth = profile_chat_auth(profile);
    Ok(Some(ImageEndpoint {
        profile: profile_name.to_string(),
        base_url,
        auth,
        api_key: named_profile_api_key(profile),
        extra_body: profile.extra_body.clone(),
        request_slots: request_slots(profile_name, profile.max_in_flight_requests),
    }))
}

/// The `image` model this profile names, when it names one.
pub fn image_model(profile_name: &str) -> Option<String> {
    crate::config::config()
        .providers
        .get(profile_name)
        .and_then(|profile| profile.model_roles.roles.image.as_deref())
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

/// The credential placement a `[providers.<name>]` profile resolves to.
///
/// The mode and header both come from the profile's resolution helpers, so the
/// runtime and the env export can never disagree about how a profile
/// authenticates.
fn profile_chat_auth(profile: &crate::config::NamedProviderConfig) -> ChatAuth {
    match profile.resolved_auth() {
        crate::config::NamedProviderAuth::Bearer => ChatAuth::Bearer,
        crate::config::NamedProviderAuth::Header => ChatAuth::Header {
            header: profile
                .resolved_auth_header()
                .unwrap_or_else(|| profile.provider_type.default_auth_header().to_string()),
        },
        crate::config::NamedProviderAuth::None => ChatAuth::None,
    }
}
