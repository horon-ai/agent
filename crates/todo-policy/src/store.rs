//! On-disk layout of a session's todo state, shared by the runtime (which
//! writes it) and the harness bridge (which serves it to SDK clients).
//!
//! Everything lives under `<horon-agent home>/todos/`. Reads are tolerant: a missing
//! or malformed file yields the empty value, matching the runtime's loaders.

use crate::followup::{TodoEffects, TodoSnapshot};
use crate::{TODO_BLOCKER_MAX_CHARS, TodoEditRefusal, TodoItem, canonical_todo_status};
use horon_agent_time::{Duration, Timestamp};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

/// Private policy. Never include this duration in model-facing text.
pub const LONG_SESSION_REVIEW_AFTER_SECS: i64 = 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoFile {
    Todos,
    Goals,
    Plan,
    GateObservations,
    ReviewState,
}

impl TodoFile {
    fn suffix(self) -> &'static str {
        match self {
            Self::Todos => "",
            Self::Goals => "-goals",
            Self::Plan => "-plan",
            Self::GateObservations => "-gate-observations",
            Self::ReviewState => "-review-state",
        }
    }
}

/// Whether `session_id` is safe to interpolate into a path.
pub fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub fn todo_file_path(home: &Path, session_id: &str, file: TodoFile) -> PathBuf {
    home.join("todos").join(format!("{session_id}{}.json", file.suffix()))
}

/// Persisted long-session review clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoReviewState {
    pub cycle_started_at: Timestamp,
    #[serde(default)]
    pub review_delivered: bool,
}

impl TodoReviewState {
    pub fn is_due(&self, now: Timestamp) -> bool {
        !self.review_delivered && now - self.cycle_started_at >= Duration::seconds(LONG_SESSION_REVIEW_AFTER_SECS)
    }
}

fn read_or_default<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Read everything the follow-up policy needs. Never fails: absent state is
/// an empty snapshot.
pub fn load_snapshot(home: &Path, session_id: &str) -> TodoSnapshot {
    load_snapshot_with_revision(home, session_id).0
}

/// Read the stored todo list and the revision that names the exact bytes it
/// was parsed from.
///
/// Both come from one read, so a writer between two reads cannot make the
/// revision describe a list other than the one returned. A missing file is the
/// empty list and carries the revision of that same empty value. A list that
/// exists but cannot be read, or cannot be parsed, still yields the empty list
/// every client already tolerates, but no revision: an empty list with no
/// revision is read-only, so it can never be offered as an editable blank
/// slate over a store this process could not understand.
pub fn load_todos_with_revision(home: &Path, session_id: &str) -> (Vec<TodoItem>, Option<String>) {
    match std::fs::read(todo_file_path(home, session_id, TodoFile::Todos)) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(todos) => (todos, Some(revision_of(&bytes))),
            // A corrupt list still reads as the empty list every tolerant
            // client already sees, but it carries no revision: the edit path
            // re-parses strictly, so exposing a revision here would only offer
            // a write that must fail.
            Err(_) => (Vec::new(), None),
        },
        // A missing file is a real empty list: that is the state a session
        // starts in, and it can carry a revision like any other list.
        Err(error) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), Some(revision_of(b"[]"))),
        // An unreadable list (permissions, I/O) is not evidence of "no todos",
        // so it reads as empty for compatibility but is not offered as an
        // editable list.
        Err(_) => (Vec::new(), None),
    }
}

/// [`load_snapshot`] with the todo list's revision beside it.
///
/// The revision covers the list only, not the plan or the goal assessments:
/// those are read and written by the agent, and a change to one of them does
/// not change what an operator is looking at when editing an item's status.
pub fn load_snapshot_with_revision(home: &Path, session_id: &str) -> (TodoSnapshot, Option<String>) {
    let path = |file| todo_file_path(home, session_id, file);
    let review: Option<TodoReviewState> = std::fs::read_to_string(path(TodoFile::ReviewState))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let (todos, revision) = load_todos_with_revision(home, session_id);
    (
        TodoSnapshot {
            todos,
            plan: read_or_default(&path(TodoFile::Plan)),
            goals: read_or_default(&path(TodoFile::Goals)),
            gate_observations: read_or_default(&path(TodoFile::GateObservations)),
            long_session_review_due: review.is_some_and(|state| state.is_due(Timestamp::now())),
        },
        revision,
    )
}

