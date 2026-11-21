//! User-defined `[providers.<name>]` profiles.
//!
//! A profile names one endpoint, one credential source and its model list. The
//! `type` key picks the request/response shape that endpoint speaks; the auth
//! keys pick how the credential travels. Resolution of an omitted `auth` lives
//! in [`NamedProviderConfig::resolved_auth`] and
//! [`NamedProviderConfig::resolved_auth_header`] only, so every caller —
//! runtime builder, env export, reporting — agrees on the effective mode.

use serde::{Deserialize, Serialize};

use crate::model_roles;

#[cfg(test)]
#[path = "named_provider_tests.rs"]
mod named_provider_tests;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NamedProviderType {
    #[serde(alias = "openai-compatible", alias = "openai_compatible")]
    #[default]
    OpenAiCompatible,
    #[serde(alias = "anthropic-compatible", alias = "anthropic_compatible")]
    AnthropicCompatible,
    OpenRouter,
    #[serde(rename = "openai-responses", alias = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "gemini-compatible", alias = "gemini_compatible")]
    GeminiCompatible,
}

impl NamedProviderType {
    /// Auth mode used when the profile does not name one.
    ///
    /// The OpenAI family authenticates with `Authorization: Bearer`; the
    /// Anthropic and Gemini wires send the credential in a dedicated header.
    pub fn default_auth(&self) -> NamedProviderAuth {
        match self {
            Self::OpenAiCompatible | Self::OpenAiResponses | Self::OpenRouter => NamedProviderAuth::Bearer,
            Self::AnthropicCompatible | Self::GeminiCompatible => NamedProviderAuth::Header,
        }
    }

    /// Header that carries the credential for `auth = "header"` and for this
    /// type's default auth mode.
    pub fn default_auth_header(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible | Self::OpenAiResponses | Self::OpenRouter => "api-key",
            Self::AnthropicCompatible => "x-api-key",
            Self::GeminiCompatible => "x-goog-api-key",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
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
    /// Auth mode for this profile. Unset means the default of `type`, which
    /// [`NamedProviderConfig::resolved_auth`] resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<NamedProviderAuth>,
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
    /// Extra top-level JSON fields merged into the top level of every request
    /// body on all four wires. Lets users inject non-standard parameters some
    /// backends require (e.g. NVIDIA NIM DeepSeek-V4 needs
    /// `chat_template_kwargs = { thinking = true, reasoning_effort = "high" }`).
    /// Must be a JSON object; keys here override horon-agent-generated body fields.
    #[serde(default, alias = "extra-body", skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
    /// Send a lineage-stable `prompt_cache_key` with every request to this
    /// endpoint. Unset sends it only to hosts whose gateway documents the field
    /// (`api.openai.com`, `openrouter.ai`); `false` keeps strict-schema
    /// endpoints from rejecting an unknown field.
    #[serde(default, alias = "prompt-cache-key", skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<bool>,
    /// Seconds from sending an attempt to the first meaningful stream event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_first_event_timeout_secs: Option<std::num::NonZeroU32>,
    /// Seconds without meaningful progress after the first stream event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_secs: Option<std::num::NonZeroU32>,
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
    #[serde(flatten, default)]
    pub model_roles: model_roles::ModelRoleConfig,
}

impl NamedProviderConfig {
    /// The auth mode in effect: the explicit `auth`, else the type's default.
    pub fn resolved_auth(&self) -> NamedProviderAuth {
        self.auth.unwrap_or_else(|| self.provider_type.default_auth())
    }

    /// Header name for [`NamedProviderAuth::Header`]: explicit `auth_header`,
    /// else the type default. `None` for every other auth mode.
    ///
    /// The invariant callers rely on: this returns `Some` whenever
    /// [`Self::resolved_auth`] is [`NamedProviderAuth::Header`].
    pub fn resolved_auth_header(&self) -> Option<String> {
        if self.resolved_auth() != NamedProviderAuth::Header {
            return None;
        }
        self.auth_header
            .as_deref()
            .map(str::trim)
            .filter(|header| !header.is_empty())
            .map(str::to_string)
            .or_else(|| Some(self.provider_type.default_auth_header().to_string()))
    }
}
