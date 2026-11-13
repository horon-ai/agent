//! Maps daemon events onto the host's stream frames.
//!
//! The host's order for one step is fixed: `turn_start`, the assistant
//! `message_start`, its content updates, its `message_end`, the tool executions
//! with their result messages, then `turn_end`. The daemon streams a tool call's
//! arguments *before* the response ends, so the executions and their result
//! messages are held back until the assistant message closes. That is the only
//! reordering here, and it exists to keep the host's step boundary intact.

use crate::protocol::ServerEvent;
use serde_json::{Value, json};
use std::collections::HashSet;

use super::frame::{self, AssistantParts};

#[derive(Debug)]
enum Block {
    Thinking(String),
    Text(String),
    ToolCall { id: String, name: String, args: String },
}

/// A tool call whose execution frames wait for the assistant `message_end`.
#[derive(Debug)]
struct Execution {
    tool_call_id: String,
    tool_name: String,
    args: Value,
    updates: Vec<String>,
    result: Option<(String, bool)>,
}

struct Assistant {
    message_id: String,
    timestamp: i64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_write: u64,
}

/// Per-turn state: the assistant message being built, the executions waiting to
/// be reported, and the message ids the host sees.
pub(super) struct Turn {
    provider: String,
    model: String,
    message_seq: u64,
    assistant: Option<Assistant>,
    blocks: Vec<Block>,
    started_blocks: HashSet<usize>,
    executions: Vec<Execution>,
    agent_started: bool,
    turn_open: bool,
    messages: Vec<Value>,
}

impl Turn {
    pub(super) fn new(provider: String, model: String, message_seq: u64) -> Self {
        Self {
            provider,
            model,
            message_seq,
            assistant: None,
            blocks: Vec::new(),
            started_blocks: HashSet::new(),
            executions: Vec::new(),
            agent_started: false,
            turn_open: false,
            messages: Vec::new(),
        }
    }

    /// The last message id handed out, so the next turn keeps counting up.
    pub(super) fn message_seq(&self) -> u64 {
        self.message_seq
    }

    /// Record the prompt as the host's user message and announce the run.
    pub(super) fn begin(&mut self, prompt: &str) -> Vec<Value> {
        let mut frames = Vec::new();
        if !self.agent_started {
            frames.push(json!({"type": "agent_start"}));
            self.agent_started = true;
        }
        let user = frame::user_message(prompt, frame::now_ms());
        self.messages.push(user.clone());
        let id = self.next_message_id();
        frames.push(frame::message_start(&user, &id));
        frames.push(frame::message_end(&user, &id));
        frames.push(json!({"type": "turn_start"}));
        self.turn_open = true;
        frames
    }

    /// Frames for one daemon event. An event this mode does not forward yields
    /// nothing.
    pub(super) fn event(&mut self, event: ServerEvent) -> Vec<Value> {
        match event {
            ServerEvent::ReasoningDelta { text } => self.thinking_delta(text),
            ServerEvent::ReasoningDone { .. } => self.thinking_end(),
            ServerEvent::TextDelta { text } => self.text_delta(text),
            ServerEvent::TextDone => self.text_end(),
            // A rewrite of text already streamed. The host has no frame that
            // retracts output, so only the buffer that later frames carry
            // follows the daemon.
            ServerEvent::TextReplace { text } => {
                self.replace_text(&text);
                Vec::new()
            }
            ServerEvent::TextSuffixReplace { old_text, new_text } => {
                self.replace_text_suffix(&old_text, &new_text);
                Vec::new()
            }
            ServerEvent::ToolStart { id, name } => self.tool_start(id, name),
            ServerEvent::ToolInput { id, delta } => self.tool_input(id, delta),
            ServerEvent::ToolExec { id, .. } => self.tool_exec(&id),
            ServerEvent::ToolOutputDelta { id, data } => self.tool_output_delta(&id, &data),
            ServerEvent::ToolDone {
                id,
                name,
                output,
                error,
                ..
            } => self.tool_done(id, name, output, error.is_some()),
            ServerEvent::TokenUsage {
                input,
                output,
                cache_read_input,
                cache_creation_input,
            } => {
                if let Some(assistant) = self.assistant.as_mut() {
                    assistant.input_tokens = input;
                    assistant.output_tokens = output;
                    assistant.cache_read = cache_read_input.unwrap_or(0);
                    assistant.cache_write = cache_creation_input.unwrap_or(0);
                }
                Vec::new()
            }
            ServerEvent::MessageEnd { stop_reason } => self.close(stop_reason.as_deref()),
            ServerEvent::SessionRenamed { session_id, title, .. } => vec![json!({
                "type": "session_info_update",
                "sessionId": session_id,
                "title": title.unwrap_or_default(),
            })],
            ServerEvent::ModelChanged { model, .. } => {
                self.model = model;
                vec![json!({"type": "model_changed"})]
            }
            ServerEvent::ReasoningEffortChanged { effort, .. } => vec![json!({
                "type": "thinking_level_changed",
                "thinkingLevel": effort,
            })],
            ServerEvent::ProviderGuardrail { stop_reason } => vec![frame::notice(
                "warning",
                &format!(
                    "the provider ended the turn without visible output ({})",
                    stop_reason.unwrap_or_else(|| "guardrail".to_string())
                ),
            )],
            ServerEvent::Compaction { .. } => vec![frame::notice("info", "context compacted")],
            _ => Vec::new(),
        }
    }

