//! Gemini `generateContent` request body and stream parser.
//!
//! Gemini shares the transport, the credential header and the SSE framing with
//! every other wire ([`crate::wire::sse`]); only the body shape and the chunk
//! shape are its own.
//!
//! The body has no `model` and no `stream` field: both live in the URL that
//! [`super::WireProtocol::endpoint`] builds. The conversation is one `contents`
//! array of alternating `user`/`model` turns whose parts carry text, inline
//! media, function calls and function responses. Tool results are addressed by
//! *name*, not by id, so a `functionResponse` is built from a map of call id to
//! name collected from the whole history; the id travels back only when the
//! server gave one, because this wire invents ids of its own for calls that
//! arrive without one and the server never saw those.
//!
//! Streaming is cumulative rather than delta-shaped: each `data:` payload is one
//! `GenerateContentResponse` holding the parts added since the previous chunk,
//! and there is no end marker — the last chunk carries `finishReason` and the
//! body simply stops. A body that stops before any `finishReason` is a truncated
//! stream, which [`GeminiParser::finish`] reports instead of closing the turn
//! quietly.

use crate::chat::transport::snippet;
use crate::wire::sse::WireParser;
use horon_agent_message_types::degradation::{DegradationKind, DegradationReceipt};
use horon_agent_message_types::{ContentBlock, Message, Role, StreamEvent, ToolDefinition, sanitize_tool_id};
use horon_agent_provider_core::retry_after::retry_after_body_seconds;
use horon_agent_schema_dialect::{DropLog, normalize, normalize_reporting, registry};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Prefix of a call id this wire invents for a `functionCall` the server sent
/// without one.
///
/// The prefix is what tells a later request whether an id may be replayed: a
/// server id was issued by the server and is sent back so the response can be
/// matched, while a synthesized one is unknown to it and stays local.
pub const SYNTHETIC_CALL_ID_PREFIX: &str = "hgc_";

/// Label this wire's failures start with, matching [`super::WireProtocol::label`].
const LABEL: &str = "Gemini-compatible";

/// `thinkingBudget` for effort `minimal` on a Gemini 2.5 model.
pub const THINKING_BUDGET_MINIMAL: u64 = 512;
/// `thinkingBudget` for effort `low` on a Gemini 2.5 model.
pub const THINKING_BUDGET_LOW: u64 = 1024;
/// `thinkingBudget` for effort `medium` on a Gemini 2.5 model.
pub const THINKING_BUDGET_MEDIUM: u64 = 8192;
/// `thinkingBudget` for effort `high` (and above) on a Gemini 2.5 model.
pub const THINKING_BUDGET_HIGH: u64 = 24_576;

/// `finishReason` value that means "the model has not stopped yet".
const FINISH_REASON_UNSPECIFIED: &str = "FINISH_REASON_UNSPECIFIED";
/// `blockReason` value that means "the prompt was not filtered".
const BLOCK_REASON_UNSPECIFIED: &str = "BLOCK_REASON_UNSPECIFIED";

/// Build a `streamGenerateContent` request body.
///
/// `messages` arrive already normalized by [`crate::wire::turns::normalize`], so
/// every call is answered by the turn right after it and this builder does not
/// reorder anything. `sink` receives one receipt per tool declaration the wire
/// could not carry in full.
pub fn build_generate_content_request(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    system: &str,
    reasoning_effort: Option<&str>,
    sink: Option<&mut Vec<DegradationReceipt>>,
) -> Value {
    let mut request = Map::new();
    if !system.is_empty() {
        request.insert(
            "systemInstruction".to_string(),
            json!({ "parts": [{ "text": system }] }),
        );
    }

    let contents = build_contents(messages);
    if !contents.is_empty() {
        request.insert("contents".to_string(), Value::Array(contents));
    }

    let declarations = build_function_declarations(tools, sink);
    if !declarations.is_empty() {
        request.insert("tools".to_string(), json!([{ "functionDeclarations": declarations }]));
    }

    if let Some(thinking) = thinking_config(model, reasoning_effort) {
        request.insert("generationConfig".to_string(), json!({ "thinkingConfig": thinking }));
    }

    Value::Object(request)
}

