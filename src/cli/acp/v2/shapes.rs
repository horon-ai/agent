//! Pure ACP v2 message builders.
//!
//! Each function returns one JSON value and holds no connection state; the name
//! follows the `$defs` entry `tests.rs` validates the value against.

use super::super::auth::AUTH_METHOD_CREDENTIALS;
use super::super::rpc::{JSONRPC_INVALID_PARAMS, RpcError};
use super::super::util::AcpProfile;
use super::negotiate::ACP_V2_PROTOCOL_VERSION;
use crate::acp_render::{PatchFile, base64_encode, git_patch};
use crate::protocol::{FileDiffWire, PlanEntryWire, SessionMcpServer};
use serde_json::{Value, json};
use std::path::Path;

/// The `planId` of the single to-do plan a session reports.
const PLAN_ID_TODO: &str = "todo";

/// IS-2: the v2 `initialize` result for one client.
///
/// `mcp_http` mirrors the `mcpCapabilities.http` the v1 adapter advertises.
/// No client capability adds a second auth method: this build has no
/// terminal login for a client to launch.
pub(crate) fn initialize_response(profile: AcpProfile, mcp_http: bool) -> Value {
    let mut mcp = json!({ "stdio": {} });
    if mcp_http {
        mcp["http"] = json!({});
    }
    let mut response = json!({
        "protocolVersion": ACP_V2_PROTOCOL_VERSION,
        "info": {
            "name": "horon-agent",
            "title": "HoronAgent",
            "version": horon_agent_build_meta::pkg_version(),
        },
        "capabilities": {
            "session": {
                "prompt": { "image": {}, "embeddedContext": {} },
                "mcp": mcp,
                "delete": {},
                "additionalDirectories": {},
            },
        },
        "authMethods": auth_methods(),
    });
    if profile.is_extended() {
        response["capabilities"]["_meta"] = json!({
            "horon-agent": { "profile": profile.as_str(), "extensions": ["raw_server_event"] },
        });
    }
    response
}

/// IS-15: the one `authMethods` entry; the credentials the daemon already
/// resolves.
pub(crate) fn auth_methods() -> Vec<Value> {
    vec![json!({
        "methodId": AUTH_METHOD_CREDENTIALS,
        "type": "agent",
        "name": "Use HoronAgent login",
        "description": "Uses the OpenAI-compatible provider profile configured in [providers.<name>]. Run 'horon-agent provider add' to create one.",
    })]
}

/// IS-12: v1 config options renamed to `configId`, every select group renamed
/// to `groupId`, order kept, select options only.
///
/// alpha.13 requires every select to keep at least one value
/// (`SessionConfigSelectOptions` is `minItems: 1`), so a select that would
/// publish none is dropped rather than sent as a frame the schema refuses.
pub(crate) fn config_options(v1_options: &[Value]) -> Vec<Value> {
    v1_options
        .iter()
        .filter_map(|option| {
            let option = option.as_object()?;
            if option.get("type").and_then(Value::as_str) != Some("select") {
                return None;
            }
            let mut renamed = serde_json::Map::new();
            for (key, value) in option {
                if key == "id" {
                    renamed.insert("configId".to_string(), value.clone());
                } else if key == "options" {
                    renamed.insert("options".to_string(), select_groups(value));
                } else {
                    renamed.insert(key.clone(), value.clone());
                }
            }
            if is_empty_values(renamed.get("options")) {
                return None;
            }
            Some(Value::Object(renamed))
        })
        .collect()
}

/// v2 names a select group `groupId`; a plain select option has no group field
/// and passes through unchanged.
fn select_groups(options: &Value) -> Value {
    let Some(options) = options.as_array() else {
        return options.clone();
    };
    Value::Array(
        options
            .iter()
            .filter_map(|option| {
                let Some(option) = option.as_object() else {
                    return Some(option.clone());
                };
                if !option.contains_key("group") {
                    return Some(Value::Object(option.clone()));
                }
                // A group whose own value list is empty is refused by the
                // alpha.13 `SessionConfigSelectGroup` (`minItems: 1`): drop it.
                if is_empty_values(option.get("options")) {
                    return None;
                }
                let mut renamed = serde_json::Map::new();
                for (key, value) in option {
                    let key = if key == "group" { "groupId" } else { key.as_str() };
                    renamed.insert(key.to_string(), value.clone());
                }
                Some(Value::Object(renamed))
            })
            .collect(),
    )
}

