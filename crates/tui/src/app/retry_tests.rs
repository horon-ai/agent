//! What `/retry` promises: one continuation of the run that failed, and a
//! refusal that leaves nothing running.
//!
//! The eligibility decision itself lives in the daemon (`agent-runtime`'s run
//! ledger, exercised by its own tests) because only the daemon can be trusted
//! with it. What is tested here is the client side of that contract: which
//! presses reach the bridge at all, and that a refusal gives back the turn the
//! press claimed on screen.

use std::sync::mpsc;

use super::*;

fn app() -> App {
    let (_sender, receiver) = mpsc::channel();
    App::new("test".to_string(), receiver)
}

#[test]
fn an_idle_connected_client_asks_the_bridge_to_continue() {
    let mut app = app();
    let action = app.retry_turn();
    assert_eq!(action, Some(Action::Retry), "the press must reach the bridge");
    assert!(app.pending_turn, "the retry claims the turn on screen");
}

#[test]
fn a_running_turn_refuses_a_second_retry_without_sending() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(app.retry_turn(), None, "a turn in flight has nothing to continue");
    assert!(app.pending_turn, "the running turn is left alone");
}

#[test]
fn a_lost_connection_refuses_without_sending() {
    let mut app = app();
    app.disconnected = true;
    assert_eq!(app.retry_turn(), None);
    assert!(!app.pending_turn, "nothing was claimed for a request never sent");
}

#[test]
fn a_refused_retry_returns_the_client_to_idle() {
    let mut app = app();
    app.retry_turn();
    app.retry_refused("the last turn completed; nothing to continue");
    assert!(!app.pending_turn, "the claim the press made is given back");
    assert_eq!(
        app.session_status.as_deref(),
        Some("idle"),
        "a refused retry leaves no turn running"
    );
}
