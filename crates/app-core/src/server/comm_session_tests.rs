#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::{
    CoordinatorSpawnIdentity, ensure_spawn_coordinator_swarm, resolve_coordinator_spawn_identity,
    resolve_spawn_working_dir, resolve_stop_target_session, resolve_swarm_spawn_selection, spawn_admission_lock,
    swarm_stop_allowed_by_owner,
};
use crate::agent::Agent;
use crate::message::{Message, ToolDefinition};
use crate::protocol::{NotificationType, ServerEvent};
use crate::provider::{EventStream, Provider};
use crate::server::{SwarmMember, VersionedPlan};
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_message_types::StreamEvent;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;

struct MockProvider;

struct RoutingProvider(std::sync::Mutex<String>);

#[async_trait]
impl Provider for RoutingProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        anyhow::bail!("routing regression must not make model calls")
    }
    fn name(&self) -> &str {
        "mock"
    }
    fn model(&self) -> String {
        self.0.lock().unwrap().clone()
    }
    fn set_model(&self, model: &str) -> Result<()> {
        *self.0.lock().unwrap() = model
            .rsplit_once(':')
            .map(|(_, model)| model)
            .unwrap_or(model)
            .to_string();
        Ok(())
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self(std::sync::Mutex::new(self.model())))
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!("mock provider should not be called"))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockProvider)
    }
}

fn member(session_id: &str, swarm_id: Option<&str>, role: &str) -> (SwarmMember, mpsc::UnboundedReceiver<ServerEvent>) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    (
        SwarmMember {
            session_id: session_id.to_string(),
            event_tx,
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: swarm_id.map(|id| id.to_string()),
            swarm_enabled: true,
            status: "ready".to_string(),
            detail: None,
            friendly_name: Some(session_id.to_string()),
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: role.to_string(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
            task_label: None,
        },
        event_rx,
    )
}

async fn test_agent_with_working_dir(session_id: &str, working_dir: &str) -> Arc<Mutex<Agent>> {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("mock".to_string());
    session.working_dir = Some(working_dir.to_string());
    let mut agent = Agent::new_with_session(provider, registry, session, None);
    agent.set_working_dir(working_dir);
    Arc::new(Mutex::new(agent))
}

#[tokio::test]
async fn resolve_spawn_working_dir_prefers_explicit_then_spawner_agent_dir() {
    let sessions = Arc::new(RwLock::new(HashMap::new()));
    sessions.write().await.insert(
        "req".to_string(),
        test_agent_with_working_dir("req", "/tmp/spawner-agent").await,
    );
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));

    assert_eq!(
        resolve_spawn_working_dir(Some("/tmp/explicit".to_string()), "req", &sessions, &swarm_members,)
            .await
            .as_deref(),
        Some("/tmp/explicit")
    );
    assert_eq!(
        resolve_spawn_working_dir(None, "req", &sessions, &swarm_members)
            .await
            .as_deref(),
        Some("/tmp/spawner-agent")
    );
}

#[tokio::test]
async fn resolve_spawn_working_dir_falls_back_to_member_dir() {
    let sessions = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let (mut req_member, _rx) = member("req", Some("swarm-1"), "coordinator");
    req_member.working_dir = Some(std::path::PathBuf::from("/tmp/member-dir"));
    swarm_members.write().await.insert("req".to_string(), req_member);

    assert_eq!(
        resolve_spawn_working_dir(None, "req", &sessions, &swarm_members)
            .await
            .as_deref(),
        Some("/tmp/member-dir")
    );
}

#[test]
fn stop_permission_defaults_to_sessions_spawned_by_requesting_coordinator() {
    let (mut owned, _owned_rx) = member("worker-owned", Some("swarm-1"), "agent");
    owned.report_back_to_session_id = Some("coord".to_string());
    let (mut user_created, _user_rx) = member("worker-user", Some("swarm-1"), "agent");
    user_created.report_back_to_session_id = None;
    let (mut other_owned, _other_rx) = member("worker-other", Some("swarm-1"), "agent");
    other_owned.report_back_to_session_id = Some("other-coord".to_string());

    assert!(swarm_stop_allowed_by_owner("coord", &owned, false));
    assert!(!swarm_stop_allowed_by_owner("coord", &user_created, false));
    assert!(!swarm_stop_allowed_by_owner("coord", &other_owned, false));
    assert!(swarm_stop_allowed_by_owner("coord", &user_created, true));
}

