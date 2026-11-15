//! The OpenAI Responses wire: request body and stream events.
//!
//! `POST {base}/responses` is the shape the provider has always spoken, so this
//! module adds no translation of its own for the conversation or the tool
//! catalog: [`build_responses_request`] assembles the body from the canonical
//! history with the existing input and tool builders.
//!
//! The stream is the Chat-compatible Responses parser
//! ([`parse_openai_response_event`]) wrapped so the events leave in the order the
//! turn loop relies on. The inner parser reports a reasoning delta whenever one
//! arrives and an explicit start only for a reasoning item, never a close; the
//! wrapper opens the thinking block on the first delta, closes it before text, a
//! tool call, usage or the end of the message, and reports a truncated body at
//! EOF. Native reasoning items are history-only and are not replayed here.

use crate::chat::transport::snippet;
use crate::stream::{StreamingToolCallState, parse_openai_response_event};
use crate::wire::sse::WireParser;
use horon_agent_message_types::degradation::{DegradationKind, DegradationReceipt};
use horon_agent_message_types::{Message, StreamEvent, ToolDefinition};
use horon_agent_provider_core::retry_after::retry_after_body_seconds;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

/// Names this wire in every message it reports.
const LABEL: &str = "OpenAI Responses";

/// Event kinds that fail the whole response rather than one frame of it.
const ERROR_KINDS: [&str; 3] = ["response.failed", "response.error", "error"];

/// The transport's notice that it switched from websockets to HTTPS.
///
/// Same text as `WEBSOCKET_FALLBACK_NOTICE` in `crate::stream`, which
/// `transport_frame_event` turns into "no event" for the native stream. That
/// module is out of this plan's write scope, so the string is repeated here and
/// a silent frame stays silent on this wire too.
const WEBSOCKET_FALLBACK_NOTICE: &str = "falling back from websockets to https transport";

/// Build a streaming Responses request body.
///
/// `instructions` carries the system prompt and is omitted when it is empty.
/// A catalog that the OpenAI dialect had to narrow (or a duplicate tool name,
/// which the endpoint cannot carry twice) is reported through `sink` when one is
/// given and sent identically either way.
pub fn build_responses_request(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    system: &str,
    reasoning_effort: Option<&str>,
    mut sink: Option<&mut Vec<DegradationReceipt>>,
) -> Value {
    let mut body = json!({
        "model": model,
        "input": crate::request::build_responses_input(messages),
        "stream": true,
        // Reasoning items are replayed from history instead of being stored on
        // the account, so a request never depends on earlier account state.
        "store": false,
    });
    if !system.trim().is_empty() {
        body["instructions"] = json!(system);
    }
    let declared = crate::request::build_tools_reporting(tools, sink.as_deref_mut());
    let declared = dedupe_tools(declared, sink);
    if !declared.is_empty() {
        body["tools"] = Value::Array(declared);
        body["tool_choice"] = json!("auto");
    }
    if let Some(effort) = reasoning_effort.filter(|effort| !effort.trim().is_empty()) {
        body["reasoning"] = json!({ "effort": effort });
    }
    body
}

/// Drop a second definition of an already declared name.
///
/// The endpoint keys tools by name, so only the first definition is sent; each
/// later one is a real loss of information and is reported when a sink exists.
fn dedupe_tools(declared: Vec<Value>, mut sink: Option<&mut Vec<DegradationReceipt>>) -> Vec<Value> {
    let mut seen: HashSet<String> = HashSet::with_capacity(declared.len());
    let mut tools = Vec::with_capacity(declared.len());
    for tool in declared {
        let Some(name) = tool.get("name").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        if !seen.insert(name.clone()) {
            if let Some(receipts) = sink.as_deref_mut() {
                receipts.push(DegradationReceipt::new(
                    DegradationKind::DuplicateToolDropped,
                    name.clone(),
                    name,
                ));
            }
            continue;
        }
        tools.push(tool);
    }
    tools
}

