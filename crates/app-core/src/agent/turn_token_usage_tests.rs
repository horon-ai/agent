use super::*;
use crate::session::StoredTokenUsage;

fn assistant_message(
    id: &str,
    input_tokens: u64,
    output_tokens: u64,
    prompt_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
) -> crate::session::StoredMessage {
    crate::session::StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        turn_start: false,
        id: id.to_string(),
        role: Role::Assistant,
        content: Vec::new(),
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: Some(StoredTokenUsage {
            prompt_tokens,
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        }),
    }
}

#[test]
fn sums_assistant_usage_only_from_the_turn_start() {
    let messages = vec![
        assistant_message("before", 100, 20, Some(100), Some(30), Some(4)),
        crate::session::StoredMessage {
            original_input: None,
            arg_repairs: None,
            degradations: None,
            turn_start: false,
            id: "user".to_string(),
            role: Role::User,
            content: Vec::new(),
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        },
        assistant_message("first", 10, 2, Some(12), Some(3), None),
        assistant_message("second", 20, 4, Some(25), Some(5), Some(7)),
    ];

    let usage = turn_token_usage_after(&messages, 1).expect("turn usage");
    assert_eq!(usage.prompt_tokens, Some(37));
    assert_eq!(usage.input_tokens, 30);
    assert_eq!(usage.output_tokens, 6);
    assert_eq!(usage.cache_read_input_tokens, Some(8));
    assert_eq!(usage.cache_creation_input_tokens, Some(7));
}

#[test]
fn returns_none_without_assistant_usage() {
    let messages = vec![crate::session::StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        turn_start: false,
        id: "user".to_string(),
        role: Role::User,
        content: Vec::new(),
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }];

    assert_eq!(turn_token_usage_after(&messages, 0), None);
}

#[test]
fn compacted_usage_and_stub_use_global_history_coordinates() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", home.path());
    let mut session = Session::create_with_id("usage_coordinates".into(), None, None);
    session.provider_key = Some("openai".into());
    session.model = Some("test".into());
    session.route_api_method = Some("openai-api".into());
    session.replace_messages(vec![
        assistant_message("old", 100, 20, Some(100), None, None),
        assistant_message("new", 10, 2, Some(12), None, None),
    ]);
    session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "old".into(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        compacted_count: 1,
        structured: None,
        covers_up_to_turn: 1,
        original_turn_count: 2,
    });
    session.save().unwrap();
    assert_eq!(session.active_history_offset(), 1);
    assert_eq!(session_turn_token_usage_after(&session, 1).unwrap().input_tokens, 10);
    let stub = Session::load_startup_stub(&session.id).unwrap();
    assert_eq!(session_turn_token_usage_after(&stub, 1).unwrap().output_tokens, 2);
    match previous {
        Some(value) => crate::env::set_var("HORON_HOME", value),
        None => crate::env::remove_var("HORON_HOME"),
    }
}
