//! Bounded body cache plus complete active coverage in the original offset index.
use crate::cache_values::{CachedValue, INLINE_TAIL_BYTES, PayloadField, PayloadSource};
use crate::index::Index;
use crate::projection_payload::Payload;
use crate::*;
use horon_agent_time::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub activity_at: Timestamp,
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
        let active = projection.active_messages();
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
            active_count: active.len(),
            inline_from: 0,
            tail: Vec::new(),
        };
        // `push` fills the title from the oldest active message that carries one
        // when the full message list yielded none.
        if context.first_prompt_title.is_none() {
            context.first_prompt_title = active.iter().find_map(|m| prompt_title(&m.message));
        }
        context.inline_from = inline_suffix(active, index)?;
        context.tail = active[context.inline_from..].to_vec();
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

/// First resident index of the cold inline suffix.
///
/// [`CachedContext::push`] leaves the longest suffix whose canonical array
/// bytes fit in [`INLINE_TAIL_BYTES`]: it only appends the newest message and
/// drops from the front while the array is over budget, and a blob-backed
/// message or one that cannot fit even alone clears the run. That fixpoint is
/// the same suffix found by walking back from the newest active message once,
/// so a cold rebuild never re-serializes the tail for every earlier message.
fn inline_suffix(active: &[ProjectedMessage], index: &Index) -> Result<usize, StoreError> {
    if active.is_empty() {
        return Ok(0);
    }
    let mut rows = HashMap::with_capacity(index.rows.len());
    for row in &index.rows {
        // `Index::get` reports the first row carrying an identity.
        rows.entry(&row.id).or_insert(row);
    }
    // Complete coverage: every active message still names an indexed original
    // with a resident source, whether or not it survives into the suffix.
    let mut after_blob = 0;
    for (position, message) in active.iter().enumerate() {
        let row = rows
            .get(&message.event_id)
            .ok_or_else(|| crate::recovery::corrupt("indexed event absent"))?;
        let source = row
            .active
            .as_ref()
            .ok_or_else(|| crate::recovery::corrupt("active source absent"))?;
        if source.source.blob.is_some() {
            after_blob = position + 1;
        }
    }
    // The empty array is `[]`; each message adds its bytes and, after the
    // first resident one, the separating comma.
    let mut array_bytes = 2usize;
    let mut start = active.len();
    for message in active[after_blob..].iter().rev() {
        let separator = usize::from(start < active.len());
        let next = array_bytes
            .saturating_add(separator)
            .saturating_add(crate::cache_io::json_len(message)?);
        if next > INLINE_TAIL_BYTES {
            break;
        }
        array_bytes = next;
        start -= 1;
    }
    Ok(start)
}

#[cfg(test)]
#[path = "context_cache_tests.rs"]
mod tests;