#[tokio::test]
async fn stop_target_resolves_unique_friendly_name_and_suffix() {
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let (mut worker, _worker_rx) = member("session_jellyfish_1234_abcd", Some("swarm-1"), "agent");
    worker.friendly_name = Some("jellyfish".to_string());
    swarm_members.write().await.insert(worker.session_id.clone(), worker);

    assert_eq!(
        resolve_stop_target_session("swarm-1", "jellyfish", &swarm_members)
            .await
            .as_deref(),
        Ok("session_jellyfish_1234_abcd")
    );
    assert_eq!(
        resolve_stop_target_session("swarm-1", "abcd", &swarm_members)
            .await
            .as_deref(),
        Ok("session_jellyfish_1234_abcd")
    );
}

#[tokio::test]
async fn stop_target_rejects_ambiguous_friendly_name() {
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let (mut first, _first_rx) = member("session_bear_1", Some("swarm-1"), "agent");
    first.friendly_name = Some("bear".to_string());
    let (mut second, _second_rx) = member("session_bear_2", Some("swarm-1"), "agent");
    second.friendly_name = Some("bear".to_string());
    let mut members = swarm_members.write().await;
    members.insert(first.session_id.clone(), first);
    members.insert(second.session_id.clone(), second);
    drop(members);

    let err = resolve_stop_target_session("swarm-1", "bear", &swarm_members)
        .await
        .expect_err("ambiguous friendly names should be rejected");
    assert!(err.contains("Ambiguous swarm session 'bear'"));
}

fn coordinator_identity(
    model: Option<&str>,
    provider_key: Option<&str>,
    route_api_method: Option<&str>,
) -> CoordinatorSpawnIdentity {
    CoordinatorSpawnIdentity {
        model: model.map(str::to_string),
        provider_key: provider_key.map(str::to_string),
        route_api_method: route_api_method.map(str::to_string),
    }
}

#[test]
fn resolve_swarm_spawn_model_prefers_configured_model_over_coordinator_model() {
    let selection = resolve_swarm_spawn_selection(
        None,
        Some("openai/gpt-5.4@OpenAI".to_string()),
        &coordinator_identity(
            Some("nvidia/llama-3.3-nemotron-super-49b-v1"),
            Some("nvidia"),
            Some("openai-compatible:nvidia-nim"),
        ),
    );

    assert_eq!(selection.model.as_deref(), Some("openai/gpt-5.4@OpenAI"));
    assert_eq!(selection.provider_key.as_deref(), Some("openrouter"));
    // A different configured model must not inherit the coordinator's route.
    assert_eq!(selection.route_api_method, None);
}

#[test]
fn resolve_swarm_spawn_model_inherits_coordinator_when_unconfigured() {
    let selection = resolve_swarm_spawn_selection(
        None,
        None,
        &coordinator_identity(
            Some("nvidia/llama-3.3-nemotron-super-49b-v1"),
            Some("nvidia"),
            Some("openai-compatible:nvidia-nim"),
        ),
    );

    assert_eq!(
        selection.model.as_deref(),
        Some("nvidia/llama-3.3-nemotron-super-49b-v1")
    );
    assert_eq!(selection.provider_key.as_deref(), Some("nvidia"));
    assert_eq!(
        selection.route_api_method.as_deref(),
        Some("openai-compatible:nvidia-nim")
    );
}