/// The `contents` array: one entry per non-empty turn, merged by role.
fn build_contents(messages: &[Message]) -> Vec<Value> {
    let names = tool_call_names(messages);
    let mut contents: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages {
        let parts = convert_parts(message, &names);
        if parts.is_empty() {
            continue;
        }
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "model",
        };
        // Consecutive turns of one role read as one turn to the model, and a
        // strict server accepts the merged form where it may reject the split
        // one, so they are appended to the previous entry instead of opening a
        // second turn.
        if let Some(Value::Object(entry)) = contents.last_mut()
            && entry.get("role").and_then(Value::as_str) == Some(role)
        {
            match entry.get_mut("parts") {
                Some(Value::Array(existing)) => existing.extend(parts),
                _ => {
                    entry.insert("parts".to_string(), Value::Array(parts));
                }
            }
            continue;
        }
        contents.push(json!({ "role": role, "parts": parts }));
    }
    contents
}

/// Call id to call name, from every `ToolUse` block in the history.
fn tool_call_names(messages: &[Message]) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse { id, name, .. } = block {
                names.insert(id.clone(), name.clone());
            }
        }
    }
    names
}

/// Convert one message's blocks into `Part`s this wire can carry.
fn convert_parts(message: &Message, names: &HashMap<String, String>) -> Vec<Value> {
    let mut parts = Vec::with_capacity(message.content.len());
    for block in &message.content {
        match block {
            ContentBlock::Text { text, .. } => {
                if text.is_empty() {
                    continue;
                }
                parts.push(json!({ "text": text }));
            }
            a2ui @ ContentBlock::A2ui { .. } => {
                if let Some(summary) = a2ui.a2ui_model_summary() {
                    parts.push(json!({ "text": summary }));
                }
            }
            ContentBlock::Image { media_type, data } => {
                parts.push(json!({ "inlineData": { "mimeType": media_type, "data": data } }));
            }
            ContentBlock::ToolUse {
                id,
                name,
                input,
                thought_signature,
            } => {
                let mut call = Map::new();
                call.insert("name".to_string(), json!(name));
                call.insert(
                    "args".to_string(),
                    if input.is_object() { input.clone() } else { json!({}) },
                );
                if replayable_id(id) {
                    call.insert("id".to_string(), json!(id));
                }
                let mut part = Map::new();
                part.insert("functionCall".to_string(), Value::Object(call));
                if let Some(signature) = thought_signature.as_deref().filter(|value| !value.is_empty()) {
                    part.insert("thoughtSignature".to_string(), json!(signature));
                }
                parts.push(Value::Object(part));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } => {
                // The response names the function, so a result for a call the
                // history no longer holds cannot be addressed and is dropped.
                let Some(name) = names.get(tool_use_id) else {
                    continue;
                };
                let mut response = Map::new();
                response.insert("name".to_string(), json!(name));
                let body = if matches!(is_error, Some(true)) {
                    json!({ "error": content })
                } else {
                    json!({ "output": content })
                };
                response.insert("response".to_string(), body);
                if replayable_id(tool_use_id) {
                    response.insert("id".to_string(), json!(tool_use_id));
                }
                parts.push(json!({ "functionResponse": Value::Object(response) }));
            }
            // Reasoning, provider-native items, audio and the remaining
            // provider-specific blocks have no `Part` shape here.
            _ => {}
        }
    }
    parts
}

/// Whether a call id came from the server and may therefore be sent back.
fn replayable_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with(SYNTHETIC_CALL_ID_PREFIX)
}

