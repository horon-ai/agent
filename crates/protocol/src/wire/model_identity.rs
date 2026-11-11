use super::*;

/// Model identity the daemon reports next to a catalog push.
///
/// `fallback_models` is the configured fallback list of the active named
/// provider profile, in the order it is tried. `serving_model` is the model
/// that produced the most recent stored assistant response; it differs from
/// the session's selected model after a model fallback, because a fallback is
/// restored at the start of the next turn. Both are omitted when unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeModelIdentity {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serving_model: Option<String>,
}
