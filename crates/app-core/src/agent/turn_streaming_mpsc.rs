use super::response_recovery::{MalformedToolCallInfo, TextToolRecovery};
use super::*;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;

use super::stream_rules::{MAX_INJECTIONS, STREAM_RULE_POINT, StreamRuleAction, ToolRuleAction, TurnStreamRules};
use super::streaming::find_wrap_marker_incremental;

/// Result text for a call skipped because the user interrupted the turn. Both
/// the group-boundary check and a mid-group interrupt write this, so the
/// transcript reads the same however the interrupt was noticed.
const USER_INTERRUPT_SKIP: &str = "[Skipped: user interrupted]";

/// Result text for a call skipped because the server is reloading.
const RELOAD_SKIP: &str = "[Skipped - server reloading]";

/// Send the file changes a tool call made, immediately before its `ToolDone`.
///
/// The diffs come from the tool's own metadata and are read before the history
/// caps and the round budget rewrite the result. They leave only through the
/// ACP connection's sender, so a client that renders no diffs never sees them.
fn send_tool_diff(session_id: &str, call_id: &str, output: &crate::tool::ToolOutput) {
    let Some(diffs) = output
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("file_diffs"))
        .and_then(|diffs| serde_json::from_value::<Vec<crate::protocol::FileDiffWire>>(diffs.clone()).ok())
    else {
        return;
    };
    if diffs.is_empty() {
        return;
    }
    crate::tool::acp_session::send_for_version(session_id, |version| {
        let mut diffs = diffs;
        // Deletes, creates and moves are v2 fields: a v1 owner gets the diff
        // bytes it always got.
        if version != Some(2) {
            for diff in &mut diffs {
                diff.deleted = false;
                diff.created = false;
                diff.old_path = None;
            }
        }
        ServerEvent::ToolDiff {
            id: call_id.to_string(),
            diffs,
        }
    });
}

/// Send how a shell call exited, immediately before its `ToolDone`, to an ACP
/// connection that negotiated v2 (its terminal view). A call handed to a
/// background task reports `backgrounded` without an exit.
fn send_tool_exit(session_id: &str, call_id: &str, tool_name: &str, output: &crate::tool::ToolOutput) {
    if tool_name != "bash" {
        return;
    }
    let Some(metadata) = output.metadata.as_ref() else {
        return;
    };
    let event = if let Some(exit) = metadata.get("exit_status") {
        ServerEvent::ToolExit {
            id: call_id.to_string(),
            exit_code: exit
                .get("exit_code")
                .and_then(serde_json::Value::as_i64)
                .and_then(|code| i32::try_from(code).ok()),
            signal: exit
                .get("signal")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            backgrounded: false,
        }
    } else if metadata.get("background").and_then(serde_json::Value::as_bool) == Some(true) {
        ServerEvent::ToolExit {
            id: call_id.to_string(),
            exit_code: None,
            signal: None,
            backgrounded: true,
        }
    } else {
        return;
    };
    crate::tool::acp_session::send_v2(session_id, event);
}

fn reload_interrupted_tool_result(tc: &ToolCall, elapsed_secs: f64) -> (String, bool) {
    let action = tc
        .input
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let is_wait_like =
        (tc.name == "bg" && action == "wait") || (tc.name == "swarm" && matches!(action, "await_members" | "run_plan"));

    if is_wait_like {
        let input = serde_json::to_string(&tc.input).unwrap_or_else(|_| "{}".to_string());
        return (
            format!(
                "[Tool '{}' wait interrupted by server reload after {:.1}s. The underlying operation may still be running. Resume the wait by rerunning the same tool call with input: {}]",
                tc.name, elapsed_secs, input
            ),
            false,
        );
    }

    (
        format!(
            "[Tool '{}' interrupted by server reload after {:.1}s]",
            tc.name, elapsed_secs
        ),
        true,
    )
}

/// Called only after automatic continuations have been exhausted.
fn incomplete_turn_stop(stop_reason: Option<&str>) -> Option<ServerEvent> {
    let reason = stop_reason?;
    if Agent::should_continue_after_stop_reason(reason) || Agent::is_stranded_tool_use_stop(Some(reason)) {
        Some(ServerEvent::TurnStopped {
            reason: crate::protocol::TurnStopReason::LimitReached,
            message: format!(
                "The provider stopped with {reason} after automatic continuation attempts were exhausted. Output may be incomplete."
            ),
            provider_stop_reason: Some(reason.to_string()),
        })
    } else {
        None
    }
}

