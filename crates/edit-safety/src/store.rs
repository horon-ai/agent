//! The mutation-unit journal: one unit per tool call, whole-unit eviction under
//! a byte budget, and an undo that either restores everything or refuses.
//!
//! # Contract
//!
//! A unit opens implicitly on the first [`SnapshotStore::begin_file`] for a
//! tool call id and stays active until [`SnapshotStore::commit`] or
//! [`SnapshotStore::abort`]. Committing moves it into the undo history; aborting
//! releases its blobs and leaves the history untouched. Undo works on the newest
//! committed unit only, and returns the bytes to restore rather than writing
//! them, because this crate performs no I/O.
//!
//! # What this does not do
//!
//! It does not compare the recorded post-state with the file on disk. A caller
//! that wants to refuse a stale undo compares `BlobId::of(current_bytes)` with
//! the unit's `after` state before applying anything.

use crate::blob::{BlobId, BlobStore};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

/// Byte budget used by [`SnapshotStore::new`].
pub const DEFAULT_BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// Recorded state of one file at one point in a mutation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileState {
    /// The path did not exist.
    Absent,
    /// The path held exactly the bytes named by this blob.
    Present(BlobId),
    /// The path could not be read, so its bytes are unknown.
    Unreadable { reason: String },
}

/// What the caller hands over for one side of a file change.
///
/// [`FileState`] is the recorded form; this is the input form, because the
/// store owns blob interning and only it can mint a [`BlobId`] for new bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    Absent,
    Bytes(Vec<u8>),
    Unreadable { reason: String },
}

impl FileContent {
    fn into_state(self, blobs: &mut BlobStore) -> FileState {
        match self {
            Self::Absent => FileState::Absent,
            Self::Bytes(bytes) => FileState::Present(blobs.insert(&bytes)),
            Self::Unreadable { reason } => FileState::Unreadable { reason },
        }
    }
}

/// One file's before and after state inside a mutation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: PathBuf,
    pub before: FileState,
    pub after: FileState,
}

/// Every file change made by one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationUnit {
    pub tool_call_id: String,
    pub files: Vec<FileChange>,
}

/// Why a store operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditSafetyError {
    /// No active unit exists for this tool call id.
    UnknownUnit { tool_call_id: String },
    /// The unit recorded no files, so committing it would put a no-op in the
    /// undo history.
    EmptyUnit { tool_call_id: String },
    /// The unit already records this path; one tool call describes each file
    /// once, with a single before/after pair.
    DuplicateFile { tool_call_id: String, path: PathBuf },
    /// The undo history has no committed unit.
    NothingToUndo,
    /// Undoing this unit would restore a file whose recorded state is unknown.
    UndoBlockedByUnreadable {
        tool_call_id: String,
        path: PathBuf,
        reason: String,
    },
    /// A recorded state names bytes the store no longer holds. This is an
    /// internal invariant break, not a caller error.
    MissingBlob { blob_id: BlobId },
}

impl std::fmt::Display for EditSafetyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownUnit { tool_call_id } => {
                write!(formatter, "no active mutation unit for tool call {tool_call_id}")
            }
            Self::EmptyUnit { tool_call_id } => {
                write!(formatter, "mutation unit {tool_call_id} recorded no files")
            }
            Self::DuplicateFile { tool_call_id, path } => {
                write!(
                    formatter,
                    "mutation unit {tool_call_id} already records {}",
                    path.display()
                )
            }
            Self::NothingToUndo => formatter.write_str("undo history is empty"),
            Self::UndoBlockedByUnreadable {
                tool_call_id,
                path,
                reason,
            } => write!(
                formatter,
                "cannot undo {tool_call_id}: {} was unreadable ({reason})",
                path.display()
            ),
            Self::MissingBlob { blob_id } => write!(formatter, "blob {blob_id} is missing from the store"),
        }
    }
}

impl std::error::Error for EditSafetyError {}

/// What a commit did, including any history the budget forced out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    pub tool_call_id: String,
    pub files: usize,
    pub evicted_units: Vec<String>,
    pub byte_usage: usize,
}

