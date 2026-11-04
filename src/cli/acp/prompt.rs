//! `session/prompt`: content blocks in, streamed `session/update`s out, and a
//! response whose `stopReason` is the true cause the daemon reported.
//!
//! Stop reasons come from the prompt stream. `Done` ends the turn normally;
//! `TurnStopped` (emitted before the terminal `Done`/`Error`,
//! crates/protocol/src/wire/events.rs `TurnStopped`) and `ProviderGuardrail`
//! name the cause; `Interrupted` or any recorded cancel means `cancelled`.

use super::*;
use crate::protocol::TurnStopReason;
use crate::server::foreground_state::{OUTPUT_LIMIT_STOPS, REFUSAL_STOPS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

impl StopReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug)]
pub(super) enum TurnEnd {
    Done,
    Error(String),
    /// Left without a terminal event after a cancel (a post-cancel
    /// `TurnStopped`, daemon EOF, or the cancel bound).
    Abandoned,
}

#[derive(Debug, Default)]
pub(super) struct TurnOutcome {
    stop: Option<StopReason>,
    failure: Option<String>,
}

impl TurnOutcome {
    pub(super) fn observe(&mut self, event: &ServerEvent) {
        match event {
            ServerEvent::Interrupted => self.record(StopReason::Cancelled),
            ServerEvent::TurnStopped {
                reason,
                message,
                provider_stop_reason,
            } => match reason {
                TurnStopReason::Interrupted => self.record(StopReason::Cancelled),
                TurnStopReason::ProviderGuardrail => self.record(StopReason::Refusal),
                TurnStopReason::LimitReached => {
                    let output_limit = provider_stop_reason
                        .as_deref()
                        .is_some_and(|reason| OUTPUT_LIMIT_STOPS.contains(&reason));
                    self.record(if output_limit {
                        StopReason::MaxTokens
                    } else {
                        StopReason::MaxTurnRequests
                    });
                }
                TurnStopReason::Failure | TurnStopReason::Crash | TurnStopReason::Unknown => {
                    self.failure.get_or_insert_with(|| message.clone());
                }
            },
            ServerEvent::ProviderGuardrail {
                stop_reason: Some(stop_reason),
                ..
            } if REFUSAL_STOPS.contains(&stop_reason.as_str()) => self.record(StopReason::Refusal),
            _ => {}
        }
    }

    /// The first named cause wins, except that a cancel overrides any cause.
    fn record(&mut self, reason: StopReason) {
        if self.stop.is_none() || reason == StopReason::Cancelled {
            self.stop = Some(reason);
        }
    }

    pub(super) fn resolve(&self, cancel_recorded: bool, end: TurnEnd) -> std::result::Result<StopReason, String> {
        if cancel_recorded {
            return Ok(StopReason::Cancelled);
        }
        if let Some(stop) = self.stop {
            return Ok(stop);
        }
        match end {
            TurnEnd::Done => match &self.failure {
                Some(message) => Err(message.clone()),
                None => Ok(StopReason::EndTurn),
            },
            TurnEnd::Error(message) => Err(message),
            TurnEnd::Abandoned => Ok(StopReason::Cancelled),
        }
    }
}

/// Token usage rides in `_meta`: the v1 `PromptResponse` defines only
/// `stopReason` and `_meta`.
pub(super) fn prompt_response(stop_reason: &str, usage: &TurnUsage) -> Value {
    let mut response = json!({ "stopReason": stop_reason });
    if let Some(usage) = usage.to_acp() {
        response["_meta"] = json!({ "horon-agent": { "usage": usage } });
    }
    response
}

