//! `/new`, `/switch` and `/resume` in the client: what each command refuses,
//! and what a created session leaves behind.
//!
//! These drive a real [`App`] the way the interface does, so a pass means the
//! command reached the bridge with the right request, a refusal named the
//! command the user typed, and a new session cleared everything the one before
//! it had left on screen.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, App};
use crate::prompts::Prompt;
use crate::sdk_bridge::{AttachedSession, SessionRow};
use crate::statusline::TokenTotals;
use crate::transcript::Item;

fn app() -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    App::new("socket · session_a".to_string(), receiver)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Type a whole line and press Enter, as a user would.
fn submit(app: &mut App, line: &str) -> Option<Action> {
    app.input.set_text(line);
    app.on_key(key(KeyCode::Enter))
}

fn load_old_session_state(app: &mut App) {
    app.pending_turn = true;
    app.session_status = Some("generating".to_string());
    app.queue.push(crate::app::Message {
        text: "queued".to_string(),
        display: "queued".to_string(),
    });
    app.prompt = Some(Prompt::approval("req-1".to_string(), "bash".to_string()));
    app.resend_held = true;
    app.scroll.up(12);
    app.model = Some("old-model".to_string());
    app.provider = Some("old-provider".to_string());
    app.reasoning_effort = Some("high".to_string());
    app.tokens = Some(TokenTotals {
        input: 1,
        output: 2,
        cache_read: 0,
        cache_creation: 0,
    });
    app.transcript.push_user("the old session said this");
}

fn created(session_id: &str, title: Option<&str>) -> AttachedSession {
    AttachedSession {
        description: format!("socket · {session_id}"),
        session_id: session_id.to_string(),
        status: "idle".to_string(),
        model: None,
        effort: None,
        history: Vec::new(),
        title: title.map(str::to_string),
    }
}

#[test]
fn todo_panel_updates_and_clears_when_the_session_changes() {
    use crate::sdk_bridge::Incoming;
    use horon_agent_sdk::todo::TodoItem;

    let (sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("socket · session_a".to_string(), receiver);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    let mut screen = |app: &mut App| {
        crate::loop_io::drain_events(app);
        crate::draw(&mut terminal, app).unwrap();
        terminal.backend().to_string()
    };
    let active = TodoItem {
        content: "Check the build".into(),
        status: "in_progress".into(),
        group: Some("Release".into()),
        ..TodoItem::default()
    };
    let blocked = TodoItem {
        content: "Deploy".into(),
        status: "blocked".into(),
        blocker: Some("Waiting for approval".into()),
        ..TodoItem::default()
    };
    sender
        .send(Incoming::Todos {
            session_id: "session_a".into(),
            items: vec![active.clone(), blocked.clone()],
        })
        .unwrap();
    let initial = screen(&mut app);
    assert!(initial.contains("0/2 complete"), "{initial}");
    assert!(initial.contains("Release / Check the build"), "{initial}");
    assert!(initial.contains("Waiting for approval"), "{initial}");

    sender
        .send(Incoming::Todos {
            session_id: "session_a".into(),
            items: vec![
                TodoItem {
                    status: "completed".into(),
                    ..active
                },
                blocked.clone(),
            ],
        })
        .unwrap();
    let updated = screen(&mut app);
    assert!(updated.contains("1/2 complete"), "{updated}");
    assert!(!updated.contains("Check the build"), "{updated}");

    sender
        .send(Incoming::Attached(Box::new(created("session_b", None))))
        .unwrap();
    sender
        .send(Incoming::Todos {
            session_id: "session_a".into(),
            items: vec![blocked],
        })
        .unwrap();
    let switched = screen(&mut app);
    assert!(!switched.contains("Waiting for approval"), "{switched}");
    assert!(!switched.contains("Todos"), "{switched}");
}

/// The notices and warnings on screen, from the newest one back.
fn said(app: &App) -> Vec<String> {
    app.transcript
        .items()
        .iter()
        .map(|item| match item {
            Item::Notice(text) | Item::Warning(text) => text.clone(),
            Item::User(text) => format!("user:{text}"),
            other => format!("other:{other:?}"),
        })
        .collect()
}

#[test]
fn new_asks_the_bridge_for_a_session_and_claims_nothing_yet() {
    let mut app = app();
    app.cwd = Some("/tmp/project".to_string());
    assert_eq!(submit(&mut app, "/new"), Some(Action::NewSession));
    assert!(
        app.take_pending_new_session(),
        "the answer could not tell a created session from a resumed one"
    );
    assert!(
        app.transcript.items().is_empty(),
        "the client claimed a session before the daemon answered: {:?}",
        said(&app)
    );
}

#[test]
fn new_is_refused_while_a_turn_runs() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(submit(&mut app, "/new"), None);
    assert!(
        !app.take_pending_new_session(),
        "a refused command still claimed a session"
    );
    assert_eq!(
        said(&app),
        vec!["the turn is still running; /new works once it ends".to_string()],
        "the refusal did not say which command waits"
    );
}