#[test]
fn resolve_swarm_spawn_model_inherits_coordinator_auth_route_for_oauth_vs_api() {
    // Regression: a coordinator on the Claude API route must spawn agents on
    // the same API route, not Claude OAuth (the config default).
    let selection = resolve_swarm_spawn_selection(
        None,
        None,
        &coordinator_identity(Some("claude-opus-4-6"), Some("claude-api"), Some("claude-api")),
    );

    assert_eq!(selection.model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(selection.provider_key.as_deref(), Some("claude-api"));
    assert_eq!(selection.route_api_method.as_deref(), Some("claude-api"));
}

#[test]
fn resolve_swarm_spawn_model_keeps_provider_key_when_config_matches_coordinator() {
    let selection = resolve_swarm_spawn_selection(
        None,
        Some("custom-model".to_string()),
        &coordinator_identity(Some("custom-model"), Some("custom-provider"), Some("custom-route")),
    );

    assert_eq!(selection.model.as_deref(), Some("custom-model"));
    assert_eq!(selection.provider_key.as_deref(), Some("custom-provider"));
    assert_eq!(selection.route_api_method.as_deref(), Some("custom-route"));
}

#[test]
fn resolve_swarm_spawn_model_openai_api_prefix_pins_api_route_over_coordinator() {
    // `agents.swarm_model = "openai-api:gpt-5.5"` must spawn agents on GPT-5.5
    // via the OpenAI API key route, regardless of the coordinator's model/auth.
    let selection = resolve_swarm_spawn_selection(
        None,
        Some("openai-api:gpt-5.5".to_string()),
        &coordinator_identity(Some("claude-opus-4-8"), Some("claude-oauth"), Some("claude-oauth")),
    );

    assert_eq!(selection.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(selection.provider_key.as_deref(), Some("openai-api-key"));
    assert_eq!(selection.route_api_method.as_deref(), Some("openai-api-key"));
}

#[test]
fn resolve_swarm_spawn_model_inherit_sentinel_uses_coordinator_model() {
    for sentinel in ["inherit", "INHERIT", "coordinator", " inherit ", ""] {
        let selection = resolve_swarm_spawn_selection(
            None,
            Some(sentinel.to_string()),
            &coordinator_identity(
                Some("nvidia/llama-3.3-nemotron-super-49b-v1"),
                Some("nvidia"),
                Some("openai-compatible:nvidia-nim"),
            ),
        );

        assert_eq!(
            selection.model.as_deref(),
            Some("nvidia/llama-3.3-nemotron-super-49b-v1"),
            "sentinel {sentinel:?} should inherit coordinator model",
        );
        assert_eq!(
            selection.provider_key.as_deref(),
            Some("nvidia"),
            "sentinel {sentinel:?} should inherit coordinator provider key",
        );
        assert_eq!(
            selection.route_api_method.as_deref(),
            Some("openai-compatible:nvidia-nim"),
            "sentinel {sentinel:?} should inherit coordinator auth route",
        );
    }
}

#[test]
fn resolve_swarm_spawn_model_requested_model_overrides_configured_pin() {
    for requested in ["openai-api:gpt-5.5", "  openai-api:gpt-5.5 \t"] {
        let selection = resolve_swarm_spawn_selection(
            Some(requested.to_string()),
            Some("claude-oauth:claude-opus-4-8".to_string()),
            &coordinator_identity(Some("claude-fable-5"), Some("claude-oauth"), Some("claude-oauth")),
        );

        assert_eq!(selection.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(selection.provider_key.as_deref(), Some("openai-api-key"));
        assert_eq!(selection.route_api_method.as_deref(), Some("openai-api-key"));
    }
}

#[test]
fn resolve_swarm_spawn_model_requested_inherit_overrides_configured_pin() {
    for requested in ["inherit", "INHERIT", "coordinator", " COORDINATOR ", " inherit "] {
        let selection = resolve_swarm_spawn_selection(
            Some(requested.to_string()),
            Some("openai-api:gpt-5.5".to_string()),
            &coordinator_identity(Some("claude-fable-5"), Some("claude-api"), Some("claude-api")),
        );

        assert_eq!(selection.model.as_deref(), Some("claude-fable-5"));
        assert_eq!(selection.provider_key.as_deref(), Some("claude-api"));
        assert_eq!(selection.route_api_method.as_deref(), Some("claude-api"));
    }
}

#[test]
fn resolve_swarm_spawn_model_requested_matching_coordinator_model_keeps_route() {
    let selection = resolve_swarm_spawn_selection(
        Some(" custom-model ".to_string()),
        Some("openai-api:gpt-5.5".to_string()),
        &coordinator_identity(Some("custom-model"), Some("custom-provider"), Some("custom-route")),
    );

    assert_eq!(selection.model.as_deref(), Some("custom-model"));
    assert_eq!(selection.provider_key.as_deref(), Some("custom-provider"));
    assert_eq!(selection.route_api_method.as_deref(), Some("custom-route"));
}

#[test]
fn resolve_swarm_spawn_model_blank_requested_model_falls_back_to_config() {
    for requested in ["", "   ", "\t\n"] {
        let selection = resolve_swarm_spawn_selection(
            Some(requested.to_string()),
            Some("openai-api:gpt-5.5".to_string()),
            &coordinator_identity(Some("claude-fable-5"), Some("claude-oauth"), Some("claude-oauth")),
        );

        assert_eq!(selection.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(selection.provider_key.as_deref(), Some("openai-api-key"));
        assert_eq!(selection.route_api_method.as_deref(), Some("openai-api-key"));
    }
}

#[test]
fn resolve_swarm_spawn_model_omitted_request_trims_configured_model() {
    let selection = resolve_swarm_spawn_selection(
        None,
        Some(" \topenai-api:gpt-5.5 \n".to_string()),
        &coordinator_identity(Some("claude-fable-5"), Some("claude-oauth"), Some("claude-oauth")),
    );

    assert_eq!(selection.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(selection.provider_key.as_deref(), Some("openai-api-key"));
    assert_eq!(selection.route_api_method.as_deref(), Some("openai-api-key"));
}

#[test]
fn resolve_swarm_spawn_model_blank_requested_model_inherits_when_unconfigured() {
    let selection = resolve_swarm_spawn_selection(
        Some(" \t\n".to_string()),
        None,
        &coordinator_identity(Some("custom-model"), Some("custom-provider"), Some("custom-route")),
    );

    assert_eq!(selection.model.as_deref(), Some("custom-model"));
    assert_eq!(selection.provider_key.as_deref(), Some("custom-provider"));
    assert_eq!(selection.route_api_method.as_deref(), Some("custom-route"));
}

#[tokio::test]
async fn coordinator_identity_uses_live_agent_when_lock_is_available() {
    let agent = test_agent_with_working_dir("coord", "/tmp/coord").await;
    let live_model = agent.lock().await.provider_model();
    let sessions = Arc::new(RwLock::new(HashMap::new()));
    sessions.write().await.insert("coord".to_string(), Arc::clone(&agent));

    let identity = resolve_coordinator_spawn_identity("coord", &sessions).await;
    assert_eq!(identity.model.as_deref(), Some(live_model.as_str()));
}

#[tokio::test]
async fn coordinator_identity_falls_back_to_persisted_session_when_agent_busy() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("HORON_AGENT_HOME", temp_home.path());

    let agent = test_agent_with_working_dir("coord_busy", "/tmp/coord").await;

    // Persist a coordinator session that records a concrete model + auth route.
    // Persist after the agent is built so it reflects the authoritative on-disk
    // snapshot the spawn path will read when the agent lock is unavailable.
    let mut session = crate::session::Session::create_with_id("coord_busy".to_string(), None, None);
    session.model = Some("claude-opus-4-6".to_string());
    session.provider_key = Some("claude-api".to_string());
    session.route_api_method = Some("claude-api".to_string());
    session.save_prepared().expect("persist coordinator session");

    // Hold the agent lock to simulate a coordinator mid-turn: the spawn path
    // must not block and must read the persisted identity instead of defaults.
    let _held = agent.lock().await;
    let sessions = Arc::new(RwLock::new(HashMap::new()));
    sessions
        .write()
        .await
        .insert("coord_busy".to_string(), Arc::clone(&agent));

    let identity = resolve_coordinator_spawn_identity("coord_busy", &sessions).await;
    assert_eq!(identity.model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(identity.provider_key.as_deref(), Some("claude-api"));
    assert_eq!(identity.route_api_method.as_deref(), Some("claude-api"));

    crate::env::remove_var("HORON_AGENT_HOME");
}

#[tokio::test]
async fn session_agent_models_drive_spawn_selection() {
    let _env = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let previous = std::env::var_os("HORON_AGENT_HOME");
    crate::env::set_var("HORON_AGENT_HOME", temp.path());
    let agent = test_agent_with_working_dir("routing_busy", "/tmp/coord").await;
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        "routing_busy".to_string(),
        agent.clone(),
    )])));
    agent
        .lock()
        .await
        .set_agent_model_override("swarm", Some("openai-api:gpt-5.5".into()))
        .unwrap();
    let coordinator = resolve_coordinator_spawn_identity("routing_busy", &sessions).await;
    let configured = super::session_swarm_model("routing_busy", &sessions, Some("global-model".into())).await;
    let selection = resolve_swarm_spawn_selection(None, configured, &coordinator);
    assert_eq!(selection.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(selection.provider_key.as_deref(), Some("openai-api-key"));
    let provider: Arc<dyn Provider> = Arc::new(RoutingProvider(std::sync::Mutex::new("initial-model".into())));
    let state = crate::server::SwarmState::new(HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
    let mcp = Arc::new(crate::mcp::SharedMcpPool::new(crate::mcp::McpConfig::default()));
    let child_id = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        super::spawn_swarm_agent(
            "routing_busy",
            "routing-swarm",
            Some(temp.path().to_string_lossy().into_owned()),
            None,
            Some(crate::config::SwarmSpawnMode::Headless),
            None,
            None,
            Some("routing test".into()),
            super::comm_worktree::Isolation::None,
            super::comm_worktree::IsoApply::Apply,
            None,
            &sessions,
            &Arc::new(RwLock::new(String::new())),
            &provider,
            &state.members,
            &state.swarms_by_id,
            &state.coordinators,
            &state.plans,
            &Arc::new(RwLock::new(VecDeque::new())),
            &Arc::new(AtomicU64::new(0)),
            &broadcast::channel(16).0,
            &mcp,
            &Arc::new(RwLock::new(HashMap::new())),
        ),
    )
    .await
    .expect("actual spawn must not await coordinator lock")
    .unwrap();
    let child = sessions.read().await.get(&child_id).cloned().unwrap();
    let child = child.lock().await;
    assert_eq!(child.provider_model(), "gpt-5.5");
    assert_eq!(child.session_provider_key().as_deref(), Some("openai-api"));
    drop(child);
    agent
        .lock()
        .await
        .set_agent_model_override("swarm", Some("inherit".into()))
        .unwrap();
    let configured = super::session_swarm_model("routing_busy", &sessions, Some("global-model".into())).await;
    let selection = resolve_swarm_spawn_selection(None, configured, &coordinator);
    let own_model = agent.lock().await.provider_model();
    assert_eq!(selection.model.as_deref(), Some(own_model.as_str()));
    agent.lock().await.set_agent_model_override("swarm", None).unwrap();
    assert_eq!(
        super::session_swarm_model("routing_busy", &sessions, Some("global-model".into()))
            .await
            .as_deref(),
        Some("global-model")
    );
    if let Some(previous) = previous {
        crate::env::set_var("HORON_AGENT_HOME", previous);
    } else {
        crate::env::remove_var("HORON_AGENT_HOME");
    }
}

#[tokio::test]
async fn spawn_bootstraps_coordinator_when_swarm_has_none() {
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        "swarm-1".to_string(),
        HashSet::from(["req".to_string()]),
    )])));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    let (req_member, _req_rx) = member("req", Some("swarm-1"), "agent");
    swarm_members.write().await.insert("req".to_string(), req_member);
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let swarm_id = ensure_spawn_coordinator_swarm(
        1,
        "req",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        32,
    )
    .await;

    assert_eq!(swarm_id.as_deref(), Some("swarm-1"));
    assert_eq!(
        swarm_coordinators.read().await.get("swarm-1").map(String::as_str),
        Some("req")
    );
    assert_eq!(
        swarm_members.read().await.get("req").map(|member| member.role.as_str()),
        Some("coordinator")
    );
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Notification {
            notification_type: NotificationType::Message { .. },
            message,
            ..
        }) if message == "You are the coordinator for this swarm."
    ));
}

