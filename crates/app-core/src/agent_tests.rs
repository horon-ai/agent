use super::*;
use crate::agent::environment::EnvSnapshotDetail;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use crate::tool::ToolOutput;
use crate::tool::{ConcurrencyClass, Tool, ToolEffects};
use async_trait::async_trait;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[path = "agent_tests/tool_streaming.rs"]
mod tool_streaming;

#[path = "agent_tests/context_notes.rs"]
mod context_notes;

#[path = "agent_tests/concurrency_construction.rs"]
mod concurrency_construction;

#[path = "agent_tests/mcp_remote_cache.rs"]
mod mcp_remote_cache;

#[path = "agent_tests/mcp_surface.rs"]
mod mcp_surface;

#[path = "agent_tests/session_events.rs"]
mod session_events;

#[path = "agent_tests/request_snapshot_reuse.rs"]
mod request_snapshot_reuse;

struct DelayedProvider {
    open_delay: Duration,
    first_event_delay: Duration,
}

struct NativeAutoCompactionProvider;

/// Point the agent home at a throwaway directory for the test's lifetime.
///
/// Without this, session fixtures land in the operator's real home and collide
/// with whatever an earlier run left there; the store then correctly refuses a
/// canonical log beside a same-id legacy snapshot.
struct IsolatedHome {
    _home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("isolated agent home");
        crate::storage::create_store_dir(&home.path().join("sessions"), false).expect("create sessions dir");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        crate::config::Config::invalidate_cache();
        Self { _home: home, previous }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match &self.previous {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

struct BoundSavedAccountProvider;

#[tokio::test]
async fn user_response_capability_updates_locked_tool_surface() {
    // The tool profile is process-wide: a test that sets `HORON_AGENT_TOOL_PROFILE=core`
    // and holds this lock would otherwise hide `user_response` from the definitions
    // asserted below, so this test takes the same lock and its own home.
    let _guard = crate::storage::lock_test_env();
    let _home = IsolatedHome::new();
    let mut agent = Agent::new(Arc::new(BoundSavedAccountProvider), Registry::empty());
    assert!(!agent.tool_definitions().await.iter().any(|d| d.name == "user_response"));
    assert!(agent.locked_tools.is_some());
    agent.set_desktop_response_capability(true).await;
    assert!(agent.locked_tools.is_none());
    assert!(agent.tool_definitions().await.iter().any(|d| d.name == "user_response"));
    agent.set_desktop_response_capability(true).await;
    assert!(agent.locked_tools.is_some(), "unchanged capability preserves the cache");
    agent.set_desktop_response_capability(false).await;
    assert!(agent.locked_tools.is_none());
    assert!(!agent.tool_definitions().await.iter().any(|d| d.name == "user_response"));
}

#[async_trait]
impl Provider for BoundSavedAccountProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        panic!("metadata test must not send a request")
    }
    fn name(&self) -> &str {
        "OpenAI"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[tokio::test]
async fn saved_account_restore_mismatch_blocks_model_turn() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match &self.0 {
                Some(home) => crate::env::set_var("HORON_HOME", home),
                None => crate::env::remove_var("HORON_HOME"),
            }
            crate::config::Config::invalidate_cache();
        }
    }
    let _restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    crate::config::Config::invalidate_cache();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let mut agent = Agent::new(provider, Registry::empty());
    agent.session.route_api_method = Some("openai-oauth@work".into());
    let error = agent.ensure_session_lease().unwrap_err().to_string();
    assert!(error.contains("Saved account 'work' could not be restored"));
    agent.session.route_api_method = None;
    agent.session.provider_key = Some("openai-oauth@personal".into());
    assert!(
        agent
            .ensure_session_lease()
            .unwrap_err()
            .to_string()
            .contains("personal")
    );
    agent.session.provider_key = Some("openai-oauth".into());
    assert!(agent.ensure_session_lease().is_ok());
}

struct NativeCompactionStreamProvider;

#[derive(Clone, Default)]
struct SignatureSessionProvider {
    requests: Arc<std::sync::Mutex<Vec<Vec<Message>>>>,
}

#[async_trait]
impl Provider for SignatureSessionProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let first = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(messages.to_vec());
            requests.len() == 1
        };
        let mut events = vec![StreamEvent::SessionId("provider-resume-handle".into())];
        if first {
            events.extend([
                StreamEvent::ToolUseStart {
                    id: "signed-call".into(),
                    name: "provider_owned_probe".into(),
                },
                StreamEvent::ToolInputDelta("{}".into()),
                StreamEvent::ToolUseEnd,
                StreamEvent::ToolUseSignature("test-thought-signature".into()),
                StreamEvent::ToolResult {
                    tool_use_id: "signed-call".into(),
                    content: "done".into(),
                    is_error: false,
                },
            ]);
        }
        events.extend([
            StreamEvent::TextDelta("completed".into()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            },
        ]);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "signature-session-test"
    }
    fn handles_tools_internally(&self) -> bool {
        true
    }
    fn supports_compaction(&self) -> bool {
        false
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[tokio::test]
async fn mpsc_preserves_signatures_and_never_rebinds_to_provider_session_id() {
    let _lock = crate::storage::lock_test_env();
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            if let Some(home) = &self.0 {
                crate::env::set_var("HORON_HOME", home);
            } else {
                crate::env::remove_var("HORON_HOME");
            }
            crate::config::Config::invalidate_cache();
        }
    }
    let home = tempfile::tempdir().unwrap();
    let _restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    crate::config::Config::invalidate_cache();
    let provider = Arc::new(SignatureSessionProvider::default());
    let mut agent = Agent::new(provider.clone(), Registry::empty());
    let horon_agent_id = agent.session_id().to_string();
    for prompt in ["first turn", "second turn"] {
        agent.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: prompt.into(),
                cache_control: None,
            }],
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        agent.run_turn_streaming_mpsc(tx).await.unwrap();
        while let Ok(event) = rx.try_recv() {
            if let ServerEvent::SessionId { session_id } = event {
                assert_eq!(
                    session_id, horon_agent_id,
                    "provider handle must not replace horon-agent identity"
                );
            }
        }
    }
    assert_eq!(agent.session_id(), horon_agent_id);
    let saved = Session::load(&horon_agent_id).unwrap();
    assert_eq!(saved.provider_session_id.as_deref(), Some("provider-resume-handle"));
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].iter().flat_map(|message| &message.content).any(|block| matches!(
        block, ContentBlock::ToolUse { thought_signature: Some(signature), .. } if signature == "test-thought-signature"
    )), "second request must replay the persisted signature");
    let saved_json = serde_json::to_value(&saved).unwrap();
    assert!(saved_json.to_string().contains("test-thought-signature"));
}

#[derive(Clone)]
struct ExplicitPinProvider {
    model: Arc<std::sync::Mutex<String>>,
    pin: Arc<std::sync::Mutex<Option<String>>>,
    set_model_requests: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ExplicitPinProvider {
    fn new(model: &str) -> Self {
        Self {
            model: Arc::new(std::sync::Mutex::new(model.to_string())),
            pin: Arc::new(std::sync::Mutex::new(None)),
            set_model_requests: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Provider for ExplicitPinProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        unreachable!("ExplicitPinProvider does not complete requests")
    }

    fn name(&self) -> &str {
        "openrouter"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn set_model(&self, request: &str) -> Result<()> {
        self.set_model_requests.lock().unwrap().push(request.to_string());
        let spec = request.strip_prefix("openrouter:").unwrap_or(request);
        let (model, pin) = spec
            .rsplit_once('@')
            .map(|(model, pin)| (model, Some(pin.to_string())))
            .unwrap_or((spec, None));
        *self.model.lock().unwrap() = model.to_string();
        *self.pin.lock().unwrap() = pin;
        Ok(())
    }

    fn explicit_provider_pin_for_current_model(&self) -> Option<String> {
        self.pin.lock().unwrap().clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn content_text(content: &[ContentBlock]) -> &str {
    match content.first() {
        Some(ContentBlock::Text { text, .. }) => text,
        _ => "",
    }
}

fn message_text(message: &Message) -> &str {
    content_text(&message.content)
}

#[test]
fn agent_drop_removes_its_configured_session_tool_policy() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let session = Session::create(None, None);
    let session_id = session.id.clone();
    let agent = Agent::new_with_session(
        provider,
        Registry::empty(),
        session,
        Some(HashSet::from(["bash".to_string()])),
    );

    assert_eq!(
        crate::tool::session_tool_policy_allows_tool_for_test(&session_id, "bash"),
        Some(true)
    );
    drop(agent);
    assert_eq!(
        crate::tool::session_tool_policy_allows_tool_for_test(&session_id, "bash"),
        None,
        "dropping the Agent must remove its global policy entry"
    );
}

#[test]
fn stale_agent_drop_preserves_successor_session_tool_policy() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let first_session = Session::create(None, None);
    let session_id = first_session.id.clone();
    let first = Agent::new_with_session(
        provider.clone(),
        Registry::empty(),
        first_session,
        Some(HashSet::from(["bash".to_string()])),
    );
    let mut successor_session = Session::create(None, None);
    successor_session.id.clone_from(&session_id);
    let successor = Agent::new_with_session(
        provider,
        Registry::empty(),
        successor_session,
        Some(HashSet::from(["read".to_string()])),
    );

    drop(first);

    assert_eq!(
        crate::tool::session_tool_policy_allows_tool_for_test(&session_id, "read"),
        Some(true),
        "a stale Agent must not remove its active successor's policy"
    );
    assert_eq!(
        crate::tool::session_tool_policy_allows_tool_for_test(&session_id, "bash"),
        Some(false),
        "the surviving entry must be the successor's configured policy"
    );
    drop(successor);
    assert_eq!(
        crate::tool::session_tool_policy_allows_tool_for_test(&session_id, "read"),
        None
    );
}

#[test]
fn agent_clear_keeps_tool_policy_registration_on_same_session() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let session = Session::create(None, None);
    let previous_session_id = session.id.clone();
    let mut agent = Agent::new_with_session(
        provider,
        Registry::empty(),
        session,
        Some(HashSet::from(["bash".to_string()])),
    );

    agent.clear();
    let new_session_id = agent.session.id.clone();

    assert_eq!(previous_session_id, new_session_id);
    assert_eq!(
        crate::tool::session_tool_policy_allows_tool_for_test(&previous_session_id, "bash"),
        Some(true),
        "resetting a branch must keep the live ID's policy"
    );
    assert_eq!(
        crate::tool::session_tool_policy_allows_tool_for_test(&new_session_id, "bash"),
        Some(true),
        "the new session must retain the Agent's configured policy"
    );
    drop(agent);
    assert_eq!(
        crate::tool::session_tool_policy_allows_tool_for_test(&new_session_id, "bash"),
        None
    );
}

#[test]
fn agent_clear_drops_the_secret_placeholders_of_the_old_branch() {
    // The session vault registry is process-wide, so a test that registers or
    // clears a vault under the colliding session id while this one runs would
    // make the post-clear restore read a foreign map.
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let session = Session::create(None, None);
    let mut agent = Agent::new_with_session(provider, Registry::empty(), session, None);
    let session_id = agent.session.id.clone();
    let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";
    let masked = crate::secrets::mask_for_history(
        &format!("token {secret}"),
        &session_id,
        crate::config::SecretsMode::Obfuscate,
    );
    assert!(!masked.contains(secret));
    assert!(
        crate::secrets::session_vault_with(&session_id, |vault| crate::secrets::restore(&masked, vault))
            .is_some_and(|restored| restored.contains(secret)),
        "the live vault restores its own placeholders"
    );

    agent.clear();

    assert_eq!(
        crate::secrets::session_vault_with(&session_id, |vault| crate::secrets::restore(&masked, vault)),
        Some(masked.clone()),
        "a cleared branch leaves no placeholder behind to restore"
    );
    drop(agent);
    assert!(
        crate::secrets::session_vault_with(&session_id, |vault| vault.len()).is_none(),
        "the vault goes away with its Agent"
    );
}

#[async_trait]
impl Provider for DelayedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        tokio::time::sleep(self.open_delay).await;

        let first_event_delay = self.first_event_delay;
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            tokio::time::sleep(first_event_delay).await;
            let _ = tx.send(Ok(StreamEvent::TextDelta("hello".to_string()))).await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "delayed"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            open_delay: self.open_delay,
            first_event_delay: self.first_event_delay,
        })
    }
}

