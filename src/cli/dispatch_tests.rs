use super::*;
use crate::transport::Listener;

#[test]
fn only_file_controlled_debug_clients_need_parent_lifetime_binding() {
    let _lock = crate::storage::lock_test_env();
    let previous = std::env::var_os("HORON_AGENT_DEBUG_CMD_PATH");
    crate::env::remove_var("HORON_AGENT_DEBUG_CMD_PATH");
    assert!(!is_file_controlled_debug_client());

    crate::env::set_var("HORON_AGENT_DEBUG_CMD_PATH", "/tmp/horon-agent-test-debug-command");
    assert!(is_file_controlled_debug_client());

    if let Some(previous) = previous {
        crate::env::set_var("HORON_AGENT_DEBUG_CMD_PATH", previous);
    } else {
        crate::env::remove_var("HORON_AGENT_DEBUG_CMD_PATH");
    }
}

#[test]
fn headless_launch_defaults_to_the_core_tool_profile() {
    let resolved = default_tool_profile_for_launch(None, None, "");
    eprintln!("resolved default tool profile for a fresh headless launch: {resolved:?}");
    assert_eq!(resolved, Some("core"));
    // Any explicit selection wins, so the user can widen the set.
    assert_eq!(default_tool_profile_for_launch(Some("full"), None, ""), None);
    assert_eq!(default_tool_profile_for_launch(None, Some("full"), ""), None);
    assert_eq!(default_tool_profile_for_launch(None, None, "full"), None);
    // Whitespace-only values are not a selection.
    assert_eq!(default_tool_profile_for_launch(Some("  "), None, "   "), Some("core"));
    assert_eq!(crate::config::CORE_TOOL_PROFILE, "core");
}

#[cfg(target_os = "linux")]
#[test]
fn init_and_systemd_are_recognized_as_orphan_adopters() {
    assert!(is_orphan_adopter_name("init\n"));
    assert!(is_orphan_adopter_name("systemd\n"));
    assert!(!is_orphan_adopter_name("bash\n"));
    assert!(!is_orphan_adopter_name("horon-agent\n"));
}

#[test]
fn auth_doctor_provider_focus_uses_global_provider_when_positional_is_absent() {
    assert_eq!(
        auth_doctor_provider_arg(None, &ProviderChoice::Cerebras),
        Some("cerebras")
    );
    assert_eq!(
        auth_doctor_provider_arg(None, &ProviderChoice::Auto),
        None,
        "auto should keep the default doctor behavior of checking configured providers"
    );
}

#[test]
fn auth_doctor_positional_provider_wins_over_global_provider() {
    assert_eq!(
        auth_doctor_provider_arg(Some("openai"), &ProviderChoice::Cerebras),
        Some("openai"),
        "`horon-agent --provider cerebras auth doctor openai` should diagnose the explicit positional provider"
    );
}

#[test]
fn interactive_startup_skips_bootstrap_credential_detection() {
    assert!(!should_detect_cli_bootstrap_credentials(&ProviderChoice::Auto, false));
    assert!(!should_detect_cli_bootstrap_credentials(&ProviderChoice::Openai, true));
    assert!(should_detect_cli_bootstrap_credentials(&ProviderChoice::Auto, true));
}

struct ReloadTestEnv {
    prev_socket: Option<std::ffi::OsString>,
    prev_runtime: Option<std::ffi::OsString>,
    socket_path: std::path::PathBuf,
}

impl ReloadTestEnv {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("horon-agent.sock");
        let prev_socket = std::env::var_os("HORON_AGENT_SOCKET");
        let prev_runtime = std::env::var_os("HORON_AGENT_RUNTIME_DIR");
        crate::server::set_socket_path(socket_path.to_str().expect("utf8 socket path"));
        crate::env::set_var("HORON_AGENT_RUNTIME_DIR", temp.path());
        // Keep tempdir alive for the duration of the test helper.
        let _ = temp.keep();
        Self {
            prev_socket,
            prev_runtime,
            socket_path,
        }
    }
}

