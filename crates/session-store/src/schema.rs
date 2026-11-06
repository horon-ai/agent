//! Event envelope, payload schemas, canonical JSON hashing, and identity rules.
//!
//! Stored record ordering: one JSONL line is one canonical JSON object built from
//! the envelope fields plus the event body. The canonical form is a recursive key
//! sort by UTF-8 byte order over a serde_json::Value, never struct declaration
//! order, and never a RawValue pass-through: every stored byte is produced by
//! canonicalize_value. Numbers keep serde_json's own representation (1 stays 1,
//! 1.0 stays 1.0, a full-range u64 keeps all its digits).
//!
//! Finalize before hashing: blob externalization is decided before any hash
//! exists. EventDraft::seal canonicalizes the payload, then either keeps it
//! inline or replaces it with a payload_ref, then hashes that final
//! representation. An inline record and a blob-reference record are therefore
//! different stored records with different content_hash values, and neither is
//! ever mutated afterwards. Hydrating a blob produces a separate HydratedEvent
//! view while the stored canonical bytes and hash stay untouched.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use horon_agent_session_types::{EnvSnapshot, StoredMemoryInjection, StoredMessage};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{StoreError, StoreErrorCode};

/// The schema version this build writes.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Per-record byte limit. A larger payload is externalized to a blob.
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Maximum session id length, excluding the leading character.
pub const MAX_SESSION_ID_LEN: usize = 160;

/// Maximum restoration-manifest entries retained in a compacted payload.
pub const MAX_RESTORATION_ENTRIES: usize = 10;

/// The fixed fallback timestamp for migrated events with no recorded time.
pub const LEGACY_EPOCH_RFC3339: &str = "1970-01-01T00:00:00.000000000Z";

/// Fixed namespace for deterministic legacy origin ids. Every migrated event's
/// identity depends on this value, so it must never change.
pub const LEGACY_ORIGIN_NAMESPACE: Uuid = uuid::uuid!("8f14e45f-ceea-4a1b-9c3d-1a2b3c4d5e6f");

/// Fixed namespace for deterministic legacy event ids. Must never change.
pub const LEGACY_EVENT_NAMESPACE: Uuid = uuid::uuid!("3c9a1b7e-5d42-4f88-b6a1-0e7d2c4b8f31");

/// A session id from the safe identity grammar: an ASCII letter or digit,
/// followed by up to 160 letters, digits, dot, dash or underscore, with no dot
/// path, no trailing dot, no Windows reserved spelling, and no path separator.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Validate and wrap a session id.
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, StoreError> {
        let raw = raw.as_ref();
        validate_session_id_text(raw)?;
        Ok(Self(raw.to_string()))
    }

    /// The validated id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A uuid-shaped event identity, normalized to lowercase hyphenated form.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(String);

impl EventId {
    /// Parse an event id, normalizing case to canonical lowercase hyphenated form.
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, StoreError> {
        Ok(Self(normalize_uuid_str(raw.as_ref())?))
    }

    /// A fresh random (v4) event id for live appends.
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// The canonical string form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The parsed uuid.
    pub fn as_uuid(&self) -> Uuid {
        Uuid::parse_str(&self.0).unwrap_or(Uuid::nil())
    }
}

/// A uuid-shaped origin identity: a per-install id, or a deterministic migrated id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OriginId(String);

impl OriginId {
    /// Parse an origin id, normalizing case to canonical lowercase hyphenated form.
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, StoreError> {
        Ok(Self(normalize_uuid_str(raw.as_ref())?))
    }

    /// A fresh random (v4) origin id for a live install.
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// The canonical string form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The parsed uuid.
    pub fn as_uuid(&self) -> Uuid {
        Uuid::parse_str(&self.0).unwrap_or(Uuid::nil())
    }
}

/// An event kind this build can project. Unknown kinds are preserved verbatim.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum KnownEventKind {
    SessionCreated,
    MessageAppended,
    MessageRevised,
    MetadataChanged,
    Compacted,
    CompactionBlocked,
    ArchiveChanged,
    AuxiliaryRecorded,
}

impl KnownEventKind {
    /// Every kind this build can project.
    pub const ALL: [KnownEventKind; 8] = [
        Self::SessionCreated,
        Self::MessageAppended,
        Self::MessageRevised,
        Self::MetadataChanged,
        Self::Compacted,
        Self::CompactionBlocked,
        Self::ArchiveChanged,
        Self::AuxiliaryRecorded,
    ];

    /// Canonical snake_case kind name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionCreated => "session_created",
            Self::MessageAppended => "message_appended",
            Self::MessageRevised => "message_revised",
            Self::MetadataChanged => "metadata_changed",
            Self::Compacted => "compacted",
            Self::CompactionBlocked => "compaction_blocked",
            Self::ArchiveChanged => "archive_changed",
            Self::AuxiliaryRecorded => "auxiliary_recorded",
        }
    }

    /// Parse a known kind name.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }
}

/// A kind string. Unknown values survive a roundtrip unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventKind(String);

impl EventKind {
    /// Wrap a known kind.
    pub fn new(kind: KnownEventKind) -> Self {
        Self(kind.as_str().to_string())
    }

