use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
pub use bytes::Bytes;
use futures::Stream;
use horon_agent_message_types::{StreamEvent, sanitize_tool_id};
use serde::Deserialize;
use serde_json::Value;
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context as TaskContext, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

const WEBSOCKET_FALLBACK_NOTICE: &str = "falling back from websockets to https transport";
/// The transport substitutes this notice for a Responses event when the connection drops.
const STREAM_DISCONNECTED_NOTICE: &str = "stream disconnected before completion";
/// Consumed bytes the framer must hold before it compacts. Below this, compacting would
/// move more bytes than it saves.
const SSE_COMPACT_MIN_BYTES: usize = 4096;
static FALLBACK_TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(1);
static RECOVERED_TEXT_WRAPPED_TOOL_CALLS: AtomicU64 = AtomicU64::new(0);
static NORMALIZED_NULL_TOOL_ARGUMENTS: AtomicU64 = AtomicU64::new(0);

fn truncated_stream_payload_context(data: &str) -> String {
    horon_agent_core::util::truncate_str(&data.trim().replace("\n", "\\n"), 240).to_string()
}

thread_local! {
    /// JSON parses of SSE payloads on this thread. The stream parses each payload once;
    /// tests read this counter to prove that, and to show the fallback path's extra parse.
    static SSE_JSON_PARSES: Cell<u64> = const { Cell::new(0) };
}

fn count_sse_json_parse() {
    SSE_JSON_PARSES.with(|parses| parses.set(parses.get() + 1));
}

/// Total SSE payload parses on this thread.
#[cfg(test)]
pub(crate) fn sse_json_parse_count() -> u64 {
    SSE_JSON_PARSES.with(Cell::get)
}

fn parse_typed_sse_event(data: &str) -> Result<ResponseSseEvent, serde_json::Error> {
    count_sse_json_parse();
    serde_json::from_str(data)
}

/// A `serde_json::Value` parse. Only the fallback path (a payload the typed parse already
/// rejected) and text-wrapped tool-call recovery use this.
fn parse_json_value(data: &str) -> Result<Value, serde_json::Error> {
    count_sse_json_parse();
    serde_json::from_str(data)
}

/// Whether a `type` names a structured Responses event.
fn is_structured_event_kind(kind: &str) -> bool {
    kind.starts_with("response.") || kind == "error"
}

/// Whether `data` is a structured Responses event, judged from its JSON `type` alone.
///
/// Only the fallback path calls this, where the typed parse has already failed and so
/// cannot say what kind the payload claimed to be.
fn is_structured_response_event(data: &str) -> bool {
    let Ok(value) = parse_json_value(data) else {
        return false;
    };
    let Some(kind) = value.get("type").and_then(|kind| kind.as_str()) else {
        return false;
    };
    is_structured_event_kind(kind)
}

/// What a payload that is not a structured Responses event means.
///
/// The proxy injects the websocket fallback notice as a plain-text control frame, and the
/// transport reports a dropped connection the same way. Returns `None` when the payload is
/// neither, `Some(None)` for the notice (logged and dropped), and `Some(Some(event))` for
/// the disconnect, which surfaces as an error event.
fn transport_frame_event(data: &str) -> Option<Option<StreamEvent>> {
    // A legitimate `response.*`/`error` event can contain either phrase in model output or
    // tool-call arguments, so callers only reach here for payloads that are not one.
    let lower = data.to_lowercase();
    if lower.contains(WEBSOCKET_FALLBACK_NOTICE) {
        horon_agent_logging::warn(&format!("OpenAI stream transport notice: {}", data.trim()));
        return Some(None);
    }
    if lower.contains(STREAM_DISCONNECTED_NOTICE) {
        return Some(Some(StreamEvent::Error {
            message: data.to_string(),
            retry_after_secs: None,
        }));
    }
    None
}

fn extract_error_with_retry(response: &Option<Value>, top_level_error: &Option<Value>) -> (String, Option<u64>) {
    let error = response
        .as_ref()
        .and_then(|r| r.get("error"))
        .or(top_level_error.as_ref());

    let error = match error {
        Some(e) => e,
        None => {
            if let Some(resp) = response.as_ref()
                && let Some(msg) = resp
                    .get("status_message")
                    .or_else(|| resp.get("message"))
                    .and_then(|v| v.as_str())
            {
                return (msg.to_string(), None);
            }
            return ("OpenAI response stream error (no error details)".to_string(), None);
        }
    };

    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("OpenAI response stream error (unknown)")
        .to_string();
    let error_type = error.get("type").and_then(|v| v.as_str());
    let code = error.get("code").and_then(|v| v.as_str());

    let message_lower = message.to_lowercase();
    let message = match (error_type, code) {
        (Some(error_type), Some(code))
            if !message_lower.contains(&error_type.to_lowercase()) && !message_lower.contains(&code.to_lowercase()) =>
        {
            format!("{} ({}): {}", error_type, code, message)
        }
        (Some(error_type), _) if !message_lower.contains(&error_type.to_lowercase()) => {
            format!("{}: {}", error_type, message)
        }
        (_, Some(code)) if !message_lower.contains(&code.to_lowercase()) => {
            format!("{}: {}", code, message)
        }
        _ => message,
    };

    let retry_after = error.get("retry_after").and_then(|v| v.as_u64()).or_else(|| {
        response
            .as_ref()
            .and_then(|r| r.get("retry_after"))
            .and_then(|v| v.as_u64())
    });

    (message, retry_after)
}
pub fn parse_text_wrapped_tool_call(text: &str) -> Option<(String, String, String, String)> {
    let marker = "to=functions.";
    let marker_idx = text.find(marker)?;
    let after_marker = &text[marker_idx + marker.len()..];

    let mut tool_name_end = 0usize;
    for (idx, ch) in after_marker.char_indices() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            tool_name_end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if tool_name_end == 0 {
        return None;
    }

    let tool_name = after_marker[..tool_name_end].to_string();
    let remaining = &after_marker[tool_name_end..];
    let mut fallback: Option<(String, String, String, String)> = None;
    for (brace_idx, ch) in remaining.char_indices() {
        if ch != '{' {
            continue;
        }
        let slice = &remaining[brace_idx..];
        let mut stream = serde_json::Deserializer::from_str(slice).into_iter::<Value>();
        let parsed = match stream.next() {
            Some(Ok(value)) => value,
            Some(Err(_)) => continue,
            None => continue,
        };
        let consumed = stream.byte_offset();
        if !parsed.is_object() {
            continue;
        }

        let prefix = text[..marker_idx].trim_end().to_string();
        let suffix = remaining[brace_idx + consumed..].trim().to_string();
        let args = serde_json::to_string(&parsed).ok()?;
        if suffix.is_empty() {
            return Some((prefix, tool_name.clone(), args, suffix));
        }
        if fallback.is_none() {
            fallback = Some((prefix, tool_name.clone(), args, suffix));
        }
    }

    fallback
}

