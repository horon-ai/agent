//! Fixtures for package-declared MCP servers (issue #648).
//!
//! Each scenario installs a real plugin into a private horon home. The plugin's
//! executable is this target's sibling binary `horon-package-probe`, so the
//! installer validates it and the host starts a real supervised child in `mcp`
//! mode; every answer carries the child's pid so a scenario can tell a fresh
//! child from the one it replaced.
//!
//! Two servers are covered, because the contract differs:
//!
//! - `stdio` runs `horon-package-probe` through package supervision, and every
//!   answer carries the child's pid so a scenario can tell a fresh child from
//!   the one it replaced;
//! - `remote` only refers to a user server, which [`RemoteServer`] stands in for:
//!   a loopback Streamable-HTTP MCP endpoint the scenario writes into the user's
//!   own `mcp.json`.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use horon_agent_app_core::agent::Agent;
use horon_agent_app_core::extensions::lifecycle::{PackageLifecycleRegistry, RevocationCause};
use horon_agent_app_core::mcp::dispatch_name;
use horon_agent_app_core::message::{Message, ToolDefinition};
use horon_agent_app_core::packages::{InstallRequest, PackageRecord, PackageScope, PackageStore};
use horon_agent_app_core::provider::{EventStream, Provider};
use horon_agent_app_core::session::Session;
use horon_agent_app_core::tool::{Registry, ToolOutput, ToolRejected};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The executable the fixtures install as the package binary.
const PROBE: &str = env!("CARGO_BIN_EXE_horon-package-probe");
/// Outer safety deadline while awaiting a registration or teardown event.
const CONNECT_DEADLINE: Duration = Duration::from_secs(60);
/// How long a scenario insists that nothing appears, for a negative case.
const QUIET_WINDOW: Duration = Duration::from_millis(750);
/// Definition file of the fixture plugin.
const DEFINITIONS: &str = "resources/mcp/servers.json";

// ---------------------------------------------------------------------------
// Plugin fixtures
// ---------------------------------------------------------------------------

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("package_mcp needs a supported runner, got {os}/{arch}"),
    }
}

fn binary_name() -> &'static str {
    if std::env::consts::OS == "windows" {
        "bin/horon-package-probe.exe"
    } else {
        "bin/horon-package-probe"
    }
}

/// A `stdio` entry that runs `horon-package-probe` in `mcp` mode.
fn stdio_definition() -> Value {
    json!({
        "type": "stdio",
        "mode": "mcp"
    })
}

/// A `remote` entry that refers to a server the user configured.
fn remote_definition(user_server: &str) -> Value {
    json!({ "type": "remote", "server": user_server })
}

/// Write a plugin fixture whose declared servers are `servers`.
///
/// The executable is `horon-package-probe`, staged under the extension's target
/// directory exactly like a released package, so `validate_package` checks it as
/// it checks any other package.
fn plugin_fixture(id: &str, version: &str, servers: Value) -> TempDir {
    let root = tempdir().expect("fixture directory");
    let binary = binary_name();
    let destination = root.path().join("native").join(binary);
    fs::create_dir_all(destination.parent().expect("the fixture binary directory"))
        .expect("create the fixture binary directory");
    fs::copy(PROBE, &destination).expect("stage the probe as the package executable");
    horon_agent_platform::set_permissions_executable(&destination).expect("mark the package executable runnable");
    fs::write(
        root.path().join("native/extension.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
version = "{version}"
name = "MCP {id}"
protocol = "1.0"
[targets]
{target} = "{binary}"
"#,
            target = host_target(),
        ),
    )
    .expect("write extension.toml");
    fs::create_dir_all(root.path().join("resources/mcp")).expect("create the definition directory");
    fs::write(
        root.path().join(DEFINITIONS),
        serde_json::to_string_pretty(&json!({ "mcpServers": servers })).expect("serialize the definitions"),
    )
    .expect("write the MCP definitions");
    fs::write(
        root.path().join("plugin.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
version = "{version}"
name = "Plugin {id}"
extension = "native/extension.toml"
[resources]
mcp = ["{DEFINITIONS}"]
"#
        ),
    )
    .expect("write plugin.toml");
    root
}

// ---------------------------------------------------------------------------
// A stand-in for a user-configured remote MCP server
// ---------------------------------------------------------------------------

