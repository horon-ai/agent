//! The `post_tool_feedback` hook: what a completed tool call adds for the model.
//!
//! By default a hook's stdout is appended to the tool result under a
//! `[post_tool_feedback]` header, which is how linters report findings inline.
//! A hook that prints a JSON object with `content`, `is_error` or `note`
//! instead decides the result itself: `content` replaces the text the model
//! sees, `is_error` marks the result as an error, and `note` becomes a separate
//! system reminder after the result.
//!
//! Hooks run in declaration order and each one receives the result as the
//! previous hook left it in `HORON_AGENT_HOOK_OUTPUT`. The hooks never change
//! the file state or the undo journal: only the model's view of the result.

use super::{HookEvent, build_hook_process, hook_commands, truncate_bytes};
use serde_json::Value;
use std::time::Duration;

/// Maximum bytes one hook may contribute to a tool result.
const FEEDBACK_LIMIT: usize = 8 * 1024;
/// Maximum bytes of the current result text exported as `HORON_AGENT_HOOK_OUTPUT`.
const OUTPUT_ENV_LIMIT: usize = 8 * 1024;

/// One ordered effect of a feedback hook on the result text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// Text appended under the `[post_tool_feedback]` header.
    Text(String),
    /// Text that replaces everything the model saw so far.
    Replace(String),
}

/// What the feedback hooks did to one tool result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feedback {
    parts: Vec<Part>,
    is_error: Option<bool>,
    notes: Vec<String>,
}

impl Feedback {
    /// Whether the hooks changed anything at all.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty() && self.is_error.is_none() && self.notes.is_empty()
    }

    /// The text effects in the order the hooks declared them.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The error flag the hooks asked for, if any.
    pub fn is_error(&self) -> Option<bool> {
        self.is_error
    }

    /// The system reminders the hooks asked for, in order.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }
}

/// Run the `post_tool_feedback` hooks for a completed tool call.
///
/// `current_output` is the text the model would see without feedback, and each
/// hook receives the running value. Empty stdout contributes nothing; exit code
/// is ignored because a linter conventionally exits non-zero when it has
/// findings. Timeouts and spawn failures contribute nothing and are logged.
pub async fn run(event: &HookEvent, tool_input_json: &str, current_output: &str) -> Feedback {
    let command_lines = hook_commands("post_tool_feedback");
    let mut feedback = Feedback::default();
    if command_lines.is_empty() {
        return feedback;
    }
    let timeout = Duration::from_millis(crate::config::config().hooks.post_tool_feedback_timeout_ms.max(1));
    let mut current = current_output.to_string();
    // Consecutive plain-text hooks share one header and one blank line between
    // them, so a suite of linters reads as a single block.
    let mut pending: Vec<String> = Vec::new();
    for command_line in command_lines {
        let mut command_event = event.clone();
        command_event
            .fields
            .push(("OUTPUT", truncate_bytes(&current, OUTPUT_ENV_LIMIT).to_string()));
        let stdout = match run_command(&command_line, &command_event, tool_input_json, timeout).await {
            Some(stdout) => stdout,
            None => continue,
        };
        match envelope(&stdout) {
            Some(envelope) => {
                if let Some(content) = envelope.content {
                    flush(&mut feedback, &mut pending);
                    current = content.clone();
                    feedback.parts.push(Part::Replace(content));
                }
                if let Some(is_error) = envelope.is_error {
                    feedback.is_error = Some(is_error);
                }
                if let Some(note) = envelope.note {
                    feedback.notes.push(note);
                }
            }
            None => {
                let text = capped(&stdout);
                if text.is_empty() {
                    continue;
                }
                current = append_text(&current, &text);
                pending.push(text);
            }
        }
    }
    flush(&mut feedback, &mut pending);
    feedback
}

/// A JSON envelope one hook printed.
struct Envelope {
    content: Option<String>,
    is_error: Option<bool>,
    note: Option<String>,
}

/// The envelope in `stdout`, or `None` when the output is ordinary text.
///
/// Only an object that carries at least one of `content` (string), `is_error`
/// (bool) or `note` (string) is an envelope; anything else keeps the append
/// behavior every existing hook relies on.
fn envelope(stdout: &str) -> Option<Envelope> {
    let text = stdout.trim();
    if !text.starts_with('{') {
        return None;
    }
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => return None,
    };
    let object = value.as_object()?;
    if !object.contains_key("content") && !object.contains_key("is_error") && !object.contains_key("note") {
        return None;
    }
    Some(Envelope {
        content: object.get("content").and_then(Value::as_str).map(capped),
        is_error: object.get("is_error").and_then(Value::as_bool),
        note: object.get("note").and_then(Value::as_str).map(str::to_string),
    })
}

/// `text` trimmed and capped, with the truncation marker the append path uses.
fn capped(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut capped = truncate_bytes(trimmed, FEEDBACK_LIMIT).to_string();
    if capped.len() < trimmed.len() {
        capped.push_str("\n[feedback truncated]");
    }
    capped
}

/// Append one section under the header, matching the pre-envelope behavior.
fn append_text(current: &str, text: &str) -> String {
    let mut out = current.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n[post_tool_feedback]\n");
    out.push_str(text);
    out
}

/// Emit the plain-text run collected so far as one appended section.
fn flush(feedback: &mut Feedback, pending: &mut Vec<String>) {
    if pending.is_empty() {
        return;
    }
    feedback.parts.push(Part::Text(pending.join("\n\n")));
    pending.clear();
}

/// Run one hook command and return its stdout, or `None` after a warning.
async fn run_command(
    command_line: &str,
    event: &HookEvent,
    tool_input_json: &str,
    timeout: Duration,
) -> Option<String> {
    let std_cmd = match build_hook_process(command_line, event) {
        Ok(cmd) => cmd,
        Err(error) => {
            crate::logging::warn(&format!(
                "Hook 'post_tool_feedback' command '{command_line}' is invalid: {error}"
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
                "Hook 'post_tool_feedback' command '{command_line}' failed to start: {error}"
            ));
            return None;
        }
    };
    horon_agent_perf_counters::record_hook_spawn();
    // Keep the stdin write under the deadline too: a hook that never reads
    // stdin could otherwise block once the pipe fills.
    let output = match tokio::time::timeout(timeout, async {
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            if let Err(error) = stdin.write_all(tool_input_json.as_bytes()).await {
                crate::logging::debug(&format!(
                    "Hook 'post_tool_feedback' command '{command_line}' stopped reading stdin: {error}"
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
                "Hook 'post_tool_feedback' command '{command_line}' failed: {error}"
            ));
            return None;
        }
        Err(_elapsed) => {
            crate::logging::warn(&format!(
                "Hook 'post_tool_feedback' command '{command_line}' timed out after {}ms",
                timeout.as_millis()
            ));
            return None;
        }
    };
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
#[path = "post_tool_feedback_tests.rs"]
mod tests;
