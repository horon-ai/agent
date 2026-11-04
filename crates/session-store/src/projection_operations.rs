//! Host-local selection and non-destructive event construction.
use crate::*;
use chrono::{DateTime, Utc};
use horon_agent_session_types::RenderedMessage;
use serde_json::json;

/// Identity reserved before a new event is passed to the durable writer.
pub struct EventStamp {
    pub event_id: EventId,
    pub origin_id: OriginId,
    pub timestamp: DateTime<Utc>,
}

/// Identity of a genuinely independent fork, not another branch.
pub struct ForkRequest {
    pub session_id: SessionId,
    pub origin_id: OriginId,
    pub timestamp: DateTime<Utc>,
}

/// A rendered row mapped to the exact current projection. Construct through
/// `rewind_point`; duplicate rendered entries intentionally map to the same event.
#[derive(Debug)]
pub struct RewindPoint {
    session_id: SessionId,
    source: EventId,
    target: EventId,
    visible_count: usize,
    index: usize,
}

/// Ephemeral host state. Intentionally has no Serialize/Deserialize implementation.
#[derive(Debug, Default)]
pub struct LocalHead {
    selected: Option<EventId>,
    undo: Option<(SessionId, EventId, usize)>,
}

impl LocalHead {
    pub fn selected(&self) -> Option<&EventId> {
        self.selected.as_ref()
    }

    /// Explicit selection validates executability before changing local state.
    ///
    /// # Errors
    /// Returns the graph's replay error without changing the previous selection.
    pub fn select(&mut self, graph: &EventGraph, id: &EventId) -> Result<(), StoreError> {
        graph.replay(Some(id))?;
        self.selected = Some(id.clone());
        self.undo = None;
        Ok(())
    }

    /// Restore an ancestor and remember the old head for undo.
    ///
    /// # Errors
    /// Refuses a stale rendered mapping or a non-executable target.
    pub fn rewind(&mut self, graph: &EventGraph, point: &RewindPoint) -> Result<usize, StoreError> {
        if graph.session_id() != &point.session_id || graph.selected_head(self.selected())? != point.source {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidReference,
                "stale rewind view",
            ));
        }
        graph.replay(Some(&point.target))?;
        self.undo = Some((point.session_id.clone(), point.source.clone(), point.visible_count));
        self.selected = Some(point.target.clone());
        Ok(point.visible_count.saturating_sub(point.index))
    }

    /// Restore the pre-rewind head; current count comes from the current renderer.
    ///
    /// # Errors
    /// Refuses missing undo state, another session, or a no-longer-valid old head.
    pub fn undo(&mut self, graph: &EventGraph, current_visible: usize) -> Result<usize, StoreError> {
        let (session, head, count) = self
            .undo
            .as_ref()
            .ok_or_else(|| StoreError::invalid_request(StoreErrorCode::InvalidReference, "no rewind to undo"))?;
        if session != graph.session_id() {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidReference,
                "undo belongs to another session",
            ));
        }
        graph.replay(Some(head))?;
        let restored = count.saturating_sub(current_visible);
        self.selected = Some(head.clone());
        self.undo = None;
        Ok(restored)
    }
}

impl Projection {
    /// Use the actual renderer output, including compacted-history windows and
    /// reasoning-display policy. Never infer a visible index from raw message count.
    ///
    /// # Errors
    /// Refuses zero/out-of-range indices, missing stored indices and reordered rows.
    pub fn rewind_point(&self, rows: &[RenderedMessage], index: usize) -> Result<RewindPoint, StoreError> {
        let invalid =
            || StoreError::invalid_request(StoreErrorCode::InvalidVisibleIndex, "invalid rendered transcript index");
        let mut indices = Vec::new();
        for row in rows
            .iter()
            .filter(|row| matches!(row.role.as_str(), "user" | "assistant"))
        {
            let stored = row.stored_index.ok_or_else(invalid)?;
            if stored >= self.messages.len() || indices.last().is_some_and(|last| *last > stored) {
                return Err(invalid());
            }
            indices.push(stored);
        }
        let stored = indices
            .get(index.checked_sub(1).ok_or_else(invalid)?)
            .ok_or_else(invalid)?;
        Ok(RewindPoint {
            session_id: self.session_id.clone(),
            source: self.head.clone(),
            target: self.messages[*stored].event_id.clone(),
            visible_count: indices.len(),
            index,
        })
    }

