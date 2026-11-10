//! Manager tests whose fixture is a POSIX shell MCP server.

use super::*;

#[tokio::test]
// `disconnect_all` flushes process-global sponsor provenance counters, so
// hold the test-env lock to avoid draining another test's pending reports.
#[allow(clippy::await_holding_lock)]
async fn concurrent_owned_connects_spawn_only_one_server() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    let _env_lock = crate::storage::lock_test_env();

    let temp = tempfile::tempdir().expect("tempdir");
    let script = temp.path().join("counted-mcp.sh");
    let count = temp.path().join("starts");
    std::fs::write(
            &script,
            r##"#!/bin/sh
printf 'started\n' >> "$MCP_START_COUNT"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"counted","version":"0.0.1"}}}'
      ;;
    *'"tools/list"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"tools":[]}}'
      ;;
    *'"shutdown"'*)
      exit 0
      ;;
  esac
done
"##,
        )
        .expect("write MCP script");
    let mut permissions = std::fs::metadata(&script).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("make script executable");

    let mut config = McpConfig::default();
    let mut env = HashMap::new();
    env.insert("MCP_START_COUNT".to_string(), count.display().to_string());
    config.servers.insert(
        "counted".to_string(),
        McpServerConfig {
            command: script.display().to_string(),
            args: Vec::new(),
            env,
            shared: false,
            transport: None,
            url: None,
            headers: HashMap::new(),
            enabled: None,
            disabled: None,
            timeout_secs: Some(10),
        },
    );
    let config = config.servers.get("counted").cloned().expect("server config");
    let manager = Arc::new(McpManager::with_config({
        let mut cfg = McpConfig::default();
        cfg.servers.insert("counted".to_string(), config.clone());
        cfg
    }));

    let background = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.connect_all().await })
    };
    let manual = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.connect("counted", &config).await })
    };

    let (_, failures) = background
        .await
        .expect("background task join")
        .expect("background connect_all");
    assert!(failures.is_empty(), "background failures: {failures:?}");
    manual.await.expect("manual task join").expect("manual connect");

    let starts = std::fs::read_to_string(&count).expect("read start count");
    assert_eq!(starts.lines().count(), 1, "server starts: {starts:?}");
    manager.disconnect_all().await;
}
