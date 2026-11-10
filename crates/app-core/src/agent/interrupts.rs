use super::Agent;
use super::turn_recovery_wiring::AutoContinuation;
use crate::logging;
use crate::message::{ContentBlock, Role};
use crate::session::StoredDisplayRole;
use anyhow::Result;
use horon_agent_agent_runtime::{InterruptSignal, SoftInterruptMessage, SoftInterruptQueue, SoftInterruptSource};

/// Owns the messages one turn has already injected.
///
/// The turn loops create exactly one of these per turn and keep it in an
/// `Option` local, so a helper that injects cannot drop it: the helper receives
/// `Option<&mut InFlightGuard>` and only moves pending messages across.
///
/// A turn that commits or restores calls [`disarm`](Self::disarm), which makes
/// the drop a no-op. A turn that unwinds before either point - a panic in the
/// provider, the tool layer, or anywhere else in the loop - drops an armed
/// guard, and the drop restores the in-flight messages so they are injected
/// again instead of vanishing.
pub(super) struct InFlightGuard {
    session_id: String,
    queue: SoftInterruptQueue,
    /// Ids of the messages this turn appended to the active context, in the
    /// order they landed. A failed turn removes exactly these.
    appended: Vec<String>,
    armed: bool,
}

impl InFlightGuard {
    pub(super) fn new(session_id: String, queue: SoftInterruptQueue) -> Self {
        Self {
            session_id,
            queue,
            appended: Vec::new(),
            armed: true,
        }
    }

    /// Move every pending message into this turn's in-flight set.
    fn transfer(&self) -> Vec<SoftInterruptMessage> {
        self.queue.transfer_to_in_flight()
    }

    fn record_appended(&mut self, ids: Vec<String>) {
        self.appended.extend(ids);
    }

    fn take_appended(&mut self) -> Vec<String> {
        std::mem::take(&mut self.appended)
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let restored = self.queue.restore();
        if restored > 0 {
            logging::info(&format!(
                "AGENT_SOFT_INTERRUPT_RESTORE session={} restored={} reason=turn_unwound",
                self.session_id, restored
            ));
        }
    }
}

/// Reborrow the turn's in-flight guard for one helper call.
///
/// `Option<&mut InFlightGuard>::as_mut()` yields `Option<&mut &mut InFlightGuard>`,
/// and both spellings of that reborrow - `as_deref_mut()` and
/// `as_mut().map(|guard| &mut **guard)` - are rejected by clippy for this shape,
/// so the reborrow lives in one place.
pub(super) fn reborrow_in_flight<'a, 'b: 'a>(
    in_flight: &'a mut Option<&'b mut InFlightGuard>,
) -> Option<&'a mut InFlightGuard> {
    match in_flight {
        Some(guard) => Some(&mut **guard),
        None => None,
    }
}

fn soft_interrupt_session_display_role(source: SoftInterruptSource) -> Option<StoredDisplayRole> {
    match source {
        SoftInterruptSource::User => None,
        SoftInterruptSource::System => Some(StoredDisplayRole::System),
        SoftInterruptSource::BackgroundTask => Some(StoredDisplayRole::BackgroundTask),
    }
}

mod events;

#[derive(Debug, Clone)]
pub(super) struct InjectedSoftInterrupt {
    pub(super) content: String,
    pub(super) source: SoftInterruptSource,
    /// Caller ids of the queued messages combined into this injection, in
    /// queue order. Messages queued without an id contribute nothing.
    pub(super) client_ids: Vec<String>,
}

pub(super) enum NoToolCallOutcome {
    Break,
    BreakAfterRecoveryExhausted {
        notice: String,
    },
    ContinueWithoutEvent,
    ContinueWithSoftInterrupt {
        injected: Vec<InjectedSoftInterrupt>,
        point: &'static str,
    },
}

pub(super) enum PostToolInterruptOutcome {
    NoInterrupt,
    SoftInterrupt {
        injected: Vec<InjectedSoftInterrupt>,
        point: &'static str,
    },
}

/// True when the queue holds a message that must skip the rest of the tools.
///
/// This is the single definition of "an urgent interrupt is pending": the
/// agent polls it between tools, and a running group polls it through a cloned
/// queue handle so an interrupt lands mid-group instead of after it.
pub(super) fn urgent_interrupt_pending(queue: &SoftInterruptQueue) -> bool {
    queue.lock().map(|q| q.iter().any(|m| m.urgent)).unwrap_or(false)
}

