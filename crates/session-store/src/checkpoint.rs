//! Rebuildable current context. Original graphs are only reconstructed cold.
use crate::blobs::Disk;
use crate::cache_io::{Stamp, decode, encode, publish};
use crate::cache_tools::ToolHistory;
use crate::catalog_row::CatalogEntry;
use crate::context_cache::CachedContext;
use crate::index::Index;
use crate::projection_payload::Payload;
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Complete active context, never a silently truncated provider view.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentContext {
    pub session_id: SessionId,
    pub head: EventId,
    pub metadata: SessionMetadata,
    pub archive: Option<ArchiveChangedPayload>,
    pub compaction: Option<(EventId, CompactedPayload)>,
    pub tail: Vec<ProjectedMessage>,
    pub activity_at: chrono::DateTime<chrono::Utc>,
}

/// Physical inline coverage, not a provider context cut or a history limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextCoverage {
    pub head: EventId,
    pub active_messages: usize,
    pub inline_from: usize,
    pub inline_messages: usize,
}

/// Physical inline suffix. This is not an executable context when a prefix is
/// omitted: `recent` restores that entire prefix from the original index.
#[derive(Clone, Debug)]
pub struct CachedTailView {
    pub coverage: ContextCoverage,
    pub messages: Vec<ProjectedMessage>,
}

impl From<&CachedContext> for ContextCoverage {
    fn from(context: &CachedContext) -> Self {
        Self {
            head: context.head.clone(),
            active_messages: context.active_count,
            inline_from: context.inline_from,
            inline_messages: context.tail.len(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub version: u32,
    pub source: Stamp,
    pub index: Stamp,
    pub summary: Stamp,
    pub anchor_hash: String,
    pub selected: Option<EventId>,
    pub context: CachedContext,
}

/// One derived reconstruction, with the shared listing row's outcome kept
/// beside the per-session checkpoint it published first.
pub(crate) struct Rebuilt {
    pub(crate) context: CachedContext,
    pub(crate) index: Index,
    pub(crate) tools: ToolHistory,
    pub(crate) catalog: crate::catalog::RowWrite,
}

/// Blocking real-filesystem cache reader, with per-instance read receipts.
pub struct SessionCache {
    pub(crate) paths: SessionPaths,
    pub(crate) probe: ReadProbe,
    #[cfg(test)]
    pub(crate) repair_hook: std::sync::Mutex<Option<crate::IoHook>>,
}
impl SessionCache {
    pub fn new(paths: SessionPaths, probe: ReadProbe) -> Self {
        Self {
            paths,
            probe,
            #[cfg(test)]
            repair_hook: std::sync::Mutex::new(None),
        }
    }

    fn repair_writer(&self, id: &SessionId) -> Result<BlockingWriter, StoreError> {
        #[cfg(test)]
        let hook = self
            .repair_hook
            .lock()
            .map_err(|_| StoreError::internal(StoreErrorCode::Internal, "repair hook poisoned"))?
            .take()
            .unwrap_or_else(|| Box::new(|_| Ok(())));
        #[cfg(not(test))]
        let hook: crate::IoHook = Box::new(|_| Ok(()));
        BlockingWriter::with_observation(self.paths.clone(), id.clone(), (hook, self.probe.clone()))
    }

    /// Returns the exact cold projection's active context. Rebuild requires ownership.
    /// Older bodies and large payloads are resolved from authenticated original
    /// references; only fully inline small contexts avoid all original reads.
    /// # Errors
    /// Returns Busy for an owned dirty/missing cache; damage is never skipped.
    pub fn recent(&self, id: &SessionId, selected: Option<&EventId>) -> Result<RecentContext, StoreError> {
        let checkpoint = self.ensure(id, selected)?;
        self.materialize(&checkpoint.context)
    }

    /// Inspects coverage without materializing original message or metadata bodies.
    /// # Errors
    /// Returns the same repair/ownership errors as `recent`.
    pub fn coverage(&self, id: &SessionId, selected: Option<&EventId>) -> Result<ContextCoverage, StoreError> {
        let context = self.ensure(id, selected)?.context;
        Ok(ContextCoverage::from(&context))
    }

    /// Returns only byte-bounded cached bodies plus explicit omitted-prefix coverage.
    /// A clean hit reads no original payloads. Use `recent` for complete active context.
    /// # Errors
    /// Refuses damage or an owned cache that needs repair.
    pub fn tail_view(&self, id: &SessionId, selected: Option<&EventId>) -> Result<CachedTailView, StoreError> {
        let context = self.ensure(id, selected)?.context;
        Ok(CachedTailView {
            coverage: ContextCoverage::from(&context),
            messages: context.tail,
        })
    }

    pub(crate) fn ensure(&self, id: &SessionId, selected: Option<&EventId>) -> Result<Checkpoint, StoreError> {
        match crate::blobs::checked_file(&self.paths.events_log(id)?, false) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::unknown_session("session original is absent"));
            }
            Err(error) => return Err(error.into()),
        }
        if let Some(checkpoint) = self.load(id, selected)?
            && !crate::cache_io::dirty_present(&self.paths, Some(id))?
        {
            return Ok(checkpoint);
        }
        crate::with_cache_repair(&self.paths, id, || {
            if let Some(checkpoint) = self.load(id, selected)?
                && !crate::cache_io::dirty_present(&self.paths, Some(id))?
            {
                return Ok(checkpoint);
            }
            let mut writer = self.repair_writer(id)?;
            writer.repair_cache(selected, self.probe.clone())?;
            self.load(id, selected)?
                .ok_or_else(|| StoreError::busy("cache changed during repair"))
        })
    }