/// A Streamable-HTTP MCP endpoint on loopback.
///
/// It answers every request the client sends and accepts every notification, so
/// a package `remote` entry can be connected through a real user connection
/// config instead of the package naming an endpoint of its own.
struct RemoteServer {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl RemoteServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake MCP server");
        let address = listener.local_addr().expect("the fake MCP server address");
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let Some(body) = read_request_body(&mut stream).await else {
                        return;
                    };
                    let frame: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let result = match frame.get("method").and_then(Value::as_str) {
                        Some("initialize") => Some(json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "fake-clock", "version": "0" }
                        })),
                        Some("tools/list") => Some(json!({
                            "tools": [{
                                "name": "tick",
                                "description": "Report the user's clock",
                                "inputSchema": { "type": "object", "properties": {} }
                            }]
                        })),
                        Some("tools/call") => Some(json!({
                            "content": [{ "type": "text", "text": "tick" }]
                        })),
                        _ => None,
                    };
                    let reply = match (frame.get("id"), result) {
                        (Some(id), Some(result)) => {
                            let body = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        }
                        // A notification is accepted without a body.
                        _ => "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string(),
                    };
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let _ = stream.flush().await;
                });
            }
        });
        Self {
            url: format!("http://{address}/mcp"),
            task,
        }
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Read one HTTP request's body, or `None` when the connection ended early.
async fn read_request_body(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut received = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&received[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let body_start = end + 4;
            while received.len() < body_start + length {
                let read = stream.read(&mut chunk).await.ok()?;
                if read == 0 {
                    return None;
                }
                received.extend_from_slice(&chunk[..read]);
            }
            return String::from_utf8(received[body_start..body_start + length].to_vec()).ok();
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        received.extend_from_slice(&chunk[..read]);
    }
}

// ---------------------------------------------------------------------------
// Scenario harness
// ---------------------------------------------------------------------------

/// A provider for building an agent. No scenario gets a model answer: the turn
/// boundary these cases test runs before the first request.
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
        Err(anyhow::anyhow!("the stub provider never answers"))
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }
}

/// An isolated home, its package area, and the user's own MCP config.
struct Scenario {
    env: super::IsolatedEnv,
    store: PackageStore,
}

impl Scenario {
    /// A scenario on a fresh home. The caller holds the environment lock, so the
    /// process-wide `HORON_HOME` this reads belongs to this case.
    fn new() -> Self {
        let env = super::IsolatedEnv::new();
        let store = PackageStore::for_horon_home(env.home());
        Self { env, store }
    }

    fn home(&self) -> &Path {
        self.env.home()
    }

    fn store(&self) -> &PackageStore {
        &self.store
    }

    fn install(&self, source: &Path, scope: PackageScope, replace: bool) -> PackageRecord {
        let request = InstallRequest::new(source, scope);
        self.store
            .install(if replace { request.replacing() } else { request })
            .expect("install the plugin fixture")
    }

    /// Write the user's own server map, which a `remote` entry may refer to.
    fn write_user_mcp(&self, servers: Value) {
        fs::write(
            self.home().join("mcp.json"),
            serde_json::to_string_pretty(&json!({ "mcpServers": servers })).expect("serialize the user config"),
        )
        .expect("write the user mcp.json");
    }
}

/// One session whose turns capture the scenario's package generation.
struct Runner {
    agent: Agent,
}

impl Runner {
    fn new(registry: &Registry, allowed: Option<HashSet<String>>, workspace: Option<&Path>) -> Self {
        let mut session = Session::create(None, Some("package mcp".to_string()));
        session.working_dir = workspace.map(|dir| dir.display().to_string());
        let agent = Agent::new_with_session(Arc::new(StubProvider), registry.clone(), session, allowed);
        Self { agent }
    }

    /// Run one turn, which captures the package generation and refreshes the
    /// package MCP servers before the tool surface freezes. The stub provider
    /// cannot answer, so the turn itself reports an error; that happens after the
    /// boundary this fixture is about.
    async fn turn(&mut self) {
        let _ = self.agent.run_once("hello").await;
    }

    /// Call one registry alias on this session, the way a model would.
    async fn call(&self, working_dir: &Path, alias: &str, arguments: Value) -> anyhow::Result<ToolOutput> {
        let context = super::context(self.agent.session_id(), working_dir);
        self.agent.registry().execute(alias, arguments, context).await
    }
}

