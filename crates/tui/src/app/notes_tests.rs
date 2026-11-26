//! `/note` through a real app: the command, the screen and the file.
//!
//! Every test builds its own `$HORON_HOME`-free notes file, attaches sessions
//! with explicit histories, and draws into a `TestBackend`, so the note is
//! checked where the reader sees it rather than only in the store.

use std::path::PathBuf;

use horon_agent_sdk::HistoryMessage;

use super::*;
use crate::notes;
use crate::notes::NoteStore;
use crate::sdk_bridge::AttachedSession;
use crate::transcript::Item;

/// An app whose notes live in `path` rather than in the user's home.
fn app_at(path: PathBuf) -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("socket · session_a".to_string(), receiver);
    app.notes = NoteStore::at(path);
    app.load_notes();
    app
}

fn message(role: &str, content: &str) -> HistoryMessage {
    HistoryMessage {
        response_stats: None,
        role: role.to_string(),
        content: content.to_string(),
        answer: None,
        message_id: None,
        tool: None,
    }
}

fn attached(id: &str, title: Option<&str>, rows: &[(&str, &str)]) -> AttachedSession {
    AttachedSession {
        description: format!("socket · {id}"),
        session_id: id.to_string(),
        status: "idle".to_string(),
        model: None,
        effort: None,
        history: rows.iter().map(|(role, content)| message(role, content)).collect(),
        title: title.map(str::to_string),
    }
}

fn screen(app: &mut App) -> String {
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = crate::draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    format!("{}", terminal.backend())
}

fn last_text(app: &App) -> String {
    match app.transcript.items().last() {
        Some(Item::Notice(text)) | Some(Item::Warning(text)) => text.clone(),
        other => panic!("expected a notice or a warning, got {other:?}"),
    }
}

fn note_texts(app: &App, session: &str) -> Vec<String> {
    app.notes.notes(session).map(|(_, note)| note.text.clone()).collect()
}

/// A conversation with one user message and one answer.
fn simple(id: &str) -> AttachedSession {
    attached(id, None, &[("user", "hello"), ("assistant", "the answer")])
}

#[test]
fn a_note_is_drawn_under_its_message_and_comes_back_after_reattaching() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = directory.path().join("tui-notes.json");
    let mut app = app_at(path.clone());
    app.attach_session(simple("session_a"), false);

    assert_eq!(app.submit("/note add a1 the return value is missing"), None);
    let drawn = screen(&mut app);
    assert!(
        drawn.contains("note: the return value is missing"),
        "the note is not on screen:\n{drawn}"
    );

    let mut reattached = app_at(path);
    reattached.attach_session(simple("session_a"), false);
    let drawn = screen(&mut reattached);
    assert!(
        drawn.contains("note: the return value is missing"),
        "the note did not come back after a reattach:\n{drawn}"
    );
}

#[test]
fn a_note_never_moves_to_a_message_with_different_text() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    app.attach_session(
        attached(
            "session_a",
            None,
            &[("user", "hello"), ("assistant", "the first answer")],
        ),
        false,
    );
    app.submit("/note add a1 review this answer");

    // The same session keeps its id but the answer at a1 is another text, as
    // after a compaction or a retracted turn. The note must not follow the
    // address onto it.
    app.attach_session(
        attached(
            "session_a",
            None,
            &[("user", "hello"), ("assistant", "a different answer")],
        ),
        false,
    );
    let drawn = screen(&mut app);
    assert!(
        !drawn.contains("review this answer"),
        "the note moved to a different message:\n{drawn}"
    );
    // The note is still listed as stale, and the stored address reaches it.
    assert_eq!(app.submit("/note"), None);
    assert!(last_text(&app).contains("the first answer") || last_text(&app).contains("not in this transcript"));
    assert_eq!(app.submit("/note remove a1"), None);
    assert!(last_text(&app).contains("note removed from a1"), "{}", last_text(&app));
    assert!(note_texts(&app, "session_a").is_empty());
}

#[test]
fn a_stale_address_does_not_take_another_messages_note() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    let mut first = attached(
        "session_a",
        None,
        &[("user", "hello"), ("assistant", "the first answer")],
    );
    first.history[1].message_id = Some("m1".to_string());
    app.attach_session(first, false);
    app.submit("/note add a1 review this answer");

    // The answer at a1 is another stored message now, and the one the note was
    // written on is still here at a2. Removing a1 must not take that note: it
    // is drawn under a2, and a1's own note is a different one.
    let mut moved = attached(
        "session_a",
        None,
        &[
            ("user", "hello"),
            ("assistant", "a different answer"),
            ("assistant", "the first answer"),
        ],
    );
    moved.history[1].message_id = Some("m2".to_string());
    moved.history[2].message_id = Some("m1".to_string());
    app.attach_session(moved, false);

    assert_eq!(app.submit("/note remove a1"), None);
    assert!(last_text(&app).contains("has no note to remove"), "{}", last_text(&app));
    assert_eq!(note_texts(&app, "session_a"), vec!["review this answer".to_string()]);

    // The address that still holds the note removes it.
    assert_eq!(app.submit("/note remove a2"), None);
    assert!(last_text(&app).contains("note removed from a2"), "{}", last_text(&app));
    assert!(note_texts(&app, "session_a").is_empty());
}