    /// Wrap a raw kind string.
    pub fn parse(raw: impl AsRef<str>) -> Self {
        Self(raw.as_ref().to_string())
    }

    /// The known kind, if this build understands it.
    pub fn known(&self) -> Option<KnownEventKind> {
        KnownEventKind::parse(&self.0)
    }

    /// The raw kind string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a blob's bytes are encoded.
#[derive(Clone, Debug, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadEncoding {
    /// Canonical JSON bytes as produced by canonical_json.
    #[serde(rename = "canonical-json")]
    CanonicalJson,
    /// An encoding this build cannot decode; reading refuses it.
    #[serde(other)]
    Unknown,
}

/// A reference to an externalized payload blob.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadRef {
    /// Lowercase hex SHA-256 of the canonical blob bytes.
    pub sha256: String,
    /// Exact blob length in bytes.
    pub size: u64,
    /// Blob encoding.
    pub encoding: PayloadEncoding,
}

/// One stored event record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Schema version of this record.
    pub schema_version: u32,
    pub session_id: SessionId,
    pub event_id: EventId,
    /// Absent only for a session root (SessionCreated) and an explicit reset root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<EventId>,
    pub origin_id: OriginId,
    /// Display-only event time. It is hashed, so it is part of identity.
    pub timestamp: DateTime<Utc>,
    pub kind: EventKind,
    /// Inline payload, mutually exclusive with payload_ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    /// Externalized payload reference, mutually exclusive with payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_ref: Option<PayloadRef>,
    /// Canonical SHA-256 over every other field.
    pub content_hash: String,
}

/// Which representation carries this event's body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventBody {
    Inline,
    Blob,
}

/// A draft event before finalization.
#[derive(Clone, Debug, PartialEq)]
pub struct EventDraft {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub event_id: EventId,
    pub parent_event_id: Option<EventId>,
    pub origin_id: OriginId,
    pub timestamp: DateTime<Utc>,
    pub kind: EventKind,
    pub payload: Value,
}

impl EventDraft {
    /// Build a draft at the current schema version.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        event_id: EventId,
        parent_event_id: Option<EventId>,
        origin_id: OriginId,
        timestamp: DateTime<Utc>,
        kind: EventKind,
        payload: Value,
    ) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            session_id,
            event_id,
            parent_event_id,
            origin_id,
            timestamp,
            kind,
            payload,
        }
    }

    /// Override the schema version, for boundary tests and refusal paths.
    pub fn with_schema_version(mut self, version: u32) -> Self {
        self.schema_version = version;
        self
    }

    /// Finalize at the default per-record limit.
    pub fn seal(self) -> Result<SealedEvent, StoreError> {
        self.seal_with_limit(MAX_RECORD_BYTES)
    }

    /// Finalize, externalizing the payload when the inline record would exceed limit.
    pub fn seal_with_limit(self, limit: usize) -> Result<SealedEvent, StoreError> {
        if self.schema_version != EVENT_SCHEMA_VERSION {
            return Err(StoreError::invalid_request(
                StoreErrorCode::UnsupportedVersion,
                format!(
                    "cannot write schema_version {}; this build writes {}",
                    self.schema_version, EVENT_SCHEMA_VERSION
                ),
            ));
        }
        let payload = canonicalize_value(&self.payload);
        let inline = build_envelope(&self, Some(payload.clone()), None)?;
        inline.validate()?;
        let inline_line = canonical_line(&inline)?;
        if inline_line.len() <= limit {
            return Ok(SealedEvent::Inline(inline));
        }
        let blob = canonical_json(&payload)?.into_bytes();
        let payload_ref = PayloadRef {
            sha256: sha256_hex(&blob),
            size: blob.len() as u64,
            encoding: PayloadEncoding::CanonicalJson,
        };
        let external = build_envelope(&self, None, Some(payload_ref.clone()))?;
        external.validate()?;
        let external_line = canonical_line(&external)?;
        if external_line.len() > limit {
            return Err(StoreError::invalid_request(
                StoreErrorCode::RecordTooLarge,
                format!(
                    "record needs {} bytes with an externalized payload, limit is {limit}",
                    external_line.len()
                ),
            ));
        }
        Ok(SealedEvent::Blob {
            envelope: external,
            blob,
            payload_ref,
        })
    }
}

/// A finalized event: inline, or with its payload externalized to a blob.
#[derive(Clone, Debug, PartialEq)]
pub enum SealedEvent {
    Inline(EventEnvelope),
    Blob {
        envelope: EventEnvelope,
        blob: Vec<u8>,
        payload_ref: PayloadRef,
    },
}

impl SealedEvent {
    /// The stored envelope, in either representation.
    pub fn envelope(&self) -> &EventEnvelope {
        match self {
            Self::Inline(envelope) => envelope,
            Self::Blob { envelope, .. } => envelope,
        }
    }

    /// The externalized blob bytes, if the payload was externalized.
    pub fn blob(&self) -> Option<&[u8]> {
        match self {
            Self::Inline(_) => None,
            Self::Blob { blob, .. } => Some(blob),
        }
    }