impl Agent {
    pub(super) async fn run_turn_streaming_mpsc(&mut self, event_tx: mpsc::UnboundedSender<ServerEvent>) -> Result<()> {
        // Loop detection is per turn, and a reminder this turn's guard set must
        // not reach the next turn.
        self.loop_guard.reset();
        self.turn_state = TurnState::new();
        // Register this turn's cancel signal for the whole turn, including the
        // outcome bookkeeping below: the guard resets the signal when it drops,
        // so a scope that ended before the outcome was named would report every
        // cancelled turn as a plain stop.
        let _turn_cancel_guard =
            crate::turn_cancel_registry::register_active_turn(&self.session.id, self.graceful_shutdown.clone());
        let mut in_flight = Some(InFlightGuard::new(
            self.session.id.clone(),
            self.soft_interrupt_queue.clone(),
        ));
        let outcome = AssertUnwindSafe(self.run_turn_streaming_mpsc_inner(event_tx, in_flight.as_mut()))
            .catch_unwind()
            .await;
        self.current_turn_system_reminder = None;
        match outcome {
            Ok(result) => {
                self.settle_soft_interrupt_turn(&mut in_flight, result.is_ok() && !self.is_graceful_shutdown())
                    .await;
                self.settle_streaming_turn_outcome(result.is_err());
                result
            }
            Err(payload) => {
                // A panic never reaches the normal settle point. Roll the
                // injected messages back here, then let the unwind continue
                // so the caller still observes the panic it caused.
                self.settle_soft_interrupt_turn(&mut in_flight, false).await;
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Record the streaming turn's exit reason when its loop left one open.
    fn settle_streaming_turn_outcome(&mut self, failed: bool) {
        if self.turn_state().outcome().is_some() {
            return;
        }
        let outcome = if failed {
            TurnOutcome::Error
        } else if self.is_graceful_shutdown() {
            TurnOutcome::Aborted
        } else {
            TurnOutcome::Stop
        };
        if let Err(error) = self.turn_state.finish(outcome) {
            logging::warn(&format!("Turn outcome not recorded: {error}"));
        }
    }

    async fn run_turn_streaming_mpsc_inner(
        &mut self,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
        mut in_flight: Option<&mut InFlightGuard>,
    ) -> Result<()> {
        self.ensure_session_lease()?;
        // Same barrier as the non-streaming entry point: a deferred restore
        // commit lands before any provider or tool work.
        self.finish_resume_persistence()?;
        self.set_log_context();
        self.begin_failover_chain_for_prompt();
        let usage_turn_id = self.model_usage_turn_id();
        // Mark this session as actively streaming for presence UIs (e.g. the
        // macOS menu bar indicator). Cleared automatically on every exit path.
        let _streaming_guard = crate::session::StreamingGuard::new(self.session.id.clone());
        crate::tool::superseded::clear(&self.session.id);
        let trace = trace_enabled();
        // Opt-in mid-stream rules are constructed once per turn and live across
        // every retry of it, so a rule that already matched is never retried and
        // the three-injection cap counts attempts of this turn, not of one request.
        let mut stream_rule_state = TurnStreamRules::from_config(
            &self.session.stream_rule_ledger,
            super::stream_rules::user_turn_index(&self.session),
        );

        loop {
            // Never open a new provider request after a cancel. Several paths
            // `continue` this loop (compaction retry, incomplete/stranded
            // continuation, empty-response recovery, soft-interrupt injection),
            // and without this check an Esc that landed during a stream could
            // be followed by another full request, which looks to the user like
            // the interrupt was ignored (issue #732, regression of #428).
            if self.is_graceful_shutdown() {
                logging::info("Cancel observed at turn-loop head - not starting another request");
                break;
            }
            if let Some(block) = self.session.migration_lease_block() {
                logging::info(&format!(
                    "Session migrated mid-turn - stopping local turn loop: {block}"
                ));
                break;
            }
            let repaired = self.repair_missing_tool_outputs();
            if repaired > 0 {
                logging::warn(&format!(
                    "Recovered {} missing tool output(s) before API call",
                    repaired
                ));
            }
            // Start provider transport setup before deriving and potentially
            // compacting the request history. This is the first point where the
            // stable request settings are available.
            let mut tools = self.tool_definitions().await;
            let mut split_prompt = self.build_system_prompt_split(None);
            self.provider
                .prewarm_with_context(&tools, &split_prompt.static_part, &self.provider_request_context())
                .await;
            let (messages, compaction_event) = self.messages_for_provider();
            if let Some(event) = compaction_event {
                // Reset cache tracker and tool lock on compaction since the message history changes
                self.cache_tracker.reset();
                self.kv_cache_monitor.reset();
                self.locked_tools = None;
                logging::info(&format!(
                    "Context compacted ({}{})",
                    event.trigger,
                    event.pre_tokens.map(|t| format!(" {} tokens", t)).unwrap_or_default()
                ));
                let _ = event_tx.send(ServerEvent::Compaction {
                    trigger: event.trigger.clone(),
                    pre_tokens: event.pre_tokens,
                    post_tokens: event.post_tokens,
                    tokens_saved: event.tokens_saved,
                    duration_ms: event.duration_ms,
                    messages_dropped: event.messages_dropped,
                    messages_compacted: event.messages_compacted,
                    summary_chars: event.summary_chars,
                    active_messages: event.active_messages,
                });
                // Compaction clears the tool lock, so rebuild the foreground
                // request metadata rather than relying on the pre-compaction snapshot.
                tools = self.tool_definitions().await;
                split_prompt = self.build_system_prompt_split(None);
            }

            // `messages` is this turn's single owned request snapshot. Memory,
            // timestamps and masking each derive a small delta from it instead
            // of copying the whole transcript again.
            // Non-blocking memory: uses pending result from last turn, spawns check for next turn
            let memory_pending = self.build_memory_prompt_nonblocking_shared(
                std::sync::Arc::clone(&messages),
                Some(std::sync::Arc::new({
                    let event_tx = event_tx.clone();
                    move |event| {
                        let _ = event_tx.send(event);
                    }
                })),
            );
            // Use split prompt for better caching - static content cached, dynamic not
            self.log_prompt_prefix_accounting(&split_prompt, &tools);

            // Check for client-side cache violations before memory injection.
            // Memory is an ephemeral suffix that changes each turn; tracking it would cause
            // false-positive violations every turn (prior turn's memory ≠ current history prefix).
            self.record_client_cache_request(&messages);

            // `messages` now owns the provider-facing request snapshot. Do not
            // retain the session's second, derived copy for the entire network
            // wait and response stream.
            self.session.release_provider_messages_cache();

            // Only the appended tail is owned; the transcript prefix stays borrowed.
            let mut request_suffix: Vec<Message> = Vec::new();
            // A persisted memory message joins the cache prefix, while an
            // ephemeral one is tracked as its own hash so it cannot look like a
            // prefix edit on the next turn.
            let mut persisted_signature_suffix: Vec<Message> = Vec::new();
            let mut ephemeral_signature_messages: Vec<Message> = Vec::new();

            // Inject memory as a user message at the end (preserves cache prefix)
            if let Some(memory) = memory_pending.as_ref() {
                let memory_count = memory.count.max(1);
                let computed_age_ms = memory.computed_at.elapsed().as_millis() as u64;
                crate::memory::record_injected_prompt(&memory.prompt, memory_count, computed_age_ms);
                self.record_memory_injection_in_session(memory);
                let _ = event_tx.send(ServerEvent::MemoryInjected {
                    count: memory_count,
                    prompt: memory.prompt.clone(),
                    display_prompt: memory.display_prompt.clone(),
                    prompt_chars: memory.prompt.chars().count(),
                    computed_age_ms,
                });
                let (memory_msg, persisted) = self.prepare_memory_injection_message(memory);
                if !persisted {
                    ephemeral_signature_messages.push(memory_msg.clone());
                } else {
                    persisted_signature_suffix.push(memory_msg.clone());
                }
                request_suffix.push(memory_msg);
            }
            logging::info(&format!(
                "API call starting: {} messages, {} tools",
                messages.len() + request_suffix.len(),
                tools.len()
            ));
            let api_start = Instant::now();

            // A suffix (memory, plan-limit notice) is the only reason the request
            // grows; an unmodified request stays a borrow of the snapshot.
            let joined_request =
                (!request_suffix.is_empty()).then(|| join_request_messages(&messages, &request_suffix));
            if joined_request.is_some() {
                record_request_extra_clone(&messages);
            }
            let request_base = joined_request.as_deref().unwrap_or(&messages);
            let stamped = crate::config::config()
                .features
                .message_timestamps
                .then(|| Message::with_timestamps(request_base));
            if stamped.is_some() {
                record_request_extra_clone(request_base);
            }
            let send_messages = stamped.as_deref().unwrap_or(request_base);
            // The provider reads placeholders; the vault keeps the real bytes for
            // the tool that later needs them. `block` refuses the request here.
            let masked_messages = crate::secrets::outbound_messages(
                send_messages,
                &self.session.id,
                crate::config::config().tools.secrets_mode,
            )?;
            if masked_messages.is_some() {
                record_request_extra_clone(send_messages);
            }
            let send_messages = masked_messages.as_deref().unwrap_or(send_messages);
            let prompt_has_recent_tool_result = Self::messages_end_with_tool_result(send_messages);
            let provider = Arc::clone(&self.provider);
            // Capture the model id the request was issued with. A provider may
            // transparently switch models mid-request (e.g. Anthropic's retired
            // `claude-fable-5` falls back to `claude-opus-4-8`). When that
            // happens the provider mutates its own model state, but the session
            // and clients still believe they are on the originally requested
            // model. Compare against this after the stream so we can emit a
            // `ModelChanged` and resync the UI/context-limit.
            let model_at_request_start = provider.model().to_string();
            let credential_at_request_start = provider.active_resolved_credential();
            let resume_session_id = self.provider_session_id.clone();
            self.last_status_detail = None;
            // The signature covers the transcript exactly as the provider sees
            // it, plus the persisted memory tail. `with_timestamps` maps each
            // message independently, so the stamped request's leading
            // `messages.len()` entries already are the transcript's timestamped
            // projection; reusing them avoids stamping the same history twice.
            // An ephemeral suffix is hashed separately so it never reads as a
            // mutated prefix on the following turn.
            let signature_base: &[Message] = match stamped.as_deref() {
                Some(stamped) => &stamped[..messages.len()],
                None => &messages,
            };
            let kv_request = kv_cache_request_event_ref(
                signature_base.iter().chain(persisted_signature_suffix.iter()),
                signature_base.len() + persisted_signature_suffix.len(),
                &tools,
                &split_prompt.static_part,
                &ephemeral_signature_messages,
            );
            self.begin_kv_cache_monitor_request(&kv_request, &model_at_request_start);
            let _ = event_tx.send(kv_request);
            // Only the signature's small derived vectors are released here; the
            // request itself is still borrowed by `send_messages`.
            drop(persisted_signature_suffix);
            drop(ephemeral_signature_messages);
            let mut keepalive = stream_keepalive_ticker();
            // The provider gets this session's lineage with every call, so it can
            // pin its cache key to the same family on a retry as on the first
            // attempt. Nothing is written to the shared provider state.
            let provider_context = self.provider_request_context();
            let stream = {
                let mut complete_future = std::pin::pin!(provider.complete_split_with_context(
                    send_messages,
                    &tools,
                    &split_prompt.static_part,
                    &split_prompt.dynamic_part,
                    resume_session_id.as_deref(),
                    &provider_context,
                ));
                loop {
                    tokio::select! {
                        _ = keepalive.tick() => {
                            send_stream_keepalive_mpsc(&event_tx);
                        }
                        _ = self.graceful_shutdown.notified() => {
                            logging::info(
                                "Graceful shutdown/cancel before API stream opened - stopping turn",
                            );
                            return Ok(());
                        }
                        result = &mut complete_future => {
                                            match result {
                                Ok(stream) => break Some(stream),
                                Err(e) => {
                                    self.record_failed_credential_route(&e.to_string());
                                    let attached_client =
                                        crate::server::session_has_attached_client(&self.session.id);
                                    match self.handle_provider_error(&e, attached_client).await {
                                        super::turn_failover::ProviderErrorAction::Resend { .. } => break None,
                                        super::turn_failover::ProviderErrorAction::Stop(error) => return Err(error),
                                        super::turn_failover::ProviderErrorAction::Proceed => {}
                                    }
                                    if self.try_auto_compact_after_context_limit(&e.to_string()) {
                                        let retries = self.turn_state.record_context_limit_retry();
                                        if retries > Self::MAX_CONTEXT_LIMIT_RETRIES {
                                            logging::warn(
                                                "Context-limit compaction retry limit reached; giving up",
                                            );
                                            let _ = event_tx.send(ServerEvent::TurnStopped {
                                                reason: crate::protocol::TurnStopReason::LimitReached,
                                                message: format!("Context limit exceeded after {} compaction retries", Self::MAX_CONTEXT_LIMIT_RETRIES),
                                                provider_stop_reason: None,
                                            });
                                            return Err(anyhow::anyhow!(
                                                "Context limit exceeded after {} compaction retries",
                                                Self::MAX_CONTEXT_LIMIT_RETRIES
                                            ));
                                        }
                                        let _ = event_tx.send(ServerEvent::Compaction {
                                            trigger: "auto_recovery".to_string(),
                                            pre_tokens: None,
                                            post_tokens: None,
                                            tokens_saved: None,
                                            duration_ms: None,
                                            messages_dropped: None,
                                            messages_compacted: None,
                                            summary_chars: None,
                                            active_messages: None,
                                        });
                                        continue;
                                    }
                                    return Err(e);
                                }
                            }
                        }
                    }
                }
            };

            let Some(mut stream) = stream else {
                logging::info("Provider recovery applied before the stream opened - resending the turn");
                continue;
            };

            // `complete_split` has consumed the request and returned an owned
            // response stream. Keeping these full transcript snapshots alive
            // while tokens arrive needlessly multiplies active-session memory.
            drop(stamped);
            drop(joined_request);
            drop(request_suffix);
            drop(memory_pending);
            drop(messages);
            drop(split_prompt);

            // Successful API call - reset retry counter
            self.turn_state.reset_context_limit_retries();

            logging::info(&format!(
                "API stream opened in {:.2}s",
                api_start.elapsed().as_secs_f64()
            ));
            log_agent_provider_stream_lifecycle(
                logging::LogLevel::Info,
                self,
                "stream_opened",
                api_start,
                vec![("mode", "mpsc".to_string())],
            );

            let mut text_content = String::new();
            let mut text_wrapped_detected = false;
            // Inline swarm worker output tap: publish a throttled tail of the
            // in-progress assistant text to the bus so a coordinator can render
            // a live inline gallery viewport.
            let inline_output_tap = self.inline_output_tap();
            let mut inline_tap_last = Instant::now()
                .checked_sub(std::time::Duration::from_millis(1000))
                .unwrap_or_else(Instant::now);
            // Throttled "this session is alive" marks while tokens stream, so
            // swarm status can distinguish a busy worker from a dead one
            // without paying a registry lock per token.
            let mut activity_mark_last = Instant::now()
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or_else(Instant::now);
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut current_tool: Option<String> = None;
            let mut streaming_tools: HashMap<String, (ToolCall, String)> = HashMap::new();
            let mut generated_image_contexts: Vec<Vec<ContentBlock>> = Vec::new();
            let mut usage_input: Option<u64> = None;
            let mut usage_output: Option<u64> = None;
            let mut usage_cache_read: Option<u64> = None;
            let mut usage_cache_creation: Option<u64> = None;
            let mut saw_message_end = false;
            let mut stop_reason: Option<String> = None;
            let mut sdk_tool_results: std::collections::HashMap<String, (String, bool)> =
                std::collections::HashMap::new();
            let provider_name = self.provider.name().to_string();
            let store_reasoning_content = crate::provider::stores_reasoning_content_for_context(&provider_name);
            let mut reasoning_content = String::new();
            let mut reasoning_signature = String::new();
            // Whether a live reasoning region is currently streaming to the client.
            // Raw reasoning deltas are sent as `ReasoningDelta`; the client owns the
            // dim/italic styling and live partial-line rendering. We close the region
            // (via `ReasoningDone`) before real output or a tool call begins.
            let mut reasoning_open = false;
            // Last time hidden (non-displayed) reasoning activity was relayed
            // to clients as a keepalive; throttles issue #451 keepalives.
            let mut hidden_activity_last = Instant::now();
            let mut openai_reasoning_items: Vec<ContentBlock> = Vec::new();
            // Provider-executed tool items (e.g. native web search), stored
            // verbatim at their position in the response for exact replay.
            let mut provider_native_items = horon_agent_message_types::provider_native::ProviderNativeItems::default();
            let mut provider_native_tracker =
                horon_agent_message_types::provider_native::ProviderNativeTracker::default();
            let mut openai_native_compaction: Option<(Vec<serde_json::Value>, usize, Option<u64>)> = None;
            let mut tool_id_to_name: std::collections::HashMap<String, String> = std::collections::HashMap::new();

            let mut retry_after_compaction = false;
            let mut retry_after_recovery = false;
            let mut retry_after_stream_rule = false;
            let mut stream_rule_stop = false;
            let mut keepalive = stream_keepalive_ticker();
            loop {
                let next_event = std::pin::pin!(stream.next());
                let event = tokio::select! {
                    _ = keepalive.tick() => {
                        send_stream_keepalive_mpsc(&event_tx);
                        continue;
                    }
                    _ = self.graceful_shutdown.notified() => {
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Warn,
                            self,
                            "stream_cancelled",
                            api_start,
                            vec![
                                ("mode", "mpsc".to_string()),
                                ("reason", "graceful_shutdown".to_string()),
                            ],
                        );
                        logging::info(
                            "Graceful shutdown/cancel while waiting for API stream event - stopping stream",
                        );
                        break;
                    }
                    event = next_event => event,
                };

                if activity_mark_last.elapsed() >= std::time::Duration::from_secs(2) {
                    activity_mark_last = Instant::now();
                    crate::session_metrics::record_activity(&self.session.id);
                }
                let Some(event) = event else {
                    log_agent_provider_stream_lifecycle(
                        if saw_message_end {
                            logging::LogLevel::Info
                        } else {
                            logging::LogLevel::Warn
                        },
                        self,
                        "stream_eof",
                        api_start,
                        vec![
                            ("mode", "mpsc".to_string()),
                            ("saw_message_end", saw_message_end.to_string()),
                        ],
                    );
                    break;
                };
                let event = match event {
                    Ok(event) => event,
                    Err(e) => {
                        self.record_failed_credential_route(&e.to_string());
                        let attached_client = crate::server::session_has_attached_client(&self.session.id);
                        match self.handle_provider_error(&e, attached_client).await {
                            super::turn_failover::ProviderErrorAction::Resend { attempt, max } => {
                                if !text_content.is_empty() || !tool_calls.is_empty() {
                                    let _ = event_tx.send(ServerEvent::RetryRollback { attempt, max });
                                }
                                retry_after_recovery = true;
                                break;
                            }
                            super::turn_failover::ProviderErrorAction::Stop(error) => return Err(error),
                            super::turn_failover::ProviderErrorAction::Proceed => {}
                        }
                        let err_str = e.to_string();
                        if self.try_auto_compact_after_context_limit(&err_str) {
                            log_agent_provider_stream_lifecycle(
                                logging::LogLevel::Warn,
                                self,
                                "stream_error_retry_after_compaction",
                                api_start,
                                vec![
                                    ("mode", "mpsc".to_string()),
                                    ("error", err_str.clone()),
                                    (
                                        "context_limit_retries",
                                        (self.turn_state.context_limit_retries() + 1).to_string(),
                                    ),
                                ],
                            );
                            let retries = self.turn_state.record_context_limit_retry();
                            if retries > Self::MAX_CONTEXT_LIMIT_RETRIES {
                                logging::warn("Context-limit compaction retry limit reached; giving up");
                                let _ = event_tx.send(ServerEvent::TurnStopped {
                                    reason: crate::protocol::TurnStopReason::LimitReached,
                                    message: format!(
                                        "Context limit exceeded after {} compaction retries",
                                        Self::MAX_CONTEXT_LIMIT_RETRIES
                                    ),
                                    provider_stop_reason: None,
                                });
                                return Err(anyhow::anyhow!(
                                    "Context limit exceeded after {} compaction retries",
                                    Self::MAX_CONTEXT_LIMIT_RETRIES
                                ));
                            }
                            retry_after_compaction = true;
                            let _ = event_tx.send(ServerEvent::Compaction {
                                trigger: "auto_recovery".to_string(),
                                pre_tokens: None,
                                post_tokens: None,
                                tokens_saved: None,
                                duration_ms: None,
                                messages_dropped: None,
                                messages_compacted: None,
                                summary_chars: None,
                                active_messages: None,
                            });
                            break;
                        }
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Error,
                            self,
                            "stream_error",
                            api_start,
                            vec![("mode", "mpsc".to_string()), ("error", err_str)],
                        );
                        return Err(e);
                    }
                };

                let input_tool_id = match &event {
                    StreamEvent::ToolInputDeltaFor { id, .. } | StreamEvent::ToolUseEndFor { id } => Some(id.clone()),
                    _ => current_tool.clone(),
                };
                match event {
                    StreamEvent::ThinkingStart => {
                        // Reasoning tokens are counted in provider output usage even when
                        // `display.show_thinking` hides the text. Let remote clients start
                        // their TPS timer without forcing hidden reasoning into the transcript.
                        let _ = event_tx.send(ServerEvent::ConnectionPhase {
                            phase: crate::message::ConnectionPhase::Streaming.to_string(),
                        });
                    }
                    StreamEvent::ThinkingEnd => {}
                    StreamEvent::ThinkingSignatureDelta(signature) => {
                        if store_reasoning_content {
                            reasoning_signature.push_str(&signature);
                        }
                    }
                    StreamEvent::ThinkingDelta(thinking_text) => {
                        // Always stream reasoning to clients. Whether to *render*
                        // it is a per-client presentation choice (the TUI keys off
                        // `display.reasoning_display`, the desktops off their own
                        // mode); gating it here would let one shared daemon config
                        // decide what every attached client is allowed to see.
                        if !thinking_text.is_empty() {
                            reasoning_open = true;
                            let _ = event_tx.send(ServerEvent::ReasoningDelta {
                                text: thinking_text.clone(),
                            });
                        } else if hidden_activity_last.elapsed() >= std::time::Duration::from_secs(5) {
                            // An empty delta carries provider activity but no
                            // event, so a long silent thinking phase would look
                            // identical to a dead connection and the client stall
                            // guard would cancel a healthy stream (issue #451).
                            hidden_activity_last = Instant::now();
                            send_stream_keepalive_mpsc(&event_tx);
                        }
                        // Always capture reasoning text so it can be persisted as a
                        // history-only trace, regardless of provider replay support.
                        reasoning_content.push_str(&thinking_text);
                    }
                    StreamEvent::ThinkingDone { duration_secs } => {
                        if reasoning_open {
                            reasoning_open = false;
                            let _ = event_tx.send(ServerEvent::ReasoningDone {
                                duration_secs: Some(duration_secs),
                            });
                        }
                    }
                    StreamEvent::TextDone => {
                        let _ = event_tx.send(ServerEvent::TextDone);
                    }
                    StreamEvent::TextSuffixReplace { old_text, new_text } => {
                        if provider_native_items.replace_text_suffix(&mut text_content, &old_text, &new_text) {
                            if inline_output_tap {
                                self.inline_tail.set_live(&text_content);
                                self.publish_inline_tail();
                            }
                            let _ = event_tx.send(ServerEvent::TextSuffixReplace { old_text, new_text });
                        }
                    }
                    StreamEvent::ProviderNative { provider, item } => {
                        if reasoning_open {
                            reasoning_open = false;
                            let _ = event_tx.send(ServerEvent::ReasoningDone { duration_secs: None });
                        }
                        // Render as a regular tool row: start, input, exec, done.
                        use horon_agent_message_types::provider_native::ProviderNativeToolEvent;
                        let already_open =
                            horon_agent_message_types::provider_native::provider_native_display(&provider, &item)
                                .is_some_and(|display| provider_native_tracker.is_started(&display.id));
                        match provider_native_tracker.observe(&provider, &item) {
                            Some(ProviderNativeToolEvent::Started { id, name, input }) => {
                                let _ = event_tx.send(ServerEvent::ToolStart {
                                    id: id.clone(),
                                    name: name.clone(),
                                });
                                let _ = event_tx.send(ServerEvent::ToolInput {
                                    id: Some(id.clone()),
                                    delta: input.to_string(),
                                });
                                let _ = event_tx.send(ServerEvent::ToolExec { id, name });
                            }
                            Some(ProviderNativeToolEvent::Completed {
                                id,
                                name,
                                input,
                                output,
                                is_error,
                            }) => {
                                if !already_open {
                                    // Result without a start (e.g. OpenAI items
                                    // arrive whole): open the row first.
                                    let _ = event_tx.send(ServerEvent::ToolStart {
                                        id: id.clone(),
                                        name: name.clone(),
                                    });
                                    let _ = event_tx.send(ServerEvent::ToolInput {
                                        id: Some(id.clone()),
                                        delta: input.to_string(),
                                    });
                                    let _ = event_tx.send(ServerEvent::ToolExec {
                                        id: id.clone(),
                                        name: name.clone(),
                                    });
                                }
                                let _ = event_tx.send(ServerEvent::ToolDone {
                                    id,
                                    name,
                                    output,
                                    error: is_error.then(|| "Provider tool error".to_string()),
                                });
                            }
                            None => {}
                        }
                        if let Some(display) =
                            horon_agent_message_types::provider_native::provider_native_display(&provider, &item)
                        {
                            tool_id_to_name.insert(display.id, display.name);
                        }
                        provider_native_items.push(text_content.len(), provider, item);
                    }
                    StreamEvent::TextDelta(text) => {
                        // Close any open reasoning region before real output so the
                        // answer renders as a normal paragraph rather than as reasoning.
                        if reasoning_open && !text.trim().is_empty() {
                            reasoning_open = false;
                            let _ = event_tx.send(ServerEvent::ReasoningDone { duration_secs: None });
                        }
                        text_content.push_str(&text);
                        if inline_output_tap {
                            self.inline_tail.set_live(&text_content);
                            if inline_tap_last.elapsed() >= std::time::Duration::from_millis(200) {
                                inline_tap_last = Instant::now();
                                self.publish_inline_tail();
                            }
                        }
                        // A matching rule aborts this attempt before any of it is
                        // committed: the body goes in as a system reminder and the
                        // request is re-issued from the same conversation state.
                        // Only assistant text reaches the matcher; tool arguments
                        // arrive as separate stream events.
                        if let Some(action) = stream_rule_state.check(&text_content) {
                            match action {
                                StreamRuleAction::Retry(hit) => {
                                    logging::warn(&format!(
                                        "Mid-stream rule '{}' matched; aborting the response and retrying after injection {}/{}",
                                        hit.id,
                                        stream_rule_state.injections() + 1,
                                        MAX_INJECTIONS
                                    ));
                                    if inline_output_tap {
                                        self.inline_tail.clear_live();
                                    }
                                    stream_rule_state.record_injection(&hit);
                                    stream_rule_state.flush_ledger(&mut self.session.stream_rule_ledger);
                                    let reminder = super::stream_rules::reminder(&hit);
                                    self.add_message_with_display_role(
                                        Role::User,
                                        vec![ContentBlock::Text {
                                            text: reminder.clone(),
                                            cache_control: None,
                                        }],
                                        Some(StoredDisplayRole::System),
                                    );
                                    self.save_session()?;
                                    let _ = event_tx.send(ServerEvent::RetryRollback {
                                        attempt: stream_rule_state.injections(),
                                        max: MAX_INJECTIONS,
                                    });
                                    let _ = event_tx.send(ServerEvent::SoftInterruptInjected {
                                        content: reminder,
                                        display_role: Some("system".to_string()),
                                        point: STREAM_RULE_POINT.to_string(),
                                        tools_skipped: None,
                                        client_ids: Vec::new(),
                                    });
                                    retry_after_stream_rule = true;
                                    break;
                                }
                                StreamRuleAction::Capped(hit) => {
                                    logging::warn(&format!(
                                        "Mid-stream rule '{}' matched with the injection cap spent; stopping the turn",
                                        hit.id
                                    ));
                                    if inline_output_tap {
                                        self.inline_tail.clear_live();
                                    }
                                    let notice = super::stream_rules::cap_notice(&hit);
                                    self.record_stream_rule_stop(&notice);
                                    self.save_session()?;
                                    let _ = event_tx.send(ServerEvent::TurnStopped {
                                        reason: crate::protocol::TurnStopReason::LimitReached,
                                        message: notice,
                                        provider_stop_reason: None,
                                    });
                                    stream_rule_stop = true;
                                    break;
                                }
                            }
                        }
                        if !text_wrapped_detected {
                            // Scan only the new delta (plus a short overlap for
                            // markers straddling the boundary) instead of the
                            // whole accumulated response on every token.
                            if let Some(marker_idx) = find_wrap_marker_incremental(&text_content, text.len()) {
                                text_wrapped_detected = true;
                                let clean_prefix = text_content[..marker_idx].trim_end().to_string();
                                let _ = event_tx.send(ServerEvent::TextReplace { text: clean_prefix });
                            } else {
                                let _ = event_tx.send(ServerEvent::TextDelta { text: text.clone() });
                            }
                        }
                        if self.is_graceful_shutdown() {
                            logging::info("Graceful shutdown during streaming - checkpointing partial response");
                            let _ = event_tx.send(ServerEvent::TextDelta {
                                text: "\n\n[generation interrupted - server reloading]".to_string(),
                            });
                            text_content.push_str("\n\n[generation interrupted - server reloading]");
                            break;
                        }
                    }
                    StreamEvent::ToolUseStart { id, name } => {
                        if streaming_tools.contains_key(&id) || tool_calls.iter().any(|tool: &ToolCall| tool.id == id) {
                            continue;
                        }
                        if reasoning_open {
                            reasoning_open = false;
                            let _ = event_tx.send(ServerEvent::ReasoningDone { duration_secs: None });
                        }
                        let _ = event_tx.send(ServerEvent::ToolStart {
                            id: id.clone(),
                            name: name.clone(),
                        });
                        tool_id_to_name.insert(id.clone(), name.clone());
                        current_tool = Some(id.clone());
                        streaming_tools.entry(id.clone()).or_insert_with(|| {
                            (
                                ToolCall {
                                    id,
                                    name,
                                    input: serde_json::Value::Null,
                                    intent: None,
                                    thought_signature: None,
                                },
                                String::new(),
                            )
                        });
                    }
                    StreamEvent::ToolInputDelta(delta) | StreamEvent::ToolInputDeltaFor { delta, .. } => {
                        let _ = event_tx.send(ServerEvent::ToolInput {
                            id: input_tool_id.clone(),
                            delta: delta.clone(),
                        });
                        if let Some((_, input)) = input_tool_id.as_ref().and_then(|id| streaming_tools.get_mut(id)) {
                            input.push_str(&delta);
                        }
                    }
                    StreamEvent::ToolUseEnd | StreamEvent::ToolUseEndFor { .. } => {
                        if let Some((mut tool, current_tool_input)) =
                            input_tool_id.as_ref().and_then(|id| streaming_tools.remove(id))
                        {
                            tool.input = ToolCall::parse_streamed_input_to_object(&current_tool_input);
                            tool.refresh_intent_from_input();

                            let _ = event_tx.send(ServerEvent::ToolExec {
                                id: tool.id.clone(),
                                name: tool.name.clone(),
                            });

                            tool_calls.push(tool);
                            if current_tool == input_tool_id {
                                current_tool = None;
                            }
                        }
                    }
                    StreamEvent::ToolUseSignatureFor { id, signature } => {
                        if !signature.is_empty() {
                            if let Some((tool, _)) = streaming_tools.get_mut(&id) {
                                tool.thought_signature = Some(signature);
                            } else if let Some(tool) = tool_calls.iter_mut().find(|tool| tool.id == id) {
                                tool.thought_signature = Some(signature);
                            }
                        }
                    }
                    StreamEvent::ToolUseSignature(signature) => {
                        // Attach Gemini 3 thought signature to the most recent
                        // tool call so it can be persisted and replayed.
                        if let Some(tool) = tool_calls.last_mut()
                            && !signature.is_empty()
                        {
                            tool.thought_signature = Some(signature);
                        }
                    }
                    StreamEvent::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let tool_name = tool_id_to_name.get(&tool_use_id).cloned().unwrap_or_default();
                        let _ = event_tx.send(ServerEvent::ToolDone {
                            id: tool_use_id.clone(),
                            name: tool_name,
                            output: content.clone(),
                            error: if is_error { Some("Tool error".to_string()) } else { None },
                        });
                        sdk_tool_results.insert(tool_use_id, (content, is_error));
                    }
                    StreamEvent::GeneratedImage {
                        id,
                        path,
                        metadata_path,
                        output_format,
                        revised_prompt,
                    } => {
                        let rendered_image = crate::message::generated_image_rendered_image(&id, &path, &output_format);
                        if self.provider.supports_image_input() {
                            if let Some(blocks) = crate::message::generated_image_visual_context_blocks(
                                &path,
                                metadata_path.as_deref(),
                                &output_format,
                                revised_prompt.as_deref(),
                            ) {
                                generated_image_contexts.push(blocks);
                            } else {
                                crate::logging::warn(&format!(
                                    "Generated image was not attached as visual context: {}",
                                    path
                                ));
                            }
                        }
                        let _ = event_tx.send(ServerEvent::GeneratedImage {
                            id,
                            path,
                            metadata_path,
                            output_format,
                            revised_prompt,
                        });
                        if let Some(image) = rendered_image {
                            let _ = event_tx.send(ServerEvent::SidePaneImages {
                                session_id: self.session.id.clone(),
                                images: vec![image],
                            });
                        }
                    }
                    StreamEvent::TokenUsage {
                        input_tokens,
                        output_tokens,
                        cache_read_input_tokens,
                        cache_creation_input_tokens,
                    } => {
                        if let Some(input) = input_tokens {
                            usage_input = Some(input);
                        }
                        if let Some(output) = output_tokens {
                            usage_output = Some(output);
                        }
                        if cache_read_input_tokens.is_some() {
                            usage_cache_read = cache_read_input_tokens;
                        }
                        if cache_creation_input_tokens.is_some() {
                            usage_cache_creation = cache_creation_input_tokens;
                        }
                        if let Some(input) = usage_input {
                            self.update_compaction_usage_from_stream(input, usage_cache_read, usage_cache_creation);
                        }
                    }
                    StreamEvent::ConnectionType { connection } => {
                        self.last_connection_type = Some(connection.clone());
                        let _ = event_tx.send(ServerEvent::ConnectionType { connection });
                    }
                    StreamEvent::ConnectionPhase { phase } => {
                        let _ = event_tx.send(ServerEvent::ConnectionPhase {
                            phase: phase.to_string(),
                        });
                    }
                    StreamEvent::StatusDetail { detail } => {
                        self.last_status_detail = Some(detail.clone());
                        let _ = event_tx.send(ServerEvent::StatusDetail { detail });
                    }
                    StreamEvent::RetryRollback { attempt, max } => {
                        // A transient transport fault hit mid-stream after partial
                        // output was already emitted; the provider is replaying the
                        // request from the top. Discard everything accumulated for
                        // this attempt so the replay doesn't duplicate output, and
                        // tell the client to do the same.
                        logging::warn(&format!(
                            "Mid-stream retry rollback (attempt {}/{}): discarding partial output ({} text chars, {} tool calls)",
                            attempt,
                            max,
                            text_content.len(),
                            tool_calls.len(),
                        ));
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Warn,
                            self,
                            "retry_rollback",
                            api_start,
                            vec![
                                ("mode", "mpsc".to_string()),
                                ("attempt", attempt.to_string()),
                                ("max", max.to_string()),
                                ("text_chars", text_content.len().to_string()),
                                ("tool_calls", tool_calls.len().to_string()),
                            ],
                        );
                        text_content.clear();
                        if inline_output_tap {
                            // The provider replays from the top; drop the
                            // discarded partial from the live tail too.
                            self.inline_tail.clear_live();
                        }
                        text_wrapped_detected = false;
                        tool_calls.clear();
                        current_tool = None;
                        streaming_tools.clear();
                        tool_id_to_name.clear();
                        sdk_tool_results.clear();
                        generated_image_contexts.clear();
                        reasoning_content.clear();
                        reasoning_signature.clear();
                        reasoning_open = false;
                        openai_reasoning_items.clear();
                        provider_native_items.clear();
                        provider_native_tracker.clear();
                        openai_native_compaction = None;
                        saw_message_end = false;
                        stop_reason = None;
                        let _ = event_tx.send(ServerEvent::RetryRollback { attempt, max });
                        let _ = event_tx.send(ServerEvent::ConnectionPhase {
                            phase: crate::message::ConnectionPhase::Retrying { attempt, max }.to_string(),
                        });
                    }
                    StreamEvent::MessageEnd { stop_reason: reason } => {
                        saw_message_end = true;
                        if inline_output_tap {
                            // Fold the finished text into the rolling tail so
                            // it survives the next turn/continuation.
                            self.inline_tail.set_live(&text_content);
                            self.inline_tail.commit_live();
                        }
                        // Close any still-open reasoning region (e.g. a reasoning-only
                        // step) so the client flushes its live partial line.
                        if reasoning_open {
                            reasoning_open = false;
                            let _ = event_tx.send(ServerEvent::ReasoningDone { duration_secs: None });
                        }
                        if reason.is_some() {
                            stop_reason = reason;
                        }
                        let _ = event_tx.send(ServerEvent::MessageEnd {
                            stop_reason: stop_reason.clone(),
                        });
                    }
                    StreamEvent::SessionId(sid) => {
                        // This is the *provider's* session id (Gemini/Claude
                        // CLI/Grok resume handle). It must never be forwarded
                        // as `ServerEvent::SessionId`: the client treats that
                        // event as the horon-agent session id and rebinds
                        // `remote_session_id` to it, so the next reload or
                        // reconnect resumes a session that does not exist and
                        // the user lands in an empty new session while the
                        // real transcript sits untouched on disk.
                        self.provider_session_id = Some(sid.clone());
                        self.session.provider_session_id = Some(sid);
                    }
                    StreamEvent::OpenAIReasoning {
                        id,
                        summary,
                        encrypted_content,
                        status,
                    } => {
                        if store_reasoning_content {
                            openai_reasoning_items.push(ContentBlock::OpenAIReasoning {
                                id,
                                summary,
                                encrypted_content,
                                status,
                            });
                        }
                    }
                    StreamEvent::Compaction {
                        pre_tokens,
                        openai_encrypted_content,
                        ..
                    } => {
                        if let Some(encrypted_content) = openai_encrypted_content {
                            // The auto path emits one compaction item per
                            // response; it becomes the first item of a window.
                            openai_native_compaction.get_or_insert((
                                vec![serde_json::json!({
                                    "type": "compaction",
                                    "encrypted_content": encrypted_content,
                                })],
                                self.session.active_context().len(),
                                pre_tokens,
                            ));
                        }
                    }
                    StreamEvent::NativeToolCall {
                        request_id,
                        tool_name,
                        input,
                    } => {
                        // Execute native tool and send result back to SDK bridge
                        let ctx = ToolContext {
                            session_id: self.session.id.clone(),
                            message_id: self.session.id.clone(),
                            tool_call_id: request_id.clone(),
                            working_dir: self.working_dir().map(PathBuf::from),
                            stdin_request_tx: self.stdin_request_tx.clone(),
                            graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
                            execution_mode: ToolExecutionMode::AgentTurn,
                        };
                        let tool_result = self
                            .registry
                            .execute(&tool_name, ToolCall::normalize_input_to_object(input), ctx)
                            .await;
                        let native_result = match tool_result {
                            Ok(output) => NativeToolResult::success(request_id, output.output),
                            Err(e) => NativeToolResult::error(request_id, e.to_string()),
                        };
                        if let Some(sender) = self.provider.native_result_sender() {
                            // The provider can end its stream before the tool completes.
                            // Keep the completed tool result in history, but expose the delivery failure.
                            if let Err(error) = sender.send(native_result).await {
                                logging::warn(&format!("Could not deliver native tool result to provider: {error}"));
                            }
                        }
                    }
                    StreamEvent::UpstreamProvider { provider } => {
                        self.last_upstream_provider = Some(provider.clone());
                        let _ = event_tx.send(ServerEvent::UpstreamProvider { provider });
                    }
                    StreamEvent::Error {
                        message,
                        retry_after_secs,
                    } => {
                        let stream_error: anyhow::Error = StreamError::new(message.clone(), retry_after_secs).into();
                        let attached_client = crate::server::session_has_attached_client(&self.session.id);
                        match self.handle_provider_error(&stream_error, attached_client).await {
                            super::turn_failover::ProviderErrorAction::Resend { attempt, max } => {
                                if !text_content.is_empty() || !tool_calls.is_empty() {
                                    let _ = event_tx.send(ServerEvent::RetryRollback { attempt, max });
                                }
                                retry_after_recovery = true;
                                break;
                            }
                            super::turn_failover::ProviderErrorAction::Stop(error) => return Err(error),
                            super::turn_failover::ProviderErrorAction::Proceed => {}
                        }
                        if self.try_auto_compact_after_context_limit(&message) {
                            log_agent_provider_stream_lifecycle(
                                logging::LogLevel::Warn,
                                self,
                                "stream_event_retry_after_compaction",
                                api_start,
                                vec![
                                    ("mode", "mpsc".to_string()),
                                    ("error", message.clone()),
                                    (
                                        "context_limit_retries",
                                        (self.turn_state.context_limit_retries() + 1).to_string(),
                                    ),
                                ],
                            );
                            let retries = self.turn_state.record_context_limit_retry();
                            if retries > Self::MAX_CONTEXT_LIMIT_RETRIES {
                                logging::warn("Context-limit compaction retry limit reached; giving up");
                                let _ = event_tx.send(ServerEvent::TurnStopped {
                                    reason: crate::protocol::TurnStopReason::LimitReached,
                                    message: format!(
                                        "Context limit exceeded after {} compaction retries",
                                        Self::MAX_CONTEXT_LIMIT_RETRIES
                                    ),
                                    provider_stop_reason: None,
                                });
                                return Err(anyhow::anyhow!(
                                    "Context limit exceeded after {} compaction retries",
                                    Self::MAX_CONTEXT_LIMIT_RETRIES
                                ));
                            }
                            retry_after_compaction = true;
                            let _ = event_tx.send(ServerEvent::Compaction {
                                trigger: "auto_recovery".to_string(),
                                pre_tokens: None,
                                post_tokens: None,
                                tokens_saved: None,
                                duration_ms: None,
                                messages_dropped: None,
                                messages_compacted: None,
                                summary_chars: None,
                                active_messages: None,
                            });
                            break;
                        }
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Error,
                            self,
                            "stream_event_error",
                            api_start,
                            vec![
                                ("mode", "mpsc".to_string()),
                                ("error", message.clone()),
                                (
                                    "retry_after_secs",
                                    retry_after_secs
                                        .map(|seconds| seconds.to_string())
                                        .unwrap_or_else(|| "none".to_string()),
                                ),
                            ],
                        );
                        return Err(StreamError::new(message, retry_after_secs).into());
                    }
                }
            }