/// The single `functionDeclarations` list this wire sends.
fn build_function_declarations(tools: &[ToolDefinition], mut sink: Option<&mut Vec<DegradationReceipt>>) -> Vec<Value> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut declarations = Vec::with_capacity(tools.len());
    // A deferred definition is advertised by a later turn rather than this
    // one, exactly as `request::build_tools_reporting` treats it.
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

        // Only a reporting caller pays for the collector; without a sink the
        // dialect takes its disabled logging path.
        let mut dropped: Vec<String> = Vec::new();
        let schema = if sink.is_some() {
            normalize_reporting(
                &tool.input_schema,
                &registry::GEMINI,
                &mut DropLog::enabled(&mut dropped),
            )
        } else {
            normalize(&tool.input_schema, &registry::GEMINI)
        };
        if let Some(receipts) = sink.as_deref_mut() {
            for keyword in dropped {
                receipts.push(DegradationReceipt::new(
                    DegradationKind::SchemaKeywordDropped,
                    tool.name.clone(),
                    keyword,
                ));
            }
        }

        let mut declaration = Map::new();
        declaration.insert("name".to_string(), json!(tool.name));
        declaration.insert("description".to_string(), json!(tool.description));
        // Gemini rejects a `parameters` object that declares no properties, so a
        // tool that takes no arguments carries no `parameters` at all rather
        // than an empty object schema.
        if declares_properties(&schema) {
            declaration.insert("parameters".to_string(), schema);
        }
        declarations.push(Value::Object(declaration));
    }
    declarations
}

/// Whether a schema declares at least one property.
fn declares_properties(schema: &Value) -> bool {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| !properties.is_empty())
}

/// The `generationConfig.thinkingConfig` value for `effort`, if this model
/// accepts one.
///
/// Two model generations express the same request differently: Gemini 2.5 takes
/// a token budget, and every other model takes a named level. A model that
/// matches neither the 2.5 name nor an effort word gets no config at all, since
/// sending one to a model that does not think is an error rather than a hint.
fn thinking_config(model: &str, effort: Option<&str>) -> Option<Value> {
    let effort = effort?.trim().to_ascii_lowercase();
    let stem = model
        .trim()
        .to_ascii_lowercase()
        .strip_prefix("models/")
        .map(str::to_string)
        .unwrap_or_else(|| model.trim().to_ascii_lowercase());

    if stem.starts_with("gemini-2.5") {
        let budget = match effort.as_str() {
            "minimal" => THINKING_BUDGET_MINIMAL,
            "low" => THINKING_BUDGET_LOW,
            "medium" => THINKING_BUDGET_MEDIUM,
            "high" | "xhigh" | "max" => THINKING_BUDGET_HIGH,
            _ => return None,
        };
        return Some(json!({ "thinkingBudget": budget, "includeThoughts": true }));
    }

    let level = match effort.as_str() {
        "minimal" => "MINIMAL",
        "low" => "LOW",
        "medium" => "MEDIUM",
        "high" => "HIGH",
        "xhigh" | "max" => "XHIGH",
        _ => return None,
    };
    Some(json!({ "thinkingLevel": level, "includeThoughts": true }))
}

/// Translates `GenerateContentResponse` chunks into shared stream events.
///
/// Each payload holds the parts added since the previous one, so nothing is
/// accumulated here: a text part becomes a delta as it arrives, and a call is
/// started, given its whole argument object and ended within the chunk that
/// carried it. The body ends without a marker, so `finish` is the only place the
/// turn is closed.
///
/// The reported stop reason is ASCII-lower-cased (`MAX_TOKENS` -> `max_tokens`,
/// `STOP` -> `stop`, `SAFETY` -> `safety`), because the server compares it
/// against lower-case lists and the other three wires already report lower case.
/// The two `*_UNSPECIFIED` sentinels are recognized before that step, in the
/// spelling the server sends.
pub struct GeminiParser {
    text_open: bool,
    thinking_open: bool,
    thinking_started: Option<Instant>,
    finish_reason: Option<String>,
    finished: bool,
    /// Namespace for the ids this parser invents, so two streams cannot collide.
    stamp: u128,
    next_call: u32,
}

