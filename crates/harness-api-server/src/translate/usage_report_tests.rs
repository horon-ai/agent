use super::*;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::time::{SystemTime, UNIX_EPOCH};

struct ScopedHome {
    path: PathBuf,
    previous: Option<OsString>,
    _guard: MutexGuard<'static, ()>,
}

impl ScopedHome {
    fn new(label: &str) -> Self {
        let guard = crate::translate::horon_agent_home_test_lock();
        let previous = std::env::var_os("HORON_AGENT_HOME");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "horon-agent-usage-report-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(path.join("sessions")).expect("create isolated home");
        // SAFETY: every test that mutates HORON_AGENT_HOME holds the shared lock.
        unsafe { std::env::set_var("HORON_AGENT_HOME", &path) };
        Self {
            path,
            previous,
            _guard: guard,
        }
    }
}

impl Drop for ScopedHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("HORON_AGENT_HOME", value) },
            None => unsafe { std::env::remove_var("HORON_AGENT_HOME") },
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Write a canonical event log with one created event and the given messages.
fn seed_session(home: &Path, id: &str, model: &str, messages: &[(u64, u64, Option<u64>, Option<u64>)]) {
    let dir = home.join("sessions").join(id);
    std::fs::create_dir_all(&dir).expect("create session dir");
    let mut lines = vec![json!({
        "event_id": format!("{id}-created"),
        "session_id": id,
        "kind": "session_created",
        "payload": {"branch_root": false, "metadata": {"model": model}},
    })];
    for (index, (input, output, cache_read, cache_creation)) in messages.iter().enumerate() {
        let mut usage = json!({"input_tokens": input, "output_tokens": output});
        if let Some(read) = cache_read {
            usage["cache_read_input_tokens"] = json!(read);
        }
        if let Some(creation) = cache_creation {
            usage["cache_creation_input_tokens"] = json!(creation);
        }
        lines.push(json!({
            "event_id": format!("{id}-m{index}"),
            "session_id": id,
            "kind": "message_appended",
            "payload": {"message": {"id": format!("msg{index}"), "role": "assistant", "token_usage": usage}},
        }));
    }
    let text: String = lines
        .iter()
        .map(|line| serde_json::to_string(line).expect("serialize event"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.join("events.jsonl"), format!("{text}\n")).expect("write event log");
}

fn state() -> BridgeState {
    BridgeState {
        session_id: Some("s1".into()),
        ..Default::default()
    }
}

fn report(state: &mut BridgeState, request: Value) -> ApiEvent {
    let mut outbound = state.api_request_to_legacy(&request);
    assert_eq!(outbound.len(), 1, "expected one reply, got {outbound:?}");
    match outbound.remove(0) {
        Outbound::Reply(frame) => frame.event,
        other => panic!("expected a local reply, got {other:?}"),
    }
}

fn usage(
    state: &mut BridgeState,
    request: Value,
) -> (
    Option<String>,
    Vec<horon_agent_harness_api::UsageSession>,
    horon_agent_harness_api::UsageTotals,
    Vec<horon_agent_harness_api::UsageModel>,
    Option<u64>,
) {
    match report(state, request) {
        ApiEvent::UsageReport {
            session_id,
            sessions,
            totals,
            models,
            context_window,
            limits: _,
            pricing,
            estimated_cost,
        } => {
            assert_eq!(pricing, "unknown");
            assert_eq!(estimated_cost, None);
            (session_id, sessions, totals, models, context_window)
        }
        other => panic!("expected a usage report, got {other:?}"),
    }
}

#[test]
fn usage_report_sums_stored_tokens_and_reports_the_route_window() {
    let home = ScopedHome::new("totals");
    seed_session(
        &home.path,
        "session_a",
        "model-x",
        &[(100, 20, Some(7), Some(3)), (200, 40, Some(9), Some(1))],
    );
    seed_session(&home.path, "session_b", "model-y", &[(50, 5, None, None)]);
    let mut state = BridgeState::default();
    state.legacy_event_to_api(&json!({"type": "model_changed", "model": "model-x", "context_window": 400_000}));

    let (session_id, sessions, totals, models, context_window) =
        usage(&mut state, json!({"req": "usage_report", "id": 7}));
    assert_eq!(context_window, Some(400_000));
    assert_eq!(session_id, None);
    let rows: BTreeMap<_, _> = sessions.iter().map(|row| (row.session_id.clone(), row)).collect();
    let a = rows["session_a"];
    assert_eq!(a.model.as_deref(), Some("model-x"));
    assert_eq!((a.input, a.output), (Some(300), Some(60)));
    assert_eq!((a.cache_read_input, a.cache_creation_input), (Some(16), Some(4)));
    assert_eq!(a.requests, 2);
    let b = rows["session_b"];
    assert_eq!(b.input, Some(50));
    // The provider never reported cache accounting for session_b: unknown, not zero.
    assert_eq!(b.cache_read_input, None);
    assert_eq!(b.cache_creation_input, None);
    assert_eq!(totals.input, Some(350));
    assert_eq!(totals.output, Some(65));
    assert_eq!(totals.requests, 3);
    assert_eq!(totals.sessions, 2);
    let models: BTreeMap<_, _> = models.iter().map(|row| (row.model.as_str(), row)).collect();
    assert_eq!(models["model-x"].input, Some(300));
    assert_eq!(models["model-x"].sessions, 1);
    assert_eq!(models["model-y"].output, Some(5));
}

#[test]
fn usage_report_never_invents_a_price() {
    let home = ScopedHome::new("pricing");
    seed_session(&home.path, "session_a", "model-x", &[(10, 2, None, None)]);
    let mut state = state();

    let event = report(&mut state, json!({"req": "usage_report", "id": 8}));
    let wire = serde_json::to_value(&event).expect("serialize report");
    assert_eq!(wire["pricing"], "unknown");
    assert!(wire["estimated_cost"].is_null(), "estimated_cost must be null: {wire}");
    assert!(
        wire.get("estimated_cost").is_some(),
        "the field is always present: {wire}"
    );
    let sessions = wire["sessions"].as_array().expect("session rows");
    let attached = sessions
        .iter()
        .find(|row| row["session_id"] == "s1")
        .expect("the attached session is reported even with no stored record");
    assert_eq!(attached["requests"], 0);
    assert!(
        attached["input"].is_null(),
        "unknown input must be null, not zero: {attached}"
    );
}

#[test]
fn usage_report_session_filter_narrows_to_one_session() {
    let home = ScopedHome::new("filter");
    seed_session(&home.path, "session_a", "model-x", &[(10, 2, None, None)]);
    seed_session(&home.path, "session_b", "model-y", &[(30, 4, None, None)]);
    let mut state = state();

    let (session_id, sessions, totals, models, _) = usage(
        &mut state,
        json!({"req": "usage_report", "id": 9, "session_id": "session_b"}),
    );
    assert_eq!(session_id.as_deref(), Some("session_b"));
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "session_b");
    assert_eq!(totals.input, Some(30));
    assert_eq!(totals.sessions, 1);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model, "model-y");
}

