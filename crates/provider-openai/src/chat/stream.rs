//! Chat Completions SSE -> [`StreamEvent`] translation.
//!
//! The agent consumes one event vocabulary no matter which wire protocol served
//! the turn. This module parses OpenAI-compatible `chat/completions` chunks and
//! emits the same sequence the Responses parser produces: reasoning deltas,
//! text deltas, one keyed tool-call stream per call, token usage, and a single
//! end of message.

use crate::chat::transport::{redact_secret, snippet};
use crate::stream::SseFrameBuffer;
use bytes::Bytes;
use futures::Stream;
use horon_agent_core::util::Utf8StreamDecoder;
use horon_agent_message_types::{StreamEvent, sanitize_tool_id};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

/// Marker payload that ends a streamed completion.
const DONE_SENTINEL: &str = "[DONE]";

/// Bound on a malformed payload echoed into an error message.
const MAX_ERROR_PAYLOAD_CHARS: usize = 400;

/// One tool call being assembled from deltas split across chunks.
#[derive(Debug, Default)]
struct ChatToolCallState {
    id: String,
    name: String,
    arguments: String,
    /// Bytes of `arguments` already announced with `ToolInputDeltaFor`.
    emitted: usize,
    started: bool,
    ended: bool,
}

/// Mutable parse state for one chat completion stream.
#[derive(Debug, Default)]
pub struct ChatStreamState {
    tool_calls: BTreeMap<u64, ChatToolCallState>,
    finish_reason: Option<String>,
    text_open: bool,
    thinking_open: bool,
    thinking_started: Option<Instant>,
    done: bool,
}

impl ChatStreamState {
    /// Whether the stream saw a terminal condition (`[DONE]` or end of body).
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The provider's `finish_reason`, once one arrived.
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }
}

/// Parse one SSE payload and return the next event, queueing any remainder.
///
/// A malformed payload becomes one [`StreamEvent::Error`]; it never panics and
/// never aborts the stream, so a single bad frame cannot hide the rest of a
/// response that the endpoint is still sending.
pub fn parse_chat_chunk(
    data: &str,
    state: &mut ChatStreamState,
    pending: &mut VecDeque<StreamEvent>,
    api_key: Option<&str>,
) -> Option<StreamEvent> {
    let payload = data.trim();
    if payload == DONE_SENTINEL {
        finish_stream(state, pending, api_key);
        return pending.pop_front();
    }

    let parsed: Value = match serde_json::from_str(payload) {
        Ok(parsed) => parsed,
        Err(error) => {
            let message = redact_secret(
                format!(
                    "OpenAI-compatible stream sent a malformed JSON chunk: {error}; payload={}",
                    snippet(payload, MAX_ERROR_PAYLOAD_CHARS)
                ),
                api_key,
            );
            return Some(StreamEvent::Error {
                message,
                retry_after_secs: None,
            });
        }
    };

    if let Some(error) = parsed.get("error") {
        let message = redact_secret(
            format!("OpenAI-compatible stream returned an error: {}", error_message(error)),
            api_key,
        );
        return Some(StreamEvent::Error {
            message,
            retry_after_secs: horon_agent_provider_core::retry_after::retry_after_body_seconds(payload),
        });
    }

    if let Some(usage) = parsed.get("usage").filter(|usage| usage.is_object())
        && let Some(event) = chat_usage_event(usage)
    {
        pending.push_back(event);
    }

    if let Some(choices) = parsed.get("choices").and_then(Value::as_array) {
        for choice in choices {
            handle_choice(choice, state, pending, api_key);
        }
    }

    pending.pop_front()
}

fn handle_choice(
    choice: &Value,
    state: &mut ChatStreamState,
    pending: &mut VecDeque<StreamEvent>,
    api_key: Option<&str>,
) {
    if let Some(delta) = choice.get("delta") {
        let reasoning = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .or_else(|| delta.get("reasoning").and_then(Value::as_str));
        if let Some(reasoning) = reasoning.filter(|text| !text.is_empty()) {
            if !state.thinking_open {
                state.thinking_open = true;
                state.thinking_started = Some(Instant::now());
                pending.push_back(StreamEvent::ThinkingStart);
            }
            pending.push_back(StreamEvent::ThinkingDelta(reasoning.to_string()));
        }

        if let Some(text) = delta.get("content").and_then(delta_content_text) {
            close_thinking(state, pending);
            state.text_open = true;
            pending.push_back(StreamEvent::TextDelta(text));
        }

        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                handle_tool_delta(call, state, pending);
            }
        }
    }

    if let Some(reason) = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
    {
        state.finish_reason = Some(reason.to_string());
        close_thinking(state, pending);
        close_text(state, pending);
        close_tool_calls(state, pending, api_key);
    }
}

