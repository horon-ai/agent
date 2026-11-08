//! The `pre_request` hook: rewrite the outgoing messages for one provider request.
//!
//! The hook runs before every foreground turn request and receives
//! `{session_id, turn, model, messages}` on stdin. Empty stdout means
//! "no change"; `{"messages": [...]}` replaces the messages the provider sees
//! for that request only. The stored transcript is never touched.
//!
//! Every failure fails open: invalid JSON, a non-zero exit, a timeout or a
//! replacement that drops the request's system message keep the messages the
//! request was built from and log one warning. A request with no hook configured
//! spawns nothing and copies no message: [`configured`] is checked before any
//! payload is built.

use super::{HookEvent, build_hook_process, hook_commands, hooks_suppressed};
use horon_agent_message_types::Message;
use serde_json::{Value, json};
use std::time::Duration;

/// The request facts a `pre_request` hook receives.
#[derive(Debug, Clone, Copy)]
pub struct RequestContext<'a> {
    pub session_id: &'a str,
    pub cwd: Option<&'a str>,
    /// The turn counter of the session that issues the request.
    pub turn: u32,
    pub model: &'a str,
    /// The request's system prompt. The hook sees it as the first entry of
    /// `messages` and is not allowed to drop or rewrite it.
    pub system: &'a str,
}

/// Whether a `pre_request` hook is configured for this process.
///
/// The request path asks this before it builds a payload or clones a message,
/// so an unset hook costs one configuration lookup and nothing else.
pub fn configured() -> bool {
    !hooks_suppressed() && !hook_commands("pre_request").is_empty()
}

/// Run the configured `pre_request` hooks in declaration order.
///
/// Each command receives the request as it stands after the previous command,
/// and returns the replacement messages for this request, or `None` when no
/// command replaced them.
pub async fn rewrite(context: &RequestContext<'_>, messages: &[Message]) -> Option<Vec<Message>> {
    if hooks_suppressed() {
        return None;
    }
    let command_lines = hook_commands("pre_request");
    if command_lines.is_empty() {
        return None;
    }
    let timeout = Duration::from_millis(crate::config::config().hooks.pre_request_timeout_ms.max(1));
    let mut event = HookEvent::new("pre_request")
        .session_id(context.session_id)
        .field("MODEL", context.model)
        .field("TURN", context.turn.to_string());
    if let Some(cwd) = context.cwd {
        event = event.cwd(cwd);
    }
    // The chain's state is the message array the request would send, including
    // the system entry, so every command sees exactly what the previous one
    // returned.
    let mut current = match message_array(context.system, messages) {
        Some(array) => array,
        None => {
            crate::logging::warn("Hook 'pre_request' could not serialize the request messages; leaving them unchanged");
            return None;
        }
    };
    let mut replaced = false;
    for command_line in command_lines {
        let request_json = payload(context, &current);
        let stdout = match run_command(&command_line, &event, &request_json, timeout).await {
            Some(stdout) => stdout,
            None => continue,
        };
        match replacement(&stdout, context.system) {
            Ok(None) => {}
            Ok(Some(messages)) => match message_array(context.system, &messages) {
                Some(array) => {
                    current = array;
                    replaced = true;
                }
                None => crate::logging::warn(&format!(
                    "Hook 'pre_request' command '{command_line}' produced messages that cannot be serialized; keeping the previous messages"
                )),
            },
            Err(reason) => crate::logging::warn(&format!(
                "Hook 'pre_request' command '{command_line}' {reason}; keeping the previous messages"
            )),
        }
    }
    if !replaced {
        return None;
    }
    let Value::Array(entries) = current else {
        return None;
    };
    // The system entry goes back to the request's own prompt, which the hook was
    // not allowed to change, so it is dropped here rather than forwarded.
    let entries = if context.system.trim().is_empty() {
        entries
    } else {
        entries.into_iter().skip(1).collect()
    };
    let mut messages = Vec::with_capacity(entries.len());
    for entry in entries {
        match serde_json::from_value::<Message>(entry) {
            Ok(message) => messages.push(message),
            Err(error) => {
                crate::logging::warn(&format!(
                    "Hook 'pre_request' produced a message the request cannot carry ({error}); leaving the request unchanged"
                ));
                return None;
            }
        }
    }
    Some(messages)
}

