//! Every shape the agent loop can hand to a strict wire, and the alternating
//! history it becomes.

use super::normalize;
use horon_agent_message_types::{ContentBlock, Message, Role, TOOL_OUTPUT_MISSING_TEXT};
use serde_json::json;

// ---------------------------------------------------------------------------
// Builders

fn user_text(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![text_block(text)],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn assistant_text(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![text_block(text)],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }
}

fn call(id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.to_string(),
        name: "read".to_string(),
        input: json!({ "file_path": "a.txt" }),
        thought_signature: None,
    }
}

fn result(id: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.to_string(),
        content: "done".to_string(),
        is_error: None,
        verdict: None,
    }
}

fn assistant_call(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![call(id)],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn user_result(id: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![result(id)],
        timestamp: None,
        tool_duration_ms: None,
    }
}

/// `(role, is_tool_result)` for every turn, which is the shape the wires check.
fn shape(messages: &[Message]) -> Vec<(Role, bool)> {
    messages
        .iter()
        .map(|message| {
            let all_results = message
                .content
                .iter()
                .all(|block| matches!(block, ContentBlock::ToolResult { .. }));
            (message.role.clone(), all_results)
        })
        .collect()
}

fn roles(messages: &[Message]) -> Vec<Role> {
    messages.iter().map(|message| message.role.clone()).collect()
}

fn ids_in(message: &Message) -> Vec<String> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect()
}