    /// Close whatever is open and end the run.
    pub(super) fn finish(&mut self) -> Vec<Value> {
        let mut frames = self.close(None);
        frames.push(json!({
            "type": "agent_end",
            "messages": self.messages,
            "isTerminal": true,
        }));
        frames
    }

    fn next_message_id(&mut self) -> String {
        self.message_seq += 1;
        format!("msg-{}", self.message_seq)
    }

    /// Open the assistant message for this step, if it is not open yet. A step
    /// that starts after a tool round opens a new `turn_start` first.
    fn ensure_open(&mut self) -> Vec<Value> {
        if self.assistant.is_some() {
            return Vec::new();
        }
        let mut frames = Vec::new();
        if !self.turn_open {
            frames.push(json!({"type": "turn_start"}));
            self.turn_open = true;
        }
        self.blocks.clear();
        self.started_blocks.clear();
        let message_id = self.next_message_id();
        self.assistant = Some(Assistant {
            message_id: message_id.clone(),
            timestamp: frame::now_ms(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read: 0,
            cache_write: 0,
        });
        let partial = self.partial();
        frames.push(frame::message_start(&partial, &message_id));
        frames
    }

    fn partial(&self) -> Value {
        let blocks: Vec<Value> = self.blocks.iter().map(block_json).collect();
        let (timestamp, usage) = match self.assistant.as_ref() {
            Some(assistant) => (
                assistant.timestamp,
                frame::usage(
                    assistant.input_tokens,
                    assistant.output_tokens,
                    assistant.cache_read,
                    assistant.cache_write,
                ),
            ),
            None => (frame::now_ms(), frame::usage(0, 0, 0, 0)),
        };
        frame::assistant_message(AssistantParts {
            content: &blocks,
            provider: &self.provider,
            model: &self.model,
            usage,
            stop_reason: None,
            timestamp,
            response_id: None,
            duration_ms: None,
            ttft_ms: None,
        })
    }

    fn message_id(&self) -> String {
        self.assistant
            .as_ref()
            .map(|assistant| assistant.message_id.clone())
            .unwrap_or_default()
    }

    fn update(&self, event: Value) -> Vec<Value> {
        vec![frame::message_update(&self.partial(), &self.message_id(), event)]
    }

    fn thinking_index(&mut self) -> usize {
        match self.blocks.iter().position(|block| matches!(block, Block::Thinking(_))) {
            Some(index) => index,
            None => {
                self.blocks.push(Block::Thinking(String::new()));
                self.blocks.len() - 1
            }
        }
    }

    fn text_index(&mut self) -> usize {
        if matches!(self.blocks.last(), Some(Block::Text(_))) {
            return self.blocks.len() - 1;
        }
        self.blocks.push(Block::Text(String::new()));
        self.blocks.len() - 1
    }

    fn thinking_delta(&mut self, text: String) -> Vec<Value> {
        let mut frames = self.ensure_open();
        let index = self.thinking_index();
        if let Some(Block::Thinking(buffer)) = self.blocks.get_mut(index) {
            buffer.push_str(&text);
        }
        if self.started_blocks.insert(index) {
            frames.extend(self.update(json!({
                "type": "thinking_start",
                "contentIndex": index,
                "partial": self.partial(),
            })));
        }
        frames.extend(self.update(json!({
            "type": "thinking_delta",
            "contentIndex": index,
            "delta": text,
            "partial": self.partial(),
        })));
        frames
    }

    fn thinking_end(&mut self) -> Vec<Value> {
        if self.assistant.is_none() {
            return Vec::new();
        }
        let Some(index) = self.blocks.iter().position(|block| matches!(block, Block::Thinking(_))) else {
            return Vec::new();
        };
        let content = match self.blocks.get(index) {
            Some(Block::Thinking(text)) => text.clone(),
            _ => String::new(),
        };
        self.update(json!({
            "type": "thinking_end",
            "contentIndex": index,
            "content": content,
            "partial": self.partial(),
        }))
    }

    fn text_delta(&mut self, text: String) -> Vec<Value> {
        let mut frames = self.ensure_open();
        let index = self.text_index();
        if let Some(Block::Text(buffer)) = self.blocks.get_mut(index) {
            buffer.push_str(&text);
        }
        if self.started_blocks.insert(index) {
            frames.extend(self.update(json!({
                "type": "text_start",
                "contentIndex": index,
                "partial": self.partial(),
            })));
        }
        frames.extend(self.update(json!({
            "type": "text_delta",
            "contentIndex": index,
            "delta": text,
            "partial": self.partial(),
        })));
        frames
    }

    fn text_end(&mut self) -> Vec<Value> {
        if self.assistant.is_none() {
            return Vec::new();
        }
        let Some(index) = self.blocks.iter().rposition(|block| matches!(block, Block::Text(_))) else {
            return Vec::new();
        };
        let content = match self.blocks.get(index) {
            Some(Block::Text(text)) => text.clone(),
            _ => String::new(),
        };
        self.update(json!({
            "type": "text_end",
            "contentIndex": index,
            "content": content,
            "partial": self.partial(),
        }))
    }

    fn tool_start(&mut self, id: String, name: String) -> Vec<Value> {
        let mut frames = self.ensure_open();
        self.blocks.push(Block::ToolCall {
            id: id.clone(),
            name: name.clone(),
            args: String::new(),
        });
        let index = self.blocks.len() - 1;
        self.started_blocks.insert(index);
        self.executions.push(Execution {
            tool_call_id: id,
            tool_name: name,
            args: Value::Null,
            updates: Vec::new(),
            result: None,
        });
        frames.extend(self.update(json!({
            "type": "toolcall_start",
            "contentIndex": index,
            "partial": self.partial(),
        })));
        frames
    }

    fn tool_input(&mut self, id: Option<String>, delta: String) -> Vec<Value> {
        if self.assistant.is_none() {
            return Vec::new();
        }
        let Some(target) = id.or_else(|| self.current_tool_id()) else {
            return Vec::new();
        };
        let Some(index) = self.tool_block_index(&target) else {
            return Vec::new();
        };
        if let Some(Block::ToolCall { args, .. }) = self.blocks.get_mut(index) {
            args.push_str(&delta);
        }
        self.update(json!({
            "type": "toolcall_delta",
            "contentIndex": index,
            "delta": delta,
            "partial": self.partial(),
        }))
    }

    fn tool_exec(&mut self, id: &str) -> Vec<Value> {
        if self.assistant.is_none() {
            return Vec::new();
        }
        let Some(index) = self.tool_block_index(id) else {
            return Vec::new();
        };
        let (name, arguments) = match self.blocks.get(index) {
            Some(Block::ToolCall { name, args, .. }) => (name.clone(), parse_arguments(args)),
            _ => return Vec::new(),
        };
        if let Some(execution) = self
            .executions
            .iter_mut()
            .find(|execution| execution.tool_call_id == id)
        {
            execution.args = arguments.clone();
        }
        self.update(json!({
            "type": "toolcall_end",
            "contentIndex": index,
            "toolCall": frame::tool_call_block(id, &name, arguments),
            "partial": self.partial(),
        }))
    }

    /// Output a running tool produced while it is still running. The host sees
    /// it as a `tool_execution_update` on the call's own block.
    fn tool_output_delta(&mut self, id: &str, data: &str) -> Vec<Value> {
        let Some(execution) = self
            .executions
            .iter_mut()
            .find(|execution| execution.tool_call_id == id)
        else {
            return Vec::new();
        };
        let text = decode_base64(data);
        execution.updates.push(text.clone());
        vec![json!({
            "type": "tool_execution_update",
            "toolCallId": id,
            "toolName": execution.tool_name,
            "args": execution.args,
            "partialResult": {"content": [frame::text_block(&text)], "details": {}},
        })]
    }

    fn tool_done(&mut self, id: String, name: String, output: String, is_error: bool) -> Vec<Value> {
        if let Some(execution) = self
            .executions
            .iter_mut()
            .find(|execution| execution.tool_call_id == id)
        {
            execution.result = Some((output, is_error));
            return Vec::new();
        }
        // A result for a call this turn never streamed (a tool the daemon ran
        // on its own) is reported on its own, after the current message.
        self.executions.push(Execution {
            tool_call_id: id,
            tool_name: name,
            args: Value::Null,
            updates: Vec::new(),
            result: Some((output, is_error)),
        });
        Vec::new()
    }

    fn replace_text(&mut self, text: &str) {
        if let Some(index) = self.blocks.iter().rposition(|block| matches!(block, Block::Text(_)))
            && let Some(Block::Text(buffer)) = self.blocks.get_mut(index)
        {
            buffer.clear();
            buffer.push_str(text);
        }
    }

    fn replace_text_suffix(&mut self, old_text: &str, new_text: &str) {
        if let Some(index) = self.blocks.iter().rposition(|block| matches!(block, Block::Text(_)))
            && let Some(Block::Text(buffer)) = self.blocks.get_mut(index)
            && let Some(stripped) = buffer.strip_suffix(old_text)
        {
            let mut replaced = stripped.to_string();
            replaced.push_str(new_text);
            *buffer = replaced;
        }
    }

    /// Close the assistant message: `message_end`, the deferred executions with
    /// their result messages, then `turn_end`.
    fn close(&mut self, stop_reason: Option<&str>) -> Vec<Value> {
        if self.assistant.is_none() {
            return Vec::new();
        }
        let blocks: Vec<Value> = self.blocks.iter().map(block_json).collect();
        let has_tool_call = blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"));
        let stop_reason = stop_reason.unwrap_or(if has_tool_call { "toolUse" } else { "stop" });
        let message_id = self.message_id();
        let (timestamp, usage) = match self.assistant.as_ref() {
            Some(assistant) => (
                assistant.timestamp,
                frame::usage(
                    assistant.input_tokens,
                    assistant.output_tokens,
                    assistant.cache_read,
                    assistant.cache_write,
                ),
            ),
            None => (frame::now_ms(), frame::usage(0, 0, 0, 0)),
        };
        let message = frame::assistant_message(AssistantParts {
            content: &blocks,
            provider: &self.provider,
            model: &self.model,
            usage,
            stop_reason: Some(stop_reason),
            timestamp,
            response_id: None,
            duration_ms: None,
            ttft_ms: None,
        });
        let mut frames = vec![frame::message_end(&message, &message_id)];
        self.messages.push(message.clone());

        let mut tool_results = Vec::new();
        for execution in std::mem::take(&mut self.executions) {
            frames.push(json!({
                "type": "tool_execution_start",
                "toolCallId": execution.tool_call_id,
                "toolName": execution.tool_name,
                "args": execution.args,
            }));
            for update in &execution.updates {
                frames.push(json!({
                    "type": "tool_execution_update",
                    "toolCallId": execution.tool_call_id,
                    "toolName": execution.tool_name,
                    "args": execution.args,
                    "partialResult": {"content": [frame::text_block(update)], "details": {}},
                }));
            }
            let (output, is_error) = execution.result.clone().unwrap_or_default();
            frames.push(json!({
                "type": "tool_execution_end",
                "toolCallId": execution.tool_call_id,
                "toolName": execution.tool_name,
                "result": {"content": [frame::text_block(&output)], "details": {}},
                "isError": is_error,
            }));
            let result_message =
                frame::tool_result_message(&execution.tool_call_id, &execution.tool_name, &output, frame::now_ms());
            let result_id = self.next_message_id();
            frames.push(frame::message_start(&result_message, &result_id));
            frames.push(frame::message_end(&result_message, &result_id));
            self.messages.push(result_message.clone());
            tool_results.push(result_message);
        }

        frames.push(json!({
            "type": "turn_end",
            "message": message,
            "toolResults": tool_results,
        }));
        self.assistant = None;
        self.blocks.clear();
        self.started_blocks.clear();
        self.turn_open = false;
        frames
    }

    fn current_tool_id(&self) -> Option<String> {
        self.blocks.iter().rev().find_map(|block| match block {
            Block::ToolCall { id, .. } => Some(id.clone()),
            _ => None,
        })
    }

    fn tool_block_index(&self, id: &str) -> Option<usize> {
        self.blocks.iter().position(|block| match block {
            Block::ToolCall { id: block_id, .. } => block_id == id,
            _ => false,
        })
    }
}

fn block_json(block: &Block) -> Value {
    match block {
        Block::Thinking(text) => frame::thinking_block(text),
        Block::Text(text) => frame::text_block(text),
        Block::ToolCall { id, name, args } => frame::tool_call_block(id, name, parse_arguments(args)),
    }
}

fn parse_arguments(args: &str) -> Value {
    if args.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(args).unwrap_or_else(|_| json!({"raw": args}))
}

/// Tool output arrives base64-encoded because it is raw bytes. Output that is
/// not UTF-8 is reported lossily rather than dropped: the host shows text and
/// this build never writes binary to a tool's stdout on purpose.
fn decode_base64(data: &str) -> String {
    use base64::Engine;
    match base64::engine::general_purpose::STANDARD.decode(data) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}
