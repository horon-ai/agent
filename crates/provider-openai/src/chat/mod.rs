//! Direct OpenAI-compatible Chat Completions provider.
//!
//! This is the model path that needs no plugin runtime: the agent's canonical
//! conversation is translated to the Chat Completions wire format
//! ([`request`]), streamed chunks are translated back into the shared
//! [`horon_agent_message_types::StreamEvent`] vocabulary ([`stream`]), and one
//! narrow transport adds the credential and error classification ([`transport`]).

mod model_list;
pub mod request;
pub mod schema_recovery;
pub mod stream;
pub mod transport;

mod prompt_cache_memo;
mod stream_timeout;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "prompt_cache_tests.rs"]
mod prompt_cache_tests;

#[cfg(test)]
#[path = "degradation_tests.rs"]
mod degradation_tests;

#[cfg(test)]
#[path = "schema_recovery_tests.rs"]
mod schema_recovery_tests;

use crate::chat::prompt_cache_memo::PromptCacheKeyMemo;
use crate::chat::request::build_chat_completions_request_reporting;
use crate::chat::stream::ChatCompletionsStream;
use crate::chat::transport::{
    ChatAuth, INITIAL_RESPONSE_TIMEOUT, apply_auth, http_response_error_labeled, redact_secret,
    transport_error_message_labeled,
};
use crate::wire::WireProtocol;
use crate::wire::anthropic::{AnthropicParser, build_messages_request};
use crate::wire::gemini::{GeminiParser, build_generate_content_request};
use crate::wire::responses::{ResponsesParser, build_responses_request};
use crate::wire::sse::WireSseStream;
use crate::wire::turns;
use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use horon_agent_message_types::{Message, ToolDefinition, messages_with_dynamic_system_context};
use horon_agent_provider_core::{
    EventStream, ModelRoute, Provider, ProviderRequestContext, shared_http_client,
    transport::send_with_initial_response_timeout,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout_at};

/// Stable, machine-facing provider name.
pub const PROVIDER_NAME: &str = "openai-compatible";

/// Reserved per-request field pinning a request to one prompt cache slot.
const PROMPT_CACHE_KEY_FIELD: &str = "prompt_cache_key";

/// Reserved per-request field selecting an extended prompt cache retention.
const PROMPT_CACHE_RETENTION_FIELD: &str = "prompt_cache_retention";

/// The official OpenAI endpoint, the only host that receives the retention field.
const OFFICIAL_OPENAI_HOST: &str = "api.openai.com";

/// Prefix of the identity stamped on Anthropic native thinking blocks.
const ANTHROPIC_NATIVE_PREFIX: &str = "anthropic-compatible";

