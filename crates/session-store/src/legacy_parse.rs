//! Typed legacy parsing and raw metadata preservation.
use crate::legacy::corrupt;
use crate::*;
use chrono::{DateTime, Utc};
use horon_agent_session_types::StoredMessage;
use serde::Deserialize;
use serde_json::{Map, Value, json};
#[derive(Deserialize)]
pub(crate) struct Snapshot {
    pub(crate) id: String,
    pub(crate) messages: Vec<Value>,
    #[serde(flatten)]
    pub(crate) meta: Map<String, Value>,
}

#[derive(Deserialize)]
pub(crate) struct Entry {
    pub(crate) meta: Map<String, Value>,
    #[serde(default)]
    pub(crate) append_messages: Vec<Value>,
    #[serde(default)]
    pub(crate) append_env_snapshots: Vec<Value>,
    #[serde(default)]
    pub(crate) append_memory_injections: Vec<Value>,
    #[serde(default)]
    pub(crate) append_replay_events: Vec<Value>,
    #[serde(flatten)]
    pub(crate) extra: Map<String, Value>,
}

pub(crate) fn message(value: &Value) -> Result<StoredMessage, StoreError> {
    serde_json::from_value(value.clone()).map_err(|e| corrupt(e.to_string()))
}

pub(crate) fn snapshot(raw: &[u8], id: &SessionId) -> Result<Snapshot, StoreError> {
    let value: Snapshot = serde_json::from_slice(raw).map_err(|e| corrupt(e.to_string()))?;
    if value.id != id.as_str() {
        return Err(corrupt("snapshot session identity mismatch"));
    }
    for item in &value.messages {
        message(item)?;
    }
    metadata(&value.meta)?;
    Ok(value)
}

pub(crate) fn entry(value: Entry) -> Result<Entry, StoreError> {
    let updated = value
        .meta
        .get("updated_at")
        .ok_or_else(|| corrupt("journal time absent"))?;
    serde_json::from_value::<DateTime<Utc>>(updated.clone()).map_err(|e| corrupt(e.to_string()))?;
    for item in &value.append_messages {
        message(item)?;
    }
    Ok(value)
}

pub(crate) fn metadata(meta: &Map<String, Value>) -> Result<SessionMetadata, StoreError> {
    let mut input = meta.clone();
    input.entry("created_at").or_insert(json!(DateTime::<Utc>::UNIX_EPOCH));
    let created = input["created_at"].clone();
    input.entry("updated_at").or_insert(created);
    let mut result: SessionMetadata =
        serde_json::from_value(Value::Object(input)).map_err(|e| corrupt(e.to_string()))?;
    const KNOWN: &[&str] = &[
        "parent_id",
        "title",
        "custom_title",
        "created_at",
        "updated_at",
        "system_prompt",
        "model_usage_turn_id",
        "provider_session_id",
        "provider_key",
        "model",
        "route_api_method",
        "reasoning_effort",
        "reasoning_effort_pinned",
        "subagent_model",
        "agent_model_overrides",
        "improve_mode",
        "autoreview_enabled",
        "autojudge_enabled",
        "working_dir",
        "short_name",
        "saved",
        "save_label",
        "migration_epoch",
        "legacy_extensions",
    ];
    for (key, value) in meta {
        if !KNOWN.contains(&key.as_str()) && !AUX.contains(&key.as_str()) {
            result.legacy_extensions.insert(key.clone(), value.clone());
        }
    }
    Ok(result)
}

pub(crate) const AUX: &[&str] = &[
    "compaction",
    "env_snapshots",
    "memory_injections",
    "replay_events",
    "status",
    "last_pid",
    "last_active_at",
    "is_debug",
    "persist_state",
    "provider_messages_cache",
    "provider_message_prefix_hashes_cache",
    "provider_messages_cache_len",
    "provider_messages_cache_mode",
    "memory_profile_cache",
    "memory_profile_dirty",
];

pub(crate) fn salvage(text: &str) -> Vec<Entry> {
    for (start, _) in text.match_indices("{\"meta\":") {
        let mut found = Vec::new();
        for item in serde_json::Deserializer::from_str(&text[start..]).into_iter::<Entry>() {
            match item.map_err(|e| corrupt(e.to_string())).and_then(entry) {
                Ok(e) => found.push(e),
                Err(_) => break,
            }
        }
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}