            // A rule abort keeps nothing from the aborted attempt: the partial
            // text, tool calls and reasoning are dropped, the reminder is already
            // in the transcript, and the next iteration re-issues the request from
            // the same conversation state. At the cap the turn ends instead.
            if stream_rule_stop {
                break;
            }
            if retry_after_stream_rule {
                logging::info("Retrying the turn after a mid-stream rule injection");
                continue;
            }

            if retry_after_recovery {
                logging::info("Retrying the turn after a provider recovery - resending the request");
                continue;
            }

            if retry_after_compaction {
                log_agent_provider_stream_lifecycle(
                    logging::LogLevel::Info,
                    self,
                    "retry_after_compaction",
                    api_start,
                    vec![("mode", "mpsc".to_string())],
                );
                continue;
            }

            let api_elapsed = api_start.elapsed();
            logging::info(&format!(
                "API call complete in {:.2}s (input={} output={} cache_read={} cache_write={})",
                api_elapsed.as_secs_f64(),
                usage_input.unwrap_or(0),
                usage_output.unwrap_or(0),
                usage_cache_read.unwrap_or(0),
                usage_cache_creation.unwrap_or(0),
            ));
            log_agent_provider_stream_lifecycle(
                logging::LogLevel::Info,
                self,
                "stream_complete",
                api_start,
                vec![
                    ("mode", "mpsc".to_string()),
                    ("saw_message_end", saw_message_end.to_string()),
                    ("stop_reason", stop_reason.clone().unwrap_or_else(|| "none".to_string())),
                    ("input_tokens", usage_input.unwrap_or(0).to_string()),
                    ("output_tokens", usage_output.unwrap_or(0).to_string()),
                    ("cache_read", usage_cache_read.unwrap_or(0).to_string()),
                    ("cache_write", usage_cache_creation.unwrap_or(0).to_string()),
                ],
            );