    /// Split into the stored envelope and optional blob bytes.
    pub fn into_parts(self) -> (EventEnvelope, Option<Vec<u8>>) {
        match self {
            Self::Inline(envelope) => (envelope, None),
            Self::Blob { envelope, blob, .. } => (envelope, Some(blob)),
        }
    }
}

impl EventEnvelope {
    /// Which representation carries the body.
    pub fn body(&self) -> Result<EventBody, StoreError> {
        match (&self.payload, &self.payload_ref) {
            (Some(_), None) => Ok(EventBody::Inline),
            (None, Some(_)) => Ok(EventBody::Blob),
            (Some(_), Some(_)) => Err(StoreError::invalid_request(
                StoreErrorCode::PayloadRefConflict,
                "record carries both payload and payload_ref",
            )),
            (None, None) => Err(StoreError::invalid_request(
                StoreErrorCode::PayloadRefMissing,
                "record carries neither payload nor payload_ref",
            )),
        }
    }

    /// Structural, version, and boundary validation.
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.schema_version != EVENT_SCHEMA_VERSION {
            return Err(StoreError::invalid_request(
                StoreErrorCode::UnsupportedVersion,
                format!("unsupported schema_version {}", self.schema_version),
            ));
        }
        validate_session_id_text(self.session_id.as_str())?;
        normalize_uuid_str(self.event_id.as_str())?;
        normalize_uuid_str(self.origin_id.as_str())?;
        if let Some(parent) = &self.parent_event_id {
            normalize_uuid_str(parent.as_str())?;
        }
        if self.kind.as_str().is_empty() {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "event kind is empty",
            ));
        }
        if !is_lower_hex_64(&self.content_hash) {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "content_hash is not a lowercase hex sha256",
            ));
        }
        match (&self.payload, &self.payload_ref) {
            (Some(_), None) => {}
            (None, Some(reference)) => validate_payload_ref(reference)?,
            (Some(_), Some(_)) => {
                return Err(StoreError::invalid_request(
                    StoreErrorCode::PayloadRefConflict,
                    "record carries both payload and payload_ref",
                ));
            }
            (None, None) => {
                return Err(StoreError::invalid_request(
                    StoreErrorCode::PayloadRefMissing,
                    "record carries neither payload nor payload_ref",
                ));
            }
        }
        if self.parent_event_id.is_none() && !self.allows_null_parent() {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "a non-root record must name a parent event",
            ));
        }
        if let Some(payload) = &self.payload
            && let Some(kind) = self.kind.known()
        {
            typed_payload_ok(kind, payload)?;
        }
        Ok(())
    }

    /// True when a null parent is legitimate: a session root or an explicit reset root.
    fn allows_null_parent(&self) -> bool {
        matches!(self.kind.known(), Some(KnownEventKind::SessionCreated))
    }

    /// Recompute the canonical hash over every field except content_hash.
    pub fn content_hash_of(&self) -> Result<String, StoreError> {
        Ok(sha256_hex(canonical_json(&self.hashable_value()?)?.as_bytes()))
    }

    /// Fail when the stored hash does not match the record's content.
    pub fn verify_hash(&self) -> Result<(), StoreError> {
        let computed = self.content_hash_of()?;
        if computed != self.content_hash {
            return Err(StoreError::hash_conflict(
                format!(
                    "event {} has content_hash {} but its content hashes to {}",
                    self.event_id.as_str(),
                    self.content_hash,
                    computed
                ),
                Vec::new(),
            ));
        }
        Ok(())
    }

    /// The stored canonical value: every declared field, identity and time
    /// normalized to their canonical spellings, including content_hash.
    pub fn canonical_value(&self) -> Result<Value, StoreError> {
        let mut map = serde_json::Map::new();
        map.insert("schema_version".to_string(), Value::from(self.schema_version));
        map.insert(
            "session_id".to_string(),
            Value::String(self.session_id.as_str().to_string()),
        );
        map.insert(
            "event_id".to_string(),
            Value::String(normalize_uuid_str(self.event_id.as_str())?),
        );
        if let Some(parent) = &self.parent_event_id {
            map.insert(
                "parent_event_id".to_string(),
                Value::String(normalize_uuid_str(parent.as_str())?),
            );
        }
        map.insert(
            "origin_id".to_string(),
            Value::String(normalize_uuid_str(self.origin_id.as_str())?),
        );
        map.insert(
            "timestamp".to_string(),
            Value::String(canonical_timestamp(&self.timestamp)),
        );
        map.insert("kind".to_string(), Value::String(self.kind.as_str().to_string()));
        if let Some(payload) = &self.payload {
            map.insert("payload".to_string(), canonicalize_value(payload));
        }
        if let Some(reference) = &self.payload_ref {
            map.insert(
                "payload_ref".to_string(),
                serde_json::to_value(reference).map_err(|err| {
                    StoreError::internal(
                        StoreErrorCode::Internal,
                        format!("payload_ref is not serializable: {err}"),
                    )
                })?,
            );
        }
        map.insert("content_hash".to_string(), Value::String(self.content_hash.clone()));
        Ok(Value::Object(map))
    }

    /// The hashed projection: the stored canonical value without content_hash.
    pub fn hashable_value(&self) -> Result<Value, StoreError> {
        let mut value = self.canonical_value()?;
        if let Value::Object(map) = &mut value {
            map.remove("content_hash");
        }
        Ok(value)
    }

    /// The canonical single-line JSON for this record, without a newline.
    pub fn canonical_json(&self) -> Result<String, StoreError> {
        canonical_json(&self.canonical_value()?)
    }

    /// The complete canonical event as an opaque transport record.
    pub fn canonical_record(&self) -> Result<CanonicalEventRecord, StoreError> {
        CanonicalEventRecord::from_envelope(self)
    }

    /// The inline payload object.
    pub fn payload_value(&self) -> Result<&Value, StoreError> {
        match self.body()? {
            EventBody::Inline => self
                .payload
                .as_ref()
                .ok_or_else(|| StoreError::invalid_request(StoreErrorCode::PayloadRefMissing, "payload missing")),
            EventBody::Blob => Err(StoreError::invalid_request(
                StoreErrorCode::PayloadRefConflict,
                "payload is externalized; hydrate it from its blob",
            )),
        }
    }

    /// Decode the inline payload into a typed schema, refusing a kind mismatch.
    pub fn decode_payload<T: DeserializeOwned>(&self, expected: KnownEventKind) -> Result<T, StoreError> {
        if self.kind.known() != Some(expected) {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                format!("expected a {} record, found {}", expected.as_str(), self.kind.as_str()),
            ));
        }
        let payload = self.payload_value()?;
        serde_json::from_value(payload.clone()).map_err(|err| {
            StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                format!("{} payload is malformed: {err}", expected.as_str()),
            )
        })
    }

    /// Refuse projection when this build does not know the event kind.
    pub fn require_projectable(&self) -> Result<KnownEventKind, StoreError> {
        self.kind.known().ok_or_else(|| {
            StoreError::invalid_request(
                StoreErrorCode::UnsupportedEventKind,
                format!("unknown event kind {}", self.kind.as_str()),
            )
        })
    }

    /// Verify blob bytes against the recorded ref and return a separate view.
    /// The stored envelope, its canonical bytes, and its hash stay untouched.
    pub fn hydrate_from_blob(&self, blob_bytes: &[u8]) -> Result<HydratedEvent, StoreError> {
        let reference = self.payload_ref.as_ref().ok_or_else(|| {
            StoreError::invalid_request(
                StoreErrorCode::PayloadRefMissing,
                "record has no payload_ref to hydrate",
            )
        })?;
        hydrate_payload(reference, blob_bytes)?;
        let payload: Value = serde_json::from_slice(blob_bytes).map_err(|err| {
            StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                format!("blob is not canonical JSON: {err}"),
            )
        })?;
        Ok(HydratedEvent {
            canonical_json: self.canonical_json()?,
            content_hash: self.content_hash.clone(),
            event_id: self.event_id.clone(),
            payload: canonicalize_value(&payload),
        })
    }
}