pub(super) fn prompt_from_params(params: &Value) -> std::result::Result<(String, Vec<(String, String)>), String> {
    let prompt = params
        .get("prompt")
        .and_then(Value::as_array)
        .ok_or_else(|| "Missing required prompt array".to_string())?;
    let mut text_parts = Vec::new();
    let mut images = Vec::new();

    for block in prompt {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Text content block missing text".to_string())?;
                text_parts.push(text.to_string());
            }
            Some("image") => {
                let mime_type = block
                    .get("mimeType")
                    .or_else(|| block.get("mime_type"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Image content block missing mimeType".to_string())?;
                let data = block
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Image content block missing data".to_string())?;
                images.push((mime_type.to_string(), data.to_string()));
            }
            Some("audio") => return Err("audio not supported".to_string()),
            Some("resource") => {
                let resource = block
                    .get("resource")
                    .filter(|resource| resource.get("uri").is_some_and(Value::is_string))
                    .ok_or_else(|| "Resource content block missing resource.uri".to_string())?;
                text_parts.push(format_resource_block(resource));
            }
            Some("resource_link") => {
                let uri = block
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Resource link content block missing uri".to_string())?;
                let name = block.get("name").and_then(Value::as_str).unwrap_or(uri);
                text_parts.push(format!("[Resource link: {name} <{uri}>]"));
            }
            Some(other) => {
                return Err(format!("Unsupported ACP prompt content block type: {other}"));
            }
            None => return Err("Prompt content block missing type".to_string()),
        }
    }

    Ok((text_parts.join("\n\n"), images))
}

pub(super) fn format_resource_block(resource: &Value) -> String {
    let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("resource");
    if let Some(text) = resource.get("text").and_then(Value::as_str) {
        format!("[Embedded resource: {uri}]\n{text}")
    } else if let Some(blob) = resource.get("blob").and_then(Value::as_str) {
        let mime = resource
            .get("mimeType")
            .or_else(|| resource.get("mime_type"))
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream");
        format!(
            "[Embedded binary resource: {uri} ({mime}, {} base64 bytes)]",
            blob.len()
        )
    } else {
        format!("[Embedded resource: {uri}]")
    }
}

/// Opaque prefix of the turn's message ids. The daemon request id restarts
/// with every daemon connection, so the clock keeps ids unique across reloads.
fn turn_token(prompt_id: u64) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    format!("t{millis}-{prompt_id}")
}

/// Clears the session's busy flag when the prompt's turn is over.
///
/// [`Self::release`] clears it at the point the turn ends, so the session takes
/// its next prompt before this one's response is written; [`Drop`] clears it on
/// every other exit path, including the `?` returns of a failed stdout write.
pub(super) struct PromptRunGuard<'a> {
    session: &'a DaemonSession,
    armed: bool,
}

impl<'a> PromptRunGuard<'a> {
    pub(super) fn new(session: &'a DaemonSession) -> Self {
        Self { session, armed: true }
    }

    /// Leave the busy flag set for whoever answers the prompt from here on
    /// (the v2 stream task), which clears it before it writes that answer.
    pub(super) fn hand_over(mut self) {
        self.armed = false;
    }