            if usage_input.is_some()
                || usage_output.is_some()
                || usage_cache_read.is_some()
                || usage_cache_creation.is_some()
            {
                let input = usage_input.unwrap_or(0);
                let output = usage_output.unwrap_or(0);
                let total = self
                    .effective_context_tokens_from_usage(input, usage_cache_read, usage_cache_creation)
                    .saturating_add(output);
                crate::session_metrics::record_token_usage(&self.session.id, total, output);
            }

            if usage_input.is_some()
                || usage_output.is_some()
                || usage_cache_read.is_some()
                || usage_cache_creation.is_some()
            {
                let _ = event_tx.send(ServerEvent::TokenUsage {
                    input: usage_input.unwrap_or(0),
                    output: usage_output.unwrap_or(0),
                    cache_read_input: usage_cache_read,
                    cache_creation_input: usage_cache_creation,
                });
                if let Some(miss) = self.finish_kv_cache_monitor_request(
                    usage_input.unwrap_or(0),
                    usage_cache_read,
                    usage_cache_creation,
                ) {
                    let _ = event_tx.send(miss);
                }
            }

            // Store usage for debug queries
            self.last_usage = TokenUsage {
                input_tokens: usage_input.unwrap_or(0),
                output_tokens: usage_output.unwrap_or(0),
                cache_read_input_tokens: usage_cache_read,
                cache_creation_input_tokens: usage_cache_creation,
            };