impl GeminiParser {
    /// A parser for one stream.
    pub fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        Self {
            text_open: false,
            thinking_open: false,
            thinking_started: None,
            finish_reason: None,
            finished: false,
            stamp,
            next_call: 0,
        }
    }

    /// An id for a call the server sent without one.
    fn synthesize_id(&mut self) -> String {
        let id = format!("{SYNTHETIC_CALL_ID_PREFIX}{:x}_{}", self.stamp, self.next_call);
        self.next_call = self.next_call.saturating_add(1);
        id
    }

    /// One `GenerateContentResponse`, either a bare object or one element of a
    /// gateway's array wrapper.
    fn handle_chunk(&mut self, chunk: &Value, retry_body: &str, out: &mut VecDeque<StreamEvent>) {
        if let Some(error) = chunk.get("error") {
            close_thinking(self, out);
            close_text(self, out);
            out.push_back(StreamEvent::Error {
                message: format!("{LABEL} stream returned an error: {}", error_text(error)),
                retry_after_secs: retry_after_body_seconds(retry_body),
            });
            self.finished = true;
            return;
        }

        // Usage arrives on whichever chunk the server has settled a count for, so
        // it is reported every time it is present instead of only at the end.
        push_usage(chunk, out);

        let candidate = chunk
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first());

        if let Some(parts) = candidate
            .and_then(|candidate| candidate.get("content"))
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                self.handle_part(part, out);
            }
        }

        if let Some(reason) = candidate
            .and_then(|candidate| candidate.get("finishReason"))
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty() && *reason != FINISH_REASON_UNSPECIFIED)
        {
            self.finish_reason = Some(reason.to_ascii_lowercase());
        }

        // A prompt the server refused produces no candidate at all, so its block
        // reason is the only statement about why the turn ended.
        if self.finish_reason.is_none()
            && let Some(reason) = chunk
                .get("promptFeedback")
                .and_then(|feedback| feedback.get("blockReason"))
                .and_then(Value::as_str)
                .filter(|reason| !reason.is_empty() && *reason != BLOCK_REASON_UNSPECIFIED)
        {
            self.finish_reason = Some(reason.to_ascii_lowercase());
        }
    }

    /// One element of `candidates[0].content.parts`.
    fn handle_part(&mut self, part: &Value, out: &mut VecDeque<StreamEvent>) {
        if let Some(call) = part.get("functionCall").filter(|call| call.is_object()) {
            close_thinking(self, out);
            close_text(self, out);
            let name = call.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let id = match call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
                Some(id) => sanitize_tool_id(id),
                None => self.synthesize_id(),
            };
            out.push_back(StreamEvent::ToolUseStart { id: id.clone(), name });
            // The signature belongs to the part, not to the call, and the turn
            // loop has to store it so the next request can replay it.
            if let Some(signature) = part
                .get("thoughtSignature")
                .and_then(Value::as_str)
                .filter(|signature| !signature.is_empty())
            {
                out.push_back(StreamEvent::ToolUseSignatureFor {
                    id: id.clone(),
                    signature: signature.to_string(),
                });
            }
            let args = match call.get("args") {
                Some(args) if args.is_object() => args.clone(),
                _ => json!({}),
            };
            out.push_back(StreamEvent::ToolInputDeltaFor {
                id: id.clone(),
                delta: args.to_string(),
            });
            out.push_back(StreamEvent::ToolUseEndFor { id });
            return;
        }

        let Some(text) = part.get("text").and_then(Value::as_str) else {
            // Inline data, code execution results and anything else this wire
            // can report but the agent loop cannot read produce no event.
            return;
        };
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            if !self.thinking_open {
                close_text(self, out);
                self.thinking_open = true;
                self.thinking_started = Some(Instant::now());
                out.push_back(StreamEvent::ThinkingStart);
            }
            if !text.is_empty() {
                out.push_back(StreamEvent::ThinkingDelta(text.to_string()));
            }
            return;
        }
        close_thinking(self, out);
        if !text.is_empty() {
            self.text_open = true;
            out.push_back(StreamEvent::TextDelta(text.to_string()));
        }
    }
}

impl Default for GeminiParser {
    fn default() -> Self {
        Self::new()
    }
}

