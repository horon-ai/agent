//! Incoming host commands: one JSON object per line on stdin.
//!
//! A line that is not a JSON object, or that names no `type`, is ignored: the
//! host may interleave blank lines and fragments while it starts up, and a
//! transport hiccup must not end the session. A line whose `type` is an unknown
//! command is answered with a failed response so the host learns what this
//! adapter does not implement; the connection stays open.

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;

pub(super) const A2UI_CAPABILITY_V1: &str = "a2ui_v1";
pub(super) const A2UI_VERSION_V1_0: &str = "1.0";
const A2UI_VERSION_V0_9: &str = "v0.9";
pub(super) const A2UI_BASIC_CATALOG_URI: &str = "https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct A2uiCapabilities {
    pub(super) version: String,
    pub(super) supported_catalog_ids: Vec<String>,
    pub(super) accepts_inline_catalogs: bool,
}

impl Default for A2uiCapabilities {
    fn default() -> Self {
        Self {
            version: A2UI_VERSION_V1_0.to_string(),
            supported_catalog_ids: vec![A2UI_BASIC_CATALOG_URI.to_string()],
            accepts_inline_catalogs: false,
        }
    }
}

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
    A2uiEvent,
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
            Self::A2uiEvent => "a2ui_event",
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
        "a2ui_event" => Kind::A2uiEvent,
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

    /// Parse opt-in `a2ui_v1` negotiation from `negotiate_protocol`.
    pub(super) fn a2ui_negotiation(&self) -> Result<Option<A2uiCapabilities>> {
        let (has_capabilities, requested_by_cap) = match self.raw.get("capabilities") {
            None | Some(Value::Null) => (false, false),
            Some(Value::Array(items)) => {
                let mut found = false;
                for item in items {
                    let Some(name) = item.as_str() else {
                        bail!("negotiate_protocol capabilities entries must be strings");
                    };
                    if name == A2UI_CAPABILITY_V1 {
                        found = true;
                    }
                }
                (true, found)
            }
            Some(_) => bail!("negotiate_protocol capabilities must be an array of strings"),
        };

        let a2ui_obj = match self.raw.get("a2ui") {
            None | Some(Value::Null) => None,
            Some(Value::Object(obj)) => Some(obj),
            Some(_) => bail!("negotiate_protocol a2ui capability must be an object"),
        };

        let enabled = requested_by_cap || (!has_capabilities && a2ui_obj.is_some());
        if !enabled {
            return Ok(None);
        }

        let Some(obj) = a2ui_obj else {
            return Ok(Some(A2uiCapabilities::default()));
        };

        let version = match obj.get("version") {
            None | Some(Value::Null) => A2UI_VERSION_V1_0.to_string(),
            Some(Value::String(raw)) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    A2UI_VERSION_V1_0.to_string()
                } else if trimmed == A2UI_VERSION_V1_0 || trimmed == A2UI_VERSION_V0_9 {
                    trimmed.to_string()
                } else {
                    bail!("unsupported A2UI version '{trimmed}'");
                }
            }
            Some(_) => bail!("negotiate_protocol a2ui.version must be a string"),
        };

        let supported_catalog_ids = match obj
            .get("supportedCatalogIds")
            .or_else(|| obj.get("supported_catalog_ids"))
        {
            None | Some(Value::Null) => vec![A2UI_BASIC_CATALOG_URI.to_string()],
            Some(Value::Array(items)) => {
                let mut ids = Vec::with_capacity(items.len());
                for item in items {
                    let Some(uri) = item.as_str() else {
                        bail!("negotiate_protocol a2ui.supportedCatalogIds entries must be strings");
                    };
                    let trimmed = uri.trim();
                    if trimmed.is_empty() {
                        bail!("negotiate_protocol a2ui.supportedCatalogIds entries must be non-empty");
                    }
                    ids.push(trimmed.to_string());
                }
                if ids.is_empty() {
                    vec![A2UI_BASIC_CATALOG_URI.to_string()]
                } else {
                    ids
                }
            }
            Some(_) => bail!("negotiate_protocol a2ui.supportedCatalogIds must be an array of strings"),
        };

        let accepts_inline_catalogs = match obj
            .get("acceptsInlineCatalogs")
            .or_else(|| obj.get("accepts_inline_catalogs"))
        {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => bail!("negotiate_protocol a2ui.acceptsInlineCatalogs must be a boolean"),
        };

        Ok(Some(A2uiCapabilities {
            version,
            supported_catalog_ids,
            accepts_inline_catalogs,
        }))
    }

    /// Parse outbound renderer-to-agent payload from `a2ui_event`.
    pub(super) fn a2ui_event_payload(&self) -> Result<(Value, Option<Value>)> {
        let Some(envelope) = self.raw.get("envelope") else {
            bail!("a2ui_event requires an envelope object");
        };
        if !envelope.is_object() {
            bail!("a2ui_event envelope must be a JSON object");
        }
        let data_model = match self.raw.get("dataModel").or_else(|| self.raw.get("data_model")) {
            None | Some(Value::Null) => None,
            Some(dm) if dm.is_object() => Some(dm.clone()),
            Some(_) => bail!("a2ui_event dataModel must be a JSON object"),
        };
        Ok((envelope.clone(), data_model))
    }
}
