//! IS-6 to IS-11: maps the daemon events of a v2 session to v2 `session/update` payloads.
//!
//! One mapper lives for one foreground run, own or peer. Message ids come from
//! [`shapes::message_ids`] of the run's start id, the same struct replay
//! recomputes them with, and advance once before the first text after any tool
//! activity. A tool call is created by its first `tool_call_update`, patched
//! once its input is complete, and receives its diffs, terminal reference and
//! result text as `tool_call_content_chunk`s. A bash call is an agent-owned
//! display terminal: the command once known, then the complete output with the
//! exit status. Nothing here writes a v1-only update.

use super::super::mapper::{notice, tool_location, turn_stopped_severity};
use super::super::util::{parse_json_object, tool_kind, tool_title, verdict_meta};
use super::replay::Position;
use super::shapes::{
    self, ChunkKind, MessageIds, MessageKind, ToolCallPatch, cancel_capabilities, diff_content, message_chunk,
    message_ids, terminal_content, terminal_finished, terminal_output, terminal_started, tool_call_content_chunk,
    tool_call_update, whole_message,
};
use crate::protocol::{FileDiffWire, ServerEvent};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;

/// The only tool shown as a terminal.
const BASH: &str = "bash";

/// How a shell call's process exited (`ServerEvent::ToolExit`).
#[derive(Clone, Debug)]
struct Exit {
    code: Option<i32>,
    signal: Option<String>,
    backgrounded: bool,
}

/// One tool call of the run.
#[derive(Debug, Default)]
struct Tool {
    /// Its creating `tool_call_update` was written.
    created: bool,
    name: Option<String>,
    /// The streamed input JSON.
    input: String,
    /// Diffs that arrived before the creating update.
    early_diffs: Vec<Vec<FileDiffWire>>,
    exit: Option<Exit>,
    /// The terminal reference was added to the call's content.
    terminal: bool,
    /// The `ToolDone` output of a bash call, kept for an exit that follows it.
    bash_output: Option<String>,
    done: bool,
    /// IS-4: the call went on as listed background work. Its `ToolDone` only
    /// adds content; its final status comes with the work's own end.
    held: bool,
}

pub(crate) struct V2Mapper {
    start: String,
    cwd: PathBuf,
    ids: MessageIds,
    /// A tool round happened since the last text: the next text opens a new
    /// message.
    round_pending: bool,
    /// The text of the current agent message, which a correction rewrites.
    agent_text: String,
    compactions: u64,
    current_tool: Option<String>,
    tools: HashMap<String, Tool>,
    /// Tool ids in first-seen order, so end-of-run updates are ordered.
    order: Vec<String>,
}

