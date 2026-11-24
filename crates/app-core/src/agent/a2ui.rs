//! Agent-side A2UI capability management, live envelope streaming, and turn completion.
use super::*;
use crate::tool::A2UI_TOOL;
use crate::tool::a2ui::A2UI_METADATA_KEY;
use horon_agent_tool_types::a2ui::{
    A2uiSessionCapabilities, MAX_A2UI_ENVELOPE_BYTES, OutboundBatchValidator, render_a2ui_text_card,
};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(super) struct A2uiEmission {
    envelopes: Vec<Value>,
    fallback_text: Option<String>,
    final_response: bool,
}

#[derive(Debug, Default)]
pub(super) struct TurnA2uiState {
    incremental: HashMap<String, IncrementalA2uiStreamer>,
    round: Vec<A2uiEmission>,
    deferred: Vec<A2uiEmission>,
    any_round_error: bool,
}

#[derive(Debug)]
struct IncrementalA2uiStreamer {
    buffer: String,
    scan_pos: usize,
    phase: ScanPhase,
    validator: OutboundBatchValidator,
    emitted_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanPhase {
    SeekingArray,
    SeekingElement,
    InObject {
        start: usize,
        depth: usize,
        in_string: bool,
        escaped: bool,
    },
    Done,
}

impl IncrementalA2uiStreamer {
    fn new() -> Self {
        Self {
            buffer: String::new(),
            scan_pos: 0,
            phase: ScanPhase::SeekingArray,
            validator: OutboundBatchValidator::new(),
            emitted_count: 0,
        }
    }

    /// Feed a streamed JSON argument delta and return any newly completed, validated envelopes.
    fn push_delta(&mut self, delta: &str, caps: &A2uiSessionCapabilities) -> Vec<(u32, Value)> {
        if self.phase == ScanPhase::Done || self.buffer.len() + delta.len() > MAX_A2UI_ENVELOPE_BYTES * 8 {
            return Vec::new();
        }
        self.buffer.push_str(delta);
        let mut ready = Vec::new();

        loop {
            match self.phase {
                ScanPhase::Done => break,
                ScanPhase::SeekingArray => {
                    let Some(open) = find_envelopes_array_open(&self.buffer) else {
                        break;
                    };
                    self.scan_pos = open + 1;
                    self.phase = ScanPhase::SeekingElement;
                }
                ScanPhase::SeekingElement => {
                    let bytes = self.buffer.as_bytes();
                    while self.scan_pos < bytes.len() {
                        match bytes[self.scan_pos] {
                            b' ' | b'\t' | b'\r' | b'\n' | b',' => self.scan_pos += 1,
                            b'{' => {
                                self.phase = ScanPhase::InObject {
                                    start: self.scan_pos,
                                    depth: 1,
                                    in_string: false,
                                    escaped: false,
                                };
                                self.scan_pos += 1;
                                break;
                            }
                            b']' => {
                                self.phase = ScanPhase::Done;
                                self.scan_pos += 1;
                                break;
                            }
                            _ => {
                                self.phase = ScanPhase::Done;
                                break;
                            }
                        }
                    }
                    if matches!(self.phase, ScanPhase::SeekingElement) {
                        break;
                    }
                }
                ScanPhase::InObject {
                    start,
                    mut depth,
                    mut in_string,
                    mut escaped,
                } => {
                    let bytes = self.buffer.as_bytes();
                    let mut closed_end = None;
                    while self.scan_pos < bytes.len() {
                        let b = bytes[self.scan_pos];
                        self.scan_pos += 1;
                        if in_string {
                            if escaped {
                                escaped = false;
                            } else if b == b'\\' {
                                escaped = true;
                            } else if b == b'"' {
                                in_string = false;
                            }
                            continue;
                        }
                        match b {
                            b'"' => in_string = true,
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    closed_end = Some(self.scan_pos);
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(end) = closed_end {
                        self.phase = ScanPhase::SeekingElement;
                        if let Ok(value) = serde_json::from_str::<Value>(&self.buffer[start..end])
                            && self.validator.validate_one(&value, caps).is_ok()
                        {
                            let seq = self.emitted_count;
                            self.emitted_count += 1;
                            ready.push((seq, value));
                        }
                    } else {
                        self.phase = ScanPhase::InObject {
                            start,
                            depth,
                            in_string,
                            escaped,
                        };
                        break;
                    }
                }
            }
        }

        ready
    }
}

/// Find the `[` opening the top-level `"envelopes"` array in a partial tool argument object.
fn find_envelopes_array_open(buf: &str) -> Option<usize> {
    let bytes = buf.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut last_key: Option<(usize, usize)> = None;

    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
                if depth == 1
                    && let Some((kstart, _)) = last_key
                {
                    last_key = Some((kstart, i - 1));
                }
            }
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                if depth == 1 {
                    last_key = Some((i, i));
                }
            }
            b'{' => {
                depth += 1;
                if depth > 1 {
                    last_key = None;
                }
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                last_key = None;
            }
            b'[' if depth == 1 => {
                if let Some((ks, ke)) = last_key
                    && buf.get(ks..ke) == Some("envelopes")
                {
                    return Some(i - 1);
                }
                last_key = None;
            }
            b':' | b' ' | b'\t' | b'\r' | b'\n' => {}
            _ => {
                last_key = None;
            }
        }
    }
    None
}

