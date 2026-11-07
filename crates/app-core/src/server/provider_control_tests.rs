#![cfg_attr(test, allow(clippy::await_holding_lock))]
use super::*;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, ModelRoute, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex as StdMutex, MutexGuard as StdMutexGuard};

async fn recv_final_catalog_notification(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match rx.recv().await.expect("client event channel should stay open") {
                ServerEvent::Notification {
                    notification_type: NotificationType::Message { scope: Some(scope), .. },
                    message,
                    ..
                } if scope == "catalog_activity" && message.contains("Model ready:") => {
                    break message;
                }
                _ => continue,
            }
        }
    })
    .await
    .expect("expected final auth catalog notification")
}

#[tokio::test]
async fn notify_auth_changed_clears_renewed_login_failure() {
    let _guard = EnvGuard::save(&[
        "HORON_AGENT_RUNTIME_PROVIDER",
        "HORON_AGENT_ACTIVE_PROVIDER",
        "HORON_AGENT_INITIAL_PROVIDER_EXPLICIT",
    ]);
    crate::provider::record_provider_unavailable_for_account("openai", "old refresh token rejected");
    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), Registry::empty())));
    let session_id = agent.lock().await.session_id().to_string();
    let sessions = Arc::new(RwLock::new(HashMap::new()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_notify_auth_changed(
        72,
        Some("openai".to_string()),
        None,
        false,
        &provider,
        &provider,
        &sessions,
        &session_id,
        &agent,
        &tx,
    )
    .await;
    recv_final_catalog_notification(&mut rx).await;
    assert!(crate::provider::provider_unavailability_detail_for_account("openai").is_none());
}

/// A live daemon inherits its environment at launch. When `SetApiKey` saves a
/// new key to the profile's env file, the stale inherited env var must not keep
/// winning at resolution time (issue #453).
#[tokio::test]
async fn notify_auth_changed_replaces_stale_process_api_key_with_saved_credential() {
    let _guard = EnvGuard::save(&[
        "OPENAI_API_KEY",
        "HORON_AGENT_OPENROUTER_API_BASE",
        "HORON_AGENT_OPENROUTER_API_KEY_NAME",
        "HORON_AGENT_OPENROUTER_ENV_FILE",
        "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
        "HORON_AGENT_OPENROUTER_PROVIDER_FEATURES",
        "HORON_AGENT_OPENROUTER_TRANSPORT_STATE",
        "HORON_AGENT_OPENROUTER_ALLOW_NO_AUTH",
        "HORON_AGENT_OPENROUTER_MODEL_CATALOG",
        "HORON_AGENT_OPENROUTER_MODEL",
        "HORON_AGENT_OPENROUTER_STATIC_MODELS",
        "HORON_AGENT_OPENROUTER_AUTH_HEADER",
        "HORON_AGENT_OPENROUTER_AUTH_HEADER_NAME",
        "HORON_AGENT_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "HORON_AGENT_OPENROUTER_PROVIDER",
        "HORON_AGENT_OPENROUTER_NO_FALLBACK",
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_PROVIDER_PROFILE_ACTIVE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
    ]);
    crate::env::set_var("OPENAI_API_KEY", "stale-inherited-key");
    let config_dir = crate::storage::app_config_dir().expect("resolve app config dir");
    std::fs::create_dir_all(&config_dir).expect("create app config dir");
    std::fs::write(config_dir.join("openai.env"), "OPENAI_API_KEY=fresh-saved-key\n").expect("write saved credential");

    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), Registry::empty())));
    let session_id = agent.lock().await.session_id().to_string();
    let sessions = Arc::new(RwLock::new(HashMap::new()));
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut auth = crate::protocol::AuthChanged::new("openai-api");
    auth.credential_source = Some(crate::protocol::AuthCredentialSource::ApiKeyFile);
    auth.auth_method = Some(crate::protocol::AuthMethod::RemoteTuiPasteApiKey);

    handle_notify_auth_changed(
        73,
        None,
        Some(auth),
        false,
        &provider,
        &provider,
        &sessions,
        &session_id,
        &agent,
        &tx,
    )
    .await;

    assert_eq!(
        crate::provider_catalog::load_api_key_from_env_or_config("OPENAI_API_KEY", "openai.env").as_deref(),
        Some("fresh-saved-key"),
        "the saved credential must win over the stale inherited env var"
    );
}

