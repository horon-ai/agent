#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::monitor::{self, MONITOR_TOOL, MonitorTool};
use super::{Registry, Tool, ToolContext, ToolExecutionMode};
use crate::provider::Provider;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        Err(anyhow::anyhow!("mock provider does not stream"))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockProvider)
    }
}

struct HomeGuard {
    _home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        crate::config::Config::invalidate_cache();
        Self { _home: home, previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            crate::env::set_var("HORON_HOME", previous);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
        crate::config::Config::invalidate_cache();
    }
}

fn context(session: &str, working_dir: Option<PathBuf>) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "message".to_string(),
        tool_call_id: "monitor-call".to_string(),
        working_dir,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

async fn call(tool: &MonitorTool, ctx: &ToolContext, input: Value) -> (String, Value, bool) {
    let output = tool.execute(input, ctx.clone()).await.expect("tool call");
    let metadata = output.metadata.unwrap_or(Value::Null);
    let is_error = metadata.get("error").is_some();
    (output.output, metadata, is_error)
}

#[test]
fn monitor_tool_is_registered_in_the_core_surface() {
    let _env = crate::storage::lock_test_env();
    let _guard = HomeGuard::new();
    assert!(
        crate::config::CORE_TOOLS.contains(&MONITOR_TOOL),
        "monitor must be in the core allow-list, got {:?}",
        crate::config::CORE_TOOLS
    );
    assert_eq!(crate::config::CORE_TOOLS.len(), 15);

    let registry = runtime().block_on(async { Registry::new(Arc::new(MockProvider) as Arc<dyn Provider>).await });
    runtime().block_on(async {
        let names = registry.tool_names().await;
        assert!(
            names.iter().any(|name| name == MONITOR_TOOL),
            "the registry must actually hold the monitor tool, got {names:?}"
        );
        assert_eq!(
            registry.concurrency_class(MONITOR_TOOL).await,
            super::ConcurrencyClass::Ordered,
            "a monitor writes state, so it never overlaps another call"
        );
    });
    let tool = MonitorTool::new();
    assert_eq!(tool.name(), MONITOR_TOOL);
    let schema = tool.parameters_schema();
    let actions: Vec<&str> = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action enum")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(actions, vec!["create", "list", "inspect", "cancel", "rearm"]);
    let kinds: Vec<&str> = schema["properties"]["kind"]["enum"]
        .as_array()
        .expect("kind enum")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(kinds, vec!["file", "command"]);
}

#[test]
fn monitor_tool_file_create_list_inspect_cancel() {
    let _env = crate::storage::lock_test_env();
    let _guard = HomeGuard::new();
    let workdir = tempfile::tempdir().expect("workdir");
    let watched = workdir.path().join("app.log");
    std::fs::write(&watched, "line one\n").expect("write");

    runtime().block_on(async {
        let tool = MonitorTool::new();
        let ctx = context("monitor-file-actions", Some(workdir.path().to_path_buf()));

        let (output, metadata, is_error) = call(
            &tool,
            &ctx,
            json!({
                "intent": "watch the log",
                "action": "create",
                "kind": "file",
                "path": watched.to_string_lossy(),
            }),
        )
        .await;
        assert!(!is_error, "create failed: {output}");
        let id = metadata["monitor"]["id"].as_str().expect("id").to_string();
        assert_eq!(metadata["monitor"]["status"], "running");
        assert_eq!(metadata["monitor"]["restart_policy"], "never");
        assert!(output.contains("created monitor"), "got {output}");

        let (output, metadata, is_error) = call(&tool, &ctx, json!({"intent": "list", "action": "list"})).await;
        assert!(!is_error, "{output}");
        assert_eq!(metadata["monitors"].as_array().map(Vec::len), Some(1));
        assert!(output.contains(&id), "got {output}");

        let (output, metadata, is_error) =
            call(&tool, &ctx, json!({"intent": "inspect", "action": "inspect", "id": id})).await;
        assert!(!is_error, "{output}");
        assert_eq!(metadata["monitor"]["id"], id.as_str());
        assert_eq!(metadata["monitor"]["kind"], "file");

        let (output, metadata, is_error) =
            call(&tool, &ctx, json!({"intent": "cancel", "action": "cancel", "id": id})).await;
        assert!(!is_error, "{output}");
        assert_eq!(metadata["monitor"]["status"], "stopped");

        let (_, _, is_error) = call(
            &tool,
            &ctx,
            json!({"intent": "inspect a stopped monitor", "action": "inspect", "id": "missing"}),
        )
        .await;
        assert!(is_error, "an unknown id is an error");
    });
}

