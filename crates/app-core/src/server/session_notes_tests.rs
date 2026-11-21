//! What the observation view promises a reader: notes stay with the session
//! that made them, off means nothing is kept, repeats fold, and the cap keeps
//! its own arithmetic straight.

use super::*;

fn session() -> String {
    format!("test-notes-{}", uuid::Uuid::new_v4())
}

#[test]
fn a_note_is_only_visible_to_the_session_that_recorded_it() {
    let mine = session();
    let other = session();

    let off = view(&other);
    assert!(!off.recording, "a session that never opted in must read as off");
    record(&other, NoteKind::Work, "background_task_progress", Some("build"));
    assert!(view(&other).notes.is_empty(), "a record made before opting in must not exist");

    set_recording(&mine, true);
    set_recording(&other, true);
    record(&mine, NoteKind::Work, "background_task_progress", Some("tests"));
    record(&other, NoteKind::Work, "background_task_progress", Some("build"));

    let mine_view = view(&mine);
    assert_eq!(mine_view.notes.len(), 1);
    assert_eq!(mine_view.notes[0].note, "background_task_progress · tests");
    assert_eq!(mine_view.counts.work["background_task_progress"], 1);

    let other_view = view(&other);
    assert_eq!(other_view.notes.len(), 1);
    assert_eq!(other_view.notes[0].note, "background_task_progress · build");
    assert_eq!(other_view.counts.recorded, 1);

    forget_session(&mine);
    forget_session(&other);
    assert!(!view(&mine).recording);
    assert!(view(&mine).notes.is_empty());
}

#[test]
fn recording_is_off_until_asked_for() {
    let id = session();
    record(&id, NoteKind::Hook, "pre_tool", Some("bash"));
    let view = view(&id);
    assert!(!view.recording);
    assert!(view.notes.is_empty());
    assert_eq!(view.counts.recorded, 0);
    forget_session(&id);
}

#[test]
fn repeats_fold_and_the_cap_counts_every_line_it_drops() {
    let mut notes = SessionNotes::default();
    notes.set_recording(true);

    for _ in 0..3 {
        notes.record(NoteKind::Hook, "pre_tool", Some("bash"));
    }
    assert_eq!(notes.notes.len(), 1);
    assert_eq!(notes.notes[0].repeats, 3);
    assert_eq!(notes.counts.recorded, 3);
    assert_eq!(notes.counts.repeated, 2);

    notes.clear();
    for index in 0..NOTE_CAP + 2 {
        notes.record(NoteKind::Work, "background_task_progress", Some(&format!("task {index}")));
    }

    let counts = &notes.counts;
    assert_eq!(counts.retained, NOTE_CAP);
    assert_eq!(notes.notes.len(), NOTE_CAP);
    assert_eq!(counts.dropped, 2, "two distinct lines must be evicted and counted");
    let folded: u64 = notes.notes.iter().map(|note| note.repeats).sum();
    assert_eq!(
        counts.recorded,
        folded + counts.repeated + counts.dropped,
        "the counters must account for every accepted note: {counts:?}"
    );
    assert_eq!(counts.work["background_task_progress"], (NOTE_CAP + 2) as u64);
}

#[test]
fn a_note_is_one_bounded_line() {
    let mut notes = SessionNotes::default();
    notes.set_recording(true);
    let detail = format!("line one\nline\t two {} tail", "x".repeat(NOTE_TEXT_MAX));
    notes.record(NoteKind::Hook, "post_tool", Some(&detail));

    let text = &notes.notes[0].text;
    assert!(!text.contains('\n') && !text.contains('\t'), "control characters leaked: {text:?}");
    assert!(text.chars().count() <= NOTE_TEXT_MAX, "note is not bounded: {}", text.chars().count());
    assert!(text.ends_with('…'), "a cut note must say it was cut: {text:?}");
}

#[test]
fn clear_drops_the_notes_but_keeps_the_switch() {
    let id = session();
    set_recording(&id, true);
    record(&id, NoteKind::Hook, "turn_start", Some("chat"));

    let cleared = clear(&id);
    assert!(cleared.recording, "clearing a view must not turn recording off");
    assert!(cleared.notes.is_empty());
    assert_eq!(cleared.counts.recorded, 0);

    record(&id, NoteKind::Hook, "turn_end", Some("ok"));
    assert_eq!(view(&id).notes.len(), 1, "recording continues after a clear");
    forget_session(&id);
}

#[test]
fn the_command_turns_recording_on_and_refuses_an_unknown_word() {
    let id = session();

    let (data, message) = command(&id, &[]).expect("a status view");
    assert_eq!(data["recording"], serde_json::Value::Bool(false));
    assert!(message.contains("\"recording\": false"), "the message is the payload: {message}");

    let (data, _) = command(&id, &["on".to_string()]).expect("enabling");
    assert_eq!(data["recording"], serde_json::Value::Bool(true));

    assert!(command(&id, &["sometimes".to_string()]).is_err());
    forget_session(&id);
}

#[test]
fn removal_forgets_the_session_so_a_reused_id_starts_empty() {
    let id = session();
    set_recording(&id, true);
    record(&id, NoteKind::Work, "background_task_completed", Some("bash"));
    assert_eq!(view(&id).notes.len(), 1);

    forget_session(&id);
    let after = view(&id);
    assert!(!after.recording);
    assert!(after.notes.is_empty());
    assert_eq!(after.counts.recorded, 0);
}
