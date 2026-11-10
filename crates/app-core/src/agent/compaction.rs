use super::*;

/// One-line notice a model-requested rollover leaves where a summary would be.
///
/// The boundary carries no generated summary, so this text stands in its place
/// in the provider projection. It has to name `history://current`: the covered
/// messages are still stored, and that URL is the only way back to them. The
/// working notes stay in the dynamic system prompt, not here.
pub(super) const CONTEXT_ROLLOVER_MARKER: &str = "The model rolled this session's context over: earlier turns are out of the active context but stored in full at history://current, and the working notes in the system prompt hold the state to continue from.";

/// Index of the newest user message that carries text and no tool result.
///
/// A rollover boundary starts the active context here. It must never land on a
/// tool-result message: the call that produced the result would sit before the
/// boundary, the result would replay as an orphan pair, and the session
/// projection rejects that transcript.
fn last_user_turn_start(messages: &[crate::session::StoredMessage]) -> Option<usize> {
    messages.iter().rposition(|message| {
        matches!(message.role, Role::User)
            && message
                .content
                .iter()
                .any(|block| matches!(block, crate::message::ContentBlock::Text { .. }))
            && !message
                .content
                .iter()
                .any(|block| matches!(block, crate::message::ContentBlock::ToolResult { .. }))
    })
}

impl Agent {
    /// Apply a rollover the model asked for with `new_context`, if one is due.
    ///
    /// The tool only records the request; a tool round cannot rebuild its own
    /// transcript, because the assistant message that called it and the results
    /// still to come belong to the same turn. Both turn loops reach this through
    /// the turn settle step once the transcript is final.
    ///
    /// The rebuild is a compaction boundary over every message before the
    /// newest user turn. No summary is generated: the boundary carries
    /// `CONTEXT_ROLLOVER_MARKER`, or a text summary the session already had
    /// followed by it, so the next request is the dynamic system prompt (with
    /// the working notes), that user message, and the marker. Nothing is
    /// removed from the stored transcript: `history://current` still serves all
    /// of it.
    ///
    /// One request is one rollover. Calling the tool twice in a turn leaves one
    /// pending request, and the second call is consumed by the same boundary.
    pub(super) fn apply_pending_context_rollover(&mut self) {
        let session_id = self.session.id.clone();
        if !crate::tool::new_context::rollover_pending(&session_id) {
            return;
        }
        let Some(boundary) = last_user_turn_start(self.session.active_context()) else {
            crate::tool::new_context::clear_rollover(&session_id);
            return;
        };
        // The active context already starts at the newest user turn: there is
        // nothing to drop, and the request is finished.
        if boundary == 0 {
            crate::tool::new_context::clear_rollover(&session_id);
            return;
        }
        let offset = self.session.active_history_offset();
        let active_len = self.session.active_context().len();
        // A text summary the model already had keeps describing everything the
        // new boundary now covers, so it stays in front of the marker. A
        // provider-native window is dropped instead: advancing the boundary
        // past it would replay an opaque payload the server built for the
        // earlier coverage.
        let prior_summary = self
            .session
            .compaction
            .as_ref()
            .filter(|state| !state.is_native())
            .map(|state| state.summary_text.trim().to_string())
            .filter(|text| !text.is_empty());
        let summary_text = match prior_summary {
            Some(prior) => format!("{prior}\n\n{CONTEXT_ROLLOVER_MARKER}"),
            None => CONTEXT_ROLLOVER_MARKER.to_string(),
        };
        let state = crate::session::StoredCompactionState {
            summary_text,
            covers_up_to_turn: offset + boundary,
            original_turn_count: offset + active_len,
            compacted_count: boundary,
            ..Default::default()
        };

        let compaction = self.registry.compaction();
        let Ok(mut manager) = compaction.try_write() else {
            // Leave the request pending: the boundary still applies, it just
            // waits for a turn whose manager lock is free.
            logging::warn("[compaction/rollover] deferred: compaction manager lock busy");
            return;
        };
        manager.set_budget(self.provider.context_window());
        manager.restore_persisted_stored_state_with(&state, self.session.active_context());
        // Persisting the state rewrites the stored boundary and refreshes the
        // resident active context to the messages after it.
        self.sync_session_compaction_state_from_manager(&mut manager);
        self.session.set_compaction_generation(manager.generation());
        crate::tool::new_context::clear_rollover(&session_id);
        self.note_compaction_applied();
        logging::info(&format!(
            "[compaction/rollover] session {} rolled over at message {} of {} ({} kept)",
            session_id,
            offset + boundary,
            offset + active_len,
            active_len - boundary,
        ));
    }