#[test]
fn monitor_tool_rejects_malformed_input() {
    let _env = crate::storage::lock_test_env();
    let _guard = HomeGuard::new();
    let workdir = tempfile::tempdir().expect("workdir");
    let directory = workdir.path().join("a-directory");
    std::fs::create_dir(&directory).expect("dir");

    runtime().block_on(async {
        let tool = MonitorTool::new();
        let ctx = context("monitor-malformed", Some(workdir.path().to_path_buf()));

        let (output, metadata, is_error) = call(
            &tool,
            &ctx,
            json!({"intent": "relative path", "action": "create", "kind": "file", "path": "relative.log"}),
        )
        .await;
        assert!(
            !is_error,
            "a relative path resolves against the session directory: {output}"
        );
        let target = metadata["monitor"]["target"].as_str().unwrap_or_default();
        assert_eq!(
            Path::new(target),
            workdir
                .path()
                .canonicalize()
                .expect("canonical workdir")
                .join("relative.log"),
            "the tool resolves the path against the session working directory"
        );

        let (output, metadata, is_error) = call(
            &tool,
            &ctx,
            json!({"intent": "directory", "action": "create", "kind": "file", "path": directory.to_string_lossy()}),
        )
        .await;
        assert!(is_error, "a directory is refused: {output}");
        assert_eq!(metadata["error"], "invalid");
        assert!(metadata["message"].as_str().unwrap_or_default().contains("directory"));

        let (output, metadata, is_error) = call(
            &tool,
            &ctx,
            json!({"intent": "missing path", "action": "create", "kind": "file"}),
        )
        .await;
        assert!(is_error, "a file monitor without a path is refused: {output}");
        assert_eq!(metadata["error"], "invalid");

        let (output, metadata, is_error) = call(
            &tool,
            &ctx,
            json!({
                "intent": "restart policy",
                "action": "create",
                "kind": "command",
                "command": "true",
                "restart_policy": "always",
            }),
        )
        .await;
        assert!(is_error, "only `never` is accepted: {output}");
        assert_eq!(metadata["error"], "invalid");

        let (output, metadata, is_error) =
            call(&tool, &ctx, json!({"intent": "unknown action", "action": "poll"})).await;
        assert!(is_error, "{output}");
        assert_eq!(metadata["error"], "invalid");

        let (output, metadata, is_error) = call(&tool, &ctx, json!({"intent": "missing id", "action": "rearm"})).await;
        assert!(is_error, "{output}");
        assert_eq!(metadata["error"], "invalid");
    });
}

#[test]
fn monitor_tool_accepts_an_absolute_path_outside_the_working_directory() {
    let _env = crate::storage::lock_test_env();
    let _guard = HomeGuard::new();
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let watched = elsewhere.path().join("elsewhere.log");
    std::fs::write(&watched, "x\n").expect("write");

    runtime().block_on(async {
        let tool = MonitorTool::new();
        let ctx = context("monitor-absolute", None);
        let (output, metadata, is_error) = call(
            &tool,
            &ctx,
            json!({"intent": "absolute", "action": "create", "kind": "file", "path": watched.to_string_lossy()}),
        )
        .await;
        assert!(!is_error, "{output}");
        let target = metadata["monitor"]["target"].as_str().unwrap_or_default();
        assert!(
            Path::new(target).is_absolute(),
            "the recorded target is absolute: {target}"
        );
        assert!(target.ends_with("elsewhere.log"), "got {target}");
    });
}

#[test]
fn monitor_tool_file_change_becomes_one_wake() {
    let _env = crate::storage::lock_test_env();
    let _guard = HomeGuard::new();
    let workdir = tempfile::tempdir().expect("workdir");
    let watched = workdir.path().join("change.log");
    std::fs::write(&watched, "before\n").expect("write");

    runtime().block_on(async {
        let tool = MonitorTool::new();
        let ctx = context("monitor-wake", Some(workdir.path().to_path_buf()));
        let (output, metadata, is_error) = call(
            &tool,
            &ctx,
            json!({"intent": "wake me", "action": "create", "kind": "file", "path": watched.to_string_lossy()}),
        )
        .await;
        assert!(!is_error, "{output}");
        let id = metadata["monitor"]["id"].as_str().expect("id").to_string();

        std::fs::write(&watched, "before\nafter\n").expect("rewrite");
        let observed = monitor::tick_for_test("monitor-wake");
        assert!(observed.is_ok(), "tick failed: {observed:?}");

        let queued = horon_agent_base::soft_interrupt_store::load("monitor-wake").expect("load queue");
        assert_eq!(queued.len(), 1, "one change is one wake: {queued:?}");
        let content = &queued[0].content;
        assert!(content.contains("change.log"), "got {content}");
        assert!(content.contains("changed"), "got {content}");
        assert!(content.contains("wake me"), "the intent is echoed: {content}");
        assert!(
            queued[0].client_id.is_some(),
            "the wake carries its id as the source id"
        );

        let (_, metadata, is_error) =
            call(&tool, &ctx, json!({"intent": "inspect", "action": "inspect", "id": id})).await;
        assert!(!is_error);
        assert_eq!(metadata["monitor"]["wakes"], 1);
    });
}