#[test]
fn compact_auth_catalog_activity_reports_changed_and_unchanged_in_two_lines() {
    let changed = ModelCatalogRefreshSummary {
        model_count_before: 33,
        model_count_after: 37,
        models_added: 14,
        models_removed: 10,
        route_count_before: 33,
        route_count_after: 38,
        routes_added: 24,
        routes_removed: 19,
        routes_changed: 3,
        ..ModelCatalogRefreshSummary::default()
    };
    let changed_message = format_auth_catalog_refresh_complete(Some("OpenAI"), Some("gpt-5.6-sol"), &changed, false);
    assert_eq!(changed_message.lines().count(), 2);
    assert!(changed_message.contains("OpenAI catalog changed"));
    assert!(changed_message.contains("models +14/-10"));
    assert!(changed_message.contains("routes +24/-19/~3"));

    let unchanged = ModelCatalogRefreshSummary {
        model_count_before: 37,
        model_count_after: 37,
        route_count_before: 38,
        route_count_after: 38,
        ..ModelCatalogRefreshSummary::default()
    };
    let unchanged_message =
        format_auth_catalog_refresh_complete(Some("OpenAI"), Some("gpt-5.6-sol"), &unchanged, false);
    assert_eq!(unchanged_message.lines().count(), 2);
    assert!(unchanged_message.contains("OpenAI catalog unchanged: 37 models, 38 routes"));
}

#[derive(Default)]
struct AuthChangeMockState {
    logged_in: StdRwLock<bool>,
    selected_model: StdRwLock<Option<String>>,
    route_provider: StdRwLock<String>,
    route_api_method: StdRwLock<String>,
    routes_override: StdRwLock<Option<Vec<ModelRoute>>>,
    expose_selected_model_in_routes: StdRwLock<bool>,
    auth_refresh_delay_ms: AtomicUsize,
    auth_refresh_pending: AtomicUsize,
    complete_calls: AtomicUsize,
    complete_models: StdMutex<Vec<String>>,
}

struct AuthChangeMockProvider {
    state: Arc<AuthChangeMockState>,
}

impl AuthChangeMockProvider {
    fn new() -> Self {
        let state = AuthChangeMockState {
            route_provider: StdRwLock::new("MockAuth".to_string()),
            route_api_method: StdRwLock::new("mock-auth".to_string()),
            expose_selected_model_in_routes: StdRwLock::new(true),
            ..AuthChangeMockState::default()
        };
        Self { state: Arc::new(state) }
    }
}

