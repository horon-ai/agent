use super::*;

pub(super) fn with_temp_home<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
{
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("HORON_AGENT_HOME");
    let temp = tempfile::TempDir::new().expect("create temp dir");
    crate::env::set_var("HORON_AGENT_HOME", temp.path());

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    match prev_home {
        Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }

    result.unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

#[test]
fn test_classify_auto_allowed() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        assert_eq!(sys.classify("read"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("glob"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("grep"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("ls"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("memory"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("todo"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("todowrite"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("todoread"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("conversation_search"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("session_search"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("codesearch"), ActionTier::AutoAllowed);
    });
}

#[test]
fn test_classify_requires_permission() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        assert_eq!(sys.classify("bash"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("write"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("edit"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("multiedit"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("patch"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("apply_patch"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("communicate"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("open"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("launch"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("webfetch"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("websearch"), ActionTier::RequiresPermission);
        assert_eq!(sys.classify("unknown_tool"), ActionTier::RequiresPermission);
    });
}

#[test]
fn test_classify_case_insensitive() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        assert_eq!(sys.classify("Read"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("GLOB"), ActionTier::AutoAllowed);
        assert_eq!(sys.classify("Bash"), ActionTier::RequiresPermission);
    });
}

#[test]
fn test_request_permission_returns_queued() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        let baseline = sys.pending_requests().len();
        let req = PermissionRequest {
            id: "req_test_1".to_string(),
            action: "create_pull_request".to_string(),
            description: "Create PR for test fixes".to_string(),
            rationale: "Found failing tests".to_string(),
            urgency: Urgency::Normal,
            wait: false,
            created_at: Utc::now(),
            context: None,
            run_id: None,
            expires_at: None,
            default_on_timeout: ApprovalDefault::Deny,
        };

        let result = sys.request_permission(req);
        match result {
            PermissionResult::Queued { request_id } => {
                assert_eq!(request_id, "req_test_1");
            }
            _ => panic!("Expected Queued result"),
        }

        assert_eq!(sys.pending_requests().len(), baseline + 1);
    });
}

#[test]
fn test_record_decision_removes_from_queue() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        let baseline = sys.pending_requests().len();
        let req = PermissionRequest {
            id: "req_test_2".to_string(),
            action: "push".to_string(),
            description: "Push to origin".to_string(),
            rationale: "Ready for review".to_string(),
            urgency: Urgency::Low,
            wait: false,
            created_at: Utc::now(),
            context: None,
            run_id: None,
            expires_at: None,
            default_on_timeout: ApprovalDefault::Deny,
        };

        sys.request_permission(req);
        assert_eq!(sys.pending_requests().len(), baseline + 1);

        sys.record_decision("req_test_2", true, "tui", Some("looks good".to_string()))
            .unwrap();
        assert_eq!(sys.pending_requests().len(), baseline);
    });
}

#[test]
fn test_log_action_and_summary() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        sys.log_action(ActionLog {
            action_type: "memory_consolidation".to_string(),
            description: "Merged 2 duplicate memories".to_string(),
            tier: ActionTier::AutoAllowed,
            details: None,
            timestamp: Utc::now(),
        });
        sys.log_action(ActionLog {
            action_type: "edit".to_string(),
            description: "Fixed typo in README".to_string(),
            tier: ActionTier::RequiresPermission,
            details: None,
            timestamp: Utc::now(),
        });

        let summary = sys.generate_summary();
        assert!(summary.contains("memory_consolidation"));
        assert!(summary.contains("edit"));
        assert!(summary.contains("Done (auto-allowed)"));
        assert!(summary.contains("Done (with permission)"));
    });
}

#[test]
fn test_empty_summary() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        let summary = sys.generate_summary();
        assert_eq!(summary, "No actions recorded.");
    });
}

#[test]
fn test_new_request_id_format() {
    with_temp_home(|| {
        let id = new_request_id();
        assert!(id.starts_with("req_"));
    });
}

#[test]
fn new_permission_fields_round_trip() {
    let expires_at = Utc::now() + chrono::Duration::seconds(1800);
    let request = PermissionRequest {
        id: "req_round_trip".to_string(),
        action: "push".to_string(),
        description: "Push to origin".to_string(),
        rationale: "Ready for review".to_string(),
        urgency: Urgency::Normal,
        wait: true,
        created_at: Utc::now(),
        context: None,
        run_id: Some("run_42".to_string()),
        expires_at: Some(expires_at),
        default_on_timeout: ApprovalDefault::Approve,
    };

    let encoded = serde_json::to_string(&request).expect("serialize request");
    let decoded: PermissionRequest = serde_json::from_str(&encoded).expect("parse request");
    assert_eq!(decoded.run_id.as_deref(), Some("run_42"));
    assert_eq!(decoded.expires_at, Some(expires_at));
    assert_eq!(decoded.default_on_timeout, ApprovalDefault::Approve);
    assert!(encoded.contains("\"default_on_timeout\":\"approve\""));
}

#[test]
fn past_expires_at_is_detectable_as_expired() {
    let now = Utc::now();
    let base = |expires_at: Option<DateTime<Utc>>| PermissionRequest {
        id: "req_expiry".to_string(),
        action: "push".to_string(),
        description: "Push to origin".to_string(),
        rationale: "Ready for review".to_string(),
        urgency: Urgency::Normal,
        wait: true,
        created_at: now,
        context: None,
        run_id: Some("run_42".to_string()),
        expires_at,
        default_on_timeout: ApprovalDefault::Deny,
    };

    assert!(
        base(Some(now - chrono::Duration::seconds(1))).is_expired_at(now),
        "a request past its deadline must read as expired"
    );
    assert!(!base(Some(now + chrono::Duration::seconds(60))).is_expired_at(now));
    assert!(
        !base(None).is_expired_at(now),
        "a request without a deadline never expires on time alone"
    );

    // The same request must still read as expired after a file round trip,
    // which is how the runner sees it after a restart.
    let expired = base(Some(now - chrono::Duration::seconds(1)));
    let reloaded: PermissionRequest =
        serde_json::from_str(&serde_json::to_string(&expired).expect("serialize")).expect("parse");
    assert!(reloaded.is_expired_at(now));
}

#[test]
fn approval_default_from_config_str_fails_closed() {
    assert_eq!(ApprovalDefault::from_config_str("deny"), ApprovalDefault::Deny);
    assert_eq!(ApprovalDefault::from_config_str("approve"), ApprovalDefault::Approve);
    assert_eq!(ApprovalDefault::from_config_str("  APPROVE "), ApprovalDefault::Approve);
    assert_eq!(ApprovalDefault::from_config_str("allow"), ApprovalDefault::Approve);
    assert_eq!(ApprovalDefault::from_config_str(""), ApprovalDefault::Deny);
    assert_eq!(ApprovalDefault::from_config_str("yes"), ApprovalDefault::Deny);
    assert_eq!(ApprovalDefault::default(), ApprovalDefault::Deny);
}

#[test]
fn test_record_permission_via_file() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        let baseline = sys.pending_requests().len();
        let req = PermissionRequest {
            id: "req_file_test".to_string(),
            action: "push".to_string(),
            description: "Push to origin".to_string(),
            rationale: "Ready for review".to_string(),
            urgency: Urgency::Low,
            wait: false,
            created_at: Utc::now(),
            context: None,
            run_id: None,
            expires_at: None,
            default_on_timeout: ApprovalDefault::Deny,
        };
        sys.request_permission(req);
        assert_eq!(sys.pending_requests().len(), baseline + 1);

        record_permission_via_file("req_file_test", true, "email_reply", None).unwrap();

        let sys2 = SafetySystem::new();
        let still_pending = sys2.pending_requests().iter().any(|r| r.id == "req_file_test");
        assert!(!still_pending, "request should have been removed from queue");
    });
}

