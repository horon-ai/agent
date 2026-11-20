//! The notes file: identity, persistence, permissions and concurrent saves.
//!
//! Every test uses its own temporary directory, never `$HORON_HOME`, and the
//! file is read back through a fresh store so persistence is exercised rather
//! than the in-memory map.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::*;
use crate::transcript::{Item, ToolCall, ToolState};

/// The identity of the `occurrence`-th message with this role and text.
fn key(role: Role, text: &str, occurrence: usize) -> MessageKey {
    MessageKey::of(role, text, occurrence)
}

fn user(text: &str) -> MessageKey {
    key(Role::User, text, 1)
}

fn assistant(text: &str) -> MessageKey {
    key(Role::Assistant, text, 1)
}

fn at(role: Role, ordinal: usize) -> Address {
    Address::new(role, ordinal)
}

/// A path under a fresh directory.
fn notes_path(directory: &tempfile::TempDir) -> PathBuf {
    directory.path().join("tui-notes.json")
}

/// The bytes a store would write, for comparing files directly.
fn stored(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("the notes file")).expect("valid JSON")
}

fn tool_row() -> Item {
    Item::Tool(ToolCall {
        call_id: "c1".to_string(),
        name: "bash".to_string(),
        state: ToolState::Done,
        input: String::new(),
        duration_ms: None,
        output: None,
    })
}

#[test]
fn a_note_is_stored_and_read_back_by_session_and_message_content() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    assert!(store.ensure_loaded().is_empty());
    store
        .set(
            "session_one",
            user("hello"),
            at(Role::User, 1),
            "check the first answer",
        )
        .expect("a note");
    store
        .set(
            "session_one",
            assistant("the answer"),
            at(Role::Assistant, 1),
            "this missed the return value",
        )
        .expect("a note");
    assert!(store.save().expect("saved").is_empty());

    let mut read = NoteStore::at(&path);
    assert!(read.ensure_loaded().is_empty());
    assert_eq!(read.note("session_one", user("hello")), Some("check the first answer"));
    assert_eq!(
        read.note("session_one", assistant("the answer")),
        Some("this missed the return value")
    );
    // A different text is a different message, whatever place it takes.
    assert_eq!(read.note("session_one", assistant("a different answer")), None);
    assert_eq!(read.note("session_two", user("hello")), None);
}

#[test]
fn whitespace_does_not_change_which_message_a_note_belongs_to() {
    assert_eq!(assistant("  the  answer \n"), assistant("the answer"));
    assert_ne!(assistant("the answer"), assistant("the answers"));
}

#[test]
fn a_removed_note_is_gone_from_the_file() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    store.ensure_loaded();
    store
        .set("s", assistant("one"), at(Role::Assistant, 1), "one note")
        .expect("a note");
    store
        .set("s", assistant("two"), at(Role::Assistant, 2), "two note")
        .expect("a note");
    store.save().expect("saved");

    assert!(store.remove("s", assistant("one"), None));
    assert!(!store.remove("s", assistant("one"), None));
    store.save().expect("saved");

    let mut read = NoteStore::at(&path);
    read.ensure_loaded();
    assert_eq!(read.note("s", assistant("one")), None);
    assert_eq!(read.note("s", assistant("two")), Some("two note"));
}

#[test]
fn a_note_can_be_removed_by_the_address_it_was_written_at() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut store = NoteStore::at(notes_path(&directory));
    store.ensure_loaded();
    store
        .set("s", assistant("gone text"), at(Role::Assistant, 3), "stale")
        .expect("a note");

    // The content can no longer be addressed, so the stored address is used.
    assert!(store.remove_at("s", at(Role::Assistant, 3)));
    assert!(!store.remove_at("s", at(Role::Assistant, 3)));
    assert_eq!(store.count("s"), 0);
}

#[test]
fn two_sessions_keep_separate_notes_even_with_the_same_messages() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    store.ensure_loaded();
    store
        .set("alpha", assistant("same"), at(Role::Assistant, 1), "only alpha")
        .expect("a note");
    store
        .set("beta", assistant("same"), at(Role::Assistant, 1), "only beta")
        .expect("a note");
    store.save().expect("saved");

    let mut read = NoteStore::at(&path);
    read.ensure_loaded();
    assert_eq!(read.note("alpha", assistant("same")), Some("only alpha"));
    assert_eq!(read.note("beta", assistant("same")), Some("only beta"));
    assert_eq!(read.note("gamma", assistant("same")), None);
}

