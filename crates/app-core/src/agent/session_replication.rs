//! Idle replica refresh does not construct an Agent or change its local binding.
use super::Agent;
use crate::session::event_store_api::replication::{SessionEventPage, SessionImportResult};
use anyhow::Result;

impl Agent {
    #[cfg(test)]
    pub(crate) fn set_replica_preparation_hook(&mut self, hook: Box<dyn FnOnce() + Send>) -> Result<()> {
        self.session.set_event_preparation_hook(hook)
    }

    pub(crate) fn import_session_replica(
        &mut self,
        page: SessionEventPage,
        retry_cursor: Option<String>,
    ) -> Result<SessionImportResult> {
        let before = self.session.selected_event_head();
        let result = self.session.import_replica(page, retry_cursor);
        // Errors can contain a committed prefix. Always discard derived execution state.
        if result.is_err() || self.session.selected_event_head() != before {
            self.provider_session_id = None;
            self.cache_tracker.reset();
            self.kv_cache_monitor.reset();
            self.locked_tools = None;
            self.rewind_undo_snapshot = None;
            self.reset_tool_output_tracking();
            self.seed_compaction_from_session();
        }
        result
    }
}