impl WireParser for GeminiParser {
    fn parse(&mut self, data: &str, out: &mut VecDeque<StreamEvent>) {
        if self.finished {
            return;
        }
        let payload = data.trim();
        if payload.is_empty() {
            return;
        }
        let value: Value = match serde_json::from_str(payload) {
            Ok(value) => value,
            Err(error) => {
                out.push_back(StreamEvent::Error {
                    message: format!(
                        "{LABEL} stream sent a malformed JSON chunk: {error}; payload={}",
                        snippet(payload, 400)
                    ),
                    retry_after_secs: None,
                });
                return;
            }
        };
        match value {
            // Some gateways wrap several chunks in one array payload; each
            // element is then handled exactly as if it had arrived alone.
            Value::Array(chunks) => {
                for chunk in &chunks {
                    if self.finished {
                        break;
                    }
                    let retry_body = chunk.to_string();
                    self.handle_chunk(chunk, &retry_body, out);
                }
            }
            chunk => self.handle_chunk(&chunk, payload, out),
        }
    }

    fn finish(&mut self, out: &mut VecDeque<StreamEvent>) {
        if self.finished {
            return;
        }
        self.finished = true;
        close_thinking(self, out);
        close_text(self, out);
        match self.finish_reason.take() {
            Some(reason) => out.push_back(StreamEvent::MessageEnd {
                stop_reason: Some(reason),
            }),
            None => out.push_back(StreamEvent::Error {
                message: format!("{LABEL} stream ended before the response finished (no finishReason)"),
                retry_after_secs: None,
            }),
        }
    }
}

/// Report this chunk's token counts, if it has any.
fn push_usage(chunk: &Value, out: &mut VecDeque<StreamEvent>) {
    let Some(usage) = chunk.get("usageMetadata").and_then(Value::as_object) else {
        return;
    };
    let input_tokens = usage.get("promptTokenCount").and_then(Value::as_u64);
    let candidates = usage.get("candidatesTokenCount").and_then(Value::as_u64);
    let thoughts = usage.get("thoughtsTokenCount").and_then(Value::as_u64);
    let cache_read_input_tokens = usage.get("cachedContentTokenCount").and_then(Value::as_u64);
    // Thinking is billed as output, and the server reports the two counts
    // separately; a missing one simply contributes nothing.
    let output_tokens = match (candidates, thoughts) {
        (None, None) => None,
        (candidates, thoughts) => Some(candidates.unwrap_or(0) + thoughts.unwrap_or(0)),
    };
    if input_tokens.is_none() && output_tokens.is_none() && cache_read_input_tokens.is_none() {
        return;
    }
    out.push_back(StreamEvent::TokenUsage {
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens: None,
    });
}

/// The text of a server error object: `STATUS: message` when both are known.
fn error_text(error: &Value) -> String {
    if let Some(text) = error.as_str() {
        return text.to_string();
    }
    if let Some(map) = error.as_object() {
        let kind = map
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| map.get("status").and_then(Value::as_str));
        let message = map.get("message").and_then(Value::as_str);
        match (kind, message) {
            (Some(kind), Some(message)) => return format!("{kind}: {message}"),
            (None, Some(message)) => return message.to_string(),
            _ => {}
        }
    }
    error.to_string()
}

/// Close an open thinking run, which text or a call always ends.
fn close_thinking(parser: &mut GeminiParser, out: &mut VecDeque<StreamEvent>) {
    if !parser.thinking_open {
        return;
    }
    parser.thinking_open = false;
    let duration_secs = parser
        .thinking_started
        .take()
        .map(|started| started.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    out.push_back(StreamEvent::ThinkingEnd);
    out.push_back(StreamEvent::ThinkingDone { duration_secs });
}

/// Close open text, which a call or the end of the turn always ends.
fn close_text(parser: &mut GeminiParser, out: &mut VecDeque<StreamEvent>) {
    if parser.text_open {
        parser.text_open = false;
        out.push_back(StreamEvent::TextDone);
    }
}

#[cfg(test)]
#[path = "gemini_tests.rs"]
mod gemini_tests;
