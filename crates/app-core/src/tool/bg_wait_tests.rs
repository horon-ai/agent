//! Tool-surface tests for the `children` and `messages` sources of `bg action=wait`.
//!
//! Each test drives the real `bg` tool so what is asserted is what a caller
//! sees, not an internal helper. The agent home, the config cache, and the
//! process-wide result cache are all global, so every test holds
//! `lock_test_env()` for its whole body and runs inside `Runtime::block_on`
//! from a plain `#[test]`: the guard is then never held across an `await` by an
//! async test harness.

use crate::tool::bg::BgTool;
use crate::tool::{Tool, ToolContext, ToolExecutionMode};
use futures::future::BoxFuture;
use horon_agent_irc_core::{
    Deliver, DeliverResult, Directory, Endpoint, IrcBus, IrcLimits, IrcMessage, SendRequest, global,
};
use serde_json::json;
use std::ffi::OsString;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// Isolated agent home plus an optional `config.toml`, restored on drop.
struct TestEnv {
    _home: tempfile::TempDir,
    previous: Option<OsString>,
}

impl TestEnv {
    fn new(config: &str) -> Self {
        let home = tempfile::tempdir().expect("isolated agent home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        if !config.is_empty() {
            let path = crate::config::Config::path().expect("config path");
            std::fs::create_dir_all(path.parent().expect("config parent")).expect("create config dir");
            std::fs::write(&path, config).expect("write config");
        }
        crate::config::Config::invalidate_cache();
        Self { _home: home, previous }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        global::clear();
        match self.previous.take() {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn run(body: impl Future<Output = ()>) {
    tokio::runtime::Runtime::new().expect("runtime").block_on(body);
}

fn ctx(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "message".to_string(),
        tool_call_id: "bg-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn wait_params(params: serde_json::Value) -> serde_json::Value {
    let mut object = params;
    object["action"] = json!("wait");
    object["intent"] = json!("Wait on the selected sources");
    object
}

async fn wait(params: serde_json::Value, session: &str) -> anyhow::Result<crate::tool::ToolOutput> {
    BgTool::new().execute(wait_params(params), ctx(session)).await
}

/// Resolves every name and reports no peers, which is all a buffered send needs.
struct AnyDirectory;

impl Directory for AnyDirectory {
    fn resolve<'a>(&'a self, _sender: &'a str, target: &'a str) -> BoxFuture<'a, Result<Endpoint, String>> {
        let id: Arc<str> = Arc::from(target);
        Box::pin(async move {
            Ok(Endpoint {
                id,
                handle: None,
                parent: None,
                status: Arc::from("ready"),
            })
        })
    }

    fn broadcast_targets<'a>(&'a self, _sender: &'a str) -> BoxFuture<'a, Result<Vec<Endpoint>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn roster<'a>(&'a self, _sender: &'a str) -> BoxFuture<'a, Vec<Endpoint>> {
        Box::pin(async { Vec::new() })
    }
}

/// A target that is not live, so the send is parked in the mailbox exactly as
/// it is for a busy session.
struct OfflineDeliver;

impl Deliver for OfflineDeliver {
    fn deliver<'a>(
        &'a self,
        _target: &'a Endpoint,
        _message: &'a IrcMessage,
        _from_parent: bool,
    ) -> BoxFuture<'a, DeliverResult> {
        Box::pin(async { DeliverResult::Errored("session is not accepting input".to_string()) })
    }

    fn has_pending_input<'a>(&'a self, _agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn can_respond<'a>(&'a self, _agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { true })
    }
}

fn install_bus() -> Arc<IrcBus> {
    let bus = IrcBus::new(IrcLimits::default(), Arc::new(AnyDirectory), Arc::new(OfflineDeliver));
    global::install(bus.clone());
    bus
}

/// A finished child is reported first; the running one is left for a later
/// wait, which returns it once it finishes and never repeats the first.
#[test]
fn wait_tool_children_returns_finished_child() {
    let _guard = crate::storage::lock_test_env();
    let _env = TestEnv::new("");
    crate::agent_result_cache::reset_for_test();
    let parent = "parent-finished-child";
    crate::agent_result_cache::publish_running("slow-child", parent, "running");
    crate::agent_result_cache::publish_completed("fast-child", parent, "done", None).expect("publish child");

    run(async {
        let output = wait(json!({"children": true, "max_wait_seconds": 5}), parent)
            .await
            .expect("wait succeeds");
        assert!(output.output.contains("fast-child"), "output={}", output.output);
        assert!(
            !output.output.contains("slow-child"),
            "the running child is not reported yet"
        );
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata["wait_reason"], "child_finished");
        assert_eq!(metadata["child_session_id"], "fast-child");
        assert_eq!(metadata["timed_out"], false);
        assert_eq!(metadata["sources"], json!(["children"]));

        let slower = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            crate::agent_result_cache::publish_completed("slow-child", parent, "done", None)
                .expect("publish the slower child");
        });
        let output = wait(json!({"children": true, "max_wait_seconds": 10}), parent)
            .await
            .expect("second wait succeeds");
        slower.await.expect("the publisher task finishes");
        assert!(output.output.contains("slow-child"), "output={}", output.output);
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata["wait_reason"], "child_finished");
        assert_eq!(metadata["child_session_id"], "slow-child");
        assert_eq!(metadata["timed_out"], false);
    });
}

