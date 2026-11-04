//! Additive session-replication and cursor-history wire types (protocol 1.12).
//!
//! These mirror the store's exchange contract without depending on the store
//! crate: the API boundary stays curated, and the bridge converts field by
//! field. `canonical_json` is always the original UTF-8 canonical event text,
//! carried as an opaque string. A client must never parse it into a number
//! model and re-serialize it: `u64` values above 2^53, `1.0`, and `-0.0`
//! survive only as bytes.

use serde::{Deserialize, Serialize};

/// Version of the replication exchange carried inside protocol v1.
pub const REPLICATION_PROTOCOL_VERSION: u32 = 1;
/// Maximum inline plus referenced records in one page.
pub const MAX_REPLICATION_PAGE_EVENTS: usize = 256;
/// Maximum serialized page envelope, including JSON escaping.
pub const MAX_REPLICATION_WIRE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum history messages a page may request.
pub const MAX_HISTORY_PAGE: u32 = 200;
/// History messages returned when the request omits `limit`.
pub const DEFAULT_HISTORY_PAGE: u32 = 50;

pub(crate) fn default_protocol_version() -> u32 {
    REPLICATION_PROTOCOL_VERSION
}

/// One original event record, exactly as stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalEventRecord {
    pub event_id: String,
    /// The event's own hash, which excludes its `content_hash` field.
    pub content_hash: String,
    /// The complete canonical event JSON, including `content_hash`.
    pub canonical_json: String,
}

/// A whole canonical record that did not fit the inline envelope.
///
/// Distinct from a payload `blob_ref`: `sha256` here hashes the complete
/// canonical record (including `content_hash`), while the event's
/// `content_hash` covers the hashable projection without it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRecordRef {
    /// Zero-based logical position among all inline and referenced records.
    pub position: usize,
    pub event_id: String,
    pub content_hash: String,
    /// SHA-256 of the complete canonical record bytes.
    pub sha256: String,
    pub size: u64,
}

/// Reference to an original payload blob stored outside the event log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PayloadRef {
    pub sha256: String,
    pub size: u64,
    /// Always `canonical_json` on this wire.
    pub encoding: String,
}

/// One contiguous 1 MiB chunk of an original blob.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionBlobChunk {
    pub sha256: String,
    pub offset: u64,
    /// RFC 4648 standard base64, with padding.
    pub data_base64: String,
    pub next_offset: u64,
    pub total_size: u64,
    pub complete: bool,
}

/// Reply to `ListSessionHeads` / `SelectSessionHead`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionHeads {
    pub session_id: String,
    pub heads: Vec<String>,
    /// The head execution would use. Absent when several heads need an
    /// explicit host-local selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_head: Option<String>,
}

/// Reply to `GetHistoryPage`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionHistoryPage {
    pub session_id: String,
    /// The fixed head this page was computed on.
    pub head: String,
    /// Oldest first.
    pub messages: Vec<crate::HistoryMessage>,
    /// Opaque continuation: pass it back as `before_event_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Reply to `ExportSessionEvents`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEventPage {
    pub session_id: String,
    /// Heads fixed by the first page.
    pub heads: Vec<String>,
    pub events: Vec<CanonicalEventRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blob_refs: Vec<PayloadRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub record_refs: Vec<CanonicalRecordRef>,
}

/// Reply to `ImportSessionEvents`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionImportResult {
    pub session_id: String,
    /// Durable successes, including verified identical retransmissions.
    pub accepted_ids: Vec<String>,
    pub heads: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_heads: Vec<String>,
}

/// Reply to `GetSessionBlobStatus` and `ImportSessionBlob`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionBlobStatus {
    pub session_id: String,
    pub sha256: String,
    pub received: u64,
    pub total_size: u64,
    pub complete: bool,
}

/// Reply to `GetSessionRecordBlobStatus` and `ImportSessionRecordBlob`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecordBlobStatus {
    pub session_id: String,
    pub reference: CanonicalRecordRef,
    pub received: u64,
    pub total_size: u64,
    pub complete: bool,
}

/// Bounded, optional detail on an error frame.
///
/// `ErrorCode` keeps its five stable values; this object is what a new client
/// branches on. Old clients ignore the unknown field and keep decoding the
/// enum, so no new error code is ever sent to them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreErrorDetail {
    /// Stable snake_case store cause, e.g. `hash_conflict`, `busy`.
    pub store_code: String,
    /// True only when retrying the same operation can plausibly succeed.
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_heads: Vec<String>,
}

impl StoreErrorDetail {
    /// Build from the store's own wire mapping so the two cannot drift.
    pub fn from_wire_detail(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}
