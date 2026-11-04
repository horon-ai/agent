//! Whole-record identity and transport descriptor; distinct from payload references.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRecordRef {
    /// Zero-based logical position among ALL inline and referenced events in this page.
    pub position: usize,
    pub event_id: EventId,
    /// Original event hash (which excludes its own content_hash field).
    pub content_hash: String,
    /// Hash of the entire original canonical_json, including content_hash.
    pub sha256: String,
    pub size: u64,
}

impl CanonicalRecordRef {
    pub fn from_record(record: &CanonicalEventRecord, position: usize) -> Result<Self, StoreError> {
        record.validate()?;
        let reference = Self {
            position,
            event_id: record.event_id.clone(),
            content_hash: record.content_hash.clone(),
            sha256: sha256_hex(record.canonical_json.as_bytes()),
            size: u64::try_from(record.canonical_json.len()).map_err(|_| invalid("record size overflow"))?,
        };
        reference.validate()?;
        Ok(reference)
    }

    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        if self.position >= crate::replication::MAX_PAGE_EVENTS
            || EventId::parse(self.event_id.as_str())? != self.event_id
            || self.size == 0
            || self.size >= u64::try_from(MAX_RECORD_BYTES).map_err(|_| invalid("record bound"))?
            || [&self.sha256, &self.content_hash]
                .iter()
                .any(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        {
            return Err(invalid("invalid whole-record reference"));
        }
        Ok(())
    }

    pub(crate) fn payload_transport(&self) -> PayloadRef {
        PayloadRef {
            sha256: self.sha256.clone(),
            size: self.size,
            encoding: PayloadEncoding::CanonicalJson,
        }
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::InvalidRecord, message)
}
