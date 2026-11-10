//! Identity replies: what `get_runtime_info` and a model switch report.
//!
//! Split from the request router because the field list is the contract a
//! client's status bar and model picker read, and keeping it in one place makes
//! a missing field visible instead of buried in a 4000-line translator.

use super::BridgeState;
use horon_agent_harness_api::ApiEvent;
use serde_json::Value;

impl BridgeState {
    pub(super) fn runtime_info(&self) -> ApiEvent {
        ApiEvent::RuntimeInfo {
            session_id: self.session_id.as_deref().unwrap_or("").to_string(),
            provider: self.current_provider.clone(),
            model: self.current_model.clone(),
            reasoning_effort: self.current_effort.clone(),
            auth_method: self.current_credential.clone(),
            fallback_models: self.fallback_models.clone(),
            serving_model: self.serving_model.clone(),
            routes: self.available_routes.clone(),
        }
    }

    pub(super) fn model_info(&self, session_id: String, event: &Value) -> ApiEvent {
        ApiEvent::ModelInfo {
            session_id,
            provider: event["provider_name"].as_str().map(str::to_string),
            model: event["provider_model"].as_str().map(str::to_string),
            reasoning_effort: event["reasoning_effort"]
                .as_str()
                .map(str::to_string)
                .or_else(|| self.current_effort.clone()),
            auth_method: self.current_credential.clone(),
        }
    }
}