impl Agent {
    /// Restore the pending queue after a restart.
    ///
    /// This runs inside the synchronous session-restore path, which already
    /// reads and parses the session from disk, so the journal read belongs here
    /// rather than on a second async hop.
    ///
    /// The journal is authoritative when it is available: the legacy JSON is
    /// imported once as a baseline, a `started` input with no terminal row is
    /// sealed in doubt and withheld from the queue, and every restored message
    /// is deduplicated by its `input_id` so restoring twice queues it once.
    /// Without a journal the legacy file is read exactly as before.
    pub fn restore_persisted_soft_interrupts(&self) -> usize {
        let already_queued: Vec<String> = match self.soft_interrupt_queue.lock() {
            Ok(queue) => queue.iter().filter_map(|message| message.input_id.clone()).collect(),
            Err(_) => {
                logging::warn(&format!(
                    "Failed to restore persisted soft interrupts for {} because queue lock was poisoned",
                    self.session_id()
                ));
                return 0;
            }
        };

        let mut restored: Vec<SoftInterruptMessage>;
        match crate::durable_input_runtime::recover_blocking(self.session_id()) {
            Some(recovery) => {
                if !recovery.sealed_in_doubt.is_empty() || !recovery.already_in_doubt.is_empty() {
                    logging::warn(&format!(
                        "AGENT_DURABLE_INPUT_IN_DOUBT session={} newly_sealed={} still_undetermined={} next=resolve-input",
                        self.session_id(),
                        recovery.sealed_in_doubt.len(),
                        recovery.already_in_doubt.len()
                    ));
                }
                if recovery.imported_from_legacy > 0 {
                    logging::info(&format!(
                        "AGENT_DURABLE_INPUT_BASELINE session={} imported={}",
                        self.session_id(),
                        recovery.imported_from_legacy
                    ));
                }
                restored = recovery.pending;
            }
            None => match crate::soft_interrupt_store::load(self.session_id()) {
                Ok(items) => restored = items,
                Err(err) => {
                    logging::warn(&format!(
                        "Failed to restore persisted soft interrupts for {}: {}",
                        self.session_id(),
                        err
                    ));
                    return 0;
                }
            },
        }

        restored.retain(|message| match &message.input_id {
            Some(input_id) => !already_queued.contains(input_id),
            None => true,
        });
        if restored.is_empty() {
            return 0;
        }

        let restored_count = restored.len();
        if let Ok(mut queue) = self.soft_interrupt_queue.lock() {
            queue.extend(restored);
        } else {
            logging::warn(&format!(
                "Failed to restore persisted soft interrupts for {} because queue lock was poisoned",
                self.session_id()
            ));
            return 0;
        }

        logging::info(&format!(
            "Restored {} persisted soft interrupt(s) for session {}",
            restored_count,
            self.session_id()
        ));
        restored_count
    }

    pub fn persist_soft_interrupt_snapshot(&self) {
        let pending = match self.soft_interrupt_queue.lock() {
            Ok(queue) => queue.clone(),
            Err(_) => {
                logging::warn(&format!(
                    "Failed to snapshot soft interrupts for {} because queue lock was poisoned",
                    self.session_id()
                ));
                return;
            }
        };

        if let Err(err) = crate::soft_interrupt_store::overwrite(self.session_id(), &pending) {
            logging::warn(&format!(
                "Failed to persist {} soft interrupt(s) for {}: {}",
                pending.len(),
                self.session_id(),
                err
            ));
        }
    }

