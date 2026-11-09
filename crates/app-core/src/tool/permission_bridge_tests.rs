#![allow(
    clippy::await_holding_lock,
    reason = "the process-wide test env lock is held for the whole test on purpose"
)]

use super::*;
use crate::config::ApprovalMode;
use crate::tool::{Tool, ToolEffects, ToolExecutionMode, ToolOutput, approval_override};
use async_trait::async_trait;
use horon_agent_agent_runtime::InterruptSignal;
use serde_json::json;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

const CONN: &str = "conn-acp";

struct CountingTool {
    name: &'static str,
    effects: ToolEffects,
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "counts its runs"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn effects(&self) -> ToolEffects {
        self.effects
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new("ran"))
    }
}

struct Home {
    previous: Option<std::ffi::OsString>,
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::config::invalidate_config_cache();
        Self { previous, dir }
    }

    fn configure_approval_mode(&self, mode: &str) {
        std::fs::write(
            self.dir.path().join("config.toml"),
            format!("[tools]\napproval_mode = \"{mode}\"\n"),
        )
        .expect("write config");
        crate::config::invalidate_config_cache();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

struct Fixture {
    session: String,
    registry: Registry,
    shell_runs: Arc<AtomicUsize>,
    write_runs: Arc<AtomicUsize>,
    rx: mpsc::UnboundedReceiver<ServerEvent>,
    acp_token: u64,
    bridge_token: Option<u64>,
    override_token: Option<u64>,
    turn: Option<acp_session::TurnOwnership>,
}

impl Fixture {
    async fn new(session: &str, with_bridge: bool, mode: Option<ApprovalMode>, turn_owner: Option<&str>) -> Self {
        let registry = Registry::empty();
        let shell_runs = Arc::new(AtomicUsize::new(0));
        let write_runs = Arc::new(AtomicUsize::new(0));
        registry
            .register(
                "bash".to_string(),
                Arc::new(CountingTool {
                    name: "bash",
                    effects: ToolEffects::write().union(ToolEffects::process()),
                    runs: Arc::clone(&shell_runs),
                }),
            )
            .await;
        registry
            .register(
                "write".to_string(),
                Arc::new(CountingTool {
                    name: "write",
                    effects: ToolEffects::write(),
                    runs: Arc::clone(&write_runs),
                }),
            )
            .await;
        let (tx, rx) = mpsc::unbounded_channel();
        let acp_token = acp_session::install(session, CONN, tx.clone());
        let bridge_token = with_bridge.then(|| install(session, CONN, tx));
        let override_token = mode.map(|mode| approval_override::set(session, CONN, mode));
        let turn = Some(acp_session::claim_turn(session, turn_owner));
        Self {
            session: session.to_string(),
            registry,
            shell_runs,
            write_runs,
            rx,
            acp_token,
            bridge_token,
            override_token,
            turn,
        }
    }

    fn ctx(&self, call: &str) -> ToolContext {
        ToolContext {
            session_id: self.session.clone(),
            message_id: "permission-bridge-test".to_string(),
            tool_call_id: call.to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::AgentTurn,
        }
    }

    fn bridge(&self) -> Arc<PermissionBridge> {
        bridge_for_connection(&self.session, CONN).expect("the bridge is installed")
    }

    fn spawn_shell(&self, call: &str, command: &str) -> tokio::task::JoinHandle<anyhow::Result<ToolOutput>> {
        self.spawn_with(call, command, self.ctx(call))
    }

    fn spawn_with(
        &self,
        _call: &str,
        command: &str,
        ctx: ToolContext,
    ) -> tokio::task::JoinHandle<anyhow::Result<ToolOutput>> {
        let registry = self.registry.clone();
        let input = json!({"command": command});
        tokio::spawn(async move { registry.execute("bash", input, ctx).await })
    }

    async fn next_request(&mut self) -> (String, String, String, String) {
        match next_event(&mut self.rx).await {
            ServerEvent::ToolPermissionRequested {
                request_id,
                tool_call_id,
                tool_name,
                title,
                ..
            } => (request_id, tool_call_id, tool_name, title),
            other => panic!("expected a permission request, got {other:?}"),
        }
    }

    async fn next_resolved(&mut self) -> (String, bool) {
        match next_event(&mut self.rx).await {
            ServerEvent::ToolPermissionResolved { request_id, approved } => (request_id, approved),
            other => panic!("expected a permission withdrawal, got {other:?}"),
        }
    }

    fn assert_editor_quiet(&mut self) {
        if let Ok(event) = self.rx.try_recv() {
            panic!("the editor must not be sent anything more, got {event:?}");
        }
    }

    /// What `handle_permission_decision` does for the owning connection.
    fn answer(&self, request_id: &str, approved: bool) {
        let bridge = self.bridge();
        assert_eq!(bridge.begin_editor_answer(request_id), EditorAnswer::Pending);
        crate::safety::record_permission_via_file(request_id, approved, "api_client", None)
            .expect("the editor's answer is recorded");
        // The decision-store poll may settle first; either way exactly one wins.
        bridge.finish_editor_answer(request_id, approved, "api_client");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.turn.take());
        if let Some(token) = self.bridge_token {
            clear(&self.session, token);
        }
        if let Some(token) = self.override_token {
            approval_override::clear(&self.session, token);
        }
        acp_session::clear(&self.session, self.acp_token);
    }
}