fn stream_text_or_recovered_tool_call(text: &str, pending: &mut VecDeque<StreamEvent>) -> Option<StreamEvent> {
    if text.is_empty() {
        return None;
    }

    if let Some((prefix, tool_name, arguments, suffix)) = parse_text_wrapped_tool_call(text) {
        let total = RECOVERED_TEXT_WRAPPED_TOOL_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
        horon_agent_logging::warn(&format!(
            "[openai] Recovered text-wrapped tool call for '{}' (total={})",
            tool_name, total
        ));
        let suffix = sanitize_recovered_tool_suffix(&suffix);
        if !prefix.is_empty() {
            pending.push_back(StreamEvent::TextDelta(prefix));
        }
        let id = format!(
            "fallback_text_call_{}",
            FALLBACK_TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        pending.push_back(StreamEvent::ToolUseStart {
            id: id.clone(),
            name: tool_name,
        });
        pending.push_back(StreamEvent::ToolInputDeltaFor {
            id: id.clone(),
            delta: arguments,
        });
        pending.push_back(StreamEvent::ToolUseEndFor { id });
        if !suffix.is_empty() {
            pending.push_back(StreamEvent::TextDelta(suffix));
        }
        return pending.pop_front();
    }

    Some(StreamEvent::TextDelta(text.to_string()))
}

fn sanitize_recovered_tool_suffix(suffix: &str) -> String {
    let trimmed = suffix.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let normalized = trimmed.trim_start_matches('"');

    if normalized.starts_with(",\"item_id\"")
        || normalized.starts_with(",\"output_index\"")
        || normalized.starts_with(",\"sequence_number\"")
        || normalized.starts_with(",\"call_id\"")
        || normalized.starts_with(",\"type\":\"response.")
        || (normalized.starts_with(',')
            && normalized.contains("\"item_id\"")
            && (normalized.contains("\"output_index\"") || normalized.contains("\"sequence_number\"")))
    {
        return String::new();
    }

    suffix.to_string()
}

#[derive(Deserialize, Debug)]
struct ResponseSseEvent {
    #[serde(rename = "type")]
    kind: String,
    item: Option<Value>,
    delta: Option<String>,
    item_id: Option<String>,
    output_index: Option<u64>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
    input: Option<String>,
    response: Option<Value>,
    error: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct StreamingToolCallState {
    identities: HashSet<String>,
    fallback_id: String,
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
    emitted_id: Option<String>,
    started: bool,
    emitted_arguments: usize,
    complete: bool,
    order: usize,
}

fn normalize_openai_tool_arguments(raw_arguments: String) -> String {
    let trimmed = raw_arguments.trim();
    if trimmed.is_empty() || trimmed == "null" {
        let total = NORMALIZED_NULL_TOOL_ARGUMENTS.fetch_add(1, Ordering::Relaxed) + 1;
        horon_agent_logging::warn(&format!(
            "[openai] Normalized empty/null tool arguments to empty object (total={})",
            total
        ));
        "{}".to_string()
    } else {
        raw_arguments
    }
}

fn streaming_tool_item_id(item: &Value) -> Option<String> {
    item.get("id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.trim().is_empty())
        .or_else(|| {
            item.get("item_id")
                .and_then(|v| v.as_str())
                .filter(|id| !id.trim().is_empty())
        })
        .map(|id| id.to_string())
}

fn tool_call_state<'a>(
    calls: &'a mut HashMap<String, StreamingToolCallState>,
    completed: &mut HashSet<String>,
    output_index: Option<u64>,
    item_id: Option<&str>,
    call_id: Option<&str>,
) -> Option<&'a mut StreamingToolCallState> {
    let item_id = item_id.filter(|id| !id.trim().is_empty());
    let call_id = call_id.filter(|id| !id.trim().is_empty());
    // Some Responses-compatible providers regenerate item IDs on every event.
    // output_index is stable even on argument deltas without a call_id. Keep
    // aliases so item-only/call-only snapshots still find the same call.
    // Keep raw item IDs for callers of the public parser's completed-item set,
    // and namespace synthetic identities away from ordinary provider IDs.
    let identities: Vec<String> = output_index
        .map(|index| format!("\0output:{index}"))
        .into_iter()
        .chain(item_id.map(str::to_string))
        .chain(call_id.map(|id| format!("\0call:{id}")))
        .collect();
    if identities.iter().any(|id| completed.contains(id)) {
        completed.extend(identities);
        return None;
    }
    let key = identities
        .iter()
        .find_map(|identity| {
            calls
                .iter()
                .find_map(|(key, state)| state.identities.contains(identity).then(|| key.clone()))
        })
        .or_else(|| identities.first().cloned())?;
    let order = calls.values().map(|state| state.order).max().unwrap_or(0) + 1;
    let state = calls.entry(key).or_insert_with(|| StreamingToolCallState {
        order,
        fallback_id: item_id.or(call_id).map(str::to_string).unwrap_or_else(|| {
            format!(
                "fallback_call_{}",
                FALLBACK_TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
            )
        }),
        ..Default::default()
    });
    state.identities.extend(identities);
    Some(state)
}

fn update_tool_call_from_item(state: &mut StreamingToolCallState, item: &Value, complete: bool) -> bool {
    if let Some(id) = item
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
    {
        state.call_id = Some(id.to_string());
    }
    if let Some(name) = item.get("name").and_then(Value::as_str) {
        state.name = Some(name.to_string());
    }
    let arguments = item
        .get("arguments")
        .or_else(|| item.get("input"))
        .map(|value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string()));
    if complete {
        complete_tool_arguments(state, arguments)
    } else {
        if state.arguments.is_empty() {
            state.arguments = arguments.unwrap_or_default();
        }
        true
    }
}