    pub(super) fn release(&mut self) {
        if self.armed {
            self.armed = false;
            self.session.prompt_running.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for PromptRunGuard<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

impl AcpRuntime {
    /// The `usage_update` one daemon `TokenUsage` reports: the context the
    /// turn uses now, against the session's context limit.
    pub(super) async fn write_usage_update(
        &self,
        session: &DaemonSession,
        input: u64,
        cache_read_input: Option<u64>,
        cache_creation_input: Option<u64>,
    ) -> Result<()> {
        let (provider_name, context_limit) = {
            let state = session.ui_state.lock().await;
            (state.provider_name.clone().unwrap_or_default(), state.context_limit())
        };
        let used = crate::compaction::effective_context_tokens_from_usage(
            &provider_name,
            input,
            cache_read_input,
            cache_creation_input,
        );
        self.write_notification(
            "session/update",
            json!({
                "sessionId": session.session_id,
                "update": {
                    "sessionUpdate": "usage_update",
                    "used": used,
                    "size": context_limit,
                }
            }),
        )
        .await
    }

    pub(super) async fn handle_session_prompt(&self, message: IncomingRequest) -> Result<()> {
        let id = message.id;
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await?;
                return Ok(());
            }
        };
        let (text, images) = match prompt_from_params(&message.params) {
            Ok(prompt) => prompt,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err).await?;
                return Ok(());
            }
        };
        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get(&session_id).cloned()
        };
        let Some(session) = session else {
            return self.write_rpc_error(id, RpcError::session_not_found(&session_id)).await;
        };

        if session
            .prompt_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            self.write_error_value(
                id,
                JSONRPC_INVALID_PARAMS,
                format!("Session {session_id} is already processing a prompt"),
            )
            .await?;
            return Ok(());
        }

        let cancel = message.cancel;
        if let Err(err) = self.run_prompt(id.clone(), session, text, images, &cancel).await {
            if cancel.is_recorded() {
                self.write_result(id, prompt_response("cancelled", &TurnUsage::default()))
                    .await?;
            } else {
                self.write_error_value(id, JSONRPC_INTERNAL_ERROR, format!("Prompt failed: {err:#}"))
                    .await?;
            }
        }
        Ok(())
    }

    async fn run_prompt(
        &self,
        rpc_id: Value,
        session: Arc<DaemonSession>,
        text: String,
        images: Vec<(String, String)>,
        cancel: &CancelSignal,
    ) -> Result<()> {
        let mut guard = PromptRunGuard::new(&session);
        if let Some(command) = parse_acp_slash_command(&text) {
            let response = match command {
                Ok(command) => self.run_session_command(&session, command).await,
                Err(err) => Err(err),
            };
            guard.release();
            let response = response?;
            self.write_notification(
                "session/update",
                json!({
                    "sessionId": session.session_id,
                    "update": agent_message_chunk(response),
                }),
            )
            .await?;
            self.write_result(rpc_id, prompt_response("end_turn", &TurnUsage::default()))
                .await?;
            return Ok(());
        }

        if cancel.is_recorded() {
            guard.release();
            return self
                .write_result(rpc_id, prompt_response("cancelled", &TurnUsage::default()))
                .await;
        }

        session.settle_cancelled_turn().await;
        let prompt_id = session.next_id();
        let message = Request::Message {
            id: prompt_id,
            content: text,
            images,
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        };
        let mut events = match session.foreground_request(Some(prompt_id), &message).await {
            Ok(events) => events,
            Err(err) => {
                guard.release();
                return Err(err);
            }
        };
        if cancel.mark_turn_written() {
            // The cancel was recorded before this message reached the daemon,
            // so its handler left the daemon `Cancel` to this prompt (one per
            // prompt, see `CancelSignal::cancel_turn`).
            let cancel_id = session.next_id();
            let _ = session.send(&Request::Cancel { id: cancel_id }).await;
        }

        let options = MapperOptions::new(&self.client_capabilities(), session.cwd.lock().await.clone());
        let mut mapper = EventMapper::new(self.profile, options, turn_token(prompt_id));
        let mut outcome = TurnOutcome::default();
        let mut turn_usage = TurnUsage::default();
        let mut cancel_deadline: Option<tokio::time::Instant> = None;
        let mut interrupted_by_daemon = false;
        // The early answer came on a `TurnStopped` (any reason), not on the
        // daemon going away or the cancel bound.
        let mut abandoned_on_turn_stopped = false;
        let end = loop {
            let deadline = cancel_deadline.unwrap_or_else(|| tokio::time::Instant::now() + PROMPT_CANCEL_BOUND);
            let next = tokio::select! {
                event = events.next_routed() => event,
                () = cancel.recorded(), if cancel_deadline.is_none() => {
                    cancel_deadline = Some(tokio::time::Instant::now() + PROMPT_CANCEL_BOUND);
                    continue;
                }
                () = tokio::time::sleep_until(deadline), if cancel_deadline.is_some() => break TurnEnd::Abandoned,
            };
            let event = match next {
                Ok(Routed::Daemon(event)) => *event,
                Ok(Routed::Local(update)) => {
                    self.write_local_update(&session.session_id, &mut mapper, update)
                        .await?;
                    continue;
                }
                Err(_) if cancel.is_recorded() => break TurnEnd::Abandoned,
                Err(err) => {
                    guard.release();
                    return Err(err);
                }
            };
            interrupted_by_daemon |= matches!(
                event,
                ServerEvent::TurnStopped {
                    reason: TurnStopReason::Interrupted,
                    ..
                }
            );
            if self.profile.is_extended() {
                self.write_horon_agent_extension_event(&session.session_id, &event)
                    .await?;
            }
            match event {
                ServerEvent::Ack { .. } => {}
                ServerEvent::Done { id } if id == prompt_id => break TurnEnd::Done,
                ServerEvent::Error { id, message, .. } if id == prompt_id => break TurnEnd::Error(message),
                ServerEvent::TurnStopped { .. } if cancel.is_recorded() => {
                    abandoned_on_turn_stopped = true;
                    break TurnEnd::Abandoned;
                }
                ServerEvent::TokenUsage {
                    input,
                    output,
                    cache_read_input,
                    cache_creation_input,
                } => {
                    turn_usage.add(input, output, cache_read_input, cache_creation_input);
                    self.write_usage_update(&session, input, cache_read_input, cache_creation_input)
                        .await?;
                }
                ServerEvent::ModelChanged {
                    model,
                    provider_name,
                    error,
                    ..
                } => {
                    // Mid-prompt model changes happen on provider failover;
                    // keep the selector in sync.
                    if error.is_none() {
                        {
                            let mut state = session.ui_state.lock().await;
                            state.model = Some(model);
                            if provider_name.is_some() {
                                state.provider_name = provider_name;
                            }
                        }
                        self.write_config_option_update(&session).await?;
                    }
                }
                other => {
                    outcome.observe(&other);
                    for update in mapper.map_event(other) {
                        self.write_notification(
                            "session/update",
                            json!({
                                "sessionId": session.session_id,
                                "update": update,
                            }),
                        )
                        .await?;
                    }
                }
            }
        };

        // From here on no daemon `Cancel` is sent for this turn.
        let daemon_cancel_sent = cancel.retire_daemon_cancel();
        if matches!(end, TurnEnd::Abandoned) {
            // This prompt answered before its own terminal, so the turn is
            // still streaming. When it answered on a `TurnStopped` (the
            // cancel's own `Interrupted`, a peer connection's interrupt, a
            // worker failure or limit), the turn's worker - or, if the cancel
            // aborted it, the cancel path - still sends one `Done`/`Error` for
            // the turn id; whether `Interrupted` and a second `Done` follow
            // depends on who interrupted and whether the worker had finished
            // (crates/app-core/src/server/client_lifecycle.rs:4050-4061,
            // :4131-4133, :4154-4174). So the drain ends at that first
            // terminal and then drops at most one stray note and any further
            // terminal of the old id. Only an answer on daemon EOF or the
            // cancel bound, where nothing of the turn has been seen, waits for
            // the cancel path's `Interrupted` + `Done` pair.
            if abandoned_on_turn_stopped {
                events.drain_worker_turn(prompt_id);
            } else {
                events.drain_cancelled_turn(prompt_id);
            }
            session.mark_cancelled_turn();
        } else if daemon_cancel_sent || interrupted_by_daemon {
            // The turn reported its own terminal, so the daemon has nothing
            // left to send but - when a cancel found the turn already finished
            // - at most one `Interrupted` and no `Done`. Discard that note and
            // never hold back the next prompt's frames.
            events.discard_stray_cancel_note(prompt_id);
            session.mark_cancelled_turn();
        } else {
            drop(events);
        }
        guard.release();
        match outcome.resolve(cancel.is_recorded(), end) {
            Ok(stop) => {
                self.write_result(rpc_id, prompt_response(stop.as_str(), &turn_usage))
                    .await
            }
            Err(message) => self.write_error_value(rpc_id, JSONRPC_INTERNAL_ERROR, message).await,
        }
    }
}
