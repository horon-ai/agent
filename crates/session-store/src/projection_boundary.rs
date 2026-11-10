//! Source-bound compaction coverage; no token budget or provider policy.
use crate::projection_payload::Payload;
use crate::*;
use serde_json::json;

fn invalid(message: &str) -> StoreError {
    StoreError::invalid_request(StoreErrorCode::InvalidBoundary, message)
}

impl EventGraph {
    /// Digest contiguous original `(event_id, hash)` pairs after prior coverage.
    /// Includes the prior compaction identity/hash, never arrival order or clocks.
    ///
    /// # Errors
    /// Refuses missing/non-ancestral coverage or an invalid prior boundary.
    pub fn prefix_digest(
        &self,
        source: &EventId,
        covered: &EventId,
        prior: Option<&EventId>,
    ) -> Result<String, StoreError> {
        let chain = self.ancestry(source)?;
        let end = chain
            .iter()
            .position(|id| id == covered)
            .ok_or_else(|| invalid("coverage not in source chain"))?;
        let mut start = 0;
        let mut parts = Vec::new();
        if let Some(prior_id) = prior {
            if !chain.contains(prior_id) {
                return Err(invalid("prior not in source chain"));
            }
            let Some(Payload::Compacted(previous)) = self.payloads.get(prior_id) else {
                return Err(invalid("prior is not compaction"));
            };
            start = chain
                .iter()
                .position(|id| id == &previous.covered_through_event_id)
                .ok_or_else(|| invalid("prior coverage missing"))?
                .saturating_add(1);
            parts.push(json!({"prior_compaction_id":prior_id,"content_hash":self.events[prior_id].content_hash}));
        }
        if start > end {
            return Err(invalid("coverage must advance contiguously"));
        }
        for id in &chain[start..=end] {
            parts.push(json!({"event_id":id,"content_hash":self.events[id].content_hash}));
        }
        canonical_digest_of_values(&parts)
    }

    pub(crate) fn validate_boundary(
        &self,
        chain: &[EventId],
        payload: &CompactedPayload,
        state: &Projection,
    ) -> Result<usize, StoreError> {
        let source = payload
            .source_head_id
            .as_ref()
            .ok_or_else(|| invalid("source head required"))?;
        let source_pos = chain
            .iter()
            .position(|id| id == source)
            .ok_or_else(|| invalid("source not on selected ancestry"))?;
        let covered = chain
            .iter()
            .position(|id| id == &payload.covered_through_event_id)
            .ok_or_else(|| invalid("coverage not on selected ancestry"))?;
        if covered > source_pos {
            return Err(invalid("coverage follows captured source"));
        }
        if payload.first_kept_event_id.as_ref() != chain.get(covered.saturating_add(1)).filter(|_| covered < source_pos)
        {
            return Err(invalid("first-kept is not the immediate source successor"));
        }
        if state.compaction().map(|(id, _)| id) != payload.prior_compaction_id.as_ref() {
            return Err(invalid("prior is not the currently applied compaction"));
        }
        if let Some((_, previous)) = state.compaction() {
            match previous.outcome {
                CompactionOutcome::Native => {
                    if previous.provider != payload.provider
                        || previous.model != payload.model
                        || previous.route != payload.route
                    {
                        return Err(invalid("prior native window belongs to a different model/route"));
                    }
                }
                CompactionOutcome::Summary | CompactionOutcome::LossyEmergency | CompactionOutcome::Handoff => {}
            }
        }
        for id in &chain[covered.saturating_add(1)..=source_pos] {
            if let Payload::Revised(revision) = &self.payloads[id]
                && chain[..=covered].contains(&revision.target_event_id)
            {
                return Err(invalid("covered message was revised outside the covered prefix"));
            }
        }
        for id in &chain[source_pos.saturating_add(1)..] {
            if !matches!(&self.payloads[id], Payload::Appended(_)) {
                return Err(invalid("captured source changed by more than a message append"));
            }
        }
        if self.prefix_digest(
            source,
            &payload.covered_through_event_id,
            payload.prior_compaction_id.as_ref(),
        )? != payload.source_prefix_digest
        {
            return Err(invalid("source prefix digest mismatch"));
        }
        if payload.provider.is_empty()
            || payload.model.is_empty()
            || payload.route.is_empty()
            || payload.settings_digest.len() != 64
            || !payload
                .settings_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid("invalid model/settings identity"));
        }
        let metadata = state.metadata();
        if metadata.model.as_ref().is_some_and(|m| m != &payload.model)
            || metadata.provider_key.as_ref().is_some_and(|p| p != &payload.provider)
            || metadata.route_api_method.as_ref().is_some_and(|r| r != &payload.route)
        {
            return Err(invalid("compaction identity differs from selected metadata"));
        }
        match payload.outcome {
            CompactionOutcome::Summary | CompactionOutcome::LossyEmergency | CompactionOutcome::Handoff => {
                if payload.summary_text.trim().is_empty() || !payload.native_window.is_empty() {
                    return Err(invalid("text outcome requires nonempty summary and no native window"));
                }
            }
            CompactionOutcome::Native => {
                if !payload.native_window.iter().any(|item| {
                    item.get("type").and_then(|v| v.as_str()) == Some("compaction")
                        && item
                            .get("encrypted_content")
                            .and_then(|v| v.as_str())
                            .is_some_and(|s| !s.is_empty())
                        && item
                            .get("status")
                            .is_none_or(|status| status.as_str() == Some("completed"))
                }) {
                    return Err(invalid("native window lacks a completed compaction artifact"));
                }
            }
        }
        if payload
            .restoration
            .as_ref()
            .is_some_and(|r| r.entries.len() > MAX_RESTORATION_ENTRIES)
        {
            return Err(invalid("restoration manifest exceeds bound"));
        }
        let cut = state
            .messages()
            .iter()
            .take_while(|message| chain[..=covered].contains(&message.event_id))
            .count();
        let latest_user = crate::projection_tools::validate_tools(state.messages(), Some(cut))?;
        if latest_user.is_some_and(|index| index < cut) {
            return Err(invalid("coverage drops latest user intent"));
        }
        Ok(cut)
    }
}