    /// Listing metadata for one session. Listing never requires, and never
    /// establishes, an execution head: a graph that retains diverged heads has
    /// no single executable context, but its stored metadata stays readable.
    ///
    /// # Errors
    /// Propagates original damage and cache ownership the same way `recent` does.
    pub(crate) fn catalog_entry(&self, id: &SessionId) -> Result<CatalogEntry, StoreError> {
        if let Some(checkpoint) = self.load(id, None)?
            && !crate::cache_io::dirty_present(&self.paths, Some(id))?
        {
            return Ok(CatalogEntry::new(&checkpoint.context, checkpoint.source));
        }
        crate::with_cache_repair(&self.paths, id, || {
            if let Some(checkpoint) = self.load(id, None)?
                && !crate::cache_io::dirty_present(&self.paths, Some(id))?
            {
                return Ok(CatalogEntry::new(&checkpoint.context, checkpoint.source));
            }
            let mut writer = self.repair_writer(id)?;
            match writer.repair_cache(None, self.probe.clone()) {
                Ok(_) => {
                    let checkpoint = self
                        .load(id, None)?
                        .ok_or_else(|| StoreError::busy("cache changed during listing"))?;
                    Ok(CatalogEntry::new(&checkpoint.context, checkpoint.source))
                }
                Err(error) if error.code == StoreErrorCode::ConflictHeads => {
                    writer.publish_metadata(self.probe.clone())
                }
                Err(error) => Err(error),
            }
        })
    }

    pub(crate) fn materialize(&self, context: &CachedContext) -> Result<RecentContext, StoreError> {
        let index = if context.inline_from > 0 {
            Some(Index::parse(&self.probe.read(
                &self.paths.session_dir(&context.session_id)?.join("sparse-index.jsonl"),
            )?)?)
        } else {
            None
        };
        context.materialize((&self.paths, &self.probe), index.as_ref())
    }

    /// Fetches an original event by its indexed byte range, not by full replay.
    /// # Errors
    /// Refuses stale indices, missing identities and damaged originals.
    pub fn event(&self, id: &SessionId, event: &EventId) -> Result<EventEnvelope, StoreError> {
        self.ensure(id, None)?;
        let directory = self.paths.session_dir(id)?;
        let index = Index::parse(&self.probe.read(&directory.join("sparse-index.jsonl"))?)?;
        let row = index
            .rows
            .iter()
            .find(|r| r.id == *event)
            .ok_or_else(|| StoreError::invalid_request(StoreErrorCode::MissingDependency, "event absent"))?;
        crate::index::at((&self.paths, id), row, &self.probe)
    }