fn inconsistent_tool_arguments() -> Option<StreamEvent> {
    Some(StreamEvent::Error {
        message: "OpenAI tool argument snapshot differs from streamed arguments".to_string(),
        retry_after_secs: None,
    })
}

fn stream_tool_calls(
    calls: &mut HashMap<String, StreamingToolCallState>,
    completed: &mut HashSet<String>,
    pending: &mut VecDeque<StreamEvent>,
) -> Option<StreamEvent> {
    // Keyed events let every named call advance independently, even when an
    // earlier call has not supplied any arguments yet.
    let mut ready: Vec<_> = calls
        .iter()
        .filter(|(_, state)| state.started || state.name.as_ref().is_some_and(|name| !name.is_empty()))
        .map(|(id, state)| (state.order, id.clone()))
        .collect();
    ready.sort_by_key(|(order, _)| *order);
    for (_, item_id) in ready {
        let state = calls.get_mut(&item_id).expect("selected tool call");
        let id = state
            .emitted_id
            .get_or_insert_with(|| {
                sanitize_tool_id(
                    state
                        .call_id
                        .as_deref()
                        .filter(|id| !id.trim().is_empty())
                        .unwrap_or(&state.fallback_id),
                )
            })
            .clone();
        if !state.started {
            pending.push_back(StreamEvent::ToolUseStart {
                id: id.clone(),
                name: state.name.clone().expect("named tool call"),
            });
            state.started = true;
        }
        if state.complete {
            state.arguments = normalize_openai_tool_arguments(std::mem::take(&mut state.arguments));
        }
        // Hold only a possible empty/null value until it can be normalized.
        // Ordinary JSON fragments flow through without waiting for valid JSON.
        if state.complete || !"null".starts_with(state.arguments.trim()) {
            let delta = &state.arguments[state.emitted_arguments..];
            if !delta.is_empty() {
                pending.push_back(StreamEvent::ToolInputDeltaFor {
                    id: id.clone(),
                    delta: delta.to_string(),
                });
                state.emitted_arguments = state.arguments.len();
            }
        }
        if !state.complete {
            continue;
        }
        pending.push_back(StreamEvent::ToolUseEndFor { id });
        let state = calls.remove(&item_id).expect("completed tool call");
        completed.extend(state.identities);
    }
    pending.pop_front()
}

fn arguments_match_as_json(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    matches!(
        (serde_json::from_str::<Value>(left), serde_json::from_str::<Value>(right)),
        (Ok(left), Ok(right)) if left == right
    )
}

fn complete_tool_arguments(state: &mut StreamingToolCallState, arguments: Option<String>) -> bool {
    if let Some(arguments) = arguments {
        // A done snapshot normally repeats the deltas. Only its unseen suffix
        // may be appended, otherwise the consumer would parse duplicated JSON.
        // Some gateways instead re-serialize the arguments, so the snapshot is
        // a different string for the same JSON value (for example compacted
        // whitespace). The consumer already holds that value, so the
        // accumulated text is kept and nothing further is emitted. A snapshot
        // whose value disagrees is still rejected, which keeps a genuinely
        // inconsistent snapshot from corrupting tool input.
        if arguments.starts_with(&state.arguments[..state.emitted_arguments]) {
            state.arguments = arguments;
        } else if !arguments_match_as_json(&arguments, &state.arguments) {
            return false;
        }
    }
    state.complete = true;
    true
}

/// Join the `data:` lines of one SSE block, or None when it carries no data line.
fn collect_sse_data(block: &str) -> Option<String> {
    let mut data: Option<String> = None;
    for line in block.lines() {
        let Some(line_data) = horon_agent_core::util::sse_data_line(line) else {
            continue;
        };
        match &mut data {
            Some(data) => {
                data.push('\n');
                data.push_str(line_data);
            }
            None => data = Some(line_data.to_string()),
        }
    }
    data
}

/// Frames `data:` payloads out of one growable buffer.
///
/// `cursor` marks how much of `buffer` has already been consumed, so framing searches only
/// the unconsumed suffix and one event costs one copy of its own payload instead of a copy
/// of the whole remaining buffer. The consumed prefix is compacted away only once it reaches
/// [`SSE_COMPACT_MIN_BYTES`] and is at least as large as the live remainder, which keeps the
/// amortized copy cost under one byte per consumed byte whatever the stream's length.
#[derive(Debug, Default)]
pub struct SseFrameBuffer {
    buffer: String,
    cursor: usize,
    compacted_bytes: u64,
}