/// A hydrated view of a blob-backed event. The stored record is not modified.
#[derive(Clone, Debug, PartialEq)]
pub struct HydratedEvent {
    /// The stored canonical JSON, unchanged.
    pub canonical_json: String,
    /// The stored content hash, unchanged.
    pub content_hash: String,
    /// The event this view belongs to.
    pub event_id: EventId,
    /// The canonical payload value.
    pub payload: Value,
}

/// The complete canonical event as an opaque UTF-8 transport record.
///
/// A consumer that cannot re-serialize numbers faithfully (a JavaScript client
/// reading JSON numbers into doubles, for example) carries canonical_json
/// opaquely and never reparses it for transmission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalEventRecord {
    /// The event this record carries.
    pub event_id: EventId,
    /// The stored content hash.
    pub content_hash: String,
    /// The complete canonical event JSON, as stored.
    pub canonical_json: String,
}

impl CanonicalEventRecord {
    /// Capture an envelope's canonical bytes and identity.
    pub fn from_envelope(envelope: &EventEnvelope) -> Result<Self, StoreError> {
        Ok(Self {
            event_id: envelope.event_id.clone(),
            content_hash: envelope.content_hash.clone(),
            canonical_json: envelope.canonical_json()?,
        })
    }

    /// Parse the inner canonical JSON for validation only.
    pub fn inner_value(&self) -> Result<Value, StoreError> {
        serde_json::from_str(&self.canonical_json).map_err(|err| {
            StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                format!("canonical_json is not valid JSON: {err}"),
            )
        })
    }

    /// Validate the outer id and hash against the inner canonical event.
    pub fn validate(&self) -> Result<(), StoreError> {
        let envelope = self.into_envelope()?;
        if envelope.event_id != self.event_id {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "outer event_id does not match the canonical event",
            ));
        }
        if envelope.content_hash != self.content_hash {
            return Err(StoreError::hash_conflict(
                "outer content_hash does not match the canonical event",
                Vec::new(),
            ));
        }
        envelope.verify_hash()?;
        Ok(())
    }

    /// Decode the carried event, validating identity and hash.
    pub fn into_envelope(&self) -> Result<EventEnvelope, StoreError> {
        decode_record_line(self.canonical_json.as_bytes())
    }
}