fn handle_tool_delta(call: &Value, state: &mut ChatStreamState, pending: &mut VecDeque<StreamEvent>) {
    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
    let mut entry = state.tool_calls.remove(&index).unwrap_or_default();

    if let Some(id) = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        && entry.id.is_empty()
    {
        entry.id = sanitize_tool_id(id);
    }
    if let Some(name) = call
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        && entry.name.is_empty()
    {
        entry.name = name.to_string();
    }
    if let Some(arguments) = call.get("function").and_then(|function| function.get("arguments")) {
        let fragment = arguments
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| arguments.to_string());
        entry.arguments.push_str(&fragment);
    }

    if !entry.started && !entry.name.is_empty() {
        if entry.id.is_empty() {
            entry.id = format!("call_{index}");
        }
        close_thinking(state, pending);
        close_text(state, pending);
        entry.started = true;
        pending.push_back(StreamEvent::ToolUseStart {
            id: entry.id.clone(),
            name: entry.name.clone(),
        });
    }
    if entry.started && entry.arguments.len() > entry.emitted {
        // A name can arrive after the first argument fragment; everything
        // buffered is announced here so the consumer still sees the input.
        let delta = entry.arguments[entry.emitted..].to_string();
        entry.emitted = entry.arguments.len();
        pending.push_back(StreamEvent::ToolInputDeltaFor {
            id: entry.id.clone(),
            delta,
        });
    }

    state.tool_calls.insert(index, entry);
}

fn close_thinking(state: &mut ChatStreamState, pending: &mut VecDeque<StreamEvent>) {
    if !state.thinking_open {
        return;
    }
    state.thinking_open = false;
    let duration_secs = state
        .thinking_started
        .take()
        .map(|started| started.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    pending.push_back(StreamEvent::ThinkingEnd);
    pending.push_back(StreamEvent::ThinkingDone { duration_secs });
}

fn close_text(state: &mut ChatStreamState, pending: &mut VecDeque<StreamEvent>) {
    if state.text_open {
        state.text_open = false;
        pending.push_back(StreamEvent::TextDone);
    }
}

fn close_tool_calls(state: &mut ChatStreamState, pending: &mut VecDeque<StreamEvent>, api_key: Option<&str>) {
    for (index, entry) in state.tool_calls.iter_mut() {
        if entry.ended {
            continue;
        }
        entry.ended = true;
        if !entry.started {
            if entry.name.is_empty() {
                pending.push_back(StreamEvent::Error {
                    message: redact_secret(
                        format!("OpenAI-compatible stream completed tool call {index} without a function name"),
                        api_key,
                    ),
                    retry_after_secs: None,
                });
                continue;
            }
            entry.started = true;
            pending.push_back(StreamEvent::ToolUseStart {
                id: entry.id.clone(),
                name: entry.name.clone(),
            });
            if entry.arguments.len() > entry.emitted {
                let delta = entry.arguments[entry.emitted..].to_string();
                entry.emitted = entry.arguments.len();
                pending.push_back(StreamEvent::ToolInputDeltaFor {
                    id: entry.id.clone(),
                    delta,
                });
            }
        }
        pending.push_back(StreamEvent::ToolUseEndFor { id: entry.id.clone() });
    }
}

fn finish_stream(state: &mut ChatStreamState, pending: &mut VecDeque<StreamEvent>, api_key: Option<&str>) {
    if state.done {
        return;
    }
    state.done = true;
    close_thinking(state, pending);
    close_text(state, pending);
    close_tool_calls(state, pending, api_key);
    pending.push_back(StreamEvent::MessageEnd {
        stop_reason: state.finish_reason.clone(),
    });
}

/// Close a stream whose body ended without a terminal marker.
///
/// A body that stopped after a `finish_reason` is a complete response whose
/// server omitted `[DONE]`; a body that stopped before one is truncated, and
/// saying so is the only way the turn loop can tell it apart from a normal end.
fn finish_at_eof(state: &mut ChatStreamState, pending: &mut VecDeque<StreamEvent>, api_key: Option<&str>) {
    if state.done {
        return;
    }
    state.done = true;
    let truncated = state.finish_reason.is_none();
    close_thinking(state, pending);
    close_text(state, pending);
    close_tool_calls(state, pending, api_key);
    if truncated {
        pending.push_back(StreamEvent::Error {
            message: redact_secret(
                "OpenAI-compatible stream ended before the response finished (no finish_reason or [DONE])".to_string(),
                api_key,
            ),
            retry_after_secs: None,
        });
    } else {
        pending.push_back(StreamEvent::MessageEnd {
            stop_reason: state.finish_reason.clone(),
        });
    }
}

fn delta_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => (!text.is_empty()).then(|| text.clone()),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                if matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("text") | Some("output_text")
                ) && let Some(chunk) = part.get("text").and_then(Value::as_str)
                {
                    text.push_str(chunk);
                }
            }
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn chat_usage_event(usage: &Value) -> Option<StreamEvent> {
    let input_tokens = usage.get("prompt_tokens").and_then(Value::as_u64);
    let output_tokens = usage.get("completion_tokens").and_then(Value::as_u64);
    // `prompt_tokens` already includes the cached subset, matching the inclusive
    // usage shape the Responses parser reports.
    let cache_read_input_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| usage.get("prompt_cache_hit_tokens").and_then(Value::as_u64));
    if input_tokens.is_none() && output_tokens.is_none() && cache_read_input_tokens.is_none() {
        return None;
    }
    Some(StreamEvent::TokenUsage {
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens: None,
    })
}