impl SseFrameBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_str(&mut self, text: &str) {
        self.buffer.push_str(text);
    }

    /// Take the next complete event's data out of the buffer, choosing the EARLIEST
    /// blank-line boundary, so an LF-terminated event that precedes a CRLF-terminated one is
    /// not reordered.
    pub fn take_next(&mut self) -> Option<String> {
        loop {
            let start = self.cursor;
            let (end, next) = {
                let pending = &self.buffer[start..];
                match (pending.find("\n\n"), pending.find("\r\n\r\n")) {
                    (Some(lf), Some(crlf)) if crlf < lf => (crlf, crlf + 4),
                    (Some(lf), _) => (lf, lf + 2),
                    (None, Some(crlf)) => (crlf, crlf + 4),
                    (None, None) => {
                        self.compact_if_worthwhile();
                        return None;
                    }
                }
            };
            self.cursor = start + next;
            if let Some(data) = collect_sse_data(&self.buffer[start..start + end]) {
                return Some(data);
            }
        }
    }

    /// Flush a trailing event that arrived without a final blank line (end of stream).
    pub fn flush(&mut self) -> Option<String> {
        if self.buffer[self.cursor..].trim().is_empty() {
            self.buffer.clear();
            self.cursor = 0;
            return None;
        }
        let data = collect_sse_data(&self.buffer[self.cursor..]);
        self.buffer.clear();
        self.cursor = 0;
        data
    }

    /// The bytes not yet framed out. A stream that consumed every event leaves this empty,
    /// which is what keeps one event's bytes from leaking into the next.
    pub fn pending(&self) -> &str {
        &self.buffer[self.cursor..]
    }

    /// Bytes moved while compacting the buffer. A regression test asserts this stays
    /// proportional to the input instead of growing with the stream's length.
    pub fn compacted_bytes(&self) -> u64 {
        self.compacted_bytes
    }

    fn compact_if_worthwhile(&mut self) {
        let live = self.buffer.len() - self.cursor;
        if self.cursor < SSE_COMPACT_MIN_BYTES || self.cursor < live {
            return;
        }
        self.buffer.drain(..self.cursor);
        self.compacted_bytes += live as u64;
        self.cursor = 0;
    }

    /// Drop the consumed prefix unconditionally. The plain-`String` entry points below hand
    /// the buffer back to their caller, which has no cursor of its own to keep.
    fn compact_all(&mut self) {
        let live = self.buffer.len() - self.cursor;
        if self.cursor == 0 {
            return;
        }
        self.buffer.drain(..self.cursor);
        self.compacted_bytes += live as u64;
        self.cursor = 0;
    }
}

/// Frame one event from a caller-owned buffer.
///
/// Kept for callers that hold a plain `String`. A stream should own an [`SseFrameBuffer`]
/// instead: this entry point returns the buffer after every event, so it cannot keep the
/// read cursor that stops the tail from being copied.
pub fn take_next_sse_data(buffer: &mut String) -> Option<String> {
    let mut framer = SseFrameBuffer::new();
    std::mem::swap(&mut framer.buffer, buffer);
    let data = framer.take_next();
    framer.compact_all();
    std::mem::swap(&mut framer.buffer, buffer);
    data
}

/// Flush a trailing event out of a caller-owned buffer (end of stream).
pub fn flush_sse_data(buffer: &mut String) -> Option<String> {
    let mut framer = SseFrameBuffer::new();
    std::mem::swap(&mut framer.buffer, buffer);
    let data = framer.flush();
    std::mem::swap(&mut framer.buffer, buffer);
    data
}

/// The parse-failure path: a payload the typed parse rejected is either a transport control
/// frame or a malformed event. Re-reading it as a `serde_json::Value` here is deliberate and
/// cheap: it runs only after the typed parse already failed, so a normal event still parses
/// exactly once.
fn unparsable_sse_payload(data: &str, error: serde_json::Error) -> Option<StreamEvent> {
    if !is_structured_response_event(data)
        && let Some(outcome) = transport_frame_event(data)
    {
        return outcome;
    }
    horon_agent_logging::warn(&format!(
        "OpenAI SSE JSON parse failed: {} payload={}",
        error,
        truncated_stream_payload_context(data)
    ));
    None
}

