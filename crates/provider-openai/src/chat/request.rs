//! OpenAI Responses -> Chat Completions request translation.
//!
//! horon-agent assembles the conversation in the Responses shape
//! ([`crate::request::build_responses_input`]) and the tool catalog with
//! [`crate::request::build_tools`]. A direct Chat Completions endpoint needs the
//! same information in its own shape, so this module is the single translation
//! point for a request body.

use horon_agent_message_types::degradation::{DegradationKind, DegradationReceipt};
use horon_agent_message_types::{Message, ToolDefinition, sanitize_tool_id};
use serde_json::{Value, json};
use std::collections::HashSet;

/// Path appended to a provider `base_url` for one streaming completion.
pub const CHAT_COMPLETIONS_PATH: &str = "chat/completions";

/// Build a streaming Chat Completions request body.
///
/// `messages` and `tools` are the agent's canonical inputs: the conversation is
/// serialized once through the existing Responses input builder, then translated
/// to chat messages, so the direct provider and the Responses serializer can
/// never drift apart on call ordering or missing tool outputs.
pub fn build_chat_completions_request(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    system: &str,
    reasoning_effort: Option<&str>,
) -> Value {
    build_chat_completions_request_reporting(model, messages, tools, system, reasoning_effort, None)
}

/// [`build_chat_completions_request`], also reporting every degradation the
/// tool catalog suffered on the way into the body.
pub fn build_chat_completions_request_reporting(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    system: &str,
    reasoning_effort: Option<&str>,
    mut sink: Option<&mut Vec<DegradationReceipt>>,
) -> Value {
    let input = crate::request::build_responses_input(messages);
    let tool_definitions = build_chat_tools_reporting(tools, sink.as_deref_mut());

    let mut chat_messages = Vec::with_capacity(input.len() + 1);
    if !system.trim().is_empty() {
        chat_messages.push(json!({ "role": "system", "content": system }));
    }
    chat_messages.extend(responses_input_to_chat_messages(&input));

    let mut request = json!({
        "model": model,
        "messages": chat_messages,
        "stream": true,
        // OpenAI only emits a usage object for a streamed completion when this
        // is asked for; compatible servers that do not know the option ignore it.
        "stream_options": { "include_usage": true },
    });
    if !tool_definitions.is_empty() {
        request["tools"] = Value::Array(tool_definitions);
        request["tool_choice"] = json!("auto");
    }
    if let Some(effort) = reasoning_effort.filter(|effort| !effort.trim().is_empty()) {
        request["reasoning_effort"] = json!(effort);
    }
    request
}

/// Translate Responses input items into Chat Completions messages.
///
/// Handles the item kinds [`crate::request::build_responses_input`] emits:
/// `message`, `function_call`/`custom_tool_call` and
/// `function_call_output`/`custom_tool_call_output`. Items that only exist in
/// the Responses protocol (`reasoning`, `web_search_call`, `compaction`,
/// `additional_tools`) have no chat equivalent here and are dropped: a chat
/// endpoint has no point-of-use definition loading, and this provider reports
/// `supports_deferred_tools() == false`, so the agent never routes deferred
/// definitions through it.
pub fn responses_input_to_chat_messages(input: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(input.len());
    let mut assistant: Option<Value> = None;

    for item in input {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                let content = chat_message_content(item.get("content"));
                if role == "assistant" {
                    // Keep the text in a pending assistant message so a following
                    // function_call attaches to this turn instead of opening a
                    // second assistant message before the tool output.
                    flush_assistant(&mut assistant, &mut out);
                    assistant = Some(json!({ "role": "assistant", "content": content }));
                } else {
                    flush_assistant(&mut assistant, &mut out);
                    if !content.is_null() {
                        out.push(json!({ "role": role, "content": content }));
                    }
                }
            }
            Some("function_call") | Some("custom_tool_call") => {
                let call = chat_tool_call(item);
                let message = assistant.get_or_insert_with(|| json!({ "role": "assistant", "content": Value::Null }));
                push_tool_call(message, call);
            }
            Some("function_call_output") | Some("custom_tool_call_output") => {
                flush_assistant(&mut assistant, &mut out);
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(sanitize_tool_id)
                    .unwrap_or_else(|| "unknown".to_string());
                let content = tool_output_text(item.get("output"));
                out.push(json!({ "role": "tool", "tool_call_id": call_id, "content": content }));
            }
            _ => {}
        }
    }
    flush_assistant(&mut assistant, &mut out);
    out
}