    /// Commit the messages this turn injected: disk first, memory second.
    ///
    /// The disk is overwritten with the still-pending snapshot before the
    /// in-flight set is cleared. Reversing that order would delete messages
    /// from both places when the write fails; keeping the in-flight set on a
    /// failed write costs a duplicate injection after the next restore, which
    /// is the lesser harm.
    pub(super) async fn commit_soft_interrupt_turn(&self) {
        if self.soft_interrupt_queue.in_flight_len() == 0 {
            return;
        }
        let in_flight = self.soft_interrupt_queue.in_flight_snapshot();
        let durable_ids: Vec<String> = in_flight
            .iter()
            .filter_map(|message| message.input_id.clone())
            .collect();
        let legacy: Vec<SoftInterruptMessage> = in_flight
            .iter()
            .filter(|message| message.input_id.is_none())
            .cloned()
            .collect();
        if !durable_ids.is_empty() {
            crate::durable_input_runtime::commit(self.session_id(), durable_ids, self.current_run_id()).await;
        }
        if !legacy.is_empty() {
            let pending = self.soft_interrupt_queue.pending_snapshot();
            let legacy_pending: Vec<SoftInterruptMessage> = pending
                .into_iter()
                .filter(|message| message.input_id.is_none())
                .collect();
            if let Err(err) = crate::soft_interrupt_store::overwrite(self.session_id(), &legacy_pending) {
                logging::warn(&format!(
                    "AGENT_SOFT_INTERRUPT_COMMIT_FAILED session={} in_flight={} reason=disk_write_failed error={}",
                    self.session_id(),
                    self.soft_interrupt_queue.in_flight_len(),
                    err
                ));
                return;
            }
        }
        let committed = self.soft_interrupt_queue.commit();
        logging::info(&format!(
            "AGENT_SOFT_INTERRUPT_COMMIT_TURN session={} committed={} pending_after={}",
            self.session_id(),
            committed,
            self.soft_interrupt_queue.pending_len()
        ));
    }

    /// Put this turn's injected messages back in front of the pending queue
    /// and remove the copies this turn appended to the transcript.
    ///
    /// Both halves are needed: leaving the transcript copy behind would make
    /// the retry inject the same message a second time, and the plan's IS-3
    /// row forbids that duplicate. The disk copy is left untouched - it already
    /// holds them, and the next restore must be able to inject them once.
    async fn restore_soft_interrupt_turn(&mut self, reason: &str, appended: &[String]) {
        let restored = self.soft_interrupt_queue.restore();
        if restored > 0 {
            logging::info(&format!(
                "AGENT_SOFT_INTERRUPT_RESTORE session={} restored={} reason={}",
                self.session_id(),
                restored,
                reason
            ));
        }
        let durable_ids: Vec<String> = self
            .soft_interrupt_queue
            .pending_snapshot()
            .into_iter()
            .filter_map(|message| message.input_id)
            .collect();
        if !durable_ids.is_empty() {
            crate::durable_input_runtime::return_to_pending(self.session_id(), durable_ids, reason).await;
        }
        self.roll_back_soft_interrupt_injections(appended);
    }

