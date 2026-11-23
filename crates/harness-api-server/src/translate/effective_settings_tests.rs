//! Tests for the forwarded `get_effective_settings`.

use super::*;

fn state() -> BridgeState {
    BridgeState {
        session_id: Some("s1".into()),
        ..Default::default()
    }
}

fn request(api_id: u64, session_id: &str) -> Value {
    json!({
        "v": 1,
        "id": api_id,
        "req": "get_effective_settings",
        "session_id": session_id,
    })
}

fn only<T: std::fmt::Debug>(mut items: Vec<T>) -> T {
    assert_eq!(items.len(), 1, "expected exactly one item, got {items:?}");
    items.remove(0)
}

fn forwarded(outbound: Vec<Outbound>) -> Value {
    match only(outbound) {
        Outbound::Legacy(value) => value,
        other => panic!("expected a forwarded request, got {other:?}"),
    }
}

#[test]
fn the_request_reaches_the_daemon_without_a_field_the_daemon_ignores() {
    let legacy = forwarded(request_with(
        &mut state(),
        &request(41, "s1"),
        41,
        "get_effective_settings",
        true,
    ));
    assert_eq!(legacy["type"], "get_effective_settings");
    assert!(
        legacy.get("session_id").is_none(),
        "the daemon answers for itself; a session id it ignores would only invite a reader to trust it"
    );
}

#[test]
fn a_daemon_that_cannot_answer_is_refused_by_capability_name() {
    let outbound = request_with(&mut state(), &request(42, "s1"), 42, "get_effective_settings", false);
    let frame = match only(outbound) {
        Outbound::Reply(frame) => frame,
        other => panic!("expected the structured refusal, got {other:?}"),
    };
    assert_eq!(frame.reply_to, Some(42));
    match frame.event {
        ApiEvent::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::Unsupported);
            assert!(
                message.contains(CAPABILITY_EFFECTIVE_SETTINGS_V1),
                "the refusal names the capability a client checks: {message}"
            );
        }
        other => panic!("expected the structured refusal, got {other:?}"),
    }
}

#[test]
fn an_empty_session_id_is_refused_before_the_daemon_is_asked() {
    let outbound = request_with(&mut state(), &request(43, ""), 43, "get_effective_settings", true);
    let frame = match only(outbound) {
        Outbound::Reply(frame) => frame,
        other => panic!("expected the validation refusal, got {other:?}"),
    };
    assert!(
        matches!(
            &frame.event,
            ApiEvent::Error {
                code: ErrorCode::InvalidRequest,
                ..
            }
        ),
        "an empty session id never leaves the bridge, got {:?}",
        frame.event
    );
}

#[test]
fn the_daemons_rows_become_the_api_reply_naming_the_session_it_serves() {
    let mut state = state();
    let legacy = forwarded(request_with(
        &mut state,
        &request(44, "s1"),
        44,
        "get_effective_settings",
        true,
    ));
    let frame = only(reply(
        &mut state,
        &json!({
            "type": "effective_settings",
            "id": legacy["id"],
            "settings": [{
                "key": "tools.profile",
                "label": "Tool profile",
                "value": "full",
                "source": "cli",
                "origin": "--tool-profile",
            }],
        }),
    ));
    assert_eq!(frame.reply_to, Some(44));
    match frame.event {
        ApiEvent::EffectiveSettings { session_id, settings } => {
            assert_eq!(session_id, "s1");
            assert_eq!(settings.len(), 1);
            assert_eq!(settings[0].source, horon_agent_harness_api::SettingSource::Cli);
            assert_eq!(settings[0].origin.as_deref(), Some("--tool-profile"));
        }
        other => panic!("expected effective_settings, got {other:?}"),
    }
}

#[test]
fn an_unreadable_row_list_is_an_error_rather_than_a_guess() {
    let mut state = state();
    let legacy = forwarded(request_with(
        &mut state,
        &request(45, "s1"),
        45,
        "get_effective_settings",
        true,
    ));
    let frame = only(reply(
        &mut state,
        &json!({
            "type": "effective_settings",
            "id": legacy["id"],
            "settings": [{ "key": "tools.profile" }],
        }),
    ));
    assert_eq!(frame.reply_to, Some(45));
    assert!(
        matches!(
            &frame.event,
            ApiEvent::Error {
                code: ErrorCode::Internal,
                ..
            }
        ),
        "a half-decoded row list must not be passed on, got {:?}",
        frame.event
    );
}

#[test]
fn a_reply_no_request_is_waiting_for_is_dropped() {
    let frames = reply(
        &mut state(),
        &json!({"type": "effective_settings", "id": 999, "settings": []}),
    );
    assert!(frames.is_empty(), "an uncorrelated reply answers nothing");
}

#[test]
fn get_effective_settings_requires_an_attached_session() {
    let mut unattached = BridgeState::default();
    let frame = match only(unattached.api_request_to_legacy(&request(46, "s1"))) {
        Outbound::Reply(frame) => frame,
        other => panic!("expected unattached error reply, got {other:?}"),
    };
    assert_eq!(frame.reply_to, Some(46));
    assert!(
        matches!(
            &frame.event,
            ApiEvent::Error {
                code: ErrorCode::UnknownSession,
                ..
            }
        ),
        "unattached get_effective_settings must be refused before reaching the daemon, got {:?}",
        frame.event
    );

    let mut attached_other = state();
    let frame = match only(attached_other.api_request_to_legacy(&request(47, "s2"))) {
        Outbound::Reply(frame) => frame,
        other => panic!("expected mismatched session error reply, got {other:?}"),
    };
    assert_eq!(frame.reply_to, Some(47));
    assert!(
        matches!(
            &frame.event,
            ApiEvent::Error {
                code: ErrorCode::UnknownSession,
                ..
            }
        ),
        "mismatched session_id must be refused before reaching the daemon, got {:?}",
        frame.event
    );
}
