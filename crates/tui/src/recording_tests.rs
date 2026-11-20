//! Recording and replay: what a recording keeps, what it refuses to keep, and
//! how a replay moves through one.
//!
//! Every test drives the app the way a user does — a key, an event, a slash
//! command — because the boundary that matters is what the whole client writes
//! and shows, not what one helper serialises.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::ApiEvent;

use crate::app::{Action, App};
use crate::recording::{Event, Outcome, Recorder, Recording};
use crate::sdk_bridge::Incoming;
use crate::transcript::Item;

fn app() -> App {
    let (_sender, receiver) = mpsc::channel::<Incoming>();
    App::new("socket · session".to_string(), receiver)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Type a whole line and press Enter, as a user would.
fn submit(app: &mut App, line: &str) -> Option<Action> {
    app.input.set_text(line);
    app.on_key(key(KeyCode::Enter))
}

fn text_delta(text: &str) -> ApiEvent {
    ApiEvent::TextDelta {
        session_id: "s1".into(),
        text: text.into(),
        message_id: Some("m1".into()),
    }
}

fn text_done() -> ApiEvent {
    ApiEvent::TextDone {
        session_id: "s1".into(),
        message_id: Some("m1".into()),
    }
}

fn tool_start(call_id: &str, name: &str) -> ApiEvent {
    ApiEvent::ToolStart {
        session_id: "s1".into(),
        call_id: call_id.into(),
        name: name.into(),
    }
}

fn tool_done(call_id: &str, name: &str, output: &str, error: Option<&str>) -> ApiEvent {
    ApiEvent::ToolDone {
        session_id: "s1".into(),
        call_id: call_id.into(),
        name: name.into(),
        output: output.into(),
        error: error.map(str::to_string),
        verdict: None,
    }
}

fn approval_pending(request: &str) -> ApiEvent {
    ApiEvent::ApprovalPending {
        session_id: "s1".into(),
        request_id: request.into(),
        action: "bash".into(),
        expires_at: None,
    }
}

/// A recording file written by hand, so a test controls every timestamp.
fn fixture(path: &Path, lines: &[&str]) -> PathBuf {
    let mut text = String::from(r#"{"format":"horon-tui-recording","version":1}"#);
    text.push('\n');
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(path, text).expect("write fixture");
    path.to_path_buf()
}

/// The items a transcript holds, as short words a test can compare.
fn items(app: &App) -> Vec<String> {
    app.transcript
        .items()
        .iter()
        .map(|item| match item {
            Item::User(text) => format!("user:{text}"),
            Item::Assistant(text) => format!("assistant:{text}"),
            Item::Tool(call) => format!("tool:{}:{}", call.name, call.state.label()),
            Item::Warning(text) => format!("warning:{text}"),
            Item::Notice(text) => format!("notice:{text}"),
            other => format!("other:{other:?}"),
        })
        .collect()
}

#[test]
fn a_recording_keeps_sent_messages_answers_and_tool_outcomes_in_order() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("session.jsonl");
    let mut app = app();
    app.start_recording(path.clone());

    submit(&mut app, "hello");
    app.apply_event(&text_delta("hi there"));
    app.apply_event(&text_done());
    app.apply_event(&tool_start("c1", "bash"));
    app.apply_event(&tool_done("c1", "bash", "the whole output body", None));
    app.apply_event(&tool_start("c2", "read"));
    app.apply_event(&tool_done("c2", "read", "", Some("the failure body")));
    app.stop_recording();

    let recording = Recording::load(&path).expect("the recording reads back");
    let events: Vec<Event> = recording.events.into_iter().map(|timed| timed.event).collect();
    assert_eq!(
        events,
        vec![
            Event::User("hello".to_string()),
            Event::Assistant("hi there".to_string()),
            Event::Tool {
                name: "bash".to_string(),
                outcome: Outcome::Done
            },
            Event::Tool {
                name: "read".to_string(),
                outcome: Outcome::Failed
            },
        ]
    );
    let text = std::fs::read_to_string(&path).expect("read the recording");
    assert!(
        !text.contains("the whole output body") && !text.contains("the failure body"),
        "a tool body reached the recording:\n{text}"
    );
}

#[test]
fn answers_published_before_the_recording_are_not_recorded() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("late.jsonl");
    let mut app = app();
    // A conversation that already happened is not this recording's content.
    app.apply_event(&text_delta("an earlier answer"));
    app.apply_event(&text_done());
    app.start_recording(path.clone());

    submit(&mut app, "hello");
    app.apply_event(&text_delta("a later answer"));
    app.apply_event(&text_done());
    app.stop_recording();

    let recording = Recording::load(&path).expect("the recording reads back");
    let events: Vec<Event> = recording.events.into_iter().map(|timed| timed.event).collect();
    assert_eq!(
        events,
        vec![
            Event::User("hello".to_string()),
            Event::Assistant("a later answer".to_string()),
        ]
    );
}