/// A selection holding nothing returns immediately instead of parking.
#[test]
fn wait_tool_children_nothing_to_wait_for_is_immediate() {
    let _guard = crate::storage::lock_test_env();
    let _env = TestEnv::new("");
    crate::agent_result_cache::reset_for_test();
    let parent = "parent-nothing-pending";

    run(async {
        let started = std::time::Instant::now();
        let output = wait(json!({"children": true, "max_wait_seconds": 30}), parent)
            .await
            .expect("wait succeeds");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "preflight must not park: {:?}",
            started.elapsed()
        );
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata["wait_reason"], "nothing_to_wait_for");
        assert_eq!(metadata["timed_out"], false);
        assert!(
            output.output.contains("nothing to wait for"),
            "output={}",
            output.output
        );
    });
}

/// A child that never finishes is named in the timeout summary.
#[test]
fn wait_tool_children_timeout_lists_running_ids() {
    let _guard = crate::storage::lock_test_env();
    let _env = TestEnv::new("");
    crate::agent_result_cache::reset_for_test();
    let parent = "parent-timeout-listing";
    crate::agent_result_cache::publish_running("patient-child", parent, "running");

    run(async {
        let output = wait(json!({"children": true, "max_wait_seconds": 1}), parent)
            .await
            .expect("wait succeeds");
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata["wait_reason"], "timeout");
        assert_eq!(metadata["timed_out"], true);
        assert_eq!(metadata["max_wait_seconds"], 1);
        assert_eq!(metadata["pending_child_session_ids"], json!(["patient-child"]));
        assert!(output.output.contains("patient-child"), "output={}", output.output);
    });
}

/// The wait starts with no reply obligation and a message arriving afterwards
/// still wakes it well before the deadline.
#[test]
fn wait_tool_messages_wakes_on_irc_message() {
    let _guard = crate::storage::lock_test_env();
    let _env = TestEnv::new("[tools]\nirc = true\n");
    let bus = install_bus();
    let me = "parent-message-wake";
    // No unanswered send exists, so `IrcBus::wait` cannot park on this session;
    // the message has to reach the wait through the mailbox instead.
    assert!(bus.unanswered_sends(me).is_empty());

    run(async {
        let peer = bus.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            peer.send(SendRequest {
                from: Arc::from("peer"),
                to: me.to_string(),
                body: "the build is green".to_string(),
                reply_to: None,
                progress: false,
                wake_relay: false,
            })
            .await
            .expect("buffer one message");
        });

        let started = std::time::Instant::now();
        let output = wait(json!({"messages": true, "max_wait_seconds": 10}), me)
            .await
            .expect("wait succeeds");
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "the message must wake the wait well before the deadline: {:?}",
            started.elapsed()
        );
        assert!(output.output.contains("<irc"), "output={}", output.output);
        assert!(output.output.contains("the build is green"), "output={}", output.output);
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata["wait_reason"], "message");
        assert_eq!(metadata["timed_out"], false);
        assert_eq!(bus.unread_count(me), 0, "the wait drains the message");
    });
}

/// An interrupt queued for the session ends the wait with `interrupted`.
#[test]
fn wait_tool_interrupt_ends_wait() {
    let _guard = crate::storage::lock_test_env();
    let _env = TestEnv::new("");
    crate::agent_result_cache::reset_for_test();
    let parent = "parent-interrupt";
    crate::agent_result_cache::publish_running("steady-child", parent, "running");

    run(async {
        let publisher = parent.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            crate::bus::Bus::global().publish(crate::bus::BusEvent::SessionInputQueued {
                session_id: publisher,
                kind: crate::bus::QueuedInputKind::Interrupt,
            });
        });

        let output = wait(json!({"children": true, "max_wait_seconds": 10}), parent)
            .await
            .expect("wait succeeds");
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata["wait_reason"], "interrupted");
        assert_eq!(metadata["timed_out"], false);
        assert!(output.output.contains("interrupted"), "output={}", output.output);
    });
}

/// A child finished before a mode `all` deadline stays deliverable afterwards.
///
/// The child is consumed only when the whole selection is reported, so a
/// deadline that arrives first must leave it for the next wait.
#[test]
fn wait_tool_all_timeout_keeps_finished_child() {
    let _guard = crate::storage::lock_test_env();
    let _env = TestEnv::new("");
    crate::agent_result_cache::reset_for_test();
    let parent = "parent-all-timeout-child";
    crate::agent_result_cache::publish_completed("done-child", parent, "done", None).expect("publish child");
    crate::agent_result_cache::publish_running("busy-child", parent, "running");

    run(async {
        let output = wait(
            json!({"children": true, "wait_mode": "all", "max_wait_seconds": 1}),
            parent,
        )
        .await
        .expect("wait succeeds");
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata["wait_reason"], "timeout");
        assert_eq!(metadata["timed_out"], true);
        assert_eq!(metadata["pending_child_session_ids"], json!(["busy-child"]));

        let output = wait(json!({"children": true, "max_wait_seconds": 5}), parent)
            .await
            .expect("second wait succeeds");
        assert!(output.output.contains("done-child"), "output={}", output.output);
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata["wait_reason"], "child_finished");
        assert_eq!(metadata["child_session_id"], "done-child");
    });
}

/// Without the new flags the old contract holds: a wait still needs a task.
#[test]
fn wait_tool_defaults_unchanged() {
    let _guard = crate::storage::lock_test_env();
    let _env = TestEnv::new("");
    crate::agent_result_cache::reset_for_test();
    let parent = "parent-defaults";

    run(async {
        let error = wait(json!({"max_wait_seconds": 1}), parent)
            .await
            .expect_err("the default wait still requires a task")
            .to_string();
        assert!(error.contains("task_id is required"), "error={error}");

        let output = wait(json!({"children": true, "max_wait_seconds": 1}), parent)
            .await
            .expect("the same call with a source succeeds");
        let metadata = output.metadata.expect("metadata");
        assert_eq!(metadata["wait_reason"], "nothing_to_wait_for");
    });
}