#[tokio::test]
async fn nested_agent_cannot_spawn_when_root_is_light_or_normal() {
    // Both explicit light-swarm effort and ordinary ad hoc swarm use are
    // one-level fan-out. A spawned child cannot grow another generation.
    for (root_id, effort) in [
        ("light-root-no-recursion", Some("swarm")),
        ("normal-root-no-recursion", None),
    ] {
        crate::session_effort::forget_session_effort(root_id);
        crate::session_effort::record_session_effort(root_id, effort);
        let swarm_id = format!("swarm-{root_id}");
        let child_id = format!("child-{root_id}");
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
            swarm_id.clone(),
            HashSet::from([child_id.clone(), root_id.to_string()]),
        )])));
        let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(swarm_id.clone(), root_id.to_string())])));
        let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
        let (mut child_member, _child_rx) = member(&child_id, Some(&swarm_id), "agent");
        child_member.report_back_to_session_id = Some(root_id.to_string());
        let (root_member, _root_rx) = member(root_id, Some(&swarm_id), "coordinator");
        let mut members = swarm_members.write().await;
        members.insert(child_id.clone(), child_member);
        members.insert(root_id.to_string(), root_member);
        drop(members);
        let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

        let refused = ensure_spawn_coordinator_swarm(
            2,
            &child_id,
            &client_event_tx,
            &swarm_members,
            &swarms_by_id,
            &swarm_coordinators,
            &swarm_plans,
            32,
        )
        .await;

        crate::session_effort::forget_session_effort(root_id);
        assert!(refused.is_none());
        assert_eq!(
            swarm_coordinators.read().await.get(&swarm_id).map(String::as_str),
            Some(root_id)
        );
        assert_eq!(
            swarm_members
                .read()
                .await
                .get(&child_id)
                .map(|member| member.role.as_str()),
            Some("agent")
        );
        assert!(matches!(
            client_event_rx.recv().await,
            Some(ServerEvent::Error { message, .. })
                if message.contains("Recursive swarm spawning is disabled")
                    && message.contains(&format!("Only the root session ({root_id}) may spawn agents"))
        ));
    }
}

