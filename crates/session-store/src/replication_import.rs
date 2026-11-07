//! All-page validation under the existing writer guard, followed by prefix commits.
use crate::replication::*;
use crate::replication_cursor as cursor;
use crate::replication_graph::{closure, digest, resolve};
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn validate_page(page: &SessionEventPage, paths: &SessionPaths) -> Result<Vec<EventEnvelope>, StoreError> {
    let records = page.resolve_records(paths)?;
    let mut events = Vec::new();
    for record in &records {
        record.validate()?;
        let event = record.into_envelope()?;
        if event.session_id != page.session_id || event.canonical_json()? != record.canonical_json {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "noncanonical or cross-session record",
            ));
        }
        encode_record_line(&event)?;
        events.push(event);
    }
    let expected = refs(&records)?;
    let mut advertised = page.blob_refs.clone();
    advertised.sort_by(|a, b| a.sha256.cmp(&b.sha256));
    advertised.dedup();
    if advertised != expected {
        return Err(StoreError::invalid_request(
            StoreErrorCode::InvalidPayloadRef,
            "blob manifest does not match events",
        ));
    }
    Ok(events)
}

impl BlockingWriter {
    /// Import through an already-owned idle writer. Runtime busy/turn policy belongs
    /// to the caller; no second lock, implicit selection, or timestamp winner occurs.
    pub fn import_events(&mut self, page: &SessionEventPage) -> Result<SessionImportResult, StoreError> {
        self.import_events_resuming(page, None)
    }

    /// A retry cursor is bound to this complete page's canonical order. Previously
    /// processed records are verified and synchronized again, never silently skipped.
    pub fn import_events_resuming(
        &mut self,
        page: &SessionEventPage,
        retry_cursor: Option<&str>,
    ) -> Result<SessionImportResult, StoreError> {
        self.check_lock()?;
        let incoming = validate_page(page, &self.paths)?;
        if page.session_id != self.id {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "wrong import session",
            ));
        }
        // No recovery, cache publication, or append before this entire validation.
        let report = full_scan(&self.paths, &self.id)?;
        if let Some(error) = report.damage {
            return Err(error);
        }
        let original_ids: BTreeSet<_> = report.events.iter().map(|e| e.event_id.clone()).collect();
        let mut all = report.events;
        all.extend(incoming.iter().cloned());
        let graph = EventGraph::with_blobs(&self.id, &all, |e| resolve(&self.paths, &self.id, e))?;
        validate_ids(graph.heads())?;
        let order = closure(&graph, graph.heads())?;
        for head in graph.heads() {
            graph.replay_checked(Some(head), true)?;
        }
        let submitted: BTreeMap<_, _> = incoming.iter().map(|e| (e.event_id.clone(), e)).collect();
        let ordered: Vec<_> = order.into_iter().filter(|id| submitted.contains_key(id)).collect();
        if !page.record_refs.is_empty() && incoming.iter().map(|e| &e.event_id).ne(ordered.iter()) {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidRecord,
                "mixed records are not in canonical dependency order",
            ));
        }
        let hash = digest(&graph, &ordered)?;
        let mut heads = page.heads.clone();
        heads.sort();
        heads.dedup();
        if let Some(token) = retry_cursor {
            cursor::check(token, &self.id, &heads, &hash, ordered.len(), "import")?;
        }
        let mut accepted = Vec::new();
        let start = match self.pending_id() {
            Some(pending) => ordered
                .iter()
                .position(|id| id == pending)
                .ok_or_else(|| StoreError::busy("retry the reserved event's original page"))?,
            None => 0,
        };
        if ordered[..start].iter().any(|id| !original_ids.contains(id)) {
            return Err(StoreError::busy("uncommitted records precede the reserved event"));
        }
        if self.import_acknowledged.as_ref().is_none_or(|(key, _)| key != &hash) {
            self.import_acknowledged = Some((hash.clone(), Vec::new()));
        }
        if start > 0 {
            let acknowledged = self
                .import_acknowledged
                .as_ref()
                .map(|(_, ids)| ids.as_slice())
                .unwrap_or(&[]);
            accepted.extend(
                ordered[..start]
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .filter(|id| acknowledged.contains(id)),
            );
            let retry = cursor::make(&self.id, &heads, &hash, start, "import")?;
            let sync_result = (|| -> Result<(), StoreError> {
                let file = crate::blobs::checked_file(&self.paths.events_log(&self.id)?, false)?;
                self.disk.sync_file(&file, IoPoint::LogFileSync)?;
                self.disk
                    .sync_dir(&self.paths.session_dir(&self.id)?, IoPoint::LogDirectorySync)
            })();
            if let Err(mut error) = sync_result {
                error.details.accepted_ids = accepted;
                error.details.retry_cursor = Some(retry);
                return Err(error);
            }
            accepted = ordered[..start].iter().map(|id| id.as_str().to_owned()).collect();
            self.import_acknowledged = Some((hash.clone(), accepted.clone()));
        }
        for (index, id) in ordered.iter().enumerate().skip(start) {
            let event = SealedEvent::Inline(graph.events[id].clone());
            if let Err(mut error) = self.append(&event) {
                error.details.accepted_ids = accepted;
                error.details.retry_cursor = Some(cursor::make(&self.id, &heads, &hash, index, "import")?);
                return Err(error);
            }
            accepted.push(id.as_str().to_owned());
            self.import_acknowledged = Some((hash.clone(), accepted.clone()));
        }
        let tips = graph.heads().to_vec();
        if tips.len() > 1 {
            return Err(
                StoreError::invalid_request(StoreErrorCode::ConflictHeads, "diverged replica heads retained")
                    .with_details(StoreErrorDetails {
                        accepted_ids: accepted,
                        retry_cursor: None,
                        conflict_heads: tips.iter().map(|id| id.as_str().to_owned()).collect(),
                    }),
            );
        }
        let accepted_set: BTreeSet<_> = accepted.into_iter().collect();
        Ok(SessionImportResult {
            session_id: self.id.clone(),
            heads: tips,
            retry_cursor: None,
            conflict_heads: Vec::new(),
            accepted_ids: ordered
                .into_iter()
                .filter(|id| accepted_set.contains(id.as_str()))
                .collect(),
        })
    }
}
