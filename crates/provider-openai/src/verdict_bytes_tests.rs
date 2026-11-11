//! A stored verdict must not reach the provider.
//!
//! The verdict is what horon-agent's own code reads; the request bytes are what
//! the model reads. These tests pin both ends: the same conversation renders
//! the same bytes whether the result states a verdict or not, and a session
//! stored before verdicts existed replays exactly as it did then.

use crate::{build_chat_completions_request, build_responses_input};
use horon_agent_message_types::{ContentBlock, Message, Role, ToolVerdict};

const VERDICTS: [ToolVerdict; 7] = [
    ToolVerdict::Ok,
    ToolVerdict::Partial,
    ToolVerdict::Truncated,
    ToolVerdict::Withheld,
    ToolVerdict::Rejected,
    ToolVerdict::Failed,
    ToolVerdict::Interrupted,
];

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }
}

fn tool_use(id: &str, name: &str, input: serde_json::Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.to_string(),
        name: name.to_string(),
        input,
        thought_signature: None,
    }
}

fn conversation(verdict: Option<ToolVerdict>) -> Vec<Message> {
    let message = |role: Role, content: Vec<ContentBlock>| Message {
        role,
        content,
        timestamp: None,
        tool_duration_ms: None,
    };
    vec![
        message(Role::User, vec![text("read both files")]),
        message(
            Role::Assistant,
            vec![
                tool_use("call_read", "read", serde_json::json!({ "path": "a.txt" })),
                tool_use("call_bash", "bash", serde_json::json!({ "command": "false" })),
            ],
        ),
        message(
            Role::User,
            vec![
                ContentBlock::tool_result_with_verdict("call_read", "alpha", None, verdict),
                ContentBlock::tool_result_with_verdict("call_bash", "boom", Some(true), verdict),
            ],
        ),
    ]
}

#[test]
fn a_stated_verdict_does_not_change_the_request_bytes() {
    let unstated = conversation(None);
    let responses_baseline = serde_json::to_string(&build_responses_input(&unstated)).expect("responses input");
    let chat_baseline = serde_json::to_string(&build_chat_completions_request("gpt-test", &unstated, &[], "", None))
        .expect("chat request");
    assert!(
        responses_baseline.contains("function_call_output"),
        "the baseline must render tool results: {responses_baseline}"
    );

    for verdict in VERDICTS {
        let stated = conversation(Some(verdict));
        let responses = serde_json::to_string(&build_responses_input(&stated)).expect("responses input");
        assert_eq!(
            responses, responses_baseline,
            "verdict {verdict:?} changed the Responses request bytes"
        );

        let chat = serde_json::to_string(&build_chat_completions_request("gpt-test", &stated, &[], "", None))
            .expect("chat request");
        assert_eq!(
            chat, chat_baseline,
            "verdict {verdict:?} changed the Chat Completions request bytes"
        );
    }
}

/// A session stored before verdicts existed carries no `verdict` key at all;
/// one call succeeded, the other failed. Replaying it must still produce the
/// same model-facing request, which is the whole promise of the optional key.
const PRE_CHANGE_SESSION: &str = r#"[
  {
    "role": "user",
    "content": [{ "type": "text", "text": "read both files" }]
  },
  {
    "role": "assistant",
    "content": [
      { "type": "tool_use", "id": "call_read", "name": "read", "input": { "path": "a.txt" } },
      { "type": "tool_use", "id": "call_bash", "name": "bash", "input": { "command": "false" } }
    ]
  },
  {
    "role": "user",
    "content": [
      { "type": "tool_result", "tool_use_id": "call_read", "content": "alpha" },
      { "type": "tool_result", "tool_use_id": "call_bash", "content": "boom", "is_error": true }
    ]
  }
]"#;

#[test]
fn a_pre_change_session_replays_with_the_same_model_facing_bytes() {
    let messages: Vec<Message> = serde_json::from_str(PRE_CHANGE_SESSION).expect("the pre-change session must load");
    for message in &messages {
        for block in &message.content {
            if let ContentBlock::ToolResult { verdict, .. } = block {
                assert_eq!(
                    *verdict, None,
                    "a stored result without the key must read as no verdict"
                );
            }
        }
    }

    // The same conversation written with the current types, no verdict stated.
    let typed = conversation(None);
    let rendered = serde_json::to_string(&build_responses_input(&messages)).expect("responses input");
    let expected = serde_json::to_string(&build_responses_input(&typed)).expect("responses input");
    assert_eq!(
        rendered, expected,
        "a session stored before verdicts existed must replay exactly as the same typed conversation"
    );

    // Pin the shape the equality above would otherwise prove only against
    // itself: one user message, one call per tool, results keyed by call id,
    // and the error prefix unchanged.
    assert_eq!(
        rendered,
        concat!(
            r#"[{"type":"message","role":"user","content":[{"type":"input_text","text":"read both files"}]}"#,
            r#",{"type":"function_call","name":"read","arguments":"{\"path\":\"a.txt\"}","call_id":"call_read"}"#,
            r#",{"type":"function_call","name":"bash","arguments":"{\"command\":\"false\"}","call_id":"call_bash"}"#,
            r#",{"type":"function_call_output","call_id":"call_read","output":"alpha"}"#,
            r#",{"type":"function_call_output","call_id":"call_bash","output":"[Error] boom"}]"#,
        )
    );
}
