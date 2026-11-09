//! End-to-end coverage for todo 13 of the autonomous approval and failover plan.
//!
//! These tests assert observable effects (a persisted wait record, a recorded
//! resume marker in the session's event log, a stored transcript, a route that
//! actually moved) rather than the text a queued mock response returned. A tool
//! call rejected by the allow-list still advances the turn loop, so a
//! response-text assertion would pass with the feature dead.

use crate::test_support::*;
use async_trait::async_trait;
use horon_agent::ambient::{BlockedApproval, BlockedApprovalStore};
use horon_agent::ambient_runner::AmbientRunnerHandle;
use horon_agent::config::Config;
use horon_agent::message::{Message, ToolDefinition};
use horon_agent::provider::{EventStream, FailoverRequired, ModelRoute, ProviderFailoverPrompt, RouteSelection};
use horon_agent::safety::{SafetySystem, record_permission_via_file};
use horon_agent::session::{Session, SessionStatus};
use horon_agent::tool::ambient::init_safety_system;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

const TARGET_ROUTE: &str = "openai-api:gpt-5.4";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn isolated_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HORON_HOME").expect("setup_test_env sets HORON_HOME"))
}

fn write_config(body: &str) {
    std::fs::write(isolated_home().join("config.toml"), body).expect("write config.toml");
    Config::invalidate_cache();
}

fn api_key_route(model: &str) -> ModelRoute {
    ModelRoute {
        model: model.to_string(),
        provider: "OpenAI".to_string(),
        api_method: "openai-api".to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
        usage: None,
    }
}

fn failover_prompt() -> ProviderFailoverPrompt {
    ProviderFailoverPrompt {
        from_provider: "claude".to_string(),
        from_label: "Claude (work)".to_string(),
        to_provider: "openai".to_string(),
        to_label: "OpenAI API key".to_string(),
        target_route: Some(TARGET_ROUTE.to_string()),
        reason: "429 Too Many Requests".to_string(),
        estimated_input_chars: 1200,
        estimated_input_tokens: 300,
    }
}

fn failover_error() -> anyhow::Error {
    anyhow::Error::new(FailoverRequired::new(failover_prompt()))
}

fn text_response(text: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(text.to_string()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        },
    ]
}

fn pending_failover_events() -> Vec<Value> {
    let path = isolated_home().join("failover-events.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn failover_events_for(session_id: &str) -> Vec<Value> {
    pending_failover_events()
        .into_iter()
        .filter(|event| event.get("session_id").and_then(Value::as_str) == Some(session_id))
        .collect()
}

fn budget_file(session_id: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    isolated_home().join("failover-budget").join(format!("{safe}.json"))
}

fn read_json_file(path: &Path) -> Value {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// Bounded wait on a durable state change. It fails at the deadline instead of
/// passing on a timing coincidence, and the state it waits on is the assertion.
async fn wait_for(mut check: impl FnMut() -> bool, deadline: Duration, what: &str) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if check() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("timed out after {deadline:?} waiting for {what}")
}

/// Whether the session's own event log records a resume turn for `request_id`.
///
/// The marker is a `MetadataChanged` event, so this reads the same durable
/// original the session loads from. Counting it here is what makes the
/// assertion below fail when the marker is not recorded at all, which a
/// transcript-only check would miss.
fn resume_marker_recorded(session_id: &str, request_id: &str) -> bool {
    Session::journal_has_resume_key(session_id, request_id).expect("the event log is readable")
}

/// Uses and results for the call a retry re-runs, counted across both ids.
///
/// The interrupted call keeps its original id in the transcript, and the re-run
/// carries the fresh id the provider minted for it, so one logical call spans
/// two ids once a retry has run. Counting only the original id would report the
/// re-run as absent.
fn named_call_counts(session_id: &str) -> (usize, usize) {
    let (uses, results) = tool_calls_for(session_id, RETRY_CALL);
    let (rerun_uses, rerun_results) = tool_calls_for(session_id, RETRY_RERUN_CALL);
    (uses + rerun_uses, results + rerun_results)
}

fn resume_prompts(session_id: &str) -> usize {
    Session::load(session_id)
        .map(|session| {
            session
                .active_context()
                .iter()
                .filter(|message| {
                    message.content.iter().any(
                        |block| matches!(block, ContentBlock::Text { text, .. } if text.starts_with("[Resume after approval]")),
                    )
                })
                .count()
        })
        .unwrap_or(0)
}

fn text_block_count(session_id: &str, needle: &str) -> usize {
    Session::load(session_id)
        .map(|session| {
            session
                .active_context()
                .iter()
                .flat_map(|message| message.content.iter())
                .filter(|block| matches!(block, ContentBlock::Text { text, .. } if text == needle))
                .count()
        })
        .unwrap_or(0)
}

/// The stored result text of one tool call, or `None` when it produced none.
///
/// A call whose wait never blocked still records a result (the "queued"
/// text), so this text separates a real decision from the queue path; the
/// model's reply text does not, because the loop advances either way.
fn tool_result_text(session_id: &str, tool_call_id: &str) -> Option<String> {
    let session = Session::load(session_id).ok()?;
    session
        .active_context()
        .iter()
        .flat_map(|message| message.content.iter())
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id, content, ..
            } if tool_use_id == tool_call_id => Some(content.clone()),
            _ => None,
        })
}