#[test]
fn new_is_refused_on_a_lost_connection() {
    let mut app = app();
    app.disconnect("the harness connection closed");
    let before = app.transcript.items().len();
    assert_eq!(submit(&mut app, "/new"), None);
    assert!(!app.take_pending_new_session());
    match app.transcript.items().last() {
        Some(Item::Warning(text)) => {
            assert!(text.contains("no session to change to"), "the warning was {text}");
        }
        other => panic!("the refusal was not a warning: {other:?}"),
    }
    assert_eq!(app.transcript.items().len(), before + 1);
}

#[test]
fn switch_opens_the_list_under_its_own_title_and_resume_under_its_own() {
    let mut app = app();
    assert_eq!(submit(&mut app, "/switch"), Some(Action::Sessions));
    assert_eq!(
        app.picker.as_ref().map(|picker| picker.title()),
        Some("Switch session"),
        "the /switch list does not say which command opened it"
    );

    app.picker = None;
    assert_eq!(submit(&mut app, "/resume"), Some(Action::Sessions));
    assert_eq!(app.picker.as_ref().map(|picker| picker.title()), Some("Resume session"));
}

#[test]
fn switch_and_resume_attach_directly_to_the_id_typed() {
    let mut app = app();
    assert_eq!(
        submit(&mut app, "/switch session_b"),
        Some(Action::Attach("session_b".to_string()))
    );
    assert!(app.picker.is_none(), "a named session opened the list");
    assert_eq!(
        submit(&mut app, "/resume session_c"),
        Some(Action::Attach("session_c".to_string()))
    );
    // Spaces around the id are the draft's, not the session's.
    assert_eq!(
        submit(&mut app, "/switch   session_d  "),
        Some(Action::Attach("session_d".to_string()))
    );
    // An argument of nothing but spaces is no argument.
    assert_eq!(submit(&mut app, "/switch    "), Some(Action::Sessions));
}

#[test]
fn switch_and_resume_are_refused_while_a_turn_runs() {
    let mut app = app();
    app.pending_turn = true;
    assert_eq!(submit(&mut app, "/switch"), None);
    assert!(app.picker.is_none(), "the list opened during a turn");
    assert_eq!(
        said(&app),
        vec!["the turn is still running; /switch works once it ends".to_string()]
    );

    app.transcript.clear();
    assert_eq!(submit(&mut app, "/resume session_b"), None);
    assert_eq!(
        said(&app),
        vec!["the turn is still running; /resume works once it ends".to_string()]
    );
}

#[test]
fn a_created_session_clears_everything_the_old_one_left() {
    let mut app = app();
    // Asked for while the client is idle, which is the only state that allows
    // it; the turn below then loads the state the answer has to clear.
    assert_eq!(app.start_new_session(), Some(Action::NewSession));
    load_old_session_state(&mut app);
    app.session_title = Some("the old title".to_string());

    let started = app.take_pending_new_session();
    assert!(started, "the create did not ask for a started notice");

    app.attach_session(created("session_new", Some("a fresh start")), started);

    assert!(!app.pending_turn, "the old turn survived");
    assert_eq!(app.session_status.as_deref(), Some("idle"));
    assert!(app.queue.is_empty(), "the old queue survived");
    assert!(app.prompt.is_none(), "a prompt of the old session survived");
    assert!(app.waiting_prompts.is_empty());
    assert!(!app.resend_held);
    assert!(app.scroll.is_following(), "the view stayed held above the new session");
    assert_eq!(app.session_id.as_deref(), Some("session_new"));
    assert_eq!(app.description, "socket · session_new");
    assert_eq!(app.session_title.as_deref(), Some("a fresh start"));
    assert!(app.model.is_none(), "the old model survived");
    assert!(app.provider.is_none(), "the old provider survived");
    assert!(app.reasoning_effort.is_none(), "the old effort survived");
    assert!(app.tokens.is_none(), "the old token counts survived");
    assert_eq!(
        said(&app),
        vec!["started a new session session_new".to_string()],
        "a new session was reported as a stored one, or said it had no messages"
    );
}