pub fn parse_openai_response_event(
    data: &str,
    saw_text_delta: &mut bool,
    saw_thinking_delta: &mut bool,
    streaming_tool_calls: &mut HashMap<String, StreamingToolCallState>,
    completed_tool_items: &mut HashSet<String>,
    pending: &mut VecDeque<StreamEvent>,
) -> Option<StreamEvent> {
    if data == "[DONE]" {
        return Some(StreamEvent::MessageEnd { stop_reason: None });
    }

    // One parse per payload: the typed parse decides both whether this is a structured
    // event and what it says, so the normal path never reads the payload a second time.
    // Only a payload it rejects falls back to the transport-notice scan, where the payload
    // is re-read to learn what kind it claimed to be.
    let event: ResponseSseEvent = match parse_typed_sse_event(data) {
        Ok(parsed) => parsed,
        Err(error) => return unparsable_sse_payload(data, error),
    };

    if !is_structured_event_kind(&event.kind)
        && let Some(outcome) = transport_frame_event(data)
    {
        return outcome;
    }

    match event.kind.as_str() {
        "response.output_text.delta" => {
            if let Some(delta) = event.delta {
                *saw_text_delta = true;
                return stream_text_or_recovered_tool_call(&delta, pending);
            }
        }
        "response.reasoning.delta" | "response.reasoning_summary_text.delta" => {
            if let Some(delta) = event.delta {
                *saw_thinking_delta = true;
                return Some(StreamEvent::ThinkingDelta(delta));
            }
        }
        "response.reasoning.done" | "response.output_item.added" => {
            if let Some(item) = &event.item {
                if item.get("type").and_then(|v| v.as_str()) == Some("reasoning") {
                    return Some(StreamEvent::ThinkingStart);
                }
                if matches!(
                    item.get("type").and_then(|v| v.as_str()),
                    Some("function_call") | Some("custom_tool_call")
                ) {
                    let state = tool_call_state(
                        streaming_tool_calls,
                        completed_tool_items,
                        event.output_index,
                        streaming_tool_item_id(item).as_deref(),
                        item.get("call_id").and_then(Value::as_str),
                    )?;
                    update_tool_call_from_item(state, item, false);
                    return stream_tool_calls(streaming_tool_calls, completed_tool_items, pending);
                }
            }
        }
        "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
            let state = tool_call_state(
                streaming_tool_calls,
                completed_tool_items,
                event.output_index,
                event.item_id.as_deref(),
                event.call_id.as_deref(),
            )?;
            if let Some(call_id) = event.call_id.filter(|id| !id.trim().is_empty()) {
                state.call_id = Some(call_id);
            }
            if let Some(name) = event.name {
                state.name = Some(name);
            }
            if let Some(delta) = event.delta {
                state.arguments.push_str(&delta);
            }
            return stream_tool_calls(streaming_tool_calls, completed_tool_items, pending);
        }
        "response.function_call_arguments.done" | "response.custom_tool_call_input.done" => {
            let state = tool_call_state(
                streaming_tool_calls,
                completed_tool_items,
                event.output_index,
                event.item_id.as_deref(),
                event.call_id.as_deref(),
            )?;
            if let Some(call_id) = event.call_id.filter(|id| !id.trim().is_empty()) {
                state.call_id = Some(call_id);
            }
            if let Some(name) = event.name {
                state.name = Some(name);
            }
            if !complete_tool_arguments(state, event.arguments.or(event.input)) {
                return inconsistent_tool_arguments();
            }
            return stream_tool_calls(streaming_tool_calls, completed_tool_items, pending);
        }
        "response.output_item.done" => {
            if let Some(item) = event.item {
                if matches!(
                    item.get("type").and_then(|v| v.as_str()),
                    Some("function_call") | Some("custom_tool_call")
                ) && (event.output_index.is_some()
                    || streaming_tool_item_id(&item).is_some()
                    || item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| !id.trim().is_empty()))
                {
                    let state = tool_call_state(
                        streaming_tool_calls,
                        completed_tool_items,
                        event.output_index,
                        streaming_tool_item_id(&item).as_deref(),
                        item.get("call_id").and_then(Value::as_str),
                    )?;
                    if !update_tool_call_from_item(state, &item, true) {
                        return inconsistent_tool_arguments();
                    }
                    return stream_tool_calls(streaming_tool_calls, completed_tool_items, pending);
                }
                let is_message = item.get("type").and_then(Value::as_str) == Some("message");
                let first = handle_openai_output_item(item, saw_text_delta, saw_thinking_delta, pending);
                if is_message {
                    // output_item.done, not reasoning or response.completed, is
                    // the actual assistant-message boundary. Preserve fallback
                    // text before the marker and reset deduplication per message.
                    pending.push_back(StreamEvent::TextDone);
                    *saw_text_delta = false;
                }
                return first.or_else(|| pending.pop_front());
            }
        }
        "response.incomplete" => {
            let stop_reason = event
                .response
                .as_ref()
                .and_then(extract_stop_reason_from_response)
                .or_else(|| Some("incomplete".to_string()));
            if let Some(response) = event.response
                && let Some(usage_event) = extract_usage_from_response(&response)
            {
                pending.push_back(usage_event);
            }
            pending.push_back(StreamEvent::MessageEnd { stop_reason });
            return pending.pop_front();
        }
        "response.completed" => {
            let stop_reason = event.response.as_ref().and_then(extract_stop_reason_from_response);
            if let Some(response) = event.response
                && let Some(usage_event) = extract_usage_from_response(&response)
            {
                pending.push_back(usage_event);
            }
            pending.push_back(StreamEvent::MessageEnd { stop_reason });
            return pending.pop_front();
        }
        "response.failed" | "response.error" | "error" => {
            horon_agent_logging::warn(&format!(
                "OpenAI stream error event (type={}): response={:?}, error={:?}",
                event.kind, event.response, event.error
            ));
            let (message, retry_after_secs) = extract_error_with_retry(&event.response, &event.error);
            return Some(StreamEvent::Error {
                message,
                retry_after_secs,
            });
        }
        _ => {}
    }

    None
}

fn extract_last_assistant_message_phase(response: &Value) -> Option<String> {
    let output = response.get("output")?.as_array()?;
    output.iter().rev().find_map(|item| {
        if item.get("type").and_then(|v| v.as_str()) != Some("message") {
            return None;
        }
        if item.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            return None;
        }
        item.get("phase")
            .and_then(|v| v.as_str())
            .map(|phase| phase.to_string())
    })
}

fn extract_stop_reason_from_response(response: &Value) -> Option<String> {
    let status = response.get("status").and_then(|v| v.as_str());
    if status == Some("completed") {
        if extract_last_assistant_message_phase(response).as_deref() == Some("commentary") {
            return Some("commentary".to_string());
        }
        return None;
    }

    let incomplete_reason = response
        .get("incomplete_details")
        .and_then(|v| v.get("reason"))
        .and_then(|v| v.as_str());

    if let Some(reason) = incomplete_reason {
        return Some(reason.to_string());
    }

    status.filter(|value| !value.is_empty()).map(|value| value.to_string())
}

