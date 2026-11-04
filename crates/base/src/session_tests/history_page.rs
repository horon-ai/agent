use super::tests::cases::canonical_tempdir;
use super::*;
use anyhow::Result;

fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: value.into(),
        cache_control: None,
    }]
}

fn digest(messages: &[StoredMessage]) -> String {
    let bytes = serde_json::to_vec(messages).expect("messages serialize");
    horon_agent_session_store::sha256_hex(&bytes)
}

fn long_session(id: &str, messages: usize) -> Result<Session> {
    let mut session = Session::create_with_id(id.into(), None, None);
    for index in 0..messages {
        let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
        session.add_message(role, text(&format!("message {index}")));
    }
    session.save()?;
    Ok(session)
}

fn expected_page(history: &[StoredMessage], before: Option<&str>, limit: usize) -> Vec<StoredMessage> {
    let end = match before {
        Some(id) => history
            .iter()
            .position(|message| message.id == id)
            .expect("cursor is in history"),
        None => history.len(),
    };
    history[end.saturating_sub(limit)..end].to_vec()
}

#[test]
fn first_middle_and_last_page_match_full_history() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-history-page-parity-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let session = long_session("page_parity", 240)?;
    let history = session.history_messages()?;
    assert_eq!(history.len(), 240);

    let first = session.history_page(None, 20)?;
    assert_eq!(digest(&first), digest(&expected_page(&history, None, 20)));

    let middle_cursor = history[120].id.clone();
    let middle = session.history_page(Some(&middle_cursor), 20)?;
    assert_eq!(
        digest(&middle),
        digest(&expected_page(&history, Some(&middle_cursor), 20))
    );

    let last_cursor = history[20].id.clone();
    let last = session.history_page(Some(&last_cursor), 20)?;
    assert_eq!(digest(&last), digest(&expected_page(&history, Some(&last_cursor), 20)));

    let wide_cursor = history[200].id.clone();
    let wide = session.history_page(Some(&wide_cursor), 200)?;
    assert_eq!(digest(&wide), digest(&expected_page(&history, Some(&wide_cursor), 200)));

    let reloaded = Session::load(&session.id)?;
    assert_eq!(digest(&reloaded.history_page(None, 20)?), digest(&first));
    assert_eq!(
        digest(&reloaded.history_page(Some(&middle_cursor), 20)?),
        digest(&middle)
    );
    Ok(())
}

#[test]
fn a_page_over_a_compacted_prefix_matches_full_history() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-history-page-compacted-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = Session::create_with_id("page_compacted".into(), None, None);
    session.provider_key = Some("openai".into());
    session.model = Some("test-model".into());
    session.route_api_method = Some("openai-api".into());
    for index in 0..40 {
        let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
        session.add_message(role, text(&format!("message {index}")));
    }
    session.save()?;
    session.compaction = Some(StoredCompactionState {
        summary_text: "first turns summarized".into(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 30,
        original_turn_count: 40,
        compacted_count: 30,
        structured: None,
    });
    session.save()?;

    let loaded = Session::load(&session.id)?;
    let history = loaded.history_messages()?;
    assert_eq!(history.len(), 40);
    assert!(loaded.active_context().len() < history.len(), "the prefix is compacted");
    let first = loaded.history_page(None, 12)?;
    assert_eq!(digest(&first), digest(&expected_page(&history, None, 12)));
    let cursor = history[10].id.clone();
    let early = loaded.history_page(Some(&cursor), 5)?;
    assert_eq!(digest(&early), digest(&expected_page(&history, Some(&cursor), 5)));
    assert_eq!(early.len(), 5);
    Ok(())
}

#[test]
fn a_revised_message_is_paged_as_its_replacement() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-history-page-revision-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = long_session("page_revision", 30)?;
    let history = session.history_messages()?;
    let mut target = history[27].clone();
    target.content = text("revised answer");
    session.revise_message(target.clone())?;
    session.save()?;

    let loaded = Session::load(&session.id)?;
    let revised = loaded.history_messages()?;
    assert_eq!(revised.len(), 30);
    let cursor = history[28].id.clone();
    let page = loaded.history_page(Some(&cursor), 3)?;
    assert_eq!(digest(&page), digest(&expected_page(&revised, Some(&cursor), 3)));
    assert_eq!(page.last().unwrap().content_preview(), "revised answer");
    Ok(())
}

#[test]
fn a_cursor_from_a_replaced_head_is_refused_by_name() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-history-page-rewind-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = long_session("page_rewind", 30)?;
    let history = session.history_messages()?;
    let late = history[25].id.clone();
    assert!(session.history_page(Some(&late), 5).is_ok());

    session.rewind_to_rendered_message(6)?;
    let error = session
        .history_page(Some(&late), 5)
        .expect_err("a cursor past the selected head cannot be served");
    assert!(
        error.to_string().contains("unknown history cursor"),
        "the refusal must name the cursor: {error}"
    );
    let short = session.history_page(None, 50)?;
    assert_eq!(short.len(), session.history_messages()?.len());
    assert!(short.len() < history.len(), "the rewind shortened the visible history");
    Ok(())
}

#[test]
fn an_append_between_two_page_requests_keeps_the_older_page_identical() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-history-page-append-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
    let mut session = long_session("page_append", 60)?;
    let history = session.history_messages()?;
    let cursor = history[40].id.clone();
    let before = session.history_page(Some(&cursor), 10)?;
    assert_eq!(digest(&before), digest(&expected_page(&history, Some(&cursor), 10)));

    session.add_message(Role::Assistant, text("one more answer"));
    session.save()?;
    let after = session.history_page(Some(&cursor), 10)?;
    assert_eq!(digest(&after), digest(&before), "an append must not move an older page");
    let newest = session.history_page(None, 3)?;
    assert_eq!(newest.last().unwrap().content_preview(), "one more answer");

    let reloaded = Session::load(&session.id)?;
    assert_eq!(digest(&reloaded.history_page(Some(&cursor), 10)?), digest(&before));
    Ok(())
}