            // Detect a transparent mid-request model switch (e.g. Anthropic's
            // retired `claude-fable-5` falling back to `claude-opus-4-8`). The
            // provider mutates its own model state during the stream, so the
            // session and clients would otherwise keep showing the originally
            // requested model with a stale context-limit. Resync the session and
            // notify clients with a `ModelChanged` so the header, picker, and
            // context budget all reflect the model that actually served.
            let model_after_stream = self.provider.model();
            if model_after_stream != model_at_request_start {
                logging::warn(&format!(
                    "Provider switched model mid-request: '{}' -> '{}' (resyncing session/UI)",
                    model_at_request_start, model_after_stream
                ));
                self.session.model = Some(self.provider_model());
                self.provider_runtime_state
                    .apply(crate::provider::ProviderStateEvent::RuntimeModelObserved {
                        model: model_after_stream.clone(),
                    });
                self.persist_session_best_effort("model fallback");
            }
            // Refresh authoritative credential identity after every upstream
            // request, including auto credential resolution and OAuth failover.
            let credential_after_stream = self.provider.active_resolved_credential();
            if model_after_stream != model_at_request_start || credential_after_stream != credential_at_request_start {
                let _ = event_tx.send(ServerEvent::ModelChanged {
                    id: 0,
                    model: model_after_stream,
                    provider_name: Some(self.provider.display_name()),
                    context_window: Some(self.provider.context_window() as u64),
                    error: None,
                    resolved_credential: credential_after_stream,
                    reasoning_effort: self.provider.reasoning_effort(),
                });
            }

            let had_tool_calls_before = !tool_calls.is_empty();
            let text_tool_recovery = self.recover_text_wrapped_tool_call(&mut text_content, &mut tool_calls);

            // Passive text rules never abort the response: their reminders
            // enter this turn's reminder slot, so the next request carries them
            // and the model can correct itself on the following round.
            for hit in stream_rule_state.passive_text_hits(&text_content) {
                let reminder = super::stream_rules::text_passive_reminder(&hit);
                Self::merge_system_reminder(&mut self.current_turn_system_reminder, &reminder);
            }
            stream_rule_state.flush_ledger(&mut self.session.stream_rule_ledger);

            if !had_tool_calls_before && text_tool_recovery == TextToolRecovery::Recovered {
                let _ = event_tx.send(ServerEvent::TextReplace {
                    text: text_content.clone(),
                });
                // Markup envelopes can carry several calls, so announce each.
                for tc in tool_calls.iter().filter(|tc| tc.id.starts_with("fallback_text_call_")) {
                    let _ = event_tx.send(ServerEvent::ToolStart {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                    });
                    tool_id_to_name.insert(tc.id.clone(), tc.name.clone());
                    let _ = event_tx.send(ServerEvent::ToolInput {
                        id: Some(tc.id.clone()),
                        delta: tc.input.to_string(),
                    });
                    let _ = event_tx.send(ServerEvent::ToolExec {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                    });
                }
            }

            // Add assistant message to history
            let mut content_blocks = Vec::new();
            if !provider_native_items.is_empty() {
                content_blocks.extend(provider_native_items.interleave(&text_content));
            } else if !text_content.is_empty() {
                content_blocks.push(ContentBlock::Text {
                    text: text_content.clone(),
                    cache_control: None,
                });
            }
            crate::message::push_reasoning_blocks(
                &mut content_blocks,
                &provider_name,
                &reasoning_content,
                Some(&reasoning_signature),
                store_reasoning_content,
            );
            if store_reasoning_content {
                content_blocks.extend(openai_reasoning_items.iter().cloned());
            }
            for tc in &tool_calls {
                content_blocks.push(tc.to_tool_use_block());
            }

            let assistant_message_id = if !content_blocks.is_empty() {
                let token_usage = Some(crate::session::StoredTokenUsage {
                    prompt_tokens: Some(self.effective_context_tokens_from_usage(
                        self.last_usage.input_tokens,
                        self.last_usage.cache_read_input_tokens,
                        self.last_usage.cache_creation_input_tokens,
                    )),
                    input_tokens: self.last_usage.input_tokens,
                    output_tokens: self.last_usage.output_tokens,
                    cache_read_input_tokens: self.last_usage.cache_read_input_tokens,
                    cache_creation_input_tokens: self.last_usage.cache_creation_input_tokens,
                });
                let message_id = self.add_message_ext(Role::Assistant, content_blocks, None, token_usage);
                self.push_embedding_snapshot_if_semantic(&text_content);
                self.save_session()?;
                self.record_model_turn_usage(&usage_turn_id);
                Some(message_id)
            } else {
                None
            };

            if let Some((window, compacted_count, native_pre_tokens)) = openai_native_compaction.take() {
                // A compaction item that arrived mid-stream belongs to a
                // response the provider never finished. Applying it would move
                // the compacted boundary over that response, so the originals
                // stay active until a completed response produces a window.
                if !saw_message_end {
                    logging::warn(
                        "[OI] native compaction item arrived on an unfinished stream; keeping the compacted boundary unchanged",
                    );
                } else {
                    self.apply_openai_native_compaction_window(window, compacted_count)?;
                    // Native OpenAI compaction is applied after the provider stream,
                    // so `messages_for_provider()` did not have an event to emit at
                    // the top of this iteration. Notify clients now, before any
                    // tool-driven continuation can enqueue its next KvCacheRequest.
                    // The FIFO event ordering lets the TUI invalidate its old
                    // append-only baseline before seeing the compacted signature.
                    //
                    // Only a provider-supplied pre-compaction count is trustworthy
                    // here. The response's own input usage is not the pre-compaction
                    // context size, so omit the value rather than mislabel it (#1178).
                    let _ = event_tx.send(ServerEvent::Compaction {
                        trigger: "openai_native".to_string(),
                        pre_tokens: native_pre_tokens,
                        post_tokens: None,
                        tokens_saved: None,
                        duration_ms: None,
                        messages_dropped: None,
                        messages_compacted: Some(compacted_count),
                        summary_chars: None,
                        active_messages: None,
                    });
                }
            }

            // If stop_reason indicates truncation (e.g. max_tokens), discard tool calls
            // with null/empty inputs since they were likely truncated mid-generation.
            self.filter_truncated_tool_calls(stop_reason.as_deref(), &mut tool_calls, assistant_message_id.as_ref());

            if tool_calls.is_empty() && !generated_image_contexts.is_empty() {
                for blocks in generated_image_contexts.drain(..) {
                    self.add_message(Role::User, blocks);
                }
                self.save_session()?;
                logging::info("Continuing turn so model can inspect generated image visual context");
                continue;
            }

            // If no tool calls, check for soft interrupt or exit
            // NOTE: We only inject here (Point B) when there are no tools.
            // Injecting before tool_results would break the API requirement that
            // tool_use must be immediately followed by tool_result.
            if tool_calls.is_empty() {
                // The model printed a tool call as markup we could not parse
                // (issue #1702): say so visibly and ask it to re-issue the
                // call natively instead of silently ending the turn.
                if saw_message_end
                    && !self.is_graceful_shutdown()
                    && let Some(notice) =
                        self.maybe_nudge_unparsed_tool_markup(&text_tool_recovery, stop_reason.as_deref())?
                {
                    let _ = event_tx.send(ServerEvent::SoftInterruptInjected {
                        content: notice,
                        display_role: Some("system".to_string()),
                        point: "B".to_string(),
                        tools_skipped: None,
                        client_ids: Vec::new(),
                    });
                    continue;
                }
                if saw_message_end
                    && !self.is_graceful_shutdown()
                    && self.maybe_reconsider_fable_guardrail(stop_reason.as_deref())?
                {
                    continue;
                }
                // Retry transient empty responses (dropped/empty upstream
                // streams) before surfacing anything, matching the
                // non-streaming loop's recovery behavior (issue #672).
                if saw_message_end
                    && !self.is_graceful_shutdown()
                    && self.maybe_continue_empty_post_tool_response(
                        text_content.trim().is_empty(),
                        prompt_has_recent_tool_result,
                        stop_reason.as_deref(),
                    )?
                {
                    continue;
                }
                match self
                    .handle_streaming_no_tool_calls(
                        stop_reason.as_deref(),
                        &text_content,
                        saw_message_end,
                        super::interrupts::reborrow_in_flight(&mut in_flight),
                    )
                    .await?
                {
                    NoToolCallOutcome::Break => {
                        if saw_message_end
                            && !self.is_graceful_shutdown()
                            && let Some(event) = incomplete_turn_stop(stop_reason.as_deref())
                        {
                            let _ = event_tx.send(event);
                        }
                        // Surface silent guardrail/refusal stops: the provider
                        // ended the turn with no visible output (e.g. Anthropic
                        // stop_reason "refusal", or a reasoning-only response).
                        // Only when the provider actually finished the message
                        // (saw_message_end) and the user did not cancel, so
                        // interrupted turns never show a spurious notice.
                        if saw_message_end
                            && !self.is_graceful_shutdown()
                            && let Some(notice) = Self::provider_guardrail_notice(
                                stop_reason.as_deref(),
                                text_content.trim().is_empty(),
                                !reasoning_content.trim().is_empty(),
                            )
                        {
                            logging::warn(&format!(
                                "{}: turn ended with no visible output (stop_reason={:?}, reasoning_chars={})",
                                Self::empty_turn_log_event(stop_reason.as_deref()),
                                stop_reason,
                                reasoning_content.len()
                            ));
                            let _ = event_tx.send(ServerEvent::ProviderGuardrail {
                                stop_reason: stop_reason.clone(),
                                message: notice,
                            });
                        }
                        self.turn_state.set_final_text(&text_content);
                        let outcome = if self.is_graceful_shutdown() {
                            TurnOutcome::Aborted
                        } else {
                            outcome_for_stop_reason(stop_reason.as_deref())
                        };
                        self.turn_state.finish(outcome)?;
                        break;
                    }
                    NoToolCallOutcome::BreakAfterRecoveryExhausted { notice } => {
                        let _ = event_tx.send(ServerEvent::TurnStopped {
                            reason: crate::protocol::TurnStopReason::LimitReached,
                            message: notice.clone(),
                            provider_stop_reason: stop_reason.clone(),
                        });
                        let final_text = if text_content.trim().is_empty() {
                            notice
                        } else {
                            format!("{}\n\n{}", text_content, notice)
                        };
                        self.turn_state.set_final_text(&final_text);
                        let outcome = if self.is_graceful_shutdown() {
                            TurnOutcome::Aborted
                        } else {
                            outcome_for_stop_reason(stop_reason.as_deref())
                        };
                        self.turn_state.finish(outcome)?;
                        break;
                    }
                    NoToolCallOutcome::ContinueWithoutEvent => continue,
                    NoToolCallOutcome::ContinueWithSoftInterrupt { injected, point } => {
                        for event in Self::build_soft_interrupt_events(injected, point, None) {
                            let _ = event_tx.send(event);
                        }
                        continue;
                    }
                }
            }