#[test]
fn usage_report_rejects_an_unknown_session() {
    let home = ScopedHome::new("unknown");
    seed_session(&home.path, "session_a", "model-x", &[(10, 2, None, None)]);
    let mut state = state();

    let event = report(
        &mut state,
        json!({"req": "usage_report", "id": 10, "session_id": "session_missing"}),
    );
    match event {
        ApiEvent::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::UnknownSession);
            assert!(message.contains("session_missing"), "{message}");
        }
        other => panic!("expected unknown_session, got {other:?}"),
    }
}

#[test]
fn a_deferred_report_answers_after_the_subscription_probe() {
    let home = ScopedHome::new("probe");
    seed_session(&home.path, "session_a", "model-x", &[(10, 2, None, None)]);
    let mut state = BridgeState {
        session_id: Some("s1".into()),
        runtime_commands_supported: true,
        ..Default::default()
    };

    let outbound = state.api_request_to_legacy(&json!({"req": "usage_report", "id": 12}));
    let probe = match &outbound[0] {
        Outbound::Legacy(value) => value.clone(),
        other => panic!("expected a subscription probe, got {other:?}"),
    };
    assert_eq!(probe["type"], "runtime_command");
    assert_eq!(probe["command"], "subscription");

    let frames = state.legacy_event_to_api(&json!({
        "type": "runtime_command_result",
        "id": probe["id"],
        "command": "subscription",
        "message": "",
        "data": {"configured": true, "account": {"usage": {"used_usd": 12.5, "budget_usd": 200.0, "resets_at": "2026-11-01T00:00:00Z"}}},
    }));
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].reply_to, Some(12));
    let ApiEvent::UsageReport { sessions, limits, .. } = &frames[0].event else {
        panic!("expected the deferred report, got {:?}", frames[0].event);
    };
    let row = sessions
        .iter()
        .find(|row| row.session_id == "session_a")
        .expect("the seeded session is reported");
    assert_eq!(row.input, Some(10));
    assert_eq!(limits.len(), 1);
    assert_eq!(limits[0].name, "subscription_spend");
    assert_eq!(limits[0].used, Some(12.5));
    assert_eq!(limits[0].limit, Some(200.0));
    assert_eq!(limits[0].resets_at.as_deref(), Some("2026-11-01T00:00:00Z"));

    let second = usage(&mut state, json!({"req": "usage_report", "id": 13}));
    assert_eq!(second.0, None);
}

