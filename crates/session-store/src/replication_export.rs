//! Bounded parent-first pages; existing maximum originals use wire-only references.
use crate::replication::*;
use crate::replication_cursor as cursor;
use crate::replication_graph::{closure, digest};
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

fn payload_refs(graph: &EventGraph, ids: &[EventId]) -> Vec<PayloadRef> {
    ids.iter()
        .filter_map(|id| graph.events[id].payload_ref.clone())
        .map(|r| (r.sha256.clone(), r))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

/// Escaped wire length of `"next_cursor":null`, the value the carried base omits.
const NULL_CURSOR_WIRE: usize = 4;

/// Escaped wire length of the comma, key, colon and array brackets that
/// introduce `record_refs`, which a page omits entirely while that list is empty.
const RECORD_REFS_FIELD_WIRE: usize = 17;

/// Escaped wire size of the page being assembled.
///
/// Each append adds the serialized size of its own record and cursor rather than
/// re-serializing the whole page, so building a page costs one serialization per
/// record instead of one per record of a growing page. `crate::record_page`
/// still serializes the finished page once and is the authority on the cap.
struct WireSize {
    /// The page envelope with empty lists, no record references and no cursor.
    base: usize,
    events: usize,
    events_count: usize,
    refs: usize,
    refs_count: usize,
    record_refs: usize,
    record_refs_count: usize,
    cursor: usize,
}

impl WireSize {
    fn new(page: &SessionEventPage) -> Result<Self, StoreError> {
        let empty = SessionEventPage {
            protocol_version: page.protocol_version,
            session_id: page.session_id.clone(),
            heads: page.heads.clone(),
            events: Vec::new(),
            next_cursor: None,
            blob_refs: Vec::new(),
            record_refs: Vec::new(),
        };
        let base = wire_len(&empty)?
            .checked_sub(NULL_CURSOR_WIRE)
            .ok_or_else(crate::record_page::oversized)?;
        Ok(Self {
            base,
            events: 0,
            events_count: 0,
            refs: 0,
            refs_count: 0,
            record_refs: 0,
            record_refs_count: 0,
            cursor: NULL_CURSOR_WIRE,
        })
    }

    /// The escaped wire size of the page carrying everything fed so far.
    fn total(&self) -> usize {
        let mut total = self.base + self.cursor + self.events + self.refs;
        total += self.events_count.saturating_sub(1);
        total += self.refs_count.saturating_sub(1);
        if self.record_refs_count > 0 {
            total += RECORD_REFS_FIELD_WIRE + self.record_refs + self.record_refs_count - 1;
        }
        total
    }

    /// Add one inline record and report the wire bytes it contributed.
    fn push_event(&mut self, record: &CanonicalEventRecord) -> Result<usize, StoreError> {
        let length = wire_len(record)?;
        self.events += length;
        self.events_count += 1;
        Ok(length)
    }

    /// Undo [`Self::push_event`] with the length it reported.
    fn pop_event(&mut self, length: usize) {
        self.events = self.events.saturating_sub(length);
        self.events_count = self.events_count.saturating_sub(1);
    }

    fn set_cursor(&mut self, next: Option<&String>) -> Result<(), StoreError> {
        self.cursor = match next {
            Some(text) => wire_len(text)?,
            None => NULL_CURSOR_WIRE,
        };
        Ok(())
    }

    fn push_ref(&mut self, reference: &PayloadRef) -> Result<(), StoreError> {
        self.refs += wire_len(reference)?;
        self.refs_count += 1;
        Ok(())
    }

    /// Account for a second event carrying an already-referenced blob.
    fn replace_ref(&mut self, previous: &PayloadRef, reference: &PayloadRef) -> Result<(), StoreError> {
        self.refs = self.refs.saturating_sub(wire_len(previous)?) + wire_len(reference)?;
        Ok(())
    }

    fn push_record_ref(&mut self, reference: &CanonicalRecordRef) -> Result<(), StoreError> {
        self.record_refs += wire_len(reference)?;
        self.record_refs_count += 1;
        Ok(())
    }
}

/// Export fixed heads, preserving dependency/ID order across inline and referenced
/// records. Both the actual escaped wire and resolved record-byte sum are bounded.
pub fn export_events(paths: &SessionPaths, request: &ExportRequest) -> Result<SessionEventPage, StoreError> {
    version(request.protocol_version)?;
    SessionId::parse(request.session_id.as_str())?;
    validate_ids(&request.heads)?;
    validate_ids(&request.known_ids)?;
    let graph = EventGraph::open(paths, &request.session_id)?;
    let mut heads = request.heads.clone();
    if heads.is_empty() {
        if request.cursor.is_some() {
            return Err(cursor::invalid());
        }
        heads = graph.heads().to_vec();
    }
    heads.sort();
    heads.dedup();
    validate_ids(&heads)?;
    let ordered = closure(&graph, &heads).map_err(|error| {
        if request.cursor.is_some() {
            cursor::invalid()
        } else {
            error
        }
    })?;
    let hash = digest(&graph, &ordered)?;
    let mut index = match &request.cursor {
        Some(token) => cursor::check(token, &request.session_id, &heads, &hash, ordered.len(), "export")?,
        None => 0,
    };
    let known: BTreeSet<_> = request.known_ids.iter().collect();
    let mut page = SessionEventPage {
        protocol_version: REPLICATION_PROTOCOL_VERSION,
        session_id: request.session_id.clone(),
        heads,
        events: Vec::new(),
        next_cursor: None,
        blob_refs: Vec::new(),
        record_refs: Vec::new(),
    };
    let mut included = Vec::new();
    let mut refs: BTreeMap<String, PayloadRef> = BTreeMap::new();
    let mut wire = WireSize::new(&page)?;
    let mut resolved_bytes = 0usize;
    while index < ordered.len() {
        if known.contains(&ordered[index]) {
            index += 1;
            continue;
        }
        if included.len() == MAX_PAGE_EVENTS {
            break;
        }
        let record = graph.events[&ordered[index]].canonical_record()?;
        let length = record.canonical_json.len();
        let next_size = resolved_bytes
            .checked_add(length)
            .ok_or_else(crate::record_page::oversized)?;
        if next_size > MAX_WIRE_BYTES {
            break;
        }
        let appended = wire.push_event(&record)?;
        included.push(ordered[index].clone());
        page.events.push(record);
        page.next_cursor = if index + 1 < ordered.len() {
            Some(cursor::make(&page.session_id, &page.heads, &hash, index + 1, "export")?)
        } else {
            None
        };
        wire.set_cursor(page.next_cursor.as_ref())?;
        if let Some(reference) = graph.events[&ordered[index]].payload_ref.clone() {
            match refs.insert(reference.sha256.clone(), reference.clone()) {
                Some(previous) => wire.replace_ref(&previous, &reference)?,
                None => wire.push_ref(&reference)?,
            }
        }
        if wire.total() > MAX_WIRE_BYTES {
            let record = page.events.pop().ok_or_else(crate::record_page::oversized)?;
            if included.len() > 1 {
                included.pop();
                break;
            }
            // This ORIGINAL cannot fit even alone with required continuation.
            // Do not alter its ID/hash/bytes or pretend the transfer is complete.
            page.record_refs.push(CanonicalRecordRef::from_record(&record, 0)?);
            wire.pop_event(appended);
            wire.push_record_ref(&page.record_refs[0])?;
            if wire.total() > MAX_WIRE_BYTES {
                return Err(crate::record_page::oversized());
            }
        }
        resolved_bytes = next_size;
        index += 1;
    }
    page.blob_refs = payload_refs(&graph, &included);
    page.next_cursor = if index < ordered.len() {
        Some(cursor::make(&page.session_id, &page.heads, &hash, index, "export")?)
    } else {
        None
    };
    crate::record_page::validate_limits(&page)?;
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(n: u128, canonical: &str) -> CanonicalEventRecord {
        CanonicalEventRecord {
            event_id: EventId::parse(uuid::Uuid::from_u128(n).to_string()).unwrap(),
            content_hash: "a".repeat(64),
            canonical_json: canonical.to_owned(),
        }
    }

    fn empty_page() -> SessionEventPage {
        SessionEventPage {
            protocol_version: REPLICATION_PROTOCOL_VERSION,
            session_id: SessionId::parse("replica").unwrap(),
            heads: vec![EventId::parse(uuid::Uuid::from_u128(9).to_string()).unwrap()],
            events: Vec::new(),
            next_cursor: None,
            blob_refs: Vec::new(),
            record_refs: Vec::new(),
        }
    }

    /// The carried size must equal a real serialization of the same page at
    /// every step, or the export would split pages at a different record.
    #[test]
    fn carried_size_covers_every_field_the_page_serializes() {
        let mut page = empty_page();
        let mut wire = WireSize::new(&page).unwrap();
        assert_eq!(wire.total(), wire_len(&page).unwrap());
        let payloads = [
            "{}",
            "{\"q\":\"\\\"\\n\\t\"}",
            "{\"u\":\"\u{1}\u{7}\u{7f}\"}",
            "{\"e\":\"\u{e9}\"}",
        ];
        for (index, payload) in payloads.iter().enumerate() {
            let entry = record(index as u128 + 1, payload);
            wire.push_event(&entry).unwrap();
            page.events.push(entry);
            assert_eq!(wire.total(), wire_len(&page).unwrap(), "after event {index}");
            page.next_cursor = Some(format!("cursor-{index}-{}", "x".repeat(index * 11)));
            wire.set_cursor(page.next_cursor.as_ref()).unwrap();
            assert_eq!(wire.total(), wire_len(&page).unwrap(), "after cursor {index}");
            let reference = PayloadRef {
                sha256: format!("{:064x}", index + 1),
                size: 3,
                encoding: PayloadEncoding::CanonicalJson,
            };
            page.blob_refs.push(reference.clone());
            wire.push_ref(&reference).unwrap();
            assert_eq!(wire.total(), wire_len(&page).unwrap(), "after reference {index}");
        }
        let second = PayloadRef {
            sha256: format!("{:064x}", 1),
            size: 4,
            encoding: PayloadEncoding::CanonicalJson,
        };
        wire.replace_ref(&page.blob_refs[0], &second).unwrap();
        page.blob_refs[0] = second;
        assert_eq!(wire.total(), wire_len(&page).unwrap(), "after a replaced reference");
        let entry = record(99, "{\"z\":1}");
        let appended = wire.push_event(&entry).unwrap();
        let reference = CanonicalRecordRef {
            position: page.events.len(),
            event_id: entry.event_id.clone(),
            content_hash: entry.content_hash.clone(),
            sha256: crate::sha256_hex(entry.canonical_json.as_bytes()),
            size: entry.canonical_json.len() as u64,
        };
        page.record_refs.push(reference.clone());
        wire.pop_event(appended);
        wire.push_record_ref(&reference).unwrap();
        assert_eq!(wire.total(), wire_len(&page).unwrap(), "with a record reference");
    }
}
