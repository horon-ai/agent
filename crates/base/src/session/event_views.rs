use super::{Session, StoredMessage, event_store};
use anyhow::{Context, Result};
use horon_agent_session_store as store;

impl Session {
    /// Copy a visible conversation into an independent session and retain its source identity.
    pub fn inherit_history(&mut self, parent: &Session) -> Result<()> {
        anyhow::ensure!(self.id != parent.id, "fork requires an independent session identity");
        self.replace_messages(parent.history_messages()?);
        self.parent_id = Some(parent.id.clone());
        self.event_state.fork_source = Some(serde_json::json!({
            "source_session_id":parent.id,"source_head_event_id":parent.selected_event_head()
        }));
        Ok(())
    }

    pub(super) fn refresh_active_context(&mut self) -> Result<()> {
        let paths = self.event_state.paths.clone().context("session paths missing")?;
        let id = store::SessionId::parse(&self.id)?;
        let graph = event_store::graph(&paths, &id)?;
        let projection = graph.replay(self.event_state.head.as_ref())?;
        self.event_state.compaction_executable = projection.compaction().is_some();
        let mut tools = super::event_tools::ToolChain::default();
        for message in projection.messages() {
            tools.append(&message.message)?;
        }
        self.event_state.tools = tools;
        if !self.persist_state.transcript_stripped {
            self.messages = projection.active_messages().iter().map(|m| m.message.clone()).collect();
            self.event_state.active_start = projection.messages().len() - self.messages.len();
            self.event_state.prefix_usage = Some(super::aggregate_usage(
                projection
                    .messages()
                    .iter()
                    .take(self.event_state.active_start)
                    .map(|m| &m.message),
                crate::protocol::TokenUsageTotals {
                    cache_prompt_tokens: Some(0),
                    ..Default::default()
                },
            ));
            self.event_state.prefix_visible = projection
                .messages()
                .iter()
                .take(self.event_state.active_start)
                .filter(|m| super::is_visible_conversation_message(&m.message))
                .count();
            self.release_provider_messages_cache();
            self.mark_memory_profile_dirty();
        }
        Ok(())
    }

    /// Per-writer real-filesystem fault injection for public API integration probes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_event_io_hook(&mut self, hook: store::IoHook) -> Result<()> {
        anyhow::ensure!(self.event_state.writer.is_none(), "writer already started");
        self.event_state.writer = Some(super::event_actor::Writer::open_with_hook(
            event_store::paths()?,
            store::SessionId::parse(&self.id)?,
            hook,
        )?);
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pending_event_ids(&self) -> Vec<String> {
        self.event_state
            .pending
            .iter()
            .map(|e| e.envelope().event_id.as_str().to_owned())
            .collect()
    }
    /// Borrow only the resident provider context, never an implicit full-history export.
    pub fn active_context(&self) -> &[StoredMessage] {
        &self.messages
    }

    /// Move an unsaved/imported active context into a caller-owned conversion.
    pub fn take_active_messages(&mut self) -> Vec<StoredMessage> {
        self.mark_memory_profile_dirty();
        self.mark_messages_full_dirty();
        std::mem::take(&mut self.messages)
    }

    /// Explicit full-history materialization. The returned originals belong to the caller.
    pub fn history_messages(&self) -> Result<Vec<StoredMessage>> {
        let Some(paths) = &self.event_state.paths else {
            return Ok(self.messages.clone());
        };
        let id = store::SessionId::parse(self.event_state.bound_id.as_deref().unwrap_or(&self.id))?;
        let graph = event_store::graph(paths, &id)?;
        let projection = graph.replay(self.event_state.head.as_ref())?;
        if self.persist_state.transcript_stripped {
            return Ok(projection.messages().iter().map(|m| m.message.clone()).collect());
        }
        let mut history = projection
            .messages()
            .iter()
            .take(self.event_state.active_start)
            .map(|m| m.message.clone())
            .collect::<Vec<_>>();
        history.extend(self.messages.iter().cloned());
        Ok(history)
    }

    /// A transient export/rendering object; it never owns a writer or writes over its source.
    pub fn history_snapshot(&self) -> Result<Self> {
        let history = self.history_messages()?;
        let mut result = self.clone();
        result.messages = history;
        result.event_state = Default::default();
        result.persist_state.transcript_stripped = false;
        result.reset_provider_messages_cache();
        Ok(result)
    }

