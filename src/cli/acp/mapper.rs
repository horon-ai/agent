//! Maps the daemon events of one prompt turn to ACP `session/update` payloads.
//!
//! One mapper lives for exactly one turn. It owns the per-turn presentation
//! state: message ids (a new message starts after every tool round), the
//! streamed tool input, the file diffs a tool reported, and the compaction
//! counter. `notice` and `compaction_update` are produced only when the client
//! advertised `session.notices` / `session.compaction`.

use super::*;
use crate::protocol::{FileDiffWire, PlanEntryWire, TurnStopReason};
use std::path::Path;

const LOCATION_KEYS: [&str; 3] = ["file_path", "path", "filePath"];

#[derive(Clone, Debug, Default)]
pub(super) struct MapperOptions {
    /// The session's absolute working directory; relative tool paths resolve
    /// against it.
    pub(super) cwd: Option<PathBuf>,
    pub(super) notices: bool,
    pub(super) compaction: bool,
}

impl MapperOptions {
    pub(super) fn new(client_capabilities: &Value, cwd: Option<PathBuf>) -> Self {
        let advertised = |name: &str| {
            client_capabilities
                .get("session")
                .and_then(|session| session.get(name))
                .is_some_and(Value::is_object)
        };
        Self {
            cwd,
            notices: advertised("notices"),
            compaction: advertised("compaction"),
        }
    }
}

pub(super) struct EventMapper {
    profile: AcpProfile,
    options: MapperOptions,
    /// Opaque per-turn prefix of every message and compaction id.
    turn: String,
    /// Bumped once a tool round has happened, so the next chunk opens a new
    /// message.
    round: u64,
    round_pending: bool,
    compactions: u64,
    current_tool_id: Option<String>,
    tool_inputs: HashMap<String, String>,
    tool_diffs: HashMap<String, Vec<Value>>,
    tool_terminals: HashMap<String, Vec<Value>>,
    tool_results: HashMap<String, Value>,
}

impl EventMapper {
    pub(super) fn new(profile: AcpProfile, options: MapperOptions, turn: String) -> Self {
        Self {
            profile,
            options,
            turn,
            round: 0,
            round_pending: false,
            compactions: 0,
            current_tool_id: None,
            tool_inputs: HashMap::new(),
            tool_diffs: HashMap::new(),
            tool_terminals: HashMap::new(),
            tool_results: HashMap::new(),
        }
    }

