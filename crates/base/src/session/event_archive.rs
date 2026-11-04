//! Archive and restore as durable records on the session's own writer.
use super::{Session, event_store};
use anyhow::{Context, Result};
use chrono::Utc;
use horon_agent_session_store as store;
use serde_json::json;

impl Session {
    /// Record a visibility change at the current head, through this session's writer.
    ///
    /// Returns `false` when the head already carries the requested visibility, so a
    /// repeated archive or restore appends nothing. Pending facade changes are
    /// committed first, because the record must follow the head it was computed from.
    ///
    /// # Errors
    /// Refuses a blocked, damaged or diverged session and any failed durability.
    pub fn record_archive(&mut self, archived: bool, reason: &str) -> Result<bool> {
        if !self.event_state.pending.is_empty() {
            self.save()?;
        }
        let paths = self
            .event_state
            .paths
            .clone()
            .map(Ok)
            .unwrap_or_else(event_store::paths)?;
        let id = store::SessionId::parse(&self.id)?;
        let writer = self.event_writer()?;
        let origin = super::event_origin::origin(&paths)?;
        let selected = event_store::selected(&paths, &id)?;
        let graph = event_store::graph(&paths, &id)?;
        let head = graph.selected_head(selected.as_ref())?;
        let current = graph.replay(Some(&head))?.archive().is_some_and(|state| state.archived);
        if current == archived {
            return Ok(false);
        }
        let event = store::EventDraft::new(
            id.clone(),
            store::EventId::new_v4(),
            Some(head),
            origin,
            Utc::now(),
            store::EventKind::new(store::KnownEventKind::ArchiveChanged),
            json!({"archived": archived, "reason": reason}),
        )
        .seal()?;
        let event_id = event.envelope().event_id.clone();
        let commit_paths = paths.clone();
        let commit_id = id.clone();
        let selected_next = event_id.clone();
        writer.call(move |writer| {
            writer.append(&event)?;
            event_store::select(&commit_paths, &commit_id, &selected_next, true)?;
            Ok(())
        })?;
        self.event_state.head = Some(event_id);
        self.event_state.source_hash = event_store::source_hash(&paths, &id)?;
        Ok(true)
    }

    /// Visibility recorded at the current head, if a record exists.
    ///
    /// # Errors
    /// Propagates original damage and missing blobs.
    pub fn archived_state(&self) -> Result<Option<bool>> {
        let paths = self
            .event_state
            .paths
            .clone()
            .map(Ok)
            .unwrap_or_else(event_store::paths)?;
        let id = store::SessionId::parse(&self.id)?;
        let graph = event_store::graph(&paths, &id).context("session originals unavailable")?;
        let head = graph.selected_head(event_store::selected(&paths, &id)?.as_ref())?;
        Ok(graph.replay(Some(&head))?.archive().map(|state| state.archived))
    }
}