#[test]
fn a_note_survives_a_replay_that_carries_reasoning_in_the_same_body() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    app.attach_session(simple("session_a"), false);
    app.submit("/note add a1 the answer note");

    // A stored row flattens its typed blocks by joining every block's text, so
    // reasoning and answer can be one unmarked string (`flatten_content`).
    // The history reply names the answer separately; that is what the client
    // reads, while `content` keeps its historical value for other clients.
    let mut replayed = attached(
        "session_a",
        None,
        &[("user", "hello"), ("assistant", "fixture thinking 한글\nthe answer")],
    );
    replayed.history[1].answer = Some("the answer".to_string());
    app.transcript.set_show_reasoning(true);
    app.attach_session(replayed, false);

    let drawn = screen(&mut app);
    assert!(
        drawn.contains("note: the answer note"),
        "the note was lost when the replay carried reasoning:\n{drawn}"
    );
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Assistant(text) if text == "the answer")),
        "the answer row carries the reasoning"
    );
}

#[test]
fn a_repeated_text_keeps_its_occurrence_across_a_reattach() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    let twice = [
        ("user", "hello"),
        ("assistant", "same answer"),
        ("assistant", "same answer"),
    ];
    app.attach_session(attached("session_a", None, &twice), false);
    assert_eq!(app.submit("/note add a2 the second one"), None);
    assert!(screen(&mut app).contains("note: the second one"));

    app.attach_session(attached("session_a", None, &twice), false);
    assert!(screen(&mut app).contains("note: the second one"));

    // With one copy gone, the remaining row is occurrence one; the note for
    // occurrence two is stale rather than moved onto it.
    let once = [("user", "hello"), ("assistant", "same answer")];
    app.attach_session(attached("session_a", None, &once), false);
    assert!(!screen(&mut app).contains("note: the second one"));
}

#[test]
fn a_note_comes_back_when_its_message_is_sent_from_the_queue() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    let both = [
        ("user", "hello"),
        ("assistant", "first"),
        ("user", "hello"),
        ("assistant", "second"),
    ];
    app.attach_session(attached("session_a", None, &both), false);
    assert_eq!(app.submit("/note add u2 the repeat"), None);

    // The second "hello" is gone, then typed again: queued, it is not a sent
    // message and holds no note; once sent it is the second "hello" again.
    let first = [("user", "hello"), ("assistant", "first")];
    app.attach_session(attached("session_a", None, &first), false);
    app.transcript.push_queued("hello");
    assert!(!screen(&mut app).contains("note: the repeat"));
    app.transcript.promote_queued("hello");
    let drawn = screen(&mut app);
    assert!(
        drawn.contains("note: the repeat"),
        "the sent message did not get its note back:\n{drawn}"
    );

    // Retracting that message drops the row via `remove_item`; its note must
    // leave the screen on the next frame instead of surviving in a stale cache.
    app.transcript.retract_user("hello");
    assert!(
        !screen(&mut app).contains("note: the repeat"),
        "a retracted message kept its note on screen"
    );
}

#[test]
fn notes_of_sessions_sharing_a_title_stay_with_their_session() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = directory.path().join("tui-notes.json");
    let mut app = app_at(path);
    let tied = |id: &str| {
        attached(
            id,
            Some("same title"),
            &[("user", "hello"), ("assistant", "the answer")],
        )
    };
    app.attach_session(tied("session_a"), false);
    app.submit("/note add a1 belongs to alpha");

    app.attach_session(tied("session_b"), false);
    let drawn = screen(&mut app);
    assert!(
        !drawn.contains("belongs to alpha"),
        "another session's note was drawn:\n{drawn}"
    );
    assert!(note_texts(&app, "session_b").is_empty());

    app.attach_session(tied("session_a"), false);
    let drawn = screen(&mut app);
    assert!(
        drawn.contains("belongs to alpha"),
        "the note was lost on the way back:\n{drawn}"
    );
}

#[test]
fn an_address_this_session_does_not_hold_is_refused() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    app.attach_session(simple("session_a"), false);

    assert_eq!(app.submit("/note add a9 wrong row"), None);
    assert!(last_text(&app).contains("no message a9"), "{}", last_text(&app));
    assert_eq!(app.notes.count("session_a"), 0, "a refused address stored a note");

    // An address-shaped word that is not one is refused rather than read as
    // the note's text.
    assert_eq!(app.submit("/note add u0 zero"), None);
    assert!(last_text(&app).contains("not a message address"), "{}", last_text(&app));
    assert_eq!(app.notes.count("session_a"), 0);

    // An address with no text after it is a note with no text, not a note
    // whose text is the address.
    assert_eq!(app.submit("/note add a1"), None);
    assert!(last_text(&app).contains("has no text"), "{}", last_text(&app));
    assert_eq!(app.notes.count("session_a"), 0);

    // A key that belongs to a longer session cannot attach to a shorter one.
    app.attach_session(
        attached("session_b", None, &[("user", "only"), ("assistant", "one")]),
        false,
    );
    assert_eq!(app.submit("/note add u2 not here"), None);
    assert!(last_text(&app).contains("no message u2"), "{}", last_text(&app));

    // An address this session holds still needs an existing note to edit.
    assert_eq!(app.submit("/note edit a1 edited"), None);
    assert!(last_text(&app).contains("no note to edit"), "{}", last_text(&app));
    assert!(note_texts(&app, "session_b").is_empty());
}

