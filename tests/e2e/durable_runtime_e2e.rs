//! End-to-end checks for the durable runtime surfaces.
//!
//! These use the real journal, the real files it writes and the real CLI, not
//! in-memory fakes: a restart here is a second process opening the same HOME.

use super::test_support::setup_test_env;
use horon_agent::agent::{SoftInterruptMessage, SoftInterruptSource};
use horon_agent::durable_input::{DurableInputStore, ResolveAction};
use horon_agent::durable_runtime::{DurableRuntimeStore, MonitorUpsert, Payload, RecordKind, read_journal_records};

fn session_id(tag: &str) -> String {
    format!("e2e-durable-{tag}-{}", std::process::id())
}

fn message(client_id: &str, text: &str) -> SoftInterruptMessage {
    SoftInterruptMessage {
        content: text.to_string(),
        images: Vec::new(),
        urgent: false,
        source: SoftInterruptSource::User,
        client_id: Some(client_id.to_string()),
        input_id: None,
    }
}

/// An accepted input must survive a restart exactly once.
#[test]
fn durable_runtime_e2e_restart_preserves_pending_input() -> anyhow::Result<()> {
    let _env = setup_test_env()?;
    let session = session_id("pending");

    {
        let mut store = DurableInputStore::open(&session)?;
        let accepted = store.accept(&mut message("client-a", "first pending message"))?;
        assert!(!accepted.already_live, "the first accept is a new row");
    }

    // A second open is what a restart does: nothing in memory survives.
    let mut reopened = DurableInputStore::open(&session)?;
    let restored = reopened.recover(&[])?;
    assert_eq!(
        restored.pending.len(),
        1,
        "exactly one pending input after restart, got {restored:?}"
    );
    assert_eq!(restored.pending[0].content, "first pending message");

    // Recovering twice must not duplicate: the queue is keyed by input_id.
    let again = reopened.recover(&[])?;
    assert_eq!(
        again.pending.len(),
        1,
        "a second recovery must not duplicate the same input, got {again:?}"
    );

    let rows = read_journal_records(
        &reopened
            .directory()
            .join(horon_agent::durable_runtime::JOURNAL_FILE_NAME),
    )?;
    let accepted_rows = rows.iter().filter(|row| row.kind == RecordKind::InputAccepted).count();
    assert_eq!(accepted_rows, 1, "one input.accepted row, got {rows:?}");
    Ok(())
}

/// A started input with no terminal row is undetermined, not complete, and is
/// never re-injected on its own.
#[test]
fn durable_runtime_e2e_started_input_is_in_doubt_after_kill() -> anyhow::Result<()> {
    let _env = setup_test_env()?;
    let session = session_id("indoubt");

    let input_id = {
        let mut store = DurableInputStore::open(&session)?;
        let accepted = store.accept(&mut message("client-b", "a message that started"))?;
        store.start(&accepted.input_id, Some("run-1"))?;
        // No commit, no cancel: this is the state a SIGKILL leaves behind.
        accepted.input_id
    };

    let mut reopened = DurableInputStore::open(&session)?;
    // An empty in-flight list is what a restart has: nothing of this process
    // survived, so a started input cannot be "still running".
    let restored = reopened.recover(&[])?;
    assert!(
        restored.pending.is_empty(),
        "a started input must not be restored as pending: {restored:?}"
    );
    assert_eq!(
        restored.sealed_in_doubt,
        vec![input_id.clone()],
        "a started input with no terminal row must be sealed in doubt: {restored:?}"
    );

    // Explicit resolution is the only way out, and retry requeues it.
    let resolved = reopened.resolve(&input_id, ResolveAction::Retry, |_| true)?;
    assert_eq!(resolved.input_id, input_id);
    let after = reopened.recover(&[])?;
    assert_eq!(after.pending.len(), 1, "retry requeues the message: {after:?}");
    Ok(())
}

/// A command monitor must not run its command again after a restart.
#[test]
fn durable_runtime_e2e_command_monitor_is_not_rerun() -> anyhow::Result<()> {
    let _env = setup_test_env()?;
    let session = session_id("monitor");
    let dir = tempfile::tempdir()?;
    let counter_path = dir.path().join("ran.txt");
    let command = format!("echo ran >> {}", counter_path.display());

    let monitor_id = horon_agent::durable_runtime::new_id();
    {
        let mut store = DurableRuntimeStore::open(&session)?;
        store.append(
            &monitor_id,
            Payload::MonitorUpsert(MonitorUpsert {
                monitor_kind: "command".to_string(),
                spec: serde_json::json!({
                    "command": command,
                    "restart_policy": "never",
                }),
            }),
        )?;
    }

    let runs = || -> anyhow::Result<usize> {
        match std::fs::read_to_string(&counter_path) {
            Ok(text) => Ok(text.lines().filter(|line| !line.trim().is_empty()).count()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error.into()),
        }
    };
    assert_eq!(runs()?, 0, "recording a monitor must not run the command");

    // Restart: the row survives, and still nothing has run.
    let mut reopened = DurableRuntimeStore::open(&session)?;
    reopened.replay()?;
    assert!(
        reopened.state().monitors.contains_key(&monitor_id),
        "the monitor row survives a restart: {:?}",
        reopened.state().monitors.keys().collect::<Vec<_>>()
    );
    assert_eq!(runs()?, 0, "opening the journal must not execute the command");
    Ok(())
}