#[tokio::test]
async fn nested_agent_can_spawn_when_root_is_deep() {
    let root_id = "deep-root-recursive";
    crate::session_effort::record_session_effort(root_id, Some("swarm-deep"));

    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        "swarm-deep".to_string(),
        HashSet::from(["deep-child".to_string(), root_id.to_string()]),
    )])));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-deep".to_string(),
        root_id.to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    let (mut child_member, _child_rx) = member("deep-child", Some("swarm-deep"), "agent");
    child_member.report_back_to_session_id = Some(root_id.to_string());
    let (root_member, _root_rx) = member(root_id, Some("swarm-deep"), "coordinator");
    let mut members = swarm_members.write().await;
    members.insert("deep-child".to_string(), child_member);
    members.insert(root_id.to_string(), root_member);
    drop(members);
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let allowed = ensure_spawn_coordinator_swarm(
        3,
        "deep-child",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        32,
    )
    .await;

    crate::session_effort::forget_session_effort(root_id);
    assert_eq!(allowed.as_deref(), Some("swarm-deep"));
    assert!(client_event_rx.try_recv().is_err());
}

#[tokio::test]
async fn spawn_allowed_at_arbitrary_depth_without_depth_cap() {
    // Deep-swarm mode still allows recursive decomposition at arbitrary depth.
    let root_id = "deep-root-arbitrary-depth";
    crate::session_effort::record_session_effort(root_id, Some("swarm-deep"));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-1".to_string(),
        root_id.to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    {
        let mut members = swarm_members.write().await;
        let (root, _rx) = member(root_id, Some("swarm-1"), "coordinator");
        members.insert(root_id.to_string(), root);
        let chain = [
            ("a", root_id),
            ("b", "a"),
            ("c", "b"),
            ("d", "c"),
            ("e", "d"),
            ("f", "e"),
        ];
        for (id, parent) in chain {
            let (mut m, _rx) = member(id, Some("swarm-1"), "agent");
            m.report_back_to_session_id = Some(parent.to_string());
            members.insert(id.to_string(), m);
        }
    }
    let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel();

    // `f` is deeply nested but the swarm is far below the member cap, so spawning
    // is allowed.
    let allowed = ensure_spawn_coordinator_swarm(
        7,
        "f",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        32,
    )
    .await;
    crate::session_effort::forget_session_effort(root_id);
    assert_eq!(allowed.as_deref(), Some("swarm-1"));
}

