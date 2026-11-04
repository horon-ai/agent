//! Preallocation limits and exact reconstruction of mixed wire records.
use crate::replication::{MAX_PAGE_EVENTS, MAX_WIRE_BYTES, SessionEventPage, validate_ids, version, wire_size};
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn validate_limits(page: &SessionEventPage) -> Result<(), StoreError> {
    version(page.protocol_version)?;
    SessionId::parse(page.session_id.as_str())?;
    validate_ids(&page.heads)?;
    let count = page
        .events
        .len()
        .checked_add(page.record_refs.len())
        .ok_or_else(oversized)?;
    if count > MAX_PAGE_EVENTS
        || page.blob_refs.len() > MAX_PAGE_EVENTS
        || page.next_cursor.as_ref().is_some_and(|s| s.len() > 48 * 1024)
        || wire_size(page)? > MAX_WIRE_BYTES
    {
        return Err(oversized());
    }
    let mut total = 0u64;
    for record in &page.events {
        total = total
            .checked_add(u64::try_from(record.canonical_json.len()).map_err(|_| oversized())?)
            .ok_or_else(oversized)?;
    }
    let mut positions = BTreeSet::new();
    let mut previous = None;
    for reference in &page.record_refs {
        // Sum declarations before allocation or filesystem reads, even on overflow.
        total = total.checked_add(reference.size).ok_or_else(oversized)?;
        if total > u64::try_from(MAX_WIRE_BYTES).map_err(|_| oversized())? {
            return Err(oversized());
        }
        reference.validate()?;
        if reference.position >= count
            || !positions.insert(reference.position)
            || previous.is_some_and(|position| position >= reference.position)
        {
            return Err(invalid("duplicate, unordered or out-of-range record position"));
        }
        previous = Some(reference.position);
    }
    if total > u64::try_from(MAX_WIRE_BYTES).map_err(|_| oversized())? {
        return Err(oversized());
    }
    if !page.record_refs.is_empty() {
        let mut identities = BTreeMap::new();
        for (id, hash) in page
            .events
            .iter()
            .map(|e| (&e.event_id, &e.content_hash))
            .chain(page.record_refs.iter().map(|r| (&r.event_id, &r.content_hash)))
        {
            if let Some(old) = identities.insert(id, hash) {
                return Err(if old == hash {
                    invalid("duplicate mixed record identity")
                } else {
                    StoreError::hash_conflict("conflicting mixed record identity", Vec::new())
                });
            }
        }
    }
    Ok(())
}

pub(crate) fn oversized() -> StoreError {
    StoreError::invalid_request(
        StoreErrorCode::RecordTooLarge,
        "replication wire or resolved page exceeds bounds",
    )
}
fn invalid(message: &str) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::InvalidRecord, message)
}

impl SessionEventPage {
    /// Rehydrate the exact logical sequence. This read-only operation checks both
    /// page bounds before allocating record bytes; import still validates the graph.
    pub fn resolve_records(&self, paths: &SessionPaths) -> Result<Vec<CanonicalEventRecord>, StoreError> {
        validate_limits(self)?;
        let mut inline = self.events.iter();
        let mut references = self.record_refs.iter().peekable();
        let mut records = Vec::with_capacity(self.events.len() + self.record_refs.len());
        for position in 0..self.events.len() + self.record_refs.len() {
            let record = match references.peek() {
                Some(reference) if reference.position == position => {
                    let reference = references.next().ok_or_else(|| invalid("missing record reference"))?;
                    crate::record_exchange::hydrate(paths, &self.session_id, reference)?
                }
                _ => inline.next().ok_or_else(|| invalid("missing inline record"))?.clone(),
            };
            records.push(record);
        }
        Ok(records)
    }
}
