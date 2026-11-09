// The env lock must outlive the awaited tool call; safe under the
// single-threaded test runtime (same allowance as `tool/read.rs`).
#![allow(clippy::await_holding_lock)]

use super::*;
use crate::tool::{Tool, ToolContext, ToolExecutionMode};
use serde_json::json;

/// Points `HORON_HOME` at a fresh directory for one test and restores it.
pub(super) struct IsolatedHome {
    /// Holds the temporary directory alive for the test's whole scope.
    pub(super) _dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl IsolatedHome {
    pub(super) fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::config::Config::invalidate_cache();
        Self {
            _dir: dir,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn ctx(session_id: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "m".to_string(),
        tool_call_id: "c".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

#[test]
fn another_session_cannot_read_an_artifact() {
    let _home = IsolatedHome::new();
    let id = store("session-a", "bash", "secret build log").expect("store");
    assert_eq!(load("session-a", id).unwrap(), "secret build log");
    let error = load("session-b", id).expect_err("other session").to_string();
    assert!(error.contains("not found for this session"), "{error}");
}

#[tokio::test]
async fn read_tool_pages_through_an_artifact() {
    let _home = IsolatedHome::new();
    let id = store("session-a", "bash", "one\ntwo\nthree\nfour\n").unwrap();
    let tool = crate::tool::read::ReadTool::new();
    let url = format!("artifact://{id}");
    assert!(tool.effects_for_input(&json!({"file_path": url})) == crate::tool::ToolEffects::read());
    let output = tool
        .execute(
            json!({"file_path": url, "start_line": 2, "end_line": 3}),
            ctx("session-a"),
        )
        .await
        .expect("artifact read");
    assert_eq!(
        output.output,
        "    2\ttwo\n    3\tthree\n\n... 1 more lines (use start_line=4 to continue)\n"
    );

    let foreign = tool
        .execute(json!({"file_path": url}), ctx("session-b"))
        .await
        .expect_err("other session");
    assert!(foreign.to_string().contains("not found for this session"), "{foreign}");
}