    /// Mark this agent as living inside the persistent serve/api-bridge daemon.
    /// Only such agents run idle background compaction.
    pub fn set_persistent_runtime(&mut self, persistent: bool) {
        self.persistent_runtime = persistent;
    }

    pub fn persistent_runtime(&self) -> bool {
        self.persistent_runtime
    }

    /// Invalidate any in-flight compaction result built on the transcript as it
    /// was before this mutation (rewind, clear, revision, import, model change).
    pub fn note_transcript_mutation(&mut self, reason: crate::compaction::jobs::CancelReason) {
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.invalidate_generation(reason);
        }
        // Earlier `read` output may no longer be in the transcript.
        crate::tool::read_state::forget_session(&self.session.id);
    }

    /// Cancel a running compaction job (shutdown, reset) without touching the
    /// generation: the result is discarded either way.
    pub fn cancel_background_compaction(&mut self, reason: crate::compaction::jobs::CancelReason) {
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.cancel_pending(reason);
        }
    }

    /// Cheap pre-check for an idle job: can one start at all, judged from
    /// manager counters and the resident message count?
    ///
    /// Nothing here touches message bodies, so a turn that cannot compact never
    /// pays for building the provider projection.
    fn idle_compaction_may_start(&self, manager: &crate::compaction::CompactionManager) -> bool {
        if manager.is_compacting() || manager.strategy().is_native() || manager.context_budget().is_none() {
            return false;
        }
        let total = self.session.active_context().len();
        let active_len = total.saturating_sub(manager.compacted_count().min(total));
        active_len > crate::compaction::MIN_TURNS_TO_KEEP
    }

    /// Attempt to start an idle background compaction job.
    ///
    /// Only persistent serve/api-bridge agents qualify, only at >=50% context
    /// fill, and only with no job already in flight. Returns whether a job
    /// started.
    ///
    /// The manager lock and its own gate are taken before the provider context
    /// is materialized, and the gate reads a borrowed slice: a contended lock or
    /// a declined start costs no clone of the transcript.
    pub fn maybe_start_idle_compaction(&mut self) -> bool {
        if !self.persistent_runtime || !self.provider.uses_horon_agent_compaction() {
            return false;
        }
        let persistent = self.persistent_runtime;
        let compaction = self.registry.compaction();
        let Ok(mut manager) = compaction.try_write() else {
            return false;
        };
        if !self.idle_compaction_may_start(&manager) {
            return false;
        }
        let refusal = {
            let provider_messages = self.session.provider_messages();
            manager.background_start_refusal(crate::compaction::jobs::JobTrigger::Idle, persistent, provider_messages)
        };
        if let Some(refusal) = refusal {
            logging::info(&format!(
                "[compaction/job] idle start pre-check refused reason={}",
                refusal.as_str()
            ));
            return false;
        }
        let provider = self.provider.fork();
        let provider_messages = self.session.messages_for_provider_ref();
        let started = manager.maybe_start_idle_compaction_with(provider_messages, provider, persistent);
        self.session.set_compaction_generation(manager.generation());
        started
    }

    pub(super) fn note_compaction_applied(&mut self) {
        // The compacted view no longer holds the results the ledger named.
        self.session.pruned_tool_results.clear();
        self.cache_tracker.reset();
        self.kv_cache_monitor.reset();
        self.locked_tools = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        // Compacted `read` output is no longer verbatim in context.
        crate::tool::read_state::forget_session(&self.session.id);
        self.advisor_history_rewritten();
    }

    pub fn poll_compaction_completion_event(&mut self) -> Option<CompactionEvent> {
        let compaction = self.registry.compaction();
        // Manager first: a contended lock must not materialize the context.
        let Ok(mut manager) = compaction.try_write() else {
            return None;
        };
        // The poll only reads the transcript (boundary clamp and recovery), so
        // it takes the borrowed projection instead of an owned copy.
        let event = {
            let provider_messages = self.session.messages_for_provider_ref();
            manager.poll_compaction_event_with(provider_messages)
        };

        if event.is_some() {
            self.sync_session_compaction_state_from_manager(&mut manager);
            self.note_compaction_applied();
            self.session.set_compaction_generation(manager.generation());
            self.persist_session_best_effort("compaction completion");
        }

        event
    }

    pub fn request_manual_compaction(&mut self) -> (String, bool) {
        if !self.provider.supports_compaction() {
            return (
                "Manual compaction is not available for this provider.".to_string(),
                false,
            );
        }

        let provider = self.provider.fork();
        let messages = self.session.messages_for_provider_ref();
        let compaction = self.registry.compaction();

        match compaction.try_write() {
            Ok(mut manager) => {
                let stats = manager.stats_with(messages);
                let status_msg = format!(
                    "**Context Status:**\n\
                    • Messages: {} (active), {} (total history)\n\
                    • Token usage: ~{}k (estimate ~{}k) / {}k ({:.1}%)\n\
                    • Has summary: {}\n\
                    • Compacting: {}",
                    stats.active_messages,
                    stats.total_turns,
                    stats.effective_tokens / 1000,
                    stats.token_estimate / 1000,
                    manager.token_budget() / 1000,
                    stats.context_usage * 100.0,
                    if stats.has_summary { "yes" } else { "no" },
                    if stats.is_compacting { "in progress..." } else { "no" }
                );

                match manager.force_compact_with(messages, provider) {
                    Ok(()) => (
                        format!(
                            "{}\n\n📦 **Compacting context** (manual) — summarizing older messages in the background to stay within the context window.\n\
                            The summary will be applied automatically when ready.",
                            status_msg
                        ),
                        true,
                    ),
                    Err(reason) => (format!("{status_msg}\n\n⚠ **Cannot compact:** {reason}"), false),
                }
            }
            Err(_) => ("⚠ Cannot access compaction manager (lock held)".to_string(), false),
        }
    }

    fn is_context_limit_error(error: &str) -> bool {
        let lower = error.to_lowercase();
        lower.contains("context length")
            || lower.contains("context window")
            || lower.contains("maximum context")
            || lower.contains("max context")
            || lower.contains("token limit")
            || lower.contains("too many tokens")
            || lower.contains("prompt is too long")
            || lower.contains("input is too long")
            || lower.contains("request too large")
            || lower.contains("length limit")
            || lower.contains("maximum tokens")
            || (lower.contains("exceeded") && lower.contains("tokens"))
    }

    /// Best-effort emergency recovery after a context-limit error.
    ///
    /// Performs a synchronous hard compaction and resets provider session state,
    /// allowing the caller to retry the same turn immediately.
    pub(super) fn try_auto_compact_after_context_limit(&mut self, error: &str) -> bool {
        if crate::provider::openai_request::is_openai_encrypted_content_too_large_error(error)
            && self.try_recover_oversized_openai_native_compaction()
        {
            return true;
        }
        // A provider HTTP 413 ("request too large") is a *byte-size* failure
        // driven by inline base64 images, not a token-context overflow. Token
        // accounting deliberately undercounts images, so ordinary compaction
        // would not shrink the payload and the retry would 413 again. Strip
        // oversized images first.
        if self.try_recover_after_payload_too_large(error) {
            return true;
        }
        if self.try_recover_after_image_rejection(error) {
            return true;
        }
        if !Self::is_context_limit_error(error) {
            return false;
        }
        if !self.provider.supports_compaction() {
            return false;
        }

        let context_limit = self.provider.context_window() as u64;
        let compaction = self.registry.compaction();

        let (dropped, usage_pct, lossy) = match compaction.try_write() {
            Ok(mut manager) => {
                let all_messages = self.session.provider_messages();
                manager.update_observed_input_tokens(context_limit);
                if manager.recovery_refusal_active(all_messages) {
                    logging::warn(
                        "Context-limit auto-recovery refused: this source, model and settings were already blocked; automatic repetition stays off until the transcript or route changes",
                    );
                    return false;
                }
                let usage_pct = manager.context_usage_with(all_messages) * 100.0;
                let recovered = match manager.hard_compact_with(all_messages) {
                    Ok(dropped) => Some((dropped, true)),
                    Err(reason) => {
                        logging::warn(&format!(
                            "Context-limit auto-recovery: hard compact failed ({reason}); trying the deterministic fallback"
                        ));
                        match manager.recover_failed_summary_with(all_messages, &reason) {
                            crate::compaction::recovery::RecoveryOutcome::Applied(report) => {
                                Some((report.covered_messages, true))
                            }
                            blocked => {
                                logging::warn(&format!(
                                    "Context-limit auto-recovery blocked: {:?}",
                                    blocked.blocked().map(|blocked| blocked.reason)
                                ));
                                None
                            }
                        }
                    }
                };
                let outcome = match recovered {
                    Some(outcome) => outcome,
                    None => {
                        self.sync_session_compaction_state_from_manager(&mut manager);
                        return false;
                    }
                };
                self.sync_session_compaction_state_from_manager(&mut manager);
                (outcome.0, usage_pct, outcome.1)
            }
            Err(_) => {
                logging::warn("Context-limit auto-recovery skipped: compaction manager lock busy");
                return false;
            }
        };

        self.cache_tracker.reset();
        self.kv_cache_monitor.reset();
        self.locked_tools = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;

        logging::warn(&format!(
            "Context limit exceeded; auto-compacted and retrying (dropped {} messages, usage was {:.1}%, lossy={lossy})",
            dropped, usage_pct
        ));
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "auto_compaction_applied",
                "context_limit_auto_compaction",
            )
            .with_session_id(self.session.id.clone())
            .with_detail(format!(
                "dropped_messages={dropped},usage_pct={usage_pct:.1},lossy={lossy}"
            ))
            .force_attribution(),
        );

        true
    }

    /// Best-effort recovery after a provider HTTP 413 "request too large" error.
    ///
    /// This failure is caused by the serialized request body (dominated by inline
    /// base64 images) exceeding the provider's size cap, which is independent of
    /// the token context window. We strip oversized images from the persisted
    /// transcript, oldest-first, down to a conservative byte budget and reset the
    /// provider session/cache so the caller can retry the same turn immediately.
    fn try_recover_after_payload_too_large(&mut self, error: &str) -> bool {
        if !crate::compaction::is_request_payload_too_large_error(error) {
            return false;
        }

        let stripped = self
            .session
            .strip_oversized_images(crate::compaction::PAYLOAD_IMAGE_CHAR_BUDGET);
        if stripped == 0 {
            logging::warn("Request-too-large recovery skipped: no oversized inline images to strip");
            return false;
        }
        self.reset_after_transcript_image_rewrite();

        logging::warn(&format!(
            "Request body exceeded provider size limit; stripped {} oversized inline image(s) and retrying",
            stripped
        ));
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "payload_too_large_recovered",
                "request_payload_too_large",
            )
            .with_session_id(self.session.id.clone())
            .with_detail(format!("images_stripped={stripped}"))
            .force_attribution(),
        );

        true
    }

    /// A provider rejected an inline image with a deterministic 400 (bad
    /// media type, undecodable data, a limit horon-agent does not model). Every
    /// retry would replay the same image, so replace stored images with text
    /// notes and retry once (#1712). The outbound clamp normally prevents
    /// this; this catches provider rules it does not know about.
    fn try_recover_after_image_rejection(&mut self, error: &str) -> bool {
        if !crate::compaction::is_image_rejection_error(error) {
            return false;
        }
        let stripped = self.session.strip_all_images();
        if stripped == 0 {
            return false;
        }
        self.reset_after_transcript_image_rewrite();
        if let Err(err) = self.save_session() {
            logging::warn(&format!(
                "Image-rejection recovery: failed to persist stripped transcript: {err}"
            ));
        }
        logging::warn(&format!(
            "Provider rejected an inline image; replaced {stripped} image(s) with text notes and retrying"
        ));
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "image_rejection_recovered",
                "provider_image_rejected",
            )
            .with_session_id(self.session.id.clone())
            .with_detail(format!("images_stripped={stripped}"))
            .force_attribution(),
        );
        true
    }

    /// The transcript's images changed; reseed compaction bookkeeping and
    /// reset provider session/cache state so the retry sends the new payload.
    fn reset_after_transcript_image_rewrite(&mut self) {
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            let state = self.session.compaction.clone();
            let provider_messages = self.session.messages_for_provider_ref();
            manager.reset();
            manager.set_budget(self.provider.context_window());
            if let Some(state) = state.as_ref() {
                manager.restore_persisted_state_with(state, provider_messages);
            } else {
                manager.seed_restored_messages_with(provider_messages);
            }
            self.session.set_compaction_generation(manager.generation());
            self.sync_session_compaction_state_from_manager(&mut manager);
        }

        self.cache_tracker.reset();
        self.kv_cache_monitor.reset();
        self.locked_tools = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;
    }

    fn try_recover_oversized_openai_native_compaction(&mut self) -> bool {
        let compaction = self.registry.compaction();
        let recovered = match compaction.try_write() {
            Ok(mut manager) => {
                if !manager.discard_oversized_openai_native_compaction() {
                    return false;
                }
                self.sync_session_compaction_state_from_manager(&mut manager);
                true
            }
            Err(_) => {
                logging::warn("OpenAI native compaction recovery skipped: compaction manager lock busy");
                false
            }
        };

        if !recovered {
            return false;
        }

        self.cache_tracker.reset();
        self.kv_cache_monitor.reset();
        self.locked_tools = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;

        logging::warn(
            "OpenAI native compaction payload exceeded provider size limit; discarded native state and retrying with text fallback",
        );
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "native_compaction_payload_recovered",
                "openai_encrypted_content_too_large",
            )
            .with_session_id(self.session.id.clone())
            .force_attribution(),
        );

        true
    }

    pub(super) fn effective_context_tokens_from_usage(
        &self,
        input_tokens: u64,
        cache_read_input_tokens: Option<u64>,
        cache_creation_input_tokens: Option<u64>,
    ) -> u64 {
        // Shared heuristic (horon-agent-compaction-core): keeps the compaction
        // manager's observed-token feed consistent with the client-side
        // context display.
        crate::compaction::effective_context_tokens_from_usage_with(
            self.provider.name(),
            self.provider.usage_is_inclusive(),
            input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        )
    }

    pub(super) fn update_compaction_usage_from_stream(
        &mut self,
        input_tokens: u64,
        cache_read_input_tokens: Option<u64>,
        cache_creation_input_tokens: Option<u64>,
    ) {
        // The prompt cache was written or refreshed by this response.
        self.last_provider_response_at = Some(std::time::Instant::now());
        if !self.provider.uses_horon_agent_compaction() || input_tokens == 0 {
            return;
        }
        let observed = self.effective_context_tokens_from_usage(
            input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        );
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.update_observed_input_tokens(observed);
            manager.push_token_snapshot(observed);
        };
    }

    /// Push an embedding snapshot for the semantic compaction mode.
    /// Called after each assistant turn with a short text snippet.
    /// No-op if the embedding model is unavailable or mode is not semantic.
    pub(super) fn push_embedding_snapshot_if_semantic(&mut self, text: &str) {
        use crate::config::CompactionMode;
        let is_semantic = {
            let compaction = self.registry.compaction();
            compaction
                .try_read()
                .map(|m| m.mode() == CompactionMode::Semantic)
                .unwrap_or(false)
        };
        if !is_semantic {
            return;
        }
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.push_embedding_snapshot(text);
        };
    }
}

#[cfg(test)]
#[path = "compaction_job_tests.rs"]
mod compaction_job_tests;

#[cfg(test)]
#[path = "context_limit_tests.rs"]
mod context_limit_tests;

#[cfg(test)]
#[path = "context_rollover_tests.rs"]
mod context_rollover_tests;
