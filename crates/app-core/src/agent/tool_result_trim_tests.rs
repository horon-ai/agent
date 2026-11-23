use super::*;
use horon_agent_message_types::Role;

fn message(role: Role, content: Vec<ContentBlock>) -> Message {
    Message {
        role,
        content,
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn call(id: &str) -> Message {
    message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: id.to_owned(),
            name: "read".to_owned(),
            input: serde_json::json!({}),
            thought_signature: None,
        }],
    )
}

fn result(id: &str, content: String, is_error: Option<bool>) -> Message {
    message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: id.to_owned(),
            content,
            is_error,
            verdict: None,
        }],
    )
}

fn transcript() -> Vec<Message> {
    let mut messages = vec![
        call("big"),
        result("big", "x".repeat(10_000), None),
        call("small"),
        result("small", "ok".to_owned(), None),
        call("failed"),
        result("failed", "e".repeat(10_000), Some(true)),
    ];
    // Six newer messages are protected, whatever their size.
    for index in 0..3 {
        messages.push(call(&format!("recent-{index}")));
        messages.push(result(&format!("recent-{index}"), "r".repeat(10_000), None));
    }
    messages
}

fn result_text<'a>(messages: &'a [Message], id: &str) -> &'a str {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id, content, ..
            } if tool_use_id == id => Some(content.as_str()),
            _ => None,
        })
        .expect("result present")
}

#[test]
fn only_old_oversized_successful_results_are_trimmed() {
    let original = transcript();
    let policy = TrimPolicy::new(1_000).unwrap();
    let stats = preview(&original, policy);
    assert_eq!(
        stats,
        TrimStats {
            results: 1,
            chars_removed: 9_600
        }
    );

    let trimmed = apply(&original, policy).expect("one result is eligible");
    assert!(result_text(&trimmed, "big").len() < 1_000);
    assert!(result_text(&trimmed, "big").contains("history://current"));
    assert_eq!(result_text(&trimmed, "small"), "ok");
    assert_eq!(result_text(&trimmed, "failed").len(), 10_000);
    assert_eq!(result_text(&trimmed, "recent-0").len(), 10_000);
}

#[test]
fn a_trim_keeps_every_call_result_pair_and_never_changes_the_original() {
    let original = transcript();
    let before = original.clone();
    let trimmed = apply(&original, TrimPolicy::new(1_000).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_string(&original).unwrap(),
        serde_json::to_string(&before).unwrap()
    );
    assert_eq!(trimmed.len(), original.len());
    let ids = |messages: &[Message]| -> Vec<String> {
        messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(ids(&trimmed), ids(&original));
}

#[test]
fn nothing_eligible_returns_no_copy_and_a_bad_limit_is_refused() {
    let messages = transcript();
    assert!(apply(&messages, TrimPolicy::new(100_000).unwrap()).is_none());
    assert!(TrimPolicy::new(199).is_err());
    assert!(TrimPolicy::new(1_000_001).is_err());
}

#[test]
fn the_policy_follows_the_session_until_it_is_cleared() {
    let session = "trim-policy-session";
    assert!(policy_for(session).is_none());
    set_policy(session, TrimPolicy::new(2_000).unwrap());
    assert_eq!(policy_for(session), Some(TrimPolicy { max_chars: 2_000 }));
    assert!(clear_policy(session));
    assert!(!clear_policy(session));
}