pub fn handle_openai_output_item(
    item: Value,
    saw_text_delta: &mut bool,
    saw_thinking_delta: &mut bool,
    pending: &mut VecDeque<StreamEvent>,
) -> Option<StreamEvent> {
    let item_type = item.get("type")?.as_str()?;
    match item_type {
        "compaction" => {
            let encrypted_content = item
                .get("encrypted_content")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())?;
            return Some(StreamEvent::Compaction {
                trigger: "openai_native_auto".to_string(),
                pre_tokens: None,
                openai_encrypted_content: Some(encrypted_content),
            });
        }
        "function_call" | "custom_tool_call" => {
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let raw_arguments = item
                .get("arguments")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .or_else(|| {
                    item.get("input").and_then(|v| {
                        if v.is_object() || v.is_array() {
                            Some(v.to_string())
                        } else {
                            v.as_str().map(|s| s.to_string())
                        }
                    })
                })
                .unwrap_or_else(|| "{}".to_string());
            let arguments = normalize_openai_tool_arguments(raw_arguments);

            pending.push_back(StreamEvent::ToolUseStart {
                id: call_id.clone(),
                name,
            });
            pending.push_back(StreamEvent::ToolInputDeltaFor {
                id: call_id.clone(),
                delta: arguments,
            });
            pending.push_back(StreamEvent::ToolUseEndFor { id: call_id });
            return pending.pop_front();
        }
        "image_generation_call" => {
            if let Some(event) = handle_openai_image_generation_item(&item, pending) {
                return Some(event);
            }
        }
        "web_search_call" => {
            // Hosted web search already ran on OpenAI's side. Keep the item
            // verbatim so it is replayed in later requests.
            return Some(StreamEvent::ProviderNative {
                provider: horon_agent_message_types::provider_native::PROVIDER_NATIVE_OPENAI.to_string(),
                item,
            });
        }
        "message" => {
            let mut text = String::new();
            let mut rendered = String::new();
            if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                for entry in content {
                    let entry_type = entry.get("type").and_then(|v| v.as_str());
                    if matches!(entry_type, Some("output_text") | Some("text"))
                        && let Some(t) = entry.get("text").and_then(|v| v.as_str())
                    {
                        text.push_str(t);
                        rendered.push_str(&crate::citations::render_annotated_text(t, entry.get("annotations")));
                    }
                }
            }
            if *saw_text_delta {
                // URL annotations arrive after raw deltas. Correct only this
                // completed message, before TextDone closes its render frame.
                return (rendered != text).then_some(StreamEvent::TextSuffixReplace {
                    old_text: text,
                    new_text: rendered,
                });
            }
            return stream_text_or_recovered_tool_call(&rendered, pending);
        }
        "reasoning" => {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let mut summary = Vec::new();
            if let Some(summary_arr) = item.get("summary").and_then(|v| v.as_array()) {
                for summary_item in summary_arr {
                    if summary_item.get("type").and_then(|v| v.as_str()) == Some("summary_text")
                        && let Some(text) = summary_item.get("text").and_then(|v| v.as_str())
                    {
                        summary.push(text.to_string());
                    }
                }
            }
            let encrypted_content = item
                .get("encrypted_content")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string());
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string());

            if !id.is_empty() && (encrypted_content.is_some() || !summary.is_empty()) {
                pending.push_back(StreamEvent::OpenAIReasoning {
                    id,
                    summary: summary.clone(),
                    encrypted_content,
                    status,
                });
            }

            // Only replay the full summary when it was NOT already streamed
            // live via `response.reasoning_summary_text.delta`; otherwise the
            // thinking text renders twice (once streamed, once from item.done).
            if !summary.is_empty() && !*saw_thinking_delta {
                pending.push_back(StreamEvent::ThinkingStart);
                pending.push_back(StreamEvent::ThinkingDelta(summary.join("\n")));
                pending.push_back(StreamEvent::ThinkingEnd);
                return pending.pop_front();
            }
            return pending.pop_front();
        }
        _ => {}
    }

    None
}

