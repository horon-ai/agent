//! Reading a stored message's `content`.
//!
//! Lives outside [`translate`](crate::translate) because several callers flatten
//! content and that file is at its size budget.

use serde::Deserialize;
use serde_json::Value;

use horon_agent_base::message::ContentBlock;
use horon_agent_harness_api::{HistoryMessage, ResponseStats};

/// Flatten a stored message's `content` to plain text.
///
/// The daemon writes content either as a bare string or as an array of typed
/// blocks, so both shapes are accepted; anything without visible output (a tool
/// call, an image) contributes nothing, while stored A2UI blocks render through
/// [`ContentBlock::a2ui_text_card`] so history readers and replay see the card.
pub(crate) fn flatten_content(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    let mut prev_was_a2ui = false;
    for block in blocks {
        if let Some(text) = block["text"].as_str() {
            if prev_was_a2ui && !text.is_empty() && !text.starts_with('\n') && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(text);
            prev_was_a2ui = false;
        } else if block["type"].as_str() == Some("a2ui")
            && let Ok(parsed) = serde_json::from_value::<ContentBlock>(block.clone())
            && let Some(card) = parsed.a2ui_text_card()
        {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&card);
            prev_was_a2ui = true;
        }
    }
    out
}

/// The visible answer of a stored assistant row that also carries reasoning.
///
/// [`flatten_content`] keeps the historical value: every block's `text` in
/// storage order, so reasoning and answer can be one string with no marker.
/// This walks the typed blocks again and joins only visible text, answering
/// `None` when the row carries no reasoning block so every other row keeps
/// reading its `content`. Only the concrete `text` kind is a visible answer;
/// every other kind contributes nothing, so an unknown block type can never
/// make a row look answerless.
///
/// The kinds are the stored tags of `ContentBlock`'s reasoning variants, which
/// are their serde `snake_case` names because the record is written and read
/// through that enum. `OpenAIReasoning` therefore stores as
/// `open_a_i_reasoning`: serde breaks `snake_case` before every capital, so the
/// acronym keeps its underscores. That spelling is what sessions on disk hold,
/// and it is not a typo to "fix" to `openai_reasoning`.
pub(crate) fn answer_without_reasoning(content: &Value) -> Option<String> {
    const REASONING_KINDS: [&str; 4] = [
        "reasoning",
        "reasoning_trace",
        "anthropic_thinking",
        "open_a_i_reasoning",
    ];
    let blocks = content.as_array()?;
    // A row without reasoning keeps its `content`, so its text is never copied.
    if !blocks
        .iter()
        .any(|block| REASONING_KINDS.contains(&block["type"].as_str().unwrap_or("")))
    {
        return None;
    }
    let mut answer = String::new();
    let mut prev_was_a2ui = false;
    for block in blocks {
        if block["type"] == "text" {
            if let Some(text) = block["text"].as_str() {
                if prev_was_a2ui
                    && !text.is_empty()
                    && !text.starts_with('\n')
                    && !answer.is_empty()
                    && !answer.ends_with('\n')
                {
                    answer.push('\n');
                }
                answer.push_str(text);
                prev_was_a2ui = false;
            }
        } else if block["type"].as_str() == Some("a2ui")
            && let Ok(parsed) = serde_json::from_value::<ContentBlock>(block.clone())
            && let Some(card) = parsed.a2ui_text_card()
        {
            if !answer.is_empty() && !answer.ends_with('\n') {
                answer.push('\n');
            }
            answer.push_str(&card);
            prev_was_a2ui = true;
        }
    }
    Some(answer)
}

/// One history row from the daemon's flat `role`/`content` event message.
///
/// The daemon already has the reasoning markup applied, so the row's `content`
/// is forwarded as it is and `answer` is derived with the same helper the
/// daemon used to write it. `message_id` is forwarded only when the daemon put
/// one on the row, which it does for this bridge's subscription alone.
pub(crate) fn history_message(message: &Value) -> HistoryMessage {
    let content = message["content"].as_str().unwrap_or("").to_string();
    let answer = horon_agent_base::session::split_rendered_reasoning(&content).map(|(_, answer)| answer);
    HistoryMessage {
        // Deserialized from the borrowed value, so the stats are not cloned first.
        response_stats: Option::<ResponseStats>::deserialize(&message["response_stats"]).unwrap_or(None),
        role: message["role"].as_str().unwrap_or("").to_string(),
        content,
        answer,
        message_id: message["message_id"].as_str().map(str::to_string),
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

        // `open_a_i_reasoning` is the tag serde writes for
        // `ContentBlock::OpenAIReasoning`, and the one stored sessions already
        // hold; a row that spells it any other way was never written.
        let native = json!([
            {"type": "open_a_i_reasoning", "summary": ["hidden"]},
            {"type": "text", "text": "the answer"},
        ]);
        assert_eq!(answer_without_reasoning(&native), Some("the answer".to_string()));
    }

    /// The tags above are the serde names of the enum's variants, so a rename
    /// there has to reach the match instead of quietly leaving every stored
    /// reasoning row without an answer.
    #[test]
    fn every_reasoning_variant_stores_a_tag_the_match_reads() {
        use horon_agent_base::message::ContentBlock;

        let answer = ContentBlock::Text {
            text: "the answer".to_string(),
            cache_control: None,
        };
        let stored = |block: &ContentBlock| {
            json!([
                serde_json::to_value(block).expect("a stored block"),
                serde_json::to_value(&answer).expect("a stored block"),
            ])
        };
        for block in [
            ContentBlock::Reasoning {
                text: "thinking".to_string(),
            },
            ContentBlock::ReasoningTrace {
                text: "thinking".to_string(),
            },
            ContentBlock::AnthropicThinking {
                thinking: "thinking".to_string(),
                signature: "signature".to_string(),
            },
            ContentBlock::OpenAIReasoning {
                id: "rs_1".to_string(),
                summary: vec!["hidden".to_string()],
                encrypted_content: None,
                status: None,
            },
        ] {
            assert_eq!(
                answer_without_reasoning(&stored(&block)),
                Some("the answer".to_string()),
                "{block:?} is stored as a tag this match must read"
            );
        }
    }

    #[test]
    fn a_row_without_reasoning_keeps_its_content() {
        let content = json!([{"type": "text", "text": "the answer"}]);
        assert_eq!(answer_without_reasoning(&content), None);
        assert_eq!(answer_without_reasoning(&json!("plain string")), None);
    }

    #[test]
    fn a2ui_blocks_appear_in_flattened_history_and_answers_without_reasoning() {
        let a2ui_block = serde_json::to_value(ContentBlock::A2ui {
            envelopes: vec![json!({
                "version": horon_agent_harness_api::A2UI_VERSION_V1_0,
                "createSurface": {
                    "surfaceId": "main",
                    "catalogId": horon_agent_harness_api::A2UI_BASIC_CATALOG_V1_0,
                }
            })],
            fallback_text: Some("Deployment form".to_string()),
        })
        .expect("serialize a2ui block");

        let only_a2ui = json!([a2ui_block.clone()]);
        assert_eq!(flatten_content(&only_a2ui), "[A2UI surface: main]\nDeployment form");

        let with_reasoning = json!([
            {"type": "reasoning", "text": "thinking"},
            a2ui_block,
        ]);
        assert_eq!(
            answer_without_reasoning(&with_reasoning),
            Some("[A2UI surface: main]\nDeployment form".to_string())
        );
    }
}