/// Provenance of an explicit session reset (Clear) that starts a fresh branch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResetProvenance {
    /// When the reset was recorded.
    pub reset_at: DateTime<Utc>,
    /// The head that was active when the reset happened, for audit only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_head_event_id: Option<EventId>,
    /// Why the reset happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Durable session display and setting metadata. Excludes messages, compaction,
/// environment snapshots, memory injections, replay events, and every runtime
/// cache, PID, or socket field; unknown older keys are preserved in
/// legacy_extensions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_usage_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// Cache lineage root shared by one fork family; a fork keeps its parent root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    /// Legacy native plugin backend id.
    ///
    /// Kept so sessions written before the plugin runtime was removed still
    /// deserialize. The field is not interpreted: the direct OpenAI-compatible
    /// provider owns the model path and `provider_key` keeps the provider
    /// classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_api_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_model: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_model_overrides: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub improve_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoreview_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autojudge_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(default)]
    pub saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_label: Option<String>,
    #[serde(default)]
    pub migration_epoch: u64,
    /// Preserved metadata this build does not interpret, including legacy status
    /// and PID history. Never resurrected as runtime state.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legacy_extensions: BTreeMap<String, Value>,
}

/// SessionCreated payload: the session's complete durable metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionCreatedPayload {
    /// True for an explicit reset root (Clear) rather than the session's first root.
    #[serde(default)]
    pub branch_root: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_provenance: Option<ResetProvenance>,
    pub metadata: SessionMetadata,
}

/// MessageAppended payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageAppendedPayload {
    pub message: StoredMessage,
}

/// MessageRevised payload: a replacement for an existing append event. The
/// target is the append event's id, so no message index or wall-clock ordering
/// is consulted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageRevisedPayload {
    pub target_event_id: EventId,
    pub replacement: StoredMessage,
}

/// MetadataChanged payload: the complete small metadata, not a delta.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetadataChangedPayload {
    pub metadata: SessionMetadata,
    /// Set when this event also records that a resume turn was created for the
    /// named request.
    ///
    /// The resume marker is not session metadata, but it has to become durable
    /// in the same append a normal save already performs, and a dedicated event
    /// kind would make every older build refuse to project the session. The
    /// field is therefore additive and optional: a reader that does not know it
    /// ignores it, and a marker event carries the session's own metadata
    /// unchanged beside it. `resume_key_recorded_at` reads the earliest such
    /// event per key, so a repeated append cannot move the recorded moment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_key: Option<String>,
}

/// How a compaction attempt finished.
#[derive(Clone, Debug, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionOutcome {
    /// A structured summary was produced from the covered original events.
    Summary,
    /// A provider-native compaction window was retained whole.
    Native,
    /// A deterministic fallback ran and the lost context is surfaced.
    LossyEmergency,
}

/// One restored file or skill reference carried by a compacted event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestorationEntry {
    /// Path or skill identifier, never the file body.
    pub path: String,
    /// What kind of reference this is.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Bounded restoration manifest persisted with a compacted event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestorationManifest {
    pub entries: Vec<RestorationEntry>,
    /// Total bytes budgeted for re-reading the listed references.
    #[serde(default)]
    pub total_bytes: u64,
}

/// Compacted payload: a summary bound to the original event range it covers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompactedPayload {
    pub summary_text: String,
    /// The branch head when the compaction job started.
    pub source_head_id: Option<EventId>,
    /// The last original event the summary actually covered.
    pub covered_through_event_id: EventId,
    /// Canonical digest of the covered prefix.
    pub source_prefix_digest: String,
    /// First event kept as a tail rather than summarized.
    pub first_kept_event_id: Option<EventId>,
    /// The previously applied compaction, when this one is incremental.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_compaction_id: Option<EventId>,
    pub provider: String,
    pub model: String,
    pub route: String,
    pub settings_digest: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    pub outcome: CompactionOutcome,
    /// Ordered provider-native window items, retained whole for replay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_window: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restoration: Option<RestorationManifest>,
}

/// Why a compaction attempt was refused or blocked.
#[derive(Clone, Debug, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionBlockedReason {
    /// No safe tail fits the budget; automatic repetition is refused.
    ContextRecoveryBlocked,
    /// The configured budget leaves no usable window.
    ContextBudgetTooSmall,
    /// The provider call failed after its bounded retries.
    ProviderError,
    /// Summarizing would overflow and shrinking made no progress.
    OverflowNoProgress,
    /// The covered prefix or model settings changed under the job.
    StaleSource,
}