fn handle_openai_image_generation_item(item: &Value, pending: &mut VecDeque<StreamEvent>) -> Option<StreamEvent> {
    let result_b64 = item.get("result")?.as_str()?;
    if result_b64.is_empty() {
        return None;
    }

    let image_bytes = match BASE64_STANDARD.decode(result_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            horon_agent_logging::warn(&format!(
                "OpenAI image_generation_call returned invalid base64: {}",
                err
            ));
            return Some(StreamEvent::TextDelta(
                "\n[Generated image received, but HoronAgent could not decode it.]\n".to_string(),
            ));
        }
    };

    let output_format = item.get("output_format").and_then(|v| v.as_str()).unwrap_or("png");
    let extension = match output_format {
        "jpeg" | "jpg" => "jpg",
        "webp" => "webp",
        _ => "png",
    };
    let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("image");
    let safe_id: String = item_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .take(80)
        .collect();
    let safe_id = if safe_id.is_empty() {
        "image".to_string()
    } else {
        safe_id
    };
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let dir = std::env::current_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(".horon-agent")
        .join("generated-images");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        horon_agent_logging::warn(&format!("Failed to create OpenAI generated image directory: {}", err));
        return Some(StreamEvent::TextDelta(format!(
            "\n[Generated image received ({} bytes), but HoronAgent could not save it.]\n",
            image_bytes.len()
        )));
    }

    let filename = format!("{}-{}.{}", timestamp_ms, safe_id, extension);
    let path = dir.join(filename);
    if let Err(err) = std::fs::write(&path, image_bytes) {
        horon_agent_logging::warn(&format!("Failed to save OpenAI generated image: {}", err));
        return Some(StreamEvent::TextDelta(
            "\n[Generated image received, but HoronAgent could not save it.]\n".to_string(),
        ));
    }

    let metadata_path = path.with_extension("json");
    let mut response_item = item.clone();
    if let Some(object) = response_item.as_object_mut() {
        object.remove("result");
    }
    let revised_prompt = item.get("revised_prompt").and_then(|v| v.as_str()).map(str::to_string);
    let metadata = serde_json::json!({
        "schema_version": 1,
        "provider": "openai",
        "native_tool": "image_generation",
        "id": item_id,
        "status": item.get("status").and_then(|v| v.as_str()),
        "created_at_unix_ms": timestamp_ms,
        "image_path": path.display().to_string(),
        "output_format": output_format,
        "byte_count": std::fs::metadata(&path).map(|m| m.len()).unwrap_or_default(),
        "revised_prompt": revised_prompt,
        "response_item": response_item,
    });
    let metadata_path_string = match serde_json::to_vec_pretty(&metadata).ok().and_then(|bytes| {
        std::fs::write(&metadata_path, bytes)
            .ok()
            .map(|_| metadata_path.clone())
    }) {
        Some(path) => Some(path.display().to_string()),
        None => {
            horon_agent_logging::warn("Failed to save OpenAI generated image metadata");
            None
        }
    };

    let mut markdown = format!(
        "\n![Generated image]({})\n\nGenerated image saved to `{}`.",
        path.display(),
        path.display()
    );
    if let Some(metadata_path) = metadata_path_string.as_deref() {
        markdown.push_str(&format!("\nMetadata saved to `{}`.", metadata_path));
    }
    markdown.push('\n');

    pending.push_back(StreamEvent::TextDelta(markdown));

    Some(StreamEvent::GeneratedImage {
        id: item_id.to_string(),
        path: path.display().to_string(),
        metadata_path: metadata_path_string,
        output_format: output_format.to_string(),
        revised_prompt,
    })
}

pub struct OpenAIResponsesStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    framer: SseFrameBuffer,
    /// Carries incomplete multi-byte UTF-8 sequences across chunk boundaries
    /// so split CJK characters are not dropped (#609).
    utf8: horon_agent_core::util::Utf8StreamDecoder,
    pending: VecDeque<StreamEvent>,
    saw_text_delta: bool,
    saw_thinking_delta: bool,
    streaming_tool_calls: HashMap<String, StreamingToolCallState>,
    completed_tool_items: HashSet<String>,
    /// Set once the underlying stream ended and its trailing event was flushed.
    drained: bool,
}

impl OpenAIResponsesStream {
    pub fn new(stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static) -> Self {
        Self {
            inner: Box::pin(stream),
            framer: SseFrameBuffer::new(),
            utf8: horon_agent_core::util::Utf8StreamDecoder::new(),
            pending: VecDeque::new(),
            saw_text_delta: false,
            saw_thinking_delta: false,
            streaming_tool_calls: HashMap::new(),
            completed_tool_items: HashSet::new(),
            drained: false,
        }
    }

    fn parse_next_event(&mut self) -> Option<StreamEvent> {
        if let Some(event) = self.pending.pop_front() {
            return Some(event);
        }

        while let Some(data) = self.framer.take_next() {
            if let Some(event) = parse_openai_response_event(
                &data,
                &mut self.saw_text_delta,
                &mut self.saw_thinking_delta,
                &mut self.streaming_tool_calls,
                &mut self.completed_tool_items,
                &mut self.pending,
            ) {
                return Some(event);
            }
        }

        None
    }
}

fn extract_input_token_detail(usage: &Value, field: &str) -> Option<u64> {
    // Fall back per field: an empty/null Responses detail object should not
    // hide a value reported under the compatibility prompt_tokens_details key.
    ["input_tokens_details", "prompt_tokens_details"]
        .iter()
        .find_map(|key| usage.get(key)?.get(field)?.as_u64())
}

fn extract_usage_from_response(response: &Value) -> Option<StreamEvent> {
    let usage = response.get("usage")?;
    let input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64());
    let output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64());
    // OpenAI input_tokens is the total, including both cache-read and
    // cache-write subsets. Do not add these details to it or infer writes from
    // an uncached remainder: missing details mean unknown, not zero.
    let cache_read_input_tokens = extract_input_token_detail(usage, "cached_tokens");
    let cache_creation_input_tokens = extract_input_token_detail(usage, "cache_write_tokens");
    if input_tokens.is_some()
        || output_tokens.is_some()
        || cache_read_input_tokens.is_some()
        || cache_creation_input_tokens.is_some()
    {
        Some(StreamEvent::TokenUsage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        })
    } else {
        None
    }
}

