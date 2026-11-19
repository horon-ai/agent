//! How a daemon's own end is translated for a harness API client.
//!
//! The daemon announces a reload before it replaces its socket, and the bridge
//! forwards that as `server_lifecycle` so a client can reconnect instead of
//! reporting the end of the session. Kept out of `translate_tests.rs`, which is
//! already at its size budget.
use crate::translate::BridgeState;
use horon_agent_harness_api::{ApiEvent, ServerPhase};
use serde_json::json;

#[test]
fn a_reloading_daemon_is_reported_as_restarting_and_remembered_for_the_close() {
    // The close that follows a reload is the restart, not a daemon that went
    // away, and the connection loop reads this state to decide which notice the
    // client gets.
    let mut state = BridgeState::default();
    assert!(
        !state.daemon_restarting,
        "a fresh connection has not been told anything"
    );

    let frames = state.legacy_event_to_api(&json!({"type": "reloading"}));
    assert_eq!(
        frames.iter().map(|frame| frame.event.clone()).collect::<Vec<_>>(),
        vec![ApiEvent::ServerLifecycle {
            phase: ServerPhase::Restarting,
            new_socket: None,
        }]
    );
    assert!(
        frames.iter().all(|frame| frame.reply_to.is_none()),
        "a lifecycle notice is a stream event, not an answer"
    );
    assert!(
        state.daemon_restarting,
        "the close that follows must be read as a restart"
    );
}

#[test]
fn a_reload_that_names_its_socket_passes_it_through() {
    let mut state = BridgeState::default();
    let frames = state.legacy_event_to_api(&json!({
        "type": "reloading",
        "new_socket": "/tmp/next.sock",
    }));
    assert_eq!(
        frames.iter().map(|frame| frame.event.clone()).collect::<Vec<_>>(),
        vec![ApiEvent::ServerLifecycle {
            phase: ServerPhase::Restarting,
            new_socket: Some("/tmp/next.sock".into()),
        }]
    );
}
