//! Release the derived caches of an idle session.
//!
//! Only allocations that are reproducible from the canonical transcript are
//! released: the provider projection and the tool-output index. Frozen prompts,
//! the locked tool surface, rewind undo state and provider handles stay
//! resident, so waking is the next turn rebuilding those two caches with no
//! disk dependency, no lifecycle hook and no environment resnapshot.

use super::*;

/// What one release dropped, for the server's log line.
///
/// The byte count is the serialized size of the provider projection as the
/// session last measured it, so it is comparable with `server:memory`; the
/// entry count is the number of tool ids the index held.
#[derive(Debug)]
pub(crate) struct IdleCacheRelease {
    pub provider_cache_bytes: usize,
    pub tool_index_entries: usize,
}

impl Agent {
    /// Drop the derived provider projection and tool index of an idle session.
    ///
    /// The caller must hold the Agent lock and have established that no turn,
    /// compaction, alert, resume or tool is waiting on this session. Never
    /// checkpoint here: the release changes nothing on disk, so a save that a
    /// migration lease would have skipped cannot hide a lost original.
    pub(crate) fn hibernate_idle_caches(&mut self) -> Option<IdleCacheRelease> {
        if crate::turn_cancel_registry::has_active_turn(self.session_id()) {
            return None;
        }
        // One stat per call, so it sits behind the cheaper in-process gates.
        if self.session.migration_lease_block().is_some() {
            return None;
        }
        if !self.pending_alerts.is_empty() {
            return None;
        }
        // An attachment that is delivering saved History has not committed the
        // resumed state yet; its tool index must survive until it does.
        if self.pending_resume.is_some() {
            return None;
        }
        // `depth()` covers pending plus in-flight messages. The queue never
        // holds its lock across an await and touches no disk, so reading it
        // here cannot stall a turn.
        if self.soft_interrupt_queue.depth() != 0 {
            return None;
        }
        let compaction = self.registry.compaction();
        let Ok(manager) = compaction.try_read() else {
            return None;
        };
        if manager.is_compacting() {
            return None;
        }
        drop(manager);
        if crate::running_tool_registry::current(self.session_id()).is_some() {
            return None;
        }
        // A tool moved to the background outlives the Agent lock. Do not touch
        // even derived state until every tool this history names has settled,
        // or the next turn's repair would synthesize a duplicate result.
        if self.session.active_context().iter().any(|message| {
            message.content.iter().any(|block| match block {
                ContentBlock::ToolUse { id, .. } => crate::tool::inflight::is_tool_in_flight(id),
                _ => false,
            })
        }) {
            return None;
        }

        let released = IdleCacheRelease {
            provider_cache_bytes: self.session.memory_profile_snapshot().provider_cache_json_bytes,
            tool_index_entries: self.tool_call_ids.len() + self.tool_result_ids.len(),
        };
        // `release_provider_messages_cache` is the only safe way to drop the
        // projection: it also resets the mode to `Full`, so the next
        // `provider_messages()` rebuilds instead of sending an empty transcript
        // whose key still matches.
        self.session.release_provider_messages_cache();
        // A fresh set, not `clear()`: `reset_tool_output_tracking` keeps the
        // allocation, and releasing the allocation is the point.
        self.tool_call_ids = HashSet::new();
        self.tool_result_ids = HashSet::new();
        self.tool_output_scan_index = 0;
        Some(released)
    }

    /// Seed the tool index from the transcript the way a turn start does.
    #[cfg(test)]
    pub(crate) fn seed_idle_tool_index(&mut self) -> usize {
        self.repair_missing_tool_outputs()
    }

    /// Materialize the provider projection the way an idle compaction check does.
    #[cfg(test)]
    pub(crate) fn materialize_idle_provider_cache(&mut self) -> usize {
        self.session.messages_for_provider_ref().len()
    }

    /// `(tool_call_ids, tool_result_ids, tool_output_scan_index)` for the sweep tests.
    #[cfg(test)]
    pub(crate) fn idle_index_snapshot(&self) -> (usize, usize, usize) {
        (
            self.tool_call_ids.len(),
            self.tool_result_ids.len(),
            self.tool_output_scan_index,
        )
    }

    /// `(provider_cache_message_count, provider_cache_json_bytes)`.
    #[cfg(test)]
    pub(crate) fn idle_provider_cache_snapshot(&mut self) -> (usize, usize) {
        let snapshot = self.session.memory_profile_snapshot();
        (
            snapshot.provider_cache_message_count,
            snapshot.provider_cache_json_bytes,
        )
    }
}

#[cfg(test)]
#[path = "idle_hibernation_tests.rs"]
mod tests;