fn tool_calls_for(session_id: &str, tool_call_id: &str) -> (usize, usize) {
    let Ok(session) = Session::load(session_id) else {
        return (0, 0);
    };
    let mut uses = 0;
    let mut results = 0;
    for message in session.active_context() {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } if id == tool_call_id => uses += 1,
                ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == tool_call_id => results += 1,
                _ => {}
            }
        }
    }
    (uses, results)
}

/// The scenario owns its own runtime so the ambient runner loop it starts is
/// aborted when the scenario ends. That loop never returns on its own, and a
/// leaked one would read whichever `HORON_HOME` a later test installed.
fn on_dedicated_runtime<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("scenario runtime");
        runtime.block_on(future)
    })
    .join()
    .expect("scenario thread panicked")
}

// ---------------------------------------------------------------------------
// A. clientless approval: wait -> external decision -> exactly one resume
// ---------------------------------------------------------------------------

static PERMISSION_NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();
static PERMISSION_SEEN: Mutex<Option<(String, String)>> = Mutex::new(None);

fn permission_notifier(action: &str, _description: &str, request_id: &str) {
    if let Ok(mut slot) = PERMISSION_SEEN.lock() {
        *slot = Some((action.to_string(), request_id.to_string()));
    }
    if let Some(notify) = PERMISSION_NOTIFY.get() {
        notify.notify_one();
    }
}

fn request_permission_tool_use(call_id: &str) -> Vec<StreamEvent> {
    let input = json!({
        "action": "create_pull_request",
        "description": "Open the release PR",
        "rationale": "The release branch is ready",
        "urgency": "normal",
        "wait": true
    })
    .to_string();
    vec![
        StreamEvent::ToolUseStart {
            id: call_id.to_string(),
            name: "request_permission".to_string(),
        },
        StreamEvent::ToolInputDelta(input),
        StreamEvent::ToolUseEnd,
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".to_string()),
        },
    ]
}