#[async_trait]
impl Provider for NativeAutoCompactionProvider {
    fn active_resolved_credential(&self) -> Option<horon_agent_provider_core::ResolvedCredential> {
        // Scripted runtime identity, never a real credential or provider request.
        Some(horon_agent_provider_core::ResolvedCredential::ApiKey)
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (_tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(1);
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn uses_horon_agent_compaction(&self) -> bool {
        false
    }

    fn context_window(&self) -> usize {
        // Match a real [OI] provider window so fixtures exercise realistic
        // compaction budgets instead of an artificial overflow.
        200_000
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        // The compaction summarizer parses a structured JSON artifact; the
        // fixture marker lives in the rendered goal.
        Ok(r#"{"goal":"manual summary from native-auto provider","next_action":"continue"}"#.to_string())
    }

    async fn complete_simple_with_usage(
        &self,
        prompt: &str,
        system: &str,
    ) -> Result<(String, horon_agent_provider_core::SimpleCompletionUsage)> {
        Ok((
            self.complete_simple(prompt, system).await?,
            horon_agent_provider_core::SimpleCompletionUsage::default(),
        ))
    }
}

#[async_trait]
impl Provider for NativeCompactionStreamProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            // Response usage is deliberately far below the provider-reported
            // pre-compaction size so a regression that relabels usage as
            // `pre_tokens` is caught (#1178).
            let _ = tx
                .send(Ok(StreamEvent::TokenUsage {
                    input_tokens: Some(24_000),
                    output_tokens: Some(10),
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                }))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::Compaction {
                    trigger: "openai_native".to_string(),
                    pre_tokens: Some(80_000),
                    openai_encrypted_content: Some("enc_native_test".to_string()),
                }))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn uses_horon_agent_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[test]
fn tool_output_to_content_blocks_preserves_labeled_images() {
    let output = ToolOutput::new("Image ready").with_labeled_image("image/png", "ZmFrZQ==", "screenshots/example.png");

    let blocks = tool_output_to_content_blocks("call_1".to_string(), output);
    assert_eq!(blocks.len(), 3);

    match &blocks[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            assert_eq!(tool_use_id, "call_1");
            assert_eq!(content, "Image ready");
            assert_eq!(*is_error, None);
        }
        other => panic!("expected tool result, got {other:?}"),
    }

    match &blocks[1] {
        ContentBlock::Image { media_type, data } => {
            assert_eq!(media_type, "image/png");
            assert_eq!(data, "ZmFrZQ==");
        }
        other => panic!("expected image block, got {other:?}"),
    }

    match &blocks[2] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("screenshots/example.png"));
            assert!(text.contains("preceding tool result"));
        }
        other => panic!("expected trailing label text, got {other:?}"),
    }
}

#[tokio::test]
async fn queued_soft_interrupt_images_are_injected_as_image_blocks() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let _guard = crate::storage::lock_test_env();
    let mut agent = Agent::new(provider, registry);

    agent.queue_soft_interrupt(
        "look at this".to_string(),
        vec![("image/png".to_string(), "ZmFrZQ==".to_string())],
        false,
        SoftInterruptSource::User,
    );
    let injected = agent.inject_soft_interrupts(None).await;

    assert_eq!(injected.len(), 1);
    let message = agent
        .session
        .active_context()
        .last()
        .expect("soft interrupt should append a user message");
    assert!(matches!(
        &message.content[0],
        ContentBlock::Image { media_type, data }
            if media_type == "image/png" && data == "ZmFrZQ=="
    ));
    assert!(matches!(
        &message.content[1],
        ContentBlock::Text { text, .. } if text == "look at this"
    ));
}

#[tokio::test]
async fn injected_soft_interrupts_report_client_ids_per_group_in_queue_order() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let _guard = crate::storage::lock_test_env();
    let mut agent = Agent::new(provider, registry);

    let user = SoftInterruptSource::User;
    agent.queue_soft_interrupt_with_id("one".into(), vec![], false, user, Some("f1".into()));
    agent.queue_soft_interrupt_with_id("anon".into(), vec![], false, user, None);
    agent.queue_soft_interrupt_with_id("two".into(), vec![], false, user, Some("f2".into()));
    agent.queue_soft_interrupt(
        "[Background Task Completed]".into(),
        vec![],
        false,
        SoftInterruptSource::BackgroundTask,
    );
    agent.queue_soft_interrupt_with_id("three".into(), vec![], false, user, Some("f3".into()));

    let injected = agent.inject_soft_interrupts(None).await;
    let events = Agent::build_soft_interrupt_events(injected, "D", None);
    let summary: Vec<(String, Vec<String>)> = events
        .into_iter()
        .map(|event| match event {
            ServerEvent::SoftInterruptInjected {
                content, client_ids, ..
            } => (content, client_ids),
            other => panic!("unexpected event {other:?}"),
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("one\n\nanon\n\ntwo".to_string(), vec!["f1".into(), "f2".into()]),
            ("[Background Task Completed]".to_string(), vec![]),
            ("three".to_string(), vec!["f3".into()]),
        ]
    );
}

#[tokio::test]
async fn run_turn_streaming_mpsc_emits_keepalive_while_provider_is_quiet() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(DelayedProvider {
        open_delay: Duration::from_secs(2),
        first_event_delay: Duration::from_secs(2),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "test".to_string(),
            cache_control: None,
        }],
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(tx).await });

    let mut saw_keepalive = false;
    let keepalive_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < keepalive_deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(ServerEvent::Pong { id, .. })) => {
                assert_eq!(id, STREAM_KEEPALIVE_PONG_ID);
                saw_keepalive = true;
                break;
            }
            Ok(Some(ServerEvent::TextDelta { text })) => {
                panic!("expected keepalive before text delta, got: {text}");
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before keepalive"),
            Err(_) => {
                assert!(!task.is_finished(), "streaming task finished before keepalive arrived");
            }
        }
    }
    assert!(saw_keepalive, "expected keepalive before provider response");

    let mut saw_text = false;
    let text_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < text_deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(ServerEvent::TextDelta { text })) => {
                assert_eq!(text, "hello");
                saw_text = true;
                break;
            }
            Ok(Some(ServerEvent::Pong { id, .. })) => {
                assert_eq!(id, STREAM_KEEPALIVE_PONG_ID);
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before text delta"),
            Err(_) => {
                assert!(!task.is_finished(), "streaming task finished before text delta arrived");
            }
        }
    }

    assert!(saw_text, "expected delayed provider text after keepalive");
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn run_turn_streaming_mpsc_emits_native_compaction_for_client_cache_reset() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeCompactionStreamProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "compact this".to_string(),
            cache_control: None,
        }],
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(tx).await.unwrap();

    let mut saw_native_compaction = false;
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::Compaction {
            trigger,
            pre_tokens,
            messages_compacted,
            ..
        } = event
        {
            assert_eq!(trigger, "openai_native");
            assert_eq!(
                pre_tokens,
                Some(80_000),
                "remote compaction must forward the provider's pre-compaction count"
            );
            assert!(
                messages_compacted.is_some_and(|count| count > 0),
                "native compaction should report a non-empty compacted prefix"
            );
            saw_native_compaction = true;
        }
    }
    assert!(
        saw_native_compaction,
        "native provider compaction must reach clients so they clear KV baselines"
    );
}

/// Provider that transparently switches its model mid-stream, mimicking the
/// Anthropic retired-model fallback (`claude-fable-5` -> `claude-opus-4-8`).
struct MidStreamModelSwitchProvider {
    model: std::sync::Mutex<String>,
    switch_to: String,
}

#[async_trait]
impl Provider for MidStreamModelSwitchProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        // Emulate the provider switching its own model state during the request.
        *self.model.lock().unwrap() = self.switch_to.clone();
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            let _ = tx.send(Ok(StreamEvent::TextDelta("hello".to_string()))).await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: std::sync::Mutex::new(self.model.lock().unwrap().clone()),
            switch_to: self.switch_to.clone(),
        })
    }
}

#[tokio::test]
async fn run_turn_streaming_mpsc_emits_model_changed_on_midstream_switch() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(MidStreamModelSwitchProvider {
        model: std::sync::Mutex::new("claude-fable-5".to_string()),
        switch_to: "claude-opus-4-8".to_string(),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "test".to_string(),
            cache_control: None,
        }],
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(tx).await });

    let mut switched_model = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(ServerEvent::ModelChanged { model, error, .. })) => {
                assert!(error.is_none(), "unexpected model-change error: {error:?}");
                switched_model = Some(model);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                if task.is_finished() {
                    break;
                }
            }
        }
    }

    task.await.unwrap().unwrap();
    assert_eq!(
        switched_model.as_deref(),
        Some("claude-opus-4-8"),
        "expected a ModelChanged event resyncing to the served model"
    );
}

#[tokio::test]
async fn messages_for_provider_replays_persisted_native_compaction_in_auto_mode() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let _env_lock = crate::storage::lock_test_env();
    let mut agent = Agent::new(provider, registry);

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    );
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
    );

    agent
        .apply_openai_native_compaction_window(native_window("enc_auto"), 1)
        .expect("persist native compaction");

    let (messages, event) = agent.messages_for_provider();
    assert!(event.is_none());
    assert!(!messages.is_empty());
    match &messages[0].content[0] {
        ContentBlock::OpenAICompaction { encrypted_content, .. } => {
            assert_eq!(encrypted_content, "enc_auto");
        }
        other => panic!("expected OpenAI compaction block, got {other:?}"),
    }
    assert!(messages.iter().any(|message| message.role == Role::Assistant));
}

/// Provider whose model can be switched under the agent, to emulate a route
/// change between the request that produced a native window and the next one.
struct SwitchableRouteProvider {
    model: std::sync::Mutex<String>,
}

impl SwitchableRouteProvider {
    fn new(model: &str) -> Self {
        Self {
            model: std::sync::Mutex::new(model.to_string()),
        }
    }

    fn switch_model(&self, model: &str) {
        *self.model.lock().unwrap() = model.to_string();
    }
}

#[async_trait]
impl Provider for SwitchableRouteProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (_tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(1);
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn uses_horon_agent_compaction(&self) -> bool {
        false
    }

    fn context_window(&self) -> usize {
        200_000
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self::new(&self.model()))
    }
}

/// Provider that emits a compaction item and then ends the stream without the
/// message-end marker, which is what a mid-response failure looks like.
struct MidStreamCompactionProvider;

#[async_trait]
impl Provider for MidStreamCompactionProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(StreamEvent::Compaction {
                    trigger: "openai_native_auto".to_string(),
                    pre_tokens: Some(150_000),
                    openai_encrypted_content: Some("enc_mid_stream".to_string()),
                }))
                .await;
            // No MessageEnd: the response never completed.
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn uses_horon_agent_compaction(&self) -> bool {
        false
    }

    fn context_window(&self) -> usize {
        200_000
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

/// One-item native window, as the auto path emits it.
fn native_window(encrypted_content: &str) -> Vec<serde_json::Value> {
    vec![serde_json::json!({
        "type": "compaction",
        "encrypted_content": encrypted_content,
    })]
}

/// A retained window: the server keeps a message and a reasoning item
/// alongside the compaction item, in that order.
fn retained_native_window() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "retained answer" }],
        }),
        serde_json::json!({
            "type": "compaction",
            "id": "cmp_retained",
            "encrypted_content": "enc_retained",
        }),
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_retained",
            "summary": [],
            "encrypted_content": "enc_reasoning_retained",
        }),
    ]
}

fn native_block_of(messages: &[Message]) -> Option<&ContentBlock> {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .find(|block| matches!(block, ContentBlock::OpenAICompaction { .. }))
}