fn error_message(error: &Value) -> String {
    if let Some(message) = error.as_str() {
        return message.to_string();
    }
    let message = error.get("message").and_then(Value::as_str);
    let kind = error.get("type").and_then(Value::as_str);
    match (message, kind) {
        (Some(message), Some(kind)) => format!("{kind}: {message}"),
        (Some(message), None) => message.to_string(),
        (None, _) => error.to_string(),
    }
}

/// Streaming parse of a Chat Completions response body.
pub struct ChatCompletionsStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    framer: SseFrameBuffer,
    /// Carries incomplete multi-byte UTF-8 sequences across chunk boundaries.
    utf8: Utf8StreamDecoder,
    pending: VecDeque<StreamEvent>,
    state: ChatStreamState,
    drained: bool,
    /// Credential to keep out of any diagnostic produced while parsing.
    api_key: Option<Arc<str>>,
}

impl ChatCompletionsStream {
    /// `api_key` is redacted from every error this stream reports.
    pub fn new(
        stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
        api_key: Option<Arc<str>>,
    ) -> Self {
        Self {
            inner: Box::pin(stream),
            framer: SseFrameBuffer::new(),
            utf8: Utf8StreamDecoder::new(),
            pending: VecDeque::new(),
            state: ChatStreamState::default(),
            drained: false,
            api_key,
        }
    }

    fn parse_next_event(&mut self) -> Option<StreamEvent> {
        if let Some(event) = self.pending.pop_front() {
            return Some(event);
        }
        while let Some(data) = self.framer.take_next() {
            if let Some(event) = self.parse_data(&data) {
                return Some(event);
            }
        }
        None
    }

    fn parse_data(&mut self, data: &str) -> Option<StreamEvent> {
        let Self {
            state,
            pending,
            api_key,
            ..
        } = self;
        parse_chat_chunk(data, state, pending, api_key.as_deref())
    }

    fn finish_eof(&mut self) {
        let Self {
            state,
            pending,
            api_key,
            ..
        } = self;
        finish_at_eof(state, pending, api_key.as_deref());
    }
}

impl Stream for ChatCompletionsStream {
    type Item = anyhow::Result<StreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
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
                Poll::Ready(Some(Err(error))) => {
                    let message = redact_secret(
                        format!("OpenAI-compatible stream error: {error}"),
                        this.api_key.as_deref(),
                    );
                    return Poll::Ready(Some(Err(anyhow::anyhow!(message))));
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
                    // A trailing frame without its blank line is parsed here. Its
                    // events must be queued, not returned: returning early would
                    // skip the terminal-event step below on the next poll, which
                    // then only sees `drained` and ends the stream silently.
                    if let Some(data) = this.framer.flush()
                        && let Some(event) = this.parse_data(&data)
                    {
                        this.pending.push_front(event);
                    }
                    this.finish_eof();
                    if let Some(event) = this.pending.pop_front() {
                        return Poll::Ready(Some(Ok(event)));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