#[tokio::test]
async fn spawn_rejected_when_member_limit_reached() {
    use crate::server::swarm::MAX_SWARM_MEMBERS;

    // Fill the swarm to the member cap; the next spawn must be refused.
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-1".to_string(),
        "root".to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    {
        let mut members = swarm_members.write().await;
        let (root, _rx) = member("root", Some("swarm-1"), "coordinator");
        members.insert("root".to_string(), root);
        // Add filler members so the swarm holds exactly MAX_SWARM_MEMBERS total.
        for idx in 1..MAX_SWARM_MEMBERS {
            let id = format!("agent-{idx}");
            let (mut m, _rx) = member(&id, Some("swarm-1"), "agent");
            m.report_back_to_session_id = Some("root".to_string());
            members.insert(id, m);
        }
    }
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let refused = ensure_spawn_coordinator_swarm(
        7,
        "root",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        0,
    )
    .await;
    assert!(refused.is_none());
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Error { message, .. })
            if message.contains("Swarm member limit reached")
    ));
}

#[tokio::test]
async fn terminal_members_do_not_consume_spawn_capacity() {
    use crate::server::swarm::MAX_SWARM_MEMBERS;

    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-1".to_string(),
        "root".to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    {
        let mut members = swarm_members.write().await;
        let (root, _rx) = member("root", Some("swarm-1"), "coordinator");
        members.insert("root".to_string(), root);
        for idx in 0..MAX_SWARM_MEMBERS {
            let id = format!("historical-{idx}");
            let (mut historical, _rx) = member(&id, Some("swarm-1"), "agent");
            historical.status = if idx % 2 == 0 {
                "completed".to_string()
            } else {
                "stopped".to_string()
            };
            historical.latest_completion_report = Some(format!("report {idx}"));
            historical.report_back_to_session_id = Some("root".to_string());
            members.insert(id, historical);
        }
    }
    let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel();

    let allowed = ensure_spawn_coordinator_swarm(
        7,
        "root",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        32,
    )
    .await;

    assert_eq!(allowed.as_deref(), Some("swarm-1"));
}

#[tokio::test]
async fn spawn_rejected_at_configured_live_agent_limit() {
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-1".to_string(),
        "root".to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    {
        let mut members = swarm_members.write().await;
        let (root, _rx) = member("root", Some("swarm-1"), "coordinator");
        members.insert("root".to_string(), root);
        for idx in 0..2 {
            let id = format!("agent-{idx}");
            let (mut worker, _rx) = member(&id, Some("swarm-1"), "agent");
            worker.report_back_to_session_id = Some("root".to_string());
            members.insert(id, worker);
        }
    }
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let refused = ensure_spawn_coordinator_swarm(
        7,
        "root",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        2,
    )
    .await;

    assert!(refused.is_none());
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Error { message, .. })
            if message.contains("Swarm live-agent limit reached (max 2")
    ));
}