#[test]
fn scenario_a_clientless_approval_resumes_once() {
    let _env = setup_test_env().expect("isolated home");
    let outcome: Result<()> = on_dedicated_runtime(async {
        write_config(
            "[tools]\nprofile = \"core\"\n\n[ambient]\napproval_for_clientless_runs = true\napproval_timeout_secs = 60\n",
        );

        let provider = Arc::new(MockProvider::new());
        provider.queue_response(request_permission_tool_use("call_perm_1"));
        provider.queue_response(text_response("Approved and continued."));
        provider.queue_response(text_response("Resumed after approval."));
        let provider_dyn: Arc<dyn Provider> = provider.clone();

        let registry = Registry::new(provider_dyn.clone()).await;
        let mut agent = Agent::new(provider_dyn.clone(), registry);

        let visible = agent.tool_definitions_for_debug().await;
        assert!(
            visible.iter().any(|definition| definition.name == "request_permission"),
            "a clientless run must see request_permission in its model-visible tool list: {:?}",
            visible
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>()
        );

        let session_id = agent.session_id().to_string();
        let runner = AmbientRunnerHandle::new(Arc::new(SafetySystem::new()));
        init_safety_system(runner.safety().clone());
        horon_agent::safety::register_permission_notifier(permission_notifier);
        if let Ok(mut slot) = PERMISSION_SEEN.lock() {
            *slot = None;
        }
        let notify = PERMISSION_NOTIFY.get_or_init(tokio::sync::Notify::new);
        let notified = notify.notified();
        tokio::pin!(notified);
        assert!(runner.start(provider_dyn).await, "the ambient runner loop must start");

        let turn = tokio::spawn(async move {
            let response = agent.run_once_capture("Request permission").await;
            agent.mark_closed();
            response
        });

        timeout(Duration::from_secs(20), notified)
            .await
            .map_err(|_| anyhow::anyhow!("the tool never announced its permission request"))?;
        let (action, request_id) = PERMISSION_SEEN
            .lock()
            .unwrap()
            .clone()
            .expect("the notifier records the request");

        let queue = read_json_file(&isolated_home().join("safety").join("queue.json"));
        let entry = queue
            .as_array()
            .and_then(|entries| entries.iter().find(|entry| entry["id"] == request_id.as_str()))
            .expect("the pending request is persisted");
        assert_eq!(entry["action"], action);
        assert_eq!(action, "create_pull_request");
        assert_eq!(
            entry["context"]["tool_call_id"], "call_perm_1",
            "the request context names the tool call that is waiting"
        );

        let store = BlockedApprovalStore::open_default().expect("approval store");
        let record = store
            .get(&request_id)
            .expect("read the wait record")
            .expect("a wait=true call records its blocked approval");
        assert_eq!(record.session_id, session_id);
        assert_eq!(
            record.waiter_pid,
            Some(std::process::id()),
            "the waiting tool owns the record"
        );

        record_permission_via_file(
            &request_id,
            true,
            "file",
            Some("approved from another process".to_string()),
        )
        .expect("an external approver records the decision");

        turn.await.expect("turn task").expect("the waiting turn continues");

        // The queued reply text is not evidence: a tool call that never blocked
        // still advances the loop and consumes it, so the same text comes back
        // with the wait path dead. The stored tool result is the effect: a
        // call that did not wait records the "queued" text instead of the
        // decision the external approver recorded.
        let result_text = tool_result_text(&session_id, "call_perm_1").expect("the waiting call stores its result");
        assert!(
            result_text.starts_with("Permission approved:"),
            "the waiting tool returns the external decision, not the queue path: {result_text}"
        );
        assert!(
            result_text.contains("approved from another process"),
            "the result carries the approver's message: {result_text}"
        );

        runner.nudge();
        wait_for(
            || resume_marker_recorded(&session_id, &request_id),
            Duration::from_secs(15),
            "the resume marker in the session's event log",
        )
        .await?;
        wait_for(
            || {
                store
                    .get(&request_id)
                    .ok()
                    .flatten()
                    .and_then(|record| record.outcome)
                    .as_deref()
                    == Some("resumed")
            },
            Duration::from_secs(15),
            "the resumed wait record",
        )
        .await?;

        assert!(
            resume_marker_recorded(&session_id, &request_id),
            "exactly one resume turn is recorded for the request"
        );
        assert_eq!(resume_prompts(&session_id), 1, "the session is continued exactly once");
        assert_eq!(
            text_block_count(&session_id, "Resumed after approval."),
            1,
            "the resumed turn actually ran"
        );
        Ok(())
    });
    outcome.expect("scenario A");
}

// ---------------------------------------------------------------------------
// B. autonomous failover: the route moves and the switch is announced once
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_b_autonomous_failover_switches_route_once() -> Result<()> {
    let _env = setup_test_env()?;
    write_config(
        "[tools]\nprofile = \"core\"\n\n[provider]\nautonomous_failover = true\nautonomous_failover_max_attempts = 3\n",
    );

    let provider = Arc::new(MockProvider::with_routes(vec![api_key_route("gpt-5.4")]));
    provider.queue_error(failover_error());
    provider.queue_response(text_response("done"));
    let provider_dyn: Arc<dyn Provider> = provider.clone();

    let registry = Registry::new(provider_dyn.clone()).await;
    let mut agent = Agent::new(provider_dyn, registry);
    let session_id = agent.session_id().to_string();
    assert_eq!(agent.provider_model(), "mock");

    let text = agent.run_once_capture("fail over").await?;

    assert_eq!(text, "done", "the resent turn completes");
    assert_eq!(provider.calls(), 2, "one failed request and exactly one resend");
    assert_eq!(agent.provider_model(), "gpt-5.4", "the route actually changed");
    let applied = provider.applied_routes();
    assert_eq!(applied.len(), 1, "exactly one structured route switch");
    assert_eq!(applied[0].routed_model_spec(), TARGET_ROUTE);

    let switches = failover_events_for(&session_id);
    assert_eq!(
        switches.len(),
        1,
        "FailoverSwitched is announced exactly once: {switches:?}"
    );
    assert_eq!(switches[0]["attempt"], 1);
    assert_eq!(switches[0]["max"], 3);
    assert_eq!(switches[0]["from"], "mock");
    assert_eq!(switches[0]["to"], "gpt-5.4");
    Ok(())
}

