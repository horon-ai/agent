//! The Anthropic Messages wire: request body and stream events.
//!
//! Anthropic's `POST /messages` shapes the conversation as alternating `user` and
//! `assistant` turns whose content is a list of typed blocks, so neither the
//! Responses items nor the Chat messages fit it. [`build_messages_request`]
//! converts the canonical [`Message`] history block by block, and
//! [`AnthropicParser`] turns the block-indexed event stream back into the shared
//! [`StreamEvent`](StreamEvent) vocabulary.
//!
//! Thinking is the one part of that conversation this wire cannot express as a
//! plain block: a completed `thinking` (or `redacted_thinking`) block is emitted
//! as [`StreamEvent::ProviderNative`] under this endpoint's tag and replayed into
//! a later request only when the stored tag still matches.

use crate::chat::transport::snippet;
use crate::wire::sse::WireParser;
use horon_agent_message_types::degradation::{DegradationKind, DegradationReceipt};
use horon_agent_message_types::{ContentBlock, Message, Role, StreamEvent, ToolDefinition, sanitize_tool_id};
use horon_agent_provider_core::anthropic::anthropic_max_output_tokens;
use horon_agent_provider_core::{AnthropicReasoningCaps, anthropic_reasoning_caps};
use horon_agent_schema_dialect::{DropLog, normalize, normalize_reporting, registry};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::time::Instant;

/// Output budget a `minimal` or `low` effort asks manual thinking for.
const LOW_BUDGET_TOKENS: u32 = 4_096;

/// Output budget a `medium` effort asks manual thinking for.
const MEDIUM_BUDGET_TOKENS: u32 = 10_000;

/// Output budget `high`, `xhigh` and `max` ask manual thinking for.
const HIGH_BUDGET_TOKENS: u32 = 32_000;

/// Build a streaming Messages request body.
///
/// `native_tag` is the endpoint identity stored on the
/// [`ContentBlock::ProviderNative`] blocks of earlier turns; only a block whose
/// stored provider equals it is replayed, because a signature minted for another
/// endpoint is rejected here.
///
/// The caller normalizes the history ([`crate::wire::turns::normalize`]) before
/// calling this: the builders in this module only translate, they never rewrite
/// turn order or pair tool calls with results.
pub fn build_messages_request(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    system: &str,
    reasoning_effort: Option<&str>,
    native_tag: &str,
    sink: Option<&mut Vec<DegradationReceipt>>,
) -> Value {
    let max_tokens = anthropic_max_output_tokens(model);
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": messages_body(messages, native_tag),
    });
    if !system.is_empty() {
        // The system prompt is a cached block so the whole prefix is billable as
        // a cache read on the next turn instead of as fresh input.
        body["system"] = json!([{
            "type": "text",
            "text": system,
            "cache_control": { "type": "ephemeral" },
        }]);
    }
    let tool_bodies = build_tools(tools, sink);
    if !tool_bodies.is_empty() {
        body["tools"] = Value::Array(tool_bodies);
    }
    apply_effort(&mut body, model, reasoning_effort, max_tokens);
    body
}

/// Translate the history into `role` + `content` turns.
///
/// A message whose blocks all fail to convert is skipped first, then adjacent
/// turns of the same role are merged, so a dropped message can never leave two
/// consecutive turns with one role: the API rejects that shape outright.
fn messages_body(messages: &[Message], native_tag: &str) -> Vec<Value> {
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::with_capacity(messages.len());
    for message in messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let blocks: Vec<Value> = message
            .content
            .iter()
            .filter_map(|block| convert_block(block, &message.role, native_tag))
            .collect();
        if blocks.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((last_role, last_blocks)) if *last_role == role => last_blocks.extend(blocks),
            _ => turns.push((role, blocks)),
        }
    }
    turns
        .into_iter()
        .map(|(role, blocks)| json!({ "role": role, "content": blocks }))
        .collect()
}