/// A select value list with no entries, or none at all.
fn is_empty_values(options: Option<&Value>) -> bool {
    options.and_then(Value::as_array).is_none_or(Vec::is_empty)
}

/// The value of one `session/set_config_option` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigValue {
    Id(String),
    Boolean(bool),
}

/// IS-12: `configId` plus a `type`-discriminated value.
pub(crate) fn parse_set_config_option(params: &Value) -> Result<(String, ConfigValue), RpcError> {
    let config_id = params
        .get("configId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_params("session/set_config_option requires a non-empty string configId"))?;
    let value = match params.get("type").and_then(Value::as_str) {
        Some("id") => params
            .get("value")
            .and_then(Value::as_str)
            .map(|value| ConfigValue::Id(value.to_string()))
            .ok_or_else(|| invalid_params("session/set_config_option type 'id' requires a string value"))?,
        Some("boolean") => params
            .get("value")
            .and_then(Value::as_bool)
            .map(ConfigValue::Boolean)
            .ok_or_else(|| invalid_params("session/set_config_option type 'boolean' requires a boolean value"))?,
        Some(other) => {
            return Err(invalid_params(format!(
                "session/set_config_option does not support value type {other:?}"
            )));
        }
        None => {
            return Err(invalid_params(
                "session/set_config_option requires type 'id' or 'boolean'",
            ));
        }
    };
    Ok((config_id.to_string(), value))
}

/// IS-12: v1 slash commands with the required `type` discriminators.
pub(crate) fn available_commands(v1_commands: &[Value]) -> Vec<Value> {
    v1_commands
        .iter()
        .filter_map(Value::as_object)
        .map(|command| {
            let name = command.get("name").and_then(Value::as_str).unwrap_or_default();
            let description = command.get("description").and_then(Value::as_str).unwrap_or_default();
            let mut available = json!({
                "type": "command",
                "name": name,
                "description": description,
            });
            if let Some(hint) = command
                .get("input")
                .and_then(|input| input.get("hint"))
                .and_then(Value::as_str)
            {
                available["input"] = json!({ "type": "text", "hint": hint });
            }
            available
        })
        .collect()
}

/// IS-13: v2 `mcpServers`, where `type` is required and `sse` is gone.
pub(crate) fn mcp_servers(params: &Value, http_advertised: bool) -> Result<Vec<SessionMcpServer>, RpcError> {
    match params.get("mcpServers") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(servers)) => servers
            .iter()
            .map(|server| mcp_server(server, http_advertised))
            .collect(),
        Some(_) => Err(invalid_params("mcpServers must be an array")),
    }
}

fn mcp_server(server: &Value, http_advertised: bool) -> Result<SessionMcpServer, RpcError> {
    let server = server
        .as_object()
        .ok_or_else(|| invalid_params("mcpServers entries must be objects"))?;
    let name = required_string(server, "name", "mcpServers entry")?;
    let transport = server
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params(format!("MCP server {name:?}: type must be \"stdio\" or \"http\"")))?;
    match transport {
        "stdio" => Ok(SessionMcpServer {
            command: Some(required_string(server, "command", &format!("MCP server {name:?}"))?),
            args: string_array(server, "args", &name)?,
            env: name_value_pairs(server, "env", &name)?,
            name,
            transport: "stdio".to_string(),
            url: None,
            headers: Vec::new(),
        }),
        "http" => {
            if !http_advertised {
                return Err(invalid_params(format!(
                    "MCP server {name:?}: the http transport is not supported by this agent"
                )));
            }
            Ok(SessionMcpServer {
                url: Some(required_string(server, "url", &format!("MCP server {name:?}"))?),
                headers: name_value_pairs(server, "headers", &name)?,
                name,
                transport: "http".to_string(),
                command: None,
                args: Vec::new(),
                env: Vec::new(),
            })
        }
        other => Err(invalid_params(format!(
            "MCP server {name:?}: unsupported transport type {other:?}"
        ))),
    }
}

fn required_string(object: &serde_json::Map<String, Value>, field: &str, owner: &str) -> Result<String, RpcError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid_params(format!("{owner}: {field} must be a string")))
}

