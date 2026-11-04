//! Bounded durable session-state journal with corruption recovery.
//!
//! One session owns one append-only JSONL file,
//! `<agent dir>/durable-runtime/<session_id>/runtime.jsonl`, guarded by the
//! per-session `writer.lock`. The journal records runtime bookkeeping that has
//! no home in the canonical conversation log; it never rewrites
//! `events.jsonl`, its event kinds, or the replication graph.
//!
//! # Contract
//!
//! * Every row is `{v,seq,session_id,kind,id,at,payload}` with `v` fixed at 1.
//!   `seq` is contiguous from the checkpoint that starts the file, and one
//!   state transition is exactly one row.
//! * A row is published only after `write` → file `sync_all`, and a newly
//!   created file or an atomic replacement is followed by a parent-directory
//!   sync. A caller that gets `Ok` holds a durable transition; a caller that
//!   gets `Err` holds nothing, because the write is rolled back to the previous
//!   length and re-synced. When even the rollback fails the store turns
//!   read-only so nothing later appends past an uncertain row.
//! * A tail without a newline is preserved byte-for-byte at
//!   `.recovery-<generation>` before the valid prefix is atomically rewritten,
//!   and appends resume only after that rewrite lands.
//! * A corrupt complete row, an unknown `v`, or a `seq` gap blocks both writes
//!   and automatic recovery: the store refuses to open and never falls back to
//!   an older JSON copy.
//! * **Admissions** are the rows that bring new work in: `input.accepted` and
//!   `monitor.upsert`. Every other kind is a **lifecycle row** over work that is
//!   already admitted: `input.started`, `input.committed`, `input.cancelled`,
//!   `input.returned`, `input.in_doubt`, `run.started`, `run.stopped`,
//!   `control.applying`, `control.terminal`, `monitor.checkpoint`,
//!   `monitor.stopped`, `wake.pending`, `wake.acked`, and the checkpoint the
//!   store writes.
//! * 32 MiB per file, 1,000 live inputs and 16 MiB of live input payload are
//!   hard ceilings. Only admissions are refused with busy at those ceilings, so
//!   a cancellation, abort, commit, or completion is always recordable while the
//!   queue is full. The last 1 MiB of the file is a lifecycle reserve that
//!   admissions never enter: every non-admission row may be written inside it,
//!   and admissions stop at the 31 MiB admission ceiling. Once even the reserve
//!   is exhausted the store returns read-only errors.
//! * Compaction folds the replayed state into a single checkpoint row. It keeps
//!   every piece of unfinished work: live inputs (accepted, started, returned),
//!   inputs sealed in doubt, nonterminal controls, live runs, live monitors,
//!   undelivered wakes, and the last `seq`. Terminal history is bounded instead
//!   of kept forever: a checkpoint retains the most recent
//!   [`TERMINAL_RUN_CAP`] (256) stopped runs and the most recent
//!   [`TERMINAL_RECEIPT_CAP`] (1,024) terminal input receipts, terminal
//!   controls, stopped monitors, and acknowledged wakes. 1,024 is the plan's
//!   receipt bound for control and answer receipts, so a result a client may
//!   still ask for keeps its answer; 256 runs hold the run section to a few tens
//!   of kilobytes, far below the 4 MiB compaction threshold. Every checkpoint
//!   this build writes is pruned before it is serialized, so the bound holds on
//!   disk and not only in memory. A pruned id is unknown, never a fabricated
//!   result.
//!
//! # Layout of this module
//!
//! * [`record`] owns the row schema, the typed payloads, and the runtime state
//!   they build.
//! * [`replay`] owns scanning, the state machine that applies one row, and the
//!   retention caps that bound terminal history.
//! * [`store`] owns the file, the writer lock, durability, capacity, compaction,
//!   and recovery.

mod record;
mod replay;
mod store;

pub use record::*;
pub use replay::*;
pub use store::*;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::fmt;
use std::path::Path;

/// Why the journal refused an operation.
///
/// The distinction is what callers branch on: corruption blocks the session and
/// must never be retried as a transient failure; busy is a capacity answer; and
/// read-only means the file cannot take another row at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalErrorKind {
    /// The filesystem refused a read, write, sync, rename, or removal.
    Io,
    /// The writer lock could not be taken or released.
    Lock,
    /// A complete row is unreadable, mis-shaped, or out of order with its kind.
    Corrupt,
    /// The row was written by a version this build does not understand.
    UnsupportedVersion,
    /// `seq` skipped, restarted, or does not continue from the checkpoint.
    SequenceGap,
    /// A new admission does not fit the journal or live-input budget.
    Busy,
    /// The journal exhausted its lifecycle reserve or was poisoned by a failed
    /// rollback. Nothing more can be appended.
    ReadOnly,
    /// The caller asked for a transition the state machine refuses.
    InvalidRequest,
}

impl JournalErrorKind {
    /// A stable label for logs, tests, and CLI output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Lock => "lock",
            Self::Corrupt => "corrupt",
            Self::UnsupportedVersion => "unsupported_version",
            Self::SequenceGap => "sequence_gap",
            Self::Busy => "busy",
            Self::ReadOnly => "read_only",
            Self::InvalidRequest => "invalid_request",
        }
    }
}

/// A journal failure with the reason callers branch on.
#[derive(Debug)]
pub struct JournalError {
    kind: JournalErrorKind,
    message: String,
}

impl JournalError {
    pub(crate) fn new(kind: JournalErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn corrupt(message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::Corrupt, message)
    }

    pub(crate) fn unsupported_version(message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::UnsupportedVersion, message)
    }

    pub(crate) fn sequence_gap(message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::SequenceGap, message)
    }

    pub(crate) fn busy(message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::Busy, message)
    }

    pub(crate) fn read_only(message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::ReadOnly, message)
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::InvalidRequest, message)
    }

    pub(crate) fn lock(message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::Lock, message)
    }

    pub(crate) fn io_context(what: &str, path: &Path, error: std::io::Error) -> Self {
        Self::new(
            JournalErrorKind::Io,
            format!("{what} at {} failed: {error}", path.display()),
        )
    }

    /// The reason callers branch on.
    pub fn kind(&self) -> JournalErrorKind {
        self.kind
    }

    /// The human-readable detail, without the kind label.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether this failure means the journal itself is untrustworthy.
    ///
    /// A corrupt, unknown-version, or gapped journal must not be retried as a
    /// transient error: the caller has to surface it and stop, because falling
    /// back to an older copy would silently drop acknowledged transitions.
    pub fn is_corruption(&self) -> bool {
        matches!(
            self.kind,
            JournalErrorKind::Corrupt | JournalErrorKind::UnsupportedVersion | JournalErrorKind::SequenceGap
        )
    }
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for JournalError {}

impl From<std::io::Error> for JournalError {
    fn from(error: std::io::Error) -> Self {
        Self::new(JournalErrorKind::Io, error.to_string())
    }
}

/// The result of one journal operation.
pub type JournalResult<T> = Result<T, JournalError>;