#[async_trait]
impl Provider for AuthChangeMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        self.state.complete_calls.fetch_add(1, Ordering::SeqCst);
        self.state.complete_models.lock().unwrap().push(self.model());
        let stream = futures::stream::iter([
            Ok(StreamEvent::TextDelta("ok".to_string())),
            Ok(StreamEvent::MessageEnd { stop_reason: None }),
        ]);
        Ok(Box::pin(stream) as Pin<Box<dyn futures::Stream<Item = _> + Send>>)
    }

    fn name(&self) -> &str {
        "mock-auth"
    }

    fn model(&self) -> String {
        if let Some(model) = self.state.selected_model.read().unwrap().clone() {
            return model;
        }

        if *self.state.logged_in.read().unwrap() {
            "logged-in-model".to_string()
        } else {
            "logged-out-model".to_string()
        }
    }

    fn available_models_display(&self) -> Vec<String> {
        if let Some(routes) = self.state.routes_override.read().unwrap().as_ref() {
            return routes.iter().map(|route| route.model.clone()).collect();
        }
        let mut models = if *self.state.logged_in.read().unwrap() {
            vec!["logged-in-model".to_string(), "second-model".to_string()]
        } else {
            vec!["logged-out-model".to_string()]
        };

        if *self.state.expose_selected_model_in_routes.read().unwrap()
            && let Some(model) = self.state.selected_model.read().unwrap().clone()
            && !models.iter().any(|candidate| candidate == &model)
        {
            models.insert(0, model);
        }

        models
    }

    fn available_models_for_switching(&self) -> Vec<String> {
        self.available_models_display()
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        let model = model.trim();
        let model = model.split_once(':').map(|(_, model)| model).unwrap_or(model).trim();
        if model.is_empty() {
            anyhow::bail!("model cannot be empty");
        }

        *self.state.selected_model.write().unwrap() = Some(model.to_string());
        Ok(())
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        if let Some(routes) = self.state.routes_override.read().unwrap().as_ref() {
            return routes.clone();
        }
        let provider = self.state.route_provider.read().unwrap().clone();
        let api_method = self.state.route_api_method.read().unwrap().clone();
        self.available_models_display()
            .into_iter()
            .map(|model| ModelRoute {
                model,
                provider: provider.clone(),
                api_method: api_method.clone(),
                available: true,
                detail: String::new(),
                usage: None,
                cheapness: None,
            })
            .collect()
    }

    fn on_auth_changed(&self) {
        let delay_ms = self.state.auth_refresh_delay_ms.load(Ordering::Acquire);
        if delay_ms == 0 {
            *self.state.logged_in.write().unwrap() = true;
            crate::bus::Bus::global().publish_models_updated();
            return;
        }

        self.state.auth_refresh_pending.store(1, Ordering::Release);
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms as u64)).await;
            *state.logged_in.write().unwrap() = true;
            state.auth_refresh_pending.store(0, Ordering::Release);
            crate::bus::Bus::global().publish_models_updated();
        });
    }

    fn auth_model_refresh_pending(&self) -> bool {
        self.state.auth_refresh_pending.load(Ordering::Acquire) > 0
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            state: Arc::clone(&self.state),
        })
    }
}

/// Shared process-wide lock: env vars are global, so a private mutex here would
/// race every other env-mutating test (issue #593).
fn lock_env() -> StdMutexGuard<'static, ()> {
    crate::storage::lock_test_env()
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
    _temp_home: tempfile::TempDir,
    _lock: StdMutexGuard<'static, ()>,
}