#[tokio::test]
async fn spawn_admission_lock_serializes_per_swarm_only() {
    use std::time::Duration;

    let key = format!("lock-test-{}", std::process::id());
    let same_a = spawn_admission_lock(&key);
    let same_b = spawn_admission_lock(&key);
    let other = spawn_admission_lock(&format!("{key}-other"));

    let held = same_a.lock().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(10), same_b.lock())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), other.lock())
            .await
            .is_ok()
    );
    drop(held);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), same_b.lock())
            .await
            .is_ok()
    );
}

#[test]
fn swarm_spawn_effort_prefers_explicit_then_config_pin_then_inherit() {
    use super::resolve_swarm_spawn_effort;

    // Explicit spawn argument wins over the config pin (#1165).
    assert_eq!(
        resolve_swarm_spawn_effort(Some("low"), Some("medium")),
        Some("low".to_string())
    );
    // A missing or blank spawn argument falls back to `agents.swarm_effort`.
    assert_eq!(
        resolve_swarm_spawn_effort(None, Some("medium")),
        Some("medium".to_string())
    );
    assert_eq!(
        resolve_swarm_spawn_effort(Some("  "), Some(" medium ")),
        Some("medium".to_string())
    );
    // With neither, the worker inherits the provider-wide effort.
    assert_eq!(resolve_swarm_spawn_effort(None, None), None);
    assert_eq!(resolve_swarm_spawn_effort(Some(""), Some("")), None);
}

#[test]
fn per_call_auto_spawn_mode_defers_to_configured_mode() {
    use super::resolve_swarm_spawn_mode;
    use crate::config::SwarmSpawnMode::{Auto, Headless, Inline};

    // A model passing `auto` expresses no preference, so the configured mode
    // decides. Every mode runs the worker in this process.
    assert_eq!(resolve_swarm_spawn_mode(Some(Auto), Inline), Inline);
    assert_eq!(resolve_swarm_spawn_mode(Some(Auto), Headless), Headless);
    assert_eq!(resolve_swarm_spawn_mode(None, Inline), Inline);
    // A configured `auto` stays `auto` when nothing overrides it.
    assert_eq!(resolve_swarm_spawn_mode(Some(Auto), Auto), Auto);
    assert_eq!(resolve_swarm_spawn_mode(None, Auto), Auto);
    // Explicit concrete modes still win over config.
    assert_eq!(resolve_swarm_spawn_mode(Some(Headless), Inline), Headless);
    assert_eq!(resolve_swarm_spawn_mode(Some(Inline), Headless), Inline);
}
/// Provider whose first request is a scripted stream and whose later requests
/// answer with plain text, so one child turn runs exactly one tool round.
///
/// `complete_split`'s default carries the dynamic system prompt on the message
/// list rather than in `system`, so a request is recorded as the static system
/// part followed by the serialized messages.
struct YieldScriptProvider {
    rounds: Arc<std::sync::Mutex<Option<mpsc::Receiver<Result<StreamEvent>>>>>,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for YieldScriptProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
        system: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        // Render text blocks instead of the raw JSON: the JSON escapes quotes,
        // so a literal assertion on `{"data": …}` would never match.
        let mut request = system.to_string();
        for message in messages {
            for block in &message.content {
                if let crate::message::ContentBlock::Text { text, .. } = block {
                    request.push('\n');
                    request.push_str(text);
                }
            }
        }
        self.requests.lock().unwrap().push(request);
        if let Some(rx) = self.rounds.lock().unwrap().take() {
            Ok(Box::pin(ReceiverStream::new(rx)))
        } else {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::TextDelta("done".into())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                }),
            ])))
        }
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn model(&self) -> String {
        "mock".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            rounds: Arc::clone(&self.rounds),
            requests: Arc::clone(&self.requests),
        })
    }
}

