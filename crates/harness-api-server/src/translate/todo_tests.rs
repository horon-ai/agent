//! Todo edit translation through the bridge.
//!
//! A child of `translate` because it builds `BridgeState`, whose fields are
//! private to that module. Kept in its own file so the shared translation test
//! file stays at its test-size ceiling.

use super::{BridgeState, Outbound, horon_agent_home_test_lock};
use horon_agent_harness_api::ApiEvent;
use serde_json::json;

fn state_with_session() -> BridgeState {
    BridgeState {
        session_id: Some("s1".into()),
        ..Default::default()
    }
}

fn only_reply_event(outbound: Vec<Outbound>) -> ApiEvent {
    assert_eq!(outbound.len(), 1, "expected exactly one reply");
    match outbound.into_iter().next().expect("one outbound") {
        Outbound::Reply(frame) => frame.event,
        other => panic!("expected an API reply, got {other:?}"),
    }
}

#[test]
fn todo_status_edit_is_forwarded_and_the_daemons_reply_carries_the_refresh() {
    let _lock = horon_agent_home_test_lock();
    let home = std::env::temp_dir().join(format!("bridge-todo-edit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("todos")).unwrap();
    let list = r#"[{"content":"ship","status":"pending","priority":"high","id":"ship"}]"#;
    std::fs::write(home.join("todos/s1.json"), list).unwrap();
    let previous = std::env::var_os("HORON_HOME");
    unsafe { std::env::set_var("HORON_HOME", &home) };
    let mut state = state_with_session();

    let event = only_reply_event(state.api_request_to_legacy(&json!({
        "req": "get_todo_state", "id": 94, "session_id": "s1"
    })));
    let ApiEvent::TodoState { revision, .. } = event else {
        panic!("expected todo state, got {event:?}");
    };
    let revision = revision.expect("the bridge reports the list revision");

    // The bridge forwards the edit; it never writes the list itself, so the
    // daemon owns the lock and the `TodoChanged` push.
    let forwarded = state.api_request_to_legacy(&json!({
        "req": "set_todo_status", "id": 95, "session_id": "s1",
        "expected_revision": revision, "id": "ship", "status": "completed"
    }));
    let Outbound::Legacy(frame) = forwarded.into_iter().next().expect("one forwarded frame") else {
        panic!("the edit must be forwarded to the daemon");
    };
    assert_eq!(frame["type"], "set_todo_status");
    assert_eq!(frame["expected_revision"], revision);
    assert_eq!(frame["todo_id"], "ship");
    assert_eq!(frame["status"], "completed");
    let legacy_id = frame["id"].as_u64().expect("a legacy request id");

    // The daemon wrote the list and answered: the API reply carries the state
    // and revision read back, as one pair.
    let completed = r#"[{"content":"ship","status":"completed","priority":"high","id":"ship"}]"#;
    std::fs::write(home.join("todos/s1.json"), completed).unwrap();
    let frames = state.legacy_event_to_api(&json!({
        "type": "todo_edit_result", "id": legacy_id, "applied": true
    }));
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].reply_to, Some(95));
    let ApiEvent::TodoEditResult {
        applied,
        refusal,
        state: snapshot,
        revision: current,
        ..
    } = frames[0].event.clone()
    else {
        panic!("expected an edit result, got {:?}", frames[0].event);
    };
    assert!(applied);
    assert!(refusal.is_none());
    assert_eq!(snapshot.todos[0].status, "completed");
    assert!(!current.is_empty());

    // A stale refusal is a reply, not an error frame: it names the reason and
    // carries the current list for the client to refresh.
    let forwarded = state.api_request_to_legacy(&json!({
        "req": "set_todo_status", "id": 96, "session_id": "s1",
        "expected_revision": revision, "id": "ship", "status": "cancelled"
    }));
    let Outbound::Legacy(frame) = forwarded.into_iter().next().expect("one forwarded frame") else {
        panic!("the edit must be forwarded to the daemon");
    };
    let legacy_id = frame["id"].as_u64().expect("a legacy request id");
    let frames = state.legacy_event_to_api(&json!({
        "type": "todo_edit_result", "id": legacy_id, "applied": false, "refusal": "stale_revision"
    }));
    assert_eq!(frames.len(), 1);
    let ApiEvent::TodoEditResult {
        applied,
        refusal,
        state: snapshot,
        ..
    } = frames[0].event.clone()
    else {
        panic!("expected an edit result, got {:?}", frames[0].event);
    };
    assert!(!applied);
    assert_eq!(refusal, Some(horon_agent_todo_policy::TodoEditRefusal::StaleRevision));
    assert_eq!(snapshot.todos[0].status, "completed");

    match previous {
        Some(value) => unsafe { std::env::set_var("HORON_HOME", value) },
        None => unsafe { std::env::remove_var("HORON_HOME") },
    }
    let _ = std::fs::remove_dir_all(&home);
}
