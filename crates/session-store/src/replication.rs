//! Bounded exchange of original events. No runtime ownership or network policy.
use crate::*;
use serde::{Deserialize, Serialize};

pub const REPLICATION_PROTOCOL_VERSION: u32 = 1;
pub const MAX_PAGE_EVENTS: usize = 256;
pub const MAX_KNOWN_IDS: usize = 256;
pub const MAX_WIRE_BYTES: usize = 16 * 1024 * 1024;

pub use crate::record_exchange::CanonicalRecordRef;
pub use crate::replication_export::export_events;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRequest {
    pub protocol_version: u32,
    pub session_id: SessionId,
    /// Fixed on the first page. Empty means all current heads on that first page.
    pub heads: Vec<EventId>,
    pub known_ids: Vec<EventId>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEventPage {
    pub protocol_version: u32,
    pub session_id: SessionId,
    pub heads: Vec<EventId>,
    pub events: Vec<CanonicalEventRecord>,
    pub next_cursor: Option<String>,
    pub blob_refs: Vec<PayloadRef>,
    /// Whole canonical records omitted from events, at their page-local logical positions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub record_refs: Vec<CanonicalRecordRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionImportResult {
    pub session_id: SessionId,
    /// Successfully synchronized IDs, including verified identical retransmissions.
    pub accepted_ids: Vec<EventId>,
    pub heads: Vec<EventId>,
    pub retry_cursor: Option<String>,
    pub conflict_heads: Vec<EventId>,
}

pub(crate) fn validate_ids(ids: &[EventId]) -> Result<(), StoreError> {
    if ids.len() > MAX_PAGE_EVENTS {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidRecord,
            "too many event IDs",
        ));
    }
    for id in ids {
        if EventId::parse(id.as_str())? != *id {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "noncanonical event ID",
            ));
        }
    }
    Ok(())
}

pub(crate) fn version(version: u32) -> Result<(), StoreError> {
    if version != REPLICATION_PROTOCOL_VERSION {
        return Err(StoreError::invalid_request(
            StoreErrorCode::UnsupportedVersion,
            "unsupported replication protocol",
        ));
    }
    Ok(())
}

/// The escaped wire size of one page.
pub(crate) fn wire_size(page: &SessionEventPage) -> Result<usize, StoreError> {
    wire_len(page)
}

/// Escaped wire length of `value`, counted while serializing instead of after
/// materializing the bytes.
///
/// The export loop measures one record at a time through this, so its total work
/// stays proportional to the page it builds.
pub(crate) fn wire_len<T>(value: &T) -> Result<usize, StoreError>
where
    T: ?Sized + Serialize,
{
    let mut counter = WireCounter::default();
    serde_json::to_writer(&mut counter, value).map_err(crate::cache_io::json_error)?;
    WIRE_BYTES.set(WIRE_BYTES.get() + counter.bytes);
    WIRE_SERIALIZATIONS.set(WIRE_SERIALIZATIONS.get() + 1);
    Ok(counter.bytes)
}

/// Counts written bytes and drops them.
#[derive(Default)]
struct WireCounter {
    bytes: usize,
}

impl std::io::Write for WireCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

thread_local! {
    static WIRE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static WIRE_SERIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Serialization work this thread has spent measuring wire sizes since the last
/// reset: total escaped bytes, then the number of serializations.
///
/// Exported so a benchmark can show the export loop's work grows with the page
/// rather than with the square of it.
pub fn wire_work() -> (usize, usize) {
    (WIRE_BYTES.get(), WIRE_SERIALIZATIONS.get())
}

/// Restart the [`wire_work`] counters for this thread.
pub fn reset_wire_work() {
    WIRE_BYTES.set(0);
    WIRE_SERIALIZATIONS.set(0);
}

pub(crate) fn refs(events: &[CanonicalEventRecord]) -> Result<Vec<PayloadRef>, StoreError> {
    let mut refs = std::collections::BTreeMap::new();
    for record in events {
        if let Some(reference) = record.into_envelope()?.payload_ref {
            if refs.get(&reference.sha256).is_some_and(|old| old != &reference) {
                return Err(StoreError::invalid_request(
                    StoreErrorCode::InvalidPayloadRef,
                    "conflicting blob manifest",
                ));
            }
            refs.insert(reference.sha256.clone(), reference);
        }
    }
    Ok(refs.into_values().collect())
}

/// Acquire the existing exclusive guard, prevalidate the whole page, then append.
/// On I/O failure only acknowledged prefix IDs are reported; resend the same page.
pub fn import_events(paths: &SessionPaths, page: &SessionEventPage) -> Result<SessionImportResult, StoreError> {
    crate::record_page::validate_limits(page)?;
    let mut writer = BlockingWriter::lock_for_exchange(
        paths.clone(),
        page.session_id.clone(),
        (Box::new(|_| Ok(())), ReadProbe::default()),
    )?;
    writer.import_events(page)
}
