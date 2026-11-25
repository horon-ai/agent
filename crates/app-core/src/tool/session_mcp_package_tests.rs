use super::*;
use crate::agent::Agent;
use crate::packages::{InstallRequest, PackageScope, PackageStore};
use crate::session::Session;
use serde_json::json;

fn host_target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("unsupported test host {os}/{arch}"),
    }
}

fn fixture_binary_path() -> &'static str {
    if std::env::consts::OS == "windows" {
        "bin/child.exe"
    } else {
        "bin/child"
    }
}

fn dummy_target_binary_bytes() -> Vec<u8> {
    let arch_arm64 = std::env::consts::ARCH == "aarch64";
    match std::env::consts::OS {
        "macos" => {
            let mut bytes = vec![0u8; 56];
            bytes[0..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            let cpu: u32 = if arch_arm64 { 0x0100_000c } else { 0x0100_0007 };
            bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
            bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
            bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
            bytes[20..24].copy_from_slice(&24u32.to_le_bytes());
            bytes[32..36].copy_from_slice(&0x32u32.to_le_bytes());
            bytes[36..40].copy_from_slice(&24u32.to_le_bytes());
            bytes[40..44].copy_from_slice(&1u32.to_le_bytes());
            bytes
        }
        "linux" => {
            let mut bytes = vec![0u8; 120];
            bytes[0..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
            bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
            let cpu: u16 = if arch_arm64 { 183 } else { 62 };
            bytes[18..20].copy_from_slice(&cpu.to_le_bytes());
            bytes[20..24].copy_from_slice(&[1, 0, 0, 0]);
            bytes[24..32].copy_from_slice(&1u64.to_le_bytes());
            bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
            bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
            bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
            bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
            bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
            bytes
        }
        "windows" => {
            let mut bytes = vec![0u8; 240];
            bytes[0..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
            bytes[64..68].copy_from_slice(b"PE\0\0");
            let cpu: u16 = if arch_arm64 { 0xaa64 } else { 0x8664 };
            bytes[68..70].copy_from_slice(&cpu.to_le_bytes());
            bytes[70..72].copy_from_slice(&1u16.to_le_bytes());
            bytes[84..86].copy_from_slice(&112u16.to_le_bytes());
            bytes[86..88].copy_from_slice(&2u16.to_le_bytes());
            bytes[88..90].copy_from_slice(&[0x0b, 0x02]);
            bytes
        }
        other => panic!("unsupported test OS {other}"),
    }
}

fn install_mcp_plugin(store: &PackageStore, id: &str, version: &str, servers: Value, replace: bool) {
    let source = tempfile::tempdir().expect("plugin source tempdir");
    let binary_rel = fixture_binary_path();
    let binary_dest = source.path().join("native").join(binary_rel);
    std::fs::create_dir_all(binary_dest.parent().expect("binary parent")).expect("mkdir bin");
    std::fs::write(&binary_dest, dummy_target_binary_bytes()).expect("write dummy target binary");
    horon_agent_platform::set_permissions_executable(&binary_dest).expect("chmod executable");
    std::fs::write(
        source.path().join("native/extension.toml"),
        format!(
            "schema_version = 1\nid = \"{id}\"\nversion = \"{version}\"\nname = \"MCP {id}\"\nprotocol = \"1.0\"\n[targets]\n{} = \"{binary_rel}\"\n",
            host_target_triple(),
        ),
    )
    .expect("write extension.toml");
    std::fs::create_dir_all(source.path().join("resources/mcp")).expect("mkdir mcp");
    std::fs::write(source.path().join("resources/mcp/servers.json"), servers.to_string()).expect("write servers.json");
    std::fs::write(
        source.path().join("plugin.toml"),
        format!(
            "schema_version = 1\nid = \"{id}\"\nversion = \"{version}\"\nname = \"Plugin {id}\"\nextension = \"native/extension.toml\"\n[resources]\nmcp = [\"resources/mcp/servers.json\"]\n"
        ),
    )
    .expect("write plugin.toml");
    let mut request = InstallRequest::new(source.path(), PackageScope::Global);
    if replace {
        request = request.replacing();
    }
    store.install(request).expect("install plugin with MCP servers");
}

fn install_stdio_mcp_plugin(store: &PackageStore, id: &str) {
    install_mcp_plugin(
        store,
        id,
        "1.0.0",
        json!({ "mcpServers": { "echo": { "type": "stdio", "mode": "mcp" } } }),
        false,
    );
}

#[tokio::test]
async fn empty_package_snapshot_skips_mcp_config_and_session_allocation() {
    let env = TestEnv::new();
    let store = PackageStore::for_horon_home(env.path("missing-store"));
    let session_id = "session-empty-pkg-fast-skip";
    let pkg_session = PackageSession::open(store.clone(), session_id.to_string());
    let registry = Registry::empty();

    assert!(!store.root().exists());
    refresh_package_mcp(session_id, registry.clone(), &pkg_session).await;
    assert!(
        existing_package_state(session_id).is_none(),
        "empty package store must not allocate PACKAGE_MCP_SESSIONS state"
    );

    // Even when a store directory exists on disk with zero installed plugins,
    // refreshing must remain a zero-allocation no-op for PACKAGE_MCP_SESSIONS.
    std::fs::create_dir_all(store.root()).expect("create empty store root");
    let pkg_session = PackageSession::open(store, session_id.to_string());
    refresh_package_mcp(session_id, registry, &pkg_session).await;
    assert!(
        existing_package_state(session_id).is_none(),
        "empty catalog with zero plugins must not populate PACKAGE_MCP_SESSIONS"
    );
}

#[test]
fn shared_worker_turn_one_queues_package_mcp_hosted_notice_synchronously() {
    run_isolated_child(
        "tool::session_mcp::tests::package_tests::shared_worker_turn_one_queues_package_mcp_hosted_notice_synchronously_child",
    );
}

#[tokio::test]
#[ignore = "spawned by shared_worker_turn_one_queues_package_mcp_hosted_notice_synchronously"]
async fn shared_worker_turn_one_queues_package_mcp_hosted_notice_synchronously_child() {
    let home = tempfile::tempdir().expect("isolated home");
    crate::env::set_var("HORON_HOME", home.path());
    crate::env::set_var("HORON_AGENT_HOSTING_MODE", "shared-worker");
    crate::env::set_var("HORON_AGENT_TENANT_ID", "acme");
    crate::logging::init();

    let store = PackageStore::for_horon_home(home.path());
    install_stdio_mcp_plugin(&store, "turn1-demo");

    let mut agent = Agent::new_with_session(
        Arc::new(StubProvider),
        Registry::empty(),
        Session::create(None, None),
        None,
    );
    let session_id = agent.session_id().to_owned();
    agent.enter_package_generation().await;

    let notice = crate::agent::hosted_notice::take(&session_id)
        .expect("Turn 1 must queue `<hosted-policy>` refusal notice before enter_package_generation returns");
    assert!(
        notice.contains("MCP server 'plugin_turn1-demo_echo' was not started"),
        "{notice}"
    );
    assert!(notice.contains("hosting policy 'shared-worker'"), "{notice}");

    // Same catalog generation on Turn 2 must not emit duplicate refusal notices.
    agent.enter_package_generation().await;
    assert!(
        crate::agent::hosted_notice::take(&session_id).is_none(),
        "unchanged generation must not re-queue a duplicate notice"
    );
}

struct StubMcpProxy {
    server: &'static str,
    raw: &'static str,
}

#[async_trait]
impl Tool for StubMcpProxy {
    fn name(&self) -> &str {
        self.raw
    }

    fn mcp_identity(&self) -> Option<(&str, &str)> {
        Some((self.server, self.raw))
    }

    fn description(&self) -> &str {
        "stub MCP proxy"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::new("ok"))
    }
}

async fn wait_for_mcp_server_state(
    registry: &Registry,
    surface_rx: &mut tokio::sync::watch::Receiver<u64>,
    server: &str,
    expected: bool,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if registry.has_mcp_tools_for_server(server) == expected {
            return true;
        }
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            return registry.has_mcp_tools_for_server(server) == expected;
        };
        if remaining.is_zero() {
            return registry.has_mcp_tools_for_server(server) == expected;
        }
        match tokio::time::timeout(remaining, surface_rx.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => return registry.has_mcp_tools_for_server(server) == expected,
        }
    }
}

