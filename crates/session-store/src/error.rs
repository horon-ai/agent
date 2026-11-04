//! Typed store errors.
//!
//! The wire protocol has a single `ErrorCode` enum (`UnsupportedVersion`,
//! `UnknownRequest`, `UnknownSession`, `InvalidRequest`, `Internal`) that old
//! clients already decode. This module keeps that enum stable and adds a typed
//! Rust error carrying a stable `store_code`, a retry hint, and bounded optional
//! details, so a new client can tell "same id, different hash" from "missing
//! dependency" without new wire enum values.

use std::fmt;

/// The wire-level error family this store error maps onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreErrorCategory {
    /// Malformed input, a boundary violation, a conflict, or a missing dependency.
    InvalidRequest,
    /// Local I/O, corruption, or a busy resource.
    Internal,
    /// The addressed session does not exist.
    UnknownSession,
}

impl StoreErrorCategory {
    /// The existing `ErrorCode` name this category serializes as.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Internal => "internal",
            Self::UnknownSession => "unknown_session",
        }
    }
}

/// Stable machine-readable cause, finer than the wire enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreErrorCode {
    /// A replica continuation does not describe this fixed event closure.
    InvalidCursor,
    /// A blob chunk differs from bytes already received at that offset.
    Conflict,
    /// A blob chunk starts beyond the durable contiguous prefix.
    MissingOffset,
    /// The record is not a well-formed event envelope.
    InvalidRecord,
    /// The record exceeds the per-record byte limit and cannot be stored inline.
    RecordTooLarge,
    /// Both `payload` and `payload_ref` were present.
    PayloadRefConflict,
    /// Neither `payload` nor `payload_ref` was present.
    PayloadRefMissing,
    /// The `payload_ref` is not a valid `{sha256,size,encoding}` reference.
    InvalidPayloadRef,
    /// The referenced blob bytes do not match the recorded hash or size.
    PayloadDigestMismatch,
    /// The referenced blob uses an encoding this version cannot decode.
    UnsupportedPayloadEncoding,
    /// The record declares a schema version this build must not read or write.
    UnsupportedVersion,
    /// A projection-required event kind that this build does not know.
    UnsupportedEventKind,
    /// The same event id already exists with a different canonical hash.
    HashConflict,
    /// The session id is outside the safe identity grammar.
    UnsafeSessionId,
    /// A resolved path escapes the store root or traverses a symlink.
    UnsafePath,
    /// A referenced parent, blob, or compaction ancestor is absent.
    MissingDependency,
    /// Diverged heads need an explicit host-local selection.
    ConflictHeads,
    /// A reference is present but is not valid on this ancestry.
    InvalidReference,
    /// The parent/reset graph contains a cycle.
    CyclicHistory,
    /// Compaction coverage is not a valid contiguous source prefix.
    InvalidBoundary,
    /// A tool result or boundary breaks a call/result group.
    BrokenToolChain,
    /// A visible index or rendered row does not address this projection.
    InvalidVisibleIndex,
    /// The log is corrupt in the middle rather than truncated at the tail.
    CorruptLog,
    /// Local I/O failed.
    Io,
    /// The platform or filesystem cannot provide the required durability.
    DurabilityUnsupported,
    /// Another writer or import currently owns the session.
    Busy,
    /// An offline legacy index import must complete before activation.
    MigrationRequired,
    /// Legacy input changed since its receipt or during staging.
    SourceConflict,
    /// Legacy bytes cannot be replayed without inventing history.
    CorruptLegacy,
    /// Published migration has not completed its durability protocol.
    MigrationIncomplete,
    /// The addressed session does not exist.
    UnknownSession,
    /// A bug or invariant violation inside the store.
    Internal,
}

impl StoreErrorCode {
    /// Stable snake_case spelling used in logs and error details.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCursor => "invalid_cursor",
            Self::Conflict => "conflict",
            Self::MissingOffset => "missing_offset",
            Self::InvalidRecord => "invalid_record",
            Self::RecordTooLarge => "record_too_large",
            Self::PayloadRefConflict => "payload_ref_conflict",
            Self::PayloadRefMissing => "payload_ref_missing",
            Self::InvalidPayloadRef => "invalid_payload_ref",
            Self::PayloadDigestMismatch => "payload_digest_mismatch",
            Self::UnsupportedPayloadEncoding => "unsupported_payload_encoding",
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnsupportedEventKind => "unsupported_event_kind",
            Self::HashConflict => "hash_conflict",
            Self::UnsafeSessionId => "unsafe_session_id",
            Self::UnsafePath => "unsafe_path",
            Self::MissingDependency => "missing_dependency",
            Self::ConflictHeads => "conflict_heads",
            Self::InvalidReference => "invalid_reference",
            Self::CyclicHistory => "cyclic_history",
            Self::InvalidBoundary => "invalid_boundary",
            Self::BrokenToolChain => "broken_tool_chain",
            Self::InvalidVisibleIndex => "invalid_visible_index",
            Self::CorruptLog => "corrupt_log",
            Self::Io => "io",
            Self::DurabilityUnsupported => "durability_unsupported",
            Self::Busy => "busy",
            Self::MigrationRequired => "migration_required",
            Self::SourceConflict => "source_conflict",
            Self::CorruptLegacy => "corrupt_legacy",
            Self::MigrationIncomplete => "migration_incomplete",
            Self::UnknownSession => "unknown_session",
            Self::Internal => "internal",
        }
    }
}

