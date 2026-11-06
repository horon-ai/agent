use anyhow::Result;
use std::sync::{Arc, OnceLock};

use crate::message::{Message, ToolDefinition};
use crate::provider;
use crate::provider::EventStream;
use crate::provider_catalog::{
    OpenAiCompatibleProfile, force_apply_openai_compatible_profile_env, resolve_openai_compatible_profile,
};
use crate::tool;

use super::output;

pub(crate) use crate::external_auth::maybe_run_external_auth_auto_import_flow;

/// Provider selected on the command line.
///
/// Model requests go directly to an OpenAI-compatible `/chat/completions`
/// endpoint, so the only choices are discovery (`auto`, a configured
/// `[provider] default_provider`) and a named OpenAI-compatible profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderChoice {
    Auto,
    OpenaiCompatible,
    Profile(String),
}

impl ProviderChoice {
    pub fn as_arg_value(&self) -> String {
        match self {
            Self::Auto => "auto".to_string(),
            Self::OpenaiCompatible => "openai-compatible".to_string(),
            Self::Profile(name) => name.clone(),
        }
    }

    /// OpenAI-compatible profile this choice names, if any.
    pub fn profile_id(&self) -> Option<&str> {
        match self {
            Self::Auto => None,
            Self::OpenaiCompatible => Some("openai-compatible"),
            Self::Profile(name) => Some(name),
        }
    }
}

/// Parse and validate one `--provider` value.
///
/// Accepts `auto`, `openai-compatible` (plus its `compat`/`custom` aliases),
/// a built-in OpenAI-compatible profile id, or a `[providers.<name>]` profile
/// from `config.toml`. Unknown names fail here so `--provider claude` is a
/// usage error instead of a runtime surprise.
pub fn parse_provider_choice(value: &str) -> std::result::Result<ProviderChoice, String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" => Err("provider name cannot be empty".to_string()),
        "auto" => Ok(ProviderChoice::Auto),
        "openai-compatible" | "compat" | "custom" => Ok(ProviderChoice::OpenaiCompatible),
        _ if crate::provider_catalog::openai_compatible_profile_by_id(&normalized).is_some() => {
            Ok(ProviderChoice::Profile(normalized))
        }
        _ if crate::config::config().providers.contains_key(&normalized) => Ok(ProviderChoice::Profile(normalized)),
        _ => Err(format!(
            "unknown provider '{}': expected 'auto', 'openai-compatible', or an OpenAI-compatible profile id from config.toml",
            value.trim()
        )),
    }
}

/// `HORON_AGENT_OPENROUTER_MODEL` as set by the user when the process started.
///
/// Provider init clears and rewrites the `HORON_AGENT_OPENROUTER_*` env slots for
/// whichever runtime it activates, so by the time the runtime reads the variable
/// a user-supplied value is already gone. Snapshot it first.
static USER_OPENROUTER_MODEL: OnceLock<Option<String>> = OnceLock::new();

/// Called once from `startup::run` before any provider init. Entry points
/// that skip startup (unit tests, embedded flows) never consult the snapshot.
pub fn capture_user_openrouter_model_env() {
    let _ = USER_OPENROUTER_MODEL.set(
        std::env::var("HORON_AGENT_OPENROUTER_MODEL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    );
}

fn user_openrouter_model_env() -> Option<&'static str> {
    USER_OPENROUTER_MODEL.get()?.as_deref()
}

fn resolved_profile_default_model(profile: OpenAiCompatibleProfile) -> Option<String> {
    resolve_openai_compatible_profile(profile).default_model
}

pub async fn init_provider(choice: &ProviderChoice, model: Option<&str>) -> Result<Arc<dyn provider::Provider>> {
    init_provider_with_options(choice, model, true, true).await
}

/// Provider init for one-shot `horon-agent run`. A requested model that cannot
/// be applied is an error: silently answering with a different model is worse
/// than failing for a scripted run.
pub async fn init_provider_for_run(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<Arc<dyn provider::Provider>> {
    init_provider_with_options(choice, model, false, true).await
}

pub async fn init_provider_for_serve(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<Arc<dyn provider::Provider>> {
    init_provider_for_control_plane(choice, model, true).await
}

pub async fn init_provider_for_diagnostics(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<Arc<dyn provider::Provider>> {
    init_provider_for_control_plane(choice, model, false).await
}

/// A server/control-plane launch stays up without a usable model provider so
/// ACP clients can still authenticate and be told that no model is usable.
/// Model calls fail with the setup instructions instead of pretending to work.
struct UnconfiguredProvider {
    reason: String,
}

#[async_trait::async_trait]
impl provider::Provider for UnconfiguredProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("{}", self.reason)
    }

    fn name(&self) -> &str {
        "unconfigured"
    }

    fn model(&self) -> String {
        String::new()
    }

    fn unavailable_reason(&self) -> Option<String> {
        Some(self.reason.clone())
    }

    fn set_model(&self, _model: &str) -> Result<()> {
        anyhow::bail!("{}", self.reason)
    }

    fn fork(&self) -> Arc<dyn provider::Provider> {
        Arc::new(Self {
            reason: self.reason.clone(),
        })
    }
}

async fn init_provider_for_control_plane(
    choice: &ProviderChoice,
    model: Option<&str>,
    show_init_messages: bool,
) -> Result<Arc<dyn provider::Provider>> {
    match init_provider_with_options(choice, model, show_init_messages, false).await {
        Ok(provider) => Ok(provider),
        Err(error) if matches!(choice, ProviderChoice::Auto) && auto_provider_is_unconfigured() => {
            let provider: Arc<dyn provider::Provider> = Arc::new(UnconfiguredProvider {
                reason: error.to_string(),
            });
            crate::provider::set_active_provider(Arc::clone(&provider));
            Ok(provider)
        }
        Err(error) => Err(error),
    }
}

/// Provider names a previous build shipped and this one no longer resolves.
/// A config that still names one must load; the CLI warns and falls back to
/// auto discovery instead of failing every launch.
fn is_removed_provider_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "horon-agent" | "horon-agent-subscription" | "subscription"
    )
}