/// Text of a server error value, in the shape the provider sent it.
///
/// A bare string is used as it is; an object names its kind through `type` (or
/// Gemini's `status`) when it has one and always through `message`; anything
/// else is rendered as JSON so no detail is silently dropped.
fn error_text(error: &Value) -> String {
    if let Some(text) = error.as_str() {
        return text.to_string();
    }
    let message = error.get("message").and_then(Value::as_str);
    let kind = error
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| error.get("status").and_then(Value::as_str));
    match (kind, message) {
        (Some(kind), Some(message)) => format!("{kind}: {message}"),
        (None, Some(message)) => message.to_string(),
        _ => error.to_string(),
    }
}

/// Whether `haystack` contains `needle`, ignoring ASCII case, without allocating.
fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > haystack.len() {
        return needle.is_empty();
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// The terminal error a failed response payload means, if this payload is one.
fn server_error_event(payload: &str) -> Option<StreamEvent> {
    // A payload that is not JSON cannot be a server error; the malformed-chunk
    // path reports it instead.
    let parsed: Value = match serde_json::from_str(payload) {
        Ok(parsed) => parsed,
        Err(_) => return None,
    };
    let kind = parsed.get("type").and_then(Value::as_str)?;
    if !ERROR_KINDS.contains(&kind) {
        return None;
    }
    let error = parsed
        .get("error")
        .or_else(|| parsed.get("response").and_then(|response| response.get("error")))
        .or_else(|| parsed.get("response"))
        .unwrap_or(&parsed);
    Some(StreamEvent::Error {
        message: format!("{LABEL} stream returned an error: {}", error_text(error)),
        retry_after_secs: retry_after_body_seconds(payload),
    })
}

/// Translates Responses SSE payloads into the shared event sequence.
#[derive(Default)]
pub struct ResponsesParser {
    /// Set once a text delta arrived, so a completed message knows the text was
    /// already streamed instead of repeating it.
    saw_text_delta: bool,
    /// Whether reasoning text was streamed live rather than replayed from a
    /// finished reasoning item.
    saw_thinking_delta: bool,
    streaming_tool_calls: HashMap<String, StreamingToolCallState>,
    completed_tool_items: HashSet<String>,
    thinking_open: bool,
    thinking_started: Option<Instant>,
    text_open: bool,
    finished: bool,
}

impl ResponsesParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_thinking(&mut self, out: &mut VecDeque<StreamEvent>) {
        if self.thinking_open {
            return;
        }
        self.thinking_open = true;
        self.thinking_started = Some(Instant::now());
        out.push_back(StreamEvent::ThinkingStart);
    }

    fn close_thinking(&mut self, out: &mut VecDeque<StreamEvent>) {
        if !self.thinking_open {
            return;
        }
        self.thinking_open = false;
        let duration_secs = self
            .thinking_started
            .take()
            .map(|started| started.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        out.push_back(StreamEvent::ThinkingEnd);
        out.push_back(StreamEvent::ThinkingDone { duration_secs });
    }

    fn close_text(&mut self, out: &mut VecDeque<StreamEvent>) {
        if self.text_open {
            self.text_open = false;
            out.push_back(StreamEvent::TextDone);
        }
    }

    fn close_all(&mut self, out: &mut VecDeque<StreamEvent>) {
        self.close_thinking(out);
        self.close_text(out);
    }

    /// Give the inner parser's events the shape the turn loop expects.
    fn translate(&mut self, events: VecDeque<StreamEvent>, out: &mut VecDeque<StreamEvent>) {
        for event in events {
            match event {
                // History-only: the reasoning item is replayed by the request
                // builder, never rendered as stream content.
                StreamEvent::OpenAIReasoning { .. } => {}
                StreamEvent::ThinkingStart => self.ensure_thinking(out),
                StreamEvent::ThinkingDelta(delta) => {
                    self.ensure_thinking(out);
                    out.push_back(StreamEvent::ThinkingDelta(delta));
                }
                StreamEvent::ThinkingEnd | StreamEvent::ThinkingDone { .. } => self.close_thinking(out),
                StreamEvent::TextDelta(text) => {
                    self.close_thinking(out);
                    self.text_open = true;
                    out.push_back(StreamEvent::TextDelta(text));
                }
                StreamEvent::TextDone => self.close_text(out),
                StreamEvent::ToolUseStart { id, name } => {
                    self.close_all(out);
                    out.push_back(StreamEvent::ToolUseStart { id, name });
                }
                StreamEvent::TokenUsage {
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                } => {
                    self.close_all(out);
                    out.push_back(StreamEvent::TokenUsage {
                        input_tokens,
                        output_tokens,
                        cache_read_input_tokens,
                        cache_creation_input_tokens,
                    });
                }
                StreamEvent::MessageEnd { stop_reason } => {
                    self.close_all(out);
                    out.push_back(StreamEvent::MessageEnd { stop_reason });
                    self.finished = true;
                }
                StreamEvent::Error {
                    message,
                    retry_after_secs,
                } => {
                    self.close_all(out);
                    out.push_back(StreamEvent::Error {
                        message,
                        retry_after_secs,
                    });
                    // The consumer ends the read on `StreamEvent::Error`: it
                    // either retries the whole response or stops the turn
                    // (app-core `turn_loops`/`turn_streaming_mpsc`). Anything
                    // this wire emitted afterwards, including its own
                    // truncation error, could only add a second failure.
                    self.finished = true;
                    break;
                }
                other => out.push_back(other),
            }
        }
    }
}

impl WireParser for ResponsesParser {
    fn parse(&mut self, data: &str, out: &mut VecDeque<StreamEvent>) {
        let payload = data.trim();
        if payload.is_empty() || self.finished {
            return;
        }
        let mut events = VecDeque::new();
        if let Some(first) = parse_openai_response_event(
            payload,
            &mut self.saw_text_delta,
            &mut self.saw_thinking_delta,
            &mut self.streaming_tool_calls,
            &mut self.completed_tool_items,
            &mut events,
        ) {
            events.push_front(first);
        }
        // A failed response carries the provider's own error object, which this
        // wire renders in its own words. Every other error the inner parser
        // reports is already terminal through `translate`, which ends the read
        // because the consumer does the same on `StreamEvent::Error`.
        if events.iter().any(|event| matches!(event, StreamEvent::Error { .. }))
            && let Some(error) = server_error_event(payload)
        {
            self.close_all(out);
            out.push_back(error);
            self.finished = true;
            return;
        }
        if events.is_empty()
            && let Err(error) = serde_json::from_str::<serde::de::IgnoredAny>(payload)
        {
            if contains_ascii_case_insensitive(payload, WEBSOCKET_FALLBACK_NOTICE) {
                // The transport announced a switch to HTTPS. The notice is plain
                // text, so it can only reach this branch, and the native stream
                // drops it silently too. Checked after the JSON test so a normal
                // frame never pays for the scan.
                return;
            }
            // One unreadable frame is a bad frame, not a broken response: report
            // it and keep reading, so the rest of the answer still arrives. This
            // is the one error this wrapper reports without ending the stream.
            out.push_back(StreamEvent::Error {
                message: format!(
                    "{LABEL} stream sent a malformed JSON chunk: {error}; payload={}",
                    snippet(payload, 400)
                ),
                retry_after_secs: None,
            });
            return;
        }
        self.translate(events, out);
    }

    fn finish(&mut self, out: &mut VecDeque<StreamEvent>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.close_all(out);
        out.push_back(StreamEvent::Error {
            message: format!("{LABEL} stream ended before the response finished (no response.completed event)"),
            retry_after_secs: None,
        });
    }
}

#[cfg(test)]
#[path = "responses_tests.rs"]
mod responses_tests;
