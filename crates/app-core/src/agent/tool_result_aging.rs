//! Condense large, older tool results in one batch, cache-aware.
//!
//! Every rewrite of an earlier message invalidates the provider's prompt cache
//! from that point on, so results are never aged one per turn. A batch runs
//! only when the rewrite is free (the cache already expired) or clearly worth
//! it (compaction is near and the batch frees a meaningful amount). The full
//! text of each aged result is saved as an `artifact://` the model can read.

use super::Agent;
use crate::message::ContentBlock;
use crate::tool::artifacts;
use std::collections::HashSet;
use std::time::Duration;

/// The newest messages are never aged: they are what the model is working on.
pub(super) const AGING_KEEP_RECENT_MESSAGES: usize = 24;
/// Results at or below this size are left alone.
pub(super) const AGING_MIN_RESULT_BYTES: usize = 8 * 1024;
pub(super) const AGING_KEEP_HEAD_BYTES: usize = 1024;
pub(super) const AGING_KEEP_TAIL_BYTES: usize = 1024;
/// Context fill (of the compaction threshold) at which aging may run on a
/// warm cache.
pub(super) const AGING_PRESSURE_RATIO: f64 = 0.75;
/// A warm-cache batch must free at least this much to pay for the rewrite.
pub(super) const AGING_MIN_PRESSURE_BATCH_BYTES: usize = 64 * 1024;
/// Every aged result starts with this; it also marks a result as already aged.
pub(super) const AGED_MARKER_PREFIX: &str = "[horon: condensed older tool output";
/// Prompt-cache lifetime assumed when the route does not report one.
const DEFAULT_CACHE_TTL_SECS: u64 = 300;

/// Artifact name of an aged tool result in the session artifact store.
const AGED_ARTIFACT_TOOL: &str = "aged-tool-result";

fn is_candidate(content: &str) -> bool {
    content.len() > AGING_MIN_RESULT_BYTES && !content.starts_with(AGED_MARKER_PREFIX)
}

/// Marker first (so the re-aging check is a prefix test), then the kept head
/// and tail of the original.
pub(super) fn aged_text(content: &str, artifact_id: u64) -> String {
    let head_end = artifacts::head_end(content, AGING_KEEP_HEAD_BYTES);
    let tail_start = artifacts::tail_start(content, AGING_KEEP_TAIL_BYTES, head_end);
    let head = content[..head_end].trim_end_matches('\n');
    let tail = &content[tail_start..];
    format!(
        "{AGED_MARKER_PREFIX}: {} of {} bytes omitted. Full output: read file_path=\"{}{}\".]\n{head}\n...\n{tail}",
        tail_start - head_end,
        content.len(),
        artifacts::ARTIFACT_SCHEME,
        artifact_id,
    )
}

impl Agent {
    /// Age eligible tool results when the cache is cold or compaction is
    /// near. Returns how many results were condensed.
    pub(crate) fn maybe_age_tool_results(&mut self) -> usize {
        if !crate::config::config().tools.tool_result_aging || !self.provider.uses_horon_agent_compaction() {
            return 0;
        }
        let ttl = crate::provider::cache_ttl_for_provider_model(
            &self.kv_cache_provider_identity(),
            Some(&self.provider.model()),
        )
        .unwrap_or(DEFAULT_CACHE_TTL_SECS);
        let cold = self
            .last_provider_response_at
            .is_some_and(|at| at.elapsed() >= Duration::from_secs(ttl));

        let compaction = self.registry.compaction();
        let (compacted, near_threshold) = {
            // A running summary job captured the current prefix; rewriting it
            // now would fail that job's prefix check.
            let Ok(manager) = compaction.try_read() else {
                return 0;
            };
            if manager.is_compacting() {
                return 0;
            }
            let threshold = manager.compaction_threshold_tokens();
            let effective = manager.effective_token_count_with(self.session.provider_messages());
            (
                manager.compacted_count(),
                threshold > 0 && effective as f64 >= threshold as f64 * AGING_PRESSURE_RATIO,
            )
        };
        if !cold && !near_threshold {
            return 0;
        }

        let messages = self.session.provider_messages();
        let end = messages.len().saturating_sub(AGING_KEEP_RECENT_MESSAGES);
        let mut ids: HashSet<String> = HashSet::new();
        let mut candidate_bytes = 0usize;
        for message in messages.get(compacted..end).unwrap_or(&[]) {
            for block in &message.content {
                if let ContentBlock::ToolResult {
                    tool_use_id, content, ..
                } = block
                    && is_candidate(content)
                {
                    ids.insert(tool_use_id.clone());
                    candidate_bytes += content.len();
                }
            }
        }
        if ids.is_empty() {
            return 0;
        }
        let trigger = if cold {
            "cold"
        } else if candidate_bytes >= AGING_MIN_PRESSURE_BATCH_BYTES {
            "pressure"
        } else {
            return 0;
        };

        let targets: Vec<_> = self
            .session
            .active_context()
            .iter()
            .filter(|message| {
                message.content.iter().any(|block| {
                    matches!(block, ContentBlock::ToolResult { tool_use_id, content, .. }
                        if ids.contains(tool_use_id) && is_candidate(content))
                })
            })
            .cloned()
            .collect();

        let mut aged = 0usize;
        let mut removed_bytes = 0usize;
        for mut message in targets {
            let mut message_aged = 0usize;
            let mut message_removed = 0usize;
            for block in &mut message.content {
                let ContentBlock::ToolResult {
                    tool_use_id, content, ..
                } = block
                else {
                    continue;
                };
                if !ids.contains(tool_use_id.as_str()) || !is_candidate(content) {
                    continue;
                }
                match artifacts::store(&self.session.id, AGED_ARTIFACT_TOOL, content) {
                    Ok(artifact_id) => {
                        let condensed = aged_text(content, artifact_id);
                        message_removed += content.len().saturating_sub(condensed.len());
                        *content = condensed;
                        message_aged += 1;
                    }
                    Err(error) => crate::logging::warn(&format!(
                        "[tool-result-aging] kept result {tool_use_id} in full: artifact store failed: {error}"
                    )),
                }
            }
            if message_aged == 0 {
                continue;
            }
            if let Err(error) = self.session.revise_message(message) {
                crate::logging::warn(&format!("[tool-result-aging] stopped the batch: {error}"));
                break;
            }
            aged += message_aged;
            removed_bytes += message_removed;
        }
        if aged == 0 {
            return 0;
        }
        if let Err(error) = self.session.save() {
            crate::logging::warn(&format!(
                "[tool-result-aging] saving the condensed session failed; the next save records it: {error}"
            ));
        }
        if let Ok(mut manager) = compaction.try_write() {
            manager.note_history_condensed();
        }
        crate::cache_invalidation::record(
            "tool result aging",
            format!("{aged} results, {removed_bytes} bytes, trigger={trigger}"),
        );
        crate::tool::read_state::forget_session(&self.session.id);
        horon_agent_perf_counters::record_tool_result_aging(aged as u64, removed_bytes as u64);
        crate::logging::info(&format!(
            "[tool-result-aging] condensed {aged} older tool results ({removed_bytes} bytes, trigger={trigger}) in session {}",
            self.session.id
        ));
        aged
    }
}

#[cfg(test)]
mod tests;