impl EnvGuard {
    /// Save and clear the given env vars, and redirect `HORON_AGENT_HOME` to a fresh
    /// empty temp dir for the lifetime of the guard.
    ///
    /// The temp home keeps these tests hermetic: provider activation reads
    /// on-disk model catalog caches (`~/.horon-agent/cache/<profile>_models.json`) to
    /// pick a profile's newest default model, so without an isolated home the
    /// host's real caches leak in and a stale or non-chat model (e.g. Groq's
    /// `canopylabs/orpheus-*` TTS) can be auto-selected, breaking the test on
    /// developer machines while passing on clean CI.
    fn save(keys: &[&'static str]) -> Self {
        let lock = lock_env();
        let mut all_keys: Vec<&'static str> = keys.to_vec();
        if !all_keys.contains(&"HORON_AGENT_HOME") {
            all_keys.push("HORON_AGENT_HOME");
        }
        let saved = all_keys.iter().map(|key| (*key, std::env::var(key).ok())).collect();
        for key in &all_keys {
            crate::env::remove_var(key);
        }
        let temp_home = tempfile::tempdir().expect("create temp HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", temp_home.path());
        Self {
            saved,
            _temp_home: temp_home,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            if let Some(value) = value {
                crate::env::set_var(key, value);
            } else {
                crate::env::remove_var(key);
            }
        }
    }
}

#[tokio::test]
async fn notify_auth_changed_emits_available_models_updated_after_provider_update() {
    let _guard = EnvGuard::save(&[]);
    crate::bus::reset_models_updated_publish_state_for_tests();
    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let registry = Registry::empty();
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
    let session_id = { agent.lock().await.session_id().to_string() };
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        "test-session".to_string(),
        Arc::clone(&agent),
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    handle_notify_auth_changed(
        42,
        None,
        None,
        false,
        &provider,
        &provider,
        &sessions,
        session_id.as_str(),
        &agent,
        &client_event_tx,
    )
    .await;

    let mut saw_done = false;
    let mut saw_models = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, client_event_rx.recv())
            .await
            .expect("receive server event before timeout");
        match event.expect("channel open") {
            ServerEvent::Done { id } => {
                assert_eq!(id, 42);
                saw_done = true;
            }
            ServerEvent::AvailableModelsUpdated {
                provider_name,
                provider_model,
                available_models,
                available_model_routes,
            } => {
                saw_models = Some((provider_name, provider_model, available_models, available_model_routes));
                break;
            }
            _ => {}
        }
    }

    assert!(saw_done, "expected immediate Done ack");
    let (provider_name, provider_model, available_models, available_model_routes) =
        saw_models.expect("expected AvailableModelsUpdated event");
    assert_eq!(provider_name.as_deref(), Some("mock-auth"));
    assert_eq!(provider_model.as_deref(), Some("logged-in-model"));
    assert_eq!(
        available_models,
        vec!["logged-in-model".to_string(), "second-model".to_string()]
    );
    assert!(available_model_routes.iter().any(|route| {
        route.model == "logged-in-model" && route.provider == "MockAuth" && route.api_method == "mock-auth"
    }));

    let final_message = recv_final_catalog_notification(&mut client_event_rx).await;
    assert_eq!(final_message.lines().count(), 2);
    assert!(final_message.contains("catalog changed"));
    assert!(final_message.contains("models +2/-1"));
    assert!(final_message.contains("routes +2/-1/~0"));
    assert!(final_message.contains("**Model ready:** `logged-in-model`"));
    assert!(final_message.contains("Use `/model`"));
}

#[tokio::test]
async fn notify_auth_changed_finishes_when_provider_work_finishes_without_debounce_tail() {
    let _guard = EnvGuard::save(&[]);
    crate::bus::reset_models_updated_publish_state_for_tests();
    let provider_impl = AuthChangeMockProvider::new();
    provider_impl.state.auth_refresh_delay_ms.store(80, Ordering::Release);
    let provider: Arc<dyn Provider> = Arc::new(provider_impl);
    let registry = Registry::empty();
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
    let session_id = { agent.lock().await.session_id().to_string() };
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        "timed-session".to_string(),
        Arc::clone(&agent),
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let started = std::time::Instant::now();
    handle_notify_auth_changed(
        420,
        None,
        None,
        false,
        &provider,
        &provider,
        &sessions,
        session_id.as_str(),
        &agent,
        &client_event_tx,
    )
    .await;

    let final_message = recv_final_catalog_notification(&mut client_event_rx).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed >= std::time::Duration::from_millis(70),
        "refresh completed before provider work: {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(300),
        "refresh retained an avoidable debounce tail: {elapsed:?}"
    );
    assert_eq!(final_message.lines().count(), 2);
    assert!(final_message.contains("logged-in-model"));
}

