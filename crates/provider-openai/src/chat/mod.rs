//! Direct OpenAI-compatible Chat Completions provider.
//!
//! This is the model path that needs no plugin runtime: the agent's canonical
//! conversation is translated to the Chat Completions wire format
//! ([`request`]), streamed chunks are translated back into the shared
//! [`horon_agent_message_types::StreamEvent`] vocabulary ([`stream`]), and one
//! narrow transport adds the credential and error classification ([`transport`]).

pub mod request;
pub mod stream;
pub mod transport;

mod prompt_cache_memo;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "prompt_cache_tests.rs"]
mod prompt_cache_tests;

use crate::chat::prompt_cache_memo::PromptCacheKeyMemo;
use crate::chat::request::{CHAT_COMPLETIONS_PATH, build_chat_completions_request};
use crate::chat::stream::ChatCompletionsStream;
use crate::chat::transport::{
    ChatAuth, INITIAL_RESPONSE_TIMEOUT, apply_auth, http_response_error, redact_secret, transport_error_message,
};
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_message_types::{Message, ToolDefinition, messages_with_dynamic_system_context};
use horon_agent_provider_core::{
    EventStream, ModelRoute, Provider, ProviderRequestContext, shared_http_client,
    transport::send_with_initial_response_timeout,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// Stable, machine-facing provider name.
pub const PROVIDER_NAME: &str = "openai-compatible";

/// Reserved per-request field pinning a request to one prompt cache slot.
const PROMPT_CACHE_KEY_FIELD: &str = "prompt_cache_key";

/// Reserved per-request field selecting an extended prompt cache retention.
const PROMPT_CACHE_RETENTION_FIELD: &str = "prompt_cache_retention";

/// The official OpenAI endpoint, the only host that receives the retention field.
const OFFICIAL_OPENAI_HOST: &str = "api.openai.com";

/// Resolves the `prompt_cache_retention` value for a model id.
///
/// The model-support policy lives in base, so the runtime asks once per request
/// instead of storing a value: a model switch must not keep a retention the new
/// model refuses.
pub type PromptCacheRetentionResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// A provider that talks Chat Completions to any OpenAI-compatible endpoint.
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    /// Endpoint root, without the `/chat/completions` suffix. A root that
    /// already ends with the suffix is accepted as a full endpoint.
    base_url: String,
    auth: ChatAuth,
    api_key: Option<String>,
    /// Extra headers every request carries (beyond `auth`).
    headers: Vec<(String, String)>,
    /// Profile-level body fields merged into every request, overriding the
    /// generated values (e.g. `chat_template_kwargs`).
    extra_body: Option<Value>,
    /// Explicit `prompt_cache_key` policy. `None` sends the key only to hosts
    /// whose gateway is known to accept the field.
    prompt_cache_key: Option<bool>,
    /// Resolves the retention to send for the active model. Only consulted for
    /// [`OFFICIAL_OPENAI_HOST`].
    prompt_cache_retention: Option<PromptCacheRetentionResolver>,
    profile_id: Option<String>,
    display_name: String,
    /// The most recent `prompt_cache_key` derivation and the inputs it covers.
    /// Requests in one lineage repeat those inputs on every turn, so the memo
    /// turns a full SHA-256 pass over the system prompt and tool catalog into a
    /// string comparison.
    prompt_cache_memo: Mutex<Option<PromptCacheKeyMemo>>,
    model: Mutex<String>,
    models: Vec<String>,
    reasoning_effort: Mutex<Option<String>>,
}

