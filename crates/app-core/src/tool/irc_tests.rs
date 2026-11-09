#![cfg_attr(test, allow(clippy::await_holding_lock))]

//! Tool-surface tests for `irc`. Each test installs a fake bus behind the
//! process-wide global and serializes on the shared test-environment lock.

use super::IrcTool;
use crate::tool::{ConcurrencyClass, Tool, ToolContext, ToolExecutionMode};
use futures::future::BoxFuture;
use horon_agent_irc_core::{
    Deliver, DeliverResult, Directory, Endpoint, IrcBus, IrcLimits, IrcMessage, SendRequest, global,
};
use serde_json::json;
use std::collections::HashSet;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

struct HomeGuard {
    _home: tempfile::TempDir,
    previous: Option<OsString>,
}

impl HomeGuard {
    fn with_config(contents: &str) -> Self {
        let home = tempfile::tempdir().expect("isolated agent home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        crate::config::Config::invalidate_cache();
        let path = crate::config::Config::path().expect("config path");
        std::fs::create_dir_all(path.parent().expect("config parent")).expect("create config parent");
        std::fs::write(&path, contents).expect("write config");
        crate::config::Config::invalidate_cache();
        Self { _home: home, previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

/// Clears the process-wide bus after the test, including on panic.
struct BusGuard;

impl Drop for BusGuard {
    fn drop(&mut self) {
        global::clear();
    }
}

fn endpoint(id: &'static str, handle: Option<&'static str>) -> Endpoint {
    Endpoint {
        id: Arc::from(id),
        handle: handle.map(Arc::from),
        parent: None,
        status: Arc::from("ready"),
    }
}

struct FakeDirectory {
    peers: Vec<(&'static str, Option<&'static str>)>,
    broadcast: Vec<&'static str>,
}

impl FakeDirectory {
    fn peers(peers: Vec<(&'static str, Option<&'static str>)>) -> Self {
        Self {
            peers,
            broadcast: Vec::new(),
        }
    }

    fn with_broadcast(peers: Vec<(&'static str, Option<&'static str>)>, broadcast: Vec<&'static str>) -> Self {
        Self { peers, broadcast }
    }
}

impl Directory for FakeDirectory {
    fn resolve<'a>(&'a self, _sender: &'a str, target: &'a str) -> BoxFuture<'a, Result<Endpoint, String>> {
        let target = target.to_string();
        Box::pin(async move {
            self.peers
                .iter()
                .find(|(id, handle)| *id == target || handle.is_some_and(|value| value == target))
                .map(|(id, handle)| endpoint(id, *handle))
                .ok_or_else(|| format!("unknown agent \"{target}\""))
        })
    }

    fn broadcast_targets<'a>(&'a self, _sender: &'a str) -> BoxFuture<'a, Result<Vec<Endpoint>, String>> {
        Box::pin(async move {
            Ok(self
                .broadcast
                .iter()
                .map(|id| {
                    endpoint(
                        id,
                        self.peers
                            .iter()
                            .find(|(peer, _)| peer == id)
                            .and_then(|(_, handle)| *handle),
                    )
                })
                .collect())
        })
    }

    fn roster<'a>(&'a self, _sender: &'a str) -> BoxFuture<'a, Vec<Endpoint>> {
        Box::pin(async move { self.peers.iter().map(|(id, handle)| endpoint(id, *handle)).collect() })
    }
}

struct FakeDeliver {
    errored: HashSet<&'static str>,
    unavailable: HashSet<&'static str>,
}

impl FakeDeliver {
    fn injected() -> Self {
        Self {
            errored: HashSet::new(),
            unavailable: HashSet::new(),
        }
    }

    fn errored_for(target: &'static str) -> Self {
        Self {
            errored: HashSet::from([target]),
            unavailable: HashSet::new(),
        }
    }

    fn unavailable_for(target: &'static str) -> Self {
        Self {
            errored: HashSet::new(),
            unavailable: HashSet::from([target]),
        }
    }
}

impl Deliver for FakeDeliver {
    fn deliver<'a>(
        &'a self,
        target: &'a Endpoint,
        _message: &'a IrcMessage,
        _from_parent: bool,
    ) -> BoxFuture<'a, DeliverResult> {
        let id = target.id.to_string();
        let errored = self.errored.contains(id.as_str());
        let unavailable = self.unavailable.contains(id.as_str());
        Box::pin(async move {
            if errored {
                DeliverResult::Errored("busy".to_string())
            } else if unavailable {
                DeliverResult::Unavailable("gone".to_string())
            } else {
                DeliverResult::Injected
            }
        })
    }

