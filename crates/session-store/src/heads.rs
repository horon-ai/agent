//! Immutable original graph. Selection is supplied by the host, never an event.
use crate::projection_payload::Payload;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub struct EventGraph {
    pub(crate) session_id: SessionId,
    pub(crate) events: BTreeMap<EventId, EventEnvelope>,
    pub(crate) payloads: BTreeMap<EventId, Payload>,
    order: Vec<EventId>,
    heads: Vec<EventId>,
}

impl EventGraph {
    /// Validate an inline event set, deduplicate identical records, and sort parents first.
    ///
    /// # Errors
    /// Refuses malformed envelopes, conflicting identities, missing parents and cycles.
    pub fn from_events(id: &SessionId, events: &[EventEnvelope]) -> Result<Self, StoreError> {
        Self::with_blobs(id, events, |_| {
            Err(StoreError::invalid_request(
                StoreErrorCode::MissingDependency,
                "blob resolver required",
            ))
        })
    }

    /// Resolve real blob bytes and verify their hash/size before decoding.
    ///
    /// # Errors
    /// Propagates resolver errors and rejects invalid graph or payload contracts.
    pub fn with_blobs(
        id: &SessionId,
        events: &[EventEnvelope],
        mut resolve: impl FnMut(&EventEnvelope) -> Result<Vec<u8>, StoreError>,
    ) -> Result<Self, StoreError> {
        let mut records = BTreeMap::<EventId, EventEnvelope>::new();
        let mut payloads = BTreeMap::new();
        for event in events {
            event.validate()?;
            event.verify_hash()?;
            if event.session_id != *id {
                return Err(StoreError::invalid_request(
                    StoreErrorCode::InvalidReference,
                    "cross-session event",
                ));
            }
            if let Some(old) = records.get(&event.event_id) {
                if old.content_hash != event.content_hash {
                    return Err(StoreError::hash_conflict(
                        "duplicate identity has different hash",
                        Vec::new(),
                    ));
                }
            } else {
                let value = match event.body()? {
                    EventBody::Inline => event.payload_value()?.clone(),
                    EventBody::Blob => event.hydrate_from_blob(&resolve(event)?)?.payload,
                };
                payloads.insert(event.event_id.clone(), Payload::parse(event, &value)?);
                records.insert(event.event_id.clone(), event.clone());
            }
        }
        let mut children = BTreeMap::<EventId, BTreeSet<EventId>>::new();
        let mut ready = BTreeSet::new();
        let mut degrees = BTreeMap::new();
        let mut heads: BTreeSet<_> = records.keys().cloned().collect();
        let mut initial_roots = 0usize;
        for event in records.values() {
            let mut dependencies = BTreeSet::new();
            if let Some(parent) = &event.parent_event_id {
                dependencies.insert(parent.clone());
                heads.remove(parent);
            }
            if let Payload::Created(created) = &payloads[&event.event_id] {
                if event.parent_event_id.is_some() {
                    return Err(StoreError::invalid_request(
                        StoreErrorCode::InvalidReference,
                        "SessionCreated must be a root",
                    ));
                }
                match (created.branch_root, &created.reset_provenance) {
                    (false, None) => {
                        initial_roots += 1;
                    }
                    (true, Some(reset)) => {
                        let source = reset.cleared_head_event_id.as_ref().ok_or_else(|| {
                            StoreError::invalid_request(StoreErrorCode::InvalidReference, "reset requires source head")
                        })?;
                        dependencies.insert(source.clone());
                    }
                    _ => {
                        return Err(StoreError::invalid_request(
                            StoreErrorCode::InvalidReference,
                            "inconsistent root provenance",
                        ));
                    }
                }
            }
            for parent in &dependencies {
                if !records.contains_key(parent) {
                    return Err(StoreError::invalid_request(
                        StoreErrorCode::MissingDependency,
                        "missing parent/reset source",
                    ));
                }
                children
                    .entry(parent.clone())
                    .or_default()
                    .insert(event.event_id.clone());
            }
            degrees.insert(event.event_id.clone(), dependencies.len());
            if dependencies.is_empty() {
                ready.insert(event.event_id.clone());
            }
        }
        if !records.is_empty() && initial_roots != 1 {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidReference,
                "exactly one initial session root required",
            ));
        }
        let mut order = Vec::with_capacity(records.len());
        while let Some(id) = ready.pop_first() {
            if let Some(next) = children.get(&id) {
                for child in next {
                    let degree = degrees
                        .get_mut(child)
                        .ok_or_else(|| StoreError::internal(StoreErrorCode::Internal, "missing graph degree"))?;
                    *degree = degree
                        .checked_sub(1)
                        .ok_or_else(|| StoreError::internal(StoreErrorCode::Internal, "invalid graph degree"))?;
                    if *degree == 0 {
                        ready.insert(child.clone());
                    }
                }
            }
            order.push(id);
        }
        if order.len() != records.len() {
            return Err(StoreError::invalid_request(
                StoreErrorCode::CyclicHistory,
                "parent cycle",
            ));
        }
        Ok(Self {
            session_id: id.clone(),
            events: records,
            payloads,
            order,
            heads: heads.into_iter().collect(),
        })
    }

    /// Read complete durable originals. Never execute a damaged or torn prefix.
    ///
    /// # Errors
    /// Propagates scan damage, incomplete tails, missing blobs and graph failures.
    pub fn open(paths: &SessionPaths, id: &SessionId) -> Result<Self, StoreError> {
        let report = full_scan(paths, id)?;
        if let Some(error) = report.damage {
            return Err(error);
        }
        if !report.torn_tail.is_empty() {
            return Err(StoreError::internal(
                StoreErrorCode::CorruptLog,
                "recover torn tail before replay",
            ));
        }
        Self::with_blobs(id, &report.events, |event| {
            let reference = event
                .payload_ref
                .as_ref()
                .ok_or_else(|| StoreError::invalid_request(StoreErrorCode::PayloadRefMissing, "missing blob ref"))?;
            crate::blobs::read_file(&paths.blob_file(id, &reference.sha256)?)
        })
    }

    pub fn ordered_ids(&self) -> &[EventId] {
        &self.order
    }
    pub fn heads(&self) -> &[EventId] {
        &self.heads
    }
    pub fn event(&self, id: &EventId) -> Option<&EventEnvelope> {
        self.events.get(id)
    }
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Resolve a local selection; an ancestor is a legitimate rewind position.
    ///
    /// # Errors
    /// Refuses an absent selection or ambiguous unselected heads.
    pub fn selected_head(&self, selected: Option<&EventId>) -> Result<EventId, StoreError> {
        if let Some(id) = selected {
            return self.events.get(id).map(|_| id.clone()).ok_or_else(|| {
                StoreError::invalid_request(StoreErrorCode::MissingDependency, "selected head is absent")
            });
        }
        match self.heads.as_slice() {
            [id] => Ok(id.clone()),
            [] => Err(StoreError::unknown_session("empty event graph")),
            _ => Err(
                StoreError::invalid_request(StoreErrorCode::ConflictHeads, "select a head before execution")
                    .with_details(StoreErrorDetails {
                        conflict_heads: self.heads.iter().map(|id| id.as_str().to_owned()).collect(),
                        ..StoreErrorDetails::default()
                    }),
            ),
        }
    }

    /// Deepest event every retained head agrees on. Listing metadata is taken
    /// from it because a diverged graph has no single canonical execution head.
    ///
    /// # Errors
    /// Refuses an empty graph or retained heads with no common root.
    pub fn shared_head(&self) -> Result<EventId, StoreError> {
        let mut retained = self.heads.iter();
        let Some(first) = retained.next() else {
            return Err(StoreError::unknown_session("empty event graph"));
        };
        let mut shared = self.ancestry(first)?;
        for head in retained {
            let chain = self.ancestry(head)?;
            let common = shared
                .iter()
                .zip(chain.iter())
                .take_while(|(left, right)| left == right)
                .count();
            shared.truncate(common);
            if shared.is_empty() {
                return Err(crate::recovery::corrupt("retained heads share no root"));
            }
        }
        shared
            .pop()
            .ok_or_else(|| crate::recovery::corrupt("retained heads share no root"))
    }

    /// Root-to-head parent ancestry, including all metadata and compaction records.
    ///
    /// # Errors
    /// Refuses an unknown head.
    pub fn ancestry(&self, head: &EventId) -> Result<Vec<EventId>, StoreError> {
        let mut chain = Vec::new();
        let mut cursor = Some(head);
        while let Some(id) = cursor {
            let event = self
                .events
                .get(id)
                .ok_or_else(|| StoreError::invalid_request(StoreErrorCode::MissingDependency, "head is absent"))?;
            chain.push(id.clone());
            cursor = event.parent_event_id.as_ref();
        }
        chain.reverse();
        Ok(chain)
    }
}