/// CompactionBlocked payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompactionBlockedPayload {
    pub source_head_id: Option<EventId>,
    pub source_digest: String,
    pub provider: String,
    pub model: String,
    pub route: String,
    pub settings_digest: String,
    pub generation: u64,
    pub reason: CompactionBlockedReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// ArchiveChanged payload. Archive and restore record an event and never delete.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArchiveChangedPayload {
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// AuxiliaryRecorded payload: preserved non-conversation history.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "aux_kind", rename_all = "snake_case")]
pub enum AuxiliaryRecordedPayload {
    /// An environment snapshot.
    EnvSnapshot(Box<EnvSnapshot>),
    /// A stored memory injection.
    MemoryInjection(StoredMemoryInjection),
    /// A base-only replay event, preserved as versioned JSON.
    ReplayEvent { schema_version: u32, value: Value },
    /// Any other preserved legacy auxiliary record, as versioned JSON.
    Legacy { schema_version: u32, value: Value },
}

/// Recursively sort object keys by UTF-8 byte order, keeping array order.
pub fn canonicalize_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            let mut out = serde_json::Map::with_capacity(map.len());
            for key in keys {
                out.insert(key.clone(), canonicalize_value(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_value).collect()),
        other => other.clone(),
    }
}

/// Compact canonical JSON: no whitespace, object keys sorted recursively.
pub fn canonical_json(value: &Value) -> Result<String, StoreError> {
    serde_json::to_string(&canonicalize_value(value))
        .map_err(|err| StoreError::internal(StoreErrorCode::Internal, format!("cannot encode canonical json: {err}")))
}

/// The canonical timestamp form used inside hashed content: UTC RFC3339 with
/// fixed nanosecond precision, so equal instants never hash differently.
pub fn canonical_timestamp(timestamp: &DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// Lowercase hex SHA-256 of the given bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

/// The read buffer [`sha256_hex_stream`] allocates, once, whatever the source length.
///
/// The digest never holds the source in memory, so this constant is the peak
/// temporary buffer of a streamed hash: it does not grow with the input.
pub const HASH_STREAM_BUFFER: usize = 64 * 1024;

/// A digest produced by reading a source instead of holding it in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamDigest {
    /// Lowercase hex SHA-256, byte-identical to [`sha256_hex`] over the same bytes.
    pub hex: String,
    /// Bytes the reader yielded.
    pub bytes: u64,
}

/// Lowercase hex SHA-256 of everything `reader` yields, through one fixed buffer.
///
/// Identical to [`sha256_hex`] over the concatenated bytes: the hasher sees the
/// same byte sequence, only in [`HASH_STREAM_BUFFER`]-sized pieces. The buffer is
/// the only allocation that depends on the read loop, and it never depends on the
/// source length.
pub fn sha256_hex_stream(reader: &mut impl Read) -> std::io::Result<StreamDigest> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_STREAM_BUFFER];
    let mut bytes = 0u64;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                hasher.update(&buffer[..read]);
                bytes += u64::try_from(read).unwrap_or(u64::MAX);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(StreamDigest {
        hex: hex_encode(&hasher.finalize()),
        bytes,
    })
}

/// Canonical digest of a set of JSON parts, used for source and settings digests.
pub fn canonical_digest_of_values(parts: &[Value]) -> Result<String, StoreError> {
    let array = Value::Array(parts.iter().map(canonicalize_value).collect());
    Ok(sha256_hex(canonical_json(&array)?.as_bytes()))
}

/// Encode one stored record as a canonical line with a trailing newline.
pub fn encode_record_line(envelope: &EventEnvelope) -> Result<Vec<u8>, StoreError> {
    encode_record_line_with_limit(envelope, MAX_RECORD_BYTES)
}

/// Encode one stored record, refusing a line longer than limit.
pub fn encode_record_line_with_limit(envelope: &EventEnvelope, limit: usize) -> Result<Vec<u8>, StoreError> {
    envelope.validate()?;
    envelope.verify_hash()?;
    let line = canonical_line(envelope)?;
    if line.len() > limit {
        return Err(StoreError::invalid_request(
            StoreErrorCode::RecordTooLarge,
            format!("record is {} bytes, limit is {limit}", line.len()),
        ));
    }
    Ok(line)
}

/// Decode one stored record line, verifying structure, version, and hash.
pub fn decode_record_line(line: &[u8]) -> Result<EventEnvelope, StoreError> {
    let body = match line.strip_suffix(b"\n") {
        Some(body) => body,
        None => line,
    };
    if body.is_empty() {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidRecord,
            "empty record line",
        ));
    }
    if body.contains(&b'\r') {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidRecord,
            "record line contains a carriage return",
        ));
    }
    let text = std::str::from_utf8(body)
        .map_err(|_| StoreError::invalid_request(StoreErrorCode::InvalidRecord, "record line is not valid UTF-8"))?;
    let parsed: EventEnvelope = serde_json::from_str(text).map_err(|err| {
        StoreError::invalid_request(
            StoreErrorCode::InvalidRecord,
            format!("record line is not a valid event envelope: {err}"),
        )
    })?;
    if parsed.schema_version != EVENT_SCHEMA_VERSION {
        return Err(StoreError::invalid_request(
            StoreErrorCode::UnsupportedVersion,
            format!(
                "record schema_version {} cannot be read by this build",
                parsed.schema_version
            ),
        ));
    }
    parsed.validate()?;
    parsed.verify_hash()?;
    Ok(parsed)
}