fn body_of(message: &Message, id: &str) -> Option<String> {
    message.content.iter().find_map(|block| match block {
        ContentBlock::ToolResult {
            tool_use_id, content, ..
        } if tool_use_id == id => Some(content.clone()),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Merging

#[test]
fn consecutive_messages_of_one_role_become_one_turn() {
    let input = vec![user_text("first"), user_text("second"), assistant_text("reply")];
    let out = normalize(&input);
    assert_eq!(roles(&out), vec![Role::User, Role::Assistant]);
    // Block order is kept, so the model still reads the two prompts in order.
    assert_eq!(out[0].content.len(), 2);
    assert!(matches!(&out[0].content[0], ContentBlock::Text { text, .. } if text == "first"));
    assert!(matches!(&out[0].content[1], ContentBlock::Text { text, .. } if text == "second"));
}

#[test]
fn a_history_that_already_alternates_is_returned_unchanged() {
    let input = vec![user_text("hi"), assistant_text("hello"), user_text("bye")];
    let out = normalize(&input);
    assert_eq!(roles(&out), roles(&input));
    assert_eq!(out.len(), input.len());
}

#[test]
fn a_merge_keeps_the_first_turns_metadata() {
    let mut first = user_text("first");
    first.tool_duration_ms = Some(12);
    let out = normalize(&[first, user_text("second")]);
    // The merged turn keeps the first message's metadata; the blocks carry the
    // content, and nothing downstream reads a per-block duration.
    assert_eq!(out[0].tool_duration_ms, Some(12));
    assert_eq!(out[0].content.len(), 2);
}

#[test]
fn an_empty_message_does_not_create_an_empty_turn() {
    let empty = Message {
        role: Role::Assistant,
        content: Vec::new(),
        timestamp: None,
        tool_duration_ms: None,
    };
    let out = normalize(&[user_text("hi"), empty, assistant_text("hello")]);
    assert_eq!(roles(&out), vec![Role::User, Role::Assistant]);
}

#[test]
fn an_empty_message_between_two_user_turns_does_not_break_alternation() {
    let empty = Message {
        role: Role::Assistant,
        content: Vec::new(),
        timestamp: None,
        tool_duration_ms: None,
    };
    let out = normalize(&[user_text("hi"), empty, user_text("again")]);
    // Dropping the empty turn leaves two user turns adjacent, which the wires
    // reject, so they fold into one.
    assert_eq!(roles(&out), vec![Role::User]);
    assert_eq!(out[0].content.len(), 2);
}

// ---------------------------------------------------------------------------
// Pairing

#[test]
fn a_paired_call_and_result_are_left_alone() {
    let input = vec![user_text("read it"), assistant_call("call_1"), user_result("call_1")];
    let out = normalize(&input);
    assert_eq!(roles(&out), vec![Role::User, Role::Assistant, Role::User]);
    assert_eq!(ids_in(&out[2]), vec!["call_1"]);
    assert_eq!(body_of(&out[2], "call_1").as_deref(), Some("done"));
}

#[test]
fn a_result_with_no_call_is_dropped() {
    let input = vec![user_text("read it"), user_result("call_ghost")];
    let out = normalize(&input);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].content.len(), 1);
    assert!(ids_in(&out[0]).is_empty(), "the orphan answered nothing");
}

#[test]
fn a_second_result_for_one_call_is_dropped() {
    let duplicate = Message {
        role: Role::User,
        content: vec![result("call_1"), result("call_1")],
        timestamp: None,
        tool_duration_ms: None,
    };
    let out = normalize(&[assistant_call("call_1"), duplicate]);
    assert_eq!(ids_in(&out[1]), vec!["call_1"], "one call, one answer");
}

#[test]
fn a_call_with_no_result_gets_a_stand_in_result() {
    let out = normalize(&[
        user_text("read it"),
        assistant_call("call_1"),
        user_text("still there?"),
    ]);
    assert_eq!(roles(&out), vec![Role::User, Role::Assistant, Role::User]);
    // The stand-in joins the user turn that follows the call, so the call is
    // answered inside the reply the wire expects.
    assert_eq!(ids_in(&out[2]), vec!["call_1"]);
    let body = body_of(&out[2], "call_1").expect("the call is answered");
    assert!(body.contains(TOOL_OUTPUT_MISSING_TEXT), "{body}");
    assert!(body.starts_with("[Error] "), "{body}");
}

#[test]
fn the_stand_in_result_is_marked_an_error() {
    let out = normalize(&[assistant_call("call_1")]);
    let ContentBlock::ToolResult { is_error, verdict, .. } = &out[1].content[0] else {
        panic!("expected a tool result, got {:?}", out[1].content[0]);
    };
    assert_eq!(*is_error, Some(true));
    assert!(verdict.is_none());
}

#[test]
fn a_call_at_the_end_of_the_history_gets_a_trailing_reply() {
    let out = normalize(&[user_text("read it"), assistant_call("call_1")]);
    assert_eq!(roles(&out), vec![Role::User, Role::Assistant, Role::User]);
    assert_eq!(ids_in(&out[2]), vec!["call_1"]);
}

#[test]
fn two_unanswered_calls_are_both_answered_in_call_order() {
    let two_calls = Message {
        role: Role::Assistant,
        content: vec![call("call_1"), call("call_2")],
        timestamp: None,
        tool_duration_ms: None,
    };
    let out = normalize(&[user_text("read both"), two_calls]);
    assert_eq!(ids_in(&out[2]), vec!["call_1", "call_2"]);
}

#[test]
fn a_real_result_comes_after_the_stand_in_it_pairs_with() {
    let two_calls = Message {
        role: Role::Assistant,
        content: vec![call("call_1"), call("call_2")],
        timestamp: None,
        tool_duration_ms: None,
    };
    let out = normalize(&[two_calls, user_result("call_2")]);
    // call_1 has no output, call_2 does; both answers sit in one reply, the
    // synthesized one first because that is the call that was issued first.
    assert_eq!(ids_in(&out[1]), vec!["call_1", "call_2"]);
    assert_eq!(body_of(&out[1], "call_2").as_deref(), Some("done"));
}

#[test]
fn a_result_carried_alongside_prose_keeps_the_prose() {
    let mixed = Message {
        role: Role::User,
        content: vec![result("call_1"), text_block("and also check b.txt")],
        timestamp: None,
        tool_duration_ms: None,
    };
    let out = normalize(&[assistant_call("call_1"), mixed]);
    assert_eq!(out[1].content.len(), 2);
    assert_eq!(ids_in(&out[1]), vec!["call_1"]);
    assert!(matches!(&out[1].content[1], ContentBlock::Text { text, .. } if text.contains("b.txt")));
}

#[test]
fn a_prompt_that_follows_a_result_stays_a_separate_turn() {
    // The result answers the call, and the prompt that came with it belongs in
    // the same turn: two user turns in a row would be rejected.
    let out = normalize(&[
        assistant_call("call_1"),
        user_result("call_1"),
        user_text("next question"),
    ]);
    assert_eq!(roles(&out), vec![Role::Assistant, Role::User]);
    assert_eq!(out[1].content.len(), 2);
}

// ---------------------------------------------------------------------------
// Output invariants

#[test]
fn the_output_never_repeats_a_role() {
    let input = vec![
        user_text("a"),
        user_text("b"),
        assistant_call("call_1"),
        assistant_text("thinking out loud"),
        user_result("call_1"),
        user_result("call_ghost"),
        user_text("c"),
        assistant_call("call_2"),
    ];
    let out = normalize(&input);
    let mut previous: Option<Role> = None;
    for message in &out {
        assert_ne!(
            previous.as_ref(),
            Some(&message.role),
            "role repeated: {:?}",
            roles(&out)
        );
        previous = Some(message.role.clone());
    }
}

#[test]
fn every_call_in_the_output_is_answered() {
    let input = vec![
        assistant_call("call_1"),
        user_result("call_2"),
        assistant_call("call_3"),
        user_text("hi"),
    ];
    let out = normalize(&input);
    let mut calls: Vec<String> = Vec::new();
    let mut answers: Vec<String> = Vec::new();
    for message in &out {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => calls.push(id.clone()),
                ContentBlock::ToolResult { tool_use_id, .. } => answers.push(tool_use_id.clone()),
                _ => {}
            }
        }
    }
    calls.sort();
    answers.sort();
    assert_eq!(calls, answers, "a call or a result is unpaired in {out:?}");
}

#[test]
fn the_input_is_not_modified() {
    let input = vec![user_text("a"), user_text("b")];
    let before = input.iter().map(|message| message.content.len()).collect::<Vec<_>>();
    let _ = normalize(&input);
    let after = input.iter().map(|message| message.content.len()).collect::<Vec<_>>();
    assert_eq!(before, after);
}

#[test]
fn an_empty_history_normalizes_to_an_empty_history() {
    assert!(normalize(&[]).is_empty());
}

#[test]
fn the_turn_shape_is_reportable_for_a_strict_wire() {
    // The shared assertion each wire's own test reuses: alternating roles, and
    // a user turn carrying tool results only where a call precedes it.
    let out = normalize(&[
        user_text("hi"),
        assistant_call("call_1"),
        user_result("call_1"),
        assistant_text("done"),
    ]);
    assert_eq!(
        shape(&out),
        vec![
            (Role::User, false),
            (Role::Assistant, false),
            (Role::User, true),
            (Role::Assistant, false)
        ]
    );
}