    fn has_pending_input<'a>(&'a self, _agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn can_respond<'a>(&'a self, _agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async { true })
    }
}

fn install(limits: IrcLimits, directory: Arc<dyn Directory>, deliver: Arc<dyn Deliver>) -> Arc<IrcBus> {
    let bus = IrcBus::new(limits, directory, deliver);
    global::install(bus.clone());
    bus
}

fn ctx(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "message".to_string(),
        tool_call_id: "irc-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

async fn enqueue(bus: &IrcBus, from: &'static str, to: &'static str, body: &'static str) {
    bus.send(SendRequest {
        from: Arc::from(from),
        to: to.to_string(),
        body: body.to_string(),
        reply_to: None,
        progress: false,
        wake_relay: false,
    })
    .await
    .expect("buffer one message");
}

#[tokio::test]
async fn send_reports_one_receipt_per_target() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    let _bus = BusGuard;
    install(
        IrcLimits::default(),
        Arc::new(FakeDirectory::with_broadcast(
            vec![("a", None), ("b", None)],
            vec!["a", "b"],
        )),
        Arc::new(FakeDeliver::injected()),
    );

    let output = IrcTool::new()
        .execute(json!({"action": "send", "to": "all", "message": "hello"}), ctx("root"))
        .await
        .expect("broadcast succeeds");

    let lines: Vec<&str> = output.output.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "one header plus one receipt per target: {}",
        output.output
    );
    assert!(lines[0].starts_with("message ") && lines[0].ends_with(" sent"));
    assert!(output.output.contains("a: injected"));
    assert!(output.output.contains("b: injected"));
}

#[tokio::test]
async fn partial_broadcast_failure_is_not_a_tool_error() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    let _bus = BusGuard;
    install(
        IrcLimits::default(),
        Arc::new(FakeDirectory::with_broadcast(
            vec![("a", None), ("b", None)],
            vec!["a", "b"],
        )),
        Arc::new(FakeDeliver::unavailable_for("b")),
    );

    let output = IrcTool::new()
        .execute(json!({"action": "send", "to": "all", "message": "hello"}), ctx("root"))
        .await
        .expect("one delivered leg keeps the call successful");
    assert!(output.output.contains("a: injected"));
    assert!(output.output.contains("b: failed (gone)"));
}

#[tokio::test]
async fn broadcast_with_no_targets_is_an_error_without_sent_line() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    let _bus = BusGuard;
    install(
        IrcLimits::default(),
        Arc::new(FakeDirectory::with_broadcast(vec![("root", None)], Vec::new())),
        Arc::new(FakeDeliver::injected()),
    );

    let error = IrcTool::new()
        .execute(json!({"action": "send", "to": "all", "message": "hello"}), ctx("root"))
        .await
        .expect_err("a broadcast with no visible targets is a tool error");

    let body = error.to_string();
    assert!(
        !body.starts_with("message "),
        "an empty broadcast must not claim the message was sent: {body}"
    );
    assert!(
        body.ends_with("no recipients: there is no visible agent to broadcast to"),
        "the error should name the missing recipients: {body}"
    );
}

#[tokio::test]
async fn wait_returns_the_reply_envelope() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    let _bus = BusGuard;
    let bus = install(
        IrcLimits::default(),
        Arc::new(FakeDirectory::peers(vec![("peer", None), ("me", None)])),
        Arc::new(FakeDeliver::errored_for("me")),
    );
    enqueue(&bus, "peer", "me", "pong").await;

    let output = IrcTool::new()
        .execute(json!({"action": "wait", "timeout_secs": 1}), ctx("me"))
        .await
        .expect("a buffered reply returns at once");
    assert!(output.output.starts_with("<irc "), "{}", output.output);
    assert!(output.output.contains("pong"));
    assert!(output.output.ends_with("</irc>"));
}