fn string_array(object: &serde_json::Map<String, Value>, field: &str, owner: &str) -> Result<Vec<String>, RpcError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| invalid_params(format!("{owner}: {field} entries must be strings")))
            })
            .collect(),
        Some(_) => Err(invalid_params(format!("{owner}: {field} must be an array"))),
    }
}

fn name_value_pairs(
    object: &serde_json::Map<String, Value>,
    field: &str,
    owner: &str,
) -> Result<Vec<(String, String)>, RpcError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|pair| {
                let name = pair.get("name").and_then(Value::as_str);
                let value = pair.get("value").and_then(Value::as_str);
                match (name, value) {
                    (Some(name), Some(value)) => Ok((name.to_string(), value.to_string())),
                    _ => Err(invalid_params(format!(
                        "{owner}: {field} entries must be {{name, value}} strings"
                    ))),
                }
            })
            .collect(),
        Some(_) => Err(invalid_params(format!("{owner}: {field} must be an array"))),
    }
}

/// IS-10: `session/request_permission`.
pub(crate) fn permission_request(
    session_id: &str,
    title: &str,
    description: Option<&str>,
    subject: Option<Value>,
    options: &[Value],
) -> Value {
    let mut request = json!({
        "sessionId": session_id,
        "title": title,
        "options": options,
    });
    if let Some(description) = description {
        request["description"] = json!(description);
    }
    if let Some(subject) = subject {
        request["subject"] = subject;
    }
    request
}

/// IS-10: the subject of a bash permission, which is never a tool call patch.
pub(crate) fn command_subject(command: &str, cwd: &Path, tool_call_id: Option<&str>) -> Value {
    let mut subject = json!({
        "type": "command",
        "command": command,
        "cwd": cwd_string(cwd),
    });
    if let Some(tool_call_id) = tool_call_id {
        subject["toolCallId"] = json!(tool_call_id);
    }
    subject
}

/// IS-10: the subject of every non-bash permission request.
pub(crate) fn tool_call_subject(tool_call: Value) -> Value {
    json!({ "type": "tool_call", "toolCall": tool_call })
}

/// IS-11: a plan update whose `planId` is always the to-do list.
pub(crate) fn plan_update(entries: &[PlanEntryWire]) -> Value {
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
    json!({
        "sessionUpdate": "plan_update",
        "plan": { "type": "items", "planId": PLAN_ID_TODO, "entries": entries },
    })
}

/// IS-8: a file-edit result as v2 changes plus a `git_patch` of absolute paths.
pub(crate) fn diff_content(diffs: &[FileDiffWire], cwd: &Path) -> Value {
    struct Entry {
        change: Value,
        path: String,
        old_path: Option<String>,
        old_text: Option<String>,
        new_text: Option<String>,
    }
    let entries: Vec<Entry> = diffs
        .iter()
        .map(|diff| {
            let path = absolute(&diff.path, cwd);
            let (change, old_path, old_text, new_text) = if diff.deleted {
                let change = json!({ "operation": "delete", "path": path, "fileType": "text" });
                (change, None, diff.old_text.clone(), None)
            } else if let Some(previous) = &diff.old_path {
                let old_path = absolute(previous, cwd);
                let change = json!({
                    "operation": "move",
                    "oldPath": old_path,
                    "path": path,
                    "fileType": "text",
                });
                (
                    change,
                    Some(old_path),
                    diff.old_text.clone(),
                    Some(diff.new_text.clone()),
                )
            } else if diff.created || diff.old_text.is_none() {
                let change = json!({ "operation": "add", "path": path, "fileType": "text" });
                (change, None, None, Some(diff.new_text.clone()))
            } else {
                let change = json!({ "operation": "modify", "path": path, "fileType": "text" });
                (change, None, diff.old_text.clone(), Some(diff.new_text.clone()))
            };
            Entry {
                change,
                path,
                old_path,
                old_text,
                new_text,
            }
        })
        .collect();
    let files: Vec<PatchFile<'_>> = entries
        .iter()
        .map(|entry| PatchFile {
            path: &entry.path,
            old_path: entry.old_path.as_deref(),
            old_text: entry.old_text.as_deref(),
            new_text: entry.new_text.as_deref(),
        })
        .collect();
    let patch = git_patch(&files);
    let changes: Vec<&Value> = entries.iter().map(|entry| &entry.change).collect();
    json!({
        "type": "diff",
        "changes": changes,
        "patch": { "format": "git_patch", "text": patch },
    })
}