/// The revision of one stored todo file.
///
/// A digest rather than a modification time or a counter: every writer replaces
/// the file atomically, and a digest of the bytes it wrote is the one value
/// that changes exactly when the list an operator read would change.
fn revision_of(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// What one revision-checked edit did to the stored list.
///
/// A refusal is not a failure: the list still stands, `applied` is false, and
/// `revision`/`todos` are what the client should now show. `refusal` says which
/// check stopped the write so a caller can word its own sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoEditOutcome {
    pub applied: bool,
    pub refusal: Option<TodoEditRefusal>,
    pub todos: Vec<TodoItem>,
    pub revision: String,
}

impl TodoEditOutcome {
    fn refused(refusal: TodoEditRefusal, todos: Vec<TodoItem>, revision: String) -> Self {
        Self {
            applied: false,
            refusal: Some(refusal),
            todos,
            revision,
        }
    }
}

/// Set one item's status in the stored list, only if the list is still the one
/// the caller read.
///
/// The comparison, the change and the write happen under one exclusive lock, so
/// two edits through this function cannot both pass on the same revision. The
/// revision names the bytes the caller last read; a list that moved since then
/// answers [`TodoEditRefusal::StaleRevision`] with the current list and revision
/// and is left untouched. An id the list does not hold is
/// [`TodoEditRefusal::UnknownItem`], and a status outside the canonical
/// vocabulary is [`TodoEditRefusal::InvalidStatus`]; neither writes.
///
/// The edit follows the same invariants as the todo tool: the stored status is
/// canonical, a `blocked` item must already carry a non-empty note (this edit
/// has no field to supply one), and a note is dropped from every other status.
/// Only the list is written: the plan, the goal assessments and the private
/// long-session review clock belong to the agent and are not touched.
///
/// The lock is a sibling file rather than the list itself, so a reader never
/// waits on a writer and a crashed writer leaves a lock the kernel releases.
pub fn apply_todo_status_edit(
    home: &Path,
    session_id: &str,
    expected_revision: &str,
    id: &str,
    status: &str,
) -> io::Result<TodoEditOutcome> {
    let path = todo_file_path(home, session_id, TodoFile::Todos);
    apply_todo_status_edit_with(home, session_id, expected_revision, id, status, |todos| {
        let written = serde_json::to_vec(todos).map_err(io::Error::other)?;
        write_atomic(&path, &written)
    })
}

/// [`apply_todo_status_edit`] with the caller's own save function.
///
/// The caller owns the durable write so every writer of the list can use the
/// same storage function, but the revision comparison, the canonical status,
/// the blocker rule and the lock are decided here, once. `save` runs while the
/// lock is held.
pub fn apply_todo_status_edit_with(
    home: &Path,
    session_id: &str,
    expected_revision: &str,
    id: &str,
    status: &str,
    save: impl FnOnce(&[TodoItem]) -> io::Result<()>,
) -> io::Result<TodoEditOutcome> {
    let path = todo_file_path(home, session_id, TodoFile::Todos);
    let _lock = lock_todos(home, session_id)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let revision = revision_of(bytes.as_deref().unwrap_or(b"[]"));
    // The list is parsed strictly before any refusal is built. A store this
    // process cannot understand must not be reported as an empty editable list
    // through a stale reply: the caller gets an error and the bytes stay as
    // they are.
    let mut todos: Vec<TodoItem> = match bytes.as_deref() {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the stored todo list is not valid JSON: {error}"),
            )
        })?,
        None => Vec::new(),
    };
    if expected_revision.trim() != revision {
        return Ok(TodoEditOutcome::refused(
            TodoEditRefusal::StaleRevision,
            todos,
            revision,
        ));
    }
    let Some(canonical) = canonical_todo_status(status) else {
        return Ok(TodoEditOutcome::refused(
            TodoEditRefusal::InvalidStatus,
            todos,
            revision,
        ));
    };
    let Some(index) = todos.iter().position(|todo| todo.id == id) else {
        return Ok(TodoEditOutcome::refused(TodoEditRefusal::UnknownItem, todos, revision));
    };
    // The note is read out and validated before the item is borrowed mutably:
    // a `blocked` status without a usable note refuses the whole edit instead
    // of writing a status the reader cannot act on.
    let note = todos
        .get(index)
        .and_then(|todo| todo.blocker.as_deref())
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(str::to_string);
    let blocked_note = if canonical == "blocked" {
        let Some(note) = note.filter(|note| note.chars().count() <= TODO_BLOCKER_MAX_CHARS) else {
            return Ok(TodoEditOutcome::refused(
                TodoEditRefusal::InvalidBlocker,
                todos,
                revision,
            ));
        };
        Some(note)
    } else {
        None
    };
    todos[index].status = canonical.to_string();
    todos[index].blocker = blocked_note;
    save(&todos)?;
    let written = serde_json::to_vec(&todos).map_err(io::Error::other)?;
    Ok(TodoEditOutcome {
        applied: true,
        refusal: None,
        todos,
        revision: revision_of(&written),
    })
}