// ---------------------------------------------------------------------------
// C. exhaustion: the cap is named, the resend count is exact, the budget lasts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_c_exhaustion_names_the_cap_and_persists_the_budget() -> Result<()> {
    let _env = setup_test_env()?;
    write_config(
        "[tools]\nprofile = \"core\"\n\n[provider]\nautonomous_failover = true\nautonomous_failover_max_attempts = 3\n",
    );

    let provider = Arc::new(MockProvider::with_routes(vec![api_key_route("gpt-5.4")]));
    for _ in 0..4 {
        provider.queue_error(failover_error());
    }
    let provider_dyn: Arc<dyn Provider> = provider.clone();

    let registry = Registry::new(provider_dyn.clone()).await;
    let mut agent = Agent::new(provider_dyn, registry);
    let session_id = agent.session_id().to_string();

    let error = agent
        .run_once_capture("fail over")
        .await
        .expect_err("an exhausted chain must end the turn");
    let message = error.to_string();

    assert!(
        message.contains("failover attempts exhausted"),
        "the final error names the exhausted budget: {message}"
    );
    assert!(message.contains("cap 3"), "the final error names the cap: {message}");
    assert_eq!(provider.calls(), 4, "the initial request plus exactly 3 resends");
    assert_eq!(provider.applied_routes().len(), 3, "one switch per spent attempt");
    assert_eq!(failover_events_for(&session_id).len(), 3, "one announcement per switch");

    let path = budget_file(&session_id);
    let stored = read_json_file(&path);
    assert_eq!(stored["attempts"], 3, "the persisted budget holds the spent attempts");
    let chain_id = stored["chain_id"]
        .as_str()
        .expect("the budget names its chain")
        .to_string();
    assert!(
        message.contains(&chain_id),
        "the final error names the persisted chain {chain_id}: {message}"
    );

    // A restart is the assertion. No `Drop` impl writes the budget, so reading
    // the same file twice only proves nothing wrote to it: that shape passes
    // with the restore deleted (verifier mutation M6). The restart has to be
    // observed through the agent that comes back.
    //
    // The three budget accessors are `pub(crate)` (`agent.rs:529`, `:536`,
    // `:567`) and `mod failover_budget` is private, so an integration test gets
    // E0624 and cannot read them directly. The public path is the error the
    // resumed run produces, and it is only reachable without a fresh chain when
    // the turn starts with automatic failover off: with it on,
    // `begin_failover_chain_for_prompt()` writes a new chain before the request
    // and the restored one is never read. That per-prompt chain is by design,
    // not a bug to "fix".
    //
    // So this turn starts with the setting off (it opens no chain) and the
    // provider arms it as the request goes out, which is what an operator
    // enabling the feature mid-run does. The failover policy that runs after the
    // provider error then spends against whatever the rebuilt agent restored.
    // A working restore refuses at once; a restore that starts a fresh chain
    // finds zero attempts and spends three more resends.
    agent.mark_closed();
    drop(agent);

    write_config(
        "[tools]\nprofile = \"core\"\n\n[provider]\nautonomous_failover = false\nautonomous_failover_max_attempts = 3\n",
    );
    // The resumed run gets its own failures. A working restore refuses the very
    // first one; a fresh chain would let the policy resend, which is what the
    // no-resend assertion below measures.
    for _ in 0..4 {
        provider.queue_error(failover_error());
    }
    let arming: Arc<dyn Provider> = Arc::new(ArmFailoverOnRequest {
        inner: provider.clone(),
    });
    let restart_registry = Registry::new(arming.clone()).await;
    let persisted = Session::load(&session_id).expect("the persisted session loads for the restart");
    let mut restored = Agent::new_with_session(arming, restart_registry, persisted, None);
    assert_eq!(
        restored.session_id(),
        session_id,
        "a fresh agent rebuilt from the persisted session owns that session"
    );

    let calls_before = provider.calls();
    let restarted_error = restored
        .run_once_capture("fail over again")
        .await
        .expect_err("the restored budget must refuse the resumed run's first attempt");
    let restarted_message = restarted_error.to_string();
    restored.mark_closed();

    assert_eq!(
        provider.calls() - calls_before,
        1,
        "the rebuilt agent makes no resend: the restored budget is already spent"
    );
    assert!(
        restarted_message.contains("failover attempts exhausted after 3 attempt(s)"),
        "the rebuilt agent reports the restored count, not a refunded zero: {restarted_message}"
    );
    assert!(
        restarted_message.contains(&chain_id),
        "the rebuilt agent spends the chain the previous process spent ({chain_id}), not a fresh one: {restarted_message}"
    );
    Ok(())
}

/// A provider that arms automatic failover as the resumed request goes out.
///
/// The resumed turn starts with the setting off, so it opens no chain of its
/// own; the failover policy that runs after the provider error then reads the
/// budget the rebuilt agent restored. Without that restore it finds a fresh
/// chain at zero attempts and spends three more resends, which is exactly what
/// the scenario's no-resend and chain-id assertions catch.
struct ArmFailoverOnRequest {
    inner: Arc<MockProvider>,
}