#[test]
fn a_rejected_probe_still_answers_the_report_without_limits() {
    let home = ScopedHome::new("probe-error");
    seed_session(&home.path, "session_a", "model-x", &[(7, 3, None, None)]);
    let mut state = BridgeState {
        session_id: Some("s1".into()),
        runtime_commands_supported: true,
        ..Default::default()
    };

    let Outbound::Legacy(probe) = state
        .api_request_to_legacy(&json!({"req": "usage_report", "id": 14}))
        .remove(0)
    else {
        panic!("expected a subscription probe");
    };
    let frames = state.legacy_event_to_api(&json!({
        "type": "error",
        "id": probe["id"],
        "message": "runtime commands are unavailable",
    }));
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].reply_to, Some(14));
    let ApiEvent::UsageReport { sessions, limits, .. } = &frames[0].event else {
        panic!("expected a usage report, got {:?}", frames[0].event);
    };
    let row = sessions
        .iter()
        .find(|row| row.session_id == "session_a")
        .expect("the seeded session is reported");
    assert_eq!(row.output, Some(3));
    assert!(limits.is_empty());
}

#[test]
fn a_session_without_recorded_usage_reports_unknown_not_zero() {
    let home = ScopedHome::new("stale");
    let dir = home.path.join("sessions").join("session_quiet");
    std::fs::create_dir_all(&dir).expect("create session dir");
    std::fs::write(
        dir.join("events.jsonl"),
        format!(
            "{}\n",
            json!({
                "event_id": "quiet-created",
                "session_id": "session_quiet",
                "kind": "session_created",
                "payload": {"metadata": {"model": "model-x"}},
            })
        ),
    )
    .expect("write event log");
    let mut state = state();

    let (_, sessions, totals, _, _) = usage(
        &mut state,
        json!({"req": "usage_report", "id": 11, "session_id": "session_quiet"}),
    );
    let row = &sessions[0];
    assert_eq!(row.requests, 0);
    assert_eq!(row.input, None);
    assert_eq!(row.output, None);
    assert_eq!(row.cache_read_input, None);
    assert_eq!(totals.input, None);
    assert_eq!(totals.requests, 0);
}
