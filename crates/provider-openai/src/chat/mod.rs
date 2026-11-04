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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use crate::chat::request::{CHAT_COMPLETIONS_PATH, build_chat_completions_request};
use crate::chat::stream::ChatCompletionsStream;
use crate::chat::transport::{
    ChatAuth, INITIAL_RESPONSE_TIMEOUT, apply_auth, http_response_error, redact_secret, transport_error_message,
};
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_message_types::{Message, ToolDefinition};
use horon_agent_provider_core::{
    EventStream, ModelRoute, Provider, shared_http_client, transport::send_with_initial_response_timeout,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// Stable, machine-facing provider name.
pub const PROVIDER_NAME: &str = "openai-compatible";

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
    profile_id: Option<String>,
    display_name: String,
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
            profile_id: None,
            display_name,
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

    fn build_body(&self, messages: &[Message], tools: &[ToolDefinition], system: &str) -> Vec<u8> {
        let reasoning = self
            .reasoning_effort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut body = build_chat_completions_request(&self.model(), messages, tools, system, reasoning.as_deref());
        if let Some(extra) = &self.extra_body
            && let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object())
        {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        serde_json::to_vec(&body).expect("a JSON object always serializes")
    }

    fn sanitize(&self, message: String) -> String {
        redact_secret(message, self.api_key.as_deref())
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

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        if resume_session_id.is_some() {
            anyhow::bail!(
                "the openai-compatible provider cannot resume a provider-side session; sending a resumed request as a fresh one would be a silent substitution"
            );
        }

        let endpoint = self.endpoint();
        let body = self.build_body(messages, tools, system);
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
        // The HTTP client, endpoint and credential are shared; the routed model
        // and per-request settings are this adapter's own, so a fork starts from
        // the parent's current state and moves independently.
        Arc::new(Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            auth: self.auth.clone(),
            api_key: self.api_key.clone(),
            headers: self.headers.clone(),
            extra_body: self.extra_body.clone(),
            profile_id: self.profile_id.clone(),
            display_name: self.display_name.clone(),
            model: Mutex::new(self.model()),
            models: self.models.clone(),
            reasoning_effort: Mutex::new(self.reasoning_effort()),
        })
    }
}