#[async_trait]
impl Provider for ArmFailoverOnRequest {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        write_config(
            "[tools]\nprofile = \"core\"\n\n[provider]\nautonomous_failover = true\nautonomous_failover_max_attempts = 3\n",
        );
        self.inner.complete(messages, tools, system, resume_session_id).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model(&self) -> String {
        self.inner.model()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        self.inner.set_model(model)
    }

    fn set_route_selection(&self, selection: &RouteSelection) -> Result<()> {
        self.inner.set_route_selection(selection)
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        self.inner.model_routes()
    }

    fn available_models(&self) -> Vec<&'static str> {
        self.inner.available_models()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            inner: self.inner.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// D. streaming rollback: partial output is rolled back, never duplicated
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_d_streaming_rollback_keeps_the_answer_once() -> Result<()> {
    let _env = setup_test_env()?;
    write_config(
        "[tools]\nprofile = \"core\"\n\n[provider]\nautonomous_failover = true\nautonomous_failover_max_attempts = 3\n",
    );

    let provider = Arc::new(MockProvider::with_routes(vec![api_key_route("gpt-5.4")]));
    provider.queue_stream(vec![
        Ok(StreamEvent::TextDelta("partial answer".to_string())),
        Err(failover_error()),
    ]);
    provider.queue_response(text_response("partial answer"));
    let provider_dyn: Arc<dyn Provider> = provider.clone();

    let registry = Registry::new(provider_dyn.clone()).await;
    let mut agent = Agent::new(provider_dyn, registry);
    let session_id = agent.session_id().to_string();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("answer me", Vec::new(), None, event_tx)
        .await?;

    let mut rollbacks = 0;
    while let Ok(event) = event_rx.try_recv() {
        if matches!(event, horon_agent::protocol::ServerEvent::RetryRollback { .. }) {
            rollbacks += 1;
        }
    }

    assert_eq!(rollbacks, 1, "partial output is rolled back exactly once");
    assert_eq!(provider.calls(), 2, "one failed stream and exactly one resend");
    assert_eq!(agent.provider_model(), "gpt-5.4", "the route actually changed");
    assert_eq!(
        failover_events_for(&session_id).len(),
        1,
        "the switch is announced once"
    );
    assert_eq!(
        text_block_count(&session_id, "partial answer"),
        1,
        "the stored answer text appears exactly once, never duplicated"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// E. human verdict: a run that stopped for a person resumes when one rules
// ---------------------------------------------------------------------------

const RETRY_CALL: &str = "call_risky_write";
/// The id the provider mints for the call it re-runs after a retry verdict.
///
/// A real provider names each new call itself, so a re-run carries a new id and
/// the transcript never repeats one. The event store's tool chain refuses a
/// repeated `tool_use` id, which is correct: two calls sharing an id is a
/// malformed transcript, not a retry.
const RETRY_RERUN_CALL: &str = "call_risky_write_rerun";
const OTHER_CALL: &str = "call_other";
const UNVERIFIABLE_LINE: &str = "[resume-unverifiable] call_risky_write the write may or may not have landed";

/// Answers each resume turn from the prompt it was handed, so the re-run in the
/// transcript is caused by the recorded verdict and not by the test
/// pre-scripting a tool call.
struct VerdictProvider {
    calls: Arc<AtomicUsize>,
    reran: Arc<StdMutex<bool>>,
    model: StdMutex<String>,
}

impl VerdictProvider {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            reran: Arc::new(StdMutex::new(false)),
            model: StdMutex::new("mock".to_string()),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn message_text(messages: &[Message]) -> String {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl Provider for VerdictProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = message_text(messages);
        let mut reran = self.reran.lock().unwrap();
        let retry_verdict = text.contains(&format!("{RETRY_CALL}: human_retry"));
        let skip_verdict = text.contains(&format!("{RETRY_CALL}: human_skip"));
        let events = if retry_verdict && !*reran {
            // The person said to run the interrupted call again: do exactly
            // that, then continue.
            *reran = true;
            let config = isolated_home().join("config.toml");
            vec![
                Ok(StreamEvent::ToolUseStart {
                    id: RETRY_RERUN_CALL.to_string(),
                    name: "read".to_string(),
                }),
                Ok(StreamEvent::ToolInputDelta(
                    json!({ "path": config.to_string_lossy() }).to_string(),
                )),
                Ok(StreamEvent::ToolUseEnd),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".to_string()),
                }),
            ]
        } else if skip_verdict {
            // The person said not to run it again: continue without it.
            vec![
                Ok(StreamEvent::TextDelta("skipped and continued".to_string())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }),
            ]
        } else if text.contains("[Resume after approval]") && !retry_verdict && !skip_verdict {
            // The first resume cannot tell whether the interrupted write
            // landed, so it stops and hands the run to a person. That end is
            // what writes `outcome = "needs-human"`.
            vec![
                Ok(StreamEvent::TextDelta(UNVERIFIABLE_LINE.to_string())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }),
            ]
        } else {
            vec![
                Ok(StreamEvent::TextDelta("verdict followed".to_string())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }),
            ]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn name(&self) -> &str {
        "verdict-following"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn set_route_selection(&self, selection: &RouteSelection) -> Result<()> {
        *self.model.lock().unwrap() = selection.model.clone();
        Ok(())
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        Vec::new()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            calls: self.calls.clone(),
            reran: self.reran.clone(),
            model: StdMutex::new(self.model()),
        })
    }
}