impl Agent {
    /// Enable, update, or disable A2UI response capability for this session.
    /// No-op when unchanged so unnegotiated sessions never touch their locked tool cache.
    pub async fn set_a2ui_capabilities(&mut self, capabilities: Option<A2uiSessionCapabilities>) {
        if self.a2ui_capabilities == capabilities {
            return;
        }
        self.a2ui_capabilities = capabilities.clone();
        self.registry.set_a2ui_capabilities(capabilities).await;
        self.invalidate_sdk_tools();
    }

    pub(super) async fn include_negotiated_a2ui_tool_def(
        &self,
        tools: &mut Vec<ToolDefinition>,
        allowed: Option<&HashSet<String>>,
        disabled: &HashSet<String>,
    ) {
        if self.a2ui_capabilities.is_none()
            || allowed.is_some_and(HashSet::is_empty)
            || self.registry.tool_is_disabled(disabled, A2UI_TOOL)
            || self.load_tiers.tier(A2UI_TOOL) == crate::config::LoadMode::Off
            || tools.iter().any(|tool| tool.name == A2UI_TOOL)
        {
            return;
        }
        if let Some(def) = self.registry.a2ui_tool_definition().await {
            tools.push(def);
            tools.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    pub(super) fn a2ui_tool_explicitly_allowed(&self, name: &str) -> bool {
        name == A2UI_TOOL
            && self.a2ui_capabilities.is_some()
            && self.load_tiers.tier(A2UI_TOOL) != crate::config::LoadMode::Off
            && self.allowed_tools.as_ref().is_none_or(|set| !set.is_empty())
            && !self.registry.tool_is_disabled(&self.disabled_tools, A2UI_TOOL)
    }

    pub(super) fn reset_a2ui_turn(&mut self) {
        self.a2ui_turn = TurnA2uiState::default();
    }

    pub(super) fn reset_a2ui_stream_attempt(&mut self) {
        self.a2ui_turn.incremental.clear();
    }

    pub(super) fn register_a2ui_stream_tool(&mut self, call_id: &str, name: &str) {
        if name == A2UI_TOOL && self.a2ui_capabilities.is_some() {
            self.a2ui_turn
                .incremental
                .insert(call_id.to_string(), IncrementalA2uiStreamer::new());
        }
    }

    pub(super) fn feed_a2ui_stream_delta(
        &mut self,
        call_id: Option<&str>,
        delta: &str,
        event_tx: &mpsc::UnboundedSender<ServerEvent>,
    ) {
        let Some(call_id) = call_id else {
            return;
        };
        let Some(caps) = self.a2ui_capabilities.as_ref() else {
            return;
        };
        let Some(streamer) = self.a2ui_turn.incremental.get_mut(call_id) else {
            return;
        };
        let ready = streamer.push_delta(delta, caps);
        if ready.is_empty() {
            return;
        }
        let mode = crate::config::config().tools.secrets_mode;
        for (sequence, mut envelope) in ready {
            crate::secrets::mask_json_for_history(&mut envelope, &self.session.id, mode);
            if event_tx
                .send(ServerEvent::A2uiEnvelope {
                    session_id: self.session.id.clone(),
                    tool_call_id: call_id.to_string(),
                    sequence,
                    envelope,
                    fallback_text: None,
                })
                .is_err()
            {
                crate::logging::warn("Failed to deliver streamed A2UI envelope to client");
            }
        }
    }

    pub(super) fn collect_a2ui_tool_output(
        &mut self,
        call_id: &str,
        tool_name: &str,
        output: &crate::tool::ToolOutput,
        event_tx: Option<&mpsc::UnboundedSender<ServerEvent>>,
    ) {
        let streamed_count = self
            .a2ui_turn
            .incremental
            .remove(call_id)
            .map_or(0usize, |s| s.emitted_count as usize);
        if tool_name != A2UI_TOOL {
            return;
        }
        let Some(payload) = output.metadata.as_ref().and_then(|m| m.get(A2UI_METADATA_KEY)) else {
            return;
        };
        let Some(raw_envelopes) = payload.get("envelopes").and_then(Value::as_array) else {
            return;
        };
        let mode = crate::config::config().tools.secrets_mode;
        let mut envelopes = raw_envelopes.clone();
        for env in &mut envelopes {
            crate::secrets::mask_json_for_history(env, &self.session.id, mode);
        }
        let fallback_text = payload
            .get("fallback_text")
            .and_then(Value::as_str)
            .map(|text| crate::secrets::mask_for_history(text, &self.session.id, mode));
        let final_response = payload.get("final_response").and_then(Value::as_bool).unwrap_or(true);

        if let Some(tx) = event_tx {
            for (idx, envelope) in envelopes.iter().enumerate().skip(streamed_count) {
                if tx
                    .send(ServerEvent::A2uiEnvelope {
                        session_id: self.session.id.clone(),
                        tool_call_id: call_id.to_string(),
                        sequence: idx as u32,
                        envelope: envelope.clone(),
                        fallback_text: (idx == 0).then(|| fallback_text.clone()).flatten(),
                    })
                    .is_err()
                {
                    crate::logging::warn("Failed to deliver finalized A2UI envelope to client");
                }
            }
        }

        self.a2ui_turn.round.push(A2uiEmission {
            envelopes,
            fallback_text,
            final_response,
        });
    }

    pub(super) fn note_a2ui_round_failure(&mut self) {
        self.a2ui_turn.any_round_error = true;
    }

    pub(super) fn finalize_a2ui_round(&mut self) -> Result<bool> {
        self.a2ui_turn.incremental.clear();
        let any_error = std::mem::take(&mut self.a2ui_turn.any_round_error);
        let round = std::mem::take(&mut self.a2ui_turn.round);
        if round.is_empty() {
            return Ok(false);
        }
        let wants_finish =
            round.iter().any(|e| e.final_response) && !any_error && self.soft_interrupt_queue.pending_len() == 0;
        self.a2ui_turn.deferred.extend(round);
        if !wants_finish {
            return Ok(false);
        }
        self.commit_a2ui_response(true)
    }

    pub(super) fn flush_deferred_a2ui_blocks(&mut self) -> Result<()> {
        if self.a2ui_turn.deferred.is_empty() {
            return Ok(());
        }
        self.commit_a2ui_response(false)?;
        Ok(())
    }

    fn commit_a2ui_response(&mut self, finish_turn: bool) -> Result<bool> {
        let emissions = std::mem::take(&mut self.a2ui_turn.deferred);
        if emissions.is_empty() {
            return Ok(false);
        }
        let mut blocks = Vec::with_capacity(emissions.len());
        let mut cards = Vec::new();
        for item in emissions {
            cards.push(render_a2ui_text_card(&item.envelopes, item.fallback_text.as_deref()));
            blocks.push(ContentBlock::A2ui {
                envelopes: item.envelopes,
                fallback_text: item.fallback_text,
            });
        }
        self.add_message(Role::Assistant, blocks);
        let final_summary = cards.join("\n");
        if finish_turn {
            self.turn_state.set_final_text(&final_summary);
            self.turn_state.finish(TurnOutcome::Stop)?;
            self.save_final_answer()?;
        } else {
            self.save_session()?;
        }
        Ok(finish_turn)
    }
}