/// A fresh random (v4) event id for a live append.
pub fn new_event_id() -> EventId {
    EventId::new_v4()
}

/// A fresh random (v4) origin id for this install.
pub fn new_origin_id() -> OriginId {
    OriginId::new_v4()
}

/// Deterministic legacy origin id: uuid5 of the fixed namespace over
/// legacy-origin/{session_id}/{logical_source_digest}. No install id, host path,
/// file mtime, or run time is an input, so migrating the same source on two
/// machines yields the same id.
pub fn legacy_origin_id(session_id: &str, logical_source_digest: &str) -> Result<OriginId, StoreError> {
    let session_id = SessionId::parse(session_id)?;
    if !is_lower_hex_64(logical_source_digest) {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidRecord,
            "logical_source_digest must be a lowercase hex sha256",
        ));
    }
    let name = format!("legacy-origin/{}/{}", session_id.as_str(), logical_source_digest);
    OriginId::parse(Uuid::new_v5(&LEGACY_ORIGIN_NAMESPACE, name.as_bytes()).to_string())
}

/// Deterministic legacy event id: uuid5 of the fixed namespace over
/// legacy-event/{session_id}/{source_kind}/{normalized_ordinal}.
pub fn legacy_event_id(session_id: &str, source_kind: &str, normalized_ordinal: u64) -> Result<EventId, StoreError> {
    let session_id = SessionId::parse(session_id)?;
    if source_kind.is_empty() {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidRecord,
            "source_kind must not be empty",
        ));
    }
    let name = format!(
        "legacy-event/{}/{}/{}",
        session_id.as_str(),
        source_kind,
        normalized_ordinal
    );
    EventId::parse(Uuid::new_v5(&LEGACY_EVENT_NAMESPACE, name.as_bytes()).to_string())
}

/// The fixed timestamp for a migrated event with no recorded time.
pub fn legacy_epoch_timestamp() -> DateTime<Utc> {
    DateTime::UNIX_EPOCH
}

/// Pick a migrated event's timestamp: the original event time, else the session
/// creation time, else the fixed epoch. Never the current time.
pub fn pick_legacy_timestamp(
    event_time: Option<DateTime<Utc>>,
    session_created_at: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    event_time.or(session_created_at).unwrap_or_else(legacy_epoch_timestamp)
}

fn build_envelope(
    draft: &EventDraft,
    payload: Option<Value>,
    payload_ref: Option<PayloadRef>,
) -> Result<EventEnvelope, StoreError> {
    let mut envelope = EventEnvelope {
        schema_version: draft.schema_version,
        session_id: draft.session_id.clone(),
        event_id: draft.event_id.clone(),
        parent_event_id: draft.parent_event_id.clone(),
        origin_id: draft.origin_id.clone(),
        timestamp: draft.timestamp,
        kind: draft.kind.clone(),
        payload,
        payload_ref,
        content_hash: String::new(),
    };
    envelope.content_hash = envelope.content_hash_of()?;
    Ok(envelope)
}

fn canonical_line(envelope: &EventEnvelope) -> Result<Vec<u8>, StoreError> {
    let mut line = envelope.canonical_json()?.into_bytes();
    line.push(b'\n');
    Ok(line)
}

fn validate_payload_ref(reference: &PayloadRef) -> Result<(), StoreError> {
    if !is_lower_hex_64(&reference.sha256) {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidPayloadRef,
            "payload_ref.sha256 must be a lowercase hex sha256",
        ));
    }
    if reference.encoding == PayloadEncoding::Unknown {
        return Err(StoreError::invalid_request(
            StoreErrorCode::UnsupportedPayloadEncoding,
            "payload_ref encoding is not decodable by this build",
        ));
    }
    Ok(())
}

fn hydrate_payload(reference: &PayloadRef, blob_bytes: &[u8]) -> Result<(), StoreError> {
    validate_payload_ref(reference)?;
    if reference.size != blob_bytes.len() as u64 {
        return Err(StoreError::invalid_request(
            StoreErrorCode::PayloadDigestMismatch,
            format!(
                "blob is {} bytes but payload_ref records {}",
                blob_bytes.len(),
                reference.size
            ),
        ));
    }
    if sha256_hex(blob_bytes) != reference.sha256 {
        return Err(StoreError::invalid_request(
            StoreErrorCode::PayloadDigestMismatch,
            "blob bytes do not match payload_ref.sha256",
        ));
    }
    Ok(())
}