#[test]
fn note_text_is_one_line_and_bounded() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut store = NoteStore::at(notes_path(&directory));
    store.ensure_loaded();
    store
        .set("s", user("hello"), at(Role::User, 1), "first\nsecond\tthird")
        .expect("a note");
    assert_eq!(store.note("s", user("hello")), Some("first second third"));

    let long: String = "x".repeat(MAX_NOTE_CHARS + 25);
    store.set("s", user("hello"), at(Role::User, 1), &long).expect("a note");
    assert_eq!(
        store.note("s", user("hello")).expect("the note").chars().count(),
        MAX_NOTE_CHARS
    );

    assert_eq!(
        store.set("s", user("hello"), at(Role::User, 1), "  \n\t "),
        Err(NoteError::Empty)
    );
    assert_eq!(
        store.set("  ", user("hello"), at(Role::User, 1), "text"),
        Err(NoteError::NoSession)
    );
}

#[test]
fn addresses_parse_and_reject_zero_and_letters() {
    assert_eq!(Address::parse("a3"), Some(at(Role::Assistant, 3)));
    assert_eq!(Address::parse("u12"), Some(at(Role::User, 12)));
    assert_eq!(Address::parse("A1"), Some(at(Role::Assistant, 1)));
    for bad in ["a0", "u0", "x1", "", "a", "1a", "a-1", "a 1", "a1b"] {
        assert_eq!(Address::parse(bad), None, "`{bad}` parsed as an address");
    }
    assert_eq!(at(Role::Assistant, 3).to_string(), "a3");
    assert_eq!(at(Role::User, 12).to_string(), "u12");
}

#[test]
fn keys_count_only_the_rows_a_replay_restores_and_follow_content() {
    let items = vec![
        Item::User("hello".to_string()),
        Item::Assistant("hi".to_string()),
        tool_row(),
        Item::Notice("a notice".to_string()),
        Item::Assistant("again".to_string()),
        Item::Assistant("hi".to_string()),
    ];
    let entries = keyed(&items, &[]);
    assert_eq!(entries[0].as_ref().expect("user").address, at(Role::User, 1));
    assert_eq!(entries[1].as_ref().expect("assistant").address, at(Role::Assistant, 1));
    assert_eq!(entries[2], None);
    assert_eq!(entries[3], None);
    assert_eq!(entries[4].as_ref().expect("assistant").address, at(Role::Assistant, 2));
    // A repeated text is a second occurrence, not the same message.
    assert_eq!(entries[5].as_ref().expect("assistant").address, at(Role::Assistant, 3));
    assert_eq!(entries[1].as_ref().expect("assistant").key, assistant("hi"));
    assert_eq!(
        entries[5].as_ref().expect("assistant").key,
        key(Role::Assistant, "hi", 2)
    );

    // The same place with different content holds a different identity.
    let replaced = keyed(&[Item::Assistant("different answer".to_string())], &[]);
    assert_ne!(replaced[0].as_ref().expect("assistant").key, assistant("hi"));
    assert_eq!(
        keyed(&items, &[]).into_iter().flatten().last().expect("newest").address,
        at(Role::Assistant, 3)
    );
}

#[test]
fn a_stored_key_survives_the_file_round_trip() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    store.ensure_loaded();
    let stored_key = key(Role::Assistant, "the  answer", 2);
    store
        .set("s", stored_key, at(Role::Assistant, 4), "round trip")
        .expect("a note");
    store.save().expect("saved");

    let mut read = NoteStore::at(&path);
    read.ensure_loaded();
    // The file names the content, so a key built from the same text finds it.
    assert_eq!(read.note("s", assistant_text("the answer", 2)), Some("round trip"));
    let value = stored(&path);
    let items = value["sessions"]["s"]["items"].as_object().expect("items");
    assert_eq!(items.len(), 1);
    let note = items.values().next().expect("one note");
    assert_eq!(note["address"], Value::from("a4"));
    assert_eq!(note["text"], Value::from("round trip"));
    // No stored message was named, so the file holds no id for it.
    assert!(note.get("stored_id").is_none(), "{note}");
}

#[test]
fn a_note_names_the_stored_message_it_was_written_on() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    store.ensure_loaded();
    store
        .set_stored(
            "s",
            assistant("hi"),
            at(Role::Assistant, 1),
            Some("message_2"),
            "on the second",
        )
        .expect("a note");
    store.save().expect("saved");

    let mut read = NoteStore::at(&path);
    read.ensure_loaded();
    let note = read
        .note_record("s", assistant("hi"), Some("message_2"))
        .expect("the note");
    assert_eq!(note.text, "on the second");
    let value = stored(&path);
    assert_eq!(value["sessions"]["s"]["items"].as_object().expect("items").len(), 1);
}

