//! Reading a stored message's `content`.
//!
//! Lives outside [`translate`](crate::translate) because several callers flatten
//! content and that file is at its size budget.

use serde_json::Value;

use horon_agent_harness_api::{HistoryMessage, ResponseStats};

/// Flatten a stored message's `content` to plain text.
///
/// The daemon writes content either as a bare string or as an array of typed
/// blocks, so both shapes are accepted; anything without text (a tool call, an
/// image) contributes nothing rather than a placeholder.
pub(crate) fn flatten_content(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    blocks
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

/// The visible answer of a stored assistant row that also carries reasoning.
///
/// [`flatten_content`] keeps the historical value: every block's `text` in
/// storage order, so reasoning and answer can be one string with no marker.
/// This walks the typed blocks again and joins only visible text, answering
/// `None` when the row carries no reasoning block so every other row keeps
/// reading its `content`. Only the concrete `text` kind is a visible answer;
/// every other kind contributes nothing. The reasoning kinds are listed
/// explicitly, with both spellings serde can produce for the provider-native
/// variant, so an unknown block type can never make a row look answerless.
pub(crate) fn answer_without_reasoning(content: &Value) -> Option<String> {
    const REASONING_KINDS: [&str; 5] = [
        "reasoning",
        "reasoning_trace",
        "anthropic_thinking",
        "open_ai_reasoning",
        "openai_reasoning",
    ];
    let blocks = content.as_array()?;
    let mut reasoning = false;
    let mut answer = String::new();
    for block in blocks {
        match block["type"].as_str().unwrap_or("") {
            "text" => {
                if let Some(text) = block["text"].as_str() {
                    answer.push_str(text);
                }
            }
            kind if REASONING_KINDS.contains(&kind) => reasoning = true,
            _ => {}
        }
    }
    reasoning.then_some(answer)
}

/// One history row from the daemon's flat `role`/`content` event message.
///
/// The daemon already has the reasoning markup applied, so the row's `content`
/// is forwarded as it is and `answer` is derived with the same helper the
/// daemon used to write it.
pub(crate) fn history_message(message: &Value) -> HistoryMessage {
    let content = message["content"].as_str().unwrap_or("").to_string();
    let answer = horon_agent_base::session::split_rendered_reasoning(&content).map(|(_, answer)| answer);
    HistoryMessage {
        response_stats: serde_json::from_value::<Option<ResponseStats>>(message["response_stats"].clone())
            .unwrap_or(None),
        role: message["role"].as_str().unwrap_or("").to_string(),
        content,
        answer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_row_with_reasoning_names_the_visible_answer() {
        let content = json!([
            {"type": "reasoning", "text": "thinking"},
            {"type": "text", "text": "the answer"},
        ]);
        assert_eq!(answer_without_reasoning(&content), Some("the answer".to_string()));

        // The serde spelling of the provider-native reasoning kinds is not
        // part of this match; any kind naming reasoning or thinking counts.
        let native = json!([
            {"type": "open_a_i_reasoning", "summary": ["hidden"]},
            {"type": "text", "text": "the answer"},
        ]);
        assert_eq!(answer_without_reasoning(&native), Some("the answer".to_string()));
    }

    #[test]
    fn a_row_without_reasoning_keeps_its_content() {
        let content = json!([{"type": "text", "text": "the answer"}]);
        assert_eq!(answer_without_reasoning(&content), None);
        assert_eq!(answer_without_reasoning(&json!("plain string")), None);
    }
}