async fn start_slow_delete_http_stub(
    delete_entered: tokio::sync::watch::Sender<bool>,
    delete_release: tokio::sync::watch::Receiver<bool>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind slow-delete stub");
    let address = listener.local_addr().expect("slow-delete address");
    let url = format!("http://{address}/mcp");
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((socket, _)) = accepted else { return };
                    let delete_entered = delete_entered.clone();
                    let delete_release = delete_release.clone();
                    connections.spawn(serve_slow_delete_http(socket, delete_entered, delete_release));
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    (url, task)
}

async fn serve_slow_delete_http(
    socket: TcpStream,
    delete_entered: tokio::sync::watch::Sender<bool>,
    mut delete_release: tokio::sync::watch::Receiver<bool>,
) {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let method = line.split_whitespace().next().unwrap_or_default().to_string();
    let mut length = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':')
            && key.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let mut socket = reader.into_inner();
    if method == "DELETE" {
        let _ = delete_entered.send(true);
        let _ = tokio::time::timeout(
            Duration::from_millis(800),
            delete_release.wait_for(|released| *released),
        )
        .await;
        let _ = write_response(&mut socket, 200, "text/plain", "", "").await;
        return;
    }
    if method != "POST" {
        let _ = write_response(&mut socket, 405, "text/plain", "", "").await;
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(id) = request.get("id").cloned() else {
        let _ = write_response(&mut socket, 202, "text/plain", "", "").await;
        return;
    };
    let result = match request.get("method").and_then(Value::as_str).unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": request.pointer("/params/protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "slow-delete-stub", "version": "1"},
        }),
        "tools/list" => json!({"tools": [{
            "name": "ping",
            "description": "slow delete ping",
            "inputSchema": {"type": "object"},
        }]}),
        "tools/call" => json!({"content": [{"type": "text", "text": "pong"}]}),
        _ => json!({}),
    };
    let reply = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
    let _ = write_response(
        &mut socket,
        200,
        "application/json",
        &reply,
        "Mcp-Session-Id: slow-session\r\n",
    )
    .await;
}