// -----------------------------------------------------------------
// Bounded approval wait, cross-process decisions, and notifiers
// -----------------------------------------------------------------

fn pending_request(id: &str, default_on_timeout: ApprovalDefault) -> PermissionRequest {
    PermissionRequest {
        id: id.to_string(),
        action: "push".to_string(),
        description: "Push to origin".to_string(),
        rationale: "Ready for review".to_string(),
        urgency: Urgency::Normal,
        wait: true,
        created_at: Utc::now(),
        context: None,
        run_id: Some("run_test".to_string()),
        expires_at: None,
        default_on_timeout,
    }
}

fn read_history_decisions() -> Vec<Decision> {
    let path = history_path().expect("history path");
    if !path.exists() {
        return Vec::new();
    }
    let raw = std::fs::read_to_string(&path).expect("read history");
    serde_json::from_str(&raw).expect("history must stay valid JSON")
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build a test runtime")
        .block_on(future)
}

#[test]
fn await_decision_returns_an_in_process_decision() {
    with_temp_home(|| {
        let sys = Arc::new(SafetySystem::new());
        sys.request_permission(pending_request("req_in_process", ApprovalDefault::Deny));

        let waiter = {
            let sys = Arc::clone(&sys);
            std::thread::spawn(move || {
                block_on(sys.await_decision_with_poll_interval(
                    "req_in_process",
                    Duration::from_secs(5),
                    Duration::from_millis(10),
                ))
            })
        };

        std::thread::sleep(Duration::from_millis(20));
        sys.record_decision("req_in_process", true, "tui", Some("looks good".to_string()))
            .unwrap();

        let result = waiter.join().expect("waiter thread");
        assert!(
            matches!(result, PermissionResult::Approved { .. }),
            "in-process approval must wake the waiter immediately, got {result:?}"
        );
    });
}