/// IS-9: the display-only reference a bash tool call carries.
pub(crate) fn terminal_content(terminal_id: &str) -> Value {
    json!({ "type": "terminal", "terminalId": terminal_id })
}

/// IS-9: the first terminal state, sent once the command is known.
pub(crate) fn terminal_started(terminal_id: &str, command: &str, cwd: &Path) -> Value {
    json!({
        "sessionUpdate": "terminal_update",
        "terminalId": terminal_id,
        "command": command,
        "cwd": cwd_string(cwd),
    })
}

/// IS-9: a complete output snapshot with no exit status, used while a bash call
/// runs in the background.
pub(crate) fn terminal_output(terminal_id: &str, output: &str) -> Value {
    json!({
        "sessionUpdate": "terminal_update",
        "terminalId": terminal_id,
        "output": { "data": base64_encode(output.as_bytes()) },
    })
}

/// IS-9: the final snapshot, which also marks the terminal exited.
pub(crate) fn terminal_finished(
    terminal_id: &str,
    output: &str,
    exit_code: Option<i64>,
    signal: Option<&str>,
) -> Value {
    json!({
        "sessionUpdate": "terminal_update",
        "terminalId": terminal_id,
        "output": { "data": base64_encode(output.as_bytes()) },
        "exitStatus": { "exitCode": exit_code, "signal": signal },
    })
}

/// IS-5: foreground work started.
pub(crate) fn state_running() -> Value {
    json!({ "sessionUpdate": "state_update", "state": "running" })
}

/// IS-5: foreground work is blocked on a permission or elicitation answer.
pub(crate) fn state_requires_action() -> Value {
    json!({ "sessionUpdate": "state_update", "state": "requires_action" })
}

/// IS-5: foreground work ended; `error` is only valid with the `error` reason.
pub(crate) fn state_idle(stop_reason: Option<&str>, error: Option<(i64, String)>) -> Value {
    debug_assert!(
        error.is_none() || stop_reason == Some("error"),
        "an idle error requires the error stop reason"
    );
    let mut update = json!({ "sessionUpdate": "state_update", "state": "idle" });
    if let Some(stop_reason) = stop_reason {
        update["stopReason"] = json!(stop_reason);
    }
    if let Some((code, message)) = error {
        update["error"] = json!({ "code": code, "message": message });
    }
    update
}

/// IS-6: the per-message identifiers of one turn.
///
/// `n` starts at 1 and grows by one after every tool round, so the ids of a
/// round repeat for every chunk of that round and replay recomputes them.
#[derive(Clone, Debug)]
pub(crate) struct MessageIds {
    start_id: String,
    round: u32,
}

pub(crate) fn message_ids(start_id: &str) -> MessageIds {
    MessageIds {
        start_id: start_id.to_string(),
        round: 1,
    }
}

impl MessageIds {
    pub(crate) fn agent(&self) -> String {
        format!("{}:a{}", self.start_id, self.round)
    }

    pub(crate) fn thought(&self) -> String {
        format!("{}:t{}", self.start_id, self.round)
    }

    pub(crate) fn advance_after_tool_round(&mut self) {
        self.round += 1;
    }
}

/// IS-6: which chunk update a builder writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChunkKind {
    AgentMessage,
    AgentThought,
}

/// IS-6: which whole-message update a builder writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MessageKind {
    UserMessage,
    AgentMessage,
    AgentThought,
}

impl ChunkKind {
    fn session_update(self) -> &'static str {
        match self {
            Self::AgentMessage => "agent_message_chunk",
            Self::AgentThought => "agent_thought_chunk",
        }
    }
}

impl MessageKind {
    fn session_update(self) -> &'static str {
        match self {
            Self::UserMessage => "user_message",
            Self::AgentMessage => "agent_message",
            Self::AgentThought => "agent_thought",
        }
    }
}

/// IS-6: one streamed text block of a message.
pub(crate) fn message_chunk(kind: ChunkKind, message_id: &str, text: &str) -> Value {
    json!({
        "sessionUpdate": kind.session_update(),
        "messageId": message_id,
        "content": { "type": "text", "text": text },
    })
}

/// IS-6: a whole-message upsert used for replay and correction.
pub(crate) fn whole_message(kind: MessageKind, message_id: &str, content: &[Value]) -> Value {
    json!({
        "sessionUpdate": kind.session_update(),
        "messageId": message_id,
        "content": content,
    })
}