#[test]
fn editing_requires_a_note_and_replaces_it() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    app.attach_session(simple("session_a"), false);

    app.submit("/note edit u1 first text");
    assert!(last_text(&app).contains("no note to edit"), "{}", last_text(&app));
    assert_eq!(app.notes.count("session_a"), 0);

    app.submit("/note add u1 first text");
    assert_eq!(app.submit("/note edit u1 second text"), None);
    assert!(last_text(&app).contains("updated"), "{}", last_text(&app));
    assert_eq!(note_texts(&app, "session_a"), vec!["second text".to_string()]);
}

#[test]
fn a_removed_note_leaves_the_screen_and_the_file() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = directory.path().join("tui-notes.json");
    let mut app = app_at(path.clone());
    app.attach_session(simple("session_a"), false);
    app.submit("/note add a1 temporary thought");
    assert!(screen(&mut app).contains("temporary thought"));

    assert_eq!(app.submit("/note remove a1"), None);
    let drawn = screen(&mut app);
    assert!(
        !drawn.contains("temporary thought"),
        "the note is still drawn:\n{drawn}"
    );

    let mut read = NoteStore::at(path);
    read.ensure_loaded();
    assert_eq!(read.count("session_a"), 0);
}

#[test]
fn note_commands_never_produce_a_send() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    app.attach_session(simple("session_a"), false);

    assert_eq!(app.submit("/note add a1 review this answer"), None);
    assert_eq!(app.submit("/note list"), None);
    assert_eq!(app.submit("/note remove a1"), None);

    // The one thing the client sends is the draft, and a note is not in it.
    assert_eq!(
        app.submit("next message"),
        Some(Action::Send("next message".to_string()))
    );
    assert!(
        !app.transcript
            .items()
            .iter()
            .any(|item| notes::excerpt(item).is_some_and(|text| text.contains("review this answer"))),
        "a note reached the conversation rows"
    );
}

#[test]
fn listing_names_the_message_its_quote_and_the_note() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    app.attach_session(simple("session_a"), false);
    app.submit("/note add a1 needs a test");

    assert_eq!(app.submit("/note"), None);
    let listing = last_text(&app);
    assert!(listing.contains("a1"), "{listing}");
    assert!(listing.contains("the answer"), "{listing}");
    assert!(listing.contains("needs a test"), "{listing}");
}

#[test]
fn cleanup_removes_gone_sessions_and_keeps_the_one_in_force() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = directory.path().join("tui-notes.json");
    let mut app = app_at(path.clone());
    app.attach_session(simple("session_a"), false);
    app.submit("/note add a1 alpha note");
    app.attach_session(simple("session_b"), false);
    app.submit("/note add a1 beta note");

    app.sessions_checked(&["session_a".to_string(), "session_b".to_string()], 0);
    assert!(note_texts(&app, "session_a").is_empty());
    assert_eq!(
        note_texts(&app, "session_b"),
        vec!["beta note".to_string()],
        "the session in force lost its note"
    );
    assert!(last_text(&app).contains("removed 1 note"), "{}", last_text(&app));

    app.sessions_checked(&[], 2);
    assert!(last_text(&app).contains("could not be checked"), "{}", last_text(&app));

    let mut read = NoteStore::at(path);
    read.ensure_loaded();
    assert!(read.count("session_a") == 0);
    assert_eq!(read.count("session_b"), 1);
}

#[test]
fn prune_asks_the_harness_about_every_noted_session() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    app.attach_session(simple("session_a"), false);
    assert_eq!(app.submit("/note add a1 check"), None);

    assert_eq!(
        app.submit("/note prune"),
        Some(Action::CheckSessions(vec!["session_a".to_string()]))
    );
}

#[test]
fn attach_session_strips_leading_system_reminders_from_user_history() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut app = app_at(directory.path().join("tui-notes.json"));
    app.attach_session(
        attached(
            "session_a",
            None,
            &[
                (
                    "user",
                    "<system-reminder>\n# Session Context\nDate: 2026-11-25\n</system-reminder>",
                ),
                (
                    "user",
                    "<system-reminder>\nInternal hint\n</system-reminder>\n\nactual prompt",
                ),
                ("assistant", "actual answer"),
            ],
        ),
        false,
    );

    let drawn = screen(&mut app);
    assert!(!drawn.contains("<system-reminder>"), "{drawn}");
    assert!(!drawn.contains("# Session Context"), "{drawn}");
    assert!(drawn.contains("actual prompt"), "{drawn}");
    assert!(drawn.contains("actual answer"), "{drawn}");
}
