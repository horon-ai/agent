//! `list_models` requests that wait for the daemon's catalog.

use super::BridgeState;
use super::simple_kind::SimpleKind;
use horon_agent_harness_api::{ApiEvent, ServerFrame};

impl BridgeState {
    /// Answer every `list_models` waiting for a fresh catalog with the one the
    /// daemon just pushed.
    ///
    /// The daemon answers `get_model_catalog` with an `available_models_updated`
    /// push rather than a reply to the request's id, so a list that asked
    /// before the first catalog, or after a refresh, is answered here.
    pub(super) fn answer_waiting_model_lists(&mut self) -> Vec<ServerFrame> {
        let mut waiting = Vec::new();
        self.pending_simple.retain(|(_, api_id, kind)| {
            let models = *kind == SimpleKind::Models;
            if models {
                waiting.push(*api_id);
            }
            !models
        });
        // A bridge not yet attached answers for no session, as the cached
        // `list_models` reply does.
        let session_id = match &self.session_id {
            Some(session_id) => session_id.clone(),
            None => String::new(),
        };
        waiting
            .into_iter()
            .map(|api_id| {
                ServerFrame::reply(
                    api_id,
                    ApiEvent::Models {
                        session_id: session_id.clone(),
                        models: self.available_models.clone(),
                        current: self.current_model.clone(),
                    },
                )
            })
            .collect()
    }
}