#[test]
fn an_unsent_draft_is_never_recorded() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("draft.jsonl");
    let mut app = app();
    app.start_recording(path.clone());

    for letter in "a draft that was never sent".chars() {
        app.on_key(key(KeyCode::Char(letter)));
    }
    app.stop_recording();

    let recording = Recording::load(&path).expect("the recording reads back");
    assert!(
        recording.events.is_empty(),
        "a draft reached the recording: {:?}",
        recording.events
    );
}

#[test]
fn an_approval_and_its_arguments_stay_out_of_the_recording() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("approval.jsonl");
    let mut app = app();
    app.start_recording(path.clone());

    // The line the gate refused, with the request id and the arguments a prompt
    // would draw beside it.
    app.apply_event(&tool_start("c1", "bash"));
    app.apply_event(&ApiEvent::ToolInputDelta {
        session_id: "s1".into(),
        call_id: "c1".into(),
        delta: r#"{"command":"secret-command"}"#.into(),
    });
    app.apply_event(&tool_done(
        "c1",
        "bash",
        "",
        Some("bash needs approval before it runs. Request secret-request is queued; answer it through the approval surface. Nothing was executed."),
    ));
    app.apply_event(&approval_pending("secret-request"));
    // Answer it the way a user would.
    app.on_key(key(KeyCode::Char('y')));
    app.stop_recording();

    let text = std::fs::read_to_string(&path).expect("read the recording");
    assert!(
        !text.contains("secret-command") && !text.contains("secret-request"),
        "an approval secret reached the recording:\n{text}"
    );
    let recording = Recording::load(&path).expect("the recording reads back");
    assert!(
        recording.events.is_empty(),
        "an approval left an event behind: {:?}",
        recording.events
    );
}

#[test]
fn a_recording_stops_at_its_byte_limit() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("full.jsonl");
    let mut recorder = Recorder::create(&path, 200).expect("recording");
    // The first line fits; the next one cannot, and neither can any later one.
    assert!(
        recorder.push(&Event::User("one".to_string())).is_ok(),
        "the first event was refused"
    );
    let error = recorder
        .push(&Event::User("x".repeat(400)))
        .expect_err("an oversized event was written");
    assert!(matches!(error, crate::recording::RecordError::Full(_)));
    let events = recorder.events();
    assert_eq!(events, 1, "the refused event was counted");
    let written = std::fs::metadata(&path).expect("metadata").len();
    assert!(written <= 200, "the file grew past its limit: {written} bytes");
}

#[test]
fn a_warning_is_recorded_without_its_text() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("warning.jsonl");
    let mut app = app();
    app.start_recording(path.clone());
    app.warn("the file is at /Users/private/.horon/notes.txt");
    app.stop_recording();

    let text = std::fs::read_to_string(&path).expect("read the recording");
    assert!(
        !text.contains("/Users/private"),
        "a warning path reached the recording:\n{text}"
    );
    let recording = Recording::load(&path).expect("the recording reads back");
    let events: Vec<Event> = recording.events.into_iter().map(|timed| timed.event).collect();
    assert_eq!(events, vec![Event::Warning]);

    app.run_replay(path.to_str().expect("path"));
    assert!(
        items(&app)
            .iter()
            .any(|item| item == "warning:a warning was shown; its text was not recorded"),
        "the replay did not say the warning text was left out: {:?}",
        items(&app)
    );
}

#[test]
fn an_answer_with_control_sequences_is_recorded_as_plain_text() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("escape.jsonl");
    let mut app = app();
    app.start_recording(path.clone());
    // A model can emit a terminal escape; the recording keeps the text, not the
    // control sequence that was never part of the words.
    app.apply_event(&text_delta("plain \u{1b}[31mred\u{1b}[0m"));
    app.apply_event(&text_done());
    app.stop_recording();

    let text = std::fs::read_to_string(&path).expect("read the recording");
    assert!(!text.contains("[31m"), "a control sequence reached the file:\n{text}");
    let recording = Recording::load(&path).expect("the recording reads back");
    let events: Vec<Event> = recording.events.into_iter().map(|timed| timed.event).collect();
    assert_eq!(events, vec![Event::Assistant("plain red".to_string())]);
}