    /// Remove this turn's injected messages from the active context.
    ///
    /// The transcript mutation marks the compaction generation stale and
    /// re-seeds its char estimate, so a compaction job built on the injected
    /// message cannot land after the rollback. Saving here keeps a restart from
    /// resurrecting the removed copy while the disk store still holds it.
    fn roll_back_soft_interrupt_injections(&mut self, appended: &[String]) {
        if appended.is_empty() {
            return;
        }
        let removed = self.session.remove_messages_by_id(appended);
        if removed == 0 {
            return;
        }
        self.note_transcript_mutation(crate::compaction::jobs::CancelReason::Rewind);
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.seed_restored_stored_messages_with(self.session.active_context());
        }
        if let Err(err) = self.save_session() {
            logging::warn(&format!(
                "AGENT_SOFT_INTERRUPT_ROLLBACK_SAVE_FAILED session={} removed={} error={}",
                self.session_id(),
                removed,
                err
            ));
        }
        logging::info(&format!(
            "AGENT_SOFT_INTERRUPT_ROLLBACK session={} removed={} reason=injected_messages_rolled_back",
            self.session_id(),
            removed
        ));
    }

    /// Settle the turn's in-flight messages according to how it ended.
    ///
    /// `committed` is true only for a turn that finished normally; a failed or
    /// cancelled turn restores. Either way the guard is disarmed afterwards, so
    /// its drop cannot restore a second time.
    ///
    /// This is also the turn's single settle point shared by both turn loops,
    /// so a context rollover the model asked for with `new_context` is committed
    /// here: the transcript is final, and a later turn can no longer land it on
    /// a transcript the model never saw. A turn that failed or was cancelled
    /// drops the request instead of carrying it into an unrelated turn.
    pub(super) async fn settle_soft_interrupt_turn(&mut self, in_flight: &mut Option<InFlightGuard>, committed: bool) {
        if let Some(mut guard) = in_flight.take() {
            if committed {
                self.commit_soft_interrupt_turn().await;
            } else {
                let appended = guard.take_appended();
                self.restore_soft_interrupt_turn("turn_failed_or_cancelled", &appended)
                    .await;
            }
            guard.disarm();
        }
        if committed {
            self.apply_pending_context_rollover();
        } else {
            crate::tool::new_context::clear_rollover(&self.session.id);
        }
    }

    /// This turn's run id, so a durable transition can name it.
    fn current_run_id(&self) -> Option<String> {
        horon_agent_agent_runtime::current_run(self.session_id()).map(|run| run.run_id().to_string())
    }

    /// Add a swarm alert to be injected into the next turn
    pub fn push_alert(&mut self, alert: String) {
        self.pending_alerts.push(alert);
    }

    /// Take all pending alerts (clears the queue)
    pub fn take_alerts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_alerts)
    }

    /// Queue a soft interrupt message to be injected at the next safe point.
    /// This method can be called even while the agent is processing (uses separate lock).
    pub fn queue_soft_interrupt(
        &self,
        content: String,
        images: Vec<(String, String)>,
        urgent: bool,
        source: SoftInterruptSource,
    ) {
        self.queue_soft_interrupt_with_id(content, images, urgent, source, None);
    }

    /// Like [`queue_soft_interrupt`](Self::queue_soft_interrupt), with an
    /// opaque caller id echoed back in `SoftInterruptInjected`.
    pub fn queue_soft_interrupt_with_id(
        &self,
        content: String,
        images: Vec<(String, String)>,
        urgent: bool,
        source: SoftInterruptSource,
        client_id: Option<String>,
    ) {
        let content_bytes = content.len();
        let content_chars = content.chars().count();
        let image_count = images.len();
        if let Ok(mut queue) = self.soft_interrupt_queue.lock() {
            let pending_before = queue.len();
            queue.push(SoftInterruptMessage {
                content,
                images,
                urgent,
                source,
                client_id,
                input_id: None,
            });
            logging::info(&format!(
                "AGENT_SOFT_INTERRUPT_QUEUE_PUSH session={} source={:?} urgent={} content_bytes={} content_chars={} image_count={} pending_before={} pending_after={}",
                self.session_id(),
                source,
                urgent,
                content_bytes,
                content_chars,
                image_count,
                pending_before,
                queue.len()
            ));
        } else {
            logging::warn(&format!(
                "AGENT_SOFT_INTERRUPT_QUEUE_PUSH_FAILED session={} source={:?} urgent={} content_bytes={} content_chars={} image_count={} reason=queue_lock_poisoned",
                self.session_id(),
                source,
                urgent,
                content_bytes,
                content_chars,
                image_count
            ));
        }
    }

    /// Get a handle to the soft interrupt queue.
    /// The server can use this to queue interrupts without holding the agent lock.
    pub fn soft_interrupt_queue(&self) -> SoftInterruptQueue {
        self.soft_interrupt_queue.clone()
    }

    /// Push a message the durable layer already accepted.
    ///
    /// Callers that go through the durable input store hand the accepted message
    /// here, so the queue holds exactly the input the journal recorded.
    pub fn enqueue_accepted_soft_interrupt(&self, message: SoftInterruptMessage) -> bool {
        match self.soft_interrupt_queue.lock() {
            Ok(mut queue) => {
                queue.push(message);
                drop(queue);
                self.soft_interrupt_queue.observe();
                true
            }
            Err(_) => {
                logging::warn(&format!(
                    "AGENT_SOFT_INTERRUPT_QUEUE_PUSH_FAILED session={} reason=queue_lock_poisoned",
                    self.session_id()
                ));
                false
            }
        }
    }

    /// Get a handle to the background tool signal.
    /// The server can use this to signal "move tool to background" without holding the agent lock.
    pub fn background_tool_signal(&self) -> InterruptSignal {
        self.background_tool_signal.clone()
    }

    pub fn graceful_shutdown_signal(&self) -> InterruptSignal {
        self.graceful_shutdown.clone()
    }

    pub fn request_graceful_shutdown(&self) {
        self.graceful_shutdown.fire();
    }

    pub(super) fn is_graceful_shutdown(&self) -> bool {
        self.graceful_shutdown.is_set()
    }

    /// Check if there are pending soft interrupts
    pub fn has_soft_interrupts(&self) -> bool {
        self.soft_interrupt_queue.lock().map(|q| !q.is_empty()).unwrap_or(false)
    }

    /// Check if there's an urgent soft interrupt that should skip remaining tools
    pub fn has_urgent_interrupt(&self) -> bool {
        urgent_interrupt_pending(&self.soft_interrupt_queue)
    }

    /// Get count of queued soft interrupts
    pub fn soft_interrupt_count(&self) -> usize {
        self.soft_interrupt_queue.lock().map(|q| q.len()).unwrap_or(0)
    }

    /// Get count of pending alerts
    pub fn pending_alert_count(&self) -> usize {
        self.pending_alerts.len()
    }

    /// Get pending alerts (for debug visibility)
    pub fn pending_alerts_preview(&self) -> Vec<String> {
        self.pending_alerts
            .iter()
            .take(10)
            .map(|s| {
                if s.len() > 100 {
                    format!("{}...", crate::util::truncate_str(s, 100))
                } else {
                    s.clone()
                }
            })
            .collect()
    }

    /// Get comprehensive debug info about agent internal state
    pub fn debug_info(&self) -> serde_json::Value {
        serde_json::json!({
            "provider": self.provider.name(),
            "model": self.provider.model(),
            "provider_session_id": self.provider_session_id,
            "last_upstream_provider": self.last_upstream_provider,
            "last_connection_type": self.last_connection_type,
            "active_skill": self.active_skill,
            "allowed_tools": self.allowed_tools,
            "disabled_tools": self.disabled_tools,
            "session": {
                "id": self.session.id,
                "model": self.session.model,
                "working_dir": self.session.working_dir,
                "message_count": self.session.active_history_offset() + self.session.active_context().len(),
            },
            "interrupts": {
                "soft_interrupt_count": self.soft_interrupt_count(),
                "has_urgent": self.has_urgent_interrupt(),
                "pending_alert_count": self.pending_alert_count(),
                "soft_interrupts": self.soft_interrupts_preview(),
                "pending_alerts": self.pending_alerts_preview(),
            },
            "cache_tracker": {
                "turn_count": self.cache_tracker.turn_count(),
                "had_violation": self.cache_tracker.had_violation(),
            },
            "features": {
                "memory_enabled": self.memory_enabled,
            },
            "token_usage": {
                "input": self.last_usage.input_tokens,
                "output": self.last_usage.output_tokens,
                "cache_read": self.last_usage.cache_read_input_tokens,
                "cache_write": self.last_usage.cache_creation_input_tokens,
            },
        })
    }

    pub fn debug_memory_profile(&self) -> serde_json::Value {
        let process = crate::process_memory::snapshot_with_source("agent:memory");
        let soft_interrupt_text_bytes: usize = self
            .soft_interrupt_queue
            .lock()
            .map(|queue| queue.iter().map(|msg| msg.content.len()).sum())
            .unwrap_or(0);
        let pending_alert_text_bytes: usize = self.pending_alerts.iter().map(|alert| alert.len()).sum();

        serde_json::json!({
            "process": process,
            "session": self.session.debug_memory_profile(),
            "interrupts": {
                "soft_interrupt_count": self.soft_interrupt_count(),
                "soft_interrupt_text_bytes": soft_interrupt_text_bytes,
                "pending_alert_count": self.pending_alert_count(),
                "pending_alert_text_bytes": pending_alert_text_bytes,
            },
            "agent": {
                "memory_enabled": self.memory_enabled,
                "allowed_tools_count": self.allowed_tools.as_ref().map(|tools| tools.len()),
                "disabled_tools_count": self.disabled_tools.len(),
                "tool_call_ids": self.tool_call_ids.len(),
                "tool_result_ids": self.tool_result_ids.len(),
                "last_usage": {
                    "input": self.last_usage.input_tokens,
                    "output": self.last_usage.output_tokens,
                    "cache_read": self.last_usage.cache_read_input_tokens,
                    "cache_write": self.last_usage.cache_creation_input_tokens,
                },
            }
        })
    }

    /// Get soft interrupt previews (for debug visibility)
    pub fn soft_interrupts_preview(&self) -> Vec<(String, bool)> {
        self.soft_interrupt_queue
            .lock()
            .map(|q| {
                q.iter()
                    .take(10)
                    .map(|m| {
                        let preview = if m.content.len() > 100 {
                            format!("{}...", crate::util::truncate_str(&m.content, 100))
                        } else {
                            m.content.clone()
                        };
                        (preview, m.urgent)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Inject all pending soft interrupt messages into the conversation.
    /// Returns the combined message content and stages them as in flight.
    ///
    /// `in_flight` is the running turn's guard. This helper never creates or
    /// drops one: the turn loop owns it for the whole turn, so a return from
    /// here cannot trigger a restore while the turn is still going.
    pub(super) async fn inject_soft_interrupts(
        &mut self,
        mut in_flight: Option<&mut InFlightGuard>,
    ) -> Vec<InjectedSoftInterrupt> {
        // A cancelled turn never makes another request. Injecting now would
        // put the user's next prompt into history with no reply, and the
        // client would treat it as delivered. Esc after typing a follow-up
        // must stop this turn and run that prompt, so leave it queued: the
        // client recovers it and sends it as the next turn.
        if self.is_graceful_shutdown() {
            if self.has_soft_interrupts() {
                logging::info(&format!(
                    "AGENT_SOFT_INTERRUPT_INJECT_SKIPPED_CANCELLED session={} pending_count={}",
                    self.session_id(),
                    self.soft_interrupt_count()
                ));
            }
            return Vec::new();
        }
        let messages: Vec<SoftInterruptMessage> = {
            let queue = match self.soft_interrupt_queue.lock() {
                Ok(queue) => queue,
                Err(_) => {
                    logging::warn(&format!(
                        "AGENT_SOFT_INTERRUPT_INJECT_LOCK_FAILED session={}",
                        self.session_id()
                    ));
                    return Vec::new();
                }
            };
            if queue.is_empty() {
                return Vec::new();
            }
            logging::info(&format!(
                "AGENT_SOFT_INTERRUPT_INJECT_DRAIN session={} pending_count={} urgent_count={} total_content_bytes={}",
                self.session_id(),
                queue.len(),
                queue.iter().filter(|message| message.urgent).count(),
                queue.iter().map(|message| message.content.len()).sum::<usize>()
            ));
            drop(queue);
            match reborrow_in_flight(&mut in_flight) {
                Some(guard) => guard.transfer(),
                None => self.soft_interrupt_queue.transfer_to_in_flight(),
            }
        };

        let durable_ids: Vec<String> = messages.iter().filter_map(|message| message.input_id.clone()).collect();
        let started: Vec<String> = if durable_ids.is_empty() {
            Vec::new()
        } else {
            let run_id = self.current_run_id();
            crate::durable_input_runtime::start(self.session_id(), durable_ids.clone(), run_id).await
        };
        let messages: Vec<SoftInterruptMessage> = messages
            .into_iter()
            .filter(|message| match &message.input_id {
                Some(input_id) => started.contains(input_id),
                None => true,
            })
            .collect();
        if messages.is_empty() {
            return Vec::new();
        }

        let injected_ids: Vec<String> = messages.iter().filter_map(|message| message.input_id.clone()).collect();
        let mut appended_ids: Vec<String> = Vec::new();

        let mut injected = Vec::new();
        let mut current_source: Option<SoftInterruptSource> = None;
        let mut current_parts: Vec<String> = Vec::new();
        let mut current_images: Vec<(String, String)> = Vec::new();
        let mut current_ids: Vec<String> = Vec::new();

        let mut flush_group = |agent: &mut Self,
                               injected: &mut Vec<InjectedSoftInterrupt>,
                               source: SoftInterruptSource,
                               parts: &mut Vec<String>,
                               images: &mut Vec<(String, String)>,
                               ids: &mut Vec<String>| {
            if parts.is_empty() && images.is_empty() {
                ids.clear();
                return;
            }
            let content = parts.join("\n\n");
            parts.clear();
            let mut blocks = super::turn_execution::audio::attachment_blocks(std::mem::take(images));
            if !content.is_empty() {
                blocks.push(ContentBlock::Text {
                    text: content.clone(),
                    cache_control: None,
                });
            }
            agent.add_message_with_display_role(Role::User, blocks, soft_interrupt_session_display_role(source));
            let message_id = agent.session.active_context().last().map(|message| message.id.clone());
            if let Some(message_id) = message_id {
                // A v2 editor learns about this injection as it lands, under the
                // id the row was stored with, so its live view and a later replay
                // name the same row. An injection with a display role is an
                // internal note the editor already receives through the turn's
                // events, and a turn-start injection is reported as that turn's
                // own prompt instead, so neither is announced here.
                if soft_interrupt_session_display_role(source).is_none() {
                    events::send_stored(agent.session_id(), &message_id, &content);
                }
                appended_ids.push(message_id);
            }
            injected.push(InjectedSoftInterrupt {
                content,
                source,
                client_ids: std::mem::take(ids),
            });
        };

        for message in messages {
            match current_source {
                Some(source) if source != message.source => {
                    flush_group(
                        self,
                        &mut injected,
                        source,
                        &mut current_parts,
                        &mut current_images,
                        &mut current_ids,
                    );
                    current_source = Some(message.source);
                }
                None => current_source = Some(message.source),
                _ => {}
            }
            current_parts.push(message.content);
            current_images.extend(message.images);
            current_ids.extend(message.client_id);
        }

        if let Some(source) = current_source {
            flush_group(
                self,
                &mut injected,
                source,
                &mut current_parts,
                &mut current_images,
                &mut current_ids,
            );
        }

        if let Some(guard) = reborrow_in_flight(&mut in_flight) {
            guard.record_appended(std::mem::take(&mut appended_ids));
        }
        if !injected_ids.is_empty()
            && let Some(message_id) = self.session.active_context().last().map(|message| message.id.clone())
        {
            crate::durable_input_runtime::note_injected_copies(self.session_id(), injected_ids, message_id).await;
        }

        self.persist_session_best_effort("soft interrupt injection");
        logging::info(&format!(
            "AGENT_SOFT_INTERRUPT_INJECT_COMMIT session={} groups={} total_content_bytes={}",
            self.session_id(),
            injected.len(),
            injected.iter().map(|interrupt| interrupt.content.len()).sum::<usize>()
        ));
        injected
    }

    pub(super) async fn handle_streaming_no_tool_calls(
        &mut self,
        stop_reason: Option<&str>,
        final_text: &str,
        terminal_message_complete: bool,
        in_flight: Option<&mut InFlightGuard>,
    ) -> Result<NoToolCallOutcome> {
        if terminal_message_complete
            && let Some(outcome) = self.maybe_auto_continue_unexpected_stop(stop_reason, final_text)?
        {
            return Ok(match outcome {
                AutoContinuation::Continued => NoToolCallOutcome::ContinueWithoutEvent,
                AutoContinuation::Exhausted(notice) => NoToolCallOutcome::BreakAfterRecoveryExhausted { notice },
            });
        }
        if self.maybe_continue_incomplete_response(stop_reason)? {
            return Ok(NoToolCallOutcome::ContinueWithoutEvent);
        }
        if self.maybe_continue_stranded_tool_use(stop_reason)? {
            return Ok(NoToolCallOutcome::ContinueWithoutEvent);
        }
        logging::info("Turn complete - no tool calls");
        let injected = self.inject_soft_interrupts(in_flight).await;
        if !injected.is_empty() {
            return Ok(NoToolCallOutcome::ContinueWithSoftInterrupt { injected, point: "B" });
        }
        if self.maybe_continue_goal()? {
            return Ok(NoToolCallOutcome::ContinueWithoutEvent);
        }
        Ok(NoToolCallOutcome::Break)
    }

    pub(super) async fn take_post_tool_soft_interrupt(
        &mut self,
        in_flight: Option<&mut InFlightGuard>,
    ) -> PostToolInterruptOutcome {
        let injected = self.inject_soft_interrupts(in_flight).await;
        if !injected.is_empty() {
            PostToolInterruptOutcome::SoftInterrupt { injected, point: "D" }
        } else {
            PostToolInterruptOutcome::NoInterrupt
        }
    }
}