#[test]
fn a_new_session_with_no_title_does_not_keep_the_old_title() {
    let mut app = app();
    app.session_title = Some("the old title".to_string());
    app.attach_session(created("session_new", None), true);
    assert_eq!(app.session_title, None, "the window kept naming the session just left");
}

#[test]
fn a_resumed_session_still_says_it_had_nothing_stored() {
    let mut app = app();
    app.attach_session(created("session_old", None), false);
    assert_eq!(
        said(&app),
        vec![
            "the stored session has no messages".to_string(),
            "resumed session_old".to_string(),
        ]
    );
}

#[test]
fn a_failed_create_does_not_make_the_next_attach_claim_a_started_session() {
    let mut app = app();
    app.start_new_session();
    app.request_failed("could not start a new session: the harness refused the request");
    assert!(
        matches!(app.transcript.items().last(), Some(Item::Warning(text)) if text.contains("could not start a new session")),
        "the failure was not reported: {:?}",
        said(&app)
    );
    assert!(!app.take_pending_new_session(), "the failed create was still pending");

    let started = app.take_pending_new_session();
    assert!(!started);
    app.attach_session(created("session_b", None), started);
    assert_eq!(
        said(&app),
        vec![
            "the stored session has no messages".to_string(),
            "resumed session_b".to_string(),
        ]
    );
}

#[test]
fn a_message_a_session_said_finds_that_row_and_shows_it_in_the_preview() {
    let mut app = app();
    submit(&mut app, "/switch");
    app.show_sessions(vec![
        SessionRow {
            id: "session_quiet".to_string(),
            label: "a quiet session".to_string(),
            detail: "11-16 09:41".to_string(),
            current: false,
            preview: vec![
                "/one/project · 11-16 09:41".to_string(),
                "you: something else entirely".to_string(),
            ],
        },
        SessionRow {
            id: "session_target".to_string(),
            label: "a shared title".to_string(),
            detail: "11-16 10:02".to_string(),
            current: true,
            preview: vec![
                "/two/project · 11-16 10:02".to_string(),
                "you: hello-1794".to_string(),
                "assistant: hello back".to_string(),
            ],
        },
    ]);

    // The title alone would have selected the other row: only the preview says
    // which session this is.
    for character in "hello-1794".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    let picker = app.picker.as_ref().expect("the list went away");
    assert_eq!(picker.len(), 1, "the filter left {} rows", picker.len());
    let row = picker.selected_row().expect("no row is selected");
    assert_eq!(row.value, "session_target");
    assert_eq!(row.preview[1], "you: hello-1794");

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Action::Attach("session_target".to_string()))
    );
}

#[test]
fn the_directory_every_session_preview_starts_with_does_not_match_the_filter() {
    // The head line is the directory and the time, and every row of one
    // directory carries it: searching it would make a filter on the path keep
    // the whole list and hide the row the user is looking for.
    let mut app = app();
    submit(&mut app, "/switch");
    app.show_sessions(vec![
        SessionRow {
            id: "session_one".to_string(),
            label: "a quiet session".to_string(),
            detail: "11-16 09:41".to_string(),
            current: false,
            preview: vec![
                "/work/project · 11-16 09:41".to_string(),
                "you: something else".to_string(),
            ],
        },
        SessionRow {
            id: "session_two".to_string(),
            label: "another session".to_string(),
            detail: "11-16 10:02".to_string(),
            current: false,
            preview: vec![
                "/work/project · 11-16 10:02".to_string(),
                "assistant: a reply".to_string(),
            ],
        },
    ]);

    for character in "work".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    let picker = app.picker.as_ref().expect("the list went away");
    assert!(
        picker.is_empty(),
        "the shared directory matched {} row(s) although no label or detail holds it",
        picker.len()
    );
}