#[tokio::test]
async fn native_compaction_window_survives_save_resume_and_retransmit() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let _guard = crate::storage::lock_test_env();
    let mut agent = Agent::new(provider.clone(), registry.clone());

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "originals before compaction".to_string(),
            cache_control: None,
        }],
    );
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "reply before compaction".to_string(),
            cache_control: None,
        }],
    );

    let window = retained_native_window();
    let expected_json: Vec<String> = window.iter().map(|item| item.to_string()).collect();
    // The boundary must leave the latest user intent active, so only the first
    // message is covered.
    agent
        .apply_openai_native_compaction_window(window.clone(), 1)
        .expect("persist the retained window");

    let state = agent
        .session
        .compaction
        .as_ref()
        .expect("native compaction must be persisted");
    assert_eq!(
        state.native_window, window,
        "the whole ordered window is stored, not just encrypted_content"
    );
    assert_eq!(state.native_encrypted_content(), Some("enc_retained"));

    // Retransmit before the restart: every item, in order, byte for byte.
    let (messages, _) = agent.messages_for_provider();
    let Some(ContentBlock::OpenAICompaction { window, .. }) = native_block_of(&messages) else {
        panic!("expected the native window in the provider request");
    };
    assert_eq!(
        window.iter().map(|item| item.to_string()).collect::<Vec<_>>(),
        expected_json
    );

    // Restart: persist, reload from the event store, and re-send.
    let session_id = agent.session_id().to_string();
    agent.session.save().expect("session saves");
    let resumed = Session::load(&session_id).expect("session reloads");
    let resumed_state = resumed
        .compaction
        .as_ref()
        .expect("resume restores the compaction state");
    assert_eq!(
        resumed_state.native_window,
        window.clone(),
        "the window survives save/resume byte for byte and in order"
    );

    let mut resumed_agent = Agent::new_with_session(provider, registry, resumed, None);
    let (resumed_messages, _) = resumed_agent.messages_for_provider();
    let Some(ContentBlock::OpenAICompaction { window, .. }) = native_block_of(&resumed_messages) else {
        panic!("expected the restored native window to be retransmitted");
    };
    assert_eq!(
        window.iter().map(|item| item.to_string()).collect::<Vec<_>>(),
        expected_json,
        "retransmit after resume preserves order and bytes"
    );
    let joined = resumed_messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !joined.contains("originals before compaction"),
        "the compacted prefix must stay compacted after resume"
    );
}

#[tokio::test]
async fn native_compaction_window_without_compaction_item_keeps_the_boundary() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let _guard = crate::storage::lock_test_env();
    let mut agent = Agent::new(provider, registry);

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "originals".to_string(),
            cache_control: None,
        }],
    );

    // An empty or invalid native response is not a compaction.
    agent
        .apply_openai_native_compaction_window(Vec::new(), 1)
        .expect("an empty window is reported without failing the turn");
    assert!(
        agent.session.compaction.is_none(),
        "an empty window must not persist a boundary"
    );

    agent
        .apply_openai_native_compaction_window(
            vec![serde_json::json!({"type": "message", "role": "assistant", "content": []})],
            1,
        )
        .expect("a window without a compaction item is reported without failing the turn");
    assert!(
        agent.session.compaction.is_none(),
        "a window without a compaction item must not persist a boundary"
    );

    let (messages, _) = agent.messages_for_provider();
    assert!(native_block_of(&messages).is_none());
    let joined = messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("originals"));
}

#[tokio::test]
async fn native_compaction_window_from_another_route_restores_the_originals() {
    let provider = Arc::new(SwitchableRouteProvider::new("gpt-route-a"));
    let provider_handle: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(provider_handle.clone()).await;
    let _guard = crate::storage::lock_test_env();
    let mut agent = Agent::new(provider_handle, registry);

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "originals before compaction".to_string(),
            cache_control: None,
        }],
    );
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "reply before compaction".to_string(),
            cache_control: None,
        }],
    );

    agent
        .apply_openai_native_compaction_window(retained_native_window(), 2)
        .expect("persist the retained window");
    let bound = agent
        .session
        .compaction
        .as_ref()
        .and_then(|state| state.native_route.clone())
        .expect("the window is bound to its producing route");
    assert_eq!(bound.model, "gpt-route-a");

    // The model changes under the session: the old window is not replayable.
    provider.switch_model("gpt-route-b");

    let (messages, _) = agent.messages_for_provider();
    assert!(
        native_block_of(&messages).is_none(),
        "a window produced on another model must not be replayed"
    );
    let joined = messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("originals before compaction"),
        "a foreign window restores the originals so they can be re-summarized: {joined}"
    );
    assert!(
        agent
            .session
            .compaction
            .as_ref()
            .is_none_or(|state| state.native_window.is_empty()),
        "the foreign window must be dropped from the persisted state"
    );
}

#[tokio::test]
async fn mid_stream_native_compaction_item_keeps_the_boundary() {
    let provider: Arc<dyn Provider> = Arc::new(MidStreamCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let _guard = crate::storage::lock_test_env();
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "compact me mid-stream".to_string(),
            cache_control: None,
        }],
    );

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(tx).await.unwrap();

    assert!(
        agent.session.compaction.is_none(),
        "a compaction item from an unfinished response must not advance the boundary"
    );
}

#[tokio::test]
async fn oversized_openai_native_window_does_not_advance_the_compacted_boundary() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let _guard = crate::storage::lock_test_env();
    let mut agent = Agent::new(provider, registry);

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    );
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
    );

    let oversized = "x".repeat(crate::provider::openai_request::OPENAI_ENCRYPTED_CONTENT_SAFE_MAX_CHARS + 1);
    agent
        .apply_openai_native_compaction_window(native_window(&oversized), 1)
        .expect("an unreplayable window is reported without failing the turn");

    // D9: an untransmittable native must not advance the compacted count. The
    // originals stay active so a structured summary can be rebuilt from them.
    assert!(
        agent.session.compaction.is_none(),
        "an oversized native window must not persist a compacted boundary"
    );
    let (messages, event) = agent.messages_for_provider();
    assert!(event.is_none());
    assert!(
        messages.iter().all(|message| message
            .content
            .iter()
            .all(|block| !matches!(block, ContentBlock::OpenAICompaction { .. }))),
        "an oversized native window must never be replayed"
    );
    let joined = messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("first"), "originals must stay active: {joined}");
    assert!(joined.contains("second"), "originals must stay active: {joined}");
}

#[tokio::test]
async fn messages_for_provider_applies_manual_compaction_in_native_auto_mode() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    // A 200k-token window keeps a 20k-token verbatim recent tail, so the
    // fixture needs enough history for a compactable prefix to remain.
    for i in 0..100 {
        agent.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("turn {i} {}", "x".repeat(1_600)),
                cache_control: None,
            }],
        );
    }

    agent.provider_session_id = Some("stale-provider-session".to_string());
    agent.session.provider_session_id = Some("stale-provider-session".to_string());

    let provider_messages = agent.provider_messages();
    let (message, success) = agent.request_manual_compaction();
    assert!(success, "manual compaction should start: {message}");

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut event = None;
    let mut compacted_messages: Arc<[Message]> = Arc::from(Vec::new());
    while Instant::now() < deadline {
        let (messages, maybe_event) = agent.messages_for_provider();
        if maybe_event.is_some() {
            event = maybe_event;
            compacted_messages = messages;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let event = event.expect("manual compaction event should be applied");
    assert_eq!(event.trigger, "manual");
    assert!(agent.session.compaction.is_some());
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    assert!(compacted_messages.len() < provider_messages.len());
    match &compacted_messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
            assert!(text.contains("manual summary from native-auto provider"));
        }
        other => panic!("expected text summary block, got {other:?}"),
    }
}

// ── InterruptSignal tests ────────────────────────────────────────────────

#[tokio::test]
async fn interrupt_signal_fire_before_notified_does_not_hang() {
    // Regression test: fire() called BEFORE notified().await must not hang.
    // The old code called notify_waiters() which drops the notification if
    // nobody is waiting yet. The flag is still set so the fast path catches it,
    // but only if the future is created before the flag check.
    let sig = InterruptSignal::new();
    sig.fire(); // fire before anyone is waiting
    tokio::time::timeout(std::time::Duration::from_millis(100), sig.notified())
        .await
        .expect("notified() hung when signal was already set before call");
}

#[tokio::test]
async fn interrupt_signal_fire_concurrent_with_notified() {
    // Regression test for the race window: fire() is called concurrently while
    // notified() is being set up. The fix (create future before flag check) ensures
    // the notify_waiters() in fire() wakes the registered future.
    let sig = Arc::new(InterruptSignal::new());
    let sig2 = Arc::clone(&sig);

    // Spawn a task that fires after a tiny delay, giving the main task time to
    // enter notified() but before it reaches notified().await.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        sig2.fire();
    });

    tokio::time::timeout(std::time::Duration::from_millis(500), sig.notified())
        .await
        .expect("notified() hung during concurrent fire()");
}

#[tokio::test]
async fn interrupt_signal_is_set_false_initially() {
    let sig = InterruptSignal::new();
    assert!(!sig.is_set());
}

#[tokio::test]
async fn interrupt_signal_is_set_true_after_fire() {
    let sig = InterruptSignal::new();
    sig.fire();
    assert!(sig.is_set());
}

#[tokio::test]
async fn interrupt_signal_reset_clears_flag() {
    let sig = InterruptSignal::new();
    sig.fire();
    assert!(sig.is_set());
    sig.reset();
    assert!(!sig.is_set());
}

#[tokio::test]
async fn interrupt_signal_notified_completes_after_fire() {
    let sig = Arc::new(InterruptSignal::new());
    let sig2 = Arc::clone(&sig);

    let handle = tokio::spawn(async move {
        sig2.notified().await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    sig.fire();

    tokio::time::timeout(std::time::Duration::from_millis(200), handle)
        .await
        .expect("notified() task timed out after fire()")
        .expect("task panicked");
}

#[tokio::test]
async fn new_agent_registers_active_pid_and_clear_keeps_it() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let first_session_id = agent.session_id().to_string();
    assert!(
        crate::session::active_session_ids().contains(&first_session_id),
        "fresh agent session should be tracked as active"
    );

    agent.clear();

    let second_session_id = agent.session_id().to_string();
    let active = crate::session::active_session_ids();
    assert_eq!(first_session_id, second_session_id);
    assert!(
        active.contains(&second_session_id),
        "replacement session should be tracked as active"
    );
    assert!(
        active.contains(&first_session_id),
        "clearing a branch keeps the same locally owned session active"
    );
}

#[tokio::test]
async fn horon_agent_docs_is_exposed_by_default_and_can_be_explicitly_disabled() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("HORON_HOME");
    let prev_tools = std::env::var_os("HORON_AGENT_TOOLS");
    let prev_disabled_tools = std::env::var_os("HORON_AGENT_DISABLED_TOOLS");
    let prev_tool_profile = std::env::var_os("HORON_AGENT_TOOL_PROFILE");
    let prev_disable_base_tools = std::env::var_os("HORON_AGENT_DISABLE_BASE_TOOLS");
    let temp_home = tempfile::TempDir::new().expect("temp home");

    crate::env::set_var("HORON_HOME", temp_home.path());
    crate::env::remove_var("HORON_AGENT_TOOLS");
    crate::env::remove_var("HORON_AGENT_DISABLED_TOOLS");
    crate::env::remove_var("HORON_AGENT_TOOL_PROFILE");
    crate::env::remove_var("HORON_AGENT_DISABLE_BASE_TOOLS");
    crate::config::Config::invalidate_cache();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let definitions = agent.tool_definitions().await;
    let tool_names = agent.tool_names().await;
    let tool_name = "horon_agent_docs";

    assert!(
        definitions.iter().any(|definition| definition.name == tool_name),
        "{tool_name} must be sent in model-visible tool definitions in every session"
    );
    assert!(
        tool_names.iter().any(|name| name == tool_name),
        "{tool_name} must be listed as model-visible in every session"
    );
    agent
        .validate_tool_allowed(tool_name)
        .expect("horon_agent_docs must be executable in every session");

    crate::env::set_var("HORON_AGENT_DISABLED_TOOLS", tool_name);
    crate::config::Config::invalidate_cache();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let definitions = agent.tool_definitions().await;
    let tool_names = agent.tool_names().await;

    assert!(
        !definitions.iter().any(|definition| definition.name == tool_name),
        "explicitly disabled {tool_name} must not be sent in model-visible tool definitions"
    );
    assert!(
        !tool_names.iter().any(|name| name == tool_name),
        "explicitly disabled {tool_name} must not be listed as model-visible"
    );
    let err = agent
        .validate_tool_allowed(tool_name)
        .expect_err("explicitly disabled horon_agent_docs must not be executable");
    assert!(err.to_string().contains("disabled"));

    if let Some(previous) = prev_home {
        crate::env::set_var("HORON_HOME", previous);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
    if let Some(previous) = prev_tools {
        crate::env::set_var("HORON_AGENT_TOOLS", previous);
    } else {
        crate::env::remove_var("HORON_AGENT_TOOLS");
    }
    if let Some(previous) = prev_disabled_tools {
        crate::env::set_var("HORON_AGENT_DISABLED_TOOLS", previous);
    } else {
        crate::env::remove_var("HORON_AGENT_DISABLED_TOOLS");
    }
    if let Some(previous) = prev_tool_profile {
        crate::env::set_var("HORON_AGENT_TOOL_PROFILE", previous);
    } else {
        crate::env::remove_var("HORON_AGENT_TOOL_PROFILE");
    }
    if let Some(previous) = prev_disable_base_tools {
        crate::env::set_var("HORON_AGENT_DISABLE_BASE_TOOLS", previous);
    } else {
        crate::env::remove_var("HORON_AGENT_DISABLE_BASE_TOOLS");
    }
    crate::config::Config::invalidate_cache();
}

fn seed_transient_session_state(agent: &mut Agent) {
    agent.push_alert("pending alert".to_string());
    agent.queue_soft_interrupt(
        "queued interrupt".to_string(),
        Vec::new(),
        true,
        SoftInterruptSource::User,
    );
    agent.background_tool_signal.fire();
    agent.request_graceful_shutdown();
    agent.tool_call_ids.insert("tool_call_old".to_string());
    agent.tool_result_ids.insert("tool_result_old".to_string());
    agent.tool_output_scan_index = 7;
    agent.last_upstream_provider = Some("upstream_old".to_string());
    agent.last_connection_type = Some("websocket".to_string());
    agent.current_turn_system_reminder = Some("reminder".to_string());
    agent.last_usage = TokenUsage {
        input_tokens: 11,
        output_tokens: 17,
        cache_read_input_tokens: Some(3),
        cache_creation_input_tokens: Some(5),
    };
    let frozen = ToolDefinition::new("test_tool", "test tool", serde_json::json!({"type": "object"}));
    agent.locked_tools = Some(vec![frozen].into());
}

#[tokio::test]
async fn clear_resets_runtime_interrupt_and_queue_state() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    seed_transient_session_state(&mut agent);
    assert_eq!(agent.soft_interrupt_count(), 1);
    assert!(agent.background_tool_signal().is_set());
    assert!(agent.graceful_shutdown_signal().is_set());

    agent.clear();

    assert_eq!(agent.soft_interrupt_count(), 0);
    assert!(!agent.background_tool_signal().is_set());
    assert!(!agent.graceful_shutdown_signal().is_set());
    assert_eq!(agent.pending_alert_count(), 0);
    assert!(agent.tool_call_ids.is_empty());
    assert!(agent.tool_result_ids.is_empty());
    assert_eq!(agent.tool_output_scan_index, 0);
    assert!(agent.last_upstream_provider.is_none());
    assert!(agent.last_connection_type.is_none());
    assert!(agent.current_turn_system_reminder.is_none());
    assert_eq!(agent.last_usage.input_tokens, 0);
    assert_eq!(agent.last_usage.output_tokens, 0);
    assert!(agent.locked_tools.is_none());
}

