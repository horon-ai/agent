//! A user takeover must actually stop the browser tool.
//!
//! The tool reads the mode the API bridge pushed into the daemon's cache, so
//! these tests drive the real `BrowserTool::execute` against a stub bridge CLI
//! and assert what did or did not reach it.
#![allow(clippy::await_holding_lock)]
use super::*;
use crate::protocol::BrowserMode;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

const FAKE_CLI: &str = r#"#!/bin/sh
case "$1" in
  ping) printf '%s\n' '{"pong":true,"extensionVersion":"0.10.0","browser":"chrome"}' ;;
  *)
    printf '%s\n' "$1" >> "$HORON_AGENT_HOME/calls"
    printf '%s\n' '{"ok":true}'
    ;;
esac
"#;

struct Fixture {
    _env_guard: std::sync::MutexGuard<'static, ()>,
    temp: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
    previous_browser: Option<std::ffi::OsString>,
    previous_session: Option<std::ffi::OsString>,
}

impl Fixture {
    fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().unwrap();
        let previous_home = std::env::var_os("HORON_AGENT_HOME");
        let previous_browser = std::env::var_os("HORON_AGENT_BROWSER");
        let previous_session = std::env::var_os("BROWSER_SESSION");
        crate::env::set_var("HORON_AGENT_HOME", temp.path());
        crate::env::set_var("HORON_AGENT_BROWSER", "chrome");
        // A pinned session makes the tool talk to the CLI directly instead of
        // starting a browser session daemon.
        crate::env::set_var("BROWSER_SESSION", "browser-mode-gate-test");
        let bin = temp.path().join("browser/browser");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, FAKE_CLI).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _env_guard: guard,
            temp,
            previous_home,
            previous_browser,
            previous_session,
        }
    }

    fn context(&self, name: &str) -> ToolContext {
        ToolContext {
            session_id: format!("browser-gate-{}-{name}", std::process::id()),
            message_id: "m".into(),
            tool_call_id: "t".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        }
    }

    /// Actions the stub bridge was asked to perform, in order.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.temp.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for (key, previous) in [
            ("HORON_AGENT_HOME", &self.previous_home),
            ("HORON_AGENT_BROWSER", &self.previous_browser),
            ("BROWSER_SESSION", &self.previous_session),
        ] {
            match previous {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
    }
}

fn click() -> Value {
    json!({"action": "click", "selector": "#buy", "browser": "chrome"})
}

#[tokio::test]
async fn a_user_takeover_refuses_a_driving_action_with_the_reason() {
    let fixture = Fixture::new();
    let ctx = fixture.context("refused");
    crate::browser_mode::set(&ctx.session_id, BrowserMode::User);

    let error = BrowserTool::new()
        .execute(click(), ctx.clone())
        .await
        .expect_err("a user takeover must refuse a driving action");
    let message = format!("{error:#}");

    assert!(
        message.contains("The user has taken direct control of the browser"),
        "{message}"
    );
    assert!(message.contains("`click` was refused"), "{message}");
    assert!(message.contains("resume"), "{message}");
    assert!(message.contains("Read-only actions still work"), "{message}");
    assert!(
        fixture.calls().is_empty(),
        "the refusal must come before anything reaches the browser: {:?}",
        fixture.calls()
    );
}

#[tokio::test]
async fn a_read_only_action_still_works_during_a_takeover() {
    let fixture = Fixture::new();
    let ctx = fixture.context("read-only");
    crate::browser_mode::set(&ctx.session_id, BrowserMode::User);

    let output = BrowserTool::new()
        .execute(json!({"action": "status", "browser": "chrome"}), ctx)
        .await
        .expect("status only observes, so a takeover must not block it");

    assert!(
        output.output.contains("responding"),
        "status must still report the bridge: {}",
        output.output
    );
}

#[tokio::test]
async fn a_handoff_is_refused_during_a_takeover() {
    let fixture = Fixture::new();
    let ctx = fixture.context("handoff");
    crate::browser_mode::set(&ctx.session_id, BrowserMode::User);

    let error = BrowserTool::new()
        .execute(
            json!({"action": "handoff", "goal": "buy the item", "tab_id": 7, "browser": "chrome"}),
            ctx,
        )
        .await
        .expect_err("the fast browser agent must not start while the user drives");
    let message = format!("{error:#}");

    assert!(
        message.contains("The user has taken direct control of the browser"),
        "{message}"
    );
    assert!(fixture.calls().is_empty(), "{:?}", fixture.calls());
}

#[tokio::test]
async fn a_resume_allows_the_driving_action_again() {
    let fixture = Fixture::new();
    let ctx = fixture.context("resumed");
    crate::browser_mode::set(&ctx.session_id, BrowserMode::User);
    crate::browser_mode::set(&ctx.session_id, BrowserMode::Agent);

    let output = BrowserTool::new()
        .execute(click(), ctx)
        .await
        .expect("a resume returns control immediately");

    assert!(output.output.contains("ok"), "{}", output.output);
    assert_eq!(fixture.calls(), vec!["click".to_string()]);
}

#[tokio::test]
async fn a_takeover_older_than_the_lease_allows_the_driving_action() {
    let fixture = Fixture::new();
    let ctx = fixture.context("stale");
    crate::browser_mode::set_at_for_test(
        &ctx.session_id,
        BrowserMode::User,
        Instant::now() - crate::browser_mode::LEASE - Duration::from_secs(1),
    );

    BrowserTool::new()
        .execute(click(), ctx)
        .await
        .expect("a vanished panel must not leave the browser locked");
    assert_eq!(fixture.calls(), vec!["click".to_string()]);
}

#[test]
fn observation_actions_are_exactly_the_ones_that_do_not_drive() {
    for action in OBSERVATION_ACTIONS {
        assert!(
            !matches!(
                *action,
                "handoff"
                    | "open"
                    | "new_tab"
                    | "select_tab"
                    | "click"
                    | "type"
                    | "fill_form"
                    | "select"
                    | "eval"
                    | "scroll"
                    | "upload"
                    | "press"
                    | "provider_command"
            ),
            "{action} changes the page and cannot be observation-only"
        );
    }
    let session = format!("browser-gate-schema-{}", std::process::id());
    crate::browser_mode::set(&session, BrowserMode::User);
    for action in ["click", "open", "eval", "press", "provider_command"] {
        assert!(driving_refusal(&session, action).is_some(), "{action} must be gated");
    }
    for action in ["status", "snapshot", "screenshot", "wait", "list_tabs"] {
        assert!(driving_refusal(&session, action).is_none(), "{action} must stay usable");
    }
}