/// One content block in the wire's shape, or `None` when it has no counterpart.
///
/// Every reasoning variant except a replayed native block is dropped:
/// `ReasoningTrace` is history-only by contract, `AnthropicThinking` carries a
/// signature from another endpoint, and the OpenAI forms mean nothing here.
fn convert_block(block: &ContentBlock, role: &Role, native_tag: &str) -> Option<Value> {
    match block {
        ContentBlock::Text { text, cache_control } => {
            if text.is_empty() {
                return None;
            }
            let mut wire = json!({ "type": "text", "text": text });
            if let Some(cache_control) = cache_control
                && let Ok(value) = serde_json::to_value(cache_control)
            {
                wire["cache_control"] = value;
            }
            Some(wire)
        }
        ContentBlock::Image { media_type, data } => Some(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data,
            },
        })),
        ContentBlock::ToolUse { id, name, input, .. } => {
            let input = if input.is_object() { input.clone() } else { json!({}) };
            Some(json!({
                "type": "tool_use",
                "id": sanitize_tool_id(id),
                "name": name,
                "input": input,
            }))
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => {
            let mut wire = json!({
                "type": "tool_result",
                "tool_use_id": sanitize_tool_id(tool_use_id),
                "content": content,
            });
            if *is_error == Some(true) {
                wire["is_error"] = json!(true);
            }
            Some(wire)
        }
        ContentBlock::ProviderNative { provider, item } => {
            // A thinking block is only valid inside the assistant turn that
            // produced it, and only for the endpoint that signed it.
            let replayed = matches!(role, Role::Assistant)
                && provider == native_tag
                && matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("thinking" | "redacted_thinking")
                );
            replayed.then(|| item.clone())
        }
        ContentBlock::Reasoning { .. }
        | ContentBlock::ReasoningTrace { .. }
        | ContentBlock::AnthropicThinking { .. }
        | ContentBlock::OpenAIReasoning { .. }
        | ContentBlock::Audio { .. }
        | ContentBlock::OpenAICompaction { .. }
        | ContentBlock::ToolReference { .. } => None,
    }
}

/// The `tools` array, with each schema normalized for this dialect.
///
/// Two definitions with one name cannot both be declared, and a definition with
/// no name cannot be called; both are reported rather than silently reshaped.
/// Deferred definitions are not sent: this wire reports no point-of-use loading,
/// so their body would never arrive at the point that references it.
fn build_tools(tools: &[ToolDefinition], mut sink: Option<&mut Vec<DegradationReceipt>>) -> Vec<Value> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut built = Vec::with_capacity(tools.len());
    for tool in tools.iter().filter(|tool| !tool.defer_loading) {
        if tool.name.is_empty() {
            continue;
        }
        if !seen.insert(tool.name.as_str()) {
            if let Some(receipts) = sink.as_deref_mut() {
                receipts.push(DegradationReceipt::new(
                    DegradationKind::DuplicateToolDropped,
                    tool.name.clone(),
                    tool.name.clone(),
                ));
            }
            continue;
        }
        let mut dropped: Vec<String> = Vec::new();
        let schema = if sink.is_some() {
            normalize_reporting(
                &tool.input_schema,
                &registry::ANTHROPIC,
                &mut DropLog::enabled(&mut dropped),
            )
        } else {
            normalize(&tool.input_schema, &registry::ANTHROPIC)
        };
        // The API rejects an `input_schema` that is not an object schema with a
        // `properties` map; the dialect guarantees it, and this keeps the
        // invariant local to the wire.
        let input_schema = if schema.is_object() {
            schema
        } else {
            json!({ "type": "object", "properties": {} })
        };
        built.push(json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": input_schema,
        }));
        if let Some(receipts) = sink.as_deref_mut() {
            for keyword in dropped {
                receipts.push(DegradationReceipt::new(
                    DegradationKind::SchemaKeywordDropped,
                    tool.name.clone(),
                    keyword,
                ));
            }
        }
    }
    built
}