impl OpenAiCompatibleProvider {
    /// Build a provider from the profile's endpoint, credential and catalog.
    ///
    /// `api_key` stays optional because `auth = "none"` endpoints legitimately
    /// have none. For `Bearer`/`Header` auth, a caller that requires a key must
    /// resolve it before construction; without one the request is sent
    /// unauthenticated and the endpoint's 401 is reported.
    pub fn new(
        base_url: impl Into<String>,
        auth: ChatAuth,
        api_key: Option<String>,
        default_model: impl Into<String>,
        models: Vec<String>,
    ) -> Result<Self> {
        let base_url = base_url.into();
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            anyhow::bail!("the openai-compatible profile has an empty base_url");
        }
        let lower = base_url.to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            anyhow::bail!("the openai-compatible base_url must be an absolute http:// or https:// URL");
        }
        let default_model = default_model.into();
        let default_model = default_model.trim().to_string();
        let models = normalize_models(models);
        let display_name = format!("OpenAI-compatible ({base_url})");

        Ok(Self {
            client: shared_http_client(),
            base_url,
            auth,
            api_key: api_key.filter(|key| !key.trim().is_empty()),
            headers: Vec::new(),
            extra_body: None,
            prompt_cache_key: None,
            prompt_cache_retention: None,
            profile_id: None,
            display_name,
            prompt_cache_memo: Mutex::new(None),
            model: Mutex::new(default_model),
            models,
            reasoning_effort: Mutex::new(None),
        })
    }

    /// Name shown to the user for this profile.
    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = display_name.into();
        self
    }

    /// Stable profile id, kept as the route detail and runtime identity.
    pub fn with_profile_id(mut self, profile_id: impl Into<String>) -> Self {
        self.profile_id = Some(profile_id.into());
        self
    }

    /// Extra HTTP headers sent with every request.
    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
        self
    }

    /// Extra top-level request-body fields; keys override generated ones.
    pub fn with_extra_body(mut self, extra_body: Value) -> Self {
        self.extra_body = Some(extra_body);
        self
    }

    /// Set the `prompt_cache_key` policy.
    ///
    /// `Some(true)` forces the field on, `Some(false)` disables it, and `None`
    /// keeps the default: the field is sent only to [`OFFICIAL_OPENAI_HOST`] and
    /// `openrouter.ai`, whose gateways document it.
    pub fn with_prompt_cache_key_policy(mut self, enabled: Option<bool>) -> Self {
        self.prompt_cache_key = enabled;
        self
    }

    /// Set the resolver for `prompt_cache_retention`.
    pub fn with_prompt_cache_retention_resolver(mut self, resolver: PromptCacheRetentionResolver) -> Self {
        self.prompt_cache_retention = Some(resolver);
        self
    }

    /// Initial reasoning effort for the selected model.
    pub fn with_reasoning_effort(self, effort: impl Into<String>) -> Self {
        let effort = effort.into();
        *self
            .reasoning_effort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = (!effort.trim().is_empty()).then_some(effort);
        self
    }

    /// The URL one streaming completion is POSTed to.
    pub fn endpoint(&self) -> String {
        if self.base_url.ends_with(&format!("/{}", CHAT_COMPLETIONS_PATH)) {
            self.base_url.clone()
        } else {
            format!("{}/{CHAT_COMPLETIONS_PATH}", self.base_url)
        }
    }

    /// The configured base URL, without the request path.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The profile id this runtime belongs to, when it has one.
    pub fn profile_id(&self) -> Option<&str> {
        self.profile_id.as_deref()
    }

    fn build_body(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        context: Option<&ProviderRequestContext>,
    ) -> Vec<u8> {
        let reasoning = self
            .reasoning_effort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut body = build_chat_completions_request(&self.model(), messages, tools, system, reasoning.as_deref());
        self.apply_prompt_cache(&mut body, context);
        if let Some(extra) = &self.extra_body
            && let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object())
        {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        serde_json::to_vec(&body).expect("a JSON object always serializes")
    }

    /// Add the prompt-cache fields this endpoint accepts.
    ///
    /// The key is derived from the already-built body, so a request pays no
    /// second build: the cacheable prefix is the system message plus the tool
    /// catalog, both read straight out of the body about to be sent. The
    /// profile's `extra_body` is merged afterwards, so it still wins on either
    /// field.
    fn apply_prompt_cache(&self, body: &mut Value, context: Option<&ProviderRequestContext>) {
        let host = base_url_host(&self.base_url);
        let key_enabled = self
            .prompt_cache_key
            .unwrap_or_else(|| host.as_deref().is_some_and(host_accepts_prompt_cache_key));
        if key_enabled && let Some(scope) = context.and_then(cache_scope) {
            let key = self.prompt_cache_key_for(scope, body);
            if let Some(object) = body.as_object_mut() {
                object.insert(PROMPT_CACHE_KEY_FIELD.to_string(), Value::String(key));
            }
        }
        if host.as_deref() == Some(OFFICIAL_OPENAI_HOST)
            && let Some(retention) = self
                .prompt_cache_retention
                .as_ref()
                .and_then(|resolve| resolve(&self.model()))
            && let Some(object) = body.as_object_mut()
        {
            object.insert(PROMPT_CACHE_RETENTION_FIELD.to_string(), Value::String(retention));
        }
    }

    /// The key for this request's cacheable prefix.
    ///
    /// The memo holds scope, system text and tool catalog beside the key, so a
    /// steady-state turn only compares those three and skips both the
    /// serialization of the tool catalog and the SHA-256 pass. The lock never
    /// spans an await: this runs inside the synchronous body build.
    fn prompt_cache_key_for(&self, scope: &str, body: &Value) -> String {
        let (system, tools) = prompt_cache_key_field_values(body);
        let mut memo = self
            .prompt_cache_memo
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match memo.as_mut() {
            Some(existing) if existing.matches(scope, system, tools) => existing.key().to_string(),
            Some(existing) => {
                existing.refresh(scope, system, tools);
                existing.key().to_string()
            }
            None => {
                let derived = PromptCacheKeyMemo::derive(scope, system, tools);
                let key = derived.key().to_string();
                *memo = Some(derived);
                key
            }
        }
    }

    /// Keys this provider derived so far; a steady-state turn must not grow it.
    #[cfg(test)]
    fn prompt_cache_key_derivations(&self) -> u64 {
        self.prompt_cache_memo
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(PromptCacheKeyMemo::derivations)
            .unwrap_or(0)
    }

    fn sanitize(&self, message: String) -> String {
        redact_secret(message, self.api_key.as_deref())
    }

    /// POST one streaming completion with the request's session identity.
    ///
    /// `context` only selects the cache policy; `None` builds exactly the body
    /// this provider sent before cache keys existed.
    async fn send(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
        context: Option<&ProviderRequestContext>,
    ) -> Result<EventStream> {
        if resume_session_id.is_some() {
            anyhow::bail!(
                "the openai-compatible provider cannot resume a provider-side session; sending a resumed request as a fresh one would be a silent substitution"
            );
        }

        let endpoint = self.endpoint();
        let body = self.build_body(messages, tools, system, context);
        let mut request = self
            .client
            .post(&endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .body(body);
        request = apply_auth(request, &self.auth, self.api_key.as_deref())?;
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }

        let response = send_with_initial_response_timeout(request, INITIAL_RESPONSE_TIMEOUT)
            .await
            .map_err(|error| anyhow::anyhow!(self.sanitize(transport_error_message(&endpoint, &error.to_string()))))?;

        let retry_after = horon_agent_provider_core::retry_after::retry_after(response.headers());
        let status = response.status();
        if !status.is_success() {
            let body = horon_agent_provider_core::http_error_body(response, "chat completions").await;
            return Err(http_response_error(
                &endpoint,
                status.as_u16(),
                retry_after,
                &body,
                self.api_key.as_deref(),
            ));
        }

        let redact: Option<Arc<str>> = self.api_key.as_deref().map(Arc::from);
        Ok(Box::pin(ChatCompletionsStream::new(response.bytes_stream(), redact)))
    }
}

