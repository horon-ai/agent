//! Bounded body cache plus complete active coverage in the original offset index.
use crate::cache_values::{CachedValue, INLINE_TAIL_BYTES, PayloadField, PayloadSource};
use crate::index::Index;
use crate::projection_payload::Payload;
use crate::*;
use serde::{Deserialize, Serialize};

pub(crate) fn prompt_title(message: &horon_agent_session_types::StoredMessage) -> Option<String> {
    use horon_agent_message_types::{ContentBlock, Role};
    if message.role != Role::User || message.display_role.is_some() {
        return None;
    }
    let mut texts = message.content.iter().filter_map(|block| match block {
        ContentBlock::Text { text, .. } => Some(text.as_str()),
        _ => None,
    });
    let first = texts.next()?;
    if first.trim_start().starts_with("<system-reminder>") {
        return None;
    }
    std::iter::once(first)
        .chain(texts)
        .find_map(horon_agent_session_types::prompt_title)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CachedContext {
    pub session_id: SessionId,
    pub head: EventId,
    pub metadata: CachedValue<SessionMetadata>,
    pub archive: Option<CachedValue<ArchiveChangedPayload>>,
    pub compaction: Option<(EventId, CachedValue<CompactedPayload>)>,
    pub activity_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub first_prompt_title: Option<String>,
    pub active_count: usize,
    pub inline_from: usize,
    pub tail: Vec<ProjectedMessage>,
}

impl CachedContext {
    pub fn build(graph: &EventGraph, projection: &Projection, index: &Index) -> Result<Self, StoreError> {
        let chain = graph.ancestry(projection.head())?;
        let metadata_id = chain
            .iter()
            .rev()
            .find(|id| matches!(graph.payloads[*id], Payload::Created(_) | Payload::Metadata(_)))
            .ok_or_else(|| crate::recovery::corrupt("metadata source absent"))?;
        let metadata = CachedValue::capture(
            projection.metadata(),
            PayloadSource {
                record: index.get(metadata_id)?.reference(),
                field: PayloadField::Metadata,
            },
        )?;
        let archive = match projection.archive() {
            Some(value) => {
                let id = chain
                    .iter()
                    .rev()
                    .find(|id| matches!(graph.payloads[*id], Payload::Archive(_)))
                    .ok_or_else(|| crate::recovery::corrupt("archive source absent"))?;
                Some(CachedValue::capture(
                    value,
                    PayloadSource {
                        record: index.get(id)?.reference(),
                        field: PayloadField::Whole,
                    },
                )?)
            }
            None => None,
        };
        let compaction = match projection.compaction() {
            Some((id, value)) => Some((
                id.clone(),
                CachedValue::capture(
                    value,
                    PayloadSource {
                        record: index.get(id)?.reference(),
                        field: PayloadField::Whole,
                    },
                )?,
            )),
            None => None,
        };
        let mut context = Self {
            session_id: projection.session_id().clone(),
            head: projection.head().clone(),
            metadata,
            archive,
            compaction,
            activity_at: graph
                .event(projection.head())
                .ok_or_else(|| crate::recovery::corrupt("head absent"))?
                .timestamp,
            first_prompt_title: projection.messages().iter().find_map(|m| prompt_title(&m.message)),
            active_count: 0,
            inline_from: 0,
            tail: Vec::new(),
        };
        for message in projection.active_messages() {
            let active = index
                .get(&message.event_id)?
                .active
                .as_ref()
                .ok_or_else(|| crate::recovery::corrupt("active source absent"))?;
            context.push(message, active.source.blob.is_none())?;
        }
        Ok(context)
    }

    pub fn push(&mut self, message: &ProjectedMessage, inline_original: bool) -> Result<(), StoreError> {
        if self.first_prompt_title.is_none() {
            self.first_prompt_title = prompt_title(&message.message);
        }
        self.active_count = self
            .active_count
            .checked_add(1)
            .ok_or_else(|| crate::recovery::corrupt("active count overflow"))?;
        if !inline_original
            || serde_json::to_vec(message).map_err(crate::cache_io::json_error)?.len() > INLINE_TAIL_BYTES
        {
            self.tail.clear();
        } else {
            self.tail.push(message.clone());
            while serde_json::to_vec(&self.tail)
                .map_err(crate::cache_io::json_error)?
                .len()
                > INLINE_TAIL_BYTES
            {
                self.tail.remove(0);
            }
        }
        self.inline_from = self
            .active_count
            .checked_sub(self.tail.len())
            .ok_or_else(|| crate::recovery::corrupt("invalid inline coverage"))?;
        Ok(())
    }

    pub fn materialize(
        &self,
        store: (&SessionPaths, &ReadProbe),
        index: Option<&Index>,
    ) -> Result<crate::checkpoint::RecentContext, StoreError> {
        let (paths, probe) = store;
        let source = (paths, &self.session_id);
        let mut tail = Vec::new();
        if self.inline_from > 0 {
            let index = index.ok_or_else(|| crate::recovery::corrupt("active index required"))?;
            let mut active: Vec<_> = index
                .rows
                .iter()
                .filter_map(|r| r.active.as_ref().map(|a| (r, a)))
                .collect();
            active.sort_by_key(|(_, a)| a.order);
            if active.len() != self.active_count || active.iter().enumerate().any(|(n, (_, a))| n != a.order) {
                return Err(crate::recovery::corrupt("active index coverage mismatch"));
            }
            for (row, active) in active.into_iter().take(self.inline_from) {
                let payload = PayloadSource {
                    record: active.source.clone(),
                    field: if active.revised {
                        PayloadField::Replacement
                    } else {
                        PayloadField::Message
                    },
                };
                tail.push(ProjectedMessage {
                    event_id: row.id.clone(),
                    message: payload.resolve(source, probe)?,
                });
            }
        }
        tail.extend_from_slice(&self.tail);
        if tail.len() != self.active_count {
            return Err(crate::recovery::corrupt("active context coverage mismatch"));
        }
        let compaction = match &self.compaction {
            Some((id, value)) => Some((id.clone(), value.resolve(source, probe)?)),
            None => None,
        };
        let archive = match &self.archive {
            Some(value) => Some(value.resolve(source, probe)?),
            None => None,
        };
        Ok(crate::checkpoint::RecentContext {
            session_id: self.session_id.clone(),
            head: self.head.clone(),
            metadata: self.metadata.resolve(source, probe)?,
            archive,
            compaction,
            tail,
            activity_at: self.activity_at,
        })
    }
}
