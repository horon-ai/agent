use super::*;
use std::time::Duration;

fn empty_config() -> McpConfig {
    McpConfig::default()
}

#[test]
fn issue_790_owned_manager_keeps_its_initial_config_directory() {
    let _guard = crate::storage::lock_test_env();
    let original_cwd = std::env::current_dir().expect("current cwd");
    let previous_home = std::env::var_os("HORON_HOME");
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let elsewhere = tempfile::tempdir().expect("elsewhere tempdir");
    crate::env::set_var("HORON_HOME", home.path());
    std::fs::write(
        project.path().join(".mcp.json"),
        r#"{"mcpServers":{"project-only":{"command":"project-server"}}}"#,
    )
    .expect("write project config");
    crate::workspace_trust::trust_for_tests(project.path());

    std::env::set_current_dir(project.path()).expect("set project cwd");
    let manager = McpManager::new();
    std::env::set_current_dir(elsewhere.path()).expect("set different cwd");
    let fresh = manager.load_fresh_config();

    std::env::set_current_dir(original_cwd).expect("restore cwd");
    if let Some(previous_home) = previous_home {
        crate::env::set_var("HORON_HOME", previous_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }

    assert!(fresh.servers.contains_key("project-only"));
}

#[tokio::test]
async fn call_tool_unconfigured_server_bails_cleanly() {
    let manager = McpManager::with_config(empty_config());
    let err = manager
        .call_tool("ghost", "do_thing", serde_json::json!({}))
        .await
        .expect_err("calling an unknown server must error");
    assert!(
        err.to_string().contains("ghost"),
        "error should name the missing server: {err}"
    );
}

#[tokio::test]
async fn ensure_server_connected_unconfigured_errors() {
    let manager = McpManager::with_config(empty_config());
    let err = manager
        .ensure_server_connected("ghost", Duration::from_millis(50))
        .await
        .expect_err("ensuring an unconfigured server must error");
    assert!(err.to_string().contains("not configured"), "{err}");
}

#[tokio::test]
async fn connect_all_skips_disabled_servers() {
    // Issue #436: disabled servers stay in config but are never
    // auto-spawned. This config's command would fail to connect (and thus
    // produce a failure entry) if it were attempted at all.
    let mut config = McpConfig::default();
    config.servers.insert(
        "off".to_string(),
        McpServerConfig {
            command: "true".to_string(),
            args: vec![],
            env: HashMap::new(),
            shared: false,
            transport: None,
            url: None,
            headers: std::collections::HashMap::new(),
            enabled: Some(false),
            disabled: None,
            timeout_secs: None,
        },
    );
    let manager = McpManager::with_config(config);
    let (successes, failures) = manager.connect_all().await.expect("connect_all");
    assert_eq!(successes, 0, "disabled server must not be spawned");
    assert!(
        failures.is_empty(),
        "disabled server must not be attempted: {failures:?}"
    );
    assert!(manager.connected_servers().await.is_empty());
    // Still present in config so it can be connected on demand by name.
    assert!(manager.config().servers.contains_key("off"));
}

#[tokio::test]
async fn connect_on_first_call_fails_cleanly_for_broken_server() {
    // A configured server whose command exits immediately and never speaks
    // MCP. connect-on-first-call must surface a clean, bounded tool error
    // (connection failure) rather than hanging or panicking.
    let mut config = McpConfig::default();
    config.servers.insert(
        "broken".to_string(),
        McpServerConfig {
            // `true` exits 0 immediately: the stdio handshake gets EOF, so
            // connect fails fast instead of waiting on the initialize bound.
            command: "true".to_string(),
            args: vec![],
            env: HashMap::new(),
            shared: false,
            transport: None,
            url: None,
            headers: std::collections::HashMap::new(),
            enabled: None,
            disabled: None,
            timeout_secs: None,
        },
    );
    let manager = McpManager::with_config(config);

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(40),
        manager.call_tool("broken", "anything", serde_json::json!({})),
    )
    .await;
    let inner = result.expect("call_tool must return, not hang");
    assert!(inner.is_err(), "broken server must yield a tool error");
    let msg = inner.unwrap_err().to_string();
    assert!(msg.contains("broken"), "tool error should name the server: {msg}");
    assert!(
        started.elapsed() < Duration::from_secs(35),
        "connect-on-first-call must be bounded"
    );
}