fn normalize_models(models: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    models
        .into_iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty() && seen.insert(model.clone()))
        .collect()
}

/// The host of a validated `http(s)://` base URL, lowercased.
fn base_url_host(base_url: &str) -> Option<String> {
    let (_, rest) = base_url.split_once("://")?;
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']')?.0,
        None => authority.split(':').next()?,
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Whether a host is documented to understand `prompt_cache_key`.
fn host_accepts_prompt_cache_key(host: &str) -> bool {
    matches!(host, OFFICIAL_OPENAI_HOST | "openrouter.ai")
}

/// The cache scope a request shares with the rest of its lineage.
fn cache_scope(context: &ProviderRequestContext) -> Option<&str> {
    context
        .cache_root_id
        .as_deref()
        .or(context.session_id.as_deref())
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
}

/// The cacheable prefix read out of a request body about to be sent: the static
/// system text and the tool catalog. Conversation turns are deliberately absent
/// so a compaction that rewrites them leaves the key alone.
fn prompt_cache_key_field_values(body: &Value) -> (&str, Option<&Value>) {
    let system = body["messages"]
        .as_array()
        .and_then(|messages| messages.first())
        .filter(|first| first["role"].as_str() == Some("system"))
        .and_then(|first| first["content"].as_str())
        .unwrap_or("");
    (system, body.get("tools"))
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.send(messages, tools, system, resume_session_id, None).await
    }

    async fn complete_with_context(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
        context: &ProviderRequestContext,
    ) -> Result<EventStream> {
        self.send(messages, tools, system, resume_session_id, Some(context))
            .await
    }

    async fn complete_split_with_context(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        resume_session_id: Option<&str>,
        context: &ProviderRequestContext,
    ) -> Result<EventStream> {
        let dynamic_messages = messages_with_dynamic_system_context(messages, system_dynamic);
        self.send(
            &dynamic_messages,
            tools,
            system_static,
            resume_session_id,
            Some(context),
        )
        .await
    }

    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    /// Chat Completions `prompt_tokens` is the total input, already including
    /// the cached subset; adding the cached slice again would double count it.
    fn usage_is_inclusive(&self) -> Option<bool> {
        Some(true)
    }

    /// No native compaction artifact exists for a Chat Completions endpoint,
    /// but horon-agent's summary compaction asks the active model for a
    /// summary over the same provider, so manual and proactive compaction are
    /// available.
    fn supports_compaction(&self) -> bool {
        true
    }

    fn display_name(&self) -> String {
        self.display_name.clone()
    }

    fn model(&self) -> String {
        self.model
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        self.restore_model(model)
    }

    fn restore_model(&self, model: &str) -> Result<()> {
        let model = model.trim();
        if model.is_empty() {
            anyhow::bail!("the openai-compatible model cannot be empty");
        }
        *self.model.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = model.to_string();
        Ok(())
    }

    fn available_models_display(&self) -> Vec<String> {
        self.models.clone()
    }

    fn ready_models_display(&self) -> Option<Vec<String>> {
        Some(self.available_models_display())
    }

    fn available_models_for_switching(&self) -> Vec<String> {
        self.models.clone()
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        let detail = self.profile_id.clone().unwrap_or_else(|| self.base_url.clone());
        self.models
            .iter()
            .map(|model| ModelRoute {
                model: model.clone(),
                provider: self.display_name.clone(),
                api_method: "chat_completions".to_string(),
                available: true,
                detail: detail.clone(),
                cheapness: None,
                usage: None,
            })
            .collect()
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.reasoning_effort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
        let effort = effort.trim();
        if effort.is_empty() {
            anyhow::bail!("the openai-compatible reasoning effort cannot be empty");
        }
        *self
            .reasoning_effort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(effort.to_string());
        Ok(())
    }

    fn available_efforts(&self) -> Vec<&'static str> {
        horon_agent_provider_core::OPENAI_SELECTABLE_EFFORTS.to_vec()
    }

    fn supports_image_input(&self) -> bool {
        true
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone_for_fork())
    }
}

impl OpenAiCompatibleProvider {
    /// Copy this adapter for a fork.
    ///
    /// The HTTP client, endpoint and credential are shared; the routed model
    /// and per-request settings are this adapter's own, so a fork starts from
    /// the parent's current state and moves independently.
    fn clone_for_fork(&self) -> Self {
        Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            auth: self.auth.clone(),
            api_key: self.api_key.clone(),
            headers: self.headers.clone(),
            extra_body: self.extra_body.clone(),
            prompt_cache_key: self.prompt_cache_key,
            prompt_cache_retention: self.prompt_cache_retention.clone(),
            profile_id: self.profile_id.clone(),
            display_name: self.display_name.clone(),
            // A fork repeats the same prefix, but it is a separate lineage
            // object: starting empty keeps a parent's memo from being reused
            // with a scope it never derived.
            prompt_cache_memo: Mutex::new(None),
            model: Mutex::new(self.model()),
            models: self.models.clone(),
            reasoning_effort: Mutex::new(self.reasoning_effort()),
        }
    }
}