/// Bounded, optional detail carried only when it is meaningful.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreErrorDetails {
    /// Events already accepted by a partial import.
    pub accepted_ids: Vec<String>,
    /// Cursor a rejected transfer should resume from.
    pub retry_cursor: Option<String>,
    /// Diverged heads kept alongside a conflict.
    pub conflict_heads: Vec<String>,
}

impl StoreErrorDetails {
    /// True when no detail is present, so the field can be omitted on the wire.
    pub fn is_empty(&self) -> bool {
        self.accepted_ids.is_empty() && self.retry_cursor.is_none() && self.conflict_heads.is_empty()
    }
}

/// A typed store error: category, stable code, message, retry hint, details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    pub category: StoreErrorCategory,
    pub code: StoreErrorCode,
    pub message: String,
    /// True only when retrying the same operation can plausibly succeed.
    pub retryable: bool,
    pub details: StoreErrorDetails,
}

impl StoreError {
    /// Malformed input, boundary violation, conflict, or missing dependency.
    pub fn invalid_request(code: StoreErrorCode, message: impl Into<String>) -> Self {
        Self {
            category: StoreErrorCategory::InvalidRequest,
            code,
            message: message.into(),
            retryable: false,
            details: StoreErrorDetails::default(),
        }
    }

    /// Local I/O, corruption, or an invariant failure inside the store.
    pub fn internal(code: StoreErrorCode, message: impl Into<String>) -> Self {
        Self {
            category: StoreErrorCategory::Internal,
            code,
            message: message.into(),
            retryable: false,
            details: StoreErrorDetails::default(),
        }
    }

    /// The addressed session does not exist.
    pub fn unknown_session(message: impl Into<String>) -> Self {
        Self {
            category: StoreErrorCategory::UnknownSession,
            code: StoreErrorCode::UnknownSession,
            message: message.into(),
            retryable: false,
            details: StoreErrorDetails::default(),
        }
    }

    /// Another writer or import currently owns the session; retry later.
    pub fn busy(message: impl Into<String>) -> Self {
        Self {
            category: StoreErrorCategory::Internal,
            code: StoreErrorCode::Busy,
            message: message.into(),
            retryable: true,
            details: StoreErrorDetails::default(),
        }
    }

    /// Same id, different canonical hash: the two records cannot both be applied.
    pub fn hash_conflict(message: impl Into<String>, conflict_heads: Vec<String>) -> Self {
        Self {
            category: StoreErrorCategory::InvalidRequest,
            code: StoreErrorCode::HashConflict,
            message: message.into(),
            retryable: false,
            details: StoreErrorDetails {
                conflict_heads,
                ..StoreErrorDetails::default()
            },
        }
    }

    /// Attach bounded details to this error.
    pub fn with_details(mut self, details: StoreErrorDetails) -> Self {
        self.details = details;
        self
    }

    /// Stable snake_case code, safe to branch on.
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }

    /// The existing wire `ErrorCode` name this error maps onto.
    pub fn wire_error_code(&self) -> &'static str {
        self.category.as_str()
    }

    /// `{store_code, retryable, ...}` detail object, omitting empty detail lists.
    ///
    /// Old clients ignore this object entirely; they keep decoding the stable
    /// `ErrorCode` enum.
    pub fn wire_detail(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "store_code".to_string(),
            serde_json::Value::String(self.code_str().to_string()),
        );
        map.insert("retryable".to_string(), serde_json::Value::Bool(self.retryable));
        if !self.details.accepted_ids.is_empty() {
            map.insert(
                "accepted_ids".to_string(),
                serde_json::to_value(&self.details.accepted_ids).unwrap_or(serde_json::Value::Array(Vec::new())),
            );
        }
        if let Some(cursor) = &self.details.retry_cursor {
            map.insert("retry_cursor".to_string(), serde_json::Value::String(cursor.clone()));
        }
        if !self.details.conflict_heads.is_empty() {
            map.insert(
                "conflict_heads".to_string(),
                serde_json::to_value(&self.details.conflict_heads).unwrap_or(serde_json::Value::Array(Vec::new())),
            );
        }
        serde_json::Value::Object(map)
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "store error [{}:{}]{} {}",
            self.category.as_str(),
            self.code_str(),
            if self.retryable { " retryable" } else { "" },
            self.message
        )
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(err: std::io::Error) -> Self {
        Self::internal(StoreErrorCode::Io, format!("io error: {err}"))
    }
}