/// The state one file must be returned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreState {
    Absent,
    Bytes(Vec<u8>),
}

/// One file to restore, with its bytes already copied out of the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreFile {
    pub path: PathBuf,
    pub state: RestoreState,
}

/// What an undo hands back to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoOutcome {
    pub tool_call_id: String,
    pub files: Vec<RestoreFile>,
}

/// Content-addressed snapshots for one process, with a bounded undo history.
pub struct SnapshotStore {
    blobs: BlobStore,
    history: VecDeque<MutationUnit>,
    active: HashMap<String, MutationUnit>,
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotStore {
    /// A store with [`DEFAULT_BYTE_BUDGET`].
    pub fn new() -> Self {
        Self::with_byte_budget(DEFAULT_BYTE_BUDGET)
    }

    /// A store whose committed history is trimmed to `byte_budget` unique bytes
    /// whenever a unit commits.
    pub fn with_byte_budget(byte_budget: usize) -> Self {
        Self {
            blobs: BlobStore::new(byte_budget),
            history: VecDeque::new(),
            active: HashMap::new(),
        }
    }

    /// Record one file's before and after state in the unit for
    /// `tool_call_id`, opening the unit if this is its first file.
    pub fn begin_file(
        &mut self,
        tool_call_id: &str,
        path: impl Into<PathBuf>,
        before: FileContent,
        after: FileContent,
    ) -> Result<(), EditSafetyError> {
        let path = path.into();
        if self
            .active
            .get(tool_call_id)
            .is_some_and(|unit| unit.files.iter().any(|file| file.path == path))
        {
            return Err(EditSafetyError::DuplicateFile {
                tool_call_id: tool_call_id.to_string(),
                path,
            });
        }
        let before = before.into_state(&mut self.blobs);
        let after = after.into_state(&mut self.blobs);
        self.active
            .entry(tool_call_id.to_string())
            .or_insert_with(|| MutationUnit {
                tool_call_id: tool_call_id.to_string(),
                files: Vec::new(),
            })
            .files
            .push(FileChange { path, before, after });
        Ok(())
    }

    /// Move the unit into the undo history and trim the history to the budget.
    ///
    /// Eviction removes the oldest units whole, one at a time, until usage fits
    /// or only the unit just committed remains. A unit larger than the entire
    /// budget therefore survives while it is newest.
    pub fn commit(&mut self, tool_call_id: &str) -> Result<CommitOutcome, EditSafetyError> {
        let Some(unit) = self.active.remove(tool_call_id) else {
            return Err(EditSafetyError::UnknownUnit {
                tool_call_id: tool_call_id.to_string(),
            });
        };
        if unit.files.is_empty() {
            return Err(EditSafetyError::EmptyUnit {
                tool_call_id: tool_call_id.to_string(),
            });
        }
        let files = unit.files.len();
        self.history.push_back(unit);

        let mut evicted_units = Vec::new();
        while self.blobs.used_bytes() > self.blobs.budget_bytes() && self.history.len() > 1 {
            let Some(oldest) = self.history.pop_front() else {
                break;
            };
            evicted_units.push(oldest.tool_call_id.clone());
            self.release_unit(&oldest);
        }

        Ok(CommitOutcome {
            tool_call_id: tool_call_id.to_string(),
            files,
            evicted_units,
            byte_usage: self.blobs.used_bytes(),
        })
    }

    /// Drop the active unit and release its blobs. Nothing about it reaches the
    /// undo history.
    pub fn abort(&mut self, tool_call_id: &str) -> Result<(), EditSafetyError> {
        let Some(unit) = self.active.remove(tool_call_id) else {
            return Err(EditSafetyError::UnknownUnit {
                tool_call_id: tool_call_id.to_string(),
            });
        };
        self.release_unit(&unit);
        Ok(())
    }

    /// Undo the newest committed unit, returning the bytes to put back.
    ///
    /// The unit is removed from the history only when every file could be
    /// materialized. A unit that records an unreadable file is refused as a
    /// whole, because restoring the rest would report a partial state as a
    /// successful undo.
    pub fn undo(&mut self) -> Result<UndoOutcome, EditSafetyError> {
        let Some(unit) = self.history.pop_back() else {
            return Err(EditSafetyError::NothingToUndo);
        };
        if let Some(blocked) = blocked_by_unreadable(&unit) {
            self.history.push_back(unit);
            return Err(blocked);
        }
        let files = match restore_files(&self.blobs, &unit) {
            Ok(files) => files,
            Err(error) => {
                self.history.push_back(unit);
                return Err(error);
            }
        };
        self.release_unit(&unit);
        Ok(UndoOutcome {
            tool_call_id: unit.tool_call_id,
            files,
        })
    }

    /// Bytes stored under `id`, or `None` once the last referencing unit is
    /// gone.
    pub fn blob(&self, id: &BlobId) -> Option<&[u8]> {
        self.blobs.get(id)
    }

    pub fn byte_usage(&self) -> usize {
        self.blobs.used_bytes()
    }

    pub fn byte_budget(&self) -> usize {
        self.blobs.budget_bytes()
    }

    pub fn blob_count(&self) -> usize {
        self.blobs.blob_count()
    }

    /// Committed units in the undo history.
    pub fn unit_count(&self) -> usize {
        self.history.len()
    }

    pub fn active_unit_count(&self) -> usize {
        self.active.len()
    }

    /// Committed tool call ids, oldest first.
    pub fn history_ids(&self) -> Vec<&str> {
        self.history.iter().map(|unit| unit.tool_call_id.as_str()).collect()
    }

    /// Committed units, oldest first.
    pub fn history(&self) -> impl Iterator<Item = &MutationUnit> + '_ {
        self.history.iter()
    }

