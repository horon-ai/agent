//! Stable, deterministic hashing helpers for request-shape accounting.

use std::hash::{Hash, Hasher};

pub(super) fn stable_hash_str(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Serialize for request-shape accounting, which is best-effort by design: a
/// value that cannot be encoded contributes an empty string instead of failing
/// the turn that is merely measuring its own payload.
fn stable_json_string<T: serde::Serialize + ?Sized>(value: &T) -> String {
    match serde_json::to_string(value) {
        Ok(encoded) => encoded,
        Err(_) => String::new(),
    }
}

pub(super) fn stable_hash_json<T: serde::Serialize + ?Sized>(value: &T) -> u64 {
    stable_hash_str(&stable_json_string(value))
}

pub(super) fn stable_json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    stable_json_string(value).len()
}

/// Hash the cache-relevant projection, not the raw `Message`.
///
/// Raw hashing keys off non-transmitted metadata (timestamp,
/// `tool_duration_ms`, `ReasoningTrace` blocks, `cache_control` markers), which
/// triggers spurious `harness:_prefix_changed` KV-cache miss reports when the
/// same message is re-serialized with backfilled metadata on the next turn.
pub(super) fn projected_message_hash(projected: &serde_json::Value) -> u64 {
    let encoded = stable_json_string(projected);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&encoded, &mut hasher);
    std::hash::Hasher::finish(&hasher)
}
