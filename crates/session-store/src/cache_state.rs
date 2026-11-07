//! Writer-local original identity index; never stores transcript payloads.
use crate::cache_io::Stamp;
use crate::index::Index;
use crate::*;

#[derive(Default)]
pub(crate) struct WriterState {
    pub source: Option<Stamp>,
    pub index: Index,
    pub probe: ReadProbe,
    pub context: Option<crate::context_cache::CachedContext>,
    pub tools: crate::cache_tools::ToolHistory,
    pub enabled: bool,
    /// Derived publication is deferred to one rebuild at the end of this batch.
    pub batch: bool,
    /// This batch's marker identity, captured only after file and directory sync.
    pub batch_dirty: Option<Stamp>,
    pub warning: Option<StoreError>,
    pub blobs: std::collections::BTreeMap<String, Stamp>,
    /// Whether the log's directory entry has been confirmed with a flush.
    pub log_directory_durable: bool,
    /// Log bytes were written and their flush is deferred to the batch's end.
    pub pending_log_sync: bool,
}

impl BlockingWriter {
    pub(crate) fn finish_original(&mut self, sealed: &SealedEvent) {
        match self.state.committed((&self.paths, &self.id), sealed.envelope()) {
            Ok(()) => self.update_cache(sealed),
            Err(error) => {
                self.state.source = None;
                self.state.context = None;
                self.state.warning = Some(error);
            }
        }
    }
    /// Opts into derived publication while preserving the originals-only writer API.
    pub fn enable_cache(&mut self, probe: ReadProbe) {
        self.state.probe = probe;
        self.state.enabled = true;
    }

    /// Last derived-publication failure. The original append remains durable.
    pub fn cache_warning(&self) -> Option<&StoreError> {
        self.state.warning.as_ref()
    }

    /// Rebuilds under this writer's already-held session lock.
    /// # Errors
    /// Returns original damage, invalid projection or cache I/O errors.
    pub fn refresh_cache(
        &mut self,
        selected: Option<&EventId>,
        probe: ReadProbe,
    ) -> Result<crate::checkpoint::RecentContext, StoreError> {
        let (context, _catalog) = self.repair_cache(selected, probe.clone())?;
        context.materialize((&self.paths, &probe), Some(&self.state.index))
    }