/// Add the reasoning fields this model accepts, if any.
///
/// Effort is expressed three different ways across Claude generations, so the
/// model's capabilities decide the shape: an `output_config.effort` word, an
/// adaptive thinking block, a manual thinking budget, or nothing at all. A word
/// the endpoint would reject adds no field rather than a guess.
fn apply_effort(body: &mut Value, model: &str, reasoning_effort: Option<&str>, max_tokens: u32) {
    let Some(word) = reasoning_effort
        .map(|effort| effort.trim().to_ascii_lowercase())
        .filter(|effort| !effort.is_empty() && effort != "none")
    else {
        return;
    };
    let caps = anthropic_reasoning_caps(model);
    let Some(effort) = mapped_effort(&word, caps) else {
        return;
    };
    if caps.output_effort {
        body["output_config"] = json!({ "effort": effort });
        if caps.adaptive_thinking {
            body["thinking"] = json!({ "type": "adaptive" });
        } else if caps.manual_thinking {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": thinking_budget(&word, max_tokens),
            });
        }
    } else if caps.manual_thinking {
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": thinking_budget(&word, max_tokens),
        });
    }
}

/// The effort word to send, or `None` when this model has no such level.
fn mapped_effort(word: &str, caps: AnthropicReasoningCaps) -> Option<&'static str> {
    match word {
        "minimal" => Some("low"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some(if caps.xhigh_effort { "xhigh" } else { "high" }),
        "max" => Some(if caps.max_effort { "max" } else { "high" }),
        _ => None,
    }
}

/// The manual thinking budget for `word`, kept below `max_tokens`.
///
/// A budget equal to the output limit is rejected, and a model whose limit is
/// smaller than the word's budget (a small context, a low cap) must still get a
/// valid request.
fn thinking_budget(word: &str, max_tokens: u32) -> u32 {
    let budget = match word {
        "minimal" | "low" => LOW_BUDGET_TOKENS,
        "medium" => MEDIUM_BUDGET_TOKENS,
        _ => HIGH_BUDGET_TOKENS,
    };
    budget.min(max_tokens.saturating_sub(1))
}

/// One content block open in the stream, keyed by its server index.
enum Block {
    /// A text block. Its deltas were already emitted, so closing it only ends
    /// the text run.
    Text,
    /// A thinking block: the text and signature accumulate here because the
    /// replayable `ProviderNative` item is built only once the block closes.
    Thinking {
        text: String,
        signature: String,
        started: Instant,
    },
    /// A `redacted_thinking` block, forwarded verbatim.
    Redacted { item: Value },
    /// A tool call. `initial_input` is the complete input an endpoint may put on
    /// the start event instead of streaming it as JSON deltas.
    ToolUse {
        id: String,
        initial_input: Option<String>,
        saw_delta: bool,
    },
    /// A block kind this crate does not translate; its deltas are ignored.
    Other,
}

/// Translates Messages SSE payloads into shared stream events.
///
/// The API streams by content-block index: a block is started, its deltas
/// arrive, and only then is it stopped. Each index therefore holds state, and
/// the blocks are kept in index order so a response that ends early closes them
/// in the order the server opened them.
pub struct AnthropicParser {
    /// Endpoint identity stamped on every replayed native thinking block.
    native_tag: String,
    blocks: BTreeMap<u64, Block>,
    /// The `stop_reason` a `message_delta` announced, kept until the message
    /// ends so an omitted `message_stop` can still complete it.
    stop_reason: Option<String>,
    /// Set by the terminal event, after which nothing more is parsed.
    finished: bool,
}

impl AnthropicParser {
    /// A parser that tags native thinking blocks with `native_tag`.
    pub fn new(native_tag: impl Into<String>) -> Self {
        Self {
            native_tag: native_tag.into(),
            blocks: BTreeMap::new(),
            stop_reason: None,
            finished: false,
        }
    }

    /// Close one block, emitting whatever the block kind owes its consumer.
    fn close_block(&self, block: Block, out: &mut VecDeque<StreamEvent>) {
        match block {
            Block::Text => out.push_back(StreamEvent::TextDone),
            Block::Thinking {
                text,
                signature,
                started,
            } => {
                out.push_back(StreamEvent::ThinkingEnd);
                out.push_back(StreamEvent::ThinkingDone {
                    duration_secs: started.elapsed().as_secs_f64(),
                });
                // An unsigned block cannot be replayed: the API rejects thinking
                // context it did not sign, so there is nothing to store.
                if !signature.is_empty() {
                    out.push_back(StreamEvent::ProviderNative {
                        provider: self.native_tag.clone(),
                        item: json!({
                            "type": "thinking",
                            "thinking": text,
                            "signature": signature,
                        }),
                    });
                }
            }
            Block::Redacted { item } => out.push_back(StreamEvent::ProviderNative {
                provider: self.native_tag.clone(),
                item,
            }),
            Block::ToolUse {
                id,
                initial_input,
                saw_delta,
            } => {
                // An endpoint may send the whole input on the start event. The
                // consumer only ever assembles the JSON from deltas, so it is
                // emitted here exactly once, and never beside real deltas.
                if !saw_delta && let Some(initial_input) = initial_input {
                    out.push_back(StreamEvent::ToolInputDeltaFor {
                        id: id.clone(),
                        delta: initial_input,
                    });
                }
                out.push_back(StreamEvent::ToolUseEndFor { id });
            }
            Block::Other => {}
        }
    }