#[tokio::test]
async fn restore_session_resets_runtime_interrupt_and_queue_state() {
    let _guard = crate::storage::lock_test_env();
    let _home = IsolatedHome::new();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let mut restored_session =
        crate::session::Session::create_with_id("session_restore_resets_runtime_state".to_string(), None, None);
    restored_session.save_prepared().expect("save restored session");

    seed_transient_session_state(&mut agent);
    assert_eq!(agent.soft_interrupt_count(), 1);
    assert!(agent.background_tool_signal().is_set());
    assert!(agent.graceful_shutdown_signal().is_set());

    let status = agent
        .restore_session(&restored_session.id)
        .expect("restore session should succeed");

    assert_eq!(status, crate::session::SessionStatus::Active);
    assert_eq!(agent.session_id(), restored_session.id);
    assert_eq!(agent.soft_interrupt_count(), 0);
    assert!(!agent.background_tool_signal().is_set());
    assert!(!agent.graceful_shutdown_signal().is_set());
    assert_eq!(agent.pending_alert_count(), 0);
    assert!(agent.tool_call_ids.is_empty());
    assert!(agent.tool_result_ids.is_empty());
    assert_eq!(agent.tool_output_scan_index, 0);
    assert!(agent.last_upstream_provider.is_none());
    assert!(agent.last_connection_type.is_none());
    assert!(agent.current_turn_system_reminder.is_none());
    assert_eq!(agent.last_usage.input_tokens, 0);
    assert_eq!(agent.last_usage.output_tokens, 0);
    assert!(agent.locked_tools.is_none());
}

#[tokio::test]
async fn explicit_provider_pin_is_persisted_and_reapplied_on_restore() {
    let _guard = crate::storage::lock_test_env();
    let provider = Arc::new(ExplicitPinProvider::new("z-ai/glm-5.2"));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(provider_dyn.clone()).await;
    let mut agent = Agent::new(provider_dyn, registry);
    // Untouched sessions are not persisted (783c979a0); materialize the
    // snapshot so the pin written by set_model lands on disk.
    agent.session.save_prepared().expect("materialize session snapshot");

    agent
        .set_model("z-ai/glm-5.2@Novita")
        .expect("set explicitly pinned model");
    assert_eq!(agent.provider_model(), "z-ai/glm-5.2@Novita");
    let persisted = crate::session::Session::load(agent.session_id()).expect("load saved session");
    assert_eq!(persisted.model.as_deref(), Some("z-ai/glm-5.2@Novita"));

    let restored_provider = Arc::new(ExplicitPinProvider::new("other/model"));
    let restored_provider_dyn: Arc<dyn Provider> = restored_provider.clone();
    let restored_registry = Registry::new(restored_provider_dyn.clone()).await;
    let restored_agent = Agent::new_with_session(restored_provider_dyn, restored_registry, persisted, None);

    assert_eq!(
        restored_provider.set_model_requests.lock().unwrap().as_slice(),
        ["openrouter:z-ai/glm-5.2@Novita"]
    );
    assert_eq!(restored_agent.provider_model(), "z-ai/glm-5.2@Novita");
}

#[tokio::test]
async fn restore_session_rehydrates_injected_memory_ids() {
    let _guard = crate::storage::lock_test_env();
    let _home = IsolatedHome::new();
    crate::memory::clear_all_pending_memory();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let mut restored_session =
        crate::session::Session::create_with_id("session_restore_memory_dedup".to_string(), None, None);
    restored_session.record_memory_injection(
        "🧠 auto-recalled 1 memory".to_string(),
        "persisted memory".to_string(),
        1,
        5,
        vec!["memory-persisted".to_string()],
    );
    restored_session.save_prepared().expect("save restored session");

    crate::memory::mark_memories_injected(&restored_session.id, &["memory-stale".to_string()]);

    agent
        .restore_session(&restored_session.id)
        .expect("restore session should succeed");

    assert!(crate::memory::is_memory_injected(
        &restored_session.id,
        "memory-persisted"
    ));
    assert!(
        !crate::memory::is_memory_injected(&restored_session.id, "memory-stale"),
        "restore should replace stale in-memory dedup state with persisted session data"
    );

    crate::memory::clear_all_pending_memory();
}

#[tokio::test]
async fn build_memory_prompt_nonblocking_defers_pending_memory_during_tool_loop() {
    let _guard = crate::storage::lock_test_env();
    struct RestoreMemoryHome(Option<std::ffi::OsString>);
    impl Drop for RestoreMemoryHome {
        fn drop(&mut self) {
            crate::memory::clear_all_pending_memory();
            match &self.0 {
                Some(home) => crate::env::set_var("HORON_HOME", home),
                None => crate::env::remove_var("HORON_HOME"),
            }
            crate::config::Config::invalidate_cache();
        }
    }
    let home = tempfile::tempdir().expect("isolated memory home");
    let _restore = RestoreMemoryHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    crate::config::Config::invalidate_cache();
    crate::memory::clear_all_pending_memory();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.memory_enabled = true;
    let project = home.path().join("project");
    std::fs::create_dir(&project).expect("isolated project");
    agent.session.working_dir = Some(project.to_string_lossy().into_owned());
    let session_id = agent.session.id.clone();

    let entry = crate::memory::MemoryEntry::new(crate::memory::MemoryCategory::Fact, "remember this later");
    crate::memory::MemoryManager::new()
        .with_project_dir(&project)
        .remember_project(entry.clone())
        .expect("persist the selected memory for scoped revalidation");
    let prompt =
        crate::memory::format_relevant_prompt(std::slice::from_ref(&entry), 1).expect("canonical memory prompt");
    crate::memory::set_pending_memory_for_project(
        &session_id,
        prompt.clone(),
        1,
        vec![entry.id.clone()],
        None,
        agent.session.working_dir.as_deref(),
    );
    assert!(crate::memory::has_pending_memory(&session_id));

    let tool_loop_messages = vec![
        Message::user("hello"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({}),
                thought_signature: None,
            }],
            timestamp: Some(chrono::Utc::now()),
            tool_duration_ms: None,
        },
        Message::tool_result("call_1", "ok", false),
    ];

    let pending = agent.build_memory_prompt_nonblocking(&tool_loop_messages, None);
    assert!(pending.is_none(), "memory should not inject mid tool loop");
    assert!(crate::memory::has_pending_memory(&session_id));
    assert!(!crate::memory::is_memory_injected(&session_id, &entry.id));

    let next_turn_messages = vec![Message::user("follow up")];
    let pending = agent.build_memory_prompt_nonblocking(&next_turn_messages, None);
    assert!(pending.is_some(), "memory should inject on the next real user turn");
    let pending = pending.unwrap();
    assert_eq!(pending.prompt, prompt);
    assert_eq!(pending.memory_ids, vec![entry.id.clone()]);
    assert!(crate::memory::is_memory_injected(&session_id, &entry.id));
    assert!(!crate::memory::has_pending_memory(&session_id));

    crate::memory::clear_all_pending_memory();
}

#[tokio::test]
async fn memory_injection_message_defaults_to_ephemeral_history() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("HORON_AGENT_PERSIST_MEMORY_INJECTIONS");
    crate::env::set_var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS", "false");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let before = agent.session.active_context().len();
    let memory = crate::memory::PendingMemory {
        prompt: "# Memory\n\n## Facts\n1. Use ephemeral mode".to_string(),
        display_prompt: None,
        computed_at: Instant::now(),
        count: 1,
        memory_ids: vec!["mem-ephemeral".to_string()],
    };

    let (message, persisted) = agent.prepare_memory_injection_message(&memory);

    assert!(!persisted);
    assert_eq!(agent.session.active_context().len(), before);
    assert!(matches!(message.role, Role::User));
    assert!(message_text(&message).contains("Use ephemeral mode"));

    match previous {
        Some(value) => crate::env::set_var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS", value),
        None => crate::env::remove_var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS"),
    }
    crate::config::invalidate_config_cache();
}

#[tokio::test]
async fn memory_injection_message_can_persist_to_history() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("HORON_AGENT_PERSIST_MEMORY_INJECTIONS");
    crate::env::set_var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS", "true");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let before = agent.session.active_context().len();
    let memory = crate::memory::PendingMemory {
        prompt: "# Memory\n\n## Facts\n1. Persist for cache".to_string(),
        display_prompt: None,
        computed_at: Instant::now(),
        count: 1,
        memory_ids: vec!["mem-persisted".to_string()],
    };

    let (message, persisted) = agent.prepare_memory_injection_message(&memory);

    assert!(persisted);
    assert_eq!(agent.session.active_context().len(), before + 1);
    assert_eq!(
        content_text(&agent.session.active_context().last().unwrap().content),
        message_text(&message)
    );
    assert!(content_text(&agent.session.active_context().last().unwrap().content).contains("Persist for cache"));

    match previous {
        Some(value) => crate::env::set_var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS", value),
        None => crate::env::remove_var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS"),
    }
    crate::config::invalidate_config_cache();
}