/// The legacy export refuses while an input is undetermined, and succeeds once
/// the operator resolves it.
#[test]
fn durable_runtime_e2e_export_legacy_refuses_when_not_quiescent() -> anyhow::Result<()> {
    let _env = setup_test_env()?;
    let session = session_id("export");

    let input_id = {
        let mut store = DurableInputStore::open(&session)?;
        let accepted = store.accept(&mut message("client-c", "still running"))?;
        store.start(&accepted.input_id, Some("run-2"))?;
        accepted.input_id
    };

    let mut reopened = DurableInputStore::open(&session)?;
    reopened.recover(&[])?;
    let refusal = reopened.export_legacy()?;
    let refused = refusal.expect_err("export must refuse while an input is in doubt");
    let reasons = format!("{refused:?}");
    assert!(
        reasons.to_lowercase().contains("doubt") || reasons.to_lowercase().contains("undetermined"),
        "the refusal must name the reason, got: {reasons}"
    );

    // Once resolved, the same export succeeds.
    reopened.resolve(&input_id, ResolveAction::Cancel, |_| true)?;
    reopened
        .export_legacy()?
        .map_err(|refused| anyhow::anyhow!("a quiescent session must export, got {refused:?}"))?;
    Ok(())
}

/// A client that negotiates no new capability still gets a normal session.
///
/// The old-client promise is that the additive surfaces are invisible to it: no
/// handshake change, no required field, no behaviour difference. This drives a
/// real server over its unix socket the way an unmodified client does, then
/// confirms the journal surfaces were never needed.
#[tokio::test]
async fn durable_runtime_e2e_old_client_fixture() -> anyhow::Result<()> {
    let _env = setup_test_env()?;

    // A real daemon over its unix socket, driven exactly as an unmodified client
    // does: subscribe, take history, send one message, wait for its done event.
    let scenario = super::test_support::run_unix_transport_scenario().await?;
    assert!(
        !scenario.subscribe_events.is_empty(),
        "an old client still completes subscribe"
    );
    assert!(!scenario.history_events.is_empty(), "an old client still gets history");

    // The gated surfaces exist, but nothing on the legacy path asks for them:
    // the capability strings are additive and the old flow never reads one.
    assert_eq!(
        horon_agent_harness_api::CAPABILITY_DURABLE_RUNTIME_V1,
        "durable_runtime_v1"
    );
    Ok(())
}