fn parked_session(session_id: &str) {
    let mut session = Session::create_with_id(session_id.to_string(), None, Some("Blocked run".to_string()));
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "Do the risky job".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::ToolUse {
                id: RETRY_CALL.to_string(),
                name: "write".to_string(),
                input: json!({"path": "notes/out.md", "content": "x"}),
                thought_signature: None,
            },
            ContentBlock::ToolUse {
                id: OTHER_CALL.to_string(),
                name: "write".to_string(),
                input: json!({"path": "notes/other.md", "content": "y"}),
                thought_signature: None,
            },
        ],
    );
    session.status = SessionStatus::Closed;
    session.save().expect("save the parked session");
}

fn record_human_verdict(request_id: &str, verdict: &str) -> Result<()> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_horon-agent"))
        .args([
            "approvals",
            "resolve",
            "--request-id",
            request_id,
            "--tool-call-id",
            RETRY_CALL,
            "--verdict",
            verdict,
        ])
        .env("HORON_HOME", isolated_home())
        .output()?;
    assert!(
        output.status.success(),
        "approvals resolve {verdict} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

struct VerdictOutcome {
    provider: Arc<VerdictProvider>,
    store: BlockedApprovalStore,
    request_id: String,
    resumes_before: usize,
    resumes_after: usize,
    named_uses_before: usize,
    named_uses_after: usize,
    named_results_before: usize,
    named_results_after: usize,
    other_uses_before: usize,
    other_uses_after: usize,
    other_results_before: usize,
    other_results_after: usize,
}

impl VerdictOutcome {
    fn named_uses_added(&self) -> usize {
        self.named_uses_after - self.named_uses_before
    }

    fn named_results_added(&self) -> usize {
        self.named_results_after - self.named_results_before
    }

    fn other_uses_added(&self) -> usize {
        self.other_uses_after - self.other_uses_before
    }

    fn other_results_added(&self) -> usize {
        self.other_results_after - self.other_results_before
    }
}

/// Drive the real product into the state the bug is about, then rule on it.
///
/// Phase 1 runs the real runner: a recorded approval resumes the parked turn,
/// that turn reports it cannot verify the interrupted call, and the runner
/// escalates to `outcome = "needs-human"`. Phase 2 records the verdict through
/// the real `horon-agent approvals resolve` binary and lets the same runner
/// pick it up.
async fn verdict_run(session_id: &str, request_id: &str, verdict: &str) -> Result<VerdictOutcome> {
    write_config("[tools]\nprofile = \"core\"\n");
    parked_session(session_id);
    assert_eq!(
        tool_calls_for(session_id, RETRY_CALL),
        (1, 0),
        "precondition: the named call started once and never recorded a result"
    );

    let store = BlockedApprovalStore::open_default()?;
    store.add(BlockedApproval {
        session_id: session_id.to_string(),
        request_id: request_id.to_string(),
        ..BlockedApproval::default()
    })?;
    record_permission_via_file(request_id, true, "file", None)?;

    let provider = Arc::new(VerdictProvider::new());
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let runner = AmbientRunnerHandle::new(Arc::new(SafetySystem::new()));
    assert!(runner.start(provider_dyn).await, "the ambient runner loop must start");

    let rid = request_id.to_string();
    let sid = session_id.to_string();
    wait_for(
        || {
            store
                .get(&rid)
                .ok()
                .flatten()
                .and_then(|record| record.outcome)
                .as_deref()
                == Some("needs-human")
        },
        Duration::from_secs(20),
        "the unverifiable stop to hand the run to a person",
    )
    .await?;
    let stopped = store.get(&rid)?.expect("the wait record exists");
    assert_eq!(
        stopped.outcome.as_deref(),
        Some("needs-human"),
        "precondition: the run ended unverifiable and asked for a person"
    );
    assert!(
        stopped.tool_verdicts.is_empty(),
        "precondition: no verdict has been recorded yet"
    );

    let resumes_before = resume_prompts(&sid);
    let (named_uses_before, named_results_before) = named_call_counts(&sid);
    let (other_uses_before, other_results_before) = tool_calls_for(&sid, OTHER_CALL);
    assert_eq!(resumes_before, 1, "one resume ran and then stopped for a person");

    record_human_verdict(&rid, verdict)?;
    let recorded = store.get(&rid)?.expect("the wait record exists");
    assert_eq!(
        recorded.outcome.as_deref(),
        Some(if verdict == "retry" {
            "human_retry"
        } else {
            "human_skip"
        }),
        "the human verdict replaces the needs-human stop"
    );
    assert!(
        recorded.tool_verdicts.contains_key(RETRY_CALL),
        "the verdict names the exact call: {:?}",
        recorded.tool_verdicts
    );

    runner.nudge();
    wait_for(
        || {
            store
                .get(&rid)
                .ok()
                .flatten()
                .and_then(|record| record.outcome)
                .as_deref()
                == Some("resumed")
        },
        Duration::from_secs(20),
        "the verdict-triggered resume turn to finish",
    )
    .await?;

    let (named_uses_after, named_results_after) = named_call_counts(&sid);
    let (other_uses_after, other_results_after) = tool_calls_for(&sid, OTHER_CALL);
    Ok(VerdictOutcome {
        provider: provider.clone(),
        store,
        request_id: rid,
        resumes_before,
        resumes_after: resume_prompts(&sid),
        named_uses_before,
        named_uses_after,
        named_results_before,
        named_results_after,
        other_uses_before,
        other_uses_after,
        other_results_before,
        other_results_after,
    })
}

fn approval_event(run: &VerdictOutcome) -> Value {
    run.store
        .get(&run.request_id)
        .expect("read the wait record")
        .expect("the wait record exists")
        .pending_event
        .expect("the verdict owes the client an ApprovalResolved event")
}

#[test]
fn scenario_e_retry_reruns_the_named_call() {
    let _env = setup_test_env().expect("isolated home");
    let outcome: Result<()> = std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        runtime.block_on(async {
            let run = verdict_run("sess_verdict_retry", "req_verdict_retry", "retry").await?;
            assert_eq!(
                run.resumes_after,
                run.resumes_before + 1,
                "the verdict continues the session exactly once"
            );
            assert_eq!(
                run.named_uses_added(),
                1,
                "retry must start the named call again, on top of the original start"
            );
            assert_eq!(
                run.named_results_added(),
                1,
                "retry must record a new tool finish for the named call; a retry treated as a skip records none"
            );
            assert_eq!(
                run.other_uses_added(),
                0,
                "another tool_call_id in the same request is untouched"
            );
            assert_eq!(run.other_results_added(), 0, "the other call gains no result");
            assert!(
                run.provider.calls() >= 3,
                "the re-run turn reached the provider again: {} calls",
                run.provider.calls()
            );

            let event = approval_event(&run);
            assert_eq!(event["type"], "approval_resolved");
            assert_eq!(event["request_id"], "req_verdict_retry");
            assert_eq!(event["approved"], true, "a retry is an approval");
            assert_eq!(event["via"], "human_retry");
            assert_eq!(event["resolved_by"], "human");
            Ok(())
        })
    })
    .join()
    .expect("thread");
    outcome.expect("scenario E retry");
}