impl Drop for ReloadTestEnv {
    fn drop(&mut self) {
        crate::server::clear_reload_marker();
        let _ = std::fs::remove_file(&self.socket_path);
        if let Some(prev_socket) = &self.prev_socket {
            crate::env::set_var("HORON_AGENT_SOCKET", prev_socket);
        } else {
            crate::env::remove_var("HORON_AGENT_SOCKET");
        }
        if let Some(prev_runtime) = &self.prev_runtime {
            crate::env::set_var("HORON_AGENT_RUNTIME_DIR", prev_runtime);
        } else {
            crate::env::remove_var("HORON_AGENT_RUNTIME_DIR");
        }
    }
}

/// The shared daemon must keep its plain `serve` argv; an owned server must
/// ask for the temporary lifecycle and name its owner, or the ephemeral server
/// a headless `run` starts would outlive the run (#1748).
#[test]
fn owned_server_argv_requests_the_temporary_lifecycle() {
    assert_eq!(
        serve_subcommand_args(ServerLifetime::Shared, 4321),
        vec!["serve".to_string()]
    );
    assert_eq!(
        serve_subcommand_args(ServerLifetime::OwnedByThisProcess, 4321),
        vec![
            "serve".to_string(),
            "--temporary-server".to_string(),
            "--owner-pid".to_string(),
            "4321".to_string(),
        ]
    );

    // The argv must parse back into the flags `serve` actually reads.
    let argv = std::iter::once("horon-agent".to_string())
        .chain(serve_subcommand_args(ServerLifetime::OwnedByThisProcess, 4321))
        .collect::<Vec<_>>();
    let parsed = <crate::cli::args::Args as clap::Parser>::try_parse_from(argv).expect("parse owned serve argv");
    assert!(matches!(
        parsed.command,
        Some(Command::Serve {
            temporary_server: true,
            owner_pid: Some(4321),
            ..
        })
    ));
}

#[cfg(unix)]
#[test]
fn spawn_lock_serializes_shared_server_bootstrap() {
    let temp = tempfile::tempdir().expect("tempdir");
    let socket_path = temp.path().join("horon-agent.sock");
    let lock_path = spawn_lock_path(&socket_path);

    let first = try_acquire_spawn_lock(&lock_path)
        .expect("acquire first lock")
        .expect("first lock should succeed");
    let second = try_acquire_spawn_lock(&lock_path).expect("acquire second lock");
    assert!(second.is_none(), "second lock should be held by first guard");

    drop(first);

    let third = try_acquire_spawn_lock(&lock_path)
        .expect("acquire third lock")
        .expect("third lock should succeed after release");
    drop(third);

    assert!(
        !lock_path.exists(),
        "lock file should be cleaned up when the guard drops"
    );
}

#[test]
fn resolve_resume_id_imports_raw_codex_session_ids() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    crate::env::set_var("HORON_AGENT_HOME", temp.path());

    let codex_dir = temp.path().join("external/.codex/sessions/2026/04/16");
    std::fs::create_dir_all(&codex_dir).expect("create codex dir");
    std::fs::write(
            codex_dir.join("rollout.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-04-16T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-cli-resume-test\",\"timestamp\":\"2026-04-16T09:59:00Z\",\"cwd\":\"/tmp/codex-cli-resume\"}}\n",
                "{\"timestamp\":\"2026-04-16T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Resume this Codex session\"}]}}\n",
                "{\"timestamp\":\"2026-04-16T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Imported\"}]}}\n"
            ),
        )
        .expect("write codex transcript");

    let resolved = resolve_resume_id("codex-cli-resume-test").expect("resolve codex id");
    let imported_id = crate::import::imported_codex_session_id("codex-cli-resume-test");
    assert_eq!(resolved, imported_id);

    let session = crate::session::Session::load(&resolved).expect("load imported session");
    assert_eq!(session.active_context().len(), 2);

    crate::env::remove_var("HORON_AGENT_HOME");
}

#[test]
fn resume_failure_defers_to_server_during_reload_handoff() {
    // Issue #328: when `--resume <id>` cannot be resolved locally but a reload/
    // update/restart handoff is in progress (HORON_AGENT_RESUMING set), we must defer
    // to the server instead of exiting with "No session found matching ...".
    let with_resuming = |key: &str| {
        if key == "HORON_AGENT_RESUMING" {
            Some(std::ffi::OsString::from("1"))
        } else {
            None
        }
    };
    assert_eq!(
        resume_resolution_failure_action("ses_1780655839703_174710856", with_resuming),
        ResumeResolutionFailureAction::DeferToServer,
    );
}