#[tokio::test]
async fn registry_subscribe_mcp_surface_wakes_on_refresh_and_unregister() {
    let registry = Registry::empty();
    let mut rx = registry.subscribe_mcp_surface();
    assert_eq!(*rx.borrow_and_update(), 0);

    let server = "plugin_demo_echo".to_string();
    registry
        .refresh_mcp_tools(
            vec![(
                server.clone(),
                Arc::new(StubMcpProxy {
                    server: "plugin_demo_echo",
                    raw: "ping",
                }) as Arc<dyn Tool>,
            )],
            std::slice::from_ref(&server),
        )
        .await;
    assert!(rx.has_changed().expect("watch sender alive after refresh"));
    assert_eq!(*rx.borrow_and_update(), 1);
    assert!(registry.has_mcp_tools_for_server(&server));

    registry.unregister_mcp_server(&server).await;
    assert!(rx.has_changed().expect("watch sender alive after unregister"));
    assert_eq!(*rx.borrow_and_update(), 2);
    assert!(!registry.has_mcp_tools_for_server(&server));
}

#[tokio::test]
async fn replacing_a_package_waits_for_stale_teardown_before_registering_replacement() {
    let env = TestEnv::new();
    let fast_stub = HttpStub::start(false).await;
    let (delete_entered_tx, mut delete_entered_rx) = tokio::sync::watch::channel(false);
    let (delete_release_tx, delete_release_rx) = tokio::sync::watch::channel(false);
    let (slow_url, slow_task) = start_slow_delete_http_stub(delete_entered_tx, delete_release_rx).await;

    std::fs::write(
        env.path("mcp.json"),
        json!({
            "mcpServers": {
                "slow-delete": { "type": "http", "url": slow_url },
                "fast-stub": { "type": "http", "url": fast_stub.url },
            }
        })
        .to_string(),
    )
    .expect("write HORON_HOME/mcp.json");

    let store = PackageStore::for_horon_home(env.dir.path());
    install_mcp_plugin(
        &store,
        "alpha-retire",
        "1.0.0",
        json!({ "mcpServers": { "clock": { "type": "remote", "server": "slow-delete" } } }),
        false,
    );
    install_mcp_plugin(
        &store,
        "omega-replace",
        "1.0.0",
        json!({ "mcpServers": { "clock": { "type": "remote", "server": "fast-stub" } } }),
        false,
    );

    let session_id = "session-replace-teardown-order";
    let pkg_session = PackageSession::open(store.clone(), session_id.to_string());
    let registry = session_registry().await;
    let mut surface_rx = registry.subscribe_mcp_surface();

    pkg_session.enter_turn(None).await.expect("enter turn 1");
    refresh_package_mcp(session_id, registry.clone(), &pkg_session).await;
    assert!(
        wait_for_mcp_server_state(
            &registry,
            &mut surface_rx,
            "plugin_alpha-retire_clock",
            true,
            Duration::from_secs(5),
        )
        .await,
        "alpha-retire should register on turn 1"
    );
    assert!(
        wait_for_mcp_server_state(
            &registry,
            &mut surface_rx,
            "plugin_omega-replace_clock",
            true,
            Duration::from_secs(5),
        )
        .await,
        "omega-replace should register on turn 1"
    );

    store.disable("alpha-retire").expect("disable alpha-retire");
    install_mcp_plugin(
        &store,
        "omega-replace",
        "1.0.1",
        json!({ "mcpServers": { "clock": { "type": "remote", "server": "fast-stub" } } }),
        true,
    );
    pkg_session.enter_turn(None).await.expect("enter turn 2");

    let mut gate_rx = registry.subscribe_mcp_surface();
    gate_rx.borrow_and_update();
    let gate = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), delete_entered_rx.wait_for(|entered| *entered))
            .await
            .expect("stale remote server should send HTTP DELETE during teardown")
            .expect("delete entered watch channel stays open");
        // Clear any unregister notification that preceded HTTP DELETE, then give
        // any prematurely spawned replacement a chance to publish before releasing.
        gate_rx.borrow_and_update();
        let _ = tokio::time::timeout(Duration::from_millis(150), gate_rx.changed()).await;
        let _ = delete_release_tx.send(true);
    });

    surface_rx.borrow_and_update();
    refresh_package_mcp(session_id, registry.clone(), &pkg_session).await;
    tokio::time::timeout(Duration::from_secs(5), gate)
        .await
        .expect("delete gate should finish within 5s")
        .expect("delete gate task finished");

    assert!(
        wait_for_mcp_server_state(
            &registry,
            &mut surface_rx,
            "plugin_omega-replace_clock",
            true,
            Duration::from_millis(800),
        )
        .await,
        "replacement package server must stay registered after stale generation teardown"
    );
    assert!(
        !registry.has_mcp_tools_for_server("plugin_alpha-retire_clock"),
        "disabled package server must stay unregistered"
    );

    let alias = crate::mcp::dispatch_name("plugin_omega-replace_clock", "echo");
    let output = registry
        .execute(&alias, json!({"token": "v2"}), context(session_id))
        .await
        .expect("replacement server tool remains callable");
    assert!(output.output.contains("http-stub"), "{}", output.output);
    assert!(output.output.contains("v2"), "{}", output.output);

    forget_package_mcp(session_id);
    slow_task.abort();
}