            // If graceful shutdown was signaled during streaming and we have tool calls,
            // we need to provide tool results for them (API requires tool_use -> tool_result)
            // then exit cleanly
            if self.is_graceful_shutdown() {
                logging::info(&format!(
                    "Graceful shutdown - skipping {} tool call(s)",
                    tool_calls.len()
                ));
                for tc in &tool_calls {
                    self.add_message(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: "[Skipped - server reloading]".to_string(),
                            is_error: Some(true),
                        }],
                    );
                }
                self.save_session()?;
                break;
            }

            logging::info(&format!("Turn has {} tool calls to execute", tool_calls.len()));

            if self.provider.handles_tools_internally() {
                tool_calls.retain(|tc| HORON_AGENT_NATIVE_TOOLS.contains(&tc.name.as_str()));
                if tool_calls.is_empty() {
                    // === INJECTION POINT D: After provider-handled tools, before next API call ===
                    let injected = self
                        .inject_soft_interrupts(super::interrupts::reborrow_in_flight(&mut in_flight))
                        .await;
                    if !injected.is_empty() {
                        for event in Self::build_soft_interrupt_events(injected, "D", None) {
                            let _ = event_tx.send(event);
                        }
                        // Don't break - continue loop to process injected message
                        continue;
                    }
                    break;
                }
            }

            // Execute tools and add results.
            //
            // The round is planned once through the shared scheduler
            // (`agent::tool_round`), so grouping and the group caps live in one
            // place and cannot drift from the blocking loop. Two execution
            // paths stay deliberately distinct:
            //
            // * a one-member group (every `Ordered` call, and every call when
            //   `parallel_tools` is off) runs inline exactly as it always has,
            //   so Alt+B can still detach it: `running_tool_registry` holds ONE
            //   entry per session and the background signal carries no target
            //   identity, so only a single awaited call can be named.
            // * a multi-member group runs through `run_group` and registers no
            //   entry. A background request during the group therefore reports
            //   `moved: false`; the signal is reset afterwards so it cannot
            //   move the next one-member call. The group also watches for an
            //   urgent interrupt while its members are in flight, so the
            //   daemon still reacts per call rather than per group.
            let mut malformed_calls: Vec<MalformedToolCallInfo> = Vec::new();
            let mut executed_valid_call = false;
            let mut tool_results_dirty = false;
            let parallel_tools = crate::config::config().features.parallel_tools;
            let groups = self.plan_tool_round_groups(&tool_calls, parallel_tools).await;
            // Seeded from the transcript as it stands before this round's
            // results land, which is the view the tool layer's own guard has.
            let round_messages = self.session.messages_for_provider_ref();
            // One budget for the whole round: seeded once from THIS agent's
            // compaction manager, advanced by every output the round applies,
            // and shared across the groups below. Re-seeding it per group would
            // let a second group spend a window the first already filled.
            let mut round_budget =
                super::round_budget::RoundTokenBudget::seed(&self.registry.compaction(), round_messages).await;
            for group in groups {
                // === INJECTION POINT C (before): Check for urgent abort before each group (except first) ===
                if group.start > 0 && self.has_urgent_interrupt() {
                    let skipped = tool_calls.len() - group.start;
                    self.finish_streamed_round_interrupt(
                        &tool_calls,
                        group.start,
                        skipped,
                        &event_tx,
                        super::interrupts::reborrow_in_flight(&mut in_flight),
                    )
                    .await;
                    break; // Skip remaining tools
                }

                if group.len() > 1 {
                    // The group runs as ONE loop-guard dispatch decision, made
                    // inside `run_streamed_tool_group` before any member starts.
                    // The same call also carries the round's own context-budget
                    // counter, which is spent at result-application time.
                    let outcome = self
                        .run_streamed_tool_group(
                            &tool_calls,
                            group,
                            assistant_message_id.as_deref(),
                            &event_tx,
                            &mut sdk_tool_results,
                            &mut malformed_calls,
                            &mut executed_valid_call,
                            &mut tool_results_dirty,
                            &mut round_budget,
                            &mut stream_rule_state,
                            trace,
                            inline_output_tap,
                        )
                        .await?;
                    match outcome {
                        StreamedGroupOutcome::GuardStop(notice) => {
                            self.finish_loop_guard_stop(&tool_calls, group.end, &notice, &event_tx)?;
                            return Ok(());
                        }
                        StreamedGroupOutcome::Interrupted { skipped } => {
                            // The group already applied its own members: the
                            // ones that finished kept their results and the
                            // rest carry a skip. Every later call of the round
                            // still owes one, so the next request is formed
                            // from a valid transcript.
                            self.finish_streamed_round_interrupt(
                                &tool_calls,
                                group.end,
                                skipped,
                                &event_tx,
                                super::interrupts::reborrow_in_flight(&mut in_flight),
                            )
                            .await;
                            break;
                        }
                        StreamedGroupOutcome::Continue => {}
                    }
                    if self.is_graceful_shutdown() {
                        // The group was interrupted by a server reload: every
                        // later call owes a result too, so the next request is
                        // formed from a valid transcript.
                        for remaining_tc in &tool_calls[group.end..] {
                            self.add_message(
                                Role::User,
                                vec![ContentBlock::ToolResult {
                                    tool_use_id: remaining_tc.id.clone(),
                                    content: RELOAD_SKIP.to_string(),
                                    is_error: Some(true),
                                }],
                            );
                        }
                        self.save_session()?;
                        return Ok(());
                    }
                    continue;
                }

                let tool_index = group.start;
                let tc = &tool_calls[tool_index];

                let message_id = assistant_message_id.clone().unwrap_or_else(|| self.session.id.clone());

                if let Some(error_msg) = tc.validation_error() {
                    malformed_calls.push(MalformedToolCallInfo {
                        name: tc.name.clone(),
                        error: error_msg.clone(),
                    });
                    logging::warn(&error_msg);
                    let _ = event_tx.send(ServerEvent::ToolDone {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        output: error_msg.clone(),
                        error: Some(error_msg.clone()),
                    });
                    self.add_message(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: error_msg,
                            is_error: Some(true),
                        }],
                    );
                    tool_results_dirty = true;
                    continue;
                }

                // A call that passed validation is genuinely valid even if
                // the tool itself later fails: it resets the malformed-round
                // bound in handle_malformed_tool_round below.
                executed_valid_call = true;
                self.validate_tool_allowed(&tc.name)?;

                let is_native_tool = HORON_AGENT_NATIVE_TOOLS.contains(&tc.name.as_str());

                if let Some((sdk_content, sdk_is_error)) = sdk_tool_results.remove(&tc.id) {
                    // For native tools, ignore SDK errors and execute locally
                    if !(is_native_tool && sdk_is_error) {
                        // The provider answered this call, so no tool layer capped
                        // or admitted it. It is still an output this round
                        // applies, so it takes the round's own decision.
                        let sdk_content = round_budget.admit_sdk_result(
                            &self.session.id,
                            &tc.name,
                            &tc.input,
                            sdk_content,
                            sdk_is_error,
                        );
                        self.add_message(
                            Role::User,
                            vec![ContentBlock::ToolResult {
                                tool_use_id: tc.id.clone(),
                                content: sdk_content,
                                is_error: if sdk_is_error { Some(true) } else { None },
                            }],
                        );
                        tool_results_dirty = true;

                        // NOTE: No injection here - wait for Point D after all tools

                        continue;
                    }
                    // Fall through to local execution for native tools with SDK errors
                }

                // One loop-guard decision for this ordered call, taken before
                // it runs. A malformed call and an SDK-answered one never reach
                // a tool, so they are not recorded and not judged here.
                match self.consult_loop_guard(std::iter::once(tc)).pop() {
                    Some(super::loop_guard::LoopGuardVerdict::Allow)
                    | Some(super::loop_guard::LoopGuardVerdict::Warn(_))
                    | None => {}
                    Some(super::loop_guard::LoopGuardVerdict::Refuse(message)) => {
                        self.write_loop_guard_result(tc, &message, &event_tx);
                        tool_results_dirty = true;
                        continue;
                    }
                    Some(super::loop_guard::LoopGuardVerdict::Stop(message)) => {
                        self.write_loop_guard_result(tc, &message, &event_tx);
                        self.finish_loop_guard_stop(&tool_calls, group.end, &message, &event_tx)?;
                        return Ok(());
                    }
                }

                // A tool-scoped rule decides the call before it reaches a tool.
                // An interrupt refuses it outright; a passive hit runs the call
                // and rides along in its result.
                let mut rule_passive: Option<String> = None;
                match stream_rule_state.check_tool(&tc.name, &tc.input) {
                    Some(ToolRuleAction::Block(hit)) => {
                        let message = super::stream_rules::blocked_message(&hit);
                        logging::warn(&format!("Stream rule blocked tool '{}': {message}", tc.name));
                        let _ = event_tx.send(ServerEvent::ToolDone {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            output: message.clone(),
                            error: Some(message.clone()),
                        });
                        self.add_message(
                            Role::User,
                            vec![ContentBlock::ToolResult {
                                tool_use_id: tc.id.clone(),
                                content: message,
                                is_error: Some(true),
                            }],
                        );
                        tool_results_dirty = true;
                        stream_rule_state.flush_ledger(&mut self.session.stream_rule_ledger);
                        continue;
                    }
                    Some(ToolRuleAction::Passive(hit)) => {
                        rule_passive = Some(super::stream_rules::tool_passive_reminder(&hit));
                        stream_rule_state.flush_ledger(&mut self.session.stream_rule_ledger);
                    }
                    None => {}
                }

                let ctx = ToolContext {
                    session_id: self.session.id.clone(),
                    message_id: message_id.clone(),
                    tool_call_id: tc.id.clone(),
                    working_dir: self.working_dir().map(PathBuf::from),
                    stdin_request_tx: self.stdin_request_tx.clone(),
                    graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
                    execution_mode: ToolExecutionMode::AgentTurn,
                };

                if trace {
                    eprintln!("[trace] tool_exec_start name={} id={}", tc.name, tc.id);
                }

                logging::info(&format!("Tool starting: {}", tc.name));
                crate::session_metrics::record_activity(&self.session.id);
                if inline_output_tap {
                    // Surface the tool execution on the coordinator's inline
                    // viewport immediately: workers spend most wall-clock time
                    // here, where no assistant text streams.
                    self.inline_tail.start_tool(&tc.name, &tc.input);
                    self.publish_inline_tail();
                }
                let tool_start = Instant::now();

                // Spawn tool in its own task so we can detach it to background on Alt+B
                // Executing for this session, so the guard must read this
                // session's budget rather than a fresh manager's floor.
                let registry_clone = self.registry.clone_sharing_compaction();
                let tool_name_for_spawn = tc.name.clone();
                let tool_input_for_spawn = tc.input.clone();
                let tool_handle = tokio::spawn(async move {
                    registry_clone
                        .execute(&tool_name_for_spawn, tool_input_for_spawn, ctx)
                        .await
                });

                // Reset background signal before waiting
                self.background_tool_signal.reset();

                // Wait for tool completion OR background signal from user (Alt+B)
                // OR graceful shutdown signal from server reload
                let bg_signal = self.background_tool_signal.clone();
                let shutdown_signal = self.graceful_shutdown.clone();
                let allow_reload_handoff = tc.name == "bash";
                let tool_result;
                let mut tool_handle = tool_handle;
                let running_tool_guard = crate::running_tool_registry::register(self.session_id(), &tc.id, &tc.name);
                tokio::select! {
                    biased;
                    res = &mut tool_handle => {
                        tool_result = Some(match res {
                            Ok(r) => r,
                            Err(e) => Err(anyhow::anyhow!("Tool task panicked: {}", e)),
                        });
                    }
                    _ = async {
                        tokio::select! {
                            _ = bg_signal.notified() => {}
                            _ = shutdown_signal.notified() => {}
                        }
                    } => {
                        if self.is_graceful_shutdown() && allow_reload_handoff {
                            tool_result = match tokio::time::timeout(
                                Duration::from_millis(750),
                                &mut tool_handle,
                            )
                            .await
                            {
                                Ok(res) => Some(match res {
                                    Ok(r) => r,
                                    Err(e) => Err(anyhow::anyhow!("Tool task panicked: {}", e)),
                                }),
                                Err(_) => None,
                            };
                        } else {
                            tool_result = None;
                        }
                    }
                };
                drop(running_tool_guard);

                self.unlock_tools_if_needed(&tc.name, &tc.input);
                let tool_elapsed = tool_start.elapsed();
                crate::session_metrics::record_activity(&self.session.id);

                if let Some(result) = tool_result {
                    // Normal tool completion
                    logging::info(&format!(
                        "Tool finished: {} in {:.2}s",
                        tc.name,
                        tool_elapsed.as_secs_f64()
                    ));
                    if inline_output_tap {
                        // Update the tool marker in place with duration/error.
                        self.inline_tail
                            .finish_tool(tool_elapsed.as_secs_f64(), result.is_err());
                        self.publish_inline_tail();
                    }

                    match result {
                        Ok(output) => {
                            // The editor learns what the file holds before it
                            // hears that the call finished.
                            send_tool_diff(&self.session.id, &tc.id, &output);
                            send_tool_exit(&self.session.id, &tc.id, &tc.name, &output);
                            let output = cap_tool_output_for_history(&tc.name, &self.session.id, output);
                            // A one-member group is part of the same round, so
                            // it spends from the same budget as the concurrent
                            // groups around it.
                            let mut output = round_budget.admit(&tc.name, output);
                            if let Some(reminder) = rule_passive.take() {
                                output.output.push_str(&reminder);
                            }
                            let _ = event_tx.send(ServerEvent::ToolDone {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                output: output.output.clone(),
                                error: None,
                            });

                            let side_pane_images = tool_output_side_pane_images(&tc.id, &tc.name, &tc.input, &output);
                            if !side_pane_images.is_empty() {
                                logging::info(&format!(
                                    "SidePaneImages: emitting {} image(s) from tool '{}' (session={})",
                                    side_pane_images.len(),
                                    tc.name,
                                    self.session.id
                                ));
                                let _ = event_tx.send(ServerEvent::SidePaneImages {
                                    session_id: self.session.id.clone(),
                                    images: side_pane_images,
                                });
                            }

                            let blocks = tool_output_to_history_blocks(tc.id.clone(), output, &self.session.id);
                            self.add_message_with_duration(Role::User, blocks, Some(tool_elapsed.as_millis() as u64));
                            tool_results_dirty = true;
                        }
                        Err(e) => {
                            // A takeover is not a tool failure: the tool already
                            // fired this turn's cancel signal. Recording its
                            // error would let the model answer it while the
                            // resumed turn works, and the siblings in this
                            // message would run too.
                            if e.downcast_ref::<crate::tool::SupersededToolCall>().is_some() {
                                crate::tool::superseded::clear(&self.session.id);
                                logging::info(&format!(
                                    "Tool '{}' handed the turn to another owner; stopping without recording a result",
                                    tc.name
                                ));
                                break;
                            }
                            let error_msg = format!("Error: {}", e);
                            let _ = event_tx.send(ServerEvent::ToolDone {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                output: error_msg.clone(),
                                error: Some(error_msg.clone()),
                            });

                            self.add_message_with_duration(
                                Role::User,
                                vec![ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content: error_msg,
                                    is_error: Some(true),
                                }],
                                Some(tool_elapsed.as_millis() as u64),
                            );
                            tool_results_dirty = true;
                        }
                    }
                } else if self.is_graceful_shutdown() {
                    // The turn's cancel signal was fired by the tool itself: the
                    // approval wait was taken over by the ambient runner. The
                    // tool may not have handed back its typed error yet, so the
                    // out-of-band mark is what names the reason. Reporting a
                    // reload here would be a lie: nothing reloaded.
                    if crate::tool::superseded::is_marked(&self.session.id) {
                        crate::tool::superseded::clear(&self.session.id);
                        logging::info(&format!(
                            "Tool '{}' handed the turn to another owner after {:.1}s; stopping without a result",
                            tc.name,
                            tool_elapsed.as_secs_f64()
                        ));
                        tool_handle.abort();
                        return Ok(());
                    }
                    // Server reload - abort tool and save interrupted result
                    logging::info(&format!(
                        "Tool '{}' interrupted by server reload after {:.1}s",
                        tc.name,
                        tool_elapsed.as_secs_f64()
                    ));
                    tool_handle.abort();

                    // For wait-like tools, the interruption is expected: the reload
                    // interrupted the wait, so it should be resumed after reload
                    // rather than treated as failed work.
                    let (interrupted_msg, is_error) = reload_interrupted_tool_result(tc, tool_elapsed.as_secs_f64());

                    let _ = event_tx.send(ServerEvent::ToolDone {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        output: interrupted_msg.clone(),
                        error: if is_error {
                            Some("interrupted by reload".to_string())
                        } else {
                            None
                        },
                    });

                    self.add_message_with_duration(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: interrupted_msg,
                            is_error: Some(is_error),
                        }],
                        Some(tool_elapsed.as_millis() as u64),
                    );
                    self.save_session()?;

                    // Add results for any remaining tools too
                    for remaining_tc in &tool_calls[(tool_index + 1)..] {
                        self.add_message(
                            Role::User,
                            vec![ContentBlock::ToolResult {
                                tool_use_id: remaining_tc.id.clone(),
                                content: "[Skipped - server reloading]".to_string(),
                                is_error: Some(true),
                            }],
                        );
                    }
                    self.save_session()?;
                    return Ok(());
                } else {
                    // User pressed Alt+B — move tool to background
                    logging::info(&format!(
                        "Tool '{}' moved to background after {:.1}s",
                        tc.name,
                        tool_elapsed.as_secs_f64()
                    ));

                    let bg_info = crate::background::global()
                        .adopt(&tc.name, &self.session.id, tool_handle)
                        .await;

                    let bg_msg = format!(
                        "Tool '{}' was moved to background by the user (task_id: {}). \
                         Use the `bg` tool with action 'wait' to wait for completion/checkpoints, \
                         or action 'status'/'output' to inspect it.",
                        tc.name, bg_info.task_id
                    );

                    let _ = event_tx.send(ServerEvent::ToolDone {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        output: bg_msg.clone(),
                        error: None,
                    });

                    self.add_message_with_duration(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: bg_msg,
                            is_error: None,
                        }],
                        Some(tool_elapsed.as_millis() as u64),
                    );
                    self.save_session()?;

                    self.background_tool_signal.reset();
                }

                // NOTE: We do NOT inject between tools (non-urgent) because that would
                // place user text between tool_results, which may violate API constraints.
                // All non-urgent injection happens at Point D after all tools are done.
            }

            if tool_results_dirty {
                self.save_session()?;
            }

            if !generated_image_contexts.is_empty() {
                for blocks in generated_image_contexts.drain(..) {
                    self.add_message(Role::User, blocks);
                }
                self.save_session()?;
            }

            // Bounded recovery for repeated malformed tool calls (e.g. a
            // model emitting null arguments): correct with the expected
            // schema, then end the turn with an actionable error instead of
            // retrying the same malformed round forever. Mirrors the blocking
            // loop (turn_loops.rs) via the shared helper.
            self.handle_malformed_tool_round(&malformed_calls, executed_valid_call, &tools)?;

            // === INJECTION POINT D: All tools done, before next API call ===
            // This is the safest point for non-urgent injection since all tool_results
            // have been added and the conversation is in a valid state.
            //
            // Feed the opted-in advisor first: a blocker it queues below is then
            // part of this boundary's soft-interrupt delivery.
            self.advisor_after_round().await;
            if let PostToolInterruptOutcome::SoftInterrupt { injected, point } = self
                .take_post_tool_soft_interrupt(super::interrupts::reborrow_in_flight(&mut in_flight))
                .await
            {
                for event in Self::build_soft_interrupt_events(injected, point, None) {
                    let _ = event_tx.send(event);
                }
            }
        }

        Ok(())
    }
}