#[test]
fn a_replay_plays_in_order_and_a_pause_holds_it() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = fixture(
        &directory.path().join("play.jsonl"),
        &[
            r#"{"at":0,"kind":"user","text":"one"}"#,
            r#"{"at":1000,"kind":"assistant","text":"two"}"#,
            r#"{"at":2000,"kind":"tool","name":"bash","outcome":"done"}"#,
        ],
    );
    let mut app = app();
    app.transcript.push_user("the live line");
    app.run_replay(path.to_str().expect("path"));

    assert!(app.replaying(), "the replay did not start");
    assert!(
        items(&app).iter().any(|item| item == "user:one"),
        "the event at zero was not drawn: {:?}",
        items(&app)
    );
    assert!(!items(&app).iter().any(|item| item == "assistant:two"));

    assert!(app.advance_replay(1000));
    assert!(items(&app).iter().any(|item| item == "assistant:two"));
    assert!(!items(&app).iter().any(|item| item == "tool:bash:done"));

    app.toggle_replay_pause();
    assert!(!app.advance_replay(5000), "a paused replay moved");
    assert!(!items(&app).iter().any(|item| item == "tool:bash:done"));

    app.toggle_replay_pause();
    app.advance_replay(1000);
    let rows = items(&app);
    let answer = rows
        .iter()
        .position(|item| item == "assistant:two")
        .expect("the answer row is on screen");
    let tool = rows
        .iter()
        .position(|item| item == "tool:bash:done")
        .expect("the tool row is on screen");
    assert!(answer < tool, "the replay drew its events out of order: {rows:?}");
}

#[test]
fn seeking_back_rebuilds_the_rows_at_the_target() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = fixture(
        &directory.path().join("seek.jsonl"),
        &[
            r#"{"at":0,"kind":"user","text":"one"}"#,
            r#"{"at":1000,"kind":"assistant","text":"two"}"#,
            r#"{"at":2000,"kind":"tool","name":"bash","outcome":"done"}"#,
        ],
    );
    let mut app = app();
    app.run_replay(path.to_str().expect("path"));
    app.advance_replay(5000);
    assert!(items(&app).iter().any(|item| item == "tool:bash:done"));

    app.seek_replay_to(1000);
    let rows = items(&app);
    assert!(
        rows.iter().any(|item| item == "assistant:two"),
        "the seek lost the events before it: {rows:?}"
    );
    assert!(
        !rows.iter().any(|item| item == "tool:bash:done"),
        "a row from after the seek survived it: {rows:?}"
    );
    assert!(
        rows.iter().any(|item| item.starts_with("notice:replay seeked to")),
        "the seek said nothing: {rows:?}"
    );
}

#[test]
fn a_file_that_is_not_a_recording_is_refused() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("other.jsonl");
    std::fs::write(&path, "{\"hello\":\"world\"}\n").expect("write");
    let mut app = app();
    app.run_replay(path.to_str().expect("path"));
    assert!(!app.replaying(), "a foreign file started a replay");
    assert!(
        items(&app).iter().any(|item| item.starts_with("warning:replay:")),
        "the refusal was silent: {:?}",
        items(&app)
    );
}

#[test]
fn an_event_after_the_one_it_names_is_rejected() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = fixture(
        &directory.path().join("order.jsonl"),
        &[
            r#"{"at":500,"kind":"user","text":"one"}"#,
            r#"{"at":100,"kind":"assistant","text":"two"}"#,
        ],
    );
    let error = Recording::load(&path).expect_err("an out-of-order file loaded");
    let shown = error.to_string();
    assert!(shown.contains("line 3"), "unexpected error: {shown}");
}

#[test]
fn escaping_a_replay_returns_the_live_conversation() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = fixture(
        &directory.path().join("stop.jsonl"),
        &[r#"{"at":0,"kind":"user","text":"recorded"}"#],
    );
    let mut app = app();
    app.transcript.push_user("the live line");
    let live: Vec<Item> = app.transcript.items().to_vec();
    app.run_replay(path.to_str().expect("path"));

    app.on_key(key(KeyCode::Esc));
    assert!(!app.replaying(), "esc left the replay running");
    assert_eq!(
        &app.transcript.items()[..live.len()],
        live.as_slice(),
        "the live conversation did not come back"
    );
    assert!(
        items(&app)
            .iter()
            .any(|item| item.starts_with("notice:replay stopped at")),
        "the replay did not say it stopped: {:?}",
        items(&app)
    );
}

#[test]
fn live_activity_ends_a_replay() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = fixture(
        &directory.path().join("live.jsonl"),
        &[r#"{"at":0,"kind":"user","text":"recorded"}"#],
    );
    let mut app = app();
    app.run_replay(path.to_str().expect("path"));
    assert!(app.replaying());

    app.apply_event(&text_delta("a live answer"));
    assert!(!app.replaying(), "a live event stayed inside the replay");
    assert!(
        app.transcript.open_text().is_some_and(|text| text == "a live answer"),
        "the live event did not reach the live conversation"
    );
}