#[test]
fn await_decision_sees_a_decision_written_only_to_the_file() {
    with_temp_home(|| {
        let sys = Arc::new(SafetySystem::new());
        sys.request_permission(pending_request("req_file_only", ApprovalDefault::Deny));

        let waiter = {
            let sys = Arc::clone(&sys);
            std::thread::spawn(move || {
                block_on(sys.await_decision_with_poll_interval(
                    "req_file_only",
                    Duration::from_secs(5),
                    Duration::from_millis(10),
                ))
            })
        };

        let started = std::time::Instant::now();
        std::thread::sleep(Duration::from_millis(30));
        record_permission_via_file("req_file_only", true, "email_reply", None).unwrap();

        let result = waiter.join().expect("waiter thread");
        assert!(
            matches!(result, PermissionResult::Approved { .. }),
            "a decision written only to the file must be picked up, got {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "file-only decision must be reflected within one second, took {:?}",
            started.elapsed()
        );
    });
}

#[test]
fn await_decision_times_out_with_the_default_deny() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        sys.request_permission(pending_request("req_timeout_deny", ApprovalDefault::Deny));

        let result = block_on(sys.await_decision_with_poll_interval(
            "req_timeout_deny",
            Duration::from_millis(100),
            Duration::from_millis(10),
        ));

        assert!(
            matches!(result, PermissionResult::Denied { .. }),
            "an unanswered request with the deny default must be denied, got {result:?}"
        );
        let decisions = read_history_decisions();
        assert_eq!(decisions.len(), 1, "exactly one decision must be recorded");
        assert_eq!(decisions[0].decided_via, TIMEOUT_DEFAULT_VIA);
        assert!(!decisions[0].approved);
    });
}

#[test]
fn await_decision_honors_an_approve_default() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        sys.request_permission(pending_request("req_timeout_approve", ApprovalDefault::Approve));

        let result = block_on(sys.await_decision_with_poll_interval(
            "req_timeout_approve",
            Duration::from_millis(100),
            Duration::from_millis(10),
        ));

        assert!(
            matches!(result, PermissionResult::Approved { .. }),
            "an approve default must be applied and recorded, got {result:?}"
        );
        let decisions = read_history_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].decided_via, TIMEOUT_DEFAULT_VIA);
        assert!(decisions[0].approved);
    });
}

#[test]
fn await_decision_never_approves_when_the_decision_lock_is_busy() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        sys.request_permission(pending_request("req_lock_busy", ApprovalDefault::Approve));

        let lock_path = decisions_lock_path().expect("decision lock path");
        std::fs::create_dir_all(lock_path.parent().expect("lock dir")).unwrap();
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("open lock file");
        held.try_lock().expect("hold the decision lock");

        let result = block_on(sys.await_decision_with_poll_interval(
            "req_lock_busy",
            Duration::from_millis(40),
            Duration::from_millis(10),
        ));

        assert!(
            matches!(result, PermissionResult::Queued { .. }),
            "a lock failure must stay queued even with an approve default, got {result:?}"
        );
        assert!(read_history_decisions().is_empty());

        held.unlock().expect("release the decision lock");
    });
}