/// How one member of a multi-member concurrent group is resolved: executed
/// locally, answered by the SDK, or rejected before it reaches a tool.
///
/// The variant decides where the call's single tool result and terminal event
/// come from, never their order: members are always applied by request index.
enum StreamedGroupMember {
    Execute,
    Sdk {
        content: String,
        is_error: bool,
    },
    Malformed {
        error: String,
    },
    /// The loop guard refused this call, so it never reached a tool. The
    /// message is the refusal itself, returned as the call's error result.
    Blocked {
        message: String,
    },
    /// A tool-scoped stream rule refused this call. Same result as `Blocked`,
    /// but it is not a loop-guard decision.
    RuleBlocked {
        message: String,
    },
    /// A loop-guard stop earlier in the same group ended the turn before this
    /// call was dispatched.
    Skipped,
}

/// How a multi-member group ended, for the round that called it.
enum StreamedGroupOutcome {
    /// Every member was applied; the round continues with the next group.
    Continue,
    /// The loop guard ended the turn at this notice.
    GuardStop(String),
    /// An urgent interrupt cut the group short. `skipped` is how many calls of
    /// the round produced no result: the members that were cancelled plus every
    /// call after the group. The caller writes the round's remaining skips and
    /// injects the message at point C.
    Interrupted { skipped: usize },
}

