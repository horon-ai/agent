//! The deferred commit a restored session owes before it may take new work.
//!
//! A restore prepares the session in memory and hands back the saved transcript
//! first; the write that makes the restored state durable runs afterwards. Until
//! it lands the session is "owed" a commit. Every save of the owning [`Agent`]
//! goes through [`Agent::save_session`], so the first save after a restore *is*
//! that commit, and the process-wide registry lets server handlers that hold no
//! agent lock see whether a session still owes one.

use super::Agent;
use anyhow::Result;
use std::collections::HashSet;
use std::sync::LazyLock;

static OWED_RESUME_COMMITS: LazyLock<std::sync::Mutex<HashSet<String>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

fn owed_resume_commits() -> std::sync::MutexGuard<'static, HashSet<String>> {
    // Plain registry data: a panic while it is held leaves no half-applied
    // invariant, so recovering from the poison keeps later resumes working.
    OWED_RESUME_COMMITS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a restored session still owes its resume commit.
pub(crate) fn resume_commit_owed(session_id: &str) -> bool {
    owed_resume_commits().contains(session_id)
}

/// The resume commit a restored session still owes.
///
/// Owning this value is owning the registry entry: dropping it (cleared,
/// replaced, or with its Agent) removes the id, so the registry never names a
/// session whose owner is gone.
#[derive(Debug)]
pub(crate) struct PendingResumeCommit {
    pub(crate) session_id: String,
    /// The resume environment event is part of the commit; a retried commit
    /// must not record it twice.
    pub(crate) env_snapshot_recorded: bool,
}

impl Drop for PendingResumeCommit {
    fn drop(&mut self) {
        owed_resume_commits().remove(&self.session_id);
    }
}

/// A resume commit failed; the session stays owed and the caller may retry.
#[derive(Debug)]
pub(crate) struct ResumeCommitFailed {
    pub(crate) session_id: String,
}

impl std::fmt::Display for ResumeCommitFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "resumed session {} is not durable yet; retry", self.session_id)
    }
}

impl std::error::Error for ResumeCommitFailed {}

impl Agent {
    /// Open the barrier for the session just restored into this Agent.
    pub(super) fn begin_pending_resume(&mut self) {
        // Drop any earlier entry before registering, so its removal cannot
        // erase the new one when both name the same session.
        self.pending_resume = None;
        owed_resume_commits().insert(self.session.id.clone());
        self.pending_resume = Some(PendingResumeCommit {
            session_id: self.session.id.clone(),
            env_snapshot_recorded: false,
        });
    }

    /// Release the owed commit without writing it.
    pub(super) fn clear_pending_resume(&mut self) {
        self.pending_resume = None;
    }

    /// Save this Agent's session. While a resume commit is owed, the save is
    /// that commit: it writes the whole session, including the mutation that
    /// asked for it, and fires `session_start` once.
    pub(crate) fn save_session(&mut self) -> Result<()> {
        if self.pending_resume.is_some() {
            return self.finish_resume_persistence();
        }
        self.session.save()
    }
}