/// IS-6: the user message of a prompt, keyed by the id the prompt response used.
pub(crate) fn user_message(message_id: &str, content: &[Value]) -> Value {
    whole_message(MessageKind::UserMessage, message_id, content)
}

/// IS-2: the client control a running tool call advertises while it can be
/// stopped with `tool_call/cancel`.
pub(crate) fn cancel_capabilities() -> Value {
    json!({ "cancel": {} })
}

/// The fields one `tool_call_update` patches; `None` leaves the field unchanged.
#[derive(Clone, Debug, Default)]
pub(crate) struct ToolCallPatch {
    pub(crate) name: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) content: Option<Vec<Value>>,
    pub(crate) locations: Option<Vec<Value>>,
    pub(crate) raw_input: Option<Value>,
    pub(crate) raw_output: Option<Value>,
    /// The client controls the call currently supports. `Some(Value::Null)`
    /// clears them, which every final status does.
    pub(crate) capabilities: Option<Value>,
}

impl ToolCallPatch {
    /// IS-7: the first update of a tool call, which creates it.
    pub(crate) fn created(title: &str, name: &str, kind: &str) -> Self {
        Self {
            name: Some(name.to_string()),
            title: Some(title.to_string()),
            kind: Some(kind.to_string()),
            status: Some("pending".to_string()),
            ..Self::default()
        }
    }

    /// A final status of a tool call. The call stops advertising
    /// `capabilities.cancel` the moment it is no longer cancellable, so every
    /// final update carries `capabilities: null`.
    pub(crate) fn final_status(status: &str) -> Self {
        Self {
            status: Some(status.to_string()),
            capabilities: Some(Value::Null),
            ..Self::default()
        }
    }
}

/// IS-7: the single upsert notification for a tool call.
pub(crate) fn tool_call_update(tool_call_id: &str, patch: &ToolCallPatch) -> Value {
    let mut update = json!({ "sessionUpdate": "tool_call_update", "toolCallId": tool_call_id });
    if let Some(name) = &patch.name {
        update["name"] = json!(name);
    }
    if let Some(title) = &patch.title {
        update["title"] = json!(title);
    }
    if let Some(kind) = &patch.kind {
        update["kind"] = json!(kind);
    }
    if let Some(status) = &patch.status {
        update["status"] = json!(status);
    }
    if let Some(content) = &patch.content {
        update["content"] = json!(content);
    }
    if let Some(locations) = &patch.locations {
        update["locations"] = json!(locations);
    }
    if let Some(raw_input) = &patch.raw_input {
        update["rawInput"] = raw_input.clone();
    }
    if let Some(raw_output) = &patch.raw_output {
        update["rawOutput"] = raw_output.clone();
    }
    if let Some(capabilities) = &patch.capabilities {
        update["capabilities"] = capabilities.clone();
    }
    update
}

/// IS-7: one content item appended to a tool call.
pub(crate) fn tool_call_content_chunk(tool_call_id: &str, content: Value) -> Value {
    json!({
        "sessionUpdate": "tool_call_content_chunk",
        "toolCallId": tool_call_id,
        "content": content,
    })
}

/// IS-13: one `session/list` row.
pub(crate) fn session_info(
    session_id: &str,
    cwd: &Path,
    title: Option<&str>,
    updated_at: Option<&str>,
    additional_directories: &[String],
) -> Value {
    let mut info = json!({ "sessionId": session_id, "cwd": cwd_string(cwd) });
    if let Some(title) = title {
        info["title"] = json!(title);
    }
    if let Some(updated_at) = updated_at {
        info["updatedAt"] = json!(updated_at);
    }
    if !additional_directories.is_empty() {
        info["additionalDirectories"] = json!(additional_directories);
    }
    info
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new(JSONRPC_INVALID_PARAMS, message)
}

fn cwd_string(cwd: &Path) -> String {
    debug_assert!(cwd.is_absolute(), "the v2 contract requires an absolute cwd: {cwd:?}");
    cwd.to_string_lossy().into_owned()
}

fn absolute(path: &str, cwd: &Path) -> String {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_string_lossy().into_owned()
    } else {
        cwd.join(path).to_string_lossy().into_owned()
    }
}
