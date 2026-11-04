use super::*;
use crate::provider_catalog::{LoginProviderDescriptor, LoginProviderTarget};
pub(super) use horon_agent_provider_core::{ActiveProvider, ProviderAvailability};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigProviderSelection {
    BuiltIn(ActiveProvider),
    OpenAiCompatibleProfile(&'static str),
    NamedProfile(String, ActiveProvider),
}

impl ConfigProviderSelection {
    pub(crate) fn active_provider(&self) -> ActiveProvider {
        match self {
            Self::BuiltIn(provider) => *provider,
            Self::OpenAiCompatibleProfile(_) | Self::NamedProfile(_, _) => ActiveProvider::OpenAI,
        }
    }

    pub(crate) fn display_label(&self) -> String {
        match self {
            Self::BuiltIn(provider) => MultiProvider::provider_key(*provider).to_string(),
            Self::OpenAiCompatibleProfile(profile_id) => {
                let resolved = crate::provider_catalog::resolve_openai_compatible_profile_selection(profile_id)
                    .map(crate::provider_catalog::resolve_openai_compatible_profile);
                match resolved {
                    Some(profile) => format!("OpenAI-compatible profile {}", profile.display_name),
                    None => format!("OpenAI-compatible profile {}", profile_id),
                }
            }
            Self::NamedProfile(profile, _) => format!("provider profile '{}'", profile),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultModelSelection {
    pub model_spec: String,
    pub provider_key: Option<String>,
}

impl MultiProvider {
    pub(super) fn auto_default_provider(availability: ProviderAvailability) -> ActiveProvider {
        horon_agent_provider_core::auto_default_provider(availability)
    }

    pub(super) fn parse_provider_hint(value: &str) -> Option<ActiveProvider> {
        horon_agent_provider_core::parse_provider_hint(value)
    }

    pub(super) fn initial_provider_from_env() -> Option<ActiveProvider> {
        let explicit = std::env::var("HORON_AGENT_INITIAL_PROVIDER_EXPLICIT")
            .ok()
            .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"));
        if !explicit {
            return None;
        }

        std::env::var("HORON_AGENT_ACTIVE_PROVIDER")
            .ok()
            .and_then(|value| Self::parse_provider_hint(&value))
    }

    pub(super) fn provider_label(provider: ActiveProvider) -> &'static str {
        horon_agent_provider_core::provider_label(provider)
    }

    pub(super) fn provider_key(provider: ActiveProvider) -> &'static str {
        horon_agent_provider_core::provider_key(provider)
    }

    pub(super) fn set_active_provider(&self, provider: ActiveProvider) {
        *self.active.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = provider;
        self.invalidate_routes_memo();
    }

    pub fn config_default_provider_for_login_provider(provider: LoginProviderDescriptor) -> Option<&'static str> {
        match provider.target {
            LoginProviderTarget::Claude | LoginProviderTarget::ClaudeApiKey => Some("claude"),
            LoginProviderTarget::OpenAi | LoginProviderTarget::OpenAiApiKey => Some("openai"),
            LoginProviderTarget::OpenRouter => Some("openrouter"),
            LoginProviderTarget::Bedrock => Some("bedrock"),
            LoginProviderTarget::OpenAiCompatible(profile) => Some(profile.id),
            LoginProviderTarget::Cursor => Some("cursor"),
            LoginProviderTarget::Copilot => Some("copilot"),
            LoginProviderTarget::Gemini => Some("gemini"),
            LoginProviderTarget::Antigravity => Some("antigravity"),
            LoginProviderTarget::AutoImport
            | LoginProviderTarget::HoronAgent
            | LoginProviderTarget::Azure
            | LoginProviderTarget::GrokBuild
            | LoginProviderTarget::Google => None,
        }
    }

    pub fn openai_compatible_profile_id_from_route<'a>(api_method: &'a str, provider_display: &str) -> Option<&'a str> {
        let parsed = ModelRouteApiMethod::parse(api_method);
        match parsed {
            ModelRouteApiMethod::OpenAiCompatible { profile_id: Some(_) } => api_method
                .split_once(':')
                .map(|(_, profile_id)| profile_id.trim())
                .filter(|profile_id| !profile_id.is_empty()),
            ModelRouteApiMethod::OpenAiCompatible { profile_id: None } => {
                crate::provider_catalog::openai_compatible_profile_id_for_display_name(provider_display)
            }
            _ => None,
        }
    }

    pub fn default_model_selection_from_route(
        bare_name: &str,
        api_method: &str,
        provider_display: &str,
    ) -> DefaultModelSelection {
        let api_method_kind = ModelRouteApiMethod::parse(api_method);
        let profile_id = match &api_method_kind {
            ModelRouteApiMethod::OpenAiCompatible {
                profile_id: Some(profile_id),
            } => Some(profile_id.clone()),
            ModelRouteApiMethod::OpenAiCompatible { profile_id: None } => {
                crate::provider_catalog::openai_compatible_profile_id_for_display_name(provider_display)
                    .map(ToOwned::to_owned)
            }
            _ => None,
        };
        let model_spec = match &api_method_kind {
            ModelRouteApiMethod::Copilot => format!("copilot:{}", bare_name),
            ModelRouteApiMethod::GrokBuild => crate::provider::grok_build_model_spec(bare_name),
            ModelRouteApiMethod::ClaudeOAuth => format!("claude-oauth:{}", bare_name),
            ModelRouteApiMethod::AnthropicApiKey if provider_display == "Anthropic" => {
                format!("claude-api:{}", bare_name)
            }
            ModelRouteApiMethod::Cursor => format!("cursor:{}", bare_name),
            ModelRouteApiMethod::Bedrock => format!("bedrock:{}", bare_name),
            ModelRouteApiMethod::OpenAIApiKey => format!("openai-api:{}", bare_name),
            ModelRouteApiMethod::OpenAIOAuth => format!("openai-oauth:{}", bare_name),
            _ if provider_display == "Antigravity" => format!("antigravity:{}", bare_name),
            ModelRouteApiMethod::OpenAiCompatible { .. } => bare_name.to_string(),
            ModelRouteApiMethod::OpenRouter if provider_display != "auto" => {
                let model_id =
                    crate::provider::openrouter_catalog_model_id(bare_name).unwrap_or_else(|| bare_name.to_string());
                format!("{}@{}", model_id, provider_display)
            }
            _ => bare_name.to_string(),
        };

        let provider_key = match &api_method_kind {
            ModelRouteApiMethod::HoronAgentSubscription => Some("horon-agent".to_string()),
            ModelRouteApiMethod::GrokBuild => Some("grok-build".to_string()),
            ModelRouteApiMethod::AnthropicApiKey
                if provider_display == "Anthropic"
                    && crate::provider::provider_for_model(bare_name) == Some("claude") =>
            {
                Some("claude-api".to_string())
            }
            ModelRouteApiMethod::ClaudeOAuth if crate::provider::provider_for_model(bare_name) == Some("claude") => {
                // Persist the unambiguous OAuth key. The bare `claude` key is
                // also what `derive_session_provider_key` writes for automatic
                // credential mode, so storing it here made a deliberate OAuth
                // pick indistinguishable from "auto" on restore.
                Some("claude-oauth".to_string())
            }
            ModelRouteApiMethod::OpenAIApiKey => Some("openai-api".to_string()),
            ModelRouteApiMethod::OpenAIOAuth => Some("openai-oauth".to_string()),
            ModelRouteApiMethod::Copilot => Some("copilot".to_string()),
            ModelRouteApiMethod::Cursor => Some("cursor".to_string()),
            ModelRouteApiMethod::Bedrock => Some("bedrock".to_string()),
            ModelRouteApiMethod::Other(method) if method == "cli" && provider_display == "Antigravity" => {
                Some("antigravity".to_string())
            }
            ModelRouteApiMethod::OpenRouter => Some("openrouter".to_string()),
            ModelRouteApiMethod::OpenAiCompatible { .. } => profile_id.clone(),
            _ => profile_id.clone(),
        };

        let model_spec = if horon_agent_provider_core::oauth_account_label(api_method).is_some() {
            format!("{api_method}:{bare_name}")
        } else {
            model_spec
        };
        DefaultModelSelection {
            model_spec,
            provider_key,
        }
    }

    /// Canonicalize a persisted session `provider_key` into the legacy
    /// vocabulary the reconstruction helpers below understand.
    ///
    /// Two vocabularies persist into sessions and must be treated as
    /// equivalent, otherwise the OAuth-vs-API-key auth mode is silently lost on
    /// restore/model-switch:
    ///
    /// - Legacy `/model` + login path: `claude` / `claude-api` / `openai` /
    ///   `openai-api`.
    /// - Structured model-route picker (`RouteSelection::stable_id`):
    ///   `claude-oauth` / `anthropic-api-key` / `openai-oauth` /
    ///   `openai-api-key`.
    ///
    /// Both encode the same auth route; we fold the picker forms back onto the
    /// canonical keys so a session whose `provider_key` is `anthropic-api-key`
    /// (and whose `route_api_method` was not also persisted, e.g. inherited by a
    /// child/forked session) still reconstructs the Anthropic API-key route
    /// instead of falling through to Auto (which prefers OAuth).
    pub(crate) fn canonical_session_provider_key(provider_key: &str) -> &str {
        // Fold any dual-auth (Anthropic/OpenAI OAuth-vs-API) alias onto its
        // canonical session key via the single shared parser, so this never
        // drifts from the route/runtime vocabularies. Non-dual keys pass through.
        if let Some(route) = horon_agent_provider_core::AuthRoute::parse(provider_key) {
            return route.session_provider_key();
        }
        provider_key.trim()
    }

    fn explicit_session_provider_key_for_model_request(model_request: &str) -> Option<String> {
        let model_request = model_request.trim();
        if let Some((prefix, rest)) = model_request.split_once(':') {
            let prefix = prefix.trim();
            if !prefix.is_empty() && !rest.trim().is_empty() {
                if horon_agent_provider_core::oauth_account_label(prefix).is_some() {
                    return Some(prefix.to_string());
                }
                // Dual-auth (Anthropic/OpenAI) prefixes fold onto their canonical
                // session key via the single shared parser.
                if let Some(route) = horon_agent_provider_core::AuthRoute::parse(prefix) {
                    return Some(route.session_provider_key().to_string());
                }
                match prefix {
                    "copilot" | "antigravity" | "gemini" | "cursor" | "bedrock" | "openrouter" => {
                        return Some(prefix.to_string());
                    }
                    _ => {
                        if crate::provider_catalog::resolve_openai_compatible_profile_selection(prefix).is_some()
                            || crate::config::config().providers.contains_key(prefix)
                        {
                            return Some(prefix.to_string());
                        }
                    }
                }
            }
        }

        if model_request.contains('@') {
            return Some("openrouter".to_string());
        }

        None
    }

    pub fn session_provider_key_for_model_request(model_request: &str, provider_name: &str) -> Option<String> {
        if let Some(provider_key) = Self::explicit_session_provider_key_for_model_request(model_request) {
            return Some(provider_key);
        }

        Self::session_provider_key_from_provider_name(provider_name)
            .or_else(|| crate::session::derive_session_provider_key(provider_name))
    }

    pub fn session_provider_key_after_model_switch(
        model_request: &str,
        provider_name: &str,
        previous_provider_key: Option<&str>,
    ) -> Option<String> {
        if let Some(provider_key) = Self::explicit_session_provider_key_for_model_request(model_request) {
            return Some(provider_key);
        }

        if let Some(previous_provider_key) = previous_provider_key
            .map(str::trim)
            .filter(|provider_key| !provider_key.is_empty())
            && Self::session_provider_key_matches_provider_name(previous_provider_key, provider_name)
        {
            return Some(previous_provider_key.to_string());
        }

        Self::session_provider_key_from_provider_name(provider_name)
            .or_else(|| crate::session::derive_session_provider_key(provider_name))
    }

    fn session_provider_key_from_provider_name(provider_name: &str) -> Option<String> {
        let normalized = provider_name.trim().to_ascii_lowercase();
        let key = match normalized.as_str() {
            "horon-agent" => "horon-agent",
            "anthropic" | "claude" | "claude cli" => "claude",
            "openai" => "openai",
            "github copilot" | "copilot" => "copilot",
            "openrouter" => "openrouter",
            "cursor" => "cursor",
            "gemini" | "google" => "gemini",
            "antigravity" => "antigravity",
            "bedrock" | "aws bedrock" => "bedrock",
            "" => return None,
            _ => return None,
        };
        Some(key.to_string())
    }

    fn session_provider_key_matches_provider_name(provider_key: &str, provider_name: &str) -> bool {
        let provider_key = if horon_agent_provider_core::oauth_account_label(provider_key).is_some() {
            provider_key
                .split_once('@')
                .map(|(method, _)| method)
                .unwrap_or(provider_key)
        } else {
            provider_key
        };
        let provider_key = Self::canonical_session_provider_key(provider_key.trim());
        let Some(derived) = Self::session_provider_key_from_provider_name(provider_name)
            .or_else(|| crate::session::derive_session_provider_key(provider_name))
        else {
            return false;
        };
        match derived.as_str() {
            "claude" => matches!(provider_key, "claude" | "claude-oauth" | "claude-api" | "anthropic"),
            "openai" => matches!(provider_key, "openai" | "openai-oauth" | "openai-api"),
            "openrouter" => {
                provider_key == "openrouter"
                    || crate::provider_catalog::resolve_openai_compatible_profile_selection(provider_key).is_some()
                    || crate::config::config().providers.contains_key(provider_key)
            }
            other => provider_key == other,
        }
    }

    pub fn model_switch_request_for_session_model(model: &str, provider_key: Option<&str>) -> String {
        let model = model.trim();
        if model.is_empty() {
            return String::new();
        }

        if crate::provider::explicit_model_provider_prefix(model).is_some() {
            return model.to_string();
        }
        if model
            .split_once(':')
            .is_some_and(|(method, _)| horon_agent_provider_core::oauth_account_label(method).is_some())
        {
            return model.to_string();
        }

        // An `@provider` suffix is an explicit OpenRouter upstream pin. It is
        // stronger route identity than older/stale `provider_key` metadata.
        // In particular, sessions created through the public CLI could retain
        // an `openai-api` key from process-level runtime state while correctly
        // persisting `z-ai/glm-5.2@Novita` as their model. Restoring through the
        // stale key sent the pinned model to OpenAI and left the resumed run on
        // its default model instead. Reconstruct the explicit OpenRouter route
        // first so both the provider and pin survive resume.
        if model.contains('@') {
            return format!("openrouter:{model}");
        }

        if let Some((prefix, rest)) = model.split_once(':') {
            let prefix = prefix.trim();
            if !prefix.is_empty()
                && !rest.trim().is_empty()
                && (crate::provider_catalog::resolve_openai_compatible_profile_selection(prefix).is_some()
                    || crate::config::config().providers.contains_key(prefix))
            {
                return model.to_string();
            }
        }

        let Some(provider_key) = provider_key
            .map(str::trim)
            .filter(|provider_key| !provider_key.is_empty())
        else {
            return model.to_string();
        };
        if horon_agent_provider_core::oauth_account_label(provider_key).is_some() {
            return format!("{provider_key}:{model}");
        }
        // Dual-auth keys map to a model prefix via the single shared parser,
        // keeping the emitted prefix in lockstep with the parsers.
        //
        // Only an *explicitly credential-pinned* key may emit a pinning prefix.
        // A bare `claude` / `anthropic` / `openai` key is what
        // `derive_session_provider_key` writes for any Anthropic/OpenAI session,
        // including ones running in automatic credential mode. Folding those
        // onto `claude-oauth:` / `openai-oauth:` turned "auto" into a hard OAuth
        // pin on restore, which disabled the runtime's API-key fallback and made
        // every turn fail with "OAuth token is expired and refresh failed" even
        // though a working API key was configured.
        //
        // Deliberate picks stay unambiguous because `session_provider_key`
        // spells the OAuth routes out (`claude-oauth` / `openai-oauth`) rather
        // than reusing the bare provider name.
        if let Some(route) = horon_agent_provider_core::AuthRoute::parse_explicit_credential_prefix(provider_key) {
            return format!("{}:{model}", route.model_prefix());
        }
        if let Some(route) = horon_agent_provider_core::AuthRoute::parse(provider_key) {
            // Bare provider key: route to the provider without pinning a
            // credential so automatic OAuth -> API-key fallback stays available.
            return format!("{}:{model}", route.provider.bare_model_prefix());
        }

        // Fold the remaining picker vocabulary onto the canonical keys
        // (non-dual-auth providers and OpenAI-compatible profiles).
        let provider_key = Self::canonical_session_provider_key(provider_key);

        match provider_key {
            "copilot" | "antigravity" | "gemini" | "cursor" | "bedrock" | "openrouter" => {
                format!("{provider_key}:{model}")
            }
            _ => {
                if crate::provider_catalog::resolve_openai_compatible_profile_selection(provider_key).is_some()
                    || crate::config::config().providers.contains_key(provider_key)
                {
                    format!("{provider_key}:{model}")
                } else {
                    model.to_string()
                }
            }
        }
    }

    pub fn model_switch_request_for_session_route(
        model: &str,
        provider_key: Option<&str>,
        route_api_method: Option<&str>,
    ) -> String {
        let model = model.trim();
        if model.is_empty() {
            return String::new();
        }
        // A worker model override can carry its own transport while the child
        // still has its parent's saved route. Honor the explicit model first.
        if crate::provider::explicit_model_provider_prefix(model).is_some()
            || model
                .split_once(':')
                .is_some_and(|(method, _)| horon_agent_provider_core::oauth_account_label(method).is_some())
            || model.split_once(':').is_some_and(|(prefix, rest)| {
                !prefix.trim().is_empty()
                    && !rest.trim().is_empty()
                    && (crate::provider_catalog::resolve_openai_compatible_profile_selection(prefix).is_some()
                        || crate::config::config().providers.contains_key(prefix))
            })
        {
            return model.to_string();
        }
        if model.contains('@') {
            return format!("openrouter:{model}");
        }
        if let Some(api_method) = route_api_method
            .map(str::trim)
            .filter(|api_method| !api_method.is_empty())
        {
            if horon_agent_provider_core::oauth_account_label(api_method).is_some() {
                return format!("{api_method}:{model}");
            }
            match ModelRouteApiMethod::parse(api_method) {
                ModelRouteApiMethod::HoronAgentSubscription => return model.to_string(),
                ModelRouteApiMethod::GrokBuild => {
                    return crate::provider::grok_build_model_spec(model);
                }
                ModelRouteApiMethod::ClaudeOAuth => return format!("claude-oauth:{model}"),
                ModelRouteApiMethod::AnthropicApiKey => return format!("claude-api:{model}"),
                ModelRouteApiMethod::OpenAIOAuth => return format!("openai-oauth:{model}"),
                ModelRouteApiMethod::OpenAIApiKey => return format!("openai-api:{model}"),
                ModelRouteApiMethod::OpenRouter => return format!("openrouter:{model}"),
                ModelRouteApiMethod::OpenAiCompatible {
                    profile_id: Some(profile_id),
                } => return format!("{profile_id}:{model}"),
                ModelRouteApiMethod::Copilot => return format!("copilot:{model}"),
                ModelRouteApiMethod::Cursor => return format!("cursor:{model}"),
                ModelRouteApiMethod::Bedrock => return format!("bedrock:{model}"),
                ModelRouteApiMethod::AntigravityHttps => return format!("antigravity:{model}"),
                ModelRouteApiMethod::OpenAiCompatible { profile_id: None }
                | ModelRouteApiMethod::CodeAssistOAuth
                | ModelRouteApiMethod::RemoteCatalog
                | ModelRouteApiMethod::Current
                | ModelRouteApiMethod::Other(_) => {}
            }
        }

        Self::model_switch_request_for_session_model(model, provider_key)
    }

    pub(super) fn resolve_config_provider_selection(
        value: &str,
        cfg: &crate::config::Config,
    ) -> Option<ConfigProviderSelection> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(profile) = crate::provider_catalog::resolve_openai_compatible_profile_selection(trimmed) {
            return Some(ConfigProviderSelection::OpenAiCompatibleProfile(profile.id));
        }

        if cfg.providers.contains_key(trimmed) {
            return Some(ConfigProviderSelection::NamedProfile(
                trimmed.to_string(),
                ActiveProvider::OpenAI,
            ));
        }

        // Accept the dual-auth `--provider` vocabulary (`anthropic-api`,
        // `claude-api`, `openai-api`, `claude-oauth`, ...) in addition to the
        // bare provider hints. Without this, a config `default_provider =
        // "anthropic-api"` is rejected as "Unknown default_provider" and the
        // OAuth-vs-API routing decision it encodes is silently dropped.
        Self::parse_provider_hint(trimmed)
            .or_else(|| horon_agent_provider_core::AuthRoute::parse(trimmed).map(|route| route.active_provider()))
            .map(ConfigProviderSelection::BuiltIn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_provider_defaults_are_canonical_config_keys() {
        assert_eq!(
            MultiProvider::config_default_provider_for_login_provider(crate::provider_catalog::OPENAI_LOGIN_PROVIDER,),
            Some("openai")
        );
        assert_eq!(
            MultiProvider::config_default_provider_for_login_provider(
                crate::provider_catalog::OPENAI_API_LOGIN_PROVIDER,
            ),
            Some("openai")
        );
        assert_eq!(
            MultiProvider::config_default_provider_for_login_provider(crate::provider_catalog::OPENCODE_LOGIN_PROVIDER,),
            Some("opencode")
        );
        assert_eq!(
            MultiProvider::config_default_provider_for_login_provider(crate::provider_catalog::AZURE_LOGIN_PROVIDER,),
            None
        );
    }
}
