//! Frames and the stdout writer for the stdio JSONL RPC mode.
//!
//! One frame is one JSON object on one line. The host reads stdout as protocol
//! only, so diagnostics go to stderr through the logging subsystem and nothing
//! else may be printed. A frame larger than [`MAX_FRAME_BYTES`] cannot be
//! delivered on a v1 stream (there is no chunking), so the emitter replaces it
//! with a `rpc_frame_error` frame instead of writing a line the host rejects.

use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Largest single frame either side may write, in bytes.
pub(super) const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Largest frame a chunking client may reassemble. Advertised only; this
/// adapter never chunks because it speaks version 1.
pub(super) const MAX_REASSEMBLED_FRAME_BYTES: usize = 64 * 1024 * 1024;
pub(super) const PROTOCOL_VERSION: u32 = 1;
/// The `api` value every assistant message carries: the only model protocol
/// this build speaks.
pub(super) const API_METHOD: &str = "openai-completions";

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// The first frame on stdout.
pub(super) fn ready() -> Value {
    json!({
        "type": "ready",
        "protocolVersion": PROTOCOL_VERSION,
        "supportedProtocolVersions": [PROTOCOL_VERSION],
        "maxFrameBytes": MAX_FRAME_BYTES,
        "maxReassembledFrameBytes": MAX_REASSEMBLED_FRAME_BYTES,
        "supportsBashExcludeFromContext": true,
    })
}

/// A successful `response`, with `data` only when the command produces one.
pub(super) fn response_ok(id: Option<&Value>, command: &str, data: Option<Value>) -> Value {
    let mut frame = json!({"type": "response", "command": command, "success": true});
    with_id(&mut frame, id);
    if let Some(data) = data {
        frame["data"] = data;
    }
    frame
}

pub(super) fn response_error(id: Option<&Value>, command: &str, error: &str, code: Option<&str>) -> Value {
    let mut frame = json!({
        "type": "response",
        "command": command,
        "success": false,
        "error": error,
    });
    with_id(&mut frame, id);
    if let Some(code) = code {
        frame["code"] = Value::String(code.to_string());
    }
    frame
}

fn with_id(frame: &mut Value, id: Option<&Value>) {
    if let Some(id) = id {
        frame["id"] = id.clone();
    }
}

/// A logical output that does not fit the active transport.
pub(super) fn frame_error(error: &str, original_type: Option<&str>) -> Value {
    let mut frame = json!({"type": "rpc_frame_error", "error": error});
    if let Some(kind) = original_type {
        frame["originalType"] = Value::String(kind.to_string());
    }
    frame
}

pub(super) fn prompt_result(id: Option<&Value>, status: &str, agent_invoked: bool) -> Value {
    let mut frame = json!({
        "type": "prompt_result",
        "agentInvoked": agent_invoked,
        "status": status,
        "sessionSettled": true,
    });
    with_id(&mut frame, id);
    frame
}

pub(super) fn session_settled() -> Value {
    json!({"type": "session_settled"})
}

pub(super) fn notice(level: &str, message: &str) -> Value {
    json!({"type": "notice", "level": level, "message": message})
}

pub(super) fn message_start(message: &Value, message_id: &str) -> Value {
    json!({"type": "message_start", "message": message, "messageId": message_id})
}

pub(super) fn message_end(message: &Value, message_id: &str) -> Value {
    json!({"type": "message_end", "message": message, "messageId": message_id})
}

pub(super) fn message_update(message: &Value, message_id: &str, event: Value) -> Value {
    json!({
        "type": "message_update",
        "message": message,
        "messageId": message_id,
        "assistantMessageEvent": event,
    })
}

/// Token usage in the shape the host expects. Cost is not modelled here, so
/// every cost field is zero; the host bills nothing for these turns.
pub(super) fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Value {
    let zero_cost = json!({"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0.0});
    json!({
        "input": input,
        "output": output,
        "cacheRead": cache_read,
        "cacheWrite": cache_write,
        "totalTokens": input + output,
        "cost": zero_cost,
    })
}

pub(super) fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

pub(super) fn thinking_block(thinking: &str) -> Value {
    json!({"type": "thinking", "thinking": thinking, "thinkingSignature": "reasoning_content", "summary": false})
}

pub(super) fn tool_call_block(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type": "toolCall", "id": id, "name": name, "arguments": arguments})
}

pub(super) fn user_message(text: &str, timestamp: i64) -> Value {
    json!({
        "role": "user",
        "content": [text_block(text)],
        "attribution": "user",
        "timestamp": timestamp,
    })
}

pub(super) fn tool_result_message(tool_call_id: &str, tool_name: &str, text: &str, timestamp: i64) -> Value {
    json!({
        "role": "toolResult",
        "toolCallId": tool_call_id,
        "toolName": tool_name,
        "content": [text_block(text)],
        "details": {},
        "timestamp": timestamp,
    })
}

pub(super) struct AssistantParts<'a> {
    pub(super) content: &'a [Value],
    pub(super) provider: &'a str,
    pub(super) model: &'a str,
    pub(super) usage: Value,
    pub(super) stop_reason: Option<&'a str>,
    pub(super) timestamp: i64,
    pub(super) response_id: Option<&'a str>,
    pub(super) duration_ms: Option<f64>,
    pub(super) ttft_ms: Option<f64>,
}

pub(super) fn assistant_message(parts: AssistantParts<'_>) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": parts.content,
        "api": API_METHOD,
        "provider": parts.provider,
        "model": parts.model,
        "usage": parts.usage,
        "stopReason": parts.stop_reason,
        "timestamp": parts.timestamp,
    });
    if let Some(response_id) = parts.response_id {
        message["responseId"] = Value::String(response_id.to_string());
    }
    if let Some(duration) = parts.duration_ms {
        message["duration"] = json!(duration);
    }
    if let Some(ttft) = parts.ttft_ms {
        message["ttft"] = json!(ttft);
    }
    message
}

/// Writes frames to stdout, one line each.
pub(super) struct Emitter<W> {
    out: W,
}

impl<W: AsyncWrite + Unpin> Emitter<W> {
    pub(super) fn new(out: W) -> Self {
        Self { out }
    }

    /// Write one frame and flush it. A frame over the transport limit becomes
    /// a `rpc_frame_error` so the host learns why the output is missing
    /// instead of reading a truncated line.
    pub(super) async fn send(&mut self, frame: &Value) -> std::io::Result<()> {
        let mut line = frame.to_string();
        if line.len() >= MAX_FRAME_BYTES {
            let kind = frame.get("type").and_then(Value::as_str);
            let error = format!(
                "frame of {} bytes exceeds maxFrameBytes ({MAX_FRAME_BYTES}); v1 has no chunking",
                line.len()
            );
            line = frame_error(&error, kind).to_string();
        }
        line.push('\n');
        self.out.write_all(line.as_bytes()).await?;
        self.out.flush().await
    }

    pub(super) async fn send_all(&mut self, frames: Vec<Value>) -> std::io::Result<()> {
        for frame in frames {
            self.send(&frame).await?;
        }
        Ok(())
    }
}