#[tokio::test]
async fn mark_closed_persists_soft_interrupts_for_restore_after_reload() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider.clone(), registry.clone());
    let session_id = agent.session_id().to_string();
    agent.session.save_prepared().expect("save active session");
    agent.queue_soft_interrupt(
        "resume me after reload".to_string(),
        Vec::new(),
        true,
        SoftInterruptSource::System,
    );

    agent.mark_closed();

    let mut restored = Agent::new(provider, registry);
    restored
        .restore_session(&session_id)
        .expect("restore session with persisted interrupts");

    assert_eq!(restored.soft_interrupt_count(), 1);
    assert!(restored.has_urgent_interrupt());
    assert_eq!(
        crate::soft_interrupt_store::load(&session_id)
            .expect("store should be readable after restore")
            .len(),
        1,
        "restore is non-destructive, so the disk copy survives until a turn commits"
    );
    assert_eq!(
        restored.soft_interrupt_count(),
        1,
        "a non-destructive restore must not duplicate the message in memory"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[tokio::test]
async fn env_snapshot_detail_is_minimal_for_empty_sessions_and_full_after_history() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    assert_eq!(agent.env_snapshot_detail(), EnvSnapshotDetail::Minimal);
    let minimal = agent.build_env_snapshot("create", agent.env_snapshot_detail());
    assert!(minimal.horon_agent_git_hash.is_none());
    assert!(minimal.horon_agent_git_dirty.is_none());
    assert!(minimal.working_git.is_none());

    agent.session.append_stored_message(crate::session::StoredMessage {
        turn_start: false,
        id: "msg_env_snapshot_detail".to_string(),
        role: crate::message::Role::User,
        content: vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    });

    assert_eq!(agent.env_snapshot_detail(), EnvSnapshotDetail::Full);
}

/// A trivial tool used to simulate an MCP tool registering on the registry
/// after the agent has already locked its tool snapshot.
struct FakeMcpTool {
    name: String,
}

#[async_trait]
impl crate::tool::Tool for FakeMcpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "fake mcp tool"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, _input: serde_json::Value, _ctx: crate::tool::ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new("ok"))
    }
}

struct VerboseFakeMcpTool {
    name: String,
    description: String,
}

#[async_trait]
impl crate::tool::Tool for VerboseFakeMcpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"value": {"type": "string"}}
        })
    }
    async fn execute(&self, _input: serde_json::Value, _ctx: crate::tool::ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new("ok"))
    }
}

async fn register_fake_deferred_mcp_surface(registry: &Registry) {
    for name in ["mcp_search", "mcp_call"] {
        registry
            .register(
                name.to_string(),
                Arc::new(FakeMcpTool { name: name.to_string() }) as Arc<dyn crate::tool::Tool>,
            )
            .await;
    }
}

