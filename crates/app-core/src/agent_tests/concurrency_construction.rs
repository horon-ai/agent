use super::*;

struct IsolatedHomeEnv {
    _home: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
}

impl IsolatedHomeEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let previous_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        Self {
            _home: home,
            previous_home,
        }
    }
}

impl Drop for IsolatedHomeEnv {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

#[tokio::test]
async fn headless_child_records_its_parent_session() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedHomeEnv::new();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let child = Agent::new_with_parent_and_initial_working_dir(
        provider.clone(),
        registry,
        None,
        Some("coordinator-session".to_owned()),
    );
    assert_eq!(child.session.parent_id.as_deref(), Some("coordinator-session"));
    let registry = Registry::new(provider.clone()).await;
    let root = Agent::new_with_parent_and_initial_working_dir(provider, registry, None, None);
    assert!(root.session.parent_id.is_none());
}

#[tokio::test]
async fn provisional_session_materializes_context_and_marker_exactly_once() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedHomeEnv::new();
    let working_dir = tempfile::tempdir().expect("working dir");
    let working_dir = working_dir.path().to_string_lossy().into_owned();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new_provisional_with_initial_working_dir(provider, registry, Some(&working_dir));
    let session_id = agent.session_id().to_string();

    assert!(
        agent.messages().is_empty(),
        "a provisional placeholder must not build the session context"
    );
    assert!(
        agent.env_snapshot_reasons().is_empty(),
        "a provisional placeholder must not record the environment"
    );
    assert!(
        !crate::session::active_session_ids().contains(&session_id),
        "a provisional placeholder must not claim an active marker"
    );

    assert!(
        agent.materialize_provisional_session(),
        "the first real conversation commits the session"
    );
    assert_eq!(
        agent.env_snapshot_reasons(),
        vec!["create"],
        "confirmation records the environment exactly once"
    );
    assert_eq!(
        agent.messages().len(),
        1,
        "confirmation builds one session-context message"
    );
    assert!(agent.messages()[0].content_preview().contains("# Session Context"));
    assert!(crate::session::active_session_ids().contains(&session_id));

    assert!(
        !agent.materialize_provisional_session(),
        "a repeated Subscribe or Message must not re-run the bootstrap"
    );
    assert_eq!(agent.messages().len(), 1, "the session context must not be duplicated");
    assert_eq!(agent.env_snapshot_reasons(), vec!["create"]);
    agent.mark_closed();
}

#[tokio::test]
async fn cold_restore_from_provisional_records_resume_and_never_bootstraps() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedHomeEnv::new();
    let target_id = "session_cold_restore_from_provisional";
    let mut persisted = crate::session::Session::create_with_id(target_id.to_string(), None, None);
    persisted.replace_messages(vec![stored_user_message("msg-restored", "restored prompt")]);
    persisted.save().expect("persist the target session");

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new_provisional_with_initial_working_dir(provider, registry, None);
    assert!(
        agent.env_snapshot_reasons().is_empty(),
        "precondition: nothing recorded yet"
    );

    agent
        .restore_session_with_working_dir(target_id, None)
        .expect("cold restore from the placeholder");

    assert_eq!(agent.session_id(), target_id);
    assert_eq!(
        agent.env_snapshot_reasons(),
        vec!["resume"],
        "a restored session records its resume snapshot instead of skipping it"
    );
    assert!(
        !agent.materialize_provisional_session(),
        "a restored session must never be bootstrapped as a fresh one"
    );
    assert_eq!(
        agent.messages().len(),
        1,
        "restore must not add a second session context"
    );
    assert_eq!(agent.env_snapshot_reasons(), vec!["resume"]);
    agent.mark_closed();
}

fn stored_user_message(id: &str, text: &str) -> crate::session::StoredMessage {
    crate::session::StoredMessage {
        id: id.to_string(),
        role: crate::message::Role::User,
        content: vec![crate::message::ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
        turn_start: false,
    }
}