    /// Close every block still open, in index order.
    fn close_all(&mut self, out: &mut VecDeque<StreamEvent>) {
        let blocks = std::mem::take(&mut self.blocks);
        for (_, block) in blocks {
            self.close_block(block, out);
        }
    }
}

impl WireParser for AnthropicParser {
    fn parse(&mut self, data: &str, out: &mut VecDeque<StreamEvent>) {
        let payload = data.trim();
        if payload.is_empty() || self.finished {
            return;
        }
        let parsed: Value = match serde_json::from_str(payload) {
            Ok(parsed) => parsed,
            Err(error) => {
                // One bad frame is not a broken response: report it and keep
                // reading, so the rest of the answer still arrives.
                out.push_back(StreamEvent::Error {
                    message: format!(
                        "Anthropic-compatible stream sent a malformed JSON chunk: {error}; payload={}",
                        snippet(payload, 400)
                    ),
                    retry_after_secs: None,
                });
                return;
            }
        };
        let Some(kind) = parsed.get("type").and_then(Value::as_str) else {
            return;
        };
        match kind {
            "message_start" => {
                if let Some(event) = usage_event(&parsed["message"]["usage"]) {
                    out.push_back(event);
                }
            }
            "content_block_start" => {
                let Some(index) = parsed.get("index").and_then(Value::as_u64) else {
                    return;
                };
                let content_block = parsed.get("content_block").cloned().unwrap_or(Value::Null);
                match content_block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        self.blocks.insert(index, Block::Text);
                        if let Some(text) = content_block.get("text").and_then(Value::as_str)
                            && !text.is_empty()
                        {
                            out.push_back(StreamEvent::TextDelta(text.to_string()));
                        }
                    }
                    Some("thinking") => {
                        out.push_back(StreamEvent::ThinkingStart);
                        let text = content_block.get("thinking").and_then(Value::as_str).unwrap_or("");
                        let signature = content_block.get("signature").and_then(Value::as_str).unwrap_or("");
                        self.blocks.insert(
                            index,
                            Block::Thinking {
                                text: text.to_string(),
                                signature: signature.to_string(),
                                started: Instant::now(),
                            },
                        );
                        if !text.is_empty() {
                            out.push_back(StreamEvent::ThinkingDelta(text.to_string()));
                        }
                    }
                    Some("redacted_thinking") => {
                        self.blocks.insert(
                            index,
                            Block::Redacted {
                                item: content_block.clone(),
                            },
                        );
                    }
                    Some("tool_use") => {
                        let id = content_block
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .map(sanitize_tool_id)
                            .unwrap_or_else(|| format!("call_{index}"));
                        let name = content_block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        out.push_back(StreamEvent::ToolUseStart { id: id.clone(), name });
                        let initial_input = content_block
                            .get("input")
                            .filter(|input| input.as_object().is_some_and(|input| !input.is_empty()))
                            .map(Value::to_string);
                        self.blocks.insert(
                            index,
                            Block::ToolUse {
                                id,
                                initial_input,
                                saw_delta: false,
                            },
                        );
                    }
                    _ => {
                        self.blocks.insert(index, Block::Other);
                    }
                }
            }
            "content_block_delta" => {
                let Some(index) = parsed.get("index").and_then(Value::as_u64) else {
                    return;
                };
                let delta = parsed.get("delta").cloned().unwrap_or(Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        self.blocks.entry(index).or_insert(Block::Text);
                        if let Some(text) = delta.get("text").and_then(Value::as_str)
                            && !text.is_empty()
                        {
                            out.push_back(StreamEvent::TextDelta(text.to_string()));
                        }
                    }
                    Some("thinking_delta") => {
                        // A delta may arrive for a block whose start the server
                        // never sent; the consumer still gets a well-formed run.
                        let mut opened = false;
                        self.blocks.entry(index).or_insert_with(|| {
                            opened = true;
                            Block::Thinking {
                                text: String::new(),
                                signature: String::new(),
                                started: Instant::now(),
                            }
                        });
                        if opened {
                            out.push_back(StreamEvent::ThinkingStart);
                        }
                        if let Some(Block::Thinking { text, .. }) = self.blocks.get_mut(&index)
                            && let Some(chunk) = delta.get("thinking").and_then(Value::as_str)
                            && !chunk.is_empty()
                        {
                            text.push_str(chunk);
                            out.push_back(StreamEvent::ThinkingDelta(chunk.to_string()));
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(Block::Thinking { signature, .. }) = self.blocks.get_mut(&index)
                            && let Some(chunk) = delta.get("signature").and_then(Value::as_str)
                        {
                            signature.push_str(chunk);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(Block::ToolUse { id, saw_delta, .. }) = self.blocks.get_mut(&index) {
                            *saw_delta = true;
                            if let Some(chunk) = delta.get("partial_json").and_then(Value::as_str)
                                && !chunk.is_empty()
                            {
                                out.push_back(StreamEvent::ToolInputDeltaFor {
                                    id: id.clone(),
                                    delta: chunk.to_string(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(index) = parsed.get("index").and_then(Value::as_u64) else {
                    return;
                };
                if let Some(block) = self.blocks.remove(&index) {
                    self.close_block(block, out);
                }
            }
            "message_delta" => {
                if let Some(stop_reason) = parsed["delta"].get("stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(stop_reason.to_string());
                }
                if let Some(event) = usage_event(&parsed["usage"]) {
                    out.push_back(event);
                }
            }
            "message_stop" => {
                self.close_all(out);
                out.push_back(StreamEvent::MessageEnd {
                    stop_reason: self.stop_reason.take(),
                });
                self.finished = true;
            }
            "error" => {
                self.close_all(out);
                out.push_back(StreamEvent::Error {
                    message: format!(
                        "Anthropic-compatible stream returned an error: {}",
                        error_text(&parsed["error"])
                    ),
                    retry_after_secs: horon_agent_provider_core::retry_after::retry_after_body_seconds(payload),
                });
                self.finished = true;
            }
            // `ping` keeps the connection warm and every other event belongs to
            // a feature this wire does not consume.
            _ => {}
        }
    }

    fn finish(&mut self, out: &mut VecDeque<StreamEvent>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.close_all(out);
        match self.stop_reason.take() {
            // A `message_delta` announced how the message ended, so only the
            // final marker went missing: the response itself is complete.
            Some(stop_reason) => out.push_back(StreamEvent::MessageEnd {
                stop_reason: Some(stop_reason),
            }),
            None => out.push_back(StreamEvent::Error {
                message: "Anthropic-compatible stream ended before the response finished \
                          (no stop_reason or message_stop)"
                    .to_string(),
                retry_after_secs: None,
            }),
        }
    }
}

/// One `TokenUsage` from a `usage` object, or `None` when it carries no counts.
///
/// A field the object omits stays `None` so the consumer keeps the value an
/// earlier event reported: Anthropic sends the input side on `message_start` and
/// the output side on `message_delta`.
fn usage_event(usage: &Value) -> Option<StreamEvent> {
    let input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
    let output_tokens = usage.get("output_tokens").and_then(Value::as_u64);
    let cache_read_input_tokens = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
    let cache_creation_input_tokens = usage.get("cache_creation_input_tokens").and_then(Value::as_u64);
    if input_tokens.is_none()
        && output_tokens.is_none()
        && cache_read_input_tokens.is_none()
        && cache_creation_input_tokens.is_none()
    {
        return None;
    }
    Some(StreamEvent::TokenUsage {
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
    })
}

/// The text of an `error` event, whether it is a string, an Anthropic object or
/// a Gemini-style one carried by a compatible gateway.
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

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod anthropic_tests;
