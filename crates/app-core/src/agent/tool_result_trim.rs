//! An explicit, bounded trim of old tool results in the provider projection.
//!
//! The trim changes only what one request carries. The stored transcript, every
//! message id, and the pairing between each tool call and its result stay as
//! they are, so `history://current` still serves the original records. A
//! trimmed result keeps its call id, error flag and verdict; only its text is
//! replaced by the first part of the original and a note saying how much was
//! left out. Recent messages and error results are never trimmed.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use horon_agent_message_types::{ContentBlock, Message};

/// Smallest accepted size limit: below this a trim would keep too little to be useful.
pub const MIN_MAX_CHARS: usize = 200;
/// Largest accepted size limit.
pub const MAX_MAX_CHARS: usize = 1_000_000;
/// Limit used when the user gives none.
pub const DEFAULT_MAX_CHARS: usize = 4_000;
/// The newest messages are never trimmed: the model is still working with them.
pub const KEEP_RECENT_MESSAGES: usize = 6;
/// How much of a trimmed result is kept at its start.
const HEAD_CHARS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimPolicy {
    pub max_chars: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct TrimStats {
    pub results: usize,
    pub chars_removed: usize,
}

impl TrimPolicy {
    /// Validate a user-supplied limit. A refused limit changes nothing.
    pub fn new(max_chars: usize) -> anyhow::Result<Self> {
        if !(MIN_MAX_CHARS..=MAX_MAX_CHARS).contains(&max_chars) {
            anyhow::bail!(
                "The trim limit must be between {MIN_MAX_CHARS} and {MAX_MAX_CHARS} characters, not {max_chars}."
            );
        }
        Ok(Self { max_chars })
    }
}

#[inline]
fn char_len(content: &str) -> usize {
    if content.is_ascii() {
        content.len()
    } else {
        content.chars().count()
    }
}

#[inline]
fn head_end_for_trim(content: &str) -> usize {
    if content.is_ascii() {
        return HEAD_CHARS.min(content.len());
    }
    content
        .char_indices()
        .nth(HEAD_CHARS)
        .map_or(content.len(), |(idx, _)| idx)
}

fn push_usize(out: &mut String, mut value: usize) {
    let mut buf = [0u8; 20];
    let mut pos = buf.len();
    loop {
        pos -= 1;
        buf[pos] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    if let Ok(digits) = std::str::from_utf8(&buf[pos..]) {
        out.push_str(digits);
    }
}

/// What a trim would do to `messages`, without changing anything.
pub fn preview(messages: &[Message], policy: TrimPolicy) -> TrimStats {
    let mut stats = TrimStats::default();
    let protected_from = messages.len().saturating_sub(KEEP_RECENT_MESSAGES);
    for message in &messages[..protected_from] {
        for block in &message.content {
            if let ContentBlock::ToolResult { content, is_error, .. } = block
                && *is_error != Some(true)
                && content.len() > policy.max_chars
            {
                let total = char_len(content);
                if total > policy.max_chars {
                    stats.results += 1;
                    stats.chars_removed += total.saturating_sub(HEAD_CHARS);
                }
            }
        }
    }
    stats
}

/// A copy of `messages` with the eligible tool results shortened.
///
/// Returns `None` when nothing is eligible, so an unaffected request keeps
/// borrowing its snapshot.
pub fn apply(messages: &[Message], policy: TrimPolicy) -> Option<Vec<Message>> {
    let protected_from = messages.len().saturating_sub(KEEP_RECENT_MESSAGES);
    let mut trimmed: Option<Vec<Message>> = None;
    for (msg_idx, message) in messages[..protected_from].iter().enumerate() {
        for (block_idx, block) in message.content.iter().enumerate() {
            let ContentBlock::ToolResult { content, is_error, .. } = block else {
                continue;
            };
            if *is_error == Some(true) || content.len() <= policy.max_chars {
                continue;
            }
            let total = char_len(content);
            if total <= policy.max_chars {
                continue;
            }
            let head_end = head_end_for_trim(content);
            let out = trimmed.get_or_insert_with(|| messages.to_vec());
            if let Some(ContentBlock::ToolResult { content: dst, .. }) = out[msg_idx].content.get_mut(block_idx) {
                let head = &content[..head_end];
                let removed = total.saturating_sub(HEAD_CHARS);
                let mut replacement = String::with_capacity(head.len() + 132);
                replacement.push_str(head);
                replacement.push_str("\n[");
                push_usize(&mut replacement, removed);
                replacement.push_str(
                    " characters trimmed from this request. The full result is stored in the session history (history://current).]",
                );
                *dst = replacement;
            }
        }
    }
    trimmed
}

static POLICIES: OnceLock<Mutex<HashMap<String, TrimPolicy>>> = OnceLock::new();

fn policies() -> std::sync::MutexGuard<'static, HashMap<String, TrimPolicy>> {
    POLICIES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The trim applied to this session's requests, if the user turned one on.
pub fn policy_for(session_id: &str) -> Option<TrimPolicy> {
    policies().get(session_id).copied()
}

pub fn set_policy(session_id: &str, policy: TrimPolicy) {
    policies().insert(session_id.to_owned(), policy);
}

/// Turn the trim off. Returns whether one was on.
pub fn clear_policy(session_id: &str) -> bool {
    policies().remove(session_id).is_some()
}

impl crate::agent::Agent {
    /// `/trim [status|preview|apply|off] [max_chars]`.
    ///
    /// `preview` and `status` change nothing. `apply` turns the trim on for this
    /// session's later requests and is refused, with the previous setting kept,
    /// when the limit is out of range. The stored transcript is never touched.
    pub(crate) fn trim_command(&mut self, args: &[String]) -> anyhow::Result<(serde_json::Value, String)> {
        use anyhow::bail;
        let session_id = self.session_id().to_owned();
        let action = args.first().map(|arg| arg.trim().to_ascii_lowercase());
        let limit =
            match args.get(1) {
                Some(value) => Some(value.trim().parse::<usize>().map_err(|_| {
                    anyhow::anyhow!("The trim limit must be a whole number of characters, not '{value}'.")
                })?),
                None => None,
            };
        if args.len() > 2 {
            bail!("Usage: /trim [status|preview|apply|off] [max_chars]");
        }
        let current = policy_for(&session_id);
        let choose = |limit: Option<usize>| {
            TrimPolicy::new(
                limit
                    .or(current.map(|policy| policy.max_chars))
                    .unwrap_or(DEFAULT_MAX_CHARS),
            )
        };
        match action.as_deref() {
            None | Some("status") | Some("preview") => {
                let policy = choose(limit)?;
                let stats = preview(&self.provider_messages(), policy);
                Ok((
                    serde_json::json!({"active": current.map(|policy| policy.max_chars), "preview": {"max_chars": policy.max_chars, "results": stats.results, "chars_removed": stats.chars_removed}}),
                    format!(
                        "{} The next requests would carry {} fewer characters across {} old tool result(s) over {} characters. Recent messages and error results are kept whole. Use `/trim apply` to turn it on.",
                        match current {
                            Some(active) => format!("The trim is on at {} characters.", active.max_chars),
                            None => "The trim is off.".to_owned(),
                        },
                        stats.chars_removed,
                        stats.results,
                        policy.max_chars
                    ),
                ))
            }
            Some("apply") => {
                let policy = choose(limit)?;
                set_policy(&session_id, policy);
                let stats = preview(&self.provider_messages(), policy);
                Ok((
                    serde_json::json!({"active": policy.max_chars, "preview": {"max_chars": policy.max_chars, "results": stats.results, "chars_removed": stats.chars_removed}}),
                    format!(
                        "Trim on: tool results over {} characters are shortened in later requests. The stored history keeps the originals.",
                        policy.max_chars
                    ),
                ))
            }
            Some("off") => {
                let was_on = clear_policy(&session_id);
                Ok((
                    serde_json::json!({"active": null}),
                    if was_on {
                        "Trim off."
                    } else {
                        "The trim was already off."
                    }
                    .to_owned(),
                ))
            }
            Some(other) => bail!("Unknown /trim action '{other}'. Expected status, preview, apply, or off."),
        }
    }
}

#[cfg(test)]
#[path = "tool_result_trim_tests.rs"]
mod tests;
