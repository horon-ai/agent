use super::*;

impl Agent {
    /// Clear conversation history
    pub fn clear(&mut self) {
        if let Err(error) = self.clear_session_branch() {
            logging::error(&format!("Could not clear session {}: {error}", self.session.id));
        }
    }

    /// Reset this live binding in place; old originals and the session ID survive.
    pub(crate) fn clear_session_branch(&mut self) -> Result<()> {
        self.note_transcript_mutation(crate::compaction::jobs::CancelReason::Clear);
        self.session.clear_event_branch()?;
        self.refresh_agents_md_snapshot();
        self.reset_runtime_state_for_session_change();
        self.soft_interrupt_queue = SoftInterruptQueue::new();
        self.background_tool_signal = InterruptSignal::new();
        self.graceful_shutdown = InterruptSignal::new();
        self.provider_session_id = None;
        // A cleared branch takes its placeholders with it: nothing left in the
        // transcript can still name them, so the vault starts empty for the next turn.
        crate::secrets::reset_session_vault(&self.session.id);
        // Nothing the ledger named is in the transcript any more.
        self.session.pruned_tool_results.clear();
        // Notes describe the branch that was just cleared; keeping them would
        // leak the old task's index into the fresh conversation.
        if let Err(error) = crate::context_notes::clear(&self.session.id) {
            logging::warn(&format!(
                "Could not clear context notes for {}: {error}",
                self.session.id
            ));
        }
        self.seed_compaction_from_session();
        self.advisor_history_rewritten();
        Ok(())
    }

    /// Clear provider session so the next turn sends full context.
    pub fn reset_provider_session(&mut self) {
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        self.persist_session_best_effort("provider session reset");
    }

    /// Rewind the conversation to a 1-based visible transcript message index.
    ///
    /// The index is interpreted against the same rendered transcript the TUI
    /// numbers in `/rewind` (user/assistant entries only, tool cards and
    /// system notices excluded). Mapping through raw stored messages instead
    /// would count tool-result messages the UI never numbers, sending
    /// `/rewind N` far earlier than the on-screen message N (issue #432).
    ///
    /// Provider-side resumable sessions are reset so the next request sends the
    /// truncated context from scratch instead of continuing from a stale upstream
    /// conversation.
    pub fn rewind_to_message(&mut self, message_index: usize) -> Result<usize, String> {
        let targets = self
            .session
            .history_snapshot()
            .map_err(|e| e.to_string())?
            .rewind_target_stored_indices();
        let message_count = targets.len();
        if message_index == 0 || message_index > message_count {
            return Err(format!(
                "Invalid message number: {}. Valid range: 1-{}",
                message_index, message_count
            ));
        }
        self.save_session().map_err(|e| e.to_string())?;
        let head_event_id = self
            .session
            .selected_event_head()
            .ok_or_else(|| "Session has no durable rewind head".to_owned())?;
        let removed = self
            .session
            .rewind_to_rendered_message(message_index)
            .map_err(|e| e.to_string())?;
        self.note_transcript_mutation(crate::compaction::jobs::CancelReason::Rewind);
        self.rewind_undo_snapshot = Some(RewindUndoSnapshot {
            head_event_id,
            provider_session_id: self.provider_session_id.clone(),
            session_provider_session_id: self.session.provider_session_id.clone(),
            visible_message_count: message_count,
        });
        self.session.updated_at = chrono::Utc::now();
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        self.cache_tracker.reset();
        self.kv_cache_monitor.reset();
        self.locked_tools = None;
        self.reset_tool_output_tracking();
        self.save_session().map_err(|e| e.to_string())?;
        self.seed_compaction_from_session();
        self.advisor_history_rewritten();
        Ok(removed)
    }

    pub fn undo_rewind(&mut self) -> Result<usize, String> {
        let Some(snapshot) = self.rewind_undo_snapshot.clone() else {
            return Err("No rewind to undo.".to_string());
        };

        let current_count = self
            .session
            .history_snapshot()
            .map_err(|e| e.to_string())?
            .rewind_target_count();
        let restored = snapshot.visible_message_count.saturating_sub(current_count);
        self.session
            .select_event_head(&snapshot.head_event_id)
            .map_err(|e| e.to_string())?;
        self.note_transcript_mutation(crate::compaction::jobs::CancelReason::Rewind);
        self.provider_session_id = snapshot.provider_session_id;
        self.session.provider_session_id = snapshot.session_provider_session_id;
        self.session.updated_at = chrono::Utc::now();
        self.cache_tracker.reset();
        self.kv_cache_monitor.reset();
        self.locked_tools = None;
        self.reset_tool_output_tracking();
        self.save_session().map_err(|e| e.to_string())?;
        self.rewind_undo_snapshot = None;
        self.seed_compaction_from_session();
        self.advisor_history_rewritten();
        Ok(restored)
    }
}
