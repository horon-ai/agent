//! In-memory snapshot journal for the file-writing tools, plus the hashline
//! codec the read and anchored-edit tools share.
//!
//! # What this crate guarantees
//!
//! - Content is addressed by SHA-256. Identical bytes are stored once, and a
//!   [`BlobId`] names exactly those bytes for as long as the store holds them.
//! - A [`MutationUnit`] covers one tool call. Committed units are evicted
//!   oldest-first as whole units when the byte budget is exceeded, and the
//!   newest unit is never evicted by its own commit, so the most recent edit
//!   always stays undoable.
//! - [`SnapshotStore::abort`] releases the unit's blobs and leaves nothing in
//!   the undo history.
//! - An undo that would cross a unit holding an unreadable file fails with
//!   [`EditSafetyError::UndoBlockedByUnreadable`] and changes nothing, so a
//!   partial restore is never reported as a success.
//! - [`tag_for`] is a pure function of the bytes it is given. Equal bytes
//!   always produce the same four lowercase hex characters, and
//!   [`format_line`] always renders `{line:>5}#{tag}:{text}`.
//!
//! # What this crate does not do
//!
//! - It never touches the filesystem. Callers read files, hand the bytes over,
//!   and apply the bytes an undo returns.
//! - It does not detect external drift. Checking that a file still matches the
//!   recorded post-state is the caller's job, using [`BlobId::of`].
//! - The budget is a soft ceiling. A single unit larger than the whole budget
//!   is still stored while it is the newest unit.
//! - History lives for one store instance and one process. Nothing is
//!   persisted, shared between stores, or replayed after a restart, and there
//!   is no redo.
//! - Active units do not count against the budget until they commit.

mod blob;
mod hashline;
mod store;

pub use blob::BlobId;
pub use hashline::{Tag, format_line, tag_for};
pub use store::{
    CommitOutcome, DEFAULT_BYTE_BUDGET, EditSafetyError, FileChange, FileContent, FileState, MutationUnit, RestoreFile,
    RestoreState, SnapshotStore, UndoOutcome,
};