#[test]
fn resume_failure_exits_when_no_handoff_in_progress() {
    // Without a reload handoff, an unresolved id is a genuine user error and
    // should still surface the actionable "use --resume to list" message + exit.
    let no_env = |_key: &str| Option::<std::ffi::OsString>::None;
    assert_eq!(
        resume_resolution_failure_action("totally-bogus-id", no_env),
        ResumeResolutionFailureAction::Exit,
    );
}

#[tokio::test]
async fn wait_for_existing_reload_server_uses_reloading_server_instead_of_spawning() {
    let _guard = crate::storage::lock_test_env();
    let env = ReloadTestEnv::new();
    crate::server::write_reload_state("reload-test", "hash", crate::server::ReloadPhase::Starting, None);

    let bind_path = env.socket_path.clone();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let bind_task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let listener = Listener::bind(&bind_path).expect("bind replacement listener");
        crate::server::write_reload_state("reload-test", "hash", crate::server::ReloadPhase::SocketReady, None);
        let _listener = listener;
        let _ = release_rx.await;
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        wait_for_existing_reload_server("test"),
    )
    .await
    .expect("reload wait should not hang");
    let _ = release_tx.send(());
    bind_task.await.expect("bind task");
    assert!(result);
}

#[tokio::test]
async fn wait_for_existing_reload_server_returns_false_for_failed_reload() {
    let _guard = crate::storage::lock_test_env();
    let _env = ReloadTestEnv::new();
    crate::server::write_reload_state(
        "reload-test",
        "hash",
        crate::server::ReloadPhase::Failed,
        Some("boom".to_string()),
    );

    assert!(!wait_for_existing_reload_server("test").await);
}

#[tokio::test]
async fn wait_for_resuming_server_detects_delayed_listener_without_marker() {
    let _guard = crate::storage::lock_test_env();
    let env = ReloadTestEnv::new();

    let bind_path = env.socket_path.clone();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let bind_task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let listener = Listener::bind(&bind_path).expect("bind delayed listener");
        let _listener = listener;
        let _ = release_rx.await;
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        wait_for_resuming_server("test", std::time::Duration::from_secs(1)),
    )
    .await
    .expect("resume wait should not hang");
    let _ = release_tx.send(());
    bind_task.await.expect("bind task");
    assert!(
        result,
        "resume wait should detect a delayed server without requiring a reload marker"
    );
}

#[tokio::test]
async fn wait_for_reloading_server_returns_false_when_idle() {
    let _guard = crate::storage::lock_test_env();
    let _env = ReloadTestEnv::new();

    assert!(!wait_for_reloading_server().await);
}

#[tokio::test]
async fn wait_for_reloading_server_returns_false_when_reload_failed() {
    let _guard = crate::storage::lock_test_env();
    let _env = ReloadTestEnv::new();
    crate::server::write_reload_state(
        "reload-test",
        "hash",
        crate::server::ReloadPhase::Failed,
        Some("boom".to_string()),
    );

    assert!(!wait_for_reloading_server().await);
}

#[tokio::test]
async fn wait_for_reloading_server_returns_true_for_live_listener() {
    let _guard = crate::storage::lock_test_env();
    let env = ReloadTestEnv::new();
    let _listener = Listener::bind(&env.socket_path).expect("bind listener");

    assert!(wait_for_reloading_server().await);
}

#[tokio::test]
async fn server_is_running_at_treats_live_listener_as_running_without_pong() {
    let temp = tempfile::tempdir().expect("tempdir");
    let socket_path = temp.path().join("horon-agent.sock");

    let _listener = Listener::bind(&socket_path).expect("bind listener");

    assert!(
        server_is_running_at(&socket_path).await,
        "a live listener should prevent duplicate server spawns even if ping is slow or absent"
    );
}

