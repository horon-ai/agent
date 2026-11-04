//! Derived branch state. No runtime process, socket or executable cwd is restored.
use crate::projection_payload::Payload;
use crate::*;
use horon_agent_session_types::StoredMessage;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectedMessage {
    pub event_id: EventId,
    pub message: StoredMessage,
}

#[derive(Debug)]
pub struct Projection {
    pub(crate) session_id: SessionId,
    pub(crate) head: EventId,
    pub(crate) messages: Vec<ProjectedMessage>,
    metadata: SessionMetadata,
    archive: Option<ArchiveChangedPayload>,
    auxiliary: Vec<(EventId, AuxiliaryRecordedPayload)>,
    compaction: Option<(EventId, CompactedPayload)>,
    blocked: Vec<(EventId, CompactionBlockedPayload)>,
    active_start: usize,
}

impl Projection {
    pub fn head(&self) -> &EventId {
        &self.head
    }
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
    pub fn messages(&self) -> &[ProjectedMessage] {
        &self.messages
    }
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }
    pub fn archive(&self) -> Option<&ArchiveChangedPayload> {
        self.archive.as_ref()
    }
    pub fn auxiliary(&self) -> &[(EventId, AuxiliaryRecordedPayload)] {
        &self.auxiliary
    }
    pub fn compaction(&self) -> Option<&(EventId, CompactedPayload)> {
        self.compaction.as_ref()
    }
    pub fn blocked(&self) -> &[(EventId, CompactionBlockedPayload)] {
        &self.blocked
    }
    /// Full originals stay available through `messages`; only this slice is compacted.
    pub fn active_messages(&self) -> &[ProjectedMessage] {
        &self.messages[self.active_start..]
    }
}

impl EventGraph {
    /// Replay a selected parent chain, never sibling metadata or revisions.
    ///
    /// # Errors
    /// Refuses ambiguous heads, invalid references/boundaries, broken tools and unknown kinds.
    pub fn replay(&self, selected: Option<&EventId>) -> Result<Projection, StoreError> {
        self.replay_checked(selected, false)
    }

    // Exchange retains unknown events without granting permission to execute them.
    // Known events after an unknown event must still have every reference checked.
    pub(crate) fn replay_checked(
        &self,
        selected: Option<&EventId>,
        preserve_unknown: bool,
    ) -> Result<Projection, StoreError> {
        let head = self.selected_head(selected)?;
        let chain = self.ancestry(&head)?;
        let mut state = Projection {
            session_id: self.session_id.clone(),
            head,
            messages: Vec::new(),
            metadata: SessionMetadata::default(),
            archive: None,
            auxiliary: Vec::new(),
            compaction: None,
            blocked: Vec::new(),
            active_start: 0,
        };
        for (position, id) in chain.iter().enumerate() {
            match &self.payloads[id] {
                Payload::Created(payload) => state.metadata = payload.metadata.clone(),
                Payload::Appended(payload) => state.messages.push(ProjectedMessage {
                    event_id: id.clone(),
                    message: payload.message.clone(),
                }),
                Payload::Revised(payload) => {
                    if !self.events.contains_key(&payload.target_event_id) {
                        return Err(StoreError::invalid_request(
                            StoreErrorCode::MissingDependency,
                            "revision target absent",
                        ));
                    }
                    let target = state
                        .messages
                        .iter_mut()
                        .find(|m| m.event_id == payload.target_event_id)
                        .ok_or_else(|| {
                            StoreError::invalid_request(
                                StoreErrorCode::InvalidReference,
                                "revision target is not an ancestor message append",
                            )
                        })?;
                    target.message = payload.replacement.clone();
                    // A revised conversation must be re-compacted; old coverage is not reused.
                    state.compaction = None;
                    state.active_start = 0;
                }
                Payload::Metadata(payload) => {
                    if state.metadata.model != payload.metadata.model
                        || state.metadata.provider_key != payload.metadata.provider_key
                        || state.metadata.route_api_method != payload.metadata.route_api_method
                    {
                        state.compaction = None;
                        state.active_start = 0;
                    }
                    state.metadata = payload.metadata.clone();
                }
                Payload::Archive(payload) => state.archive = Some(payload.clone()),
                Payload::Auxiliary(payload) => state.auxiliary.push((id.clone(), payload.clone())),
                Payload::Blocked(payload) => {
                    let source = payload.source_head_id.as_ref().ok_or_else(|| {
                        StoreError::invalid_request(StoreErrorCode::InvalidReference, "blocked event has no source")
                    })?;
                    if !chain[..position].contains(source) {
                        return Err(StoreError::invalid_request(
                            StoreErrorCode::InvalidReference,
                            "blocked source not on ancestry",
                        ));
                    }
                    state.blocked.push((id.clone(), payload.clone()));
                }
                Payload::Compacted(payload) => {
                    state.active_start = self.validate_boundary(&chain[..position], payload, &state)?;
                    state.compaction = Some((id.clone(), payload.clone()));
                }
                Payload::Unknown => {
                    if !preserve_unknown {
                        self.events[id].require_projectable()?;
                    }
                }
            }
        }
        crate::projection_tools::validate_tools(&state.messages, None)?;
        Ok(state)
    }
}