#[test]
fn a_note_follows_its_stored_message_and_never_an_identical_answer() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    store.ensure_loaded();
    let first = assistant("same words");
    let second = assistant_text("same words", 2);
    store
        .set_stored("s", first, at(Role::Assistant, 1), Some("message_a"), "about A")
        .expect("a note");
    assert!(store.note_record("s", first, Some("message_b")).is_none());
    assert_eq!(store.note("s", first), Some("about A"));
    store
        .set_stored("s", first, at(Role::Assistant, 1), Some("message_b"), "about B")
        .expect("a note");
    store.save().expect("saved");

    let mut replay = NoteStore::at(&path);
    replay.ensure_loaded();
    assert_eq!(
        replay
            .note_record("s", second, Some("message_a"))
            .map(|note| note.text.as_str()),
        Some("about A")
    );
    assert_eq!(
        replay
            .note_record("s", first, Some("message_b"))
            .map(|note| note.text.as_str()),
        Some("about B")
    );
    assert_eq!(replay.note("s", first), None, "ambiguous without either ID");
    assert!(replay.remove("s", second, Some("message_a")));
    assert_eq!(replay.note("s", first), Some("about B"));

    replay
        .set("by_text", first, at(Role::Assistant, 1), "old note")
        .expect("a note");
    assert_eq!(
        replay
            .note_record("by_text", first, Some("message_a"))
            .map(|note| note.text.as_str()),
        Some("old note")
    );
}

/// A key whose text is normalised the same way the store does.
fn assistant_text(text: &str, occurrence: usize) -> MessageKey {
    key(Role::Assistant, text, occurrence)
}

#[test]
fn a_corrupt_file_is_moved_aside_instead_of_being_overwritten() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    fs::write(&path, b"{ not json").expect("the corrupt file");
    let mut store = NoteStore::at(&path);
    let warnings = store.ensure_loaded();
    assert!(
        warnings.iter().any(|warning| warning.contains("moved it to")),
        "the read did not report moving the file aside: {warnings:?}"
    );
    let aside = path.with_file_name("tui-notes.json.corrupt");
    assert_eq!(fs::read(&aside).expect("the kept file"), b"{ not json".to_vec());
    assert!(!path.exists(), "the unreadable file was left in place");

    store
        .set("s", user("hello"), at(Role::User, 1), "kept")
        .expect("a note");
    store.save().expect("saved");
    let mut read = NoteStore::at(&path);
    read.ensure_loaded();
    assert_eq!(read.note("s", user("hello")), Some("kept"));
}

#[test]
fn a_newer_schema_is_left_alone_and_nothing_is_written() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let newer = br#"{"version": 2, "sessions": {}}"#;
    fs::write(&path, newer).expect("the newer file");
    let mut store = NoteStore::at(&path);
    let warnings = store.ensure_loaded();
    assert!(
        warnings.iter().any(|warning| warning.contains("schema version 2")),
        "the read did not report the newer schema: {warnings:?}"
    );
    store
        .set("s", user("hello"), at(Role::User, 1), "must not be written")
        .expect("a note");
    let error = store.save().expect_err("a newer schema must not be replaced");
    assert!(error.to_string().contains("nothing was saved"), "{error}");
    assert_eq!(fs::read(&path).expect("the file"), newer.to_vec());
}

#[test]
fn a_second_writer_keeps_both_sets_of_sessions() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut first = NoteStore::at(&path);
    first.ensure_loaded();
    let mut second = NoteStore::at(&path);
    second.ensure_loaded();

    first
        .set("from_first", user("hello"), at(Role::User, 1), "first wrote this")
        .expect("a note");
    second
        .set("from_second", user("hello"), at(Role::User, 1), "second wrote this")
        .expect("a note");
    first.save().expect("the first save");
    second.save().expect("the second save");

    let mut read = NoteStore::at(&path);
    read.ensure_loaded();
    assert_eq!(read.note("from_first", user("hello")), Some("first wrote this"));
    assert_eq!(read.note("from_second", user("hello")), Some("second wrote this"));
}

#[test]
fn a_live_lock_defers_the_save_and_a_stale_one_does_not() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    store.ensure_loaded();
    store
        .set("s", user("hello"), at(Role::User, 1), "waits for the lock")
        .expect("a note");

    let lock = path.with_file_name("tui-notes.json.lock");
    fs::write(&lock, b"1").expect("the lock");
    let error = store.save().expect_err("a live lock must defer the save");
    assert!(error.to_string().contains("another horon-tui"), "{error}");
    // The change is still in memory, so the next save writes it.
    assert_eq!(store.note("s", user("hello")), Some("waits for the lock"));

    fs::remove_file(&lock).expect("unlocked");
    store.save().expect("saved after the lock went away");
    let mut read = NoteStore::at(&path);
    read.ensure_loaded();
    assert_eq!(read.note("s", user("hello")), Some("waits for the lock"));
}

