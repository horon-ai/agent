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

fn install_stdio_mcp_plugin(store: &PackageStore, id: &str) {
    let source = tempfile::tempdir().expect("plugin source tempdir");
    let binary_rel = fixture_binary_path();
    let binary_dest = source.path().join("native").join(binary_rel);
    std::fs::create_dir_all(binary_dest.parent().expect("binary parent")).expect("mkdir bin");
    std::fs::copy(std::env::current_exe().expect("test exe"), &binary_dest).expect("copy test exe");
    horon_agent_platform::set_permissions_executable(&binary_dest).expect("chmod executable");
    std::fs::write(
        source.path().join("native/extension.toml"),
        format!(
            "schema_version = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = \"MCP {id}\"\nprotocol = \"1.0\"\n[targets]\n{} = \"{binary_rel}\"\n",
            host_target_triple(),
        ),
    )
    .expect("write extension.toml");
    std::fs::create_dir_all(source.path().join("resources/mcp")).expect("mkdir mcp");
    std::fs::write(
        source.path().join("resources/mcp/servers.json"),
        json!({ "mcpServers": { "echo": { "type": "stdio", "mode": "mcp" } } }).to_string(),
    )
    .expect("write servers.json");
    std::fs::write(
        source.path().join("plugin.toml"),
        format!(
            "schema_version = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = \"Plugin {id}\"\nextension = \"native/extension.toml\"\n[resources]\nmcp = [\"resources/mcp/servers.json\"]\n"
        ),
    )
    .expect("write plugin.toml");
    store
        .install(InstallRequest::new(source.path(), PackageScope::Global))
        .expect("install plugin with stdio MCP");
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
