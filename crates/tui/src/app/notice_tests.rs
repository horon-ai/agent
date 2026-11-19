//! What the client says when the harness behind it restarts, stops or goes
//! away.
//!
//! These drive a real [`App`] with the deliveries the bridge sends, through the
//! same `drain_events` the frame loop uses, so a pass means the transcript ends
//! up with the entry the user reads and the connection state that follows from
//! it. The sentences are the operator's own; they are asserted here so a later
//! wording change is a decision rather than an accident.

use std::sync::mpsc::{Receiver, Sender, channel};

use horon_agent_sdk::api::{ApiEvent, ServerPhase};

use crate::app::App;
use crate::item_lines;
use crate::loop_io::drain_events;
use crate::sdk_bridge::Incoming;
use crate::theme::{Symbols, Theme};
use crate::transcript::Item;
use crate::wrap;

fn connected() -> (App, Sender<Incoming>) {
    let (sender, receiver): (Sender<Incoming>, Receiver<Incoming>) = channel();
    (App::new("socket · session_a".to_string(), receiver), sender)
}

/// Every announcement on screen, in the order it was drawn.
fn announcements(app: &App) -> Vec<&str> {
    app.transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Announcement(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn deliver(app: &mut App, sender: &Sender<Incoming>, delivery: Incoming) {
    sender.send(delivery).expect("the app is listening");
    drain_events(app);
}

fn lifecycle(phase: ServerPhase) -> Incoming {
    Incoming::Event(Box::new(ApiEvent::ServerLifecycle {
        phase,
        new_socket: None,
    }))
}

#[test]
fn a_restart_is_announced_and_then_reported_as_finished() {
    let (mut app, bridge) = connected();

    deliver(&mut app, &bridge, lifecycle(ServerPhase::Restarting));
    assert_eq!(announcements(&app), vec!["서버를 재시작하는 중입니다"]);
    assert!(!app.disconnected, "a restart is not the end of the session");

    deliver(&mut app, &bridge, Incoming::ServerRestarted);
    assert_eq!(
        announcements(&app),
        vec!["서버를 재시작하는 중입니다", "서버가 재시작되었습니다"]
    );
    assert!(!app.disconnected);
    assert!(app.banner.is_none(), "coming back is not a failure: {:?}", app.banner);
}

#[test]
fn a_restart_that_never_comes_back_is_reported_as_the_server_stopping() {
    let (mut app, bridge) = connected();

    deliver(&mut app, &bridge, lifecycle(ServerPhase::Restarting));
    deliver(
        &mut app,
        &bridge,
        Incoming::Closed("the harness did not come back".to_string()),
    );

    assert_eq!(
        announcements(&app),
        vec!["서버를 재시작하는 중입니다", "서버가 종료되었습니다"]
    );
    assert!(app.disconnected);
    assert_eq!(app.banner.as_deref(), Some("the harness did not come back"));
}

#[test]
fn a_daemon_that_stopped_without_announcing_a_restart_says_it_stopped() {
    let (mut app, bridge) = connected();

    deliver(&mut app, &bridge, lifecycle(ServerPhase::Stopped));
    // The bridge still sends the close notice it always did; the announcement
    // is what the user reads, so the English sentence must not be drawn.
    deliver(
        &mut app,
        &bridge,
        Incoming::Event(Box::new(ApiEvent::Error {
            code: horon_agent_sdk::api::ErrorCode::Internal,
            message: "daemon connection closed".to_string(),
            details: None,
        })),
    );
    deliver(
        &mut app,
        &bridge,
        Incoming::Closed("daemon connection closed".to_string()),
    );

    assert_eq!(announcements(&app), vec!["서버가 종료되었습니다"]);
    assert!(
        !has_warning(&app, "daemon connection closed"),
        "the close was drawn twice"
    );
    assert!(app.disconnected);
}

#[test]
fn a_connection_that_ends_without_a_word_says_the_connection_was_lost() {
    let (mut app, bridge) = connected();

    deliver(
        &mut app,
        &bridge,
        Incoming::Closed("the harness bridge stopped".to_string()),
    );

    assert_eq!(announcements(&app), vec!["서버 연결이 끊어졌습니다"]);
    assert!(app.disconnected);
    assert_eq!(app.banner.as_deref(), Some("the harness bridge stopped"));
}

#[test]
fn an_announcement_is_drawn_as_its_header_then_the_sentence() {
    let theme = Theme::default();
    let look = item_lines::Look {
        theme: &theme,
        symbols: Symbols::default(),
        width: 60,
        markdown: true,
    };
    let rows: Vec<String> = wrap::wrap_rows(
        item_lines::lines(&Item::Announcement("서버가 재시작되었습니다".to_string()), &look, None),
        60,
    )
    .iter()
    .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
    .collect();

    assert_eq!(rows, vec!["[알림]".to_string(), "서버가 재시작되었습니다".to_string()]);
}

fn has_warning(app: &App, text: &str) -> bool {
    app.transcript
        .items()
        .iter()
        .any(|item| matches!(item, Item::Warning(warning) if warning == text))
}