    /// Copy the current logical conversation (including its complete tool groups),
    /// not superseded revisions, into independent new message/event identities.
    ///
    /// # Errors
    /// Refuses the source session identity and propagates sealing failures.
    pub fn fork(&self, request: &ForkRequest) -> Result<Vec<SealedEvent>, StoreError> {
        if request.session_id == self.session_id {
            return Err(StoreError::invalid_request(
                StoreErrorCode::InvalidReference,
                "fork needs a different session",
            ));
        }
        let mut metadata = self.metadata().clone();
        metadata.parent_id = Some(self.session_id.as_str().to_owned());
        metadata.provider_session_id = None;
        metadata.model_usage_turn_id = None;
        metadata.short_name = None;
        metadata.created_at = request.timestamp;
        metadata.updated_at = request.timestamp;
        let root = EventId::new_v4();
        let mut events = vec![
            EventDraft::new(
                request.session_id.clone(),
                root.clone(),
                None,
                request.origin_id.clone(),
                request.timestamp,
                EventKind::new(KnownEventKind::SessionCreated),
                json!(SessionCreatedPayload {
                    branch_root: false,
                    reset_provenance: None,
                    metadata
                }),
            )
            .seal()?,
        ];
        let mut parent = root;
        for item in &self.messages {
            let id = EventId::new_v4();
            let mut message = item.message.clone();
            message.id = id.as_str().to_owned();
            events.push(
                EventDraft::new(
                    request.session_id.clone(),
                    id.clone(),
                    Some(parent),
                    request.origin_id.clone(),
                    request.timestamp,
                    EventKind::new(KnownEventKind::MessageAppended),
                    json!(MessageAppendedPayload { message }),
                )
                .seal()?,
            );
            parent = id;
        }
        events.push(
            EventDraft::new(
                request.session_id.clone(),
                EventId::new_v4(),
                Some(parent),
                request.origin_id.clone(),
                request.timestamp,
                EventKind::new(KnownEventKind::AuxiliaryRecorded),
                json!({"aux_kind":"legacy","schema_version":1,"value":{"kind":"fork_provenance",
                "source_session_id":self.session_id,"source_head_event_id":self.head}}),
            )
            .seal()?,
        );
        Ok(events)
    }
}

impl EventGraph {
    /// Prepare a same-session empty branch root. The caller commits it with the
    /// durable writer and selects it locally; this does not change Clear wire behavior.
    ///
    /// # Errors
    /// Refuses ambiguous/non-executable source or reused event identity.
    pub fn clear(&self, selected: Option<&EventId>, stamp: &EventStamp) -> Result<SealedEvent, StoreError> {
        let source = self.replay(selected)?;
        if self.events.contains_key(&stamp.event_id) {
            return Err(StoreError::hash_conflict(
                "reset event identity already exists",
                Vec::new(),
            ));
        }
        let mut metadata = source.metadata().clone();
        metadata.provider_session_id = None;
        metadata.model_usage_turn_id = None;
        metadata.updated_at = stamp.timestamp;
        EventDraft::new(
            self.session_id.clone(),
            stamp.event_id.clone(),
            None,
            stamp.origin_id.clone(),
            stamp.timestamp,
            EventKind::new(KnownEventKind::SessionCreated),
            json!(SessionCreatedPayload {
                branch_root: true,
                metadata,
                reset_provenance: Some(ResetProvenance {
                    reset_at: stamp.timestamp,
                    cleared_head_event_id: Some(source.head),
                    reason: Some("clear".into())
                })
            }),
        )
        .seal()
    }
}