#[tokio::test]
async fn newer_auth_refresh_supersedes_older_final_completion_for_the_same_session() {
    let _guard = EnvGuard::save(&[]);
    crate::bus::reset_models_updated_publish_state_for_tests();
    let provider_impl = AuthChangeMockProvider::new();
    provider_impl.state.auth_refresh_delay_ms.store(80, Ordering::Release);
    let provider: Arc<dyn Provider> = Arc::new(provider_impl);
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), Registry::empty())));
    let session_id = { agent.lock().await.session_id().to_string() };
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        "overlap-session".to_string(),
        Arc::clone(&agent),
    )])));
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();

    handle_notify_auth_changed(
        421,
        None,
        None,
        false,
        &provider,
        &provider,
        &sessions,
        session_id.as_str(),
        &agent,
        &first_tx,
    )
    .await;
    handle_notify_auth_changed(
        422,
        None,
        None,
        false,
        &provider,
        &provider,
        &sessions,
        session_id.as_str(),
        &agent,
        &second_tx,
    )
    .await;

    let final_message = recv_final_catalog_notification(&mut second_rx).await;
    assert!(final_message.contains("logged-in-model"));
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    while let Ok(event) = first_rx.try_recv() {
        assert!(
            !matches!(
                event,
                ServerEvent::Notification {
                    notification_type: NotificationType::Message { scope: Some(scope), .. },
                    ..
                } if scope == "catalog_activity"
            ),
            "superseded refresh emitted a second final catalog notification"
        );
    }
}

#[tokio::test]
async fn notify_auth_changed_defers_busy_session_refresh_until_idle() {
    let _guard = EnvGuard::save(&[]);
    crate::bus::reset_models_updated_publish_state_for_tests();
    let current_provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let busy_provider = Arc::new(AuthChangeMockProvider::new());
    let busy_state = Arc::clone(&busy_provider.state);
    let busy_provider: Arc<dyn Provider> = busy_provider;
    let registry = Registry::empty();
    let current_agent = Arc::new(Mutex::new(Agent::new(Arc::clone(&current_provider), registry.clone())));
    let current_session_id = { current_agent.lock().await.session_id().to_string() };
    let busy_agent = Arc::new(Mutex::new(Agent::new(busy_provider, registry)));
    let busy_guard = busy_agent.lock().await;
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        "busy-session".to_string(),
        Arc::clone(&busy_agent),
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    handle_notify_auth_changed(
        43,
        None,
        None,
        false,
        &current_provider,
        &current_provider,
        &sessions,
        current_session_id.as_str(),
        &current_agent,
        &client_event_tx,
    )
    .await;

    assert!(
        matches!(client_event_rx.recv().await, Some(ServerEvent::Done { id: 43 })),
        "expected immediate Done ack before waiting for the busy session"
    );
    assert!(
        !*busy_state.logged_in.read().unwrap(),
        "busy session provider should not refresh until its agent lock is released"
    );

    drop(busy_guard);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if *busy_state.logged_in.read().unwrap() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    panic!("busy session provider was not refreshed after it became idle");
}

#[tokio::test]
async fn onboarding_auth_refresh_prefers_global_gpt_5_6_route_over_fable() {
    let _guard = EnvGuard::save(&[
        "HORON_AGENT_RUNTIME_PROVIDER",
        "HORON_AGENT_ACTIVE_PROVIDER",
        "HORON_AGENT_INITIAL_PROVIDER_EXPLICIT",
    ]);
    crate::bus::reset_models_updated_publish_state_for_tests();

    let provider = Arc::new(AuthChangeMockProvider::new());
    *provider.state.routes_override.write().unwrap() = Some(vec![
        ModelRoute {
            model: "claude-fable-5".to_string(),
            provider: "Anthropic".to_string(),
            api_method: "claude-oauth".to_string(),
            available: true,
            detail: String::new(),
            usage: None,
            cheapness: None,
        },
        ModelRoute {
            model: "gpt-5.5".to_string(),
            provider: "OpenAI".to_string(),
            api_method: "openai-api-key".to_string(),
            available: true,
            detail: String::new(),
            usage: None,
            cheapness: None,
        },
        ModelRoute {
            model: "gpt-5.6-sol".to_string(),
            provider: "OpenAI".to_string(),
            api_method: "openai-api-key".to_string(),
            available: true,
            detail: String::new(),
            usage: None,
            cheapness: None,
        },
    ]);
    let provider: Arc<dyn Provider> = provider;
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), Registry::empty())));
    let session_id = { agent.lock().await.session_id().to_string() };
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    handle_notify_auth_changed(
        49,
        Some("openai".to_string()),
        None,
        true,
        &provider,
        &provider,
        &sessions,
        session_id.as_str(),
        &agent,
        &client_event_tx,
    )
    .await;

    recv_final_catalog_notification(&mut client_event_rx).await;

    assert_eq!(agent.lock().await.provider_model(), "gpt-5.6-sol");
}