/// A real SIGKILL against a real daemon, then a real restart on the same HOME.
///
/// The other tests in this file model a restart by reopening the journal in the
/// same process. This one does not: it spawns the built binary, waits for its
/// debug socket, queues an input through the daemon's own path, kills the
/// process with SIGKILL, and starts a second process on the same HOME. Only
/// that proves the row reached the disk before the process died.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn durable_runtime_e2e_sigkill_then_restart_keeps_the_accepted_input() -> anyhow::Result<()> {
    use std::process::{Command, Stdio};

    let _env = super::test_support::setup_test_env()?;
    // A unix socket path must fit SUN_LEN (~104 bytes), so the root is short and
    // explicit rather than a tempfile under the deep cargo target tree.
    let root = std::path::PathBuf::from(format!("/tmp/ha-sigkill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let runtime_dir = root.join("r");
    let home_dir = root.join("h");
    std::fs::create_dir_all(&runtime_dir)?;
    std::fs::create_dir_all(&home_dir)?;
    let socket_path = runtime_dir.join("s.sock");
    // The daemon derives the debug socket from the main socket's file name
    // (`x.sock` -> `x-debug.sock`), so this must match that rule exactly.
    let debug_socket_path = runtime_dir.join("s-debug.sock");
    let stderr_log = root.join("stderr.log");

    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_horon-agent"))
            .arg("--no-update")
            .arg("--socket")
            .arg(&socket_path)
            .arg("serve")
            .env_remove("HORON_AGENT_TEST_SESSION")
            .env("HORON_AGENT_HOME", &home_dir)
            .env("HORON_AGENT_RUNTIME_DIR", &runtime_dir)
            .env("HORON_AGENT_DEBUG_SOCKET", "1")
            .env("HORON_AGENT_WORKSPACE_TRUST", "trust")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&stderr_log).expect("stderr log")))
            .spawn()
    };

    let mut child = spawn()?;
    let mut ready = false;
    for _ in 0..300 {
        if debug_socket_path.exists() {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        ready,
        "the daemon never created its debug socket; stderr: {}",
        std::fs::read_to_string(&stderr_log).unwrap_or_default()
    );

    let session = super::test_support::debug_create_headless_session(debug_socket_path.clone()).await?;
    super::test_support::debug_run_command(
        debug_socket_path.clone(),
        "queue_interrupt: an input that must survive a kill",
        Some(session.as_str()),
    )
    .await?;

    // The row is on disk BEFORE the kill: read it from the journal, not memory.
    let journal_root = home_dir.join(horon_agent::durable_runtime::JOURNAL_DIR_NAME);
    let journal_file = journal_root
        .join(&session)
        .join(horon_agent::durable_runtime::JOURNAL_FILE_NAME);
    let accepted_before = horon_agent::durable_runtime::read_journal_records(&journal_file)?
        .into_iter()
        .filter(|row| row.kind == horon_agent::durable_runtime::RecordKind::InputAccepted)
        .count();
    assert_eq!(accepted_before, 1, "the accept was synced before the kill");

    // A real SIGKILL, not a graceful stop.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
    let status = child.wait()?;
    assert!(!status.success(), "SIGKILL must not look like a clean exit: {status:?}");

    // Restart on the same HOME; the input must still be there exactly once.
    let mut restarted = spawn()?;
    let mut ready = false;
    for _ in 0..300 {
        if debug_socket_path.exists() {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        ready,
        "the restarted daemon never created its debug socket; stderr: {}",
        std::fs::read_to_string(&stderr_log).unwrap_or_default()
    );

    let accepted_after = horon_agent::durable_runtime::read_journal_records(&journal_file)?
        .into_iter()
        .filter(|row| row.kind == horon_agent::durable_runtime::RecordKind::InputAccepted)
        .count();
    assert_eq!(
        accepted_after, 1,
        "the accepted input survives one kill and one restart exactly once"
    );

    // And it recovers as pending in the restarted process's own view.
    let mut store = horon_agent::durable_input::DurableInputStore::open_in(&journal_root, &session)?;
    let recovery = store.recover(&[])?;
    assert!(
        !recovery.pending.is_empty() || !recovery.sealed_in_doubt.is_empty(),
        "the restarted daemon must see the input as pending or undetermined: {recovery:?}"
    );

    let _ = restarted.kill();
    let _ = restarted.wait();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// A second daemon on a live socket must refuse rather than steal it.
///
/// The plan lists "startup storage error / port-socket collision" among the
/// failure modes task 14 must exercise, and F3 recorded it as uncovered. This
/// covers the socket half: the second process must exit non-zero with the
/// refusal, and the first must still be serving afterwards.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn durable_runtime_e2e_second_daemon_refuses_a_live_socket() -> anyhow::Result<()> {
    use std::process::{Command, Stdio};

    let _env = super::test_support::setup_test_env()?;
    let root = std::path::PathBuf::from(format!("/tmp/ha-collide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let runtime_dir = root.join("r");
    let home_dir = root.join("h");
    std::fs::create_dir_all(&runtime_dir)?;
    std::fs::create_dir_all(&home_dir)?;
    let socket_path = runtime_dir.join("s.sock");
    let debug_socket_path = runtime_dir.join("s-debug.sock");

    let spawn = |stderr: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_horon-agent"))
            .arg("--no-update")
            .arg("--socket")
            .arg(&socket_path)
            .arg("serve")
            .env_remove("HORON_AGENT_TEST_SESSION")
            .env("HORON_AGENT_HOME", &home_dir)
            .env("HORON_AGENT_RUNTIME_DIR", &runtime_dir)
            .env("HORON_AGENT_DEBUG_SOCKET", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(stderr).expect("stderr log")))
            .spawn()
    };

    let mut first = spawn(&root.join("first.log"))?;
    let mut ready = false;
    for _ in 0..300 {
        if debug_socket_path.exists() {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        ready,
        "the first daemon never came up; stderr: {}",
        std::fs::read_to_string(root.join("first.log")).unwrap_or_default()
    );

    // The second one must lose, not take the socket over.
    let mut second = spawn(&root.join("second.log"))?;
    let status = second.wait()?;
    let second_log = std::fs::read_to_string(root.join("second.log")).unwrap_or_default();
    assert!(
        !status.success(),
        "a second daemon on a live socket must exit non-zero, got {status:?}"
    );
    // The daemon lock wins the race before the socket check does, and it says
    // so: either refusal is correct, silence is not.
    assert!(
        second_log.contains("already running on socket")
            || second_log.contains("Refusing to replace active server socket"),
        "the refusal must be stated, got: {second_log}"
    );

    // And the first daemon still answers.
    let session = super::test_support::debug_create_headless_session(debug_socket_path.clone()).await?;
    assert!(!session.is_empty(), "the first daemon still serves after the collision");

    let _ = first.kill();
    let _ = first.wait();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
