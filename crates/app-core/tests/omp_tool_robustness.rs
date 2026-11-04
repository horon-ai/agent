//! MCP robustness, proven against a real stdio server process.
//!
//! Two behaviours a session depends on and neither of which the unit tests can
//! prove: a server that announces `notifications/tools/list_changed` reaches the
//! registry without a manual reload, and a server that dies is reconnected by
//! the background ladder rather than by the next tool call.
//!
//! Every case runs with a private `HORON_AGENT_HOME`/`HOME`, because the MCP
//! config is loaded from them; without isolation a developer's real
//! `~/.claude.json` would inject servers into the test.

use async_trait::async_trait;
use horon_agent_app_core::message::{Message, ToolDefinition};
use horon_agent_app_core::provider::{EventStream, Provider};
use horon_agent_app_core::tool::{Registry, ToolContext, ToolExecutionMode};
use serde_json::Value;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Serializes the two cases: both install process-wide environment variables.
static TEST_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The fixture server. `alpha` adds `beta` to its catalog and announces the
/// change; `die` answers and then exits so the transport closes.
const SERVER: &str = r#"
import json, os, sys

tools = ["alpha"]
pid = os.getpid()

def reply(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    if method is None or "id" not in req:
        continue
    rid = req["id"]
    if method == "initialize":
        reply({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {"listChanged": True}},
            "serverInfo": {"name": "notify", "version": "1"}}})
    elif method == "tools/list":
        reply({"jsonrpc": "2.0", "id": rid, "result": {"tools": [
            {"name": name, "description": name,
             "inputSchema": {"type": "object", "properties": {}}}
            for name in tools]}})
    elif method == "tools/call":
        name = req["params"]["name"]
        if name == "alpha" and "beta" not in tools:
            tools.append("beta")
            reply({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
        if name == "die":
            reply({"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": "bye"}], "isError": False}})
            sys.exit(0)
        reply({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": json.dumps({"pid": pid, "tool": name})}],
            "isError": False}})
    else:
        reply({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "unknown"}})
"#;

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Err(anyhow::anyhow!("the stub provider never completes a request"))
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }
}