    /// Runs one synchronous commit with the log flush and derived publication
    /// held back to its end.
    ///
    /// Every append inside still validates and durably commits its own original,
    /// including the reserved-byte retry, and still writes its log bytes; only the
    /// log flush, the derived checkpoint and the catalog work are deferred. One
    /// commit therefore pays one flush instead of one per event. A caller that
    /// publishes a head must call Self::flush_pending_log after the last append and
    /// before that head, so a confirmed head never names an unflushed event. A
    /// nested call leaves everything to its outer batch, and the flag is cleared on
    /// the way out even when the job fails, so a failed batch cannot mute later
    /// publications.
    /// # Errors
    /// Returns the error from `job`, or the flush error when the deferred flush
    /// fails too: an unconfirmed original outranks the job's own error.
    /// # Panics
    /// Resumes a panic from `job` after attempting the deferred flush and recording
    /// its failure in Self::cache_warning.
    pub fn batched<T, E>(
        &mut self,
        job: impl FnOnce(&mut Self) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<StoreError>,
    {
        if self.state.batch {
            return job(self);
        }
        self.state.batch = true;
        self.state.batch_dirty = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(self)));
        self.state.batch = false;
        self.state.batch_dirty = None;
        match result {
            Ok(Ok(value)) => self.flush_pending_log().map(|()| value).map_err(E::from),
            Ok(Err(error)) => match self.flush_pending_log() {
                Ok(()) => Err(error),
                Err(flush) => Err(E::from(flush)),
            },
            Err(panic) => {
                // Bytes the job already wrote must not be left unconfirmed by a
                // panic either; a failure here is reported with the other warnings.
                if let Err(error) = self.flush_pending_log() {
                    self.state.warning = Some(error);
                }
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// Rebuilds the derived state once for the batch that just committed.
    ///
    /// The originals and the selected head are already durable, so a failure here
    /// is recorded in Self::cache_warning and left discoverable through the dirty
    /// marker instead of turning a durable commit into a failed save. A listing
    /// row deferred by catalog contention is reported the same way.
    /// # Errors
    /// Returns original damage or cache I/O errors; committed originals remain durable.
    pub fn rebuild_cache(&mut self, selected: Option<&EventId>) -> Result<(), StoreError> {
        let probe = self.state.probe.clone();
        match self.repair_cache(selected, probe) {
            Ok((_context, catalog)) => {
                self.state.warning = match catalog {
                    crate::catalog::RowWrite::Written => None,
                    crate::catalog::RowWrite::Deferred => Some(crate::catalog::deferred_row()),
                };
                Ok(())
            }
            Err(error) => {
                self.state.context = None;
                self.state.warning = Some(error.clone());
                Err(error)
            }
        }
    }

    pub(crate) fn repair_cache(
        &mut self,
        selected: Option<&EventId>,
        probe: ReadProbe,
    ) -> Result<(crate::context_cache::CachedContext, crate::catalog::RowWrite), StoreError> {
        self.dirty()?;
        let cache = crate::checkpoint::SessionCache::new(self.paths.clone(), probe);
        let rebuilt = cache.rebuild((&self.id, selected), &mut self.disk)?;
        self.state.index = rebuilt.index;
        self.state.tools = rebuilt.tools;
        if selected.is_none() {
            self.state.context = Some(rebuilt.context.clone());
        } else {
            self.state.context = None;
        }
        Ok((rebuilt.context, rebuilt.catalog))
    }

    pub(crate) fn update_cache(&mut self, sealed: &SealedEvent) {
        if !self.state.enabled {
            return;
        }
        if self.state.batch {
            // The batch publishes derived state once, after its last original. The
            // context it started from no longer describes the log, so it is dropped
            // rather than advanced event by event.
            self.state.context = None;
            return;
        }
        match self.publish_committed(sealed) {
            Ok(crate::catalog::RowWrite::Written) => self.state.warning = None,
            // The session's own checkpoint is durable; only the shared listing row
            // is due. It is reported, and its dirty marker stays.
            Ok(crate::catalog::RowWrite::Deferred) => {
                self.state.warning = Some(crate::catalog::deferred_row());
            }
            Err(error) => {
                self.state.warning = Some(error);
                self.state.context = None;
                // A failure after dirty removal must still leave repair discoverable.
                if let Err(error) = self.dirty() {
                    self.state.warning = Some(error);
                }
            }
        }
    }

    fn publish_committed(&mut self, sealed: &SealedEvent) -> Result<crate::catalog::RowWrite, StoreError> {
        let event = sealed.envelope();
        let cache = crate::checkpoint::SessionCache::new(self.paths.clone(), self.state.probe.clone());
        if let Some(mut context) = self.state.context.take() {
            if context.head == event.event_id {
                let written = cache.publish((&context, None), &self.state.index, &mut self.disk)?;
                self.state.context = Some(context);
                return Ok(written);
            }
            if event.parent_event_id.as_ref() == Some(&context.head) {
                let value = match sealed.blob() {
                    Some(bytes) => event.hydrate_from_blob(bytes)?.payload,
                    None => event.payload_value()?.clone(),
                };
                match crate::projection_payload::Payload::parse(event, &value)? {
                    crate::projection_payload::Payload::Appended(payload) => {
                        self.state.tools.append(&payload.message)?;
                        self.state
                            .index
                            .activate_append(&event.event_id, context.active_count)?;
                        context.push(
                            &ProjectedMessage {
                                event_id: event.event_id.clone(),
                                message: payload.message,
                            },
                            event.payload_ref.is_none(),
                        )?;
                        context.head = event.event_id.clone();
                        context.activity_at = event.timestamp;
                        let written = cache.publish((&context, None), &self.state.index, &mut self.disk)?;
                        self.state.context = Some(context);
                        return Ok(written);
                    }
                    crate::projection_payload::Payload::Created(_)
                    | crate::projection_payload::Payload::Revised(_)
                    | crate::projection_payload::Payload::Metadata(_)
                    | crate::projection_payload::Payload::Compacted(_)
                    | crate::projection_payload::Payload::Blocked(_)
                    | crate::projection_payload::Payload::Archive(_)
                    | crate::projection_payload::Payload::Auxiliary(_)
                    | crate::projection_payload::Payload::Unknown => {}
                }
            }
        }
        let rebuilt = cache.rebuild((&self.id, None), &mut self.disk)?;
        self.state.context = Some(rebuilt.context);
        self.state.index = rebuilt.index;
        self.state.tools = rebuilt.tools;
        Ok(rebuilt.catalog)
    }
}

impl WriterState {
    pub fn check_event(&self, source: (&SessionPaths, &SessionId), event: &EventEnvelope) -> Result<bool, StoreError> {
        let existing = self.index.rows.iter().find(|old| old.id == event.event_id);
        if let Some(old) = existing {
            if old.hash != event.content_hash {
                return Err(StoreError::hash_conflict(
                    "same event id has different content",
                    Vec::new(),
                ));
            }
            crate::index::at(source, old, &self.probe)?;
        }
        if let Some(parent) = &event.parent_event_id
            && !self.index.rows.iter().any(|old| old.id == *parent)
        {
            return Err(StoreError::invalid_request(
                StoreErrorCode::MissingDependency,
                "parent is absent",
            ));
        }
        Ok(existing.is_some())
    }
    pub fn unchanged(&self, paths: &SessionPaths, id: &SessionId) -> Result<bool, StoreError> {
        if self.source != Stamp::optional(&paths.events_log(id)?)? {
            return Ok(false);
        }
        for (hash, stamp) in &self.blobs {
            if Stamp::optional(&paths.blob_file(id, hash)?)?.as_ref() != Some(stamp) {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub fn replace(&mut self, report: &ScanReport, source: (&SessionPaths, &SessionId)) -> Result<(), StoreError> {
        self.index = Index::from_events(&report.events)?;
        self.blobs.clear();
        for event in &report.events {
            if let Some(reference) = &event.payload_ref {
                self.blobs.insert(
                    reference.sha256.clone(),
                    Stamp::read(&source.0.blob_file(source.1, &reference.sha256)?)?,
                );
            }
        }
        Ok(())
    }
    pub fn committed(&mut self, source: (&SessionPaths, &SessionId), event: &EventEnvelope) -> Result<(), StoreError> {
        if !self.index.rows.iter().any(|r| r.id == event.event_id) {
            self.index.push(event)?;
        }
        if let Some(reference) = &event.payload_ref {
            self.blobs.insert(
                reference.sha256.clone(),
                Stamp::read(&source.0.blob_file(source.1, &reference.sha256)?)?,
            );
        }
        self.source = Stamp::optional(&source.0.events_log(source.1)?)?;
        Ok(())
    }
}
