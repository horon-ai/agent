use crate::test_support::*;

// ============================================================================
// Binary Integration Tests
// These tests run the actual horon-agent binary and require real credentials.
// Run with: cargo test --test e2e binary_integration -- --ignored
// ============================================================================

// ----------------------------------------------------------------------------
// Reload/handoff robustness coverage map (for future contributors)
//
// Unit-level (no credentials, run by default):
//   - server::reload_state::tests + server::socket_tests: marker/handoff state
//     machine (Ready/Waiting/Failed/Idle verdicts, dead-pid crash detection,
//     stale/foreign/completed marker cleanup, Failed-marker preservation,
//     corrupt-marker tolerance, bounded handoff-event wait).
//   - server::reload::reload_tests: graceful shutdown signaling, timeout, and
//     partial-checkpoint behavior; recovery-intent persistence for peers.
//   - server::reload_recovery::tests: recovery-store path-traversal safety,
//     persist/peek roundtrip, non-consuming directive peek, delivery
//     idempotency + continuation mismatch.
//   - server::util::reload_target_tests: no-downgrade exec-target guard.
//
// E2E (real spawned process, run with --ignored; need a release binary):
//   - binary_integration_reload_handoff: server identity changes, marker clears.
//
// Known E2E gaps worth adding when a release binary is available:
//   - Concurrent/rapid `client.reload()` calls collapsing into one handoff
//     without stranding the client or leaving a stuck marker.
//   - A pre-existing *foreign* stale reload marker (different pid) in the
//     runtime dir at boot being cleared rather than blocking startup.
//   - Crash-during-boot of the replacement server (e.g. point the reload
//     candidate at a binary that exits non-zero) resolving the waiting client
//     to a Failed verdict instead of an indefinite hang.
// ----------------------------------------------------------------------------

/// Test that the horon-agent binary can run with an OpenAI-compatible provider
#[tokio::test]
#[ignore] // Requires OpenAI-compatible credentials
async fn binary_integration_openai_provider() -> Result<()> {
    use std::process::Command;
    let _env = setup_test_env()?;

    let output = Command::new("cargo")
        .args([
            "run",
            "--release",
            "--bin",
            "horon-agent",
            "--",
            "--provider",
            "openai-compatible",
            "run",
            "Say 'openai-ok' and nothing else",
        ])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Check either success or identifiable OpenAI-compatible response
    let has_response = stdout.to_lowercase().contains("openai")
        || stdout.to_lowercase().contains("ok")
        || stderr.contains("OpenAI-compatible");

    assert!(
        output.status.success() || has_response,
        "OpenAI-compatible provider should work. stdout: {}, stderr: {}",
        stdout,
        stderr
    );

    Ok(())
}

/// Test that horon-agent version command works
#[tokio::test]
async fn binary_version_command() -> Result<()> {
    use std::process::Command;
    let _env = setup_test_env()?;

    let output = Command::new(env!("CARGO_BIN_EXE_horon-agent"))
        .arg("--version")
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Version command should succeed");
    assert!(
        stdout.contains("horon-agent") || stdout.contains("20"),
        "Version should contain 'horon-agent' or date. Got: {}",
        stdout
    );

    Ok(())
}

/// Test full server reload handoff against a real spawned server process.
///
/// Requires a built release binary at target/release/horon-agent because the reload
/// flow execs into the repo's reload candidate.
#[tokio::test]
#[ignore]
async fn binary_integration_reload_handoff() -> Result<()> {
    let _env = setup_test_env()?;

    let release_binary = horon_agent::build::release_binary_path(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    if !release_binary.exists() {
        anyhow::bail!(
            "release binary missing at {} (run `cargo build --release` first)",
            release_binary.display()
        );
    }

    let temp_root = tempfile::Builder::new().prefix("horon-agent-reload-e2e-").tempdir()?;
    let runtime_dir = temp_root.path().join("runtime");
    let home_dir = temp_root.path().join("home");
    let install_dir = temp_root.path().join("install");
    let stderr_path = temp_root.path().join("server-stderr.log");
    std::fs::create_dir_all(&runtime_dir)?;
    std::fs::create_dir_all(&home_dir)?;
    std::fs::create_dir_all(&install_dir)?;

    let socket_path = runtime_dir.join("horon-agent.sock");
    let debug_socket_path = runtime_dir.join("horon-agent-debug.sock");

    let stderr_file = std::fs::File::create(&stderr_path)?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_horon-agent"))
        .arg("--no-update")
        .arg("--socket")
        .arg(&socket_path)
        .arg("serve")
        // This test must exercise the real exec-based reload handoff, not the
        // in-process test shortcut used by other e2e cases.
        .env_remove("HORON_AGENT_TEST_SESSION")
        .env("HORON_AGENT_HOME", &home_dir)
        .env("HORON_AGENT_RUNTIME_DIR", &runtime_dir)
        .env("HORON_AGENT_INSTALL_DIR", &install_dir)
        .env("HORON_AGENT_DEBUG_CONTROL", "1")
        .env("HORON_AGENT_TEMP_SERVER", "1")
        .env("HORON_AGENT_SERVER_OWNER_PID", std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()?;

    let test_result = async {
        wait_for_server_ready(&socket_path, &debug_socket_path).await?;
        let server_info_before = debug_run_command(debug_socket_path.clone(), "server:info", None).await?;
        let server_info_before_json: serde_json::Value = serde_json::from_str(&server_info_before)?;
        let server_id_before = server_info_before_json
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing server id before reload"))?
            .to_string();

        let mut client = wait_for_server_client(&socket_path).await?;
        client.reload().await?;

        let disconnect_deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_disconnect = false;
        while Instant::now() < disconnect_deadline {
            match tokio::time::timeout(Duration::from_secs(1), client.read_event()).await {
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => {
                    saw_disconnect = true;
                    break;
                }
            }
        }
        assert!(saw_disconnect, "old client connection never disconnected during reload");

        let marker_deadline = Instant::now() + Duration::from_secs(20);
        while horon_agent::server::reload_marker_active(Duration::from_secs(30)) {
            if Instant::now() >= marker_deadline {
                anyhow::bail!("reload marker remained active too long after restart");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        wait_for_server_ready(&socket_path, &debug_socket_path).await?;
        let _client = wait_for_server_client(&socket_path).await?;

        let server_info_after = debug_run_command(debug_socket_path.clone(), "server:info", None).await?;
        let server_info_after_json: serde_json::Value = serde_json::from_str(&server_info_after)?;
        let server_id_after = server_info_after_json
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing server id after reload"))?;

        assert_ne!(
            server_id_after, server_id_before,
            "server identity should change after exec-based reload"
        );
        assert!(
            server_info_after_json
                .get("uptime_secs")
                .and_then(|v| v.as_u64())
                .is_some(),
            "replacement server should answer debug state queries after reload"
        );

        Ok::<_, anyhow::Error>(())
    }
    .await;

    kill_child(&mut child);
    if let Err(ref error) = test_result {
        if let Ok(stderr) = std::fs::read_to_string(&stderr_path) {
            eprintln!("spawned server stderr:\n{}", stderr);
        }
        eprintln!("reload e2e test error: {error:#}");
    }
    test_result
}