#[test]
fn decision_for_reads_the_file_and_notifies_each_callback_once() {
    with_temp_home(|| {
        reset_decision_notifiers_for_tests();
        let sys = SafetySystem::new();
        sys.request_permission(pending_request("req_multi_subscriber", ApprovalDefault::Deny));

        let first = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let first = Arc::clone(&first);
            register_decision_notifier(Arc::new(move |_decision: &Decision| {
                first.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
        }
        {
            let second = Arc::clone(&second);
            register_decision_notifier(Arc::new(move |_decision: &Decision| {
                second.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
        }

        // The live instance never sees this write: it arrives through the
        // history file only.
        record_permission_via_file("req_multi_subscriber", true, "email_reply", None).unwrap();

        let found = sys.decision_for("req_multi_subscriber");
        assert!(found.is_some(), "decision_for must read the file when memory misses");
        assert!(found.expect("decision").approved);
        assert!(sys.decision_for("req_multi_subscriber").is_some());

        assert_eq!(first.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(second.load(std::sync::atomic::Ordering::SeqCst), 1);
        reset_decision_notifiers_for_tests();
    });
}

#[test]
fn decision_notification_survives_a_panicking_callback() {
    with_temp_home(|| {
        reset_decision_notifiers_for_tests();
        let sys = SafetySystem::new();
        sys.request_permission(pending_request("req_panicking_callback", ApprovalDefault::Deny));

        register_decision_notifier(Arc::new(|_decision: &Decision| panic!("notifier exploded")));
        let survived = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let survived = Arc::clone(&survived);
            register_decision_notifier(Arc::new(move |_decision: &Decision| {
                survived.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
        }

        record_permission_via_file("req_panicking_callback", true, "email_reply", None).unwrap();
        assert!(sys.decision_for("req_panicking_callback").is_some());
        assert_eq!(survived.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(read_history_decisions().len(), 1);
        reset_decision_notifiers_for_tests();
    });
}

#[test]
fn corrupt_history_is_preserved_and_denies_the_timeout_default() {
    with_temp_home(|| {
        let path = history_path().expect("history path");
        std::fs::create_dir_all(path.parent().expect("history dir")).unwrap();
        std::fs::write(&path, "{ this is not json").unwrap();

        let sys = SafetySystem::new();
        assert!(sys.history_is_corrupt(), "a corrupt history must mark the system");
        sys.request_permission(pending_request("req_corrupt", ApprovalDefault::Approve));

        let result = block_on(sys.await_decision_with_poll_interval(
            "req_corrupt",
            Duration::from_millis(100),
            Duration::from_millis(10),
        ));

        assert!(
            matches!(result, PermissionResult::Denied { .. }),
            "a corrupt history must deny even with an approve default, got {result:?}"
        );

        let decisions = read_history_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].decided_via, CORRUPT_HISTORY_VIA);
        assert!(!decisions[0].approved);

        let quarantined: Vec<_> = std::fs::read_dir(path.parent().expect("history dir"))
            .expect("read history dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("history.corrupt-"))
            })
            .collect();
        assert_eq!(quarantined.len(), 1, "the corrupt file must be copied aside once");
        assert_eq!(
            std::fs::read_to_string(&quarantined[0]).expect("read quarantine copy"),
            "{ this is not json"
        );
    });
}

#[test]
fn record_decision_keeps_the_first_decision() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        sys.request_permission(pending_request("req_first_wins", ApprovalDefault::Deny));

        let first = sys
            .record_decision("req_first_wins", true, "tui", None)
            .expect("record the first decision");
        assert!(matches!(first, RecordDecisionOutcome::Recorded(_)));

        let second = sys
            .record_decision("req_first_wins", false, TIMEOUT_DEFAULT_VIA, None)
            .expect("second write must not fail");
        match second {
            RecordDecisionOutcome::AlreadyDecided { existing } => assert!(existing.approved),
            RecordDecisionOutcome::Recorded(_) => panic!("a second decision must not be appended"),
        }

        let decisions = read_history_decisions();
        let matching: Vec<_> = decisions
            .iter()
            .filter(|decision| decision.request_id == "req_first_wins")
            .collect();
        assert_eq!(matching.len(), 1, "exactly one decision per request id");
        assert!(matching[0].approved);
        assert_eq!(matching[0].decided_via, "tui");
    });
}

#[test]
fn concurrent_file_and_memory_records_keep_one_decision() {
    with_temp_home(|| {
        let sys = Arc::new(SafetySystem::new());
        sys.request_permission(pending_request("req_race_writers", ApprovalDefault::Deny));

        let file_writer =
            std::thread::spawn(|| record_permission_via_file("req_race_writers", true, "email_reply", None));
        let memory_writer = {
            let sys = Arc::clone(&sys);
            std::thread::spawn(move || sys.record_decision("req_race_writers", false, "tui", None))
        };

        file_writer.join().expect("file writer thread").unwrap();
        memory_writer.join().expect("memory writer thread").unwrap();

        let decisions = read_history_decisions();
        let matching: Vec<_> = decisions
            .iter()
            .filter(|decision| decision.request_id == "req_race_writers")
            .collect();
        assert_eq!(matching.len(), 1, "the decision lock must keep a single record");
        assert!(matching[0].decided_via == "email_reply" || matching[0].decided_via == "tui");
    });
}

#[test]
fn approval_and_timeout_at_the_same_moment_leave_one_decision() {
    with_temp_home(|| {
        let sys = Arc::new(SafetySystem::new());
        sys.request_permission(pending_request("req_race_timeout", ApprovalDefault::Approve));

        let waiter = {
            let sys = Arc::clone(&sys);
            std::thread::spawn(move || {
                block_on(sys.await_decision_with_poll_interval(
                    "req_race_timeout",
                    Duration::from_millis(120),
                    Duration::from_millis(5),
                ))
            })
        };

        // Land the explicit decision inside the final stretch of the wait,
        // so it competes with the timeout default.
        std::thread::sleep(Duration::from_millis(110));
        let _ = sys.record_decision("req_race_timeout", false, "tester_deny", None);

        let result = waiter.join().expect("waiter thread");
        let decisions = read_history_decisions();
        let matching: Vec<_> = decisions
            .iter()
            .filter(|decision| decision.request_id == "req_race_timeout")
            .collect();
        assert_eq!(matching.len(), 1, "exactly one decision must survive the race");
        let via = matching[0].decided_via.as_str();
        assert!(
            via == "tester_deny" || via == TIMEOUT_DEFAULT_VIA,
            "unexpected decision source {via}"
        );
        match result {
            PermissionResult::Approved { .. } => assert!(matching[0].approved),
            PermissionResult::Denied { .. } => assert!(!matching[0].approved),
            other => panic!("unexpected result {other:?}"),
        }
    });
}

#[test]
fn request_permission_fills_the_expiry_window() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        let before = Utc::now();
        sys.request_permission_with_timeout(
            pending_request("req_expiry_fill", ApprovalDefault::Deny),
            Duration::from_secs(60),
        );
        sys.request_permission(pending_request("req_expiry_default", ApprovalDefault::Deny));

        let pending = sys.pending_requests();
        let short = pending
            .iter()
            .find(|request| request.id == "req_expiry_fill")
            .expect("short request must be queued");
        let short_expiry = short.expires_at.expect("expires_at must be filled");
        assert!(short_expiry >= before + chrono::Duration::seconds(59));
        assert!(short_expiry <= Utc::now() + chrono::Duration::seconds(61));

        let default = pending
            .iter()
            .find(|request| request.id == "req_expiry_default")
            .expect("default request must be queued");
        let default_expiry = default.expires_at.expect("default expiry must be filled");
        let remaining = default_expiry - Utc::now();
        assert!(
            remaining >= chrono::Duration::seconds(1790),
            "the default wait window must be 30 minutes, got {remaining:?}"
        );
    });
}

#[test]
fn expire_dead_session_requests_honors_the_wait_deadline() {
    with_temp_home(|| {
        let sys = SafetySystem::new();
        let mut expired = pending_request("req_time_expired", ApprovalDefault::Deny);
        expired.expires_at = Some(Utc::now() - chrono::Duration::seconds(5));
        let mut live = pending_request("req_time_live", ApprovalDefault::Deny);
        live.expires_at = Some(Utc::now() + chrono::Duration::seconds(600));
        sys.request_permission(expired);
        sys.request_permission(live);

        let ids = sys.expire_dead_session_requests("test_gc").expect("sweep");
        assert_eq!(ids, vec!["req_time_expired".to_string()]);

        let pending = sys.pending_requests();
        assert!(!pending.iter().any(|request| request.id == "req_time_expired"));
        assert!(pending.iter().any(|request| request.id == "req_time_live"));

        let decisions = read_history_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].decided_via, "test_gc");
        assert!(!decisions[0].approved);
        assert!(
            decisions[0]
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("wait window elapsed"),
            "the time-expiry reason must name the wait window"
        );
    });
}