/// The pid one probe answer carries.
fn pid_of(output: &str) -> u32 {
    output
        .split_whitespace()
        .find_map(|part| part.strip_prefix("pid="))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no pid in the probe answer: {output:?}"))
}

/// Wait until `alias` is registered.
async fn tool_appears(registry: &Registry, alias: &str) -> bool {
    wait_for_tool(registry, alias, CONNECT_DEADLINE, true).await
}

/// Wait until `alias` is gone.
async fn tool_disappears(registry: &Registry, alias: &str) -> bool {
    wait_for_tool(registry, alias, CONNECT_DEADLINE, false).await
}

async fn wait_for_tool(registry: &Registry, alias: &str, limit: Duration, present: bool) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    let mut changes = registry.subscribe_mcp_surface();
    loop {
        let registered = registry.tool_names().await.iter().any(|name| name == alias);
        if registered == present {
            return true;
        }
        match tokio::time::timeout_at(deadline, changes.changed()).await {
            Ok(Ok(())) => continue,
            Ok(Err(_)) | Err(_) => return registry.tool_names().await.iter().any(|name| name == alias) == present,
        }
    }
}

/// Call `alias` until it answers with a pid other than `previous`, and return it.
///
/// A replaced package's child connects in the background after the turn, so this
/// waits on MCP surface registration events until the new child answers.
async fn pid_after_replace(runner: &Runner, working_dir: &Path, alias: &str, previous: u32) -> u32 {
    let registry = runner.agent.registry();
    let mut changes = registry.subscribe_mcp_surface();
    let deadline = tokio::time::Instant::now() + CONNECT_DEADLINE;
    loop {
        if let Ok(output) = runner.call(working_dir, alias, json!({ "text": "after" })).await {
            let pid = pid_of(&output.output);
            if pid != previous {
                return pid;
            }
        }
        assert!(
            matches!(tokio::time::timeout_at(deadline, changes.changed()).await, Ok(Ok(()))),
            "the alias kept answering from pid {previous}"
        );
    }
}

