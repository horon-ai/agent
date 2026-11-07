//! Emergency truncation of oversized tool results.
//!
//! Kept in its own module because `crates/compaction-core/src/lib.rs` is a
//! tracked oversized file: the code-size ratchet forbids growing it, so the
//! truncation cluster lives here and is re-exported from the crate root.

use horon_agent_message_types::{ContentBlock, Message};

use crate::truncate_str_boundary;

/// Longest single signal line copied out of the omitted middle of a truncated
/// tool result.
pub const EMERGENCY_SIGNAL_LINE_MAX_BYTES: usize = 200;

/// Most signal lines copied out of the omitted middle.
pub const EMERGENCY_SIGNAL_MAX_LINES: usize = 8;

/// Byte budget shared by all signal lines of one truncated result.
pub const EMERGENCY_SIGNAL_TOTAL_MAX_BYTES: usize = 600;

const EMERGENCY_SIGNAL_HEADER: &str = "[error/warning lines from the omitted part:]\n";
const EMERGENCY_TRUNCATION_MARKER_HEAD: &str = "\n\n... [";
const EMERGENCY_TRUNCATION_MARKER_TAIL: &str = " chars truncated for context recovery] ...\n\n";
const EMERGENCY_TOOL_ERROR_PREFIX: &str = "[tool error] ";

/// Words that mark a line as carrying a failure or warning signal. Matched
/// case-insensitively as whole ASCII words, so `error:` and `FAILED` match but
/// `errors_handled` does not.
const EMERGENCY_SIGNAL_WORDS: [&str; 8] = [
    "error",
    "warning",
    "failed",
    "failure",
    "panic",
    "exception",
    "traceback",
    "fatal",
];

pub fn emergency_truncate_tool_results(messages: &mut [Message], max_bytes: usize) -> usize {
    let mut truncated = 0;

    for msg in messages.iter_mut() {
        for block in msg.content.iter_mut() {
            match block {
                ContentBlock::ToolResult { content, is_error, .. } if content.len() > max_bytes => {
                    *content = emergency_truncated_tool_result(content, max_bytes, *is_error);
                    truncated += 1;
                }
                _ => {}
            }
        }
    }

    truncated
}

pub fn emergency_truncate_large_payloads(
    messages: &mut [Message],
    max_tool_result_bytes: usize,
    max_image_chars: usize,
) -> usize {
    let mut truncated = 0;

    for msg in messages.iter_mut() {
        for block in msg.content.iter_mut() {
            match block {
                ContentBlock::ToolResult { content, is_error, .. } if content.len() > max_tool_result_bytes => {
                    *content = emergency_truncated_tool_result(content, max_tool_result_bytes, *is_error);
                    truncated += 1;
                }
                ContentBlock::Image { media_type, data } if data.len() > max_image_chars => {
                    let original_len = data.len();
                    let media_type = media_type.clone();
                    *block = ContentBlock::Text {
                        text: format!(
                            "[Image omitted during emergency context recovery: media_type={media_type}, original_base64_chars={original_len}. Rely on adjacent browser/tool text, screenshots saved to disk, or re-open/re-screenshot if visual details are needed.]"
                        ),
                        cache_control: None,
                    };
                    truncated += 1;
                }
                ContentBlock::Audio { format, data } if data.len() > max_image_chars => {
                    let original_len = data.len();
                    let format = format.clone();
                    *block = ContentBlock::Text {
                        text: format!(
                            "[Audio omitted during emergency context recovery: format={format}, original_base64_chars={original_len}. Re-record or re-attach the clip if it is needed.]"
                        ),
                        cache_control: None,
                    };
                    truncated += 1;
                }
                _ => {}
            }
        }
    }

    truncated
}

/// Shorten a tool result to `max_bytes`, keeping the head, the tail and the
/// error/warning lines of the omitted middle.
///
/// The result never exceeds `max_bytes` once the budget can hold the marker;
/// a budget smaller than the marker itself keeps the marker anyway, because
/// losing the phrase would hide the truncation. `is_error` prefixes the
/// `"[tool error] "` marker so a failed tool cannot read as a success after its
/// payload was cut.
pub fn emergency_truncated_tool_result(content: &str, max_bytes: usize, is_error: Option<bool>) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }

    let prefix = if is_error == Some(true) {
        EMERGENCY_TOOL_ERROR_PREFIX
    } else {
        ""
    };
    let fixed_overhead = prefix.len()
        + EMERGENCY_TRUNCATION_MARKER_HEAD.len()
        + EMERGENCY_TRUNCATION_MARKER_TAIL.len()
        + decimal_digits(content.len());
    let signal_reserve = EMERGENCY_SIGNAL_HEADER.len() + EMERGENCY_SIGNAL_TOTAL_MAX_BYTES + 2;

    let mut head_len = max_bytes / 2;
    let mut tail_len = max_bytes / 4;
    let body_budget = max_bytes.saturating_sub(fixed_overhead + signal_reserve);
    if head_len + tail_len > body_budget {
        head_len = body_budget * 2 / 3;
        tail_len = body_budget / 3;
    }

    let head = truncate_str_boundary(content, head_len);
    let tail = tail_str_boundary(content, tail_len);
    let omitted = &content[head.len()..content.len() - tail.len()];
    let signals = collect_emergency_signal_lines(omitted);
    let truncated_len = content.len() - head.len() - tail.len();

    let mut out = String::with_capacity(max_bytes.max(EMERGENCY_TRUNCATION_MARKER_TAIL.len()));
    out.push_str(prefix);
    out.push_str(head);
    out.push_str(EMERGENCY_TRUNCATION_MARKER_HEAD);
    out.push_str(&truncated_len.to_string());
    out.push_str(EMERGENCY_TRUNCATION_MARKER_TAIL);
    if !signals.is_empty() {
        out.push_str(EMERGENCY_SIGNAL_HEADER);
        for line in &signals {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(tail);
    out
}

fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn collect_emergency_signal_lines(omitted: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut used = 0usize;
    for raw in omitted.lines() {
        if !line_has_signal_word(raw) {
            continue;
        }
        let line = truncate_str_boundary(raw.trim_end(), EMERGENCY_SIGNAL_LINE_MAX_BYTES).trim_end();
        let cost = line.len() + 1;
        if used + cost > EMERGENCY_SIGNAL_TOTAL_MAX_BYTES {
            break;
        }
        used += cost;
        lines.push(line.to_string());
        if lines.len() == EMERGENCY_SIGNAL_MAX_LINES {
            break;
        }
    }
    lines
}

pub(crate) fn line_has_signal_word(line: &str) -> bool {
    let bytes = line.as_bytes();
    EMERGENCY_SIGNAL_WORDS
        .iter()
        .any(|word| contains_ascii_word(bytes, word.as_bytes()))
}

fn contains_ascii_word(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    (0..=haystack.len() - needle.len()).any(|start| {
        haystack[start..start + needle.len()].eq_ignore_ascii_case(needle)
            && (start == 0 || !is_word_byte(haystack[start - 1]))
            && (start + needle.len() == haystack.len() || !is_word_byte(haystack[start + needle.len()]))
    })
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

pub fn tail_str_boundary(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut start = value.len().saturating_sub(max_bytes);
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}
