//! Deterministic hashing helpers shared by the request-preparation paths.

use std::hash::{Hash, Hasher};

pub(crate) fn stable_hash_str(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn stable_hash_json<T: serde::Serialize + ?Sized>(value: &T) -> u64 {
    match serde_json::to_string(value) {
        Ok(encoded) => stable_hash_str(&encoded),
        Err(_) => stable_hash_str(""),
    }
}

pub(crate) fn stable_json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_string(value).map_or(0, |encoded| encoded.len())
}
