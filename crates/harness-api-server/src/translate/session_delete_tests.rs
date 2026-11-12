//! `delete_session` through the bridge: the route, the refusals, and the reply.
//!
//! The delete itself happens in the daemon or the store; what these tests own
//! is the translation. A route that answered `ok` on the daemon's `ack` would
//! report a transcript as gone before anything was removed, so the reply is
//! driven through the same `done` frame the daemon sends.

use super::*;
use horon_agent_harness_api::ApiEvent;
use serde_json::json;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::MutexGuard;

struct ScopedHome {
    path: PathBuf,
    previous: Option<OsString>,
    _guard: MutexGuard<'static, ()>,
}

impl ScopedHome {
    fn new(label: &str) -> Self {
        let guard = horon_agent_home_test_lock();
        let previous = std::env::var_os("HORON_HOME");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("horon-agent-delete-{label}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create isolated home");
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(path.join("sessions"))
                .expect("create private sessions dir");
        }
        // SAFETY: every test that mutates HORON_HOME holds this lock.
        unsafe { std::env::set_var("HORON_HOME", &path) };
        Self {
            path,
            previous,
            _guard: guard,
        }
    }

    fn sessions(&self) -> PathBuf {
        self.path.join("sessions")
    }
}

impl Drop for ScopedHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("HORON_HOME", value) },
            None => unsafe { std::env::remove_var("HORON_HOME") },
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A stored session in the preserved layout, plus one base-owned file keyed by
/// its id, so a delete has something to prove.
fn seed_session(home: &ScopedHome, session_id: &str) {
    std::fs::write(
        home.sessions().join(format!("{session_id}.json")),
        json!({
            "id": session_id,
            "title": "A transcript",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "working_dir": "/tmp",
            "messages": [],
        })
        .to_string(),
    )
    .expect("write session record");
    let stray = home.path.join("todos");
    std::fs::create_dir_all(&stray).expect("todos dir");
    std::fs::write(stray.join(format!("{session_id}.json")), b"[]").expect("todo file");
}

fn reply_event(outbound: Vec<Outbound>) -> ApiEvent {
    assert_eq!(outbound.len(), 1, "expected exactly one reply: {outbound:?}");
    match outbound.into_iter().next().expect("one outbound") {
        Outbound::Reply(frame) => frame.event,
        other => panic!("expected an API reply, got {other:?}"),
    }
}

fn delete_request(session_id: &str) -> serde_json::Value {
    json!({"v": 1, "id": 7, "req": "delete_session", "session_id": session_id})
}

#[test]
fn a_delete_is_forwarded_with_confirm_set() {
    let _home = ScopedHome::new("forwarded");
    seed_session(&_home, "s-delete");
    let mut state = BridgeState::default();

    let outbound = state.api_request_to_legacy(&delete_request("s-delete"));

    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("a delete must reach the daemon that owns the session map: {outbound:?}");
    };
    assert_eq!(request["type"], "delete_session");
    assert_eq!(request["session_id"], "s-delete");
    assert_eq!(
        request["confirm"], true,
        "the wire default is refusal; the bridge states the client's intent explicitly"
    );
}

#[test]
fn an_unknown_session_is_refused_before_anything_is_sent() {
    let _home = ScopedHome::new("unknown");
    let mut state = BridgeState::default();

    let outbound = state.api_request_to_legacy(&delete_request("s-never-existed"));

    let ApiEvent::Error { code, message, .. } = reply_event(outbound) else {
        panic!("a delete for an unknown session must be an error");
    };
    assert_eq!(code, ErrorCode::UnknownSession, "{message}");
}

#[test]
fn an_empty_session_id_is_invalid_request() {
    let _home = ScopedHome::new("empty");
    let mut state = BridgeState::default();

    let ApiEvent::Error { code, .. } = reply_event(state.api_request_to_legacy(&delete_request(""))) else {
        panic!("an empty session id must be refused");
    };
    assert_eq!(code, ErrorCode::InvalidRequest);
}

#[test]
fn a_session_attached_to_this_connection_is_refused() {
    let home = ScopedHome::new("attached");
    seed_session(&home, "s-attached");
    let mut state = BridgeState {
        session_id: Some("s-attached".into()),
        ..Default::default()
    };

    let ApiEvent::Error { code, message, .. } = reply_event(state.api_request_to_legacy(&delete_request("s-attached")))
    else {
        panic!("deleting the attached session must be refused");
    };
    assert_eq!(code, ErrorCode::InvalidRequest, "{message}");
    assert!(
        home.sessions().join("s-attached.json").is_file(),
        "a refused delete removes nothing"
    );
}

#[test]
fn the_daemons_done_answers_ok_and_its_ack_does_not() {
    let home = ScopedHome::new("done");
    seed_session(&home, "s-done");
    let mut state = BridgeState::default();
    let outbound = state.api_request_to_legacy(&delete_request("s-done"));
    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("expected a forwarded delete: {outbound:?}");
    };
    let legacy_id = request["id"].as_u64().expect("legacy id");

    // `ack` means the daemon received the request. The delete may still be
    // refused, so it must not answer the API request.
    let after_ack = state.legacy_event_to_api(&json!({"type": "ack", "id": legacy_id}));
    assert!(after_ack.is_empty(), "an ack is not a completed delete: {after_ack:?}");

    let frames = state.legacy_event_to_api(&json!({"type": "done", "id": legacy_id}));
    assert_eq!(frames.len(), 1);
    assert!(matches!(frames[0].event, ApiEvent::Ok), "{:?}", frames[0].event);
    assert_eq!(frames[0].reply_to, Some(7));
}

#[test]
fn a_refusal_from_the_daemon_becomes_an_error_reply() {
    let home = ScopedHome::new("refused");
    seed_session(&home, "s-refused");
    let mut state = BridgeState::default();
    let outbound = state.api_request_to_legacy(&delete_request("s-refused"));
    let [Outbound::Legacy(request)] = &outbound[..] else {
        panic!("expected a forwarded delete: {outbound:?}");
    };
    let legacy_id = request["id"].as_u64().expect("legacy id");

    let frames = state.legacy_event_to_api(&json!({
        "type": "error",
        "id": legacy_id,
        "message": "session is open: loaded in this daemon, streaming, or attached to a client"
    }));
    assert_eq!(frames.len(), 1, "{frames:?}");
    let ApiEvent::Error { message, .. } = &frames[0].event else {
        panic!(
            "a daemon refusal must reach the client as an error: {:?}",
            frames[0].event
        );
    };
    assert!(message.contains("open"), "{message}");
    assert_eq!(frames[0].reply_to, Some(7));
}