async fn next_event(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> ServerEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("the editor must be sent an event")
        .expect("the editor channel stays open")
}

async fn finished(task: tokio::task::JoinHandle<anyhow::Result<ToolOutput>>) -> anyhow::Result<ToolOutput> {
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the gated call must finish")
        .expect("the gated call must not panic")
}

fn open_key_holds(request_id: &str) -> bool {
    Registry::open_approval_requests()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .any(|id| id == request_id)
}

fn recorded(request_id: &str) -> crate::safety::Decision {
    crate::tool::ambient::shared_safety_system()
        .decision_for(request_id)
        .unwrap_or_else(|| panic!("request {request_id} must have a recorded decision"))
}

#[tokio::test]
async fn gate_blocks_until_bridge_allows_then_runs() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-allow", true, Some(ApprovalMode::Write), Some(CONN)).await;

    let task = fixture.spawn_shell("call-allow", "ls");
    let (request_id, tool_call_id, tool_name, title) = fixture.next_request().await;
    assert_eq!(tool_call_id, "call-allow");
    assert_eq!(tool_name, "bash");
    assert!(title.starts_with("bash: "), "{title}");
    assert_eq!(
        fixture.shell_runs.load(Ordering::SeqCst),
        0,
        "the call must wait for the answer"
    );
    assert!(open_key_holds(&request_id), "the open request carries this call's key");

    fixture.answer(&request_id, true);
    let output = finished(task).await.expect("an approved call runs");
    assert_eq!(output.output, "ran");
    assert_eq!(
        fixture.shell_runs.load(Ordering::SeqCst),
        1,
        "the body runs exactly once"
    );
    assert_eq!(fixture.bridge().pending_len(), 0);
    assert!(!open_key_holds(&request_id), "an approval releases the key");
    fixture.assert_editor_quiet();
}

#[tokio::test]
async fn gate_bridge_deny_returns_denial_text() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-deny", true, Some(ApprovalMode::Write), Some(CONN)).await;

    let task = fixture.spawn_shell("call-deny", "ls");
    let (request_id, ..) = fixture.next_request().await;
    fixture.answer(&request_id, false);
    let error = finished(task).await.expect_err("a denied call returns its refusal");
    let message = error.to_string();
    assert!(message.contains("was denied"), "{message}");
    assert!(
        message.contains(&request_id),
        "the refusal names the request: {message}"
    );
    assert!(message.contains("answered no"), "{message}");
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 0, "a denied body never runs");
    assert!(!open_key_holds(&request_id));
    fixture.assert_editor_quiet();
}