    fn release_unit(&mut self, unit: &MutationUnit) {
        for file in &unit.files {
            for state in [&file.before, &file.after] {
                if let FileState::Present(id) = state {
                    self.blobs.release(id);
                }
            }
        }
    }
}

fn blocked_by_unreadable(unit: &MutationUnit) -> Option<EditSafetyError> {
    for file in &unit.files {
        for state in [&file.before, &file.after] {
            if let FileState::Unreadable { reason } = state {
                return Some(EditSafetyError::UndoBlockedByUnreadable {
                    tool_call_id: unit.tool_call_id.clone(),
                    path: file.path.clone(),
                    reason: reason.clone(),
                });
            }
        }
    }
    None
}

fn restore_files(blobs: &BlobStore, unit: &MutationUnit) -> Result<Vec<RestoreFile>, EditSafetyError> {
    let mut files = Vec::with_capacity(unit.files.len());
    for file in &unit.files {
        let state = match &file.before {
            FileState::Absent => RestoreState::Absent,
            FileState::Present(id) => {
                let bytes = blobs
                    .get(id)
                    .ok_or_else(|| EditSafetyError::MissingBlob { blob_id: id.clone() })?;
                RestoreState::Bytes(bytes.to_vec())
            }
            FileState::Unreadable { reason } => {
                return Err(EditSafetyError::UndoBlockedByUnreadable {
                    tool_call_id: unit.tool_call_id.clone(),
                    path: file.path.clone(),
                    reason: reason.clone(),
                });
            }
        };
        files.push(RestoreFile {
            path: file.path.clone(),
            state,
        });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(bytes: &[u8]) -> FileContent {
        FileContent::Bytes(bytes.to_vec())
    }

    #[test]
    fn identical_bytes_stored_twice_share_one_blob() {
        let mut store = SnapshotStore::new();
        store
            .begin_file("call-1", "a.txt", FileContent::Absent, present(b"same bytes\n"))
            .unwrap();
        store.commit("call-1").unwrap();
        store
            .begin_file("call-2", "a.txt", present(b"same bytes\n"), present(b"same bytes\n"))
            .unwrap();
        store.commit("call-2").unwrap();

        assert_eq!(store.blob_count(), 1);
        assert_eq!(store.byte_usage(), b"same bytes\n".len());
    }

    #[test]
    fn budget_evicts_the_oldest_unit_and_keeps_the_newest() {
        let mut store = SnapshotStore::with_byte_budget(10);
        store
            .begin_file("call-old", "old.txt", FileContent::Absent, present(b"aaaaaa"))
            .unwrap();
        store.commit("call-old").unwrap();
        assert_eq!(store.history_ids(), vec!["call-old"]);

        store
            .begin_file("call-new", "new.txt", FileContent::Absent, present(b"bbbbbb"))
            .unwrap();
        let outcome = store.commit("call-new").unwrap();

        assert_eq!(outcome.evicted_units, vec!["call-old".to_string()]);
        assert_eq!(store.history_ids(), vec!["call-new"]);
        assert_eq!(store.byte_usage(), 6);
        assert_eq!(store.unit_count(), 1);

        let restored = store.undo().unwrap();
        assert_eq!(restored.tool_call_id, "call-new");
        assert!(matches!(store.undo(), Err(EditSafetyError::NothingToUndo)));
    }

    #[test]
    fn a_unit_larger_than_the_budget_survives_while_it_is_newest() {
        let mut store = SnapshotStore::with_byte_budget(2);
        store
            .begin_file("call-big", "big.txt", FileContent::Absent, present(b"0123456789"))
            .unwrap();
        let outcome = store.commit("call-big").unwrap();

        assert!(outcome.evicted_units.is_empty());
        assert_eq!(store.byte_usage(), 10);
        assert_eq!(store.history_ids(), vec!["call-big"]);
    }

    #[test]
    fn eviction_frees_only_blobs_no_remaining_unit_holds() {
        let mut store = SnapshotStore::with_byte_budget(4);
        store
            .begin_file("call-old", "a.txt", FileContent::Absent, present(b"same"))
            .unwrap();
        store.commit("call-old").unwrap();
        store
            .begin_file("call-new", "b.txt", FileContent::Absent, present(b"same"))
            .unwrap();
        store
            .begin_file("call-new", "c.txt", FileContent::Absent, present(b"more"))
            .unwrap();
        let outcome = store.commit("call-new").unwrap();

        assert_eq!(outcome.evicted_units, vec!["call-old".to_string()]);
        assert_eq!(store.history_ids(), vec!["call-new"]);
        assert_eq!(store.blob_count(), 2);
        assert_eq!(store.byte_usage(), 8);
    }

    #[test]
    fn abort_leaves_no_trace() {
        let mut store = SnapshotStore::new();
        store
            .begin_file("call-1", "a.txt", FileContent::Absent, present(b"kept"))
            .unwrap();
        store.commit("call-1").unwrap();
        store
            .begin_file("call-2", "b.txt", FileContent::Absent, present(b"aborted"))
            .unwrap();
        assert_eq!(store.active_unit_count(), 1);

        store.abort("call-2").unwrap();

        assert_eq!(store.active_unit_count(), 0);
        assert_eq!(store.unit_count(), 1);
        assert_eq!(store.history_ids(), vec!["call-1"]);
        assert_eq!(store.blob_count(), 1);
        assert_eq!(store.byte_usage(), 4);
    }

    #[test]
    fn undo_blocks_on_an_unreadable_file_and_leaves_history_untouched() {
        let mut store = SnapshotStore::new();
        store
            .begin_file("call-1", "gone.txt", FileContent::Absent, present(b"ok"))
            .unwrap();
        store.commit("call-1").unwrap();
        store
            .begin_file(
                "call-2",
                "locked.txt",
                FileContent::Absent,
                FileContent::Unreadable {
                    reason: "permission denied".to_string(),
                },
            )
            .unwrap();
        store.commit("call-2").unwrap();

        let error = store.undo().unwrap_err();
        assert_eq!(
            error,
            EditSafetyError::UndoBlockedByUnreadable {
                tool_call_id: "call-2".to_string(),
                path: PathBuf::from("locked.txt"),
                reason: "permission denied".to_string(),
            }
        );
        assert_eq!(store.unit_count(), 2);
        assert_eq!(store.history_ids(), vec!["call-1", "call-2"]);
    }

    #[test]
    fn undo_stops_at_the_unit_that_holds_the_unreadable_file() {
        let mut store = SnapshotStore::new();
        store
            .begin_file(
                "call-bad",
                "locked.txt",
                FileContent::Unreadable {
                    reason: "input/output error".to_string(),
                },
                present(b"partial"),
            )
            .unwrap();
        store.commit("call-bad").unwrap();
        store
            .begin_file("call-good", "fine.txt", FileContent::Absent, present(b"ok"))
            .unwrap();
        store.commit("call-good").unwrap();

        assert_eq!(store.undo().unwrap().tool_call_id, "call-good");

        let error = store.undo().unwrap_err();
        assert!(matches!(
            error,
            EditSafetyError::UndoBlockedByUnreadable { ref tool_call_id, .. } if tool_call_id == "call-bad"
        ));
        assert_eq!(store.unit_count(), 1);
    }

    #[test]
    fn empty_and_single_byte_files_are_stored_and_restored() {
        let mut store = SnapshotStore::new();
        store
            .begin_file(
                "call-empty",
                "empty.txt",
                FileContent::Bytes(Vec::new()),
                present(b"filled"),
            )
            .unwrap();
        store.commit("call-empty").unwrap();
        store
            .begin_file("call-one", "one.txt", FileContent::Absent, present(b"x"))
            .unwrap();
        store.commit("call-one").unwrap();

        assert_eq!(store.blob_count(), 3);
        assert_eq!(store.byte_usage(), b"filled".len() + 1);
        assert_eq!(
            BlobId::of(b"").to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let newest = store.undo().unwrap();
        assert_eq!(
            newest.files,
            vec![RestoreFile {
                path: PathBuf::from("one.txt"),
                state: RestoreState::Absent,
            }]
        );

        let oldest = store.undo().unwrap();
        assert_eq!(
            oldest.files,
            vec![RestoreFile {
                path: PathBuf::from("empty.txt"),
                state: RestoreState::Bytes(Vec::new()),
            }]
        );
        assert_eq!(store.byte_usage(), 0);
        assert_eq!(store.blob_count(), 0);
    }

    #[test]
    fn duplicate_path_in_one_unit_is_rejected() {
        let mut store = SnapshotStore::new();
        store
            .begin_file("call-1", "a.txt", FileContent::Absent, present(b"one"))
            .unwrap();
        let error = store
            .begin_file("call-1", "a.txt", present(b"one"), present(b"two"))
            .unwrap_err();

        assert!(matches!(error, EditSafetyError::DuplicateFile { .. }));
        assert_eq!(store.byte_usage(), 3);
        store.commit("call-1").unwrap();
        assert_eq!(store.unit_count(), 1);
    }

    #[test]
    fn commit_and_abort_require_an_open_unit() {
        let mut store = SnapshotStore::new();

        assert!(matches!(
            store.commit("missing"),
            Err(EditSafetyError::UnknownUnit { .. })
        ));
        assert!(matches!(
            store.abort("missing"),
            Err(EditSafetyError::UnknownUnit { .. })
        ));
        assert!(matches!(store.undo(), Err(EditSafetyError::NothingToUndo)));
    }

    #[test]
    fn stores_do_not_share_state() {
        let mut first = SnapshotStore::new();
        first
            .begin_file("call-1", "a.txt", FileContent::Absent, present(b"shared?"))
            .unwrap();
        first.commit("call-1").unwrap();

        let second = SnapshotStore::new();
        assert_eq!(second.unit_count(), 0);
        assert_eq!(second.active_unit_count(), 0);
        assert_eq!(second.blob_count(), 0);
        assert_eq!(second.byte_usage(), 0);
        assert!(second.history_ids().is_empty());
        assert!(second.history().next().is_none());
    }
}