#[tokio::test]
async fn inbox_drains_buffered_messages_in_order() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    let _bus = BusGuard;
    let bus = install(
        IrcLimits::default(),
        Arc::new(FakeDirectory::peers(vec![("peer", None), ("me", None)])),
        Arc::new(FakeDeliver::errored_for("me")),
    );
    enqueue(&bus, "peer", "me", "first").await;
    enqueue(&bus, "peer", "me", "second").await;

    let output = IrcTool::new()
        .execute(json!({"action": "inbox"}), ctx("me"))
        .await
        .expect("inbox drains");
    let first = output.output.find("first").expect("first message");
    let second = output.output.find("second").expect("second message");
    assert!(first < second, "oldest message comes first: {}", output.output);
}

#[tokio::test]
async fn list_shows_visible_peers_with_unread_counts() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    let _bus = BusGuard;
    let bus = install(
        IrcLimits::default(),
        Arc::new(FakeDirectory::peers(vec![
            ("a", Some("alpha")),
            ("b", None),
            ("me", None),
        ])),
        Arc::new(FakeDeliver::errored_for("me")),
    );
    enqueue(&bus, "alpha", "me", "hi").await;

    let output = IrcTool::new()
        .execute(json!({"action": "list"}), ctx("me"))
        .await
        .expect("list runs");
    assert!(output.output.contains("a\talpha\tready\t-"), "{}", output.output);
    assert!(output.output.contains("b\t-\tready\t-"), "{}", output.output);
    assert!(output.output.ends_with("unread: 1"), "{}", output.output);
}

#[test]
fn irc_tool_runs_as_a_write_barrier() {
    let tool = IrcTool::new();
    assert_eq!(tool.concurrency_class(), ConcurrencyClass::Ordered);
    assert!(!tool.effects().parallel_safe());
}

#[tokio::test]
async fn missing_bus_returns_daemon_required_error() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    global::clear();

    let error = IrcTool::new()
        .execute(json!({"action": "list"}), ctx("me"))
        .await
        .expect_err("a tool outside the daemon cannot reach a bus");
    assert!(
        error.to_string().contains("not hosted by a horon-agent daemon"),
        "{error}"
    );
}

#[tokio::test]
async fn switch_off_refuses_execution() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = false\n");
    let _bus = BusGuard;
    install(
        IrcLimits::default(),
        Arc::new(FakeDirectory::peers(vec![("me", None)])),
        Arc::new(FakeDeliver::injected()),
    );

    let error = IrcTool::new()
        .execute(json!({"action": "list"}), ctx("me"))
        .await
        .expect_err("the switch is authoritative even with a bus installed");
    assert!(error.to_string().contains("IRC is off for this runtime"), "{error}");
}

#[tokio::test]
async fn oversized_message_is_a_tool_error() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    let _bus = BusGuard;
    install(
        IrcLimits {
            max_body_bytes: 8,
            ..IrcLimits::default()
        },
        Arc::new(FakeDirectory::peers(vec![("peer", None), ("me", None)])),
        Arc::new(FakeDeliver::injected()),
    );

    let error = IrcTool::new()
        .execute(
            json!({"action": "send", "to": "peer", "message": "this body is far too long"}),
            ctx("me"),
        )
        .await
        .expect_err("the bus rejects an oversized body");
    assert!(error.to_string().contains("over the 8 byte limit"), "{error}");
}

#[tokio::test]
async fn invalid_action_lists_valid_actions() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    let _bus = BusGuard;
    install(
        IrcLimits::default(),
        Arc::new(FakeDirectory::peers(vec![("me", None)])),
        Arc::new(FakeDeliver::injected()),
    );

    let error = IrcTool::new()
        .execute(json!({"action": "shout"}), ctx("me"))
        .await
        .expect_err("an unknown action is rejected");
    let message = error.to_string();
    assert!(message.contains("shout"), "{message}");
    for action in ["send", "wait", "inbox", "list"] {
        assert!(message.contains(action), "missing {action}: {message}");
    }
}

#[tokio::test]
async fn wait_with_nothing_owed_returns_immediately() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nirc = true\n");
    let _bus = BusGuard;
    install(
        IrcLimits::default(),
        Arc::new(FakeDirectory::peers(vec![("peer", None), ("me", None)])),
        Arc::new(FakeDeliver::injected()),
    );

    let output = tokio::time::timeout(
        Duration::from_secs(2),
        IrcTool::new().execute(json!({"action": "wait", "timeout_secs": 30}), ctx("me")),
    )
    .await
    .expect("nothing owed must not park the wait")
    .expect("wait returns a normal outcome");
    assert_eq!(
        output.output,
        "nothing to wait for: no one who owes you a reply can still answer"
    );
}