#[tokio::test]
async fn gate_without_bridge_keeps_refuse_and_retry() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-no-bridge", false, Some(ApprovalMode::Write), Some(CONN)).await;

    let first = finished(fixture.spawn_shell("call-1", "ls"))
        .await
        .expect_err("without a bridge the gate refuses at once");
    let first = first.to_string();
    assert!(first.contains("needs approval before it runs"), "{first}");
    assert!(first.contains("is queued"), "{first}");
    let second = finished(fixture.spawn_shell("call-2", "ls"))
        .await
        .expect_err("a retry before any answer is refused again")
        .to_string();
    let request_id = |text: &str| {
        text.split_whitespace()
            .skip_while(|word| *word != "Request")
            .nth(1)
            .map(str::to_string)
    };
    assert_eq!(
        request_id(&first),
        request_id(&second),
        "a retry names the same queued request"
    );
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 0);
    fixture.assert_editor_quiet();
}

#[tokio::test]
async fn gate_bridge_interrupt_denies() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-interrupt", true, Some(ApprovalMode::Write), Some(CONN)).await;

    let signal = InterruptSignal::new();
    let mut ctx = fixture.ctx("call-interrupt");
    ctx.graceful_shutdown_signal = Some(signal.clone());
    let task = fixture.spawn_with("call-interrupt", "ls", ctx);
    let (request_id, ..) = fixture.next_request().await;

    signal.fire();
    let message = finished(task)
        .await
        .expect_err("a cancelled turn denies the waiting call")
        .to_string();
    assert!(message.contains(&request_id), "{message}");
    assert!(message.contains("was cancelled before it was answered"), "{message}");
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.next_resolved().await,
        (request_id.clone(), false),
        "the editor is told to withdraw the prompt"
    );
    let decision = recorded(&request_id);
    assert!(!decision.approved);
    assert_eq!(decision.decided_via, CANCELLED_VIA);
    assert_eq!(fixture.bridge().pending_len(), 0);
    assert!(!open_key_holds(&request_id));
    assert_eq!(
        fixture.bridge().begin_editor_answer(&request_id),
        EditorAnswer::AlreadyDecided,
        "a late answer after the cancel must not change anything"
    );
}

#[tokio::test]
async fn gate_abort_cleans_pending_and_key() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-abort", true, Some(ApprovalMode::Write), Some(CONN)).await;

    let task = fixture.spawn_shell("call-abort", "ls");
    let (request_id, ..) = fixture.next_request().await;
    assert_eq!(fixture.bridge().waiters(&request_id), 1);

    task.abort();
    let joined = task.await;
    assert!(
        joined.as_ref().is_err_and(tokio::task::JoinError::is_cancelled),
        "{joined:?}"
    );
    assert_eq!(
        fixture.bridge().pending_len(),
        0,
        "the aborted wait leaves no pending entry"
    );
    assert!(!open_key_holds(&request_id), "the aborted wait releases its key");
    let decision = recorded(&request_id);
    assert!(!decision.approved);
    assert_eq!(decision.decided_via, CANCELLED_VIA);
    assert_eq!(fixture.next_resolved().await, (request_id.clone(), false));
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 0);

    let retry = fixture.spawn_shell("call-after-abort", "ls");
    let (fresh_id, ..) = fixture.next_request().await;
    assert_ne!(
        fresh_id, request_id,
        "a new call asks again instead of joining a dead request"
    );
    fixture.answer(&fresh_id, true);
    finished(retry).await.expect("the fresh request runs once approved");
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn gate_parallel_identical_calls_share_one_prompt() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-parallel", true, Some(ApprovalMode::Write), Some(CONN)).await;

    let first = fixture.spawn_shell("call-a", "make test");
    let second = fixture.spawn_shell("call-b", "make test");
    let (request_id, ..) = fixture.next_request().await;
    tokio::time::timeout(Duration::from_secs(10), fixture.bridge().waiters_reach(&request_id, 2))
        .await
        .expect("the identical call joins the open request");
    fixture.assert_editor_quiet();

    fixture.answer(&request_id, true);
    finished(first).await.expect("the first call runs");
    finished(second)
        .await
        .expect("the identical call runs on the same answer");
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 2);
    fixture.assert_editor_quiet();

    let bridge = fixture.bridge();
    assert_eq!(
        bridge.begin_editor_answer(&request_id),
        EditorAnswer::AlreadyDecided,
        "a second answer to the shared prompt is ignored"
    );
    assert!(!bridge.finish_editor_answer(&request_id, false, "api_client"));
    assert_eq!(bridge.pending_len(), 0);
}