fn auto_provider_is_unconfigured() -> bool {
    named_profile_from_env().is_none()
        && crate::config::config()
            .provider
            .default_provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && !is_removed_provider_name(value))
            .is_none()
        && std::env::var("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE")
            .ok()
            .as_deref()
            .and_then(builtin_profile_by_id)
            .is_none()
}

pub async fn init_provider_quiet(choice: &ProviderChoice, model: Option<&str>) -> Result<Arc<dyn provider::Provider>> {
    init_provider_with_options(choice, model, false, true).await
}

/// Auth validation sends model and tool-smoke requests, so it goes through the
/// same provider boundary as a real run.
pub async fn init_provider_for_validation(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<Arc<dyn provider::Provider>> {
    init_provider_with_options(choice, model, false, false).await
}

/// The refusal a `--provider` value gets when it names no usable profile.
fn builtin_provider_refusal(choice: &ProviderChoice) -> anyhow::Error {
    anyhow::anyhow!(
        "'--provider {}' is not available: model requests go directly to an OpenAI-compatible /chat/completions endpoint. \
         Configure a [providers.<name>] profile with type = \"openai-compatible\" in config.toml, then select it with \
         --provider-profile <name> or [provider] default_provider = \"<name>\". Built-in OpenAI-compatible profiles such as \
         openai-compatible, deepseek, groq and ollama can also be selected with --provider <name>.",
        choice.as_arg_value()
    )
}

/// The error a launch gets when nothing names a model endpoint.
fn unconfigured_provider_error() -> anyhow::Error {
    anyhow::anyhow!(
        "No model provider is configured. Add a [providers.<name>] profile with type = \"openai-compatible\" to config.toml \
         (set base_url, default_model, and either api_key_env or auth = \"none\"), then select it with \
         --provider-profile <name> or [provider] default_provider = \"<name>\". For a local server use e.g. \
         base_url = \"http://127.0.0.1:11434/v1\" and auth = \"none\"."
    )
}

fn builtin_profile_by_id(id: &str) -> Option<OpenAiCompatibleProfile> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    crate::provider_catalog::openai_compatible_profile_by_id(id)
}