impl Stream for OpenAIResponsesStream {
    type Item = Result<StreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.parse_next_event() {
                return Poll::Ready(Some(Ok(event)));
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let text = this.utf8.decode(&bytes);
                    this.framer.push_str(&text);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(anyhow::anyhow!("Stream error: {}", e))));
                }
                Poll::Ready(None) => {
                    if this.drained {
                        return Poll::Ready(None);
                    }
                    this.drained = true;
                    let tail = this.utf8.flush();
                    if !tail.is_empty() {
                        this.framer.push_str(&tail);
                    }
                    if let Some(data) = this.framer.flush()
                        && let Some(event) = parse_openai_response_event(
                            &data,
                            &mut this.saw_text_delta,
                            &mut this.saw_thinking_delta,
                            &mut this.streaming_tool_calls,
                            &mut this.completed_tool_items,
                            &mut this.pending,
                        )
                    {
                        return Poll::Ready(Some(Ok(event)));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "stream_tool_tests.rs"]
mod stream_tool_tests;

#[cfg(test)]
#[path = "stream_citation_tests.rs"]
mod stream_citation_tests;

#[cfg(test)]
mod text_framing_tests {
    use super::*;

    #[test]
    fn output_item_completion_frames_messages_not_reasoning_or_text_chunks() {
        let mut saw_text = false;
        let mut saw_thinking = false;
        let mut tools = HashMap::new();
        let mut completed = HashSet::new();
        let mut pending = VecDeque::new();
        let mut events = Vec::new();
        for value in [
            serde_json::json!({"type":"response.output_text.delta","delta":"The cause is "}),
            serde_json::json!({"type":"response.reasoning_summary_text.delta","delta":"thinking"}),
            serde_json::json!({"type":"response.output_text.delta","delta":"the retry loop."}),
            serde_json::json!({"type":"response.output_item.done","item":{"type":"message","content":[{"type":"output_text","text":"The cause is the retry loop."}]}}),
            // This message has no text delta. Per-message deduplication must
            // allow fallback output even though the preceding message streamed.
            serde_json::json!({"type":"response.output_item.done","item":{"type":"message","content":[{"type":"output_text","text":"Second message"}]}}),
            serde_json::json!({"type":"response.completed","response":{}}),
        ] {
            events.extend(parse_openai_response_event(
                &value.to_string(),
                &mut saw_text,
                &mut saw_thinking,
                &mut tools,
                &mut completed,
                &mut pending,
            ));
            events.extend(pending.drain(..));
        }
        assert!(matches!(events.as_slice(), [
            StreamEvent::TextDelta(a), StreamEvent::ThinkingDelta(_), StreamEvent::TextDelta(b),
            StreamEvent::TextDone, StreamEvent::TextDelta(c), StreamEvent::TextDone,
            StreamEvent::MessageEnd { .. }
        ] if a == "The cause is " && b == "the retry loop." && c == "Second message"));
    }

    #[test]
    fn web_search_call_item_is_emitted_verbatim() {
        let item = serde_json::json!({
            "type": "web_search_call", "id": "ws_1", "status": "completed",
            "action": {"type": "search", "query": "horon-agent"}
        });
        let mut pending = VecDeque::new();
        let event = parse_openai_response_event(
            &serde_json::json!({"type": "response.output_item.done", "item": item}).to_string(),
            &mut false,
            &mut false,
            &mut HashMap::new(),
            &mut HashSet::new(),
            &mut pending,
        );
        match event {
            Some(StreamEvent::ProviderNative { provider, item: got }) => {
                assert_eq!(provider, "openai");
                assert_eq!(got, item);
            }
            other => panic!("expected provider-native item, got {other:?}"),
        }
        assert!(pending.is_empty());
    }
}

#[cfg(test)]
mod framer_tests {
    use super::*;

    #[test]
    fn earliest_boundary_wins_when_lf_precedes_crlf() {
        let mut buffer = String::from("data: first\n\ndata: second\r\n\r\n");
        assert_eq!(take_next_sse_data(&mut buffer).as_deref(), Some("first"));
        assert_eq!(take_next_sse_data(&mut buffer).as_deref(), Some("second"));
        assert!(take_next_sse_data(&mut buffer).is_none());
        assert!(buffer.is_empty());
    }

    #[test]
    fn earliest_boundary_wins_when_crlf_precedes_lf() {
        let mut buffer = String::from("data: first\r\n\r\ndata: second\n\n");
        assert_eq!(take_next_sse_data(&mut buffer).as_deref(), Some("first"));
        assert_eq!(take_next_sse_data(&mut buffer).as_deref(), Some("second"));
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_multiline_event_joins_its_data_lines() {
        let mut buffer = String::from("data: one\ndata: two\r\n\r\n");
        assert_eq!(take_next_sse_data(&mut buffer).as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn a_trailing_event_without_a_blank_line_is_flushed_at_eof() {
        let mut buffer = String::from("data: tail");
        assert!(take_next_sse_data(&mut buffer).is_none());
        assert_eq!(flush_sse_data(&mut buffer).as_deref(), Some("tail"));
        assert!(buffer.is_empty());
        assert!(flush_sse_data(&mut buffer).is_none());
    }

    #[test]
    fn a_fragmented_multibyte_character_is_reassembled_across_chunks() {
        let bytes = "data: 가\n\n".as_bytes().to_vec();
        let cut = bytes.len() - 4;
        let mut decoder = horon_agent_core::util::Utf8StreamDecoder::new();
        let mut buffer = decoder.decode(&bytes[..cut]);
        assert!(
            take_next_sse_data(&mut buffer).is_none(),
            "the event is not complete yet"
        );
        buffer.push_str(&decoder.decode(&bytes[cut..]));
        assert_eq!(take_next_sse_data(&mut buffer).as_deref(), Some("가"));
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_truncated_character_at_eof_is_surfaced_by_flush_not_dropped() {
        let bytes = "data: 가".as_bytes().to_vec();
        let cut = bytes.len() - 2;
        let mut decoder = horon_agent_core::util::Utf8StreamDecoder::new();
        let mut buffer = decoder.decode(&bytes[..cut]);
        let tail = decoder.flush();
        assert!(!tail.is_empty(), "the truncated character is not silently dropped");
        buffer.push_str(&tail);
        let data = flush_sse_data(&mut buffer).expect("the trailing event flushes");
        assert!(data.contains('\u{FFFD}'), "the lossy remainder is visible: {data:?}");
    }
}