#[tokio::test]
async fn gate_resolved_by_cli_cancels_editor_prompt() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-cli", true, Some(ApprovalMode::Write), Some(CONN)).await;

    let task = fixture.spawn_shell("call-cli", "ls");
    let (request_id, ..) = fixture.next_request().await;
    crate::safety::record_permission_via_file(&request_id, true, "cli", None).expect("the CLI records its decision");

    assert_eq!(
        fixture.next_resolved().await,
        (request_id.clone(), true),
        "the editor withdraws its prompt when another surface decided"
    );
    finished(task).await.expect("the CLI approval releases the call");
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.bridge().begin_editor_answer(&request_id),
        EditorAnswer::AlreadyDecided
    );
    fixture.assert_editor_quiet();
}

#[tokio::test]
async fn permission_bridge_cleared_on_disconnect_denies_waiters() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-disconnect", true, Some(ApprovalMode::Write), Some(CONN)).await;

    let task = fixture.spawn_shell("call-disconnect", "ls");
    let (request_id, ..) = fixture.next_request().await;
    let bridge = fixture.bridge();
    clear(&fixture.session, fixture.bridge_token.take().expect("installed"));
    let message = finished(task).await.expect_err("a dropped editor denies").to_string();
    assert!(message.contains("lost its editor connection"), "{message}");
    assert_eq!(bridge.pending_len(), 0);
    assert_eq!(recorded(&request_id).decided_via, DISCONNECTED_VIA);
    assert!(for_session(&fixture.session).is_none());
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn permission_bridge_not_used_for_another_connections_turn() {
    let _lock = crate::storage::lock_test_env();
    let home = Home::new();
    home.configure_approval_mode("write");
    let mut fixture = Fixture::new("pb-desktop-turn", true, Some(ApprovalMode::Off), Some("conn-desktop")).await;

    assert!(for_session(&fixture.session).is_none());
    let message = finished(fixture.spawn_shell("call-desktop", "ls"))
        .await
        .expect_err("a desktop turn keeps the configured mode and the refuse-and-retry gate")
        .to_string();
    assert!(message.contains("needs approval before it runs"), "{message}");
    fixture.assert_editor_quiet();
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn override_write_mode_asks_for_bash() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-override-bash", true, Some(ApprovalMode::Write), Some(CONN)).await;

    let task = fixture.spawn_shell("call-override-bash", "ls");
    let (request_id, _, tool_name, _) = fixture.next_request().await;
    assert_eq!(tool_name, "bash", "write mode asks before a shell call");
    fixture.answer(&request_id, true);
    finished(task).await.expect("approved");
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn override_write_mode_lets_write_run() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-override-write", true, Some(ApprovalMode::Write), Some(CONN)).await;

    let ctx = fixture.ctx("call-override-write");
    let output = fixture
        .registry
        .execute("write", json!({"file_path": "a.txt", "content": "x"}), ctx)
        .await
        .expect("a plain file mutation runs in write mode");
    assert_eq!(output.output, "ran");
    assert_eq!(fixture.write_runs.load(Ordering::SeqCst), 1);
    fixture.assert_editor_quiet();
}

#[tokio::test]
async fn override_off_never_asks() {
    let _lock = crate::storage::lock_test_env();
    let home = Home::new();
    home.configure_approval_mode("write");
    let mut fixture = Fixture::new("pb-override-off", true, Some(ApprovalMode::Off), Some(CONN)).await;

    finished(fixture.spawn_shell("call-off", "ls"))
        .await
        .expect("the session's off mode wins over the configured write mode");
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 1);
    fixture.assert_editor_quiet();

    approval_override::clear(&fixture.session, fixture.override_token.take().expect("set"));
    let task = fixture.spawn_shell("call-configured", "ls");
    let (request_id, ..) = fixture.next_request().await;
    fixture.answer(&request_id, false);
    finished(task)
        .await
        .expect_err("without the override the configured write mode asks again");
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 1);
}

/// Registers the real dispatcher beside the fixture's counting tools.
async fn with_toolbox(fixture: &Fixture) -> Registry {
    let registry = fixture.registry.clone();
    registry
        .register(
            crate::config::TOOLBOX_TOOL.to_string(),
            Arc::new(crate::tool::toolbox::ToolboxTool::new(registry.downgrade())) as Arc<dyn Tool>,
        )
        .await;
    registry
}