fn named_profile_from_env() -> Option<String> {
    [
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
    ]
    .iter()
    .find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

/// Build the runtime for one named `[providers.<name>]` profile.
fn provider_for_named_profile(
    name: &str,
    model: Option<&str>,
    strict_model: bool,
) -> Result<Arc<dyn provider::Provider>> {
    let runtime = provider::MultiProvider::from_named_provider_profile(name)?;
    let runtime: Arc<dyn provider::Provider> = Arc::new(runtime);
    if let Some(model) = model
        && let Err(error) = runtime.set_model(model)
    {
        if strict_model {
            anyhow::bail!("Failed to select model '{model}': {error}");
        }
        output::stderr_info(format!("Warning: failed to set model '{model}': {error}"));
    }
    Ok(runtime)
}

/// Build the runtime for one built-in OpenAI-compatible profile.
fn provider_for_builtin_profile(
    profile: OpenAiCompatibleProfile,
    model: Option<&str>,
    strict_model: bool,
) -> Result<Arc<dyn provider::Provider>> {
    force_apply_openai_compatible_profile_env(Some(profile));
    let runtime = provider::MultiProvider::from_openai_compatible_profile(profile)?;
    let runtime: Arc<dyn provider::Provider> = Arc::new(runtime);
    let model = model
        .map(str::to_owned)
        .or_else(|| user_openrouter_model_env().map(str::to_owned))
        .or_else(|| resolved_profile_default_model(profile));
    if let Some(model) = model.as_deref()
        && let Err(error) = runtime.set_model(model)
    {
        if strict_model {
            anyhow::bail!("Failed to select model '{model}': {error}");
        }
        output::stderr_info(format!("Warning: failed to set model '{model}': {error}"));
    }
    Ok(runtime)
}

fn provider_for_configured_name(
    name: &str,
    model: Option<&str>,
    strict_model: bool,
) -> Result<Arc<dyn provider::Provider>> {
    if crate::config::config().providers.contains_key(name) {
        return provider_for_named_profile(name, model, strict_model);
    }
    if let Some(profile) = builtin_profile_by_id(name) {
        return provider_for_builtin_profile(profile, model, strict_model);
    }
    anyhow::bail!(
        "Unknown provider profile '{}'. Add [providers.{}] with type = \"openai-compatible\" to config.toml, \
         or use a built-in OpenAI-compatible profile id.",
        name,
        name
    )
}

/// `strict_model`: a requested model that cannot be applied is an error instead of a warning.
async fn init_provider_with_options(
    choice: &ProviderChoice,
    model: Option<&str>,
    show_init_messages: bool,
    strict_model: bool,
) -> Result<Arc<dyn provider::Provider>> {
    let init_notice = |message: &str| {
        if show_init_messages {
            output::stderr_info(message);
        }
    };

    // An explicit `--provider-profile` (projected as PROFILE_NAME by dispatch)
    // always names the profile to use.
    if let Some(profile_name) = named_profile_from_env() {
        crate::provider_catalog::apply_named_provider_profile_env(&profile_name)?;
        crate::env::set_var("HORON_AGENT_PROVIDER_PROFILE_ACTIVE", "1");
        init_notice(&format!("Using provider profile '{}'", profile_name));
        return provider_for_named_profile(&profile_name, model, strict_model);
    }

    match choice {
        ProviderChoice::Auto => {
            if let Some(default) = crate::config::config()
                .provider
                .default_provider
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if is_removed_provider_name(default) {
                    output::stderr_info(format!(
                        "Warning: unknown default_provider '{}'; the HoronAgent subscription was removed. Falling back to automatic provider discovery.",
                        default
                    ));
                } else {
                    init_notice(&format!("Using configured default provider '{}'", default));
                    return provider_for_configured_name(default, model, strict_model);
                }
            }
            let projected = std::env::var("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE").ok();
            if let Some(profile) = projected.as_deref().and_then(builtin_profile_by_id) {
                init_notice(&format!("Using {} via the OpenAI-compatible API", profile.display_name));
                return provider_for_builtin_profile(profile, model, strict_model);
            }
            Err(unconfigured_provider_error())
        }
        ProviderChoice::OpenaiCompatible | ProviderChoice::Profile(_) => {
            let name = choice.profile_id().ok_or_else(|| builtin_provider_refusal(choice))?;
            if let Some(profile) = builtin_profile_by_id(name) {
                if std::env::var_os("HORON_AGENT_NAMED_PROVIDER_PROFILE").is_none() {
                    force_apply_openai_compatible_profile_env(Some(profile));
                }
                init_notice(&format!(
                    "Using {} via the OpenAI-compatible API",
                    resolve_openai_compatible_profile(profile).display_name
                ));
                let model = match model {
                    Some(model) => Some(model),
                    None => user_openrouter_model_env(),
                };
                provider_for_builtin_profile(profile, model, strict_model)
            } else {
                crate::provider_catalog::apply_named_provider_profile_env(name)?;
                crate::env::set_var("HORON_AGENT_PROVIDER_PROFILE_ACTIVE", "1");
                init_notice(&format!("Using provider profile '{}'", name));
                provider_for_named_profile(name, model, strict_model)
            }
        }
    }
}

pub async fn init_provider_and_registry(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<(Arc<dyn provider::Provider>, tool::Registry)> {
    let provider = init_provider(choice, model).await?;
    let registry = tool::Registry::new(provider.clone()).await;
    Ok((provider, registry))
}

pub async fn init_provider_and_registry_for_validation(
    choice: &ProviderChoice,
    model: Option<&str>,
) -> Result<(Arc<dyn provider::Provider>, tool::Registry)> {
    let provider = init_provider_for_validation(choice, model).await?;
    let registry = tool::Registry::new(provider.clone()).await;
    Ok((provider, registry))
}

#[cfg(test)]
#[path = "provider_init_tests.rs"]
mod tests;