/// The exclusive lock one todo write holds while it compares and writes.
///
/// Every writer of the list — the todo tool, an operator edit, the RPC host —
/// takes this same lock for its whole read-modify-write, so two writers cannot
/// both validate against one list and then overwrite each other. It is a
/// sibling file rather than the list itself, so a reader never waits on a
/// writer. Left in place after the write: removing it would let a third process
/// lock a fresh file while a second still waits on the old one.
pub fn lock_todos(home: &Path, session_id: &str) -> io::Result<std::fs::File> {
    let dir = home.join("todos");
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!("{session_id}.edit.lock")))?;
    file.lock()?;
    Ok(file)
}

/// Replace the list with one atomic rename, so a reader never parses half a
/// write and the previous list survives a failure before the rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "todos.json".to_string());
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        // The rename failure is the one the caller needs; the temp file is
        // best-effort cleanup and must not replace it.
        std::fs::remove_file(&tmp).unwrap_or(());
        return Err(error);
    }
    Ok(())
}

/// Apply the persistent side effects of a follow-up decision.
pub fn apply_effects(home: &Path, session_id: &str, effects: TodoEffects) -> std::io::Result<()> {
    if effects.clear_gate_observations {
        match std::fs::remove_file(todo_file_path(home, session_id, TodoFile::GateObservations)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if effects.mark_long_session_review_delivered {
        let path = todo_file_path(home, session_id, TodoFile::ReviewState);
        if let Some(mut state) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<TodoReviewState>(&text).ok())
        {
            state.review_delivered = true;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&state)?)?;
            std::fs::rename(tmp, path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GateObservation, GateObservationKind, TodoItem};

    #[test]
    fn layout_matches_runtime_file_names() {
        let home = Path::new("/h");
        assert_eq!(todo_file_path(home, "s", TodoFile::Todos), Path::new("/h/todos/s.json"));
        assert_eq!(
            todo_file_path(home, "s", TodoFile::GateObservations),
            Path::new("/h/todos/s-gate-observations.json")
        );
        assert!(!is_safe_session_id("../x"));
        assert!(is_safe_session_id("session_a_1_ff"));
    }

    #[test]
    fn snapshot_reads_state_and_effects_consume_it() {
        let dir = std::env::temp_dir().join(format!("todo-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("todos")).unwrap();
        let write = |file, value: serde_json::Value| {
            std::fs::write(todo_file_path(&dir, "s", file), value.to_string()).unwrap()
        };
        let todos = vec![TodoItem {
            content: "a".into(),
            status: "pending".into(),
            ..Default::default()
        }];
        write(TodoFile::Todos, serde_json::to_value(&todos).unwrap());
        let observation = GateObservation {
            kind: GateObservationKind::ClosedFeedbackLoop,
            group: None,
            state: None,
        };
        write(
            TodoFile::GateObservations,
            serde_json::to_value([&observation]).unwrap(),
        );
        let old = TodoReviewState {
            cycle_started_at: Timestamp::now() - Duration::hours(2),
            review_delivered: false,
        };
        write(TodoFile::ReviewState, serde_json::to_value(&old).unwrap());

        let snapshot = load_snapshot(&dir, "s");
        assert_eq!(snapshot.todos, todos);
        assert_eq!(snapshot.gate_observations, vec![observation]);
        assert!(snapshot.long_session_review_due);

        apply_effects(
            &dir,
            "s",
            TodoEffects {
                clear_gate_observations: true,
                mark_long_session_review_delivered: true,
            },
        )
        .unwrap();
        let after = load_snapshot(&dir, "s");
        assert!(after.gate_observations.is_empty());
        assert!(!after.long_session_review_due);
        // Idempotent.
        apply_effects(
            &dir,
            "s",
            TodoEffects {
                clear_gate_observations: true,
                mark_long_session_review_delivered: true,
            },
        )
        .unwrap();
        assert_eq!(load_snapshot(&dir, "missing"), TodoSnapshot::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn edit_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("todo-edit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("todos")).unwrap();
        dir
    }

    fn write_todos(dir: &Path, todos: &[TodoItem]) {
        std::fs::write(
            todo_file_path(dir, "s", TodoFile::Todos),
            serde_json::to_vec(todos).unwrap(),
        )
        .unwrap();
    }

    fn items() -> Vec<TodoItem> {
        vec![
            TodoItem {
                id: "t1".into(),
                content: "ship".into(),
                status: "pending".into(),
                priority: "high".into(),
                ..Default::default()
            },
            TodoItem {
                id: "t2".into(),
                content: "review".into(),
                status: "completed".into(),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn stale_edit_is_refused_and_leaves_the_stored_bytes_untouched() {
        let dir = edit_home("stale");
        write_todos(&dir, &items());
        let before = std::fs::read(todo_file_path(&dir, "s", TodoFile::Todos)).unwrap();
        let (_, revision) = load_todos_with_revision(&dir, "s");
        let revision = revision.expect("a readable list has a revision");

        let outcome = apply_todo_status_edit(&dir, "s", "not-the-list", "t1", "completed").unwrap();
        assert!(!outcome.applied);
        assert_eq!(outcome.refusal, Some(TodoEditRefusal::StaleRevision));
        assert_eq!(outcome.revision, revision);
        assert_eq!(outcome.todos.len(), 2);
        assert_eq!(
            std::fs::read(todo_file_path(&dir, "s", TodoFile::Todos)).unwrap(),
            before,
            "a stale edit must not touch the stored list"
        );

        // The revision the caller read applies exactly once.
        let outcome = apply_todo_status_edit(&dir, "s", &revision, "t1", "completed").unwrap();
        assert!(outcome.applied);
        assert_eq!(outcome.refusal, None);
        assert_eq!(outcome.todos[0].status, "completed");
        assert_ne!(outcome.revision, revision);
        let (stored, stored_revision) = load_todos_with_revision(&dir, "s");
        assert_eq!(stored, outcome.todos);
        assert_eq!(stored_revision.as_deref(), Some(outcome.revision.as_str()));

        // And the old revision is stale against the new list.
        let again = apply_todo_status_edit(&dir, "s", &revision, "t1", "cancelled").unwrap();
        assert_eq!(again.refusal, Some(TodoEditRefusal::StaleRevision));
        assert_eq!(again.todos[0].status, "completed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_checks_the_item_scope_status_and_blocker_note() {
        let dir = edit_home("scope");
        write_todos(&dir, &items());
        let (_, revision) = load_todos_with_revision(&dir, "s");
        let revision = revision.expect("a readable list has a revision");

        let unknown = apply_todo_status_edit(&dir, "s", &revision, "nope", "completed").unwrap();
        assert_eq!(unknown.refusal, Some(TodoEditRefusal::UnknownItem));
        let invalid = apply_todo_status_edit(&dir, "s", &revision, "t1", "almost done").unwrap();
        assert_eq!(invalid.refusal, Some(TodoEditRefusal::InvalidStatus));
        let blocked = apply_todo_status_edit(&dir, "s", &revision, "t1", "blocked").unwrap();
        assert_eq!(blocked.refusal, Some(TodoEditRefusal::InvalidBlocker));
        assert_eq!(blocked.revision, revision);
        assert_eq!(load_todos_with_revision(&dir, "s").0, items());

        // Only the status changes: the other items and the plan/goal files are
        // the agent's, and an operator edit never writes them.
        let outcome = apply_todo_status_edit(&dir, "s", &revision, "t1", "in_progress").unwrap();
        assert!(outcome.applied);
        assert_eq!(outcome.todos[1], items()[1]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_list_is_empty_and_has_its_own_revision() {
        let dir = edit_home("missing");
        let (todos, revision) = load_todos_with_revision(&dir, "s");
        assert!(todos.is_empty());
        let revision = revision.expect("a missing list is a real empty list");
        assert!(!revision.is_empty());
        let outcome = apply_todo_status_edit(&dir, "s", &revision, "t1", "completed").unwrap();
        assert_eq!(outcome.refusal, Some(TodoEditRefusal::UnknownItem));
        assert_eq!(outcome.revision, revision);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_list_reads_empty_without_a_revision_and_cannot_be_edited() {
        let dir = edit_home("corrupt");
        let path = todo_file_path(&dir, "s", TodoFile::Todos);
        std::fs::write(&path, b"{not json").unwrap();
        let before = std::fs::read(&path).unwrap();

        let (todos, revision) = load_todos_with_revision(&dir, "s");
        assert!(todos.is_empty(), "a tolerant reader still sees no items");
        assert_eq!(revision, None, "a corrupt list must not look editable");

        // Even an edit that guesses the bytes' revision is refused before the
        // write, so a corrupt store is never replaced by an operator edit.
        let guessed = revision_of(&before);
        let error = apply_todo_status_edit(&dir, "s", &guessed, "t1", "completed").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
