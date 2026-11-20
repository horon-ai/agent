//! The bridge's session flows: the rows' previews, the title a switch carries,
//! the read order that fills a preview, and the dial settings and one step that
//! move the worker to another session.
//!
//! Nothing here talks to a daemon: the parts that would are a single SDK call
//! each, and what they feed into — the strings a client shows and the retarget
//! the worker performs — is what these tests pin.

use std::time::Duration;

use horon_agent_sdk::api::ApiEvent;
use horon_agent_sdk::{ConnectOptions, HistoryMessage, SessionInfo};

use super::AttachedSession;
use super::sessions::{
    Dial, belongs, described, first_user_text, follow, history_or_empty, messages, preview, session_of,
};

/// One streamed delta of the session named, as the daemon publishes it.
fn delta(session_id: &str) -> ApiEvent {
    ApiEvent::TextDelta {
        session_id: session_id.to_string(),
        text: "tick".to_string(),
        message_id: None,
    }
}

/// A session record as the daemon serialises one.
fn record(json: &str) -> SessionInfo {
    serde_json::from_str(json).expect("a session record")
}

fn message(role: &str, content: &str) -> HistoryMessage {
    HistoryMessage {
        role: role.to_string(),
        content: content.to_string(),
        response_stats: None,
        answer: None,
    }
}