#[test]
fn a_stale_lock_is_broken() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    store.ensure_loaded();
    store
        .set("s", user("hello"), at(Role::User, 1), "saved past a stale lock")
        .expect("a note");

    let lock = path.with_file_name("tui-notes.json.lock");
    fs::write(&lock, b"99999").expect("the lock");
    let old = SystemTime::now() - LOCK_STALE - Duration::from_secs(1);
    fs::File::options()
        .write(true)
        .open(&lock)
        .and_then(|file| file.set_modified(old))
        .expect("aged the lock");
    store.save().expect("a stale lock must not block the save");
    assert!(!lock.exists(), "the stale lock was left behind");
}

#[test]
fn the_file_is_written_for_the_owner_only() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    store.ensure_loaded();
    store
        .set("s", user("hello"), at(Role::User, 1), "private")
        .expect("a note");
    store.save().expect("saved");

    let metadata = fs::metadata(&path).expect("the file");
    assert!(
        horon_agent_sdk::private_fs::is_private_file(&metadata),
        "the notes file is not owner-only"
    );
    if let Some(mode) = horon_agent_sdk::private_fs::mode_bits(&metadata) {
        assert_eq!(mode & 0o777, 0o600, "the notes file is not private: {mode:o}");
    }
    let value = stored(&path);
    assert_eq!(value["version"], Value::from(VERSION));
}

#[test]
fn a_file_found_wider_is_narrowed_when_read() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let body = br#"{"version": 1, "sessions": {"s": {"items": {"a1@0000000000000001": {"text": "seen", "address": "a1", "updated_ms": 1}}}}}"#;
    fs::write(&path, body).expect("the notes file");
    let before = fs::metadata(&path).expect("the file");
    let was_private = horon_agent_sdk::private_fs::is_private_file(&before);

    let mut store = NoteStore::at(&path);
    let warnings = store.ensure_loaded();
    if !was_private {
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("readable by other users")),
            "the read did not report narrowing the file: {warnings:?}"
        );
    }
    let after = fs::metadata(&path).expect("the file");
    assert!(
        horon_agent_sdk::private_fs::is_private_file(&after),
        "the file was left readable by others"
    );
    assert_eq!(store.count("s"), 1, "the stored note was not read");
}

#[test]
fn forgetting_sessions_removes_only_the_named_ones() {
    let directory = tempfile::tempdir().expect("a directory");
    let path = notes_path(&directory);
    let mut store = NoteStore::at(&path);
    store.ensure_loaded();
    store
        .set("gone", user("one"), at(Role::User, 1), "one")
        .expect("a note");
    store
        .set("gone", assistant("two"), at(Role::Assistant, 1), "two")
        .expect("a note");
    store
        .set("kept", user("three"), at(Role::User, 1), "three")
        .expect("a note");
    store.save().expect("saved");

    let (sessions, notes) = store.forget_sessions(&["gone".to_string(), "unknown".to_string()]);
    assert_eq!((sessions, notes), (1, 2));
    store.save().expect("saved");
    assert_eq!(store.note("kept", user("three")), Some("three"));

    let mut read = NoteStore::at(&path);
    read.ensure_loaded();
    assert_eq!(read.note("gone", user("one")), None);
    assert_eq!(read.note("kept", user("three")), Some("three"));
}

#[test]
fn an_in_memory_store_writes_nothing() {
    let mut store = NoteStore::memory();
    assert!(store.ensure_loaded().is_empty());
    store
        .set("s", user("hello"), at(Role::User, 1), "only here")
        .expect("a note");
    assert!(store.save().expect("saved").is_empty());
    assert_eq!(store.note("s", user("hello")), Some("only here"));
}

/// A stored assistant body as the daemon renders it: reasoning markup, the
/// block terminator, then the answer.
fn rendered(reasoning: &str, answer: &str) -> String {
    use horon_agent_sdk::transcript_text::reasoning_line_markup;
    let mut out = String::new();
    for line in reasoning.split('\n') {
        out.push_str(&reasoning_line_markup(line));
    }
    out.push('\n');
    out.push_str(answer);
    out
}

#[test]
fn a_replayed_body_is_split_at_the_daemons_reasoning_markup() {
    let body = rendered("fixture thinking 한글\n\nsecond paragraph", "Fixture answer ...");
    let (reasoning, answer) = crate::transcript::Transcript::split_rendered_assistant(&body);
    assert_eq!(reasoning.as_deref(), Some("fixture thinking 한글\n\nsecond paragraph"));
    assert_eq!(answer, "Fixture answer ...");

    // A body the daemon wrote without reasoning markup is all answer.
    let plain = "Fixture answer ...";
    let (reasoning, answer) = crate::transcript::Transcript::split_rendered_assistant(plain);
    assert_eq!(reasoning, None);
    assert_eq!(answer, plain);

    // Reasoning with no answer leaves no answer row.
    let only = rendered("thinking only", "");
    let (reasoning, answer) = crate::transcript::Transcript::split_rendered_assistant(&only);
    assert_eq!(reasoning.as_deref(), Some("thinking only"));
    assert!(answer.is_empty());
}
