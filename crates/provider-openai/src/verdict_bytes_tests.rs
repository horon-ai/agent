//! A stored verdict must not reach the provider.
//!
//! The verdict is what horon-agent's own code reads; the request bytes are what
//! the model reads. These tests pin both ends: the same conversation renders
//! the same bytes whether the result states a verdict or not, and a session
//! stored before verdicts existed replays as the same model-facing request.

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

fn rendered(messages: &[Message]) -> String {
    serde_json::to_string(&build_responses_input(messages)).expect("responses input")
}

#[test]
fn a_stated_verdict_does_not_change_the_request_bytes() {
    let unstated = conversation(None);
    let responses_baseline = rendered(&unstated);
    let chat_baseline = serde_json::to_string(&build_chat_completions_request("gpt-test", &unstated, &[], "", None))
        .expect("chat request");
    assert!(
        responses_baseline.contains("function_call_output"),
        "the baseline must render tool results: {responses_baseline}"
    );

    for verdict in VERDICTS {
        let stated = conversation(Some(verdict));
        assert_eq!(
            rendered(&stated),
            responses_baseline,
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
/// one call succeeded, the other failed.
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

    let stored = rendered(&messages);
    assert_eq!(
        stored,
        rendered(&conversation(None)),
        "a session stored before verdicts existed must replay as the same typed conversation"
    );

    // Two facts the byte equality above cannot state by itself: neither result
    // leaks the key the model must never see, and the failing call still
    // carries the error the model has always read.
    assert!(
        !stored.contains("verdict"),
        "the stored verdict must not reach the request: {stored}"
    );
    let items: Vec<serde_json::Value> = serde_json::from_str(&stored).expect("request items");
    let outputs: Vec<&str> = items
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .filter_map(|item| item["output"].as_str())
        .collect();
    assert_eq!(outputs, ["alpha", "[Error] boom"]);
}