#[test]
fn scenario_e_skip_does_not_rerun_the_named_call() {
    let _env = setup_test_env().expect("isolated home");
    let outcome: Result<()> = std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        runtime.block_on(async {
            let run = verdict_run("sess_verdict_skip", "req_verdict_skip", "skip").await?;
            assert_eq!(
                run.resumes_after,
                run.resumes_before + 1,
                "a skip verdict still continues the session exactly once"
            );
            assert_eq!(run.named_uses_added(), 0, "skip must not start the named call again");
            assert_eq!(run.named_results_added(), 0, "the skipped call gains no new result");
            assert_eq!(
                run.other_uses_added(),
                0,
                "another tool_call_id in the same request is untouched"
            );
            assert_eq!(run.other_results_added(), 0, "the other call gains no result");

            let event = approval_event(&run);
            assert_eq!(event["type"], "approval_resolved");
            assert_eq!(event["request_id"], "req_verdict_skip");
            assert_eq!(event["approved"], false, "a skip is not an approval");
            assert_eq!(event["via"], "human_skip");
            assert_eq!(event["resolved_by"], "human");
            Ok(())
        })
    })
    .join()
    .expect("thread");
    outcome.expect("scenario E skip");
}

// ---------------------------------------------------------------------------
// F. a second stop is a stop: a spent verdict cannot reopen it
// ---------------------------------------------------------------------------