async fn agent_with_fake_mcp_surface(mode: crate::config::McpToolsMode, _threshold: usize) -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    register_fake_deferred_mcp_surface(&registry).await;
    registry
        .register(
            "mcp__test__verbose".to_string(),
            Arc::new(VerboseFakeMcpTool {
                name: "verbose".to_string(),
                description: "large MCP definition ".repeat(32),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let mut agent = Agent::new(provider, registry);
    agent.mcp_tools_mode = mode;
    agent
}

#[tokio::test]
async fn mcp_exposure_modes_select_eager_or_fixed_definitions() {
    let _guard = crate::storage::lock_test_env();

    let mut eager = agent_with_fake_mcp_surface(crate::config::McpToolsMode::Eager, 0).await;
    let eager_names: Vec<String> = eager
        .tool_definitions()
        .await
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert!(eager_names.iter().any(|name| name == "mcp__test__verbose"));
    assert!(!eager_names.iter().any(|name| name == "mcp_search"));
    assert!(!eager_names.iter().any(|name| name == "mcp_call"));

    let mut deferred = agent_with_fake_mcp_surface(crate::config::McpToolsMode::Deferred, usize::MAX).await;
    let deferred_names: Vec<String> = deferred
        .tool_definitions()
        .await
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert!(!deferred_names.iter().any(|name| name.starts_with("mcp__")));
    assert!(deferred_names.iter().any(|name| name == "mcp_search"));
    assert!(deferred_names.iter().any(|name| name == "mcp_call"));

    // `auto` never exposes per-server definitions on providers without
    // native deferred loading, even for a tiny catalog: any later change to
    // that list would bust the whole prompt cache.
    let mut auto_small = agent_with_fake_mcp_surface(crate::config::McpToolsMode::Auto, usize::MAX).await;
    let auto_small_names: Vec<String> = auto_small
        .tool_definitions()
        .await
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert!(!auto_small_names.iter().any(|name| name.starts_with("mcp__")));
    assert!(auto_small_names.iter().any(|name| name == "mcp_search"));
    assert!(auto_small_names.iter().any(|name| name == "mcp_call"));

    let mut auto_deferred = agent_with_fake_mcp_surface(crate::config::McpToolsMode::Auto, 1).await;
    let auto_deferred_names: Vec<String> = auto_deferred
        .tool_definitions()
        .await
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert!(!auto_deferred_names.iter().any(|name| name.starts_with("mcp__")));
    assert!(auto_deferred_names.iter().any(|name| name == "mcp_search"));
    assert!(auto_deferred_names.iter().any(|name| name == "mcp_call"));
    let stable_auto_names: Vec<String> = auto_deferred
        .tool_definitions()
        .await
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert_eq!(auto_deferred_names, stable_auto_names);
    assert!(auto_deferred.mcp_late_register_resolved);
}

#[tokio::test]
async fn deferred_mcp_surface_ignores_late_per_tool_registration() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    register_fake_deferred_mcp_surface(&registry).await;
    let mut agent = Agent::new(provider, registry);
    agent.mcp_tools_mode = crate::config::McpToolsMode::Deferred;

    let before: Vec<String> = agent
        .tool_definitions()
        .await
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    agent
        .registry
        .register(
            "mcp__late__tool".to_string(),
            Arc::new(FakeMcpTool {
                name: "late".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let after: Vec<String> = agent
        .tool_definitions()
        .await
        .iter()
        .map(|tool| tool.name.clone())
        .collect();

    assert_eq!(before, after, "fixed deferred surface must stay cache-stable");
    assert!(agent.mcp_late_register_resolved);
    assert!(!after.iter().any(|name| name.starts_with("mcp__")));
}

#[tokio::test]
async fn auto_mode_never_changes_the_tool_list_for_late_mcp_tools() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    register_fake_deferred_mcp_surface(&registry).await;
    let mut agent = Agent::new(provider, registry);
    agent.mcp_tools_mode = crate::config::McpToolsMode::Auto;

    let before = agent.tool_definitions().await;
    assert!(before.iter().any(|tool| tool.name == "mcp_search"));
    assert!(before.iter().any(|tool| tool.name == "mcp_call"));
    agent
        .registry
        .register(
            "mcp__late__large".to_string(),
            Arc::new(VerboseFakeMcpTool {
                name: "large".to_string(),
                description: "late large definition ".repeat(32),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    agent.unlock_tools_if_needed("mcp", &serde_json::json!({}));

    let after = agent.tool_definitions().await;
    assert_eq!(
        serde_json::to_string(&before[..]).unwrap(),
        serde_json::to_string(&after[..]).unwrap(),
        "late MCP registration and the mcp tool must not change the cached tool list"
    );
    assert!(agent.mcp_late_register_resolved);
}

/// Reproduction for #206: MCP tools that register on the registry *after* the
/// first turn locks the tool snapshot never reach the provider, because
/// `tool_definitions()` returns the frozen `locked_tools` snapshot and the only
/// unlock path (`unlock_tools_if_needed`) fires solely when the LLM invokes the
/// `"mcp"` management tool — which it never does, since it cannot see the
/// `mcp__*` tools it would need to trigger that unlock.
#[tokio::test]
async fn mcp_tools_registered_after_lock_are_visible_to_agent() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    // The late-registration rebuild (#206) only exists in explicit eager
    // mode; every other mode keeps the tool list fixed for cache stability.
    agent.mcp_tools_mode = crate::config::McpToolsMode::Eager;

    // First turn locks the snapshot (this is what happens before the async MCP
    // registration spawn completes).
    let before = agent.tool_definitions().await;
    let before_len = before.len();
    assert!(
        !before.iter().any(|t| t.name.starts_with("mcp__")),
        "precondition: no mcp tools before async registration completes"
    );

    // Simulate the spawned MCP registration task finishing: a new mcp__* tool
    // lands on the shared registry.
    agent
        .registry
        .register(
            "mcp__test__write_memory".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp__test__write_memory".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;

    // The next turn should now advertise the MCP tool to the provider.
    let after = agent.tool_definitions().await;
    assert!(
        after.iter().any(|t| t.name == "mcp__test__write_memory"),
        "regression #206: MCP tool registered after the first turn never reaches \
         the agent's tool surface (locked snapshot of {} tools is reused forever)",
        before_len
    );

    // Once MCP tools are present in the locked snapshot, subsequent turns must
    // return the *same* stable snapshot so provider prompt-cache hits stay warm
    // (the whole point of locked_tools). The #206 fix must not flap.
    let names = |defs: &[ToolDefinition]| -> Vec<String> { defs.iter().map(|t| t.name.clone()).collect() };
    let stable_a = agent.tool_definitions().await;
    let stable_b = agent.tool_definitions().await;
    assert_eq!(
        names(&stable_a),
        names(&stable_b),
        "tool snapshot must be stable across turns once MCP tools are present"
    );
    assert_eq!(
        names(&stable_a),
        names(&after),
        "snapshot must not change after MCP tools are already included"
    );
}

/// The intentional, MCP-driven prompt-cache miss must happen at most ONCE per
/// locked snapshot. After the first late-registered `mcp__*` tool is picked up
/// (the one accepted miss), a *second* MCP tool that registers even later must
/// NOT trigger another rebuild — otherwise a server that connects in waves would
/// thrash the provider prompt cache. Guards the `mcp_late_register_resolved`
/// one-shot flag (#206 follow-up).
#[tokio::test]
async fn mcp_late_registration_rebuild_happens_at_most_once() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    // The late-registration rebuild (#206) only exists in explicit eager
    // mode; every other mode keeps the tool list fixed for cache stability.
    agent.mcp_tools_mode = crate::config::McpToolsMode::Eager;

    // First turn locks the snapshot with no MCP tools yet.
    let _ = agent.tool_definitions().await;

    // First MCP tool arrives -> one accepted rebuild exposes it.
    agent
        .registry
        .register(
            "mcp__test__first".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp__test__first".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let after_first = agent.tool_definitions().await;
    assert!(
        after_first.iter().any(|t| t.name == "mcp__test__first"),
        "first late MCP tool must be picked up by the one accepted rebuild"
    );
    assert!(
        agent.mcp_late_register_resolved,
        "one-shot guard must latch after the accepted rebuild"
    );

    // A SECOND MCP tool registers even later (server connected in a second
    // wave). The one-shot guard means we do NOT rebuild again, so the snapshot
    // stays cache-stable and this tool is intentionally not surfaced until the
    // tool list is explicitly unlocked.
    agent
        .registry
        .register(
            "mcp__test__second".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp__test__second".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let after_second = agent.tool_definitions().await;
    let names: Vec<String> = after_second.iter().map(|t| t.name.clone()).collect();
    assert!(
        names.iter().any(|n| n == "mcp__test__first"),
        "previously surfaced MCP tool must remain"
    );
    assert!(
        !names.iter().any(|n| n == "mcp__test__second"),
        "second-wave MCP tool must NOT trigger a second cache-busting rebuild"
    );

    // An explicit unlock (e.g. the `mcp` reload tool) re-arms the one-shot guard
    // and lets the next snapshot pick up everything currently registered.
    agent.unlock_tools();
    assert!(
        !agent.mcp_late_register_resolved,
        "explicit unlock must re-arm the one-shot guard"
    );
    let after_unlock = agent.tool_definitions().await;
    let unlocked_names: Vec<String> = after_unlock.iter().map(|t| t.name.clone()).collect();
    assert!(
        unlocked_names.iter().any(|n| n == "mcp__test__second"),
        "after explicit unlock, the second-wave MCP tool must finally surface"
    );
}

/// Without any newly-registered MCP tools, the locked snapshot must be returned
/// verbatim on every turn (no rebuild, no cache invalidation). Guards the #206
/// fix against re-snapshotting on turns where nothing changed.
#[tokio::test]
async fn tool_snapshot_is_stable_without_new_mcp_tools() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    // The late-registration rebuild (#206) only exists in explicit eager
    // mode; every other mode keeps the tool list fixed for cache stability.
    agent.mcp_tools_mode = crate::config::McpToolsMode::Eager;

    let first = agent.tool_definitions().await;
    // Register a NON-mcp tool after locking — this should NOT trigger a rebuild,
    // because the cache-stability optimization only yields to MCP arrival.
    agent
        .registry
        .register(
            "not_an_mcp_tool".to_string(),
            Arc::new(FakeMcpTool {
                name: "not_an_mcp_tool".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let second = agent.tool_definitions().await;
    let first_names: Vec<String> = first.iter().map(|t| t.name.clone()).collect();
    let second_names: Vec<String> = second.iter().map(|t| t.name.clone()).collect();
    assert_eq!(
        first_names, second_names,
        "non-MCP registry changes must not invalidate the locked tool snapshot"
    );
    assert!(
        !second_names.iter().any(|n| n == "not_an_mcp_tool"),
        "non-MCP tool registered after lock must not leak into the snapshot"
    );
}

#[test]
fn empty_post_tool_response_gets_more_than_one_retry() {
    // Regression guard for the Claude Opus 5 benchmark incident. A provider can
    // return an empty response immediately after tool results; that is a
    // transient hiccup, not a finished task. With only one retry allowed, a
    // single empty response (observed once in 43 turns) ended a 20-hour agent
    // run with the work half-done and the submission unoptimized.
    const {
        assert!(
            Agent::MAX_EMPTY_POST_TOOL_CONTINUATION_ATTEMPTS > 1,
            "a single retry lets one transient empty response end a long run"
        );
        // Bounded, so a genuinely finished agent still exits instead of looping.
        assert!(Agent::MAX_EMPTY_POST_TOOL_CONTINUATION_ATTEMPTS <= 10);
    }
}

#[test]
fn output_budget_truncation_requests_a_continuation() {
    // Regression guard for the Claude Opus 5 benchmark incident. A turn cut off
    // by the output budget reports stop_reason=max_tokens and can contain zero
    // tool calls, which otherwise looks exactly like a finished turn. The agent
    // must treat it as incomplete and continue rather than ending the run.
    assert!(Agent::should_continue_after_stop_reason("max_tokens"));
    assert!(Agent::should_continue_after_stop_reason("MAX_TOKENS"));
    assert!(Agent::should_continue_after_stop_reason(" max_tokens "));
    assert!(Agent::should_continue_after_stop_reason("max_output_tokens"));
    assert!(Agent::should_continue_after_stop_reason("length"));
    assert!(Agent::should_continue_after_stop_reason("truncated"));
    assert!(Agent::should_continue_after_stop_reason("incomplete"));

    // Normal completions must not trigger a continuation loop.
    assert!(!Agent::should_continue_after_stop_reason("end_turn"));
    assert!(!Agent::should_continue_after_stop_reason("tool_use"));
    assert!(!Agent::should_continue_after_stop_reason("stop"));
    // An absent reason is the pre-fix wire behaviour: it cannot be recovered
    // from, which is precisely why MessageEnd must forward the real reason.
    assert!(!Agent::should_continue_after_stop_reason(""));
}

#[test]
fn anthropic_pause_turn_is_resumed() {
    // Long server-tool (web search) turns stop with `pause_turn`; the turn
    // must be resent to continue rather than treated as finished.
    assert!(Agent::should_continue_after_stop_reason("pause_turn"));
    assert!(Agent::is_pause_turn_stop_reason(" PAUSE_TURN "));
    assert!(!Agent::is_pause_turn_stop_reason("max_tokens"));
}

#[test]
fn stranded_tool_use_stop_is_detected() {
    // Second half of the Opus 5 DeepSWE incident: the provider reported
    // stop_reason="tool_use" while the parsed tool-call list was empty, so the
    // turn loop had nothing to execute and broke out mid-task, discarding every
    // uncommitted edit. `tool_use` is a normal completion reason, so
    // `should_continue_after_stop_reason` must keep rejecting it; the stranded
    // case is only recoverable when it is paired with zero tool calls, which is
    // exactly what this predicate is for.
    assert!(Agent::is_stranded_tool_use_stop(Some("tool_use")));
    assert!(Agent::is_stranded_tool_use_stop(Some("TOOL_USE")));
    assert!(Agent::is_stranded_tool_use_stop(Some(" tool_use ")));

    assert!(!Agent::is_stranded_tool_use_stop(Some("end_turn")));
    assert!(!Agent::is_stranded_tool_use_stop(Some("max_tokens")));
    assert!(!Agent::is_stranded_tool_use_stop(Some("")));
    assert!(!Agent::is_stranded_tool_use_stop(None));
    // Must stay disjoint from the truncation path so a turn never takes both
    // continuation branches for one stop reason.
    assert!(!Agent::should_continue_after_stop_reason("tool_use"));
}

#[test]
fn guardrail_stop_reason_detection() {
    assert!(Agent::is_guardrail_stop_reason(Some("refusal")));
    assert!(Agent::is_guardrail_stop_reason(Some("REFUSAL")));
    assert!(Agent::is_guardrail_stop_reason(Some(" content_filter ")));
    assert!(Agent::is_guardrail_stop_reason(Some("safety")));
    assert!(Agent::is_guardrail_stop_reason(Some("model_guardrail")));
    assert!(Agent::is_guardrail_stop_reason(Some("policy_violation_x")));
    assert!(!Agent::is_guardrail_stop_reason(Some("end_turn")));
    assert!(!Agent::is_guardrail_stop_reason(Some("max_tokens")));
    assert!(!Agent::is_guardrail_stop_reason(Some("tool_use")));
    assert!(!Agent::is_guardrail_stop_reason(Some("stop")));
    assert!(!Agent::is_guardrail_stop_reason(None));
}

#[test]
fn fable_guardrail_reconsideration_is_narrow_and_bounded() {
    assert!(Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("refusal"),
        0,
        1,
    ));
    assert!(Agent::should_reconsider_fable_guardrail(
        "CLAUDE-FABLE-5-20260801",
        Some("content_filter"),
        0,
        1,
    ));
    assert!(Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("refusal"),
        1,
        3,
    ));
    assert!(Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("refusal"),
        2,
        3,
    ));
    assert!(!Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("refusal"),
        3,
        3,
    ));
    assert!(!Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("end_turn"),
        0,
        1,
    ));
    assert!(!Agent::should_reconsider_fable_guardrail(
        "claude-opus-5",
        Some("refusal"),
        0,
        1,
    ));
}

#[test]
fn fable_guardrail_prompt_suite_is_distinct_and_safety_preserving() {
    let prompts = Agent::FABLE_GUARDRAIL_RECONSIDERATION_PROMPTS;
    assert_eq!(prompts.len(), 3);
    assert_ne!(prompts[0], prompts[1]);
    assert_ne!(prompts[1], prompts[2]);
    assert!(prompts[0].contains("full context"));
    assert!(prompts[1].contains("safe portions"));
    assert!(prompts[2].contains("Do not weaken a refusal"));
}

#[test]
fn guardrail_notice_for_refusal_stop() {
    let notice = Agent::provider_guardrail_notice(Some("refusal"), true, true)
        .expect("refusal with empty text must produce a notice");
    assert!(
        notice.contains("refusal"),
        "notice should name the stop reason: {notice}"
    );
    assert!(notice.to_lowercase().contains("guardrail"));
    // Guardrail stop with visible text still surfaces (partial output then refusal).
    assert!(Agent::provider_guardrail_notice(Some("refusal"), false, false).is_some());
}

#[test]
fn guardrail_notice_for_silent_empty_turn() {
    // end_turn with zero visible output and reasoning-only content: surface it.
    let notice = Agent::provider_guardrail_notice(Some("end_turn"), true, true)
        .expect("empty visible output must produce a notice");
    assert!(notice.contains("internal reasoning"), "{notice}");
    assert!(notice.contains("end_turn"), "{notice}");
    // Unknown stop reason, empty output, no reasoning.
    let notice =
        Agent::provider_guardrail_notice(None, true, false).expect("empty visible output must produce a notice");
    assert!(notice.contains("unknown"), "{notice}");
    assert!(!notice.contains("internal reasoning"), "{notice}");
}

#[test]
fn guardrail_notice_absent_for_normal_turns() {
    // Normal turn with visible text: no notice.
    assert!(Agent::provider_guardrail_notice(Some("end_turn"), false, false).is_none());
    assert!(Agent::provider_guardrail_notice(None, false, true).is_none());
}

#[test]
fn empty_turn_log_event_separates_guardrails_from_transient_empties() {
    assert_eq!(Agent::empty_turn_log_event(Some("refusal")), "PROVIDER_GUARDRAIL");
    assert_eq!(
        Agent::empty_turn_log_event(Some("content_filter")),
        "PROVIDER_GUARDRAIL"
    );
    assert_eq!(Agent::empty_turn_log_event(Some("stop")), "PROVIDER_EMPTY_RESPONSE");
    assert_eq!(Agent::empty_turn_log_event(None), "PROVIDER_EMPTY_RESPONSE");
}

#[test]
fn guardrail_notice_for_transient_empty_does_not_blame_content_filter() {
    let notice = Agent::provider_guardrail_notice(Some("stop"), true, false)
        .expect("empty visible output must produce a notice");
    assert!(
        !notice.contains("usually a provider-side guardrail"),
        "transient empty responses must not be blamed on a guardrail: {notice}"
    );
    assert!(notice.contains("empty response"), "{notice}");
}

#[tokio::test]
async fn empty_post_tool_response_is_retried_in_shared_helper() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let retried = agent
        .maybe_continue_empty_post_tool_response(true, true, Some("stop"))
        .expect("helper must not error");
    assert!(retried);
    assert_eq!(agent.turn_state().empty_post_tool_continuations(), 1);
    let recovery = agent
        .session
        .active_context()
        .last()
        .expect("recovery instruction must be persisted");
    assert_eq!(recovery.role, Role::User);
    assert!(
        recovery
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .is_some_and(|text| text.starts_with("<system-reminder>")),
        "synthetic recovery instruction must be hidden from the transcript"
    );

    // A guardrail refusal is deliberate and must not be retried.
    let retried = agent
        .maybe_continue_empty_post_tool_response(true, true, Some("refusal"))
        .expect("helper must not error");
    assert!(!retried);
    assert_eq!(
        agent.turn_state().empty_post_tool_continuations(),
        1,
        "a refusal does not spend the retry budget"
    );

    // Visible output or no recent tool result: no retry.
    assert!(
        !agent
            .maybe_continue_empty_post_tool_response(false, true, Some("stop"))
            .unwrap()
    );
    assert!(
        !agent
            .maybe_continue_empty_post_tool_response(true, false, Some("stop"))
            .unwrap()
    );

    // Retry budget is bounded: spend it, then the helper stops retrying.
    while agent.turn_state().empty_post_tool_continuations() < Agent::MAX_EMPTY_POST_TOOL_CONTINUATION_ATTEMPTS {
        assert!(
            agent
                .maybe_continue_empty_post_tool_response(true, true, Some("stop"))
                .unwrap()
        );
    }
    assert!(
        !agent
            .maybe_continue_empty_post_tool_response(true, true, Some("stop"))
            .unwrap()
    );
}

include!("agent_tests/retention_readiness.rs");

/// Provider that reproduces the DeepSWE Opus 5 incident: the first response
/// ends with `stop_reason: "tool_use"` while carrying no tool-use block at all,
/// which is what happens when an unrecognized content block is dropped from the
/// stream. The second response is a normal completion, so a correct agent
/// recovers and this provider's queue is exhausted.
#[derive(Clone, Default)]
struct StrandedToolUseProvider {
    calls: Arc<std::sync::Mutex<usize>>,
}

#[async_trait]
impl Provider for StrandedToolUseProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let call = {
            let mut guard = self.calls.lock().unwrap();
            *guard += 1;
            *guard
        };
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            if call == 1 {
                let _ = tx.send(Ok(StreamEvent::TextDelta("working on it".to_string()))).await;
                // No ToolUseStart: the tool block was lost, yet the provider
                // still reports that it stopped in order to call a tool.
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("tool_use".to_string()),
                    }))
                    .await;
            } else {
                let _ = tx.send(Ok(StreamEvent::TextDelta("all done".to_string()))).await;
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }))
                    .await;
            }
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "stranded-tool-use"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