    pub(crate) fn load(&self, id: &SessionId, selected: Option<&EventId>) -> Result<Option<Checkpoint>, StoreError> {
        let dir = self.paths.session_dir(id)?;
        // Missing/malformed derived bytes trigger one exclusive rebuild. Unsafe
        // paths are rejected by publication rather than followed or overwritten.
        let checkpoint: Checkpoint = match self
            .probe
            .bounded(&dir.join("checkpoint.json"))
            .and_then(|v| decode(&v))
        {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        if checkpoint.version != 2
            || checkpoint.selected.as_ref() != selected
            || checkpoint.context.session_id != *id
            || checkpoint.source != Stamp::read(&self.paths.events_log(id)?)?
            || Stamp::read(&dir.join("sparse-index.jsonl")).ok().as_ref() != Some(&checkpoint.index)
            || Stamp::read(&dir.join("summary.json")).ok().as_ref() != Some(&checkpoint.summary)
        {
            return Ok(None);
        }
        Ok(Some(checkpoint))
    }

    pub(crate) fn rebuild(
        &self,
        request: (&SessionId, Option<&EventId>),
        disk: &mut Disk,
    ) -> Result<Rebuilt, StoreError> {
        let (id, selected) = request;
        let source_before = Stamp::read(&self.paths.events_log(id)?)?;
        let report = crate::recovery::scan_observed(&self.paths, id, &self.probe)?;
        if let Some(error) = report.damage {
            return Err(error);
        }
        if !report.torn_tail.is_empty() {
            return Err(crate::recovery::corrupt("torn original"));
        }
        let graph = EventGraph::with_blobs(id, &report.events, |event| {
            let reference = event
                .payload_ref
                .as_ref()
                .ok_or_else(|| crate::recovery::corrupt("blob reference absent"))?;
            self.probe.read(&self.paths.blob_file(id, &reference.sha256)?)
        })?;
        let projection = graph.replay(selected)?;
        let mut index = Index::from_events(&report.events)?;
        index.project(&graph, &projection)?;
        let context = CachedContext::build(&graph, &projection, &index)?;
        let tools = ToolHistory::from_messages(projection.messages())?;
        if source_before != Stamp::read(&self.paths.events_log(id)?)? {
            return Err(StoreError::busy("original changed during cache reconstruction"));
        }
        let catalog = self.publish((&context, selected), &index, disk)?;
        Ok(Rebuilt {
            context,
            index,
            tools,
            catalog,
        })
    }

    pub(crate) fn publish(
        &self,
        current: (&CachedContext, Option<&EventId>),
        index: &Index,
        disk: &mut Disk,
    ) -> Result<crate::catalog::RowWrite, StoreError> {
        let (context, selected) = current;
        let dir = self.paths.session_dir(&context.session_id)?;
        publish(disk, &dir.join("sparse-index.jsonl"), &index.bytes()?, &self.probe)?;
        let summary =
            crate::catalog_row::CatalogEntry::new(context, Stamp::read(&self.paths.events_log(&context.session_id)?)?);
        publish(disk, &dir.join("summary.json"), &encode(&summary)?, &self.probe)?;
        let anchor_hash = index
            .rows
            .last()
            .map(|r| r.hash.clone())
            .ok_or_else(|| StoreError::unknown_session("empty log"))?;
        let checkpoint = Checkpoint {
            version: 2,
            source: Stamp::read(&self.paths.events_log(&context.session_id)?)?,
            index: Stamp::read(&dir.join("sparse-index.jsonl"))?,
            summary: Stamp::read(&dir.join("summary.json"))?,
            anchor_hash,
            selected: selected.cloned(),
            context: context.clone(),
        };
        let bytes = encode(&checkpoint)?;
        publish(disk, &dir.join("checkpoint.json"), &bytes, &self.probe)?;
        crate::catalog::publish_row(&self.paths, &summary, disk, &self.probe)
    }
}

impl BlockingWriter {
    /// Metadata row for a graph whose retained heads are ambiguous. Only the
    /// bounded summary and its catalog row are published; no checkpoint, index
    /// projection or head selection is written, so execution still refuses
    /// until the host selects a head.
    ///
    /// # Errors
    /// Propagates original damage, ownership and cache I/O failures.
    pub(crate) fn publish_metadata(&mut self, probe: ReadProbe) -> Result<CatalogEntry, StoreError> {
        let id = self.id.clone();
        let source = Stamp::read(&self.paths.events_log(&id)?)?;
        let report = crate::recovery::scan_observed(&self.paths, &id, &probe)?;
        if let Some(error) = report.damage {
            return Err(error);
        }
        if !report.torn_tail.is_empty() {
            return Err(crate::recovery::corrupt("torn original"));
        }
        let graph = EventGraph::with_blobs(&id, &report.events, |event| {
            let reference = event
                .payload_ref
                .as_ref()
                .ok_or_else(|| crate::recovery::corrupt("blob reference absent"))?;
            probe.read(&self.paths.blob_file(&id, &reference.sha256)?)
        })?;
        if graph.heads().len() < 2 {
            return Err(StoreError::busy("retained heads changed during listing"));
        }
        // The deepest shared record is what every retained head agrees on, so
        // no branch's metadata silently outranks another's. It is a display
        // source only: `head` here is never treated as a selection.
        let shared = graph.shared_head()?;
        let projection = graph.replay(Some(&shared))?;
        let mut index = Index::from_events(&report.events)?;
        index.project(&graph, &projection)?;
        let mut context = CachedContext::build(&graph, &projection, &index)?;
        // Display fields follow the newest persisted record across every
        // retained branch, tie-broken by identity, so a rename or save made on
        // either branch is shown without choosing an execution head.
        let mut retained: BTreeSet<EventId> = BTreeSet::new();
        for head in graph.heads() {
            retained.extend(graph.ancestry(head)?);
        }
        let newest = |wanted: fn(&Payload) -> bool| -> Option<EventId> {
            retained
                .iter()
                .filter(|id| wanted(&graph.payloads[*id]))
                .max_by_key(|id| (graph.event(id).map(|event| event.timestamp), *id))
                .cloned()
        };
        if let Some(id) = newest(|payload| matches!(payload, Payload::Created(_) | Payload::Metadata(_))) {
            let metadata = match &graph.payloads[&id] {
                Payload::Created(payload) => &payload.metadata,
                Payload::Metadata(payload) => &payload.metadata,
                _ => return Err(crate::recovery::corrupt("metadata source absent")),
            };
            context.metadata = crate::cache_values::CachedValue::capture(
                metadata,
                crate::cache_values::PayloadSource {
                    record: index.get(&id)?.reference(),
                    field: crate::cache_values::PayloadField::Metadata,
                },
            )?;
        }
        if let Some(id) = newest(|payload| matches!(payload, Payload::Archive(_))) {
            let Payload::Archive(archive) = &graph.payloads[&id] else {
                return Err(crate::recovery::corrupt("archive source absent"));
            };
            context.archive = Some(crate::cache_values::CachedValue::capture(
                archive,
                crate::cache_values::PayloadSource {
                    record: index.get(&id)?.reference(),
                    field: crate::cache_values::PayloadField::Whole,
                },
            )?);
        }
        if context.first_prompt_title.is_none() {
            // The first prompt is the earliest retained append, not whichever
            // branch the topological order happens to place first.
            let first = retained
                .iter()
                .filter(|id| matches!(graph.payloads[*id], Payload::Appended(_)))
                .min_by_key(|id| (graph.event(id).map(|event| event.timestamp), *id));
            context.first_prompt_title = first.and_then(|id| {
                let Payload::Appended(payload) = &graph.payloads[id] else {
                    return None;
                };
                crate::context_cache::prompt_title(&payload.message)
            });
        }
        let mut entry = CatalogEntry::new(&context, source.clone());
        // Recency still comes from the newest retained tip, so a diverged
        // session keeps its place in activity ordering.
        entry.activity_at = graph
            .heads()
            .iter()
            .filter_map(|head| graph.event(head))
            .map(|event| event.timestamp)
            .max()
            .unwrap_or(context.activity_at);
        if source != Stamp::read(&self.paths.events_log(&id)?)? {
            return Err(StoreError::busy("original changed during listing reconstruction"));
        }
        self.dirty()?;
        let probe = self.state.probe.clone();
        publish(
            &mut self.disk,
            &self.paths.session_dir(&id)?.join("summary.json"),
            &encode(&entry)?,
            &probe,
        )?;
        match crate::catalog::publish_row(&self.paths, &entry, &mut self.disk, &probe)? {
            crate::catalog::RowWrite::Written => Ok(entry),
            // The listing row is this path's whole deliverable: when the shared
            // index is owned elsewhere the listing retries instead of returning a
            // row it never published.
            crate::catalog::RowWrite::Deferred => Err(crate::catalog::deferred_row()),
        }
    }
}