/// The JSON a hook receives on stdin.
fn payload(context: &RequestContext<'_>, messages: &Value) -> String {
    json!({
        "session_id": context.session_id,
        "turn": context.turn,
        "model": context.model,
        "messages": messages,
    })
    .to_string()
}

/// The message array a hook sees: the request's messages, with the system
/// prompt in front when the provider is sent one.
fn message_array(system: &str, messages: &[Message]) -> Option<Value> {
    let mut array = match serde_json::to_value(messages) {
        Ok(Value::Array(array)) => array,
        Ok(_) | Err(_) => return None,
    };
    if !system.trim().is_empty() {
        array.insert(0, json!({ "role": "system", "content": system }));
    }
    Some(Value::Array(array))
}

/// The replacement a hook printed.
///
/// `Ok(None)` is empty stdout, which leaves the request alone. `Err` carries the
/// reason the replacement is refused, and the caller keeps the previous
/// messages.
fn replacement(stdout: &str, system: &str) -> Result<Option<Vec<Message>>, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|error| format!("printed invalid JSON ({error})"))?;
    let Some(object) = value.as_object() else {
        return Err("printed JSON that is not an object".to_string());
    };
    let Some(entries) = object.get("messages").and_then(Value::as_array) else {
        return Err("printed JSON without a `messages` array".to_string());
    };
    // A replacement that drops the system prompt would send the model a request
    // nobody configured, so it is refused whole rather than patched up.
    let mut entries = entries.iter();
    if !system.trim().is_empty() {
        let keeps_system = matches!(
            entries.next(),
            Some(Value::Object(first))
                if first.get("role").and_then(Value::as_str) == Some("system")
                    && first.get("content").and_then(Value::as_str) == Some(system)
        );
        if !keeps_system {
            return Err("printed messages without the request's system message in front".to_string());
        }
    }
    let mut messages = Vec::with_capacity(entries.len());
    for entry in entries {
        match serde_json::from_value::<Message>(entry.clone()) {
            Ok(message) => messages.push(message),
            Err(error) => return Err(format!("printed a message the request cannot carry ({error})")),
        }
    }
    Ok(Some(messages))
}

/// Run one hook command and return its stdout, or `None` after a warning.
async fn run_command(command_line: &str, event: &HookEvent, request_json: &str, timeout: Duration) -> Option<String> {
    let std_cmd = match build_hook_process(command_line, event) {
        Ok(cmd) => cmd,
        Err(error) => {
            crate::logging::warn(&format!(
                "Hook 'pre_request' command '{command_line}' is invalid: {error} (leaving the request unchanged)"
            ));
            return None;
        }
    };
    let mut cmd = tokio::process::Command::from(std_cmd);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            crate::logging::warn(&format!(
                "Hook 'pre_request' command '{command_line}' failed to start: {error} (leaving the request unchanged)"
            ));
            return None;
        }
    };
    horon_agent_perf_counters::record_hook_spawn();
    // One deadline covers the stdin write and the wait, as for the other
    // awaited hooks: a hook that never reads stdin would otherwise block once
    // the pipe fills. `kill_on_drop` reaps the child when the deadline fires.
    let output = match tokio::time::timeout(timeout, async {
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            if let Err(error) = stdin.write_all(request_json.as_bytes()).await {
                crate::logging::debug(&format!(
                    "Hook 'pre_request' command '{command_line}' stopped reading stdin: {error}"
                ));
            }
        }
        child.wait_with_output().await
    })
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            crate::logging::warn(&format!(
                "Hook 'pre_request' command '{command_line}' failed: {error} (leaving the request unchanged)"
            ));
            return None;
        }
        Err(_elapsed) => {
            crate::logging::warn(&format!(
                "Hook 'pre_request' command '{command_line}' timed out after {}ms (leaving the request unchanged)",
                timeout.as_millis()
            ));
            return None;
        }
    };
    if !output.status.success() {
        crate::logging::warn(&format!(
            "Hook 'pre_request' command '{command_line}' exited with {:?} (leaving the request unchanged)",
            output.status.code()
        ));
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
#[path = "pre_request_tests.rs"]
mod tests;