struct IsolatedEnv {
    home: tempfile::TempDir,
    /// Lives for the whole test so `HORON_AGENT_RUNTIME_DIR` stays valid.
    _runtime: tempfile::TempDir,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl IsolatedEnv {
    fn new(label: &str) -> Self {
        let home = tempfile::Builder::new()
            .prefix(&format!("horon-mcp-{label}-home-"))
            .tempdir()
            .expect("a private home");
        let runtime = tempfile::Builder::new()
            .prefix(&format!("horon-mcp-{label}-runtime-"))
            .tempdir()
            .expect("a private runtime directory");
        let previous = [
            "HORON_AGENT_HOME",
            "HORON_AGENT_RUNTIME_DIR",
            "HOME",
            "HORON_AGENT_DISABLE_CLAUDE_MCP",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
        horon_agent_app_core::env::set_var("HORON_AGENT_HOME", home.path());
        horon_agent_app_core::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        horon_agent_app_core::env::set_var("HOME", home.path());
        horon_agent_app_core::env::set_var("HORON_AGENT_DISABLE_CLAUDE_MCP", "1");
        horon_agent_app_core::config::invalidate_config_cache();
        Self {
            home,
            _runtime: runtime,
            previous,
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => horon_agent_app_core::env::set_var(key, value),
                None => horon_agent_app_core::env::remove_var(key),
            }
        }
        horon_agent_app_core::config::invalidate_config_cache();
    }
}

fn write_mcp_config(home: &Path, server: &str) {
    let config = serde_json::json!({
        "mcpServers": {
            server: {
                "command": "python3",
                "args": ["-I", "-S", "-u", "-c", SERVER],
                "shared": false,
                "timeout_secs": 10
            }
        }
    });
    std::fs::write(home.join("mcp.json"), serde_json::to_string(&config).unwrap()).expect("the MCP config is written");
}

fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn context(session: &str, working_dir: &Path) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: format!("{session}-message"),
        tool_call_id: format!("{session}-call"),
        working_dir: Some(working_dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// Poll `check` until it is true or `timeout` elapses.
async fn wait_until<F>(timeout: Duration, mut check: F) -> bool
where
    F: FnMut() -> Pin<Box<dyn Future<Output = bool> + Send>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if check().await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn mcp_list_changed_exposes_the_new_tool_without_reload() {
    let _env_guard = TEST_ENV.lock().await;
    if !python_available() {
        eprintln!("SKIP: python3 is unavailable for the MCP stdio fixture");
        return;
    }
    let env = IsolatedEnv::new("list-changed");
    write_mcp_config(env.home(), "notify");

    let registry = Registry::new(Arc::new(StubProvider)).await;
    registry
        .register_mcp_tools_for_dir(None, None, Some("list-changed".to_string()), None)
        .await;

    let ctx = context("list-changed", env.home());
    let first_tool = "mcp__notify__alpha";
    let registry_for_wait = registry.clone();
    assert!(
        wait_until(Duration::from_secs(20), || {
            let registry = registry_for_wait.clone();
            Box::pin(async move { registry.tool_names().await.iter().any(|name| name == first_tool) })
        })
        .await,
        "the fixture server's first tool must be registered"
    );
    assert!(
        !registry
            .tool_names()
            .await
            .iter()
            .any(|name| name == "mcp__notify__beta"),
        "beta must not exist before the server announces it"
    );

    // Calling `alpha` makes the server add `beta` and send
    // `notifications/tools/list_changed`.
    registry
        .execute(first_tool, serde_json::json!({}), ctx.clone())
        .await
        .expect("the alpha call is answered");

    let registry_for_wait = registry.clone();
    assert!(
        wait_until(Duration::from_secs(20), || {
            let registry = registry_for_wait.clone();
            Box::pin(async move {
                registry
                    .tool_names()
                    .await
                    .iter()
                    .any(|name| name == "mcp__notify__beta")
            })
        })
        .await,
        "notifications/tools/list_changed must expose the new tool without a reload"
    );

    // And the new tool is dispatchable, not just listed.
    let output = registry
        .execute("mcp__notify__beta", serde_json::json!({}), ctx)
        .await
        .expect("the newly announced tool is callable");
    let payload: Value = serde_json::from_str(&output.output).unwrap_or(Value::Null);
    assert_eq!(
        payload.get("tool").and_then(Value::as_str),
        Some("beta"),
        "{}",
        output.output
    );
}

#[tokio::test]
async fn mcp_stdio_server_that_exits_is_reconnected_by_the_ladder() {
    let _env_guard = TEST_ENV.lock().await;
    if !python_available() {
        eprintln!("SKIP: python3 is unavailable for the MCP stdio fixture");
        return;
    }
    let env = IsolatedEnv::new("reconnect");
    write_mcp_config(env.home(), "notify");

    let manager = Arc::new(tokio::sync::RwLock::new(
        horon_agent_app_core::mcp::McpManager::with_config(horon_agent_app_core::mcp::McpConfig::load_for_dir(Some(
            env.home(),
        ))),
    ));
    let (_, failures) = manager.write().await.connect_all().await.expect("connect_all");
    assert!(failures.is_empty(), "the fixture server must connect: {failures:?}");

    let first: Value = serde_json::from_str(
        &manager
            .read()
            .await
            .call_tool("notify", "alpha", serde_json::json!({}))
            .await
            .expect("the first call is answered")
            .content
            .first()
            .and_then(|block| match block {
                horon_agent_app_core::mcp::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .expect("text content"),
    )
    .expect("pid payload");
    let first_pid = first["pid"].as_i64().expect("pid");

    // The server exits after answering this call.
    manager
        .read()
        .await
        .call_tool("notify", "die", serde_json::json!({}))
        .await
        .expect("the die call is answered");

    let manager_for_wait = Arc::clone(&manager);
    assert!(
        wait_until(Duration::from_secs(20), || {
            let manager = Arc::clone(&manager_for_wait);
            Box::pin(async move {
                !manager
                    .read()
                    .await
                    .connected_servers()
                    .await
                    .iter()
                    .any(|s| s == "notify")
            })
        })
        .await,
        "the manager must notice the transport closed"
    );

    // No tool call is made here: the background ladder is what must reconnect.
    let manager_for_wait = Arc::clone(&manager);
    assert!(
        wait_until(Duration::from_secs(20), || {
            let manager = Arc::clone(&manager_for_wait);
            Box::pin(async move {
                manager
                    .read()
                    .await
                    .connected_servers()
                    .await
                    .iter()
                    .any(|s| s == "notify")
            })
        })
        .await,
        "the reconnect ladder must restore the connection"
    );

    let second = manager
        .read()
        .await
        .call_tool("notify", "alpha", serde_json::json!({}))
        .await
        .expect("the reconnected server answers");
    let text = second
        .content
        .first()
        .and_then(|block| match block {
            horon_agent_app_core::mcp::ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("text content");
    let payload: Value = serde_json::from_str(&text).expect("pid payload");
    assert!(
        payload["pid"].as_i64().is_some_and(|pid| pid != first_pid),
        "a reconnected server runs in a new process: {payload}"
    );
}