#[test]
fn a_switch_carries_the_session_title_trimmed() {
    let session = record(r#"{"session_id":"s1","status":"idle","title":"  Fix the parser  "}"#);
    let attached = described(session, Vec::new(), None, Some("high".to_string()), "/tmp/horon.sock");
    assert_eq!(attached.title.as_deref(), Some("Fix the parser"));
    assert_eq!(attached.description, "/tmp/horon.sock · s1");
    assert_eq!(attached.effort.as_deref(), Some("high"));
}

#[test]
fn a_blank_or_missing_title_is_no_title() {
    let blank = record(r#"{"session_id":"s1","status":"idle","title":"   "}"#);
    assert_eq!(described(blank, Vec::new(), None, None, "s").title, None);
    let untitled = record(r#"{"session_id":"s1","status":"idle"}"#);
    assert_eq!(described(untitled, Vec::new(), None, None, "s").title, None);
}

#[test]
fn a_row_preview_names_the_directory_the_time_and_the_last_words() {
    let session =
        record(r#"{"session_id":"s1","status":"idle","working_dir":"/tmp/project","updated_at_ms":1794809866522}"#);
    let messages = [
        message("user", "the first thing"),
        message("assistant", "an answer"),
        message("tool", "a tool row"),
        message("user", "a later question"),
    ];
    let lines = preview(&session, &messages);
    let time = super::detail(&session);
    assert_eq!(lines[0], format!("/tmp/project · {time}"));
    assert_eq!(
        lines[1..],
        [
            "you: the first thing".to_string(),
            "assistant: an answer".to_string(),
            "you: a later question".to_string(),
        ],
        "the preview lost a role, kept a tool row, or reordered the messages"
    );
}

#[test]
fn a_preview_shows_the_newest_six_messages_in_order() {
    let session = record(r#"{"session_id":"s1","status":"idle","working_dir":"/tmp"}"#);
    let messages: Vec<HistoryMessage> = (0..10)
        .map(|index| message(if index % 2 == 0 { "user" } else { "assistant" }, &format!("m{index}")))
        .collect();
    let lines = preview(&session, &messages);
    assert_eq!(
        lines.len(),
        7,
        "the preview is not the head plus six messages: {lines:?}"
    );
    assert_eq!(lines[1], "you: m4", "the oldest message of the kept tail is wrong");
    assert_eq!(lines[6], "assistant: m9");
}

#[test]
fn a_preview_line_is_cut_to_characters_not_bytes() {
    // A line of three-byte characters: a cut by bytes would split one and the
    // row would show a replacement character.
    let session = record(r#"{"session_id":"s1","status":"idle"}"#);
    let long = "가".repeat(250);
    let lines = preview(&session, &[message("user", &long)]);
    let body = &lines[1];
    let text = body.strip_prefix("you: ").expect("the role prefix");
    assert_eq!(text.chars().count(), 200, "the cut is not 200 characters");
    assert!(text.chars().all(|character| character == '가'));
}

#[test]
fn a_preview_line_keeps_one_line_however_the_message_was_stored() {
    let session = record(r#"{"session_id":"s1","status":"idle"}"#);
    let lines = preview(
        &session,
        &[message("user", "first line\n\n  second\tline\u{1b}[31mred")],
    );
    assert_eq!(lines[1], "you: first line second line [31mred");
}

#[test]
fn a_session_with_no_directory_says_so_on_the_head_line() {
    let session = record(r#"{"session_id":"s1","status":"generating"}"#);
    let lines = preview(&session, &[]);
    assert_eq!(lines, vec!["(no directory) · generating".to_string()]);
}

#[test]
fn the_title_derived_label_is_the_first_line_a_user_wrote() {
    let messages = [
        message("assistant", "the harness speaks first"),
        message("user", "\n\n  what the user asked\nand more"),
    ];
    assert_eq!(first_user_text(&messages).as_deref(), Some("what the user asked"));
    assert_eq!(first_user_text(&[message("assistant", "only this")]), None);
    assert_eq!(first_user_text(&[]), None);
}

#[test]
fn the_context_the_daemon_injects_is_neither_a_label_nor_a_preview_line() {
    // The session context and the loop guard's reminders are stored under the
    // user role, so without a filter a preview would show a system block where
    // a conversation belongs.
    let session = record(r#"{"session_id":"s1","status":"idle"}"#);
    let messages = [
        message(
            "user",
            "<system-reminder>\n# Session Context\nWorking directory: /tmp\n</system-reminder>",
        ),
        message("user", "the real question"),
        message("assistant", "the answer"),
    ];
    assert_eq!(first_user_text(&messages).as_deref(), Some("the real question"));
    let lines = preview(&session, &messages);
    assert_eq!(lines.len(), 3, "the injected context took a preview line: {lines:?}");
    assert_eq!(lines[1], "you: the real question");
    assert_eq!(lines[2], "assistant: the answer");
}

fn attached(session_id: &str) -> AttachedSession {
    AttachedSession {
        description: format!("socket · {session_id}"),
        session_id: session_id.to_string(),
        status: "idle".to_string(),
        model: None,
        effort: None,
        history: Vec::new(),
        title: None,
    }
}

#[test]
fn a_new_session_moves_to_the_stream_its_connection_subscribed_before_the_create() {
    // A create subscribes on its own new connection before the request, because
    // the daemon's catalog answer — the only `ModelInfo` the session gets — is
    // published while the create is in flight. The worker must follow exactly
    // that stream, with the id the answer named.
    let mut session_id = "old".to_string();
    let mut stream = "old-stream".to_string();

    follow(
        &attached("new"),
        "create-stream".to_string(),
        &mut session_id,
        &mut stream,
    );

    assert_eq!(stream, "create-stream", "the create's own stream was not kept");
    assert_eq!(session_id, "new");
}

#[test]
fn a_prefix_attach_follows_the_full_id_the_answer_names() {
    // The daemon rewrites a unique prefix of six or more characters to the full
    // session id, and the events carry the full one: the answer is the only
    // place the id the worker follows can come from.
    let mut session_id = "session_ant_1794".to_string();
    let mut stream = "old-stream".to_string();

    follow(
        &attached("session_ant_1794809866522"),
        "attach-stream".to_string(),
        &mut session_id,
        &mut stream,
    );

    assert_eq!(session_id, "session_ant_1794809866522");
    assert_eq!(stream, "attach-stream");
    assert!(
        belongs(&delta("session_ant_1794809866522"), &session_id),
        "the followed session's own events were dropped"
    );
    assert!(
        !belongs(&delta("session_ant_1794"), &session_id),
        "the typed prefix was treated as a session of its own"
    );
}

#[test]
fn an_event_of_another_session_is_dropped() {
    assert!(!belongs(&delta("session_other"), "session_mine"));
}

#[test]
fn an_event_of_the_current_session_is_forwarded() {
    assert!(belongs(&delta("session_mine"), "session_mine"));
}

#[test]
fn an_event_that_names_no_session_is_forwarded() {
    // An error, a pong or a session list is about the connection, so no session
    // filter can claim it.
    assert!(belongs(&ApiEvent::Pong, "session_mine"));
    assert!(belongs(&ApiEvent::Sessions { sessions: Vec::new() }, "session_mine"));
}

#[test]
fn after_a_new_session_an_event_of_the_session_that_was_left_is_dropped() {
    let mut session_id = "session_old".to_string();
    let mut stream = "old-stream".to_string();

    follow(
        &attached("session_new"),
        "new-stream".to_string(),
        &mut session_id,
        &mut stream,
    );

    assert!(!belongs(&delta("session_old"), &session_id));
    assert!(belongs(&delta("session_new"), &session_id));
}

#[test]
fn an_attached_event_names_the_session_it_carries() {
    let event = ApiEvent::Attached {
        session: record(r#"{"session_id":"session_ant","status":"idle"}"#),
    };
    assert_eq!(session_of(&event), Some("session_ant"));
}

#[test]
fn the_dial_settings_survive_the_first_connection_taking_the_options() {
    // `/new` dials a second connection, so every setting the first one used has
    // to come back: a lost `ensure_runtime` or ask flag would make the new
    // connection behave differently from the one the user is looking at. Both
    // value sets are checked, with the two support flags opposite inside each,
    // so a field the dial hard-coded — or the two flags swapped — fails one of
    // them.
    let loaded = ConnectOptions {
        socket_path: Some(std::path::PathBuf::from("/tmp/horon-tui-one.sock")),
        client_name: "horon-tui/test".to_string(),
        request_timeout: Some(Duration::from_millis(2345)),
        ensure_runtime: false,
        supports_user_response: true,
        supports_ask_requests: false,
    };
    let again = Dial::of(&loaded).options();
    assert_eq!(again.socket_path, loaded.socket_path);
    assert_eq!(again.client_name, loaded.client_name);
    assert_eq!(again.request_timeout, loaded.request_timeout);
    assert_eq!(again.ensure_runtime, loaded.ensure_runtime);
    assert_eq!(again.supports_user_response, loaded.supports_user_response);
    assert_eq!(again.supports_ask_requests, loaded.supports_ask_requests);

    let bare = ConnectOptions {
        socket_path: None,
        client_name: "horon-tui/other".to_string(),
        request_timeout: None,
        ensure_runtime: true,
        supports_user_response: false,
        supports_ask_requests: true,
    };
    let again = Dial::of(&bare).options();
    assert_eq!(again.socket_path, bare.socket_path);
    assert_eq!(again.client_name, bare.client_name);
    assert_eq!(again.request_timeout, bare.request_timeout);
    assert_eq!(again.ensure_runtime, bare.ensure_runtime);
    assert_eq!(again.supports_user_response, bare.supports_user_response);
    assert_eq!(again.supports_ask_requests, bare.supports_ask_requests);
}

#[test]
fn a_session_page_answers_the_preview_without_the_tail() {
    // A current session is a folder the tail cannot see, so the page is read
    // first and a page that answers must not cost the second request.
    let mut asked = false;
    let page: Result<Vec<HistoryMessage>, &str> = Ok(vec![message("user", "from the page")]);
    let lines = messages(page, || {
        asked = true;
        Ok::<_, &str>(vec![message("user", "from the tail")])
    });
    assert!(!asked, "the tail was read although the page answered");
    assert_eq!(lines, vec![message("user", "from the page")]);
}

#[test]
fn an_empty_page_falls_back_to_the_tail() {
    // An empty page is what a single-file record answers: its conversation is
    // only visible through the tail.
    let page: Result<Vec<HistoryMessage>, &str> = Ok(Vec::new());
    let lines = messages(page, || Ok::<_, &str>(vec![message("user", "from the tail")]));
    assert_eq!(lines, vec![message("user", "from the tail")]);
}

#[test]
fn a_failed_page_falls_back_to_the_tail() {
    // A daemon without the page capability fails the request rather than
    // answering it empty, and its sessions are still worth a preview.
    let page: Result<Vec<HistoryMessage>, &str> = Err("no session_history_cursor_v1");
    let lines = messages(page, || Ok::<_, &str>(vec![message("assistant", "from the tail")]));
    assert_eq!(lines, vec![message("assistant", "from the tail")]);
}

#[test]
fn a_session_neither_read_can_answer_has_no_preview_messages() {
    let page: Result<Vec<HistoryMessage>, &str> = Err("no session_history_cursor_v1");
    let lines = messages(page, || Err::<Vec<HistoryMessage>, &str>("no record"));
    assert!(
        lines.is_empty(),
        "a session nothing could read produced messages: {lines:?}"
    );
}

#[test]
fn an_attach_whose_history_read_failed_still_reports_the_session() {
    // The attach already moved this connection to the session, so the read's
    // failure cannot be the answer's: the worker would keep the id it left and
    // filter the new session's events away.
    let read: Result<Vec<HistoryMessage>, &str> = Err("the history page timed out");
    assert!(history_or_empty(read).is_empty(), "a failed read produced messages");
}

#[test]
fn an_attach_whose_history_read_answered_keeps_the_conversation() {
    let read: Result<Vec<HistoryMessage>, &str> = Ok(vec![message("user", "hello"), message("assistant", "hi")]);
    assert_eq!(
        history_or_empty(read),
        vec![message("user", "hello"), message("assistant", "hi")]
    );
}