/// End-to-end guard for the incident. Before the fix the agent took the
/// "no tool calls" branch and ended the turn on the very first response, so a
/// benchmark trial stopped mid-task and its uncommitted work was never
/// captured. The agent must instead ask the model to continue, which shows up
/// as a second provider call and a final turn that ends normally.
#[tokio::test]
async fn stranded_tool_use_stop_continues_instead_of_ending_the_turn() {
    let _guard = crate::storage::lock_test_env();
    let stranded = StrandedToolUseProvider::default();
    let calls = stranded.calls.clone();
    let provider: Arc<dyn Provider> = Arc::new(stranded);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("do the task", Vec::new(), None, tx)
        .await
        .expect("turn should complete");

    let mut text = String::new();
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::TextDelta { text: delta } = event {
            text.push_str(&delta);
        }
    }

    assert_eq!(
        *calls.lock().unwrap(),
        2,
        "a tool_use stop with no tool call must trigger exactly one continuation request"
    );
    assert!(
        text.contains("all done"),
        "the recovered turn must deliver the model's real completion, got {text:?}"
    );
}

#[derive(Clone, Default)]
struct FableGuardrailProvider {
    calls: Arc<std::sync::Mutex<usize>>,
    prompts_seen: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for FableGuardrailProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let call = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if call > 1 {
            let prompt = messages.last().map(message_text).unwrap_or_default().to_string();
            self.prompts_seen.lock().unwrap().push(prompt);
        }

        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            if call <= 3 {
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("refusal".to_string()),
                    }))
                    .await;
            } else {
                let _ = tx
                    .send(Ok(StreamEvent::TextDelta(
                        "Reconsidered and completed safely".to_string(),
                    )))
                    .await;
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }))
                    .await;
            }
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> String {
        "claude-fable-5".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[tokio::test]
async fn fable_guardrail_reconsideration_recovers_the_streaming_turn() {
    let _guard = crate::storage::lock_test_env();
    let fable = FableGuardrailProvider::default();
    let calls = fable.calls.clone();
    let prompts_seen = fable.prompts_seen.clone();
    let provider: Arc<dyn Provider> = Arc::new(fable);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("do this ordinary coding task", Vec::new(), None, tx)
        .await
        .expect("turn should recover from the guardrail");

    let mut text = String::new();
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::TextDelta { text: delta } = event {
            text.push_str(&delta);
        }
    }

    assert_eq!(*calls.lock().unwrap(), 4);
    let prompts = prompts_seen.lock().unwrap();
    assert_eq!(prompts.len(), 3);
    assert!(prompts[0].contains("concrete harmful action"));
    assert!(prompts[1].contains("safe portions"));
    assert!(prompts[2].contains("final, independent policy check"));
    assert!(text.contains("Reconsidered and completed safely"), "{text:?}");
}

#[tokio::test]
async fn sdk_custom_tool_schema_survives_locked_refresh() {
    let _lock = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(SignatureSessionProvider::default());
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    crate::tool::sdk::configure(
        agent.session_id(),
        "cache-owner",
        crate::protocol::SessionToolConfig {
            enabled: Some(vec![]),
            disabled: vec![],
            custom: vec![crate::protocol::SessionToolDefinition {
                name: "sdk_custom".into(),
                description: "SDK override".into(),
                parameters: serde_json::json!({"type":"object", "additionalProperties":false}),
            }],
        },
        tx,
    )
    .unwrap();
    for _ in 0..2 {
        let definitions = agent.tool_definitions().await;
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].description, "SDK override");
        assert_eq!(
            definitions[0].input_schema,
            serde_json::json!({"type":"object", "additionalProperties":false})
        );
    }
}

#[test]
fn system_prompt_override_restores_and_does_not_leak_across_sessions() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(home) => crate::env::set_var("HORON_HOME", home),
                None => crate::env::remove_var("HORON_HOME"),
            }
        }
    }
    let _restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    for prompt in ["custom system prompt", ""] {
        let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
        let mut agent = Agent::new(provider.clone(), Registry::empty());
        agent.set_system_prompt(prompt);
        let id = agent.session_id().to_string();
        let split = agent.build_system_prompt_split(Some("memory must not be appended"));
        assert_eq!(split.static_part, prompt);
        assert!(split.dynamic_part.is_empty());
        assert_eq!(Session::load(&id).unwrap().system_prompt.as_deref(), Some(prompt));

        let old_head = agent.session.selected_event_head().unwrap();
        agent.clear();
        assert_eq!(agent.session.system_prompt, None);
        assert_ne!(agent.build_system_prompt_split(None).static_part, prompt);
        agent.session.select_event_head(&old_head).unwrap();
        agent.restore_session(&id).unwrap();
        assert_eq!(agent.build_system_prompt_split(None).static_part, prompt);

        let mut other = Session::create(None, Some("plain session".into()));
        other.save().unwrap();
        agent.restore_session(&other.id).unwrap();
        assert_eq!(agent.session.system_prompt, None);
        assert_ne!(agent.build_system_prompt_split(None).static_part, prompt);
        let loaded = Session::load(&id).unwrap();
        let attached = Agent::new_with_session(provider, Registry::empty(), loaded, None);
        assert_eq!(attached.build_system_prompt_split(None).static_part, prompt);
    }
}

/// Test provider advertising provider-native deferred tool loading.
/// Provider whose deferred-loading capability can flip, like a mid-session
/// switch between Claude and a non-native route.
struct SwitchableDeferredProvider(Arc<std::sync::atomic::AtomicBool>);

#[async_trait]
impl Provider for SwitchableDeferredProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (_tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(1);
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "switchable"
    }

    fn supports_deferred_tools(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self(Arc::clone(&self.0)))
    }
}

fn transcript_texts(agent: &Agent) -> Vec<String> {
    agent
        .session
        .active_context()
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[path = "agent_tests/deferred_tools.rs"]
mod deferred_tools;

#[path = "agent_tests/tool_tiers.rs"]
mod tool_tiers;

#[test]
fn skill_installed_mid_session_keeps_system_prompt_stable_and_is_announced_once() {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(home) => crate::env::set_var("HORON_HOME", home),
                None => crate::env::remove_var("HORON_HOME"),
            }
        }
    }
    let _restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());

    let project = tempfile::tempdir().unwrap();
    let write_skill = |name: &str, description: &str| {
        let dir = project.path().join(".horon/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nBody of {name}.\n"),
        )
        .unwrap();
    };
    write_skill("early-skill", "Present when the session starts");

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let mut agent =
        Agent::new_with_initial_working_dir(provider, Registry::empty(), Some(project.path().to_str().unwrap()));
    let before = agent.build_system_prompt_split(None).static_part;
    assert!(before.contains("/early-skill "), "{before}");

    write_skill("late-skill", "Installed after the prompt was cached");

    // The cached system prefix must not change when a skill is installed.
    let after = agent.build_system_prompt_split(None).static_part;
    assert_eq!(before, after);
    assert!(!after.contains("late-skill"));

    agent.announce_late_skills();
    let announcements: Vec<String> = transcript_texts(&agent)
        .into_iter()
        .filter(|text| text.contains("New skills were installed."))
        .collect();
    assert_eq!(announcements.len(), 1);
    assert!(announcements[0].contains("- `/late-skill ` - Installed after the prompt was cached"));
    assert!(!announcements[0].contains("early-skill"));

    // Nothing new: no second announcement, prompt still stable.
    agent.announce_late_skills();
    let count = transcript_texts(&agent)
        .into_iter()
        .filter(|text| text.contains("New skills were installed."))
        .count();
    assert_eq!(count, 1);
    assert_eq!(agent.build_system_prompt_split(None).static_part, before);

    // A restored agent (fresh in-memory state) must not re-announce.
    agent.announced_skills.clear();
    agent.announced_skills.insert("early-skill".to_string());
    agent.announced_skills_scan_index = 0;
    agent.announce_late_skills();
    let count = transcript_texts(&agent)
        .into_iter()
        .filter(|text| text.contains("New skills were installed."))
        .count();
    assert_eq!(count, 1);
}

/// Esc after a queued follow-up: a cancelled turn must not swallow the
/// follow-up into history. It stays queued so the next turn can send it.
#[tokio::test]
async fn cancelled_turn_leaves_soft_interrupt_queued() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.queue_soft_interrupt(
        "do this instead".to_string(),
        Vec::new(),
        false,
        SoftInterruptSource::User,
    );

    agent.request_graceful_shutdown();
    assert!(agent.inject_soft_interrupts(None).await.is_empty());
    assert_eq!(agent.soft_interrupt_count(), 1, "still queued after cancel");

    agent.graceful_shutdown_signal().reset();
    assert_eq!(agent.inject_soft_interrupts(None).await.len(), 1);
    assert_eq!(agent.soft_interrupt_count(), 0);
}

// ---------------------------------------------------------------------------
// Doubles shared by the end-to-end tool-round tests of BOTH turn loops.
//
// `run_turn` and `run_turn_streaming_mpsc` are separate paths with separate
// test modules, but they must prove one and the same behavior: an assistant
// message that asks for two `Concurrent` calls and one `Ordered` call, with the
// test deciding which concurrent member finishes first. The provider, the
// scripted message, and the gated/sleeping tool live here once so both modules
// drive the same doubles instead of two near-copies that can drift apart.
// ---------------------------------------------------------------------------

/// Provider that replays one scripted assistant message per request and records
/// the dynamic system prompt of every request. Once the script is exhausted it
/// answers with plain text, so a turn that outlives its script ends normally.
#[derive(Clone)]
pub(super) struct SharedRoundProvider {
    rounds: Arc<std::sync::Mutex<std::collections::VecDeque<Vec<StreamEvent>>>>,
    dynamic_prompts: Arc<std::sync::Mutex<Vec<String>>>,
}

impl SharedRoundProvider {
    fn next_events(&self) -> Vec<Result<StreamEvent>> {
        match self.rounds.lock().unwrap().pop_front() {
            Some(events) => events.into_iter().map(Ok).collect(),
            None => vec![
                Ok(StreamEvent::TextDelta("done".into())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                }),
            ],
        }
    }
}

#[async_trait]
impl Provider for SharedRoundProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::iter(self.next_events())))
    }

    async fn complete_split(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        system_dynamic: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.dynamic_prompts.lock().unwrap().push(system_dynamic.to_string());
        Ok(Box::pin(futures::stream::iter(self.next_events())))
    }

    fn name(&self) -> &str {
        "shared-round-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

/// Build the shared provider plus the log of dynamic system prompts it saw.
pub(super) fn shared_round_provider(
    rounds: Vec<Vec<StreamEvent>>,
) -> (Arc<SharedRoundProvider>, Arc<std::sync::Mutex<Vec<String>>>) {
    let dynamic_prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = Arc::new(SharedRoundProvider {
        rounds: Arc::new(std::sync::Mutex::new(rounds.into())),
        dynamic_prompts: Arc::clone(&dynamic_prompts),
    });
    (provider, dynamic_prompts)
}

/// One streamed `tool_use` block, the way a provider delivers it.
pub(super) fn shared_tool_use(id: &str, name: &str, input: &serde_json::Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolUseStart {
            id: id.into(),
            name: name.into(),
        },
        StreamEvent::ToolInputDeltaFor {
            id: id.into(),
            delta: input.to_string(),
        },
        StreamEvent::ToolUseEndFor { id: id.into() },
    ]
}

/// The end of an assistant message that stopped in order to call tools.
pub(super) fn shared_round_end() -> StreamEvent {
    StreamEvent::MessageEnd {
        stop_reason: Some("tool_use".into()),
    }
}

/// The message both loops must handle: two `Concurrent` reads, then one
/// `Ordered` shell call, in that request order and in ONE assistant message.
pub(super) fn shared_mixed_round() -> Vec<StreamEvent> {
    let mut events = Vec::new();
    events.extend(shared_tool_use("call-a", "read", &serde_json::json!({ "key": "a" })));
    events.extend(shared_tool_use("call-b", "read", &serde_json::json!({ "key": "b" })));
    events.extend(shared_tool_use("call-c", "bash", &serde_json::json!({ "key": "c" })));
    events.push(shared_round_end());
    events
}

#[derive(Default)]
struct SharedGateState {
    released: std::collections::HashSet<String>,
    waiters: std::collections::HashMap<String, Arc<tokio::sync::Notify>>,
}