#[test]
fn monitor_tool_command_runs_once_and_rearms_explicitly() {
    let _env = crate::storage::lock_test_env();
    let _guard = HomeGuard::new();
    let workdir = tempfile::tempdir().expect("workdir");
    let counter = workdir.path().join("counter.txt");
    let command = format!("printf 'run\\n' >> {}", counter.display());

    runtime().block_on(async {
        let tool = MonitorTool::new();
        let ctx = context("monitor-command", Some(workdir.path().to_path_buf()));
        let (output, metadata, is_error) = call(
            &tool,
            &ctx,
            json!({"intent": "count runs", "action": "create", "kind": "command", "command": command}),
        )
        .await;
        assert!(!is_error, "create failed: {output}");
        let id = metadata["monitor"]["id"].as_str().expect("id").to_string();
        assert!(metadata["pid"].as_u64().is_some(), "the create started one process");

        for _ in 0..300 {
            if std::fs::read_to_string(&counter).is_ok_and(|body| body.lines().count() == 1) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(&counter).expect("counter").lines().count(),
            1,
            "one create starts exactly one process"
        );

        let (output, metadata, is_error) =
            call(&tool, &ctx, json!({"intent": "cancel", "action": "cancel", "id": id})).await;
        assert!(!is_error, "{output}");
        assert_eq!(metadata["monitor"]["status"], "stopped");

        let (output, metadata, is_error) =
            call(&tool, &ctx, json!({"intent": "rearm", "action": "rearm", "id": id})).await;
        assert!(!is_error, "{output}");
        assert_eq!(metadata["monitor"]["status"], "running");
        for _ in 0..300 {
            if std::fs::read_to_string(&counter).is_ok_and(|body| body.lines().count() == 2) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(&counter).expect("counter").lines().count(),
            2,
            "the explicit rearm ran it exactly once more"
        );
    });
}

#[test]
fn monitor_tool_denies_a_command_the_bash_gate_refuses() {
    let _env = crate::storage::lock_test_env();
    let _guard = HomeGuard::new();

    runtime().block_on(async {
        let tool = MonitorTool::new();
        let ctx = context("monitor-denied", None);
        let (output, metadata, is_error) = call(
            &tool,
            &ctx,
            json!({
                "intent": "try to wipe the root",
                "action": "create",
                "kind": "command",
                "command": "rm -rf /",
            }),
        )
        .await;
        assert!(is_error, "the bash gate must refuse this: {output}");
        assert_eq!(metadata["error"], "invalid");
        assert!(
            metadata["message"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains("blocked")
                || output.to_lowercase().contains("blocked"),
            "got {output}"
        );

        let (output, metadata, is_error) =
            call(&tool, &ctx, json!({"intent": "list after a refusal", "action": "list"})).await;
        assert!(!is_error, "{output}");
        let monitors = metadata["monitors"].as_array().expect("monitors");
        assert!(
            monitors.iter().all(|monitor| monitor["status"] != "running"),
            "a refused command leaves nothing running: {monitors:?}"
        );
    });
}

#[test]
fn monitor_tool_restores_a_file_monitor_from_the_journal() {
    let _env = crate::storage::lock_test_env();
    let _guard = HomeGuard::new();
    let workdir = tempfile::tempdir().expect("workdir");
    let watched = workdir.path().join("restored.log");
    std::fs::write(&watched, "before\n").expect("write");

    runtime().block_on(async {
        let tool = MonitorTool::new();
        let ctx = context("monitor-restore", Some(workdir.path().to_path_buf()));
        let (output, _, is_error) = call(
            &tool,
            &ctx,
            json!({"intent": "survive a restart", "action": "create", "kind": "file", "path": watched.to_string_lossy()}),
        )
        .await;
        assert!(!is_error, "{output}");
    });

    monitor::forget_session("monitor-restore");
    std::fs::write(&watched, "before\nafter the restart\n").expect("offline change");

    runtime().block_on(async {
        let tool = MonitorTool::new();
        let ctx = context("monitor-restore", Some(workdir.path().to_path_buf()));
        let (output, metadata, is_error) = call(&tool, &ctx, json!({"intent": "list", "action": "list"})).await;
        assert!(!is_error, "{output}");
        let monitors = metadata["monitors"].as_array().expect("monitors");
        assert_eq!(monitors.len(), 1, "the monitor came back: {monitors:?}");
        assert_eq!(monitors[0]["status"], "running");

        monitor::tick_for_test("monitor-restore").expect("tick after the restore");
        let queued = horon_agent_base::soft_interrupt_store::load("monitor-restore").expect("load");
        assert_eq!(queued.len(), 1, "one coalesced wake: {queued:?}");
        assert!(queued[0].content.contains("restored.log"), "got {}", queued[0].content);
        assert!(queued[0].content.contains("changed"), "got {}", queued[0].content);
    });
}