#[tokio::test]
async fn schema_spawn_installs_yield_on_the_child_only() {
    let _env = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let previous = std::env::var_os("HORON_AGENT_HOME");
    crate::env::set_var("HORON_AGENT_HOME", temp.path());

    let (tx, rx) = mpsc::channel(64);
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(YieldScriptProvider {
        rounds: Arc::new(std::sync::Mutex::new(Some(rx))),
        requests: Arc::clone(&requests),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut session = crate::session::Session::create_with_id("yield_coord".to_string(), None, None);
    session.model = Some("mock".to_string());
    session.working_dir = Some(temp.path().to_string_lossy().into_owned());
    let agent = Agent::new_with_session(provider.clone(), registry, session, None);
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        "yield_coord".to_string(),
        Arc::new(Mutex::new(agent)),
    )])));
    let state = crate::server::SwarmState::new(HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
    let mcp = Arc::new(crate::mcp::SharedMcpPool::new(crate::mcp::McpConfig::default()));
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "files": { "type": "array", "items": { "type": "string" } } },
        "required": ["files"]
    });

    let child_id = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        super::spawn_swarm_agent(
            "yield_coord",
            "yield-swarm",
            Some(temp.path().to_string_lossy().into_owned()),
            Some("produce files".to_string()),
            Some(crate::config::SwarmSpawnMode::Headless),
            None,
            None,
            Some("structured child".into()),
            super::comm_worktree::Isolation::None,
            super::comm_worktree::IsoApply::Apply,
            Some(schema),
            &sessions,
            &Arc::new(RwLock::new(String::new())),
            &provider,
            &state.members,
            &state.swarms_by_id,
            &state.coordinators,
            &state.plans,
            &Arc::new(RwLock::new(VecDeque::new())),
            &Arc::new(AtomicU64::new(0)),
            &broadcast::channel(16).0,
            &mcp,
            &Arc::new(RwLock::new(HashMap::new())),
        ),
    )
    .await
    .expect("the spawn itself must not block")
    .expect("the schema spawn succeeds");

    let child = sessions.read().await.get(&child_id).cloned().unwrap();
    let parent = sessions.read().await.get("yield_coord").cloned().unwrap();

    assert!(
        !parent
            .lock()
            .await
            .registry()
            .tool_names()
            .await
            .iter()
            .any(|name| name == crate::tool::yield_result::YIELD_TOOL),
        "a child-only tool must never reach the coordinator's registry"
    );
    assert!(
        child
            .lock()
            .await
            .registry()
            .tool_names()
            .await
            .iter()
            .any(|name| name == crate::tool::yield_result::YIELD_TOOL),
        "the child carries the yield tool"
    );
    {
        let child_guard = child.lock().await;
        let advertised = child_guard.tool_definitions_for_debug().await;
        assert!(
            advertised
                .iter()
                .any(|definition| definition.name == crate::tool::yield_result::YIELD_TOOL),
            "yield must be advertised to the child, not merely registered"
        );
    }

    // One child turn: the scripted provider asks for `yield`, then answers with
    // the default closing text.
    tx.send(Ok(StreamEvent::ToolUseStart {
        id: "call-yield".into(),
        name: crate::tool::yield_result::YIELD_TOOL.into(),
    }))
    .await
    .unwrap();
    tx.send(Ok(StreamEvent::ToolInputDeltaFor {
        id: "call-yield".into(),
        delta: r#"{"data":{"files":["a"]}}"#.into(),
    }))
    .await
    .unwrap();
    tx.send(Ok(StreamEvent::ToolUseEndFor {
        id: "call-yield".into(),
    }))
    .await
    .unwrap();
    tx.send(Ok(StreamEvent::MessageEnd {
        stop_reason: Some("tool_use".into()),
    }))
    .await
    .unwrap();
    // Close the scripted round: an open channel would leave the turn waiting
    // for more events after MessageEnd instead of finishing the round.
    drop(tx);

    let output = {
        let mut child_guard = child.lock().await;
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            child_guard.run_once_capture("produce files"),
        )
        .await
        .expect("the child turn finishes")
        .expect("the child turn succeeds")
    };
    assert_eq!(output, "done");

    let first_request = requests.lock().unwrap().first().cloned().unwrap_or_default();
    assert!(
        first_request.contains(crate::tool::yield_result::YIELD_INSTRUCTION),
        "the child is told to call yield: {first_request}"
    );

    crate::tool::yield_result::publish_completion(&child_id, "yield_coord", &output);

    let parent_registry = parent.lock().await.registry();
    let ctx = crate::tool::ToolContext {
        session_id: "yield_coord".to_string(),
        message_id: "yield-read".to_string(),
        tool_call_id: "yield-read".to_string(),
        working_dir: Some(temp.path().to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    };
    let projected = parent_registry
        .execute(
            "read",
            serde_json::json!({"intent": "qa", "file_path": format!("agent://{child_id}/files/0")}),
            ctx,
        )
        .await
        .expect("the structured field is readable by the coordinator");
    assert_eq!(projected.output, "a");

    if let Some(previous) = previous {
        crate::env::set_var("HORON_AGENT_HOME", previous);
    } else {
        crate::env::remove_var("HORON_AGENT_HOME");
    }
}