/// Wait until `pid` no longer runs. The wait yields to the runtime: the
/// package's teardown is a task on this single-threaded test runtime.
async fn wait_for_exit(pid: u32) {
    let deadline = Instant::now() + CONNECT_DEADLINE;
    while horon_agent_platform::is_process_running(pid) {
        assert!(Instant::now() < deadline, "package child {pid} kept running");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The alias of one declared server's tool.
fn alias_of(package_id: &str, local: &str, tool: &str) -> String {
    dispatch_name(&format!("plugin_{package_id}_{local}"), tool)
}

/// Whether `error` is the registry refusing the call by policy.
fn is_policy_rejection(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ToolRejected>().is_some()
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// A plugin's stdio server is registered under `plugin_<id>_<name>` and answers
/// a call with the pid of the supervised package child it runs.
#[tokio::test]
async fn package_mcp_stdio_server_registers_and_answers() {
    let _guard = super::TEST_ENV.lock().await;
    let scenario = Scenario::new();
    let fixture = plugin_fixture("mcp-serve", "1.0.0", json!({ "echo": stdio_definition() }));
    scenario.install(fixture.path(), PackageScope::Global, false);
    let registry = Registry::empty();
    let mut runner = Runner::new(&registry, None, None);
    runner.turn().await;

    let alias = alias_of("mcp-serve", "echo", "echo");
    assert!(
        tool_appears(&registry, &alias).await,
        "the package server was not registered: {:?}",
        registry.tool_names().await
    );
    let output = runner
        .call(scenario.home(), &alias, json!({ "text": "hi" }))
        .await
        .expect("the package tool answers");
    assert!(output.output.starts_with("pid="), "{}", output.output);
    assert!(output.output.ends_with("text=hi"), "{}", output.output);
}

/// A `remote` entry reaches the user's own connection config and nothing else:
/// the package names a server, the user owns its endpoint.
#[tokio::test]
async fn package_mcp_remote_reference_uses_the_user_connection() {
    let _guard = super::TEST_ENV.lock().await;
    let scenario = Scenario::new();
    let remote = RemoteServer::start().await;
    scenario.write_user_mcp(json!({ "corp-clock": { "type": "http", "url": remote.url.clone() } }));
    let fixture = plugin_fixture(
        "mcp-remote",
        "1.0.0",
        json!({ "clock": remote_definition("corp-clock") }),
    );
    scenario.install(fixture.path(), PackageScope::Global, false);
    let registry = Registry::empty();
    let mut runner = Runner::new(&registry, None, None);
    runner.turn().await;

    let alias = alias_of("mcp-remote", "clock", "tick");
    assert!(
        tool_appears(&registry, &alias).await,
        "the user-referenced server was not registered: {:?}",
        registry.tool_names().await
    );
    let output = runner
        .call(scenario.home(), &alias, json!({}))
        .await
        .expect("the referenced server answers");
    assert_eq!(output.output, "tick");
}

/// A reference the user does not define registers nothing, while a sibling stdio
/// entry of the same plugin still registers: the package cannot conjure a
/// connection the user never configured.
#[tokio::test]
async fn package_mcp_unresolved_remote_reference_registers_nothing() {
    let _guard = super::TEST_ENV.lock().await;
    let scenario = Scenario::new();
    let fixture = plugin_fixture(
        "mcp-dangling",
        "1.0.0",
        json!({
            "echo": stdio_definition(),
            "clock": remote_definition("corp-clock")
        }),
    );
    scenario.install(fixture.path(), PackageScope::Global, false);
    let registry = Registry::empty();
    let mut runner = Runner::new(&registry, None, None);
    runner.turn().await;

    let control = alias_of("mcp-dangling", "echo", "echo");
    assert!(
        tool_appears(&registry, &control).await,
        "the plugin's stdio server did not register: {:?}",
        registry.tool_names().await
    );
    let dangling = alias_of("mcp-dangling", "clock", "tick");
    assert!(
        !wait_for_tool(&registry, &dangling, QUIET_WINDOW, true).await,
        "an undefined user connection was registered: {dangling}"
    );
}

/// A package tool is a normal tool: the session's allow-list decides whether the
/// call runs.
#[tokio::test]
async fn package_mcp_calls_go_through_the_allowed_tools() {
    let _guard = super::TEST_ENV.lock().await;
    let scenario = Scenario::new();
    let fixture = plugin_fixture("mcp-policy", "1.0.0", json!({ "echo": stdio_definition() }));
    scenario.install(fixture.path(), PackageScope::Global, false);
    let alias = alias_of("mcp-policy", "echo", "echo");

    let allowed_registry = Registry::empty();
    let mut allowed = Runner::new(&allowed_registry, Some(HashSet::from([alias.clone()])), None);
    allowed.turn().await;
    assert!(
        tool_appears(&allowed_registry, &alias).await,
        "the allowed session did not register the tool"
    );
    allowed
        .call(scenario.home(), &alias, json!({ "text": "allowed" }))
        .await
        .expect("an allowed package tool runs");

    let refusing_registry = Registry::empty();
    let mut refusing = Runner::new(&refusing_registry, Some(HashSet::from(["read_file".to_string()])), None);
    refusing.turn().await;
    assert!(
        tool_appears(&refusing_registry, &alias).await,
        "the package tool is registered and then refused by policy, not hidden"
    );
    let error = refusing
        .call(scenario.home(), &alias, json!({ "text": "refused" }))
        .await
        .expect_err("a tool outside the allow-list is refused");
    assert!(is_policy_rejection(&error), "{error:#}");
}

/// A user server that already holds the package's public name keeps it: the
/// package entry is skipped and every other entry of that plugin still registers.
#[tokio::test]
async fn package_mcp_user_server_keeps_its_name() {
    let _guard = super::TEST_ENV.lock().await;
    let scenario = Scenario::new();
    let remote = RemoteServer::start().await;
    scenario.write_user_mcp(json!({ "plugin_mcp-name_echo": { "type": "http", "url": remote.url.clone() } }));
    let fixture = plugin_fixture(
        "mcp-name",
        "1.0.0",
        json!({
            "echo": stdio_definition(),
            "echo2": stdio_definition()
        }),
    );
    scenario.install(fixture.path(), PackageScope::Global, false);
    let registry = Registry::empty();
    let mut runner = Runner::new(&registry, None, None);
    runner.turn().await;

    let shadowed = alias_of("mcp-name", "echo", "echo");
    assert!(
        !wait_for_tool(&registry, &shadowed, QUIET_WINDOW, true).await,
        "the package replaced the user's server of the same name: {shadowed}"
    );
    let registered = alias_of("mcp-name", "echo2", "echo");
    assert!(
        tool_appears(&registry, &registered).await,
        "the plugin's other server did not register: {:?}",
        registry.tool_names().await
    );
}

/// A project-scoped plugin serves only its own workspace.
#[tokio::test]
async fn package_mcp_project_package_serves_only_its_workspace() {
    let _guard = super::TEST_ENV.lock().await;
    let scenario = Scenario::new();
    let workspace = scenario.home().join("workspace-a");
    fs::create_dir_all(&workspace).expect("create the project workspace");
    let workspace = fs::canonicalize(&workspace).expect("resolve the project workspace");
    let fixture = plugin_fixture("mcp-scope", "1.0.0", json!({ "echo": stdio_definition() }));
    scenario.install(
        fixture.path(),
        PackageScope::Project {
            root: workspace.display().to_string(),
        },
        false,
    );
    let alias = alias_of("mcp-scope", "echo", "echo");

    let inside_registry = Registry::empty();
    let mut inside = Runner::new(&inside_registry, None, Some(&workspace));
    inside.turn().await;
    assert!(
        tool_appears(&inside_registry, &alias).await,
        "the project package did not serve its own workspace: {:?}",
        inside_registry.tool_names().await
    );

    let other = scenario.home().join("workspace-b");
    fs::create_dir_all(&other).expect("create the second project workspace");
    let outside_registry = Registry::empty();
    let mut outside = Runner::new(&outside_registry, None, Some(&other));
    outside.turn().await;
    assert!(
        !wait_for_tool(&outside_registry, &alias, QUIET_WINDOW, true).await,
        "a project package served another workspace: {alias}"
    );
}

/// Replacing one package restarts only its child, and disabling another closes
/// only that package's connection: no connection is ever closed by name alone.
#[tokio::test]
async fn package_mcp_replace_and_disable_close_only_their_owner() {
    let _guard = super::TEST_ENV.lock().await;
    let scenario = Scenario::new();
    let first = plugin_fixture("mcp-owner-1", "1.0.0", json!({ "echo": stdio_definition() }));
    let second = plugin_fixture("mcp-owner-2", "1.0.0", json!({ "echo": stdio_definition() }));
    scenario.install(first.path(), PackageScope::Global, false);
    scenario.install(second.path(), PackageScope::Global, false);
    let registry = Registry::empty();
    let mut runner = Runner::new(&registry, None, None);
    runner.turn().await;

    let replaced = alias_of("mcp-owner-1", "echo", "echo");
    let untouched = alias_of("mcp-owner-2", "echo", "echo");
    assert!(
        tool_appears(&registry, &replaced).await,
        "{:?}",
        registry.tool_names().await
    );
    assert!(
        tool_appears(&registry, &untouched).await,
        "{:?}",
        registry.tool_names().await
    );
    let first_pid = pid_of(
        &runner
            .call(scenario.home(), &replaced, json!({ "text": "before" }))
            .await
            .expect("the first package answers")
            .output,
    );
    let second_pid = pid_of(
        &runner
            .call(scenario.home(), &untouched, json!({ "text": "before" }))
            .await
            .expect("the second package answers")
            .output,
    );

    // A replacement install moves the record's generation; the next turn closes
    // that package's child and starts the new copy.
    let replacement = plugin_fixture("mcp-owner-1", "1.0.1", json!({ "echo": stdio_definition() }));
    scenario.install(replacement.path(), PackageScope::Global, true);
    runner.turn().await;
    let new_pid = pid_after_replace(&runner, scenario.home(), &replaced, first_pid).await;
    wait_for_exit(first_pid).await;
    let still_second = pid_of(
        &runner
            .call(scenario.home(), &untouched, json!({ "text": "after" }))
            .await
            .expect("the other package still answers")
            .output,
    );
    assert_eq!(still_second, second_pid, "replacing one package restarted another");

    // A disable is published by the daemon: the lifecycle callback closes that
    // package's own connection and nothing else.
    let generation = scenario
        .store()
        .disable("mcp-owner-2")
        .expect("disable the second package");
    PackageLifecycleRegistry::shared().revoke("mcp-owner-2", generation, RevocationCause::Disabled);
    assert!(
        tool_disappears(&registry, &untouched).await,
        "the disabled package's tools stayed registered"
    );
    wait_for_exit(second_pid).await;
    let still_first = pid_of(
        &runner
            .call(scenario.home(), &replaced, json!({ "text": "after" }))
            .await
            .expect("the remaining package still answers")
            .output,
    );
    assert_eq!(still_first, new_pid, "disabling one package restarted another");
}