/// Effort levels the Gemini `generateContent` wire exposes.
const GEMINI_SELECTABLE_EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh"];

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
    service_tier: Mutex<Option<&'static str>>,
    /// Reasoning effort each model declares in its profile entry. Selecting a
    /// model that appears here sets `reasoning_effort` to the declared level,
    /// before the first request, unless a later explicit call overrides it.
    model_reasoning_efforts: BTreeMap<String, String>,
    /// Which request and response shape this profile speaks.
    wire: WireProtocol,
    stream_first_event_timeout: Option<Duration>,
    stream_idle_timeout: Option<Duration>,
    request_slots: Option<Arc<Semaphore>>,
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
            service_tier: Mutex::new(None),
            model_reasoning_efforts: BTreeMap::new(),
            wire: WireProtocol::default(),
            stream_first_event_timeout: None,
            stream_idle_timeout: None,
            request_slots: None,
        })
    }

    /// Set independent limits for startup and silence during a streamed response.
    pub fn with_stream_timeouts(mut self, first_event: Option<Duration>, idle: Option<Duration>) -> Self {
        self.stream_first_event_timeout = first_event;
        self.stream_idle_timeout = idle;
        self
    }

    /// Share a named profile's request limit across its runtimes and forks.
    pub fn with_request_slots(mut self, slots: Option<Arc<Semaphore>>) -> Self {
        self.request_slots = slots;
        self
    }

    /// Select the request and response shape this profile speaks.
    ///
    /// The default is Chat Completions, so a profile written before the other
    /// wires existed keeps sending exactly the bytes it always did.
    pub fn with_wire(mut self, wire: WireProtocol) -> Self {
        self.wire = wire;
        self
    }

    /// The wire this profile speaks.
    pub fn wire(&self) -> WireProtocol {
        self.wire
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

    /// Reasoning effort each model declares in its profile entry.
    ///
    /// The map is applied by model, so the level of the model active now (the
    /// constructor's default model) takes effect immediately, and every later
    /// [`Provider::set_model`] applies the level of the model it selects. A
    /// model with no entry leaves the current effort alone, and an explicit
    /// [`Provider::set_reasoning_effort`] still overrides the level until the
    /// model changes.
    pub fn with_model_reasoning_efforts(mut self, efforts: BTreeMap<String, String>) -> Self {
        self.model_reasoning_efforts = efforts
            .into_iter()
            .filter_map(|(model, effort)| {
                let model = model.trim().to_string();
                let effort = effort.trim().to_string();
                (!model.is_empty() && !effort.is_empty()).then_some((model, effort))
            })
            .collect();
        let current = self.model();
        self.apply_configured_effort(&current);
        self
    }

    /// Adopt the reasoning effort the profile declares for `model`, if any.
    ///
    /// Called after the routed model changes, so the level the user configured
    /// for the new model is what the session reports and sends. A model with no
    /// entry keeps whatever effort is in force, which is what lets an explicit
    /// `/effort` survive a switch to a model that names none.
    fn apply_configured_effort(&self, model: &str) {
        let Some(effort) = self.model_reasoning_efforts.get(model).cloned() else {
            return;
        };
        *self
            .reasoning_effort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(effort);
    }

    /// The URL one streaming completion is POSTed to.
    pub fn endpoint(&self) -> String {
        match self.wire {
            // Only this wire's path names the model, so the other three do not
            // pay for reading it.
            WireProtocol::GeminiGenerateContent => self.wire.endpoint(&self.base_url, &self.model()),
            _ => self.wire.endpoint(&self.base_url, ""),
        }
    }

    /// Fetch model IDs on explicit request using this profile's HTTP authentication.
    pub async fn fetch_models(&self) -> Result<Vec<String>> {
        model_list::fetch(self).await
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
        let configured = self
            .reasoning_effort
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let reasoning = match context.and_then(|c| c.reasoning_effort.as_deref()) {
            Some(effort) if !effort.is_empty() => Some(effort.to_string()),
            _ => configured,
        };
        // A request that narrowed the tool catalog leaves a receipt per loss so
        // the turn that carried it can record what the model never saw. The
        // request itself is built the same way either way, and a session id is
        // the only thing that can key the receipt, so a contextless call sends
        // exactly the bytes it sent before receipts existed.
        let session_id = context.and_then(|c| c.session_id.as_deref());
        let mut receipts = Vec::new();
        let model = self.model();
        let mut body = match self.wire {
            WireProtocol::ChatCompletions => build_chat_completions_request_reporting(
                &model,
                messages,
                tools,
                system,
                reasoning.as_deref(),
                session_id.map(|_| &mut receipts),
            ),
            WireProtocol::Responses => build_responses_request(
                &model,
                messages,
                tools,
                system,
                reasoning.as_deref(),
                session_id.map(|_| &mut receipts),
            ),
            // The Anthropic and Gemini endpoints reject a history whose turns do
            // not alternate or whose calls and results do not pair up, so both
            // read the normalized history instead of the stored one.
            WireProtocol::AnthropicMessages => build_messages_request(
                &model,
                &turns::normalize(messages),
                tools,
                system,
                reasoning.as_deref(),
                &self.native_tag(&model),
                session_id.map(|_| &mut receipts),
            ),
            WireProtocol::GeminiGenerateContent => build_generate_content_request(
                &model,
                &turns::normalize(messages),
                tools,
                system,
                reasoning.as_deref(),
                session_id.map(|_| &mut receipts),
            ),
        };
        if let Some(session_id) = session_id {
            horon_agent_provider_core::degradation::record(session_id, &receipts);
        }
        self.apply_prompt_cache(&mut body, context);
        if let Some(extra) = &self.extra_body
            && let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object())
        {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        if let Some(tier) = self.service_tier_override()
            && let Some(body) = body.as_object_mut()
        {
            if tier == "off" {
                body.remove("service_tier");
            } else {
                body.insert("service_tier".to_string(), Value::from(tier));
            }
        }
        serde_json::to_vec(&body).expect("a JSON object always serializes")
    }

    fn service_tier_override(&self) -> Option<&'static str> {
        *self
            .service_tier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The identity an Anthropic native thinking block is stamped with.
    ///
    /// A stored block is replayed only into the endpoint that produced it, so
    /// the tag names the profile when it has one and the host otherwise.
    fn native_tag(&self, model: &str) -> String {
        let host = base_url_host(&self.base_url);
        let identity = self.profile_id.as_deref().or(host.as_deref()).unwrap_or("");
        format!("{ANTHROPIC_NATIVE_PREFIX}:{identity}/{model}")
    }

    /// Add the prompt-cache fields this endpoint accepts.
    ///
    /// The key is derived from the already-built body, so a request pays no
    /// second build: the cacheable prefix is the system text plus the tool
    /// catalog, both read straight out of the body about to be sent. The
    /// profile's `extra_body` is merged afterwards, so it still wins on either
    /// field.
    ///
    /// Only the two OpenAI wires carry these fields: the Anthropic and Gemini
    /// endpoints answer an unknown top-level field with a rejection, so a
    /// request that pinned a cache slot would fail instead of running.
    fn apply_prompt_cache(&self, body: &mut Value, context: Option<&ProviderRequestContext>) {
        if !matches!(self.wire, WireProtocol::ChatCompletions | WireProtocol::Responses) {
            return;
        }
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
        let (system, tools) = prompt_cache_key_field_values(self.wire, body);
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

        // Dropping this future while queued removes the waiter before any POST.
        let permit = match &self.request_slots {
            Some(slots) => Some(Arc::clone(slots).acquire_owned().await?),
            None => None,
        };
        let endpoint = self.endpoint();
        let label = self.wire.label();
        let mut attempts = 0u8;
        loop {
            // `Bytes` so the failed-request bytes stay available to the
            // classifier without copying what was just serialized.
            let body: bytes::Bytes = self.build_body(messages, tools, system, context).into();
            let first_deadline = self.stream_first_event_timeout.map(|budget| Instant::now() + budget);
            let post = self.post_stream(&endpoint, body.clone());
            let response = match first_deadline {
                Some(deadline) => timeout_at(deadline, post)
                    .await
                    .map_err(|_| stream_timeout::first_event_error())??,
                None => post.await?,
            };

            let retry_after = horon_agent_provider_core::retry_after::retry_after(response.headers());
            let status = response.status();
            if status.is_success() {
                let redact: Option<Arc<str>> = self.api_key.as_deref().map(Arc::from);
                let stream = stream_timeout::apply(
                    self.stream_for(response, redact),
                    first_deadline,
                    self.stream_idle_timeout,
                );
                return Ok(match permit {
                    Some(permit) => Box::pin(futures::stream::unfold(
                        (stream, permit),
                        |(mut stream, permit)| async move { stream.next().await.map(|event| (event, (stream, permit))) },
                    )),
                    None => stream,
                });
            }

            let error_body = horon_agent_provider_core::http_error_body(response, self.wire.error_context()).await;
            let hint = match schema_recovery::classify_for(status.as_u16(), &error_body, &body, self.wire.dialect()) {
                schema_recovery::SchemaRecovery::Retry { description }
                    if attempts < schema_recovery::MAX_SCHEMA_RECOVERY_ATTEMPTS =>
                {
                    attempts += 1;
                    horon_agent_logging::warn(&self.sanitize(format!(
                        "{label} provider at {endpoint} rejected a tool schema: {description}"
                    )));
                    // Rebuilding the body re-runs normalization, which now drops
                    // the construct just learned; any other prevention drop is
                    // recorded on the request's session again, deduplicated.
                    continue;
                }
                schema_recovery::SchemaRecovery::Retry { description } => Some(format!(
                    "{description}; the schema recovery budget of {} requests is exhausted",
                    schema_recovery::MAX_SCHEMA_RECOVERY_ATTEMPTS
                )),
                schema_recovery::SchemaRecovery::Stop { hint } => hint,
            };
            return Err(http_response_error_labeled(
                label,
                &endpoint,
                status.as_u16(),
                retry_after,
                &error_body,
                hint.as_deref(),
                self.api_key.as_deref(),
            ));
        }
    }

    /// Wrap the response body in the stream parser this wire reads.
    fn stream_for(&self, response: reqwest::Response, redact: Option<Arc<str>>) -> EventStream {
        let label = self.wire.label();
        let body = response.bytes_stream();
        match self.wire {
            WireProtocol::ChatCompletions => Box::pin(ChatCompletionsStream::new(body, redact)),
            WireProtocol::Responses => Box::pin(WireSseStream::new(body, ResponsesParser::new(), label, redact)),
            WireProtocol::AnthropicMessages => Box::pin(WireSseStream::new(
                body,
                AnthropicParser::new(self.native_tag(&self.model())),
                label,
                redact,
            )),
            WireProtocol::GeminiGenerateContent => {
                Box::pin(WireSseStream::new(body, GeminiParser::new(), label, redact))
            }
        }
    }

    /// POST one streaming request and wait for the initial response.
    async fn post_stream(&self, endpoint: &str, body: bytes::Bytes) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .body(body);
        request = apply_auth(request, &self.auth, self.api_key.as_deref())?;
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        send_with_initial_response_timeout(request, INITIAL_RESPONSE_TIMEOUT)
            .await
            .map_err(|error| {
                anyhow::anyhow!(self.sanitize(transport_error_message_labeled(
                    self.wire.label(),
                    endpoint,
                    &error.to_string()
                )))
            })
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

/// The effort levels an Anthropic endpoint accepts for `model`.
///
/// The base ladder is the three levels every Claude generation understands;
/// `xhigh` and `max` are added only where the model's caps say the API takes
/// them, and a model with no reasoning control at all reports an empty list
/// rather than levels its request would have to drop.
fn anthropic_efforts(model: &str) -> Vec<&'static str> {
    let caps = horon_agent_provider_core::anthropic_reasoning_caps(model);
    if !caps.output_effort && !caps.manual_thinking {
        return Vec::new();
    }
    let mut efforts = vec!["low", "medium", "high"];
    if caps.xhigh_effort {
        efforts.push("xhigh");
    }
    if caps.max_effort {
        efforts.push("max");
    }
    efforts
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
///
/// Chat Completions carries the system text as the first `messages` entry and
/// Responses carries it as `instructions`; a body of either shape yields the
/// same key material.
fn prompt_cache_key_field_values(wire: WireProtocol, body: &Value) -> (&str, Option<&Value>) {
    let system = match wire {
        WireProtocol::Responses => body["instructions"].as_str().unwrap_or(""),
        _ => body["messages"]
            .as_array()
            .and_then(|messages| messages.first())
            .filter(|first| first["role"].as_str() == Some("system"))
            .and_then(|first| first["content"].as_str())
            .unwrap_or(""),
    };
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

    /// Whether the reported input tokens already include the cached subset.
    ///
    /// Chat Completions, Responses and Gemini report a total, so adding the
    /// cached slice again would double count it; Anthropic reports the cache
    /// reads and writes separately and the caller has to add them up.
    fn usage_is_inclusive(&self) -> Option<bool> {
        Some(self.wire.usage_is_inclusive())
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
        // A resumed session re-applies the effort it recorded after this, so
        // the configured level only seeds a session with no recorded effort.
        self.apply_configured_effort(model);
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
                api_method: self.wire.api_method().to_string(),
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

    /// The effort levels this wire and model accept.
    ///
    /// The two OpenAI wires take the shared OpenAI ladder. Anthropic's ladder is
    /// derived from what the model's caps allow, so a model with no reasoning
    /// control reports none instead of offering levels its request would refuse.
    /// Gemini has its own five-level scale.
    fn available_efforts(&self) -> Vec<&'static str> {
        match self.wire {
            WireProtocol::ChatCompletions | WireProtocol::Responses => {
                horon_agent_provider_core::OPENAI_SELECTABLE_EFFORTS.to_vec()
            }
            WireProtocol::AnthropicMessages => anthropic_efforts(&self.model()),
            WireProtocol::GeminiGenerateContent => GEMINI_SELECTABLE_EFFORTS.to_vec(),
        }
    }

    fn service_tier(&self) -> Option<String> {
        self.service_tier_override()
            .filter(|tier| *tier != "off")
            .map(str::to_owned)
    }

    fn set_service_tier(&self, tier: &str) -> Result<()> {
        let allowed = self.available_service_tiers();
        if allowed.is_empty() {
            anyhow::bail!("service tiers are only supported on OpenAI Chat Completions and Responses");
        }
        let tier = if tier == "fast" { "priority" } else { tier };
        let Some(tier) = allowed.into_iter().find(|allowed| *allowed == tier) else {
            anyhow::bail!("unsupported service tier {tier:?} for model {}", self.model());
        };
        *self
            .service_tier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tier);
        Ok(())
    }

    fn available_service_tiers(&self) -> Vec<&'static str> {
        if matches!(self.wire, WireProtocol::ChatCompletions | WireProtocol::Responses) {
            horon_agent_provider_core::service_tier::speed_tier_ladder(Some("openai"), Some(&self.model()))
        } else {
            Vec::new()
        }
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
            service_tier: Mutex::new(self.service_tier_override()),
            model_reasoning_efforts: self.model_reasoning_efforts.clone(),
            wire: self.wire,
            stream_first_event_timeout: self.stream_first_event_timeout,
            stream_idle_timeout: self.stream_idle_timeout,
            request_slots: self.request_slots.clone(),
        }
    }
}