#[cfg(unix)]
#[test]
fn spawn_lock_creates_missing_runtime_directory() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("missing/nested/horon-agent.sock.spawning");
    assert!(!path.parent().unwrap().exists());

    let guard = try_acquire_spawn_lock(&path)
        .expect("create runtime directory and acquire lock")
        .expect("first lock should succeed");
    assert!(path.is_file());
    assert!(try_acquire_spawn_lock(&path).unwrap().is_none());

    drop(guard);
    assert!(!path.exists());
    assert!(path.parent().unwrap().is_dir());
}

#[test]
fn provider_profile_transport_follows_profile_type() {
    // Issue #1560: --provider-profile forced OpenAI-compatible for every profile.
    let config: crate::config::Config = toml::from_str(
        r#"
[providers.fcc-anthropic]
type = "anthropic-compatible"
base_url = "http://127.0.0.1:8082/v1"
auth = "none"

[providers.local-openai]
type = "openai-compatible"
base_url = "http://127.0.0.1:8000/v1"
auth = "none"
"#,
    )
    .expect("valid config");

    assert!(matches!(
        provider_choice_for_named_profile_in("fcc-anthropic", &config),
        ProviderChoice::Auto
    ));
    assert!(matches!(
        provider_choice_for_named_profile_in("local-openai", &config),
        ProviderChoice::OpenaiCompatible
    ));
    assert!(matches!(
        provider_choice_for_named_profile_in("missing", &config),
        ProviderChoice::OpenaiCompatible
    ));
}

/// Isolates `~/.horon-agent` for one test and serializes env mutation.
///
/// `resolve_blocked_approval` writes the decision history, so a test that calls
/// it without this would write into the real home.
struct TestHome {
    previous: Option<std::ffi::OsString>,
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TestHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("temp home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", dir.path());
        crate::config::Config::invalidate_cache();
        Self {
            previous,
            _dir: dir,
            _lock: lock,
        }
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn approvals_resolve_args(verdict: &str, tool_call_id: Option<&str>, note: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "horon-agent".to_string(),
        "approvals".to_string(),
        "resolve".to_string(),
        "--request-id".to_string(),
        "req_cli_1".to_string(),
    ];
    if let Some(tool_call_id) = tool_call_id {
        argv.push("--tool-call-id".to_string());
        argv.push(tool_call_id.to_string());
    }
    argv.push("--verdict".to_string());
    argv.push(verdict.to_string());
    if let Some(note) = note {
        argv.push("--note".to_string());
        argv.push(note.to_string());
    }
    argv
}

#[test]
fn approvals_resolve_parses_retry_and_skip() {
    use clap::Parser;

    let retry = Args::parse_from(approvals_resolve_args(
        "retry",
        Some("call_1"),
        Some("checked the journal"),
    ));
    match retry.command {
        Some(Command::Approvals {
            action:
                ApprovalsCommand::Resolve {
                    request_id,
                    tool_call_id,
                    verdict,
                    note,
                },
        }) => {
            assert_eq!(request_id, "req_cli_1");
            assert_eq!(tool_call_id, "call_1");
            assert_eq!(verdict, ApprovalVerdictArg::Retry);
            assert_eq!(note.as_deref(), Some("checked the journal"));
        }
        other => panic!("expected approvals resolve, got {other:?}"),
    }

    let skip = Args::parse_from(approvals_resolve_args("skip", Some("call_2"), None));
    match skip.command {
        Some(Command::Approvals {
            action: ApprovalsCommand::Resolve { verdict, note, .. },
        }) => {
            assert_eq!(verdict, ApprovalVerdictArg::Skip);
            assert_eq!(note, None);
        }
        other => panic!("expected approvals resolve, got {other:?}"),
    }
}

#[test]
fn approvals_resolve_requires_a_tool_call_id() {
    use clap::Parser;

    let error = Args::try_parse_from(approvals_resolve_args("retry", None, None))
        .expect_err("a verdict without --tool-call-id must be rejected");
    assert!(
        error.to_string().contains("--tool-call-id"),
        "the error must name the missing flag: {error}"
    );
}

#[test]
fn approvals_resolve_rejects_an_unknown_verdict() {
    use clap::Parser;

    let error = Args::try_parse_from(approvals_resolve_args("maybe", Some("call_1"), None))
        .expect_err("an unknown verdict must be rejected");
    assert!(
        error.to_string().contains("invalid value") && error.to_string().contains("maybe"),
        "the error must reject the value: {error}"
    );
}

