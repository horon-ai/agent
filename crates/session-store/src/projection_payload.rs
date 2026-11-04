//! Typed decoding of resolved payloads, without changing their sealed envelopes.
use crate::*;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug)]
pub(crate) enum Payload {
    Created(SessionCreatedPayload),
    Appended(MessageAppendedPayload),
    Revised(MessageRevisedPayload),
    Metadata(MetadataChangedPayload),
    Compacted(CompactedPayload),
    Blocked(CompactionBlockedPayload),
    Archive(ArchiveChangedPayload),
    Auxiliary(AuxiliaryRecordedPayload),
    Unknown,
}

pub(crate) fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, StoreError> {
    serde_json::from_value(value.clone())
        .map_err(|error| StoreError::invalid_request(StoreErrorCode::InvalidRecord, error.to_string()))
}

impl Payload {
    pub(crate) fn parse(event: &EventEnvelope, value: &Value) -> Result<Self, StoreError> {
        Ok(match event.kind.known() {
            None => Self::Unknown,
            Some(KnownEventKind::SessionCreated) => {
                check_metadata(value)?;
                Self::Created(decode(value)?)
            }
            Some(KnownEventKind::MetadataChanged) => {
                check_metadata(value)?;
                Self::Metadata(decode(value)?)
            }
            Some(KnownEventKind::MessageAppended) => Self::Appended(decode(value)?),
            Some(KnownEventKind::MessageRevised) => Self::Revised(decode(value)?),
            Some(KnownEventKind::Compacted) => Self::Compacted(decode(value)?),
            Some(KnownEventKind::CompactionBlocked) => Self::Blocked(decode(value)?),
            Some(KnownEventKind::ArchiveChanged) => Self::Archive(decode(value)?),
            Some(KnownEventKind::AuxiliaryRecorded) => Self::Auxiliary(decode(value)?),
        })
    }
}

fn check_metadata(payload: &Value) -> Result<(), StoreError> {
    let metadata = payload
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or_else(|| StoreError::invalid_request(StoreErrorCode::InvalidRecord, "metadata must be an object"))?;
    for key in metadata.keys() {
        if !matches!(
            key.as_str(),
            "parent_id"
                | "title"
                | "custom_title"
                | "created_at"
                | "updated_at"
                | "system_prompt"
                | "model_usage_turn_id"
                | "provider_session_id"
                | "cache_root_id"
                | "provider_key"
                | "plugin_id"
                | "model"
                | "route_api_method"
                | "reasoning_effort"
                | "subagent_model"
                | "agent_model_overrides"
                | "improve_mode"
                | "autoreview_enabled"
                | "autojudge_enabled"
                | "is_canary"
                | "testing_build"
                | "working_dir"
                | "short_name"
                | "saved"
                | "save_label"
                | "migration_epoch"
                | "legacy_extensions"
        ) {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                format!("metadata field {key} must be preserved as inert legacy auxiliary/extension"),
            ));
        }
    }
    let parsed: SessionMetadata = decode(&Value::Object(metadata.clone()))?;
    for key in parsed.legacy_extensions.keys() {
        if matches!(
            key.as_str(),
            "messages"
                | "compaction"
                | "env_snapshots"
                | "memory_injections"
                | "replay_events"
                | "provider_messages_cache"
                | "provider_message_prefix_hashes_cache"
                | "provider_messages_cache_len"
                | "provider_messages_cache_mode"
                | "memory_profile_cache"
                | "memory_profile_dirty"
                | "persist_state"
        ) {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                format!("metadata extension {key} belongs to another event kind"),
            ));
        }
    }
    Ok(())
}