    /// The client created `terminal_id` for tool call `id`: embed it. Later
    /// updates of the call keep it, because `content` replaces what the
    /// client shows and the terminal stays displayable after release.
    pub(super) fn terminal_attached(&mut self, id: String, terminal_id: String) -> Value {
        self.tool_terminals
            .entry(id.clone())
            .or_default()
            .push(json!({ "type": "terminal", "terminalId": terminal_id }));
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "content": self.content(&id),
        })
    }

    /// What a call shows: its diffs, then its terminals, then its result.
    fn content(&self, id: &str) -> Vec<Value> {
        let mut content = self.tool_diffs.get(id).cloned().unwrap_or_default();
        content.extend(self.tool_terminals.get(id).into_iter().flatten().cloned());
        content.extend(self.tool_results.get(id).cloned());
        content
    }

    pub(super) fn map_event(&mut self, event: ServerEvent) -> Vec<Value> {
        match event {
            ServerEvent::TextDelta { text } | ServerEvent::TextReplace { text } => {
                vec![self.chunk("agent_message_chunk", "msg", text)]
            }
            // ACP chunks cannot retract prior output. Surface the corrected
            // source links rather than silently discarding provider metadata.
            ServerEvent::TextSuffixReplace { new_text, .. } => {
                vec![self.chunk("agent_message_chunk", "msg", format!("\n{new_text}"))]
            }
            ServerEvent::ReasoningDelta { text } => vec![self.chunk("agent_thought_chunk", "thought", text)],
            ServerEvent::ToolStart { id, name } => {
                self.round_pending = true;
                self.current_tool_id = Some(id.clone());
                self.tool_inputs.entry(id.clone()).or_default();
                vec![json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": id,
                    "name": name,
                    "title": tool_title(&name),
                    "kind": tool_kind(&name),
                    "status": "pending",
                })]
            }
            ServerEvent::ToolInput { id, delta } => {
                let Some(tool_id) = id.or_else(|| self.current_tool_id.clone()) else {
                    return Vec::new();
                };
                let buffer = self.tool_inputs.entry(tool_id.clone()).or_default();
                buffer.push_str(&delta);
                let mut update = json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": tool_id,
                });
                self.attach_input(&mut update, &tool_id);
                vec![update]
            }
            ServerEvent::ToolExec { id, name } => {
                self.round_pending = true;
                self.current_tool_id = Some(id.clone());
                let mut update = json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": id,
                    "title": tool_title(&name),
                    "kind": tool_kind(&name),
                    "status": "in_progress",
                });
                self.attach_input(&mut update, &id);
                vec![update]
            }
            ServerEvent::ToolDone {
                id,
                name,
                output,
                error,
                verdict,
            } => {
                self.round_pending = true;
                let text = json!({
                    "type": "content",
                    "content": { "type": "text", "text": output },
                });
                self.tool_results.insert(id.clone(), text);
                let content = self.content(&id);
                let mut update = json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": id,
                    "title": tool_title(&name),
                    "kind": tool_kind(&name),
                    "status": if error.is_some() { "failed" } else { "completed" },
                    "content": content,
                    "rawOutput": {
                        "output": output,
                        "error": error,
                    }
                });
                // `status` keeps its two values; the verdict rides beside it.
                if let Some(meta) = verdict_meta(verdict) {
                    update["_meta"] = meta;
                }
                vec![update]
            }
            ServerEvent::ToolDiff { id, diffs } => {
                let items: Vec<Value> = diffs.iter().map(|diff| self.diff_content(diff)).collect();
                self.tool_diffs.insert(id.clone(), items);
                // Content replaces what the client shows, so a diff that
                // arrives after the result keeps the result text after it.
                let content = self.content(&id);
                vec![json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": id,
                    "content": content,
                })]
            }
            ServerEvent::PlanUpdated { entries } => vec![plan_update(&entries)],
            ServerEvent::GeneratedImage {
                id,
                path,
                output_format,
                revised_prompt,
                ..
            } => vec![json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": id,
                "status": "completed",
                "content": [{
                    "type": "content",
                    "content": {
                        "type": "text",
                        "text": format!("Generated image: {path} ({output_format}){}", revised_prompt.map(|prompt| format!("\nRevised prompt: {prompt}")).unwrap_or_default()),
                    }
                }]
            })],
            ServerEvent::Compaction { trigger, .. } => {
                if self.options.compaction {
                    self.compactions += 1;
                    vec![json!({
                        "sessionUpdate": "compaction_update",
                        "compactionId": format!("{}-cmp-{}", self.turn, self.compactions),
                        "status": "completed",
                    })]
                } else if self.profile.is_extended() {
                    vec![self.chunk(
                        "agent_message_chunk",
                        "msg",
                        format!("\n[HoronAgent compacted context: {trigger}]\n"),
                    )]
                } else {
                    Vec::new()
                }
            }
            ServerEvent::ProviderGuardrail { message, .. } => {
                if self.options.notices {
                    vec![notice("warning", &message, "The provider stopped the turn")]
                } else {
                    vec![self.chunk("agent_message_chunk", "msg", message)]
                }
            }
            ServerEvent::TurnStopped { reason, message, .. } if self.options.notices => {
                vec![notice(turn_stopped_severity(reason), &message, "The turn stopped")]
            }
            ServerEvent::Notification { message, .. } if self.options.notices => {
                vec![notice("info", &message, "Notification")]
            }
            ServerEvent::SessionRenamed { display_title, .. } => vec![json!({
                "sessionUpdate": "session_info_update",
                "title": display_title,
            })],
            ServerEvent::McpStatus { servers } if self.profile.is_extended() => vec![self.chunk(
                "agent_message_chunk",
                "msg",
                format!("\n[HoronAgent MCP status: {}]\n", servers.join(", ")),
            )],
            _ => Vec::new(),
        }
    }

    /// A message or thought chunk. Chunks of one round share a message id; the
    /// first chunk after a tool round opens a new one.
    fn chunk(&mut self, update: &str, kind: &str, text: String) -> Value {
        if self.round_pending {
            self.round += 1;
            self.round_pending = false;
        }
        json!({
            "sessionUpdate": update,
            "messageId": format!("{}-{kind}-{}", self.turn, self.round),
            "content": { "type": "text", "text": text },
        })
    }

    fn attach_input(&self, update: &mut Value, tool_id: &str) {
        let Some(raw_input) = self.tool_inputs.get(tool_id).and_then(|input| parse_json_object(input)) else {
            return;
        };
        let Some(object) = update.as_object_mut() else {
            return;
        };
        if let Some(path) = self.input_location(&raw_input) {
            object.insert("locations".to_string(), json!([{ "path": path }]));
        }
        object.insert("rawInput".to_string(), raw_input);
    }

    fn input_location(&self, raw_input: &Value) -> Option<String> {
        tool_location(raw_input, self.options.cwd.as_deref())
    }

    fn diff_content(&self, diff: &FileDiffWire) -> Value {
        json!({
            "type": "diff",
            "path": absolute_path(&diff.path, self.options.cwd.as_deref()).unwrap_or_else(|| diff.path.clone()),
            "oldText": diff.old_text,
            "newText": diff.new_text,
        })
    }
}

/// The absolute file a tool call works on, from its input's path keys.
pub(super) fn tool_location(raw_input: &Value, cwd: Option<&Path>) -> Option<String> {
    let path = LOCATION_KEYS
        .iter()
        .find_map(|key| raw_input.get(*key).and_then(Value::as_str))
        .filter(|path| !path.is_empty())?;
    absolute_path(path, cwd)
}

/// `path` itself when absolute, else joined to the session cwd. `None` when no
/// absolute path can be formed.
fn absolute_path(path: &str, cwd: Option<&Path>) -> Option<String> {
    let path = Path::new(path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd?.join(path)
    };
    resolved.is_absolute().then(|| resolved.display().to_string())
}

pub(super) fn plan_update(entries: &[PlanEntryWire]) -> Value {
    let entries: Vec<Value> = entries
        .iter()
        .map(|entry| {
            json!({
                "content": entry.content,
                "priority": entry.priority,
                "status": entry.status,
            })
        })
        .collect();
    json!({ "sessionUpdate": "plan", "entries": entries })
}

pub(super) fn notice(severity: &str, message: &str, fallback: &str) -> Value {
    let title = message.trim();
    json!({
        "sessionUpdate": "notice",
        "severity": severity,
        "title": if title.is_empty() { fallback } else { title },
    })
}

pub(super) fn turn_stopped_severity(reason: TurnStopReason) -> &'static str {
    match reason {
        TurnStopReason::Interrupted => "info",
        TurnStopReason::ProviderGuardrail | TurnStopReason::LimitReached => "warning",
        TurnStopReason::Failure | TurnStopReason::Crash | TurnStopReason::Unknown => "error",
    }
}