fn typed_payload_ok(kind: KnownEventKind, payload: &Value) -> Result<(), StoreError> {
    let malformed = |err: serde_json::Error| {
        StoreError::invalid_request(
            StoreErrorCode::InvalidRecord,
            format!("{} payload is malformed: {err}", kind.as_str()),
        )
    };
    match kind {
        KnownEventKind::SessionCreated => {
            let created: SessionCreatedPayload = serde_json::from_value(payload.clone()).map_err(malformed)?;
            if created.branch_root && created.reset_provenance.is_none() {
                return Err(StoreError::invalid_request(
                    StoreErrorCode::InvalidRecord,
                    "a branch root must carry reset provenance",
                ));
            }
            for key in created.metadata.legacy_extensions.keys() {
                if is_reserved_metadata_key(key) {
                    return Err(StoreError::invalid_request(
                        StoreErrorCode::InvalidRecord,
                        format!("metadata key {key} belongs to another event kind"),
                    ));
                }
            }
        }
        KnownEventKind::MessageAppended => {
            serde_json::from_value::<MessageAppendedPayload>(payload.clone()).map_err(malformed)?;
        }
        KnownEventKind::MessageRevised => {
            serde_json::from_value::<MessageRevisedPayload>(payload.clone()).map_err(malformed)?;
        }
        KnownEventKind::MetadataChanged => {
            serde_json::from_value::<MetadataChangedPayload>(payload.clone()).map_err(malformed)?;
        }
        KnownEventKind::Compacted => {
            let compacted: CompactedPayload = serde_json::from_value(payload.clone()).map_err(malformed)?;
            if let Some(restoration) = &compacted.restoration
                && restoration.entries.len() > MAX_RESTORATION_ENTRIES
            {
                return Err(StoreError::invalid_request(
                    StoreErrorCode::InvalidRecord,
                    format!(
                        "restoration manifest has {} entries, limit is {}",
                        restoration.entries.len(),
                        MAX_RESTORATION_ENTRIES
                    ),
                ));
            }
        }
        KnownEventKind::CompactionBlocked => {
            serde_json::from_value::<CompactionBlockedPayload>(payload.clone()).map_err(malformed)?;
        }
        KnownEventKind::ArchiveChanged => {
            serde_json::from_value::<ArchiveChangedPayload>(payload.clone()).map_err(malformed)?;
        }
        KnownEventKind::AuxiliaryRecorded => {
            serde_json::from_value::<AuxiliaryRecordedPayload>(payload.clone()).map_err(malformed)?;
        }
    }
    Ok(())
}

fn is_reserved_metadata_key(key: &str) -> bool {
    matches!(
        key,
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
    )
}

pub(crate) fn validate_session_id_text(raw: &str) -> Result<(), StoreError> {
    let unsafe_id = |message: &str| StoreError::invalid_request(StoreErrorCode::UnsafeSessionId, message.to_string());
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return Err(unsafe_id("session id is empty"));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(unsafe_id("session id must start with an ASCII letter or digit"));
    }
    if raw.chars().count() > MAX_SESSION_ID_LEN {
        return Err(unsafe_id("session id is too long"));
    }
    if raw.starts_with('.') || raw.ends_with('.') {
        return Err(unsafe_id("session id must not begin or end with a dot"));
    }
    if raw
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
    {
        return Err(unsafe_id(
            "session id may only contain ASCII letters, digits, dot, dash and underscore",
        ));
    }
    if is_reserved_windows_name(raw) {
        return Err(unsafe_id("session id is a reserved Windows device name"));
    }
    Ok(())
}

fn is_reserved_windows_name(raw: &str) -> bool {
    let stem = raw.split('.').next().unwrap_or(raw).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let suffix = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"));
    matches!(suffix, Some(value) if ("1"..="9").contains(&value))
}

fn normalize_uuid_str(raw: &str) -> Result<String, StoreError> {
    Uuid::parse_str(raw)
        .map(|parsed| parsed.to_string())
        .map_err(|_| StoreError::invalid_request(StoreErrorCode::InvalidRecord, "identity field is not a uuid"))
}

pub(crate) fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn ensure_safe_component(root: &Path, candidate: &Path) -> Result<(), StoreError> {
    let unsafe_path = |message: &str| StoreError::invalid_request(StoreErrorCode::UnsafePath, message.to_string());
    // The root may itself be reached through a symlink (macOS /tmp, a linked HOME).
    // Resolving the root once is what makes the boundary the root itself, while
    // every component below it is still checked for symlinks.
    let canonical_root = root
        .canonicalize()
        .map_err(|err| unsafe_path(&format!("store root is not usable: {err}")))?;
    if !canonical_root.is_dir() {
        return Err(unsafe_path("store root is not a directory"));
    }
    let relative = candidate
        .strip_prefix(root)
        .or_else(|_| candidate.strip_prefix(&canonical_root))
        .map_err(|_| unsafe_path("path is outside the store root"))?
        .to_path_buf();
    if relative.as_os_str().is_empty() {
        return Err(unsafe_path("session path is the store root itself"));
    }
    let mut current = canonical_root;
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        current.push(component);
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            break;
        };
        if metadata.file_type().is_symlink() {
            return Err(unsafe_path("path component is a symlink"));
        }
        let is_last = index + 1 == component_count;
        if is_last && !metadata.is_dir() {
            return Err(unsafe_path("path exists and is not a directory"));
        }
        if !is_last && !metadata.is_dir() {
            return Err(unsafe_path("path component is not a directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Match Disk::directory even when a clean cache avoids the writer.
            if metadata.permissions().mode() & 0o777 != 0o700 {
                return Err(unsafe_path("path component is not owner-private"));
            }
        }
    }
    Ok(())
}