    /// Oldest-first page ending before a stable message identity.
    ///
    /// Only the requested window is materialized: the page index records where
    /// each projected message lives, bound to this session's head and the
    /// original log's identity, so a small page costs the page.
    pub fn history_page(&self, before: Option<&str>, limit: usize) -> Result<Vec<StoredMessage>> {
        anyhow::ensure!((1..=200).contains(&limit), "history page limit must be 1..=200");
        let Some(paths) = &self.event_state.paths else {
            let end = match before {
                Some(id) => self
                    .messages
                    .iter()
                    .position(|m| m.id == id)
                    .context("unknown history cursor")?,
                None => self.messages.len(),
            };
            let page = self.messages[end.saturating_sub(limit)..end].to_vec();
            horon_agent_perf_counters::record_page_hydration(page.len() as u64);
            return Ok(page);
        };
        let id = store::SessionId::parse(self.event_state.bound_id.as_deref().unwrap_or(&self.id))?;
        let pages =
            store::HistoryPages::open(paths, &id, self.event_state.head.as_ref(), &store::ReadProbe::default())?;
        let resident = if self.persist_state.transcript_stripped {
            &[][..]
        } else {
            &self.messages[..]
        };
        let prefix_end = if self.persist_state.transcript_stripped {
            pages.len()
        } else {
            self.event_state.active_start.min(pages.len())
        };
        let end = match before {
            Some(cursor) => match pages
                .position_of_message(cursor)?
                .filter(|position| *position < prefix_end)
            {
                Some(position) => position,
                None => {
                    let position = resident
                        .iter()
                        .position(|message| message.id == cursor)
                        .context("unknown history cursor")?;
                    prefix_end + position
                }
            },
            None => prefix_end + resident.len(),
        };
        let start = end.saturating_sub(limit);
        let mut page = Vec::with_capacity(end - start);
        if start < prefix_end {
            page.extend(
                pages
                    .page(start, end.min(prefix_end))?
                    .into_iter()
                    .map(|message| message.message),
            );
        }
        let tail_from = start.max(prefix_end);
        if tail_from < end {
            page.extend(resident[tail_from - prefix_end..end - prefix_end].iter().cloned());
        }
        horon_agent_perf_counters::record_page_hydration(page.len() as u64);
        Ok(page)
    }

    pub fn revise_message(&mut self, message: StoredMessage) -> Result<()> {
        self.invalidate_event_generation()?;
        let target = self
            .messages
            .iter_mut()
            .find(|m| m.id == message.id)
            .context("message is not in active context")?;
        *target = message;
        self.mark_memory_profile_dirty();
        self.mark_messages_full_dirty();
        Ok(())
    }

    pub fn selected_event_head(&self) -> Option<String> {
        self.event_state.head.as_ref().map(|head| head.as_str().to_owned())
    }

    /// Select an existing branch without copying or deleting its messages.
    pub fn select_event_head(&mut self, head: &str) -> Result<()> {
        if let Some(error) = &self.event_state.blocked {
            anyhow::ensure!(error.code == store::StoreErrorCode::ConflictHeads, error.clone());
        } else if self.has_unsaved_event_changes()? {
            self.save()?;
        }
        let paths = self
            .event_state
            .paths
            .clone()
            .context("session has no durable history")?;
        let id = store::SessionId::parse(&self.id)?;
        let head = store::EventId::parse(head)?;
        event_store::graph(&paths, &id)?.replay(Some(&head))?;
        let writer = self.event_writer()?;
        let mut operation = writer.try_operation()?;
        anyhow::ensure!(
            !operation.retry_required,
            store::StoreError::busy("retry the original import page")
        );
        let selected_paths = paths.clone();
        let selected_id = id.clone();
        writer.call(move |writer| {
            if event_store::select(&selected_paths, &selected_id, &head, true)? {
                event_store::sync_local_state(&selected_paths, &selected_id)?;
            }
            writer.refresh_cache(Some(&head), store::ReadProbe::default())?;
            Ok(())
        })?;
        operation.advance()?;
        let mut restored = Self::load_events(paths, id)?;
        restored.event_state.writer = Some(std::sync::Arc::clone(&writer));
        restored.event_state.generation = operation.snapshot().generation;
        // A history selection does not import an old process's execution ownership.
        restored.status = self.status.clone();
        restored.last_pid = self.last_pid;
        restored.last_active_at = self.last_active_at;
        *self = restored;
        Ok(())
    }

    /// Clear selects a new empty root in this identity, preserving every old branch.
    pub fn clear_event_branch(&mut self) -> Result<()> {
        if !self.persist_state.snapshot_exists || self.has_unsaved_event_changes()? {
            self.save_prepared()?;
        }
        self.replace_messages(Vec::new());
        self.system_prompt = None;
        self.provider_session_id = None;
        self.event_state.reset_requested = true;
        self.save_prepared()
    }

    pub fn rewind_to_rendered_message(&mut self, index: usize) -> Result<usize> {
        self.save()?;
        let snapshot = self.history_snapshot()?;
        let indices = snapshot.rewind_target_stored_indices();
        let stored = *indices
            .get(index.checked_sub(1).context("invalid rewind index")?)
            .context("invalid rewind index")?;
        let paths = self.event_state.paths.as_ref().context("session is not saved")?;
        let graph = event_store::graph(paths, &store::SessionId::parse(&self.id)?)?;
        let projection = graph.replay(self.event_state.head.as_ref())?;
        let head = projection
            .messages()
            .get(stored)
            .context("invalid renderer mapping")?
            .event_id
            .clone();
        self.select_event_head(head.as_str())?;
        Ok(indices.len() - index)
    }
}