impl Agent {
    /// Emit the terminal event and the stored result of a call the loop guard
    /// kept from running, at that call's own index.
    fn write_loop_guard_result(&mut self, tc: &ToolCall, message: &str, event_tx: &mpsc::UnboundedSender<ServerEvent>) {
        let _ = event_tx.send(ServerEvent::ToolDone {
            id: tc.id.clone(),
            name: tc.name.clone(),
            output: message.to_string(),
            error: Some(message.to_string()),
        });
        self.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: tc.id.clone(),
                content: message.to_string(),
                is_error: Some(true),
            }],
        );
    }

    /// Finish a round an urgent interrupt cut short.
    ///
    /// `first_unwritten` is the first call of the round that still owes a
    /// result: the group already wrote its own members' results, in request
    /// order, so this covers the calls after it. `skipped` is how many calls of
    /// the round produced no result at all, which is what the injected event
    /// reports as `tools_skipped`. The group-boundary check and a mid-group
    /// interrupt both end here, so the transcript, the client event and the
    /// persisted session are identical whichever one noticed the interrupt.
    async fn finish_streamed_round_interrupt(
        &mut self,
        tool_calls: &[ToolCall],
        first_unwritten: usize,
        skipped: usize,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        in_flight: Option<&mut InFlightGuard>,
    ) {
        // Add tool_results for all remaining skipped tools to maintain valid history
        for skipped_tc in &tool_calls[first_unwritten..] {
            self.add_message(
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: skipped_tc.id.clone(),
                    content: USER_INTERRUPT_SKIP.to_string(),
                    is_error: Some(true),
                }],
            );
        }
        let injected = self.inject_soft_interrupts(in_flight).await;
        if !injected.is_empty() {
            for event in Self::build_soft_interrupt_events(injected, "C", Some(skipped)) {
                let _ = event_tx.send(event);
            }
            // Add note about skipped tools for the AI
            self.add_message(
                Role::User,
                vec![ContentBlock::Text {
                    text: format!("[User interrupted: {skipped} remaining tool(s) skipped]"),
                    cache_control: None,
                }],
            );
        }
        self.persist_session_best_effort("streamed tool output");
    }

    /// End the turn at a loop-guard stop.
    ///
    /// Every call of the round that was never handled still owes one result, or
    /// the next request would be formed from an invalid transcript. The notice
    /// is stored as an assistant-visible message so both the model and the user
    /// read why the turn stopped, and it is announced to the live client as a
    /// stop rather than as a normal completion. `LimitReached` is the reason the
    /// wire protocol already has for "a bound ended this turn": the SDK does
    /// not auto-retry it, which is what a repeated call deserves.
    fn finish_loop_guard_stop(
        &mut self,
        tool_calls: &[ToolCall],
        first_unhandled: usize,
        notice: &str,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<()> {
        for tc in &tool_calls[first_unhandled..] {
            self.write_loop_guard_result(tc, Self::LOOP_GUARD_SKIP, event_tx);
        }
        self.record_loop_guard_stop(notice);
        let _ = event_tx.send(ServerEvent::TurnStopped {
            reason: crate::protocol::TurnStopReason::LimitReached,
            message: notice.to_string(),
            provider_stop_reason: None,
        });
        self.save_session()?;
        Ok(())
    }

    /// Run one multi-member concurrent group through the shared scheduler.
    ///
    /// This path registers no `running_tool_registry` entry. That registry
    /// holds ONE entry per session and the background signal carries no target
    /// identity, so a group can be neither named nor safely detached; a
    /// background request raised while the group runs reports `moved: false`.
    /// The signal is cleared before returning so it cannot move the next
    /// one-member call.
    ///
    /// When the server reloads mid-group, [`super::tool_round::run_group`]
    /// reports the members that produced a result plus the indexes that did
    /// not. Completed work is applied, and every unapplied index of the GROUP
    /// gets a `[Skipped - server reloading]` result, so the transcript always
    /// holds exactly one `tool_result` per `tool_use`.
    ///
    /// The loop guard is consulted once here for the whole group, before any
    /// member starts. [`StreamedGroupOutcome::GuardStop`] is returned when that
    /// decision ended the turn: the caller then writes the rest of the round's
    /// results and stops.
    ///
    /// An urgent interrupt is observed while the members are in flight, so a
    /// multi-member group stops early instead of running every paid request to
    /// completion. Members that already finished keep their results, every
    /// other member gets a skip, and
    /// [`StreamedGroupOutcome::Interrupted`] tells the caller where the round's
    /// remaining skips begin.
    #[allow(clippy::too_many_arguments)]
    async fn run_streamed_tool_group(
        &mut self,
        tool_calls: &[ToolCall],
        group: super::tool_round::ToolGroup,
        assistant_message_id: Option<&str>,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
        sdk_tool_results: &mut HashMap<String, (String, bool)>,
        malformed_calls: &mut Vec<MalformedToolCallInfo>,
        executed_valid_call: &mut bool,
        tool_results_dirty: &mut bool,
        round_budget: &mut super::round_budget::RoundTokenBudget,
        stream_rule_state: &mut TurnStreamRules,
        trace: bool,
        inline_output_tap: bool,
    ) -> Result<StreamedGroupOutcome> {
        let message_id = assistant_message_id.unwrap_or(&self.session.id).to_string();

        // Resolve every member in request order before dispatching. A malformed
        // call and an SDK-provided result never reach a tool, but each still
        // owes one tool result and one terminal event at its own index.
        let mut members: Vec<StreamedGroupMember> = Vec::with_capacity(group.len());
        let mut validation_error: Option<anyhow::Error> = None;
        for index in group.range() {
            let tc = &tool_calls[index];

            if let Some(error_msg) = tc.validation_error() {
                members.push(StreamedGroupMember::Malformed { error: error_msg });
                continue;
            }

            // A call that passed validation is genuinely valid even if the tool
            // itself later fails: it resets the malformed-round bound in
            // handle_malformed_tool_round below.
            *executed_valid_call = true;
            if let Err(error) = self.validate_tool_allowed(&tc.name) {
                // The members resolved before this one still run and land
                // first, exactly as they did when the round was one call at a
                // time.
                validation_error = Some(error);
                break;
            }

            let is_native_tool = HORON_AGENT_NATIVE_TOOLS.contains(&tc.name.as_str());
            if let Some((sdk_content, sdk_is_error)) = sdk_tool_results.remove(&tc.id)
                && !(is_native_tool && sdk_is_error)
            {
                members.push(StreamedGroupMember::Sdk {
                    // Left uncapped here on purpose: the round admits it in
                    // request order below, and capping first would make the
                    // protocol ceiling and the round's ceiling two separate
                    // decisions instead of one.
                    content: sdk_content,
                    is_error: sdk_is_error,
                });
                continue;
            }

            members.push(StreamedGroupMember::Execute);
        }

        let dispatch: Vec<(usize, ToolCall)> = members
            .iter()
            .enumerate()
            .filter(|(_, member)| matches!(member, StreamedGroupMember::Execute))
            .map(|(offset, _)| {
                let index = group.start + offset;
                (index, tool_calls[index].clone())
            })
            .collect();

        // One loop-guard decision for the whole group, taken before any member
        // runs: a refused member is dropped from the dispatch set instead of
        // being judged mid-group, and a stop ends the turn once this group's
        // results are written.
        let verdicts = self.consult_loop_guard(dispatch.iter().map(|(_, tc)| tc));
        let mut executable: Vec<(usize, ToolCall)> = Vec::with_capacity(dispatch.len());
        let mut guard_stop: Option<String> = None;
        for ((index, tc), verdict) in dispatch.into_iter().zip(verdicts) {
            if guard_stop.is_some() {
                members[index - group.start] = StreamedGroupMember::Skipped;
                continue;
            }
            match verdict {
                super::loop_guard::LoopGuardVerdict::Allow | super::loop_guard::LoopGuardVerdict::Warn(_) => {
                    executable.push((index, tc))
                }
                super::loop_guard::LoopGuardVerdict::Refuse(message) => {
                    members[index - group.start] = StreamedGroupMember::Blocked { message };
                }
                super::loop_guard::LoopGuardVerdict::Stop(message) => {
                    members[index - group.start] = StreamedGroupMember::Blocked {
                        message: message.clone(),
                    };
                    guard_stop = Some(message);
                }
            }
        }
        // A tool-scoped rule decides each call before the group dispatches. A
        // blocked member is replaced by a refusal result; a passive hit still
        // runs and carries its reminder in the member's result.
        let mut rule_passive: HashMap<usize, String> = HashMap::new();
        let mut rule_blocked: HashSet<usize> = HashSet::new();
        for (index, tc) in &executable {
            match stream_rule_state.check_tool(&tc.name, &tc.input) {
                Some(ToolRuleAction::Block(hit)) => {
                    let message = super::stream_rules::blocked_message(&hit);
                    logging::warn(&format!("Stream rule blocked tool '{}': {message}", tc.name));
                    members[*index - group.start] = StreamedGroupMember::RuleBlocked { message };
                    rule_blocked.insert(*index);
                }
                Some(ToolRuleAction::Passive(hit)) => {
                    rule_passive.insert(*index, super::stream_rules::tool_passive_reminder(&hit));
                }
                None => {}
            }
        }
        stream_rule_state.flush_ledger(&mut self.session.stream_rule_ledger);
        let dispatch: Vec<(usize, ToolCall)> = executable
            .into_iter()
            .filter(|(index, _)| !rule_blocked.contains(index))
            .collect();

        for (_, tc) in &dispatch {
            if trace {
                eprintln!("[trace] tool_exec_start name={} id={}", tc.name, tc.id);
            }
            logging::info(&format!("Tool starting: {}", tc.name));
            crate::session_metrics::record_activity(&self.session.id);
            if inline_output_tap {
                // Surface every member of the group immediately: they are all
                // in flight for the same stretch of wall-clock time.
                self.inline_tail.start_tool(&tc.name, &tc.input);
                self.publish_inline_tail();
            }
        }

        let mut results: HashMap<usize, Result<crate::tool::ToolOutput>> = HashMap::new();
        let mut tool_elapsed = Duration::ZERO;
        let mut stopped: Option<super::tool_round::GroupStop> = None;
        if !dispatch.is_empty() {
            let ctx = ToolContext {
                session_id: self.session.id.clone(),
                message_id: message_id.clone(),
                tool_call_id: String::new(),
                working_dir: self.working_dir().map(PathBuf::from),
                stdin_request_tx: self.stdin_request_tx.clone(),
                graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
                execution_mode: ToolExecutionMode::AgentTurn,
            };
            let calls: Vec<ToolCall> = dispatch.iter().map(|(_, tc)| tc.clone()).collect();
            let tool_start = Instant::now();
            // The group's registry shares this agent's `CompactionManager`, so
            // the overflow guard inside `Registry::execute` reads the real
            // context size instead of the fresh-manager floor.
            //
            // The shutdown signal cuts the group on a reload. The interrupt
            // watch is what keeps the daemon's reaction per call instead of per
            // group: an urgent interrupt that lands while members are in flight
            // cancels the rest rather than letting every paid request finish
            // first. The watch reads the same queue `has_urgent_interrupt`
            // does, so both paths agree on what an interrupt is.
            let interrupt_queue = self.soft_interrupt_queue.clone();
            let cancel = super::tool_round::GroupCancel::new(self.graceful_shutdown.clone()).watching_interrupt(
                Arc::new(move || super::interrupts::urgent_interrupt_pending(&interrupt_queue)),
            );
            let outcome =
                super::tool_round::run_group(self.registry.clone_sharing_compaction(), &calls, &ctx, Some(cancel))
                    .await;
            tool_elapsed = tool_start.elapsed();
            stopped = outcome.stopped;
            for (position, result) in outcome.results {
                results.insert(dispatch[position].0, result);
            }
        }

        // The group deliberately ignored the background signal (it has no
        // target identity), so clear whatever was raised while it ran. Without
        // this, a fire during the group could move the next one-member call.
        self.background_tool_signal.reset();

        // Apply in request order: terminal events, stored durations and history
        // all follow the model's order even when the members finished in another.
        let mut any_member_failed = false;
        let mut skipped_members = 0usize;
        for (offset, member) in members.iter().enumerate() {
            let index = group.start + offset;
            let tc = &tool_calls[index];
            match member {
                StreamedGroupMember::Malformed { error } => {
                    malformed_calls.push(MalformedToolCallInfo {
                        name: tc.name.clone(),
                        error: error.clone(),
                    });
                    logging::warn(error);
                    let _ = event_tx.send(ServerEvent::ToolDone {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        output: error.clone(),
                        error: Some(error.clone()),
                    });
                    self.add_message(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: error.clone(),
                            is_error: Some(true),
                        }],
                    );
                    *tool_results_dirty = true;
                }
                StreamedGroupMember::Sdk { content, is_error } => {
                    // Applied in request order like every other member, so the
                    // round's running total advances in the order the model
                    // asked rather than the order the provider answered.
                    let content = round_budget.admit_sdk_result(
                        &self.session.id,
                        &tc.name,
                        &tc.input,
                        content.clone(),
                        *is_error,
                    );
                    self.add_message(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content,
                            is_error: if *is_error { Some(true) } else { None },
                        }],
                    );
                    *tool_results_dirty = true;
                }
                StreamedGroupMember::Blocked { message } => {
                    self.write_loop_guard_result(tc, message, event_tx);
                    *tool_results_dirty = true;
                }
                StreamedGroupMember::RuleBlocked { message } => {
                    // A tool-scoped stream rule refused this call; it owes the
                    // same error result without touching the malformed bound.
                    self.write_loop_guard_result(tc, message, event_tx);
                    *tool_results_dirty = true;
                }
                StreamedGroupMember::Skipped => {
                    // The stop earlier in this group ended the turn before this
                    // call was dispatched; the result only keeps the transcript
                    // valid.
                    self.write_loop_guard_result(tc, Self::LOOP_GUARD_SKIP, event_tx);
                    *tool_results_dirty = true;
                }
                StreamedGroupMember::Execute => {
                    self.unlock_tools_if_needed(&tc.name, &tc.input);
                    crate::session_metrics::record_activity(&self.session.id);
                    let result = results.remove(&index);
                    any_member_failed |= result.as_ref().is_none_or(|result| result.is_err());

                    match result {
                        Some(Ok(output)) => {
                            logging::info(&format!(
                                "Tool finished: {} in {:.2}s",
                                tc.name,
                                tool_elapsed.as_secs_f64()
                            ));
                            // A member of a parallel group reports its diff on
                            // the same order as a lone call: before its
                            // terminal event.
                            send_tool_diff(&self.session.id, &tc.id, &output);
                            send_tool_exit(&self.session.id, &tc.id, &tc.name, &output);
                            let output = cap_tool_output_for_history(&tc.name, &self.session.id, output);
                            // The tool layer only saw the context size from
                            // before the round; the round's running total is
                            // what refuses a result once the window is spent.
                            let mut output = round_budget.admit(&tc.name, output);
                            if let Some(reminder) = rule_passive.get(&index) {
                                output.output.push_str(reminder);
                            }
                            let _ = event_tx.send(ServerEvent::ToolDone {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                output: output.output.clone(),
                                error: None,
                            });

                            let side_pane_images = tool_output_side_pane_images(&tc.id, &tc.name, &tc.input, &output);
                            if !side_pane_images.is_empty() {
                                logging::info(&format!(
                                    "SidePaneImages: emitting {} image(s) from tool '{}' (session={})",
                                    side_pane_images.len(),
                                    tc.name,
                                    self.session.id
                                ));
                                let _ = event_tx.send(ServerEvent::SidePaneImages {
                                    session_id: self.session.id.clone(),
                                    images: side_pane_images,
                                });
                            }

                            let blocks = tool_output_to_history_blocks(tc.id.clone(), output, &self.session.id);
                            self.add_message_with_duration(Role::User, blocks, Some(tool_elapsed.as_millis() as u64));
                            *tool_results_dirty = true;
                        }
                        Some(Err(e)) => {
                            let error_msg = format!("Error: {}", e);
                            let _ = event_tx.send(ServerEvent::ToolDone {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                output: error_msg.clone(),
                                error: Some(error_msg.clone()),
                            });
                            self.add_message_with_duration(
                                Role::User,
                                vec![ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content: error_msg,
                                    is_error: Some(true),
                                }],
                                Some(tool_elapsed.as_millis() as u64),
                            );
                            *tool_results_dirty = true;
                        }
                        None => {
                            // The group was cancelled before this member
                            // produced a result: the server reloaded, or an
                            // urgent interrupt arrived while it was in flight.
                            // The skip is written from GROUP membership, so
                            // every `tool_use` keeps exactly one `tool_result`
                            // whatever the members did.
                            skipped_members += 1;
                            let (skipped, error) = if matches!(stopped, Some(super::tool_round::GroupStop::Interrupt)) {
                                (USER_INTERRUPT_SKIP.to_string(), "interrupted by user".to_string())
                            } else {
                                (RELOAD_SKIP.to_string(), "interrupted by reload".to_string())
                            };
                            let _ = event_tx.send(ServerEvent::ToolDone {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                output: skipped.clone(),
                                error: Some(error),
                            });
                            self.add_message(
                                Role::User,
                                vec![ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content: skipped,
                                    is_error: Some(true),
                                }],
                            );
                            *tool_results_dirty = true;
                        }
                    }
                }
            }
        }

        if inline_output_tap {
            // One marker is pending for the group; finish it once with the
            // group's wall-clock time rather than once per member.
            self.inline_tail
                .finish_tool(tool_elapsed.as_secs_f64(), any_member_failed);
            self.publish_inline_tail();
        }

        if let Some(error) = validation_error {
            return Err(error);
        }
        // A loop-guard stop wins over a simultaneous interrupt: it ends the
        // turn, and the queued interrupt is injected at the next safe point
        // instead of being claimed here.
        if let Some(notice) = guard_stop {
            return Ok(StreamedGroupOutcome::GuardStop(notice));
        }
        match stopped {
            Some(super::tool_round::GroupStop::Interrupt) => {
                let skipped = skipped_members + tool_calls.len() - group.end;
                // Every member of the last group produced a result and the
                // round has no call left to skip: nothing was lost, so the
                // interrupt is injected at point D like any other post-round
                // message instead of being reported as skipped calls.
                if skipped == 0 {
                    Ok(StreamedGroupOutcome::Continue)
                } else {
                    Ok(StreamedGroupOutcome::Interrupted { skipped })
                }
            }
            _ => Ok(StreamedGroupOutcome::Continue),
        }
    }
}

#[cfg(test)]
#[path = "turn_streaming_mpsc_interrupt_tests.rs"]
mod interrupt_tests;
#[cfg(test)]
#[path = "turn_streaming_mpsc_round_tests.rs"]
mod round_tests;
#[cfg(test)]
#[path = "turn_streaming_mpsc_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "turn_streaming_mpsc_failover_tests.rs"]
mod failover_tests;