impl V2Mapper {
    /// `start` is the run's `start_message_id`, or its `run_id` when the run
    /// stored no message. The ids begin at round 1; a run attached part-way
    /// starts from [`Self::resumed`] instead. Relative tool paths resolve
    /// against `cwd`; a session without one (v2 requires it) falls back to the
    /// process directory.
    pub(crate) fn new(start: &str, cwd: Option<PathBuf>) -> Self {
        let cwd = cwd
            .filter(|cwd| cwd.is_absolute())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        Self {
            start: start.to_string(),
            cwd,
            ids: message_ids(start),
            round_pending: false,
            agent_text: String::new(),
            compactions: 0,
            current_tool: None,
            tools: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// The mapper of a run attached part-way: its ids continue from where the
    /// stored rows of the run leave them ([`super::replay::resume_position`]),
    /// so the ids it streams next follow the ones replay reported.
    pub(crate) fn resumed(position: Position, cwd: Option<PathBuf>) -> Self {
        let mut mapper = Self::new(&position.start, cwd);
        mapper.ids = position.ids;
        mapper.round_pending = position.round_pending;
        mapper
    }

    pub(crate) fn map_event(&mut self, event: ServerEvent) -> Vec<Value> {
        match event {
            ServerEvent::TextDelta { text } => {
                self.open_message();
                self.agent_text.push_str(&text);
                vec![message_chunk(ChunkKind::AgentMessage, &self.ids.agent(), &text)]
            }
            // The round's full text so far, so it replaces the message.
            ServerEvent::TextReplace { text } => {
                self.open_message();
                self.agent_text = text;
                vec![self.agent_message()]
            }
            // The v1 rule: the corrected suffix replaces the text it ends, and
            // a correction that matches nothing is appended on its own line.
            ServerEvent::TextSuffixReplace { old_text, new_text } => {
                self.open_message();
                if self.agent_text.ends_with(&old_text) {
                    let keep = self.agent_text.len() - old_text.len();
                    self.agent_text.truncate(keep);
                    self.agent_text.push_str(&new_text);
                } else {
                    self.agent_text.push('\n');
                    self.agent_text.push_str(&new_text);
                }
                vec![self.agent_message()]
            }
            ServerEvent::ReasoningDelta { text } => {
                self.open_message();
                vec![message_chunk(ChunkKind::AgentThought, &self.ids.thought(), &text)]
            }
            ServerEvent::ToolStart { id, name } => {
                self.round_pending = true;
                self.current_tool = Some(id.clone());
                self.ensure_created(&id, &name)
            }
            ServerEvent::ToolInput { id, delta } => {
                if let Some(id) = id.or_else(|| self.current_tool.clone()) {
                    self.tool(&id).input.push_str(&delta);
                }
                Vec::new()
            }
            ServerEvent::ToolExec { id, name } => {
                self.round_pending = true;
                self.current_tool = Some(id.clone());
                let mut updates = self.ensure_created(&id, &name);
                updates.extend(self.input_complete(&id));
                updates
            }
            ServerEvent::ToolDiff { id, diffs } => {
                if self.tool(&id).created {
                    vec![self.diff_chunk(&id, &diffs)]
                } else {
                    self.tool(&id).early_diffs.push(diffs);
                    Vec::new()
                }
            }
            ServerEvent::ToolExit {
                id,
                exit_code,
                signal,
                backgrounded,
            } => {
                let tool = self.tool(&id);
                tool.exit = Some(Exit {
                    code: exit_code,
                    signal,
                    backgrounded,
                });
                // The output came with `ToolDone`, which already reported it
                // without an exit: report it again, exited.
                match tool.bash_output.clone() {
                    Some(output) => vec![self.terminal_snapshot(&id, &output)],
                    None => Vec::new(),
                }
            }
            // IS-2: the daemon stopped this one call. Its final update is
            // `cancelled`, and the `ToolDone` that follows with the result the
            // model sees produces no second one.
            ServerEvent::ToolCancelled { id } => {
                self.round_pending = true;
                let name = self.tool(&id).name.clone().unwrap_or_else(|| "tool".to_string());
                let mut updates = self.ensure_created(&id, &name);
                if self.tool(&id).done {
                    return updates;
                }
                self.tool(&id).done = true;
                updates.push(tool_call_update(&id, &ToolCallPatch::final_status("cancelled")));
                updates
            }
            ServerEvent::ToolDone {
                id,
                name,
                output,
                error,
                verdict,
            } => {
                self.round_pending = true;
                self.tool_done(&id, &name, output, error.as_deref(), verdict)
            }
            ServerEvent::GeneratedImage {
                id,
                path,
                output_format,
                revised_prompt,
                ..
            } => {
                let text = format!(
                    "Generated image: {path} ({output_format}){}",
                    revised_prompt
                        .map(|prompt| format!("\nRevised prompt: {prompt}"))
                        .unwrap_or_default()
                );
                let name = self
                    .tool(&id)
                    .name
                    .clone()
                    .unwrap_or_else(|| "image_generation".to_string());
                let mut updates = self.ensure_created(&id, &name);
                updates.push(text_chunk(&id, text));
                if !self.tool(&id).done {
                    self.tool(&id).done = true;
                    updates.push(tool_call_update(&id, &ToolCallPatch::final_status("completed")));
                }
                updates
            }
            ServerEvent::PlanUpdated { entries } => vec![shapes::plan_update(&entries)],
            ServerEvent::Compaction { .. } => {
                self.compactions += 1;
                vec![json!({
                    "sessionUpdate": "compaction_update",
                    "compactionId": format!("{}:c{}", self.start, self.compactions),
                    "status": "completed",
                })]
            }
            ServerEvent::ProviderGuardrail { message, .. } => {
                vec![notice("warning", &message, "The provider stopped the turn")]
            }
            ServerEvent::TurnStopped { reason, message, .. } => {
                vec![notice(turn_stopped_severity(reason), &message, "The turn stopped")]
            }
            ServerEvent::Notification { message, .. } => vec![notice("info", &message, "Notification")],
            ServerEvent::SessionRenamed { display_title, .. } => vec![json!({
                "sessionUpdate": "session_info_update",
                "title": display_title,
            })],
            // The extended profile already gets it as `_horon_agent/server_event`;
            // v2 has no update for MCP server status.
            ServerEvent::McpStatus { .. } => Vec::new(),
            // IS-6: a soft interrupt stored mid-turn is a user message of its
            // own, keyed by the row id a `session/resume` replay uses too.
            ServerEvent::SoftInterruptStored { message_id, content } => vec![shapes::user_message(
                &message_id,
                &[json!({ "type": "text", "text": content })],
            )],
            _ => Vec::new(),
        }
    }

    /// The run's start id, which keys its message ids.
    pub(crate) fn start(&self) -> &str {
        &self.start
    }

    /// IS-4: the call keeps running as background work after its `ToolDone`.
    pub(crate) fn hold_open(&mut self, id: &str) {
        self.tool(id).held = true;
    }

    /// The run ended with `stop_reason`: a call known only from its diffs or
    /// exit is created now, and every call that never finished is reported
    /// cancelled when the run was cancelled, or failed when it errored. A
    /// daemon `EndTurn` after a call's finish leaves the call as reported.
    pub(crate) fn run_ended(&mut self, stop_reason: Option<&str>) -> Vec<Value> {
        let ended = match stop_reason {
            Some("cancelled") => Some("cancelled"),
            Some("error") => Some("failed"),
            _ => None,
        };
        let mut updates = Vec::new();
        for id in self.order.clone() {
            let tool = self.tool(&id);
            if !tool.created && (!tool.early_diffs.is_empty() || tool.exit.is_some()) {
                tool.created = true;
                let patch = ToolCallPatch {
                    title: Some("Tool call".to_string()),
                    kind: Some("other".to_string()),
                    status: Some("pending".to_string()),
                    ..ToolCallPatch::default()
                };
                updates.push(tool_call_update(&id, &patch));
                updates.extend(self.flush_early(&id));
            }
            let tool = self.tool(&id);
            if let Some(end_status) = ended
                && tool.created
                && !tool.done
            {
                tool.done = true;
                updates.push(tool_call_update(&id, &ToolCallPatch::final_status(end_status)));
            }
        }
        updates
    }

    fn open_message(&mut self) {
        if self.round_pending {
            self.round_pending = false;
            self.ids.advance_after_tool_round();
            self.agent_text.clear();
        }
    }

    fn agent_message(&self) -> Value {
        whole_message(
            MessageKind::AgentMessage,
            &self.ids.agent(),
            &[json!({ "type": "text", "text": self.agent_text })],
        )
    }

    fn tool(&mut self, id: &str) -> &mut Tool {
        if !self.tools.contains_key(id) {
            self.order.push(id.to_string());
        }
        self.tools.entry(id.to_string()).or_default()
    }

    /// The creating update, minimal (IS-7), then whatever arrived before it.
    fn ensure_created(&mut self, id: &str, name: &str) -> Vec<Value> {
        let tool = self.tool(id);
        tool.name.get_or_insert_with(|| name.to_string());
        if tool.created {
            return Vec::new();
        }
        tool.created = true;
        let mut updates = vec![tool_call_update(
            id,
            &ToolCallPatch::created(&tool_title(name), name, tool_kind(name)),
        )];
        updates.extend(self.flush_early(id));
        updates
    }

    fn flush_early(&mut self, id: &str) -> Vec<Value> {
        let diffs = std::mem::take(&mut self.tool(id).early_diffs);
        diffs.iter().map(|diffs| self.diff_chunk(id, diffs)).collect()
    }

    fn diff_chunk(&self, id: &str, diffs: &[FileDiffWire]) -> Value {
        tool_call_content_chunk(id, diff_content(diffs, &self.cwd))
    }

    /// The input is complete: patch it in with the call's absolute location,
    /// advertise `capabilities.cancel` for the call while it runs, and show a
    /// bash call's terminal once its command is known.
    fn input_complete(&mut self, id: &str) -> Vec<Value> {
        let raw_input = parse_json_object(&self.tool(id).input);
        let locations = raw_input
            .as_ref()
            .and_then(|input| tool_location(input, Some(&self.cwd)))
            .map(|path| vec![json!({ "path": path })]);
        let patch = ToolCallPatch {
            status: Some("in_progress".to_string()),
            raw_input: raw_input.clone(),
            locations,
            capabilities: Some(cancel_capabilities()),
            ..ToolCallPatch::default()
        };
        let mut updates = vec![tool_call_update(id, &patch)];
        if self.is_bash(id) {
            let command = raw_input
                .as_ref()
                .and_then(|input| input.get("command"))
                .and_then(Value::as_str)
                .map(str::to_string);
            updates.extend(self.attach_terminal(id));
            if let Some(command) = command {
                updates.push(terminal_started(id, &command, &self.cwd));
            }
        }
        updates
    }

    fn is_bash(&mut self, id: &str) -> bool {
        self.tool(id).name.as_deref() == Some(BASH)
    }

    /// The display-only terminal reference, added once; its id is the call's.
    fn attach_terminal(&mut self, id: &str) -> Vec<Value> {
        let tool = self.tool(id);
        if tool.terminal {
            return Vec::new();
        }
        tool.terminal = true;
        vec![tool_call_content_chunk(id, terminal_content(id))]
    }

    /// The complete output, with the exit status once the process exited in
    /// the foreground.
    fn terminal_snapshot(&mut self, id: &str, output: &str) -> Value {
        match &self.tool(id).exit {
            Some(exit) if !exit.backgrounded => {
                terminal_finished(id, output, exit.code.map(i64::from), exit.signal.as_deref())
            }
            _ => terminal_output(id, output),
        }
    }

    fn tool_done(
        &mut self,
        id: &str,
        name: &str,
        output: String,
        error: Option<&str>,
        verdict: Option<crate::tool::ToolVerdict>,
    ) -> Vec<Value> {
        let mut updates = self.ensure_created(id, name);
        if self.tool(id).done {
            return updates;
        }
        if self.is_bash(id) {
            updates.extend(self.attach_terminal(id));
            updates.push(self.terminal_snapshot(id, &output));
            self.tool(id).bash_output = Some(output);
        } else {
            let text = if output.is_empty() {
                error.unwrap_or_default().to_string()
            } else {
                output
            };
            if !text.is_empty() {
                updates.push(text_chunk(id, text));
            }
        }
        self.tool(id).done = true;
        if self.tool(id).held {
            // Still running in the background: no final status yet, and no
            // `tool_call/cancel` either, which only reaches a call the turn
            // is waiting on.
            updates.push(tool_call_update(
                id,
                &ToolCallPatch {
                    capabilities: Some(Value::Null),
                    ..ToolCallPatch::default()
                },
            ));
            return updates;
        }
        // `status` keeps its two values; the verdict rides beside it in `_meta`.
        updates.push(tool_call_update(
            id,
            &ToolCallPatch {
                meta: verdict_meta(verdict),
                ..ToolCallPatch::final_status(if error.is_some() { "failed" } else { "completed" })
            },
        ));
        updates
    }
}

fn text_chunk(id: &str, text: String) -> Value {
    tool_call_content_chunk(
        id,
        json!({ "type": "content", "content": { "type": "text", "text": text } }),
    )
}

/// IS-12: the selector update after a model change, in v2 option shapes.
pub(crate) fn config_option_update(v1_options: &[Value]) -> Value {
    json!({
        "sessionUpdate": "config_option_update",
        "configOptions": shapes::config_options(v1_options),
    })
}