/// The dispatcher is policy-transparent: `toolbox run bash` asks once, and the
/// prompt names `bash` with its own arguments. A second prompt for the wrapper
/// would arrive as another permission request, which `assert_editor_quiet`
/// rejects.
#[tokio::test]
async fn toolbox_run_asks_once_and_the_prompt_names_the_target() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-toolbox-ask", true, Some(ApprovalMode::Write), Some(CONN)).await;
    let registry = with_toolbox(&fixture).await;

    let ctx = fixture.ctx("call-toolbox-ask");
    let input = json!({"action": "run", "tool": "bash", "args": {"command": "echo hi"}});
    let task = tokio::spawn(async move { registry.execute(crate::config::TOOLBOX_TOOL, input, ctx).await });

    let (request_id, _tool_call_id, tool_name, title) = fixture.next_request().await;
    assert_eq!(tool_name, "bash", "the prompt names the tool that will run");
    assert!(title.starts_with("bash: "), "{title}");
    assert_eq!(
        fixture.shell_runs.load(Ordering::SeqCst),
        0,
        "nothing runs before the answer"
    );

    fixture.answer(&request_id, true);
    let output = finished(task).await.expect("the approved target runs");
    assert_eq!(output.output, "ran");
    assert_eq!(
        fixture.shell_runs.load(Ordering::SeqCst),
        1,
        "the target body runs once"
    );
    fixture.assert_editor_quiet();
}

/// A denial on the target is what the outer call returns, and the target itself
/// never executes.
#[tokio::test]
async fn toolbox_run_denied_leaves_the_target_unrun() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut fixture = Fixture::new("pb-toolbox-deny", true, Some(ApprovalMode::Write), Some(CONN)).await;
    let registry = with_toolbox(&fixture).await;

    let ctx = fixture.ctx("call-toolbox-deny");
    let input = json!({"action": "run", "tool": "bash", "args": {"command": "echo hi"}});
    let task = tokio::spawn(async move { registry.execute(crate::config::TOOLBOX_TOOL, input, ctx).await });

    let (request_id, ..) = fixture.next_request().await;
    fixture.answer(&request_id, false);
    let message = finished(task)
        .await
        .expect_err("a denied target fails the call that reached it")
        .to_string();
    assert!(message.contains("was denied"), "{message}");
    assert_eq!(
        fixture.shell_runs.load(Ordering::SeqCst),
        0,
        "a denied target never runs"
    );
    fixture.assert_editor_quiet();
}

/// The manual QA case: an isolated home whose `config.toml` sets
/// `approval_mode = "write"`, one turn that runs `bash` through the dispatcher,
/// and exactly one recorded approval request naming `bash`. Run with
/// `--nocapture` to read the transcript.
#[tokio::test]
async fn toolbox_run_uses_the_configured_write_mode_for_the_target() {
    let _lock = crate::storage::lock_test_env();
    let home = Home::new();
    home.configure_approval_mode("write");
    let mut fixture = Fixture::new("pb-toolbox-config-mode", true, None, Some(CONN)).await;
    let registry = with_toolbox(&fixture).await;

    let ctx = fixture.ctx("call-toolbox-config-mode");
    let input = json!({"action": "run", "tool": "bash", "args": {"command": "echo hi"}});
    let task = tokio::spawn(async move { registry.execute(crate::config::TOOLBOX_TOOL, input, ctx).await });

    let (request_id, _tool_call_id, tool_name, title) = fixture.next_request().await;
    println!("home: {}", home.dir.path().display());
    println!("config.toml: [tools] approval_mode = \"write\"");
    println!("toolbox run bash -> approval requests: 1, tool_name={tool_name}, title={title}");
    assert_eq!(
        tool_name, "bash",
        "the configured write mode asks about bash, not toolbox"
    );

    fixture.answer(&request_id, true);
    let output = finished(task).await.expect("the approved target runs");
    assert_eq!(output.output, "ran");
    assert_eq!(fixture.shell_runs.load(Ordering::SeqCst), 1);
    fixture.assert_editor_quiet();
}