#[test]
fn approvals_resolve_records_the_human_verdict() {
    let _home = TestHome::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let store = crate::ambient::BlockedApprovalStore::open(dir.path().join("blocked_approvals.json"));
    for request_id in ["req_cli_retry", "req_cli_skip"] {
        assert!(
            store
                .add(crate::ambient::BlockedApproval {
                    request_id: request_id.to_string(),
                    ..Default::default()
                })
                .expect("add record")
        );
    }

    resolve_blocked_approval(
        &store,
        "req_cli_retry",
        "call_retry",
        ApprovalVerdictArg::Retry,
        Some("the write actually landed"),
    )
    .expect("retry verdict records");
    resolve_blocked_approval(&store, "req_cli_skip", "call_skip", ApprovalVerdictArg::Skip, None)
        .expect("skip verdict records");

    let retried = store.get("req_cli_retry").expect("read record").expect("record exists");
    assert_eq!(retried.outcome.as_deref(), Some("human_retry"));
    assert_eq!(
        retried.tool_verdicts.get("call_retry").map(String::as_str),
        Some("human_retry")
    );
    let skipped = store.get("req_cli_skip").expect("read record").expect("record exists");
    assert_eq!(skipped.outcome.as_deref(), Some("human_skip"));
    assert_eq!(
        skipped.tool_verdicts.get("call_skip").map(String::as_str),
        Some("human_skip")
    );

    let error = resolve_blocked_approval(&store, "req_cli_missing", "call_x", ApprovalVerdictArg::Retry, None)
        .expect_err("an unknown request id must not be reported as recorded");
    assert!(error.to_string().contains("req_cli_missing"), "{error}");
}

/// One request can cover several tool calls. A verdict names one call, so the
/// other calls of the same request must keep their own (different) verdicts.
#[test]
fn approvals_resolve_names_one_tool_call_per_verdict() {
    let _home = TestHome::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let store = crate::ambient::BlockedApprovalStore::open(dir.path().join("blocked_approvals.json"));
    assert!(
        store
            .add(crate::ambient::BlockedApproval {
                request_id: "req_cli_multi".to_string(),
                ..Default::default()
            })
            .expect("add record")
    );

    resolve_blocked_approval(&store, "req_cli_multi", "call_a", ApprovalVerdictArg::Retry, None)
        .expect("first verdict records");
    resolve_blocked_approval(&store, "req_cli_multi", "call_b", ApprovalVerdictArg::Skip, None)
        .expect("second verdict records");

    let record = store.get("req_cli_multi").expect("read record").expect("record exists");
    assert_eq!(record.tool_verdicts.len(), 2, "each call keeps its own verdict");
    assert_eq!(
        record.tool_verdicts.get("call_a").map(String::as_str),
        Some("human_retry")
    );
    assert_eq!(
        record.tool_verdicts.get("call_b").map(String::as_str),
        Some("human_skip")
    );
    assert_eq!(
        record.outcome.as_deref(),
        Some("human_retry"),
        "the request-level outcome is set once, by the first verdict"
    );
}

/// A request the permission tool queued and is now blocked on, with the deny
/// timeout default an unattended run carries: the state QA reproduced.
fn queued_waiting_request(system: &crate::safety::SafetySystem) -> String {
    let request_id = crate::safety::new_request_id();
    let now = chrono::Utc::now();
    let queued = system.request_permission_with_timeout(
        crate::safety::PermissionRequest {
            id: request_id.clone(),
            action: "push".to_string(),
            description: "push the branch".to_string(),
            rationale: "ship the fix".to_string(),
            urgency: crate::safety::Urgency::Normal,
            wait: true,
            created_at: now,
            context: None,
            run_id: None,
            expires_at: Some(now + chrono::Duration::seconds(30)),
            default_on_timeout: crate::safety::ApprovalDefault::Deny,
        },
        std::time::Duration::from_secs(30),
    );
    assert!(
        matches!(queued, crate::safety::PermissionResult::Queued { .. }),
        "the request must queue: {queued:?}"
    );
    request_id
}