/// Reports the unverifiable sentinel on every resume turn, with or without a
/// verdict.
///
/// The verdict turn is the one the defect is about: it runs because a person
/// ruled, and it stops again because it still cannot verify the interrupted
/// call. The verdict that started that turn is spent at that point.
struct SecondStopProvider {
    calls: Arc<AtomicUsize>,
}

impl SecondStopProvider {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl Provider for SecondStopProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = message_text(messages);
        let events = if text.contains("[Resume after approval]") {
            vec![
                Ok(StreamEvent::TextDelta(UNVERIFIABLE_LINE.to_string())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }),
            ]
        } else {
            vec![
                Ok(StreamEvent::TextDelta("idle".to_string())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }),
            ]
        };
        Ok(Box::pin(stream::iter(events)))
    }

    fn name(&self) -> &str {
        "second-stop"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            calls: self.calls.clone(),
        })
    }
}

/// A run that stops for a person twice must stay stopped the second time.
///
/// The escalation writes `outcome = "needs-human"`, and a non-empty
/// `tool_verdicts` is the one thing that overrides that in
/// `classify_blocked_approval`. A verdict that already drove a turn therefore
/// reopened every later stop: the event log holds the verdict turn's key, so the
/// runner closed the record as `resumed` without running anything, and the run
/// was reported as continued while it was waiting for a person.
#[test]
fn scenario_f_a_second_stop_stays_needs_human() {
    let _env = setup_test_env().expect("isolated home");
    let outcome: Result<()> =
        std::thread::spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
            runtime.block_on(async {
            let session_id = "sess_second_stop";
            let request_id = "req_second_stop";
            write_config("[tools]\nprofile = \"core\"\n");
            parked_session(session_id);

            let store = BlockedApprovalStore::open_default()?;
            store.add(BlockedApproval {
                session_id: session_id.to_string(),
                request_id: request_id.to_string(),
                ..BlockedApproval::default()
            })?;
            record_permission_via_file(request_id, true, "file", None)?;

            let provider = Arc::new(SecondStopProvider::new());
            let provider_dyn: Arc<dyn Provider> = provider.clone();
            let runner = AmbientRunnerHandle::new(Arc::new(SafetySystem::new()));
            assert!(runner.start(provider_dyn).await, "the ambient runner loop must start");

            // Phase 1: the recorded approval resumes the parked run; that turn
            // reports it cannot verify the interrupted call, so the runner asks
            // for a person.
            wait_for(
                || {
                    store
                        .get(request_id)
                        .ok()
                        .flatten()
                        .and_then(|record| record.outcome)
                        .as_deref()
                        == Some("needs-human")
                },
                Duration::from_secs(20),
                "the unverifiable stop to hand the run to a person",
            )
            .await?;
            assert_eq!(resume_prompts(session_id), 1, "precondition: one resume ran, then stopped");

            // Phase 2: a person rules through the real CLI. The verdict turn runs
            // and stops the same way, so the run is waiting for a person again.
            record_human_verdict(request_id, "skip")?;
            runner.nudge();
            wait_for(
                || resume_prompts(session_id) == 2,
                Duration::from_secs(20),
                "the verdict turn to run on top of the first resume",
            )
            .await?;

            // The verdict turn has now run and reported the same stop. Every
            // later runner pass must leave the record alone. A pass that closes
            // it as `resumed` is the defect: the spent verdict still classifies
            // as a resumable human verdict, the event log already holds the
            // verdict turn's key, and the record is closed without a turn ever
            // running.
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut seen: Vec<String> = Vec::new();
            loop {
                // Keep the loop from idling: a pass that closes the record must
                // run inside this window, not after a 30-second sleep.
                runner.nudge();
                let current = store
                    .get(request_id)
                    .ok()
                    .flatten()
                    .and_then(|record| record.outcome)
                    .unwrap_or_else(|| "<none>".to_string());
                if seen.last() != Some(&current) {
                    seen.push(current.clone());
                }
                if current == "resumed" || Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            assert_eq!(
                store.get(request_id)?.and_then(|record| record.outcome),
                Some("needs-human".to_string()),
                "a second stop is a stop: a spent verdict must not reopen it. Outcomes seen after the nudge: {seen:?}"
            );
            assert_eq!(
                resume_prompts(session_id),
                2,
                "and reopening it must not create another turn"
            );
            assert!(
                store
                    .get(request_id)?
                    .expect("the wait record exists")
                    .tool_verdicts
                    .is_empty(),
                "the verdict the second stop acted on is spent"
            );
            Ok(())
        })
        })
        .join()
        .expect("thread");
    outcome.expect("scenario F");
}
