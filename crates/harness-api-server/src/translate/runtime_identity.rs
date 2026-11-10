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
            fallback_models: self.fallback_models.as_deref().unwrap_or(&[]).to_vec(),
            serving_model: self.serving_model.clone(),
            routes: self.available_routes.clone(),
        }
    }

    /// Whether the cached identity can answer `get_runtime_info` without a
    /// daemon round trip.
    ///
    /// That holds once the catalog and one identity report have arrived and no
    /// fallback list is configured: the serving model can then only be the
    /// session's own model, which the catalog pushes keep current. A configured
    /// list can move the serving model without any push, so the reply is asked
    /// for again, as it is before the first catalog.
    pub(super) fn runtime_info_is_current(&self) -> bool {
        self.model_catalog_loaded && self.fallback_models.as_ref().is_some_and(Vec::is_empty)
    }

    /// Record the model identity a catalog push carries.
    ///
    /// A push without `identity` keeps the last known values: most pushes carry
    /// none. Inside an identity the daemon omits an empty fallback list, so a
    /// missing `fallback_models` there means "none configured" and clears a
    /// list that was removed from the profile.
    pub(super) fn note_identity(&mut self, event: &Value) {
        let Some(identity) = event.get("identity") else {
            return;
        };
        let fallbacks = identity["fallback_models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect();
        self.fallback_models = Some(fallbacks);
        if identity.get("serving_model").is_some() {
            self.serving_model = identity["serving_model"].as_str().map(str::to_string);
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
