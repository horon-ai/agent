//! Public client operation options and response types.

use super::*;

/// Options for a one-shot turn.
#[derive(Default)]
pub struct RunOptions {
    pub images: Vec<(String, String)>,
    /// Called for every event of the turn, for progress rendering.
    #[allow(clippy::type_complexity)]
    pub on_event: Option<Box<dyn Fn(&ApiEvent) + Send>>,
    /// Auto-answer permission prompts. Only meaningful when the server
    /// advertises the `permissions` capability.
    pub auto_approve: bool,
}

/// Runtime identity and protocol metadata returned by [`HoronAgentClient::get_runtime_info`].
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeInfo {
    pub server: String,
    pub protocol_version: u32,
    pub capabilities: Vec<String>,
    pub healthy: bool,
    pub session_id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Reasoning effort, e.g. `high`, when the provider exposes it.
    pub reasoning_effort: Option<String>,
    /// Credential the session bills against (`oauth` or `api_key`).
    pub auth_method: Option<String>,
    /// Configured fallback model ids the active provider profile tries, in
    /// order. Empty when none is configured.
    pub fallback_models: Vec<String>,
    /// Model that produced the most recent response, which differs from
    /// [`Self::model`] after a fallback answered a turn.
    pub serving_model: Option<String>,
    pub providers: Vec<String>,
    pub routes: Vec<ModelRouteInfo>,
}

impl RuntimeInfo {
    /// Build the public view from a daemon `runtime_info` reply, handing the
    /// event back so a mismatched reply still reports what arrived.
    pub(super) fn from_reply(
        event: ApiEvent,
        server: String,
        capabilities: Vec<String>,
    ) -> std::result::Result<Self, ApiEvent> {
        let ApiEvent::RuntimeInfo {
            session_id,
            provider,
            model,
            reasoning_effort,
            auth_method,
            fallback_models,
            serving_model,
            routes,
        } = event
        else {
            return Err(event);
        };
        let mut providers = Vec::new();
        if let Some(provider) = provider.as_ref() {
            providers.push(provider.clone());
        }
        for route in &routes {
            if !providers.contains(&route.provider) {
                providers.push(route.provider.clone());
            }
        }
        Ok(Self {
            server,
            protocol_version: API_VERSION_MAJOR,
            capabilities,
            healthy: true,
            session_id,
            provider,
            model,
            reasoning_effort,
            auth_method,
            fallback_models,
            serving_model,
            providers,
            routes,
        })
    }
}

/// Content and truncation metadata returned by [`HoronAgentClient::read_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub size: u64,
    pub truncated: bool,
}

/// Optional constraints for [`HoronAgentClient::search_text`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchTextOptions {
    /// Restrict the search to this relative path under the session root.
    pub path: Option<String>,
    /// Maximum number of matches to return.
    pub limit: Option<u32>,
}

/// Safe filesystem metadata returned by [`HoronAgentClient::file_status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub path: String,
    pub exists: bool,
    pub kind: String,
    pub size: Option<u64>,
    pub modified_ms: Option<u64>,
}

/// What one turn produced.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TurnResult {
    /// None for natural completion. Failures still return Err and are also
    /// delivered to on_event as TurnStopped before the legacy Error event.
    pub stop_reason: Option<horon_agent_harness_api::TurnStopReason>,
    pub stop_message: Option<String>,
    /// All assistant text in the turn, including tool narration.
    pub text: String,
    /// Last completed assistant message, or aggregate text on older bridges.
    pub final_text: String,
    /// Framed messages. Empty when connected to an older, unframed bridge.
    pub messages: Vec<AssistantTextMessage>,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    /// Usage from the latest provider call in this turn, not a sum of calls.
    pub usage: Option<Usage>,
}

/// One assistant text message, excluding interleaved reasoning.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantTextMessage {
    /// Stream-local correlator, not a persisted history message id.
    pub message_id: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub output: String,
    pub error: Option<String>,
}

/// Provider-reported counters. Cache counters may be separate from input
/// (Anthropic) or a subset of it (OpenAI), so do not blindly add them together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read_input: Option<u64>,
    pub cache_creation_input: Option<u64>,
}