/// Holds named members of a round open so a test can decide who finishes first.
pub(super) struct SharedRoundGate {
    started_tx: tokio_mpsc::UnboundedSender<String>,
    finished_tx: tokio_mpsc::UnboundedSender<String>,
    state: std::sync::Mutex<SharedGateState>,
}

impl SharedRoundGate {
    pub(super) fn new() -> (
        Arc<Self>,
        tokio_mpsc::UnboundedReceiver<String>,
        tokio_mpsc::UnboundedReceiver<String>,
    ) {
        let (started_tx, started_rx) = tokio_mpsc::unbounded_channel();
        let (finished_tx, finished_rx) = tokio_mpsc::unbounded_channel();
        let gate = Arc::new(Self {
            started_tx,
            finished_tx,
            state: std::sync::Mutex::new(SharedGateState::default()),
        });
        (gate, started_rx, finished_rx)
    }

    async fn park(&self, key: &str) {
        let _ = self.started_tx.send(key.to_string());
        let notify = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            Arc::clone(
                state
                    .waiters
                    .entry(key.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Notify::new())),
            )
        };
        let mut notified = std::pin::pin!(notify.notified());
        notified.as_mut().enable();
        let already_released = {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.released.contains(key)
        };
        if !already_released {
            notified.await;
        }
        let _ = self.finished_tx.send(key.to_string());
    }

    pub(super) fn release(&self, key: &str) {
        let notify = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.released.insert(key.to_string());
            state.waiters.get(key).cloned()
        };
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }
}

/// One observed execution of a [`SharedRoundTool`].
#[derive(Clone)]
pub(super) struct SharedToolRun {
    pub(super) key: String,
    pub(super) started: Instant,
    pub(super) finished: Instant,
}

/// What a shared round tool does when it is called.
pub(super) enum SharedToolMode {
    /// Park until the test releases this key, so completion order can be made
    /// to differ from request order on purpose.
    Gate(Arc<SharedRoundGate>),
    /// Sleep this long. This is how the overlap test gets two real 400 ms
    /// members without a fixed sleep acting as the only synchronization: the
    /// gate channels still decide when the round is observed.
    Delay(Duration),
    /// Answer immediately.
    Echo,
}

/// Tool double whose concurrency class and behavior the test selects.
pub(super) struct SharedRoundTool {
    name: &'static str,
    class: ConcurrencyClass,
    mode: SharedToolMode,
    runs: Arc<std::sync::Mutex<Vec<SharedToolRun>>>,
    executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl Tool for SharedRoundTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "shared end-to-end round test double"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        self.class
    }

    /// The round scheduler groups by declared effects, so the double reports
    /// the effects its class stands for. The trait default is a write barrier,
    /// which would serialize every call these tests mean to overlap.
    fn effects(&self) -> ToolEffects {
        match self.class {
            ConcurrencyClass::Concurrent => ToolEffects::read(),
            ConcurrencyClass::Ordered => ToolEffects::write(),
        }
    }

    async fn execute(&self, input: serde_json::Value, _: ToolContext) -> Result<ToolOutput> {
        let key = input["key"].as_str().unwrap_or_default().to_string();
        let started = Instant::now();
        match &self.mode {
            SharedToolMode::Gate(gate) => gate.park(&key).await,
            SharedToolMode::Delay(delay) => tokio::time::sleep(*delay).await,
            SharedToolMode::Echo => {}
        }
        let finished = Instant::now();
        self.executions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.runs.lock().unwrap().push(SharedToolRun {
            key: key.clone(),
            started,
            finished,
        });
        Ok(ToolOutput::new(format!("{}:{}", self.name, key)))
    }
}

fn shared_tool(
    name: &'static str,
    class: ConcurrencyClass,
    mode: SharedToolMode,
    runs: Arc<std::sync::Mutex<Vec<SharedToolRun>>>,
    executions: Arc<std::sync::atomic::AtomicUsize>,
) -> SharedRoundTool {
    SharedRoundTool {
        name,
        class,
        mode,
        runs,
        executions,
    }
}

async fn shared_registry(tools: Vec<SharedRoundTool>) -> Registry {
    let registry = Registry::empty();
    for tool in tools {
        let name = tool.name.to_string();
        registry.register(name, Arc::new(tool) as Arc<dyn Tool>).await;
    }
    registry
}

/// Everything one gated end-to-end round needs: the provider, the registry with
/// a gated `read` and an immediate `bash`, and the gate's observation channels.
pub(super) struct SharedGatedRound {
    pub(super) provider: Arc<SharedRoundProvider>,
    pub(super) registry: Registry,
    pub(super) gate: Arc<SharedRoundGate>,
    pub(super) started: tokio_mpsc::UnboundedReceiver<String>,
    pub(super) finished: tokio_mpsc::UnboundedReceiver<String>,
    pub(super) executions: Arc<std::sync::atomic::AtomicUsize>,
}

pub(super) async fn shared_gated_round(rounds: Vec<Vec<StreamEvent>>) -> SharedGatedRound {
    let (provider, _prompts) = shared_round_provider(rounds);
    let (gate, started, finished) = SharedRoundGate::new();
    let runs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry = shared_registry(vec![
        shared_tool(
            "read",
            ConcurrencyClass::Concurrent,
            SharedToolMode::Gate(Arc::clone(&gate)),
            Arc::clone(&runs),
            Arc::clone(&executions),
        ),
        shared_tool(
            "bash",
            ConcurrencyClass::Ordered,
            SharedToolMode::Echo,
            runs,
            Arc::clone(&executions),
        ),
    ])
    .await;
    SharedGatedRound {
        provider,
        registry,
        gate,
        started,
        finished,
        executions,
    }
}

/// Everything one timed end-to-end round needs: the provider, a registry whose
/// two `Concurrent` members sleep for the same delay, and the runs they logged.
pub(super) struct SharedTimedRound {
    pub(super) provider: Arc<SharedRoundProvider>,
    pub(super) registry: Registry,
    pub(super) runs: Arc<std::sync::Mutex<Vec<SharedToolRun>>>,
    pub(super) executions: Arc<std::sync::atomic::AtomicUsize>,
}

pub(super) async fn shared_timed_round(rounds: Vec<Vec<StreamEvent>>, delay: Duration) -> SharedTimedRound {
    let (provider, _prompts) = shared_round_provider(rounds);
    let runs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry = shared_registry(vec![
        shared_tool(
            "read",
            ConcurrencyClass::Concurrent,
            SharedToolMode::Delay(delay),
            Arc::clone(&runs),
            Arc::clone(&executions),
        ),
        shared_tool(
            "bash",
            ConcurrencyClass::Ordered,
            SharedToolMode::Echo,
            Arc::clone(&runs),
            Arc::clone(&executions),
        ),
    ])
    .await;
    SharedTimedRound {
        provider,
        registry,
        runs,
        executions,
    }
}

/// One line per transcript block, in stored order, for the manual-QA artifact:
/// the role, then the block kind and its tool id or a short text prefix.
pub(super) fn shared_dump_transcript(label: &str, agent: &Agent) {
    for (index, message) in agent.session.active_context().iter().enumerate() {
        for block in &message.content {
            let line = match block {
                ContentBlock::ToolUse { id, name, .. } => format!("tool_use id={id} name={name}"),
                ContentBlock::ToolResult {
                    tool_use_id, is_error, ..
                } => format!("tool_result id={tool_use_id} error={is_error:?}"),
                ContentBlock::Text { text, .. } => {
                    format!("text {:?}", crate::util::truncate_str(text.as_str(), 40))
                }
                other => format!("block {other:?}").chars().take(60).collect(),
            };
            std::println!("QA {label} message[{index}] role={:?} {line}", message.role);
        }
    }
}

/// Registry with one immediate tool, for the loop-guard stop path.
pub(super) async fn shared_echo_registry(
    name: &'static str,
    class: ConcurrencyClass,
) -> (Registry, Arc<std::sync::atomic::AtomicUsize>) {
    let runs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry = shared_registry(vec![shared_tool(
        name,
        class,
        SharedToolMode::Echo,
        runs,
        Arc::clone(&executions),
    )])
    .await;
    (registry, executions)
}

pub(super) async fn shared_recv_or_panic(rx: &mut tokio_mpsc::UnboundedReceiver<String>, what: &str) -> String {
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap_or_else(|| panic!("gate channel closed while waiting for {what}"))
}

/// Sets `[features] parallel_tools` for one test and restores the default
/// before the sandbox drops.
///
/// The config cache is process-global and re-fingerprints at most every 500ms
/// (its interval is zero only when the base crate itself is built with `--test`),
/// so a value left behind would reach any test that reads config while the
/// sandbox is alive. Restoring here keeps the disabled path local to this test.
pub(super) struct ParallelToolsFlag;

impl ParallelToolsFlag {
    pub(super) fn set(root: &std::path::Path, enabled: bool) -> Self {
        write_parallel_tools(root, enabled);
        Self
    }
}

impl Drop for ParallelToolsFlag {
    fn drop(&mut self) {
        if let Ok(dir) = std::env::var("HORON_HOME") {
            write_parallel_tools(std::path::Path::new(&dir), true);
        }
    }
}

pub(super) fn write_parallel_tools(root: &std::path::Path, enabled: bool) {
    std::fs::write(
        root.join("config.toml"),
        format!("[features]\nparallel_tools = {enabled}\n"),
    )
    .expect("the config is written");
    crate::config::invalidate_config_cache();
}

/// One transcript's tool results, in stored order: `(tool_use_id, content,
/// is_error)`.
///
/// Deliberately carries no timing: IS-2 allows durations and timestamps to
/// differ between the parallel and sequential paths, so a content comparison
/// must not read them.
pub(super) fn shared_tool_results(agent: &Agent) -> Vec<(String, String, Option<bool>)> {
    agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((tool_use_id.clone(), content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

/// Prints the two result lists side by side for the manual-QA artifact.
pub(super) fn shared_dump_content_pair(
    parallel: &[(String, String, Option<bool>)],
    sequential: &[(String, String, Option<bool>)],
) {
    let rows = parallel.len().max(sequential.len());
    for index in 0..rows {
        let parallel = parallel.get(index);
        let sequential = sequential.get(index);
        std::println!(
            "QA content[{index}] parallel={parallel:?} sequential={sequential:?} equal={}",
            parallel == sequential
        );
    }
}

/// Drives ONE scripted mixed round through a caller-supplied entry point and
/// returns the tool results it stored.
///
/// `parallel_tools` decides the completion order the gate is released in: when
/// enabled the two `read` members overlap and `b` finishes first, which is the
/// order that exposes a payload paired with the wrong id; when disabled each
/// read runs alone, in request order. The gate, never a sleep, decides when the
/// round may continue.
pub(super) async fn shared_round_results<F, Fut>(
    parallel_tools: bool,
    rounds: Vec<Vec<StreamEvent>>,
    drive: F,
) -> Vec<(String, String, Option<bool>)>
where
    F: FnOnce(Agent) -> Fut,
    Fut: std::future::Future<Output = Agent> + Send + 'static,
{
    let SharedGatedRound {
        provider,
        registry,
        gate,
        mut started,
        mut finished,
        ..
    } = shared_gated_round(rounds).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read two files, then run the shell".into(),
            cache_control: None,
        }],
    );
    let task = tokio::spawn(drive(agent));

    if parallel_tools {
        let mut starts = vec![
            shared_recv_or_panic(&mut started, "first read start").await,
            shared_recv_or_panic(&mut started, "second read start").await,
        ];
        starts.sort();
        assert_eq!(
            starts,
            vec!["a".to_string(), "b".to_string()],
            "the parallel run must overlap both reads"
        );
        // Completion order is deliberately the reverse of request order.
        gate.release("b");
        assert_eq!(shared_recv_or_panic(&mut finished, "b finish").await, "b");
        gate.release("a");
        assert_eq!(shared_recv_or_panic(&mut finished, "a finish").await, "a");
    } else {
        for key in ["a", "b"] {
            assert_eq!(
                shared_recv_or_panic(&mut started, "read start").await,
                key,
                "with parallel_tools off the reads run one at a time, in request order"
            );
            gate.release(key);
            assert_eq!(shared_recv_or_panic(&mut finished, "read finish").await, key);
        }
    }

    let agent = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the turn completes after the gated reads")
        .unwrap_or_else(|error| panic!("the turn task failed: {error}"));
    shared_tool_results(&agent)
}
