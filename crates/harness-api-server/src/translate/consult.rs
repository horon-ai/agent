//! The legacy frame for a fork request.
//!
//! Kept out of `translate.rs` so the consult surface does not push the
//! translation table past its size budget. The normal fork and the consult fork
//! are the same legacy message; the daemon reads the two optional fields.

use serde_json::{Value, json};

/// One `split` frame for an API `fork_session` request.
pub(super) fn fork_frame(id: u64, request: &Value) -> Value {
    json!({
        "type": "split",
        "id": id,
        "consult": request["consult"].as_bool().unwrap_or(false),
        "boundary": request["boundary"].clone(),
    })
}