/// The measured defect: a headless turn blocked in `request_permission(wait =
/// true)` polls `history.json`, but `approvals resolve` wrote only the
/// blocked-approval record, so the turn applied its deny default while the
/// human verdict sat on disk.
///
/// The waiter here is its own `SafetySystem` instance, which is the same
/// relationship the live server process has to the CLI process: it can only
/// learn the decision from disk.
#[tokio::test]
async fn approvals_resolve_wakes_the_blocked_waiter() {
    let _home = TestHome::new();
    let dir = tempfile::tempdir().expect("store dir");
    let store = crate::ambient::BlockedApprovalStore::open(dir.path().join("blocked_approvals.json"));

    let waiter_system = std::sync::Arc::new(crate::safety::SafetySystem::new());
    let request_id = queued_waiting_request(&waiter_system);
    assert!(
        store
            .add(crate::ambient::BlockedApproval {
                request_id: request_id.clone(),
                ..Default::default()
            })
            .expect("add record")
    );

    let waiter = {
        let system = waiter_system.clone();
        let request_id = request_id.clone();
        tokio::spawn(async move {
            system
                .await_decision_with_poll_interval(
                    &request_id,
                    std::time::Duration::from_millis(700),
                    std::time::Duration::from_millis(10),
                )
                .await
        })
    };
    // Let the waiter reach its first await inside the poll loop, so the verdict
    // has to arrive through the poll and not the loop's opening check.
    tokio::task::yield_now().await;
    assert!(
        waiter_system.decision_for(&request_id).is_none(),
        "no decision may exist before the CLI records one"
    );

    resolve_blocked_approval(
        &store,
        &request_id,
        "call_qa_1",
        ApprovalVerdictArg::Retry,
        Some("the write landed"),
    )
    .expect("the verdict records");

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
        .await
        .expect("the waiter must wake instead of waiting out its window")
        .expect("the waiter task must not panic");
    match result {
        crate::safety::PermissionResult::Approved { message } => {
            let message = message.unwrap_or_default();
            assert!(
                message.contains("call_qa_1"),
                "the tool result must name the call: {message}"
            );
            assert!(
                message.contains("the write landed"),
                "the operator note must reach the model: {message}"
            );
        }
        other => panic!("the waiter must return the human verdict, not the timeout default: {other:?}"),
    }
}

/// `skip` means "do not re-run that call", and to the live waiter that is a
/// denial: `RequestPermissionTool` renders an approved result as "Permission
/// approved", which would tell the model to go ahead with the call the
/// operator refused.
#[tokio::test]
async fn a_human_skip_reaches_the_blocked_waiter_as_a_denial() {
    let _home = TestHome::new();
    let dir = tempfile::tempdir().expect("store dir");
    let store = crate::ambient::BlockedApprovalStore::open(dir.path().join("blocked_approvals.json"));

    let waiter_system = std::sync::Arc::new(crate::safety::SafetySystem::new());
    let request_id = queued_waiting_request(&waiter_system);
    assert!(
        store
            .add(crate::ambient::BlockedApproval {
                request_id: request_id.clone(),
                ..Default::default()
            })
            .expect("add record")
    );

    let waiter = {
        let system = waiter_system.clone();
        let request_id = request_id.clone();
        tokio::spawn(async move {
            system
                .await_decision_with_poll_interval(
                    &request_id,
                    std::time::Duration::from_millis(700),
                    std::time::Duration::from_millis(10),
                )
                .await
        })
    };
    tokio::task::yield_now().await;

    resolve_blocked_approval(&store, &request_id, "call_qa_skip", ApprovalVerdictArg::Skip, None)
        .expect("the verdict records");

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
        .await
        .expect("the waiter must wake instead of waiting out its window")
        .expect("the waiter task must not panic");
    match result {
        crate::safety::PermissionResult::Denied { reason } => {
            let reason = reason.unwrap_or_default();
            assert!(
                reason.contains("human skip for tool call call_qa_skip"),
                "the denial must name the human verdict: {reason}"
            );
            assert!(
                !reason.contains("No decision arrived"),
                "a human skip must not read as the timeout default: {reason}"
            );
        }
        other => panic!("a skip must reach the waiter as a denial: {other:?}"),
    }
}