/// Convert the Responses tool definitions built by [`crate::request::build_tools`]
/// into Chat Completions `tools[].function` entries.
pub fn build_chat_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    build_chat_tools_reporting(tools, None)
}

/// [`build_chat_tools`], also reporting the definitions a chat endpoint cannot
/// carry.
///
/// Two losses are possible here beyond the schema narrowing [`crate::request::build_tools`]
/// already reports: a definition with no name, and a second definition of a name
/// an earlier entry claimed. Only a duplicate is attributable to a tool, so only
/// a duplicate is reported.
pub fn build_chat_tools_reporting(
    tools: &[ToolDefinition],
    mut sink: Option<&mut Vec<DegradationReceipt>>,
) -> Vec<Value> {
    let declared = crate::request::build_tools_reporting(tools, sink.as_deref_mut());
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(declared.len());
    for definition in declared.iter().filter_map(chat_tool_definition) {
        let Some(name) = definition["function"]["name"].as_str().map(str::to_string) else {
            continue;
        };
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
        out.push(definition);
    }
    out
}

/// Convert one Responses tool declaration into a Chat Completions tool entry.
pub fn chat_tool_definition(tool: &Value) -> Option<Value> {
    let name = tool.get("name")?.as_str()?;
    let description = tool.get("description").cloned().unwrap_or(Value::String(String::new()));
    let parameters = tool
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    let mut function = json!({
        "name": name,
        "description": description,
        "parameters": parameters,
    });
    if let Some(strict) = tool.get("strict") {
        function["strict"] = strict.clone();
    }
    Some(json!({ "type": "function", "function": function }))
}

fn flush_assistant(assistant: &mut Option<Value>, out: &mut Vec<Value>) {
    let Some(message) = assistant.take() else {
        return;
    };
    let has_tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    let has_content = message.get("content").is_some_and(|content| !content.is_null());
    if has_tool_calls || has_content {
        out.push(message);
    }
}

fn push_tool_call(message: &mut Value, call: Value) {
    let object = message.as_object_mut().expect("assistant message is an object");
    let calls = object.entry("tool_calls").or_insert_with(|| Value::Array(Vec::new()));
    if let Some(calls) = calls.as_array_mut() {
        calls.push(call);
    }
}

fn chat_tool_call(item: &Value) -> Value {
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .or_else(|| item.get("id").and_then(Value::as_str))
        .map(sanitize_tool_id)
        .unwrap_or_else(|| "unknown".to_string());
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let arguments = match item.get("arguments").or_else(|| item.get("input")) {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => "{}".to_string(),
    };
    json!({
        "id": call_id,
        "type": "function",
        "function": { "name": name, "arguments": arguments },
    })
}

fn chat_message_content(content: Option<&Value>) -> Value {
    let Some(content) = content else {
        return Value::Null;
    };
    if let Some(text) = content.as_str() {
        return Value::String(text.to_string());
    }
    let Some(parts) = content.as_array() else {
        return Value::Null;
    };

    let mut texts: Vec<&str> = Vec::new();
    let mut chat_parts: Vec<Value> = Vec::new();
    let mut has_media = false;
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("input_text") | Some("output_text") | Some("text") => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    texts.push(text);
                    chat_parts.push(json!({ "type": "text", "text": text }));
                }
            }
            Some("input_image") => {
                if let Some(url) = part.get("image_url").and_then(image_url_string) {
                    has_media = true;
                    chat_parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                }
            }
            Some("input_audio") => {
                if let Some(audio) = part.get("input_audio") {
                    has_media = true;
                    chat_parts.push(json!({ "type": "input_audio", "input_audio": audio }));
                }
            }
            _ => {}
        }
    }

    if has_media {
        Value::Array(chat_parts)
    } else if texts.len() > 1 {
        Value::String(texts.join("\n"))
    } else if let Some(text) = texts.first() {
        Value::String((*text).to_string())
    } else {
        Value::Null
    }
}

fn image_url_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.get("url").and_then(Value::as_str).map(str::to_string))
}

fn tool_output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => {
            let mut text = String::new();
            for part in parts {
                if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                    text.push_str(chunk);
                }
            }
            text
        }
        Some(other) => other.to_string(),
        None => String::new(),
    }
}
