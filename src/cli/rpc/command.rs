//! Incoming host commands: one JSON object per line on stdin.
//!
//! A line that is not a JSON object, or that names no `type`, is ignored: the
//! host may interleave blank lines and fragments while it starts up, and a
//! transport hiccup must not end the session. A line whose `type` is an unknown
//! command is answered with a failed response so the host learns what this
//! adapter does not implement; the connection stays open.

use serde_json::Value;

/// The framing limit for one inbound line, matching the limit advertised in
/// `ready`. A longer line cannot be parsed as one frame, so its `id` (if the
/// readable prefix carries one) is failed instead.
pub(super) const MAX_INBOUND_FRAME_BYTES: usize = super::frame::MAX_FRAME_BYTES;

#[derive(Clone, Debug)]
pub(super) struct Command {
    /// Echoed back in the response. Any JSON value, absent when the host sent
    /// none.
    pub(super) id: Option<Value>,
    pub(super) kind: Kind,
    /// The whole frame, for field lookup.
    pub(super) raw: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    NegotiateProtocol,
    Prompt,
    Steer,
    FollowUp,
    Abort,
    AbortAndPrompt,
    NewSession,
    GetState,
    GetAvailableModels,
    SetModel,
    SetThinkingLevel,
    GetAvailableCommands,
    GetSessionStats,
    SetTodos,
    SetSessionName,
    Compact,
    GetMessages,
    Bash,
    AbortBash,
    /// A command this adapter does not implement. Answered with a failure; the
    /// connection stays open.
    Unsupported(String),
}

impl Kind {
    /// The name echoed in `response.command`.
    pub(super) fn name(&self) -> &str {
        match self {
            Self::NegotiateProtocol => "negotiate_protocol",
            Self::Prompt => "prompt",
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
            Self::Abort => "abort",
            Self::AbortAndPrompt => "abort_and_prompt",
            Self::NewSession => "new_session",
            Self::GetState => "get_state",
            Self::GetAvailableModels => "get_available_models",
            Self::SetModel => "set_model",
            Self::SetThinkingLevel => "set_thinking_level",
            Self::GetAvailableCommands => "get_available_commands",
            Self::GetSessionStats => "get_session_stats",
            Self::SetTodos => "set_todos",
            Self::SetSessionName => "set_session_name",
            Self::Compact => "compact",
            Self::GetMessages => "get_messages",
            Self::Bash => "bash",
            Self::AbortBash => "abort_bash",
            Self::Unsupported(name) => name,
        }
    }
}

/// Parse one inbound line. `None` means the line carries no command and must
/// not be answered.
pub(super) fn parse(line: &str) -> Option<Command> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let raw: Value = match serde_json::from_str(trimmed) {
        Ok(raw) => raw,
        Err(_) => return None,
    };
    if !raw.is_object() {
        return None;
    }
    let kind = raw.get("type")?.as_str()?;
    let kind = match kind {
        "negotiate_protocol" => Kind::NegotiateProtocol,
        "prompt" => Kind::Prompt,
        "steer" => Kind::Steer,
        "follow_up" => Kind::FollowUp,
        "abort" => Kind::Abort,
        "abort_and_prompt" => Kind::AbortAndPrompt,
        "new_session" => Kind::NewSession,
        "get_state" => Kind::GetState,
        "get_available_models" => Kind::GetAvailableModels,
        "set_model" => Kind::SetModel,
        "set_thinking_level" => Kind::SetThinkingLevel,
        "get_available_commands" => Kind::GetAvailableCommands,
        "get_session_stats" => Kind::GetSessionStats,
        "set_todos" => Kind::SetTodos,
        "set_session_name" => Kind::SetSessionName,
        "compact" => Kind::Compact,
        "get_messages" => Kind::GetMessages,
        "bash" => Kind::Bash,
        "abort_bash" => Kind::AbortBash,
        other => Kind::Unsupported(other.to_string()),
    };
    Some(Command {
        id: raw.get("id").cloned(),
        kind,
        raw,
    })
}

impl Command {
    pub(super) fn message(&self) -> Option<&str> {
        self.raw.get("message").and_then(Value::as_str)
    }

    /// `images` as `(mime, base64)` pairs, the daemon's wire shape.
    pub(super) fn images(&self) -> Vec<(String, String)> {
        let Some(items) = self.raw.get("images").and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut images = Vec::new();
        for item in items {
            let Some(mime) = item
                .get("mimeType")
                .or_else(|| item.get("mime"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some(data) = item.get("data").and_then(Value::as_str) else {
                continue;
            };
            images.push((mime.to_string(), data.to_string()));
        }
        images
    }

    pub(super) fn string_field(&self, key: &str) -> Option<&str> {
        self.raw.get(key).and_then(Value::as_str)
    }
}