#[tokio::test]
async fn cancelled_owned_connection_notifies_waiters_and_clears_entry() {
    let manager = McpManager::with_config(empty_config());
    let (sender, _receiver) = watch::channel(None);
    manager
        .connecting
        .lock()
        .await
        .insert("cancelled".to_string(), sender.clone());
    let mut waiter = sender.subscribe();

    drop(OwnedConnectionAttemptGuard::new(
        Arc::clone(&manager.connecting),
        "cancelled",
        sender,
    ));

    tokio::time::timeout(Duration::from_secs(1), waiter.changed())
        .await
        .expect("cancellation cleanup must notify waiters")
        .expect("cancellation sender must remain alive");
    assert_eq!(
        waiter
            .borrow()
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .map(String::as_str),
        Some("MCP server 'cancelled' connection attempt was cancelled")
    );
    assert!(!manager.connecting.lock().await.contains_key("cancelled"));
}

#[test]
fn breaker_suspends_after_more_than_five_attempts_in_the_window() {
    let manager = McpManager::with_config(empty_config());
    for _ in 0..BREAKER_MAX_ATTEMPTS {
        assert!(
            manager.watch.record_attempt("flaky").is_none(),
            "the breaker must not trip inside the allowed attempt count"
        );
    }
    assert!(
        manager.watch.record_attempt("flaky").is_some(),
        "the sixth attempt inside the window must suspend the server"
    );
    assert!(
        manager
            .watch
            .suspension_seconds("flaky")
            .is_some_and(|seconds| seconds <= BREAKER_SUSPENSION.as_secs()),
        "a suspended server reports the remaining wait"
    );

    // A different server is unaffected.
    assert!(manager.watch.suspension_seconds("healthy").is_none());

    // An explicit reload is the documented escape hatch.
    manager.watch.reset_breaker();
    assert!(manager.watch.suspension_seconds("flaky").is_none());
}

#[test]
fn a_successful_connect_clears_the_breaker_history() {
    let manager = McpManager::with_config(empty_config());
    for _ in 0..BREAKER_MAX_ATTEMPTS {
        manager.watch.record_attempt("flapping");
    }
    manager.watch.record_success("flapping");
    for _ in 0..BREAKER_MAX_ATTEMPTS {
        assert!(manager.watch.record_attempt("flapping").is_none());
    }
}

#[tokio::test]
async fn a_suspended_server_fails_the_tool_call_with_the_reload_hint() {
    let manager = McpManager::with_config(empty_config());
    for _ in 0..=BREAKER_MAX_ATTEMPTS {
        manager.watch.record_attempt("suspended");
    }
    let error = manager
        .call_tool("suspended", "anything", serde_json::json!({}))
        .await
        .expect_err("a suspended server must refuse the call");
    let message = error.to_string();
    assert!(
        message.starts_with("MCP server 'suspended' is suspended after repeated connection failures; retry in "),
        "{message}"
    );
    assert!(message.contains("action=reload"), "{message}");
}

#[test]
fn only_pre_send_transport_failures_are_retryable() {
    for message in [
        "MCP server 'x' exited (connection closed)",
        "Failed to send request",
        "transport closed",
        "connect error: Connection refused (os error 111)",
        "MCP server 'x' not connected",
        "HTTP 502 Bad Gateway",
    ] {
        assert!(
            is_presend_transport_failure(&anyhow::anyhow!(message)),
            "{message} happened before the request was written"
        );
    }
    for message in [
        "Request timeout after 30s (raise `timeout_secs` for MCP server 'x' if its tools legitimately run longer)",
        "MCP error -32602: invalid arguments",
    ] {
        assert!(
            !is_presend_transport_failure(&anyhow::anyhow!(message)),
            "{message} may have already run on the server"
        );
    }
}