#[tokio::test]
async fn notify_auth_changed_does_not_override_manual_model_selected_during_refresh() {
    let _guard = EnvGuard::save(&[
        "HORON_AGENT_OPENROUTER_API_BASE",
        "HORON_AGENT_OPENROUTER_API_KEY_NAME",
        "HORON_AGENT_OPENROUTER_ENV_FILE",
        "HORON_AGENT_OPENROUTER_CACHE_NAMESPACE",
        "HORON_AGENT_OPENROUTER_PROVIDER_FEATURES",
        "HORON_AGENT_OPENROUTER_TRANSPORT_STATE",
        "HORON_AGENT_OPENROUTER_MODEL_CATALOG",
        "HORON_AGENT_OPENROUTER_AUTH_HEADER",
        "HORON_AGENT_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "HORON_AGENT_OPENROUTER_MODEL",
        "HORON_AGENT_RUNTIME_PROVIDER",
        "HORON_AGENT_ACTIVE_PROVIDER",
        "HORON_AGENT_INITIAL_PROVIDER_EXPLICIT",
    ]);

    crate::bus::reset_models_updated_publish_state_for_tests();
    let provider = Arc::new(AuthChangeMockProvider::new());
    provider.state.auth_refresh_delay_ms.store(80, Ordering::Release);
    *provider.state.selected_model.write().unwrap() = Some("stale-model".to_string());
    *provider.state.route_provider.write().unwrap() = "Cerebras".to_string();
    *provider.state.route_api_method.write().unwrap() = "openai-compatible:cerebras".to_string();
    *provider.state.expose_selected_model_in_routes.write().unwrap() = false;
    let provider: Arc<dyn Provider> = provider;
    let registry = Registry::empty();
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
    let session_id = { agent.lock().await.session_id().to_string() };
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        "test-session".to_string(),
        Arc::clone(&agent),
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let mut auth = crate::protocol::AuthChanged::new("cerebras");
    auth.credential_source = Some(crate::protocol::AuthCredentialSource::ApiKeyFile);
    auth.auth_method = Some(crate::protocol::AuthMethod::RemoteTuiPasteApiKey);
    auth.expected_runtime = Some(crate::protocol::RuntimeProviderKey::new("openai-compatible"));
    auth.expected_catalog_namespace = Some(crate::protocol::CatalogNamespace::new("cerebras"));

    handle_notify_auth_changed(
        48,
        None,
        Some(auth),
        false,
        &provider,
        &provider,
        &sessions,
        session_id.as_str(),
        &agent,
        &client_event_tx,
    )
    .await;

    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Done { id: 48 })
    ));

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if matches!(
                client_event_rx.recv().await,
                Some(ServerEvent::AvailableModelsUpdated { .. })
            ) {
                break;
            }
        }
    })
    .await
    .expect("expected immediate auth model snapshot");

    {
        let mut agent_guard = agent.lock().await;
        agent_guard
            .set_model("user-picked-model")
            .expect("manual model switch should succeed");
    }

    let final_message = recv_final_catalog_notification(&mut client_event_rx).await;

    assert!(
        final_message.contains("**Model ready:** `user-picked-model`"),
        "late auth reconciliation must not override manual model selection: {}",
        final_message
    );
    assert!(
        !final_message.contains("**Model ready:** `logged-in-model`"),
        "late auth auto-selection overrode manual choice: {}",
        final_message
    );
}

#[path = "provider_control_tests/auth_selection.rs"]
mod auth_selection;
