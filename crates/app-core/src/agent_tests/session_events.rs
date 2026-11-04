use super::*;

#[tokio::test]
async fn completion_after_compacted_prefix_uses_global_history_indices() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("HORON_AGENT_HOME");
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    let mut session = Session::create_with_id("completion_coordinates".into(), None, None);
    session.provider_key = Some("openai".into());
    session.model = Some("test".into());
    session.route_api_method = Some("openai-api".into());
    let text = |s: &str| {
        vec![ContentBlock::Text {
            text: s.into(),
            cache_control: None,
        }]
    };
    for _ in 0..13 {
        session.add_message(Role::User, text("question"));
    }
    session.save().unwrap();
    session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "old work".into(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        compacted_count: 12,
        structured: None,
        covers_up_to_turn: 12,
        original_turn_count: 13,
    });
    session.save().unwrap();
    let mut agent = Agent::new_with_session(Arc::new(BoundSavedAccountProvider), Registry::empty(), session, None);
    let start = agent.message_count();
    assert_eq!(start, 13);
    agent.add_message(Role::Assistant, text("actual completion"));
    agent.session.save().unwrap();
    assert_eq!(
        agent.latest_assistant_text_after(start).as_deref(),
        Some("actual completion")
    );
    let stub = Session::load_startup_stub(agent.session_id()).unwrap();
    let stub_agent = Agent::new_with_session(Arc::new(BoundSavedAccountProvider), Registry::empty(), stub, None);
    assert_eq!(
        stub_agent.latest_assistant_text_after(start).as_deref(),
        Some("actual completion")
    );
    agent.session = Session::load(agent.session_id()).unwrap();
    let head = agent.session.selected_event_head().unwrap();
    agent.session.rewind_to_rendered_message(1).unwrap();
    agent.add_message(Role::Assistant, text("new branch"));
    agent.session.save().unwrap();
    assert_eq!(agent.latest_assistant_text_after(1).as_deref(), Some("new branch"));
    agent.session.select_event_head(&head).unwrap();
    assert_eq!(
        agent.latest_assistant_text_after(start).as_deref(),
        Some("actual completion")
    );
    match previous {
        Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
}

#[tokio::test]
async fn renderer_rewind_undo_and_restart_keep_original_events() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
                None => crate::env::remove_var("HORON_AGENT_HOME"),
            }
            crate::config::Config::invalidate_cache();
        }
    }
    let _restore = Restore(std::env::var_os("HORON_AGENT_HOME"));
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    crate::config::Config::invalidate_cache();
    let mut agent = Agent::new(Arc::new(BoundSavedAccountProvider), Registry::empty());
    let text = |value: &str| {
        vec![ContentBlock::Text {
            text: value.into(),
            cache_control: None,
        }]
    };
    agent.add_message(Role::User, text("question one"));
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "rewind-tool".into(),
            name: "read".into(),
            input: serde_json::json!({}),
            thought_signature: None,
        }],
    );
    agent.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "rewind-tool".into(),
            content: "original tool result".into(),
            is_error: None,
        }],
    );
    agent.add_message(Role::Assistant, text("answer one"));
    agent.add_message(Role::User, text("question two"));
    agent.add_message(Role::Assistant, text("answer two"));
    agent.session.save().unwrap();
    let original = std::fs::read(agent.session.event_log_path().unwrap()).unwrap();
    assert_eq!(agent.rewind_to_message(3).unwrap(), 1);
    assert_eq!(agent.last_message_text(), Some("question two"));
    assert_eq!(agent.undo_rewind().unwrap(), 1);
    assert_eq!(agent.last_message_text(), Some("answer two"));
    assert_eq!(agent.rewind_to_message(1).unwrap(), 3);
    assert_eq!(agent.last_message_text(), Some("question one"));
    let restarted = Session::load(agent.session_id()).unwrap();
    assert_eq!(restarted.rewind_target_count(), 1);
    assert!(
        std::fs::read(restarted.event_log_path().unwrap())
            .unwrap()
            .starts_with(&original)
    );
}

/// A live turn must finish even when the ambient runner records a resume
/// marker and the phone reads status while the turn is running.
///
/// This is the harness-level shape of the QA defect: a turn parked on an
/// approval keeps its session facade open, the runner appends a
/// `resume_key` marker through the real writer, the status path reads the
/// session (`Session::load_startup_stub`, what the daemon answers
/// `agent.session.status` with), and the turn then writes its final answer.
///
/// Before the fix the final write returned
/// `SourceConflict: session originals changed since this facade was loaded`
/// and the answer the model had already produced was dropped. The assertions
/// are on that outcome, not on the marker's bookkeeping.
#[tokio::test]
async fn a_live_turn_keeps_its_answer_across_a_resume_marker_and_status_reads() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("HORON_AGENT_HOME");
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    crate::config::Config::invalidate_cache();
    let text = |s: &str| {
        vec![ContentBlock::Text {
            text: s.into(),
            cache_control: None,
        }]
    };

    let mut session = Session::create_with_id("live_turn_marker".into(), None, None);
    session.provider_key = Some("openai".into());
    session.model = Some("test".into());
    session.route_api_method = Some("openai-api".into());
    session.add_message(Role::User, text("do the long job"));
    session.save().unwrap();
    let mut agent = Agent::new_with_session(Arc::new(BoundSavedAccountProvider), Registry::empty(), session, None);
    let start = agent.message_count();

    // The phone polls status while the turn runs. Each read is a fresh facade
    // over the same originals and must leave the live turn's facade alone.
    for _ in 0..3 {
        let stub = Session::load_startup_stub(agent.session_id()).unwrap();
        assert_eq!(stub.id, agent.session.id);
        let _ = Session::load(agent.session_id()).unwrap();
    }

    // The ambient runner records that it created a resume turn for the approval.
    Session::append_resume_key(agent.session_id(), "req_live_turn").unwrap();

    // The turn writes its answer through the facade it began with.
    agent.add_message(Role::Assistant, text("RESULT approved"));
    agent
        .session
        .save()
        .expect("the live turn's own write must not be refused");

    assert_eq!(
        agent.latest_assistant_text_after(start).as_deref(),
        Some("RESULT approved"),
        "the answer the model produced must survive the marker and the status reads"
    );
    let persisted = Session::load(agent.session_id()).unwrap();
    assert_eq!(
        persisted.history_messages().unwrap().last().unwrap().content_preview(),
        "RESULT approved"
    );
    assert!(
        Session::journal_has_resume_key(agent.session_id(), "req_live_turn").unwrap(),
        "the runner's record must survive the turn's write"
    );

    match previous {
        Some(home) => crate::env::set_var("HORON_AGENT_HOME", home),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
    crate::config::Config::invalidate_cache();
}
