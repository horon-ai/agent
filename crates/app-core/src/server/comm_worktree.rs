//! Checkout isolation for spawned swarm children.
//!
//! `prepare` creates a private checkout on branch `swarm/<session id>` from the
//! parent's HEAD and returns its path; the spawner runs the child with that
//! path as its working directory, so the child never edits the parent's
//! checkout while it runs. Two backends exist: `worktree` adds a linked git
//! worktree, and `clone` copies the whole repository with a copy-on-write
//! filesystem clone, which also carries ignored build caches. At settle time
//! `apply` commits whatever the child left uncommitted with the caller's
//! message, merges the branch into the parent, and reports the merge commit; a
//! conflict aborts the merge, leaving the parent's index and files as they were
//! and keeping the checkout for inspection. `keep` leaves the checkout and
//! branch in place and reports the path plus a diffstat; `drop` removes both,
//! refusing a checkout that still holds uncommitted changes. A child whose turn
//! failed is never merged.
//!
//! Every checkout carries an owner marker, and `prepare` sweeps orphans whose
//! owning process died and whose checkout is clean, so `keep`, a conflict, or a
//! failed settle does not accumulate checkouts forever.
//!
//! What this module does NOT do: it does not resolve a conflict, and it never
//! touches the parent's index or working tree except through `apply`'s merge.

use anyhow::{Context, Result, bail, ensure};
use horon_agent_base::git_clone_isolation;
use horon_agent_base::git_worktree::{self, MergeOutcome};
use std::path::Path;

/// Where a spawned child runs. `Worktree` gives it a private linked git
/// worktree; `Clone` gives it a copy-on-write clone of the repository.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Isolation {
    #[default]
    None,
    Worktree,
    Clone,
}

impl Isolation {
    pub(super) fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("none") => Ok(Self::None),
            Some("worktree") => Ok(Self::Worktree),
            Some("clone") => Ok(Self::Clone),
            Some(other) => bail!("Unknown isolation '{other}': expected 'none', 'worktree', or 'clone'"),
        }
    }

    /// The isolation's request spelling, for error messages.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Worktree => "worktree",
            Self::Clone => "clone",
        }
    }

    /// The report prefix used for this backend.
    fn report_prefix(self) -> &'static str {
        match self {
            Self::Clone => "Clone isolation",
            Self::None | Self::Worktree => "Worktree isolation",
        }
    }
}

/// What happens to a worktree child's work once its turn finishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum IsoApply {
    #[default]
    Apply,
    Keep,
    Drop,
}

impl IsoApply {
    pub(super) fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("apply") => Ok(Self::Apply),
            Some("keep") => Ok(Self::Keep),
            Some("drop") => Ok(Self::Drop),
            Some(other) => bail!("Unknown iso_apply '{other}': expected 'apply', 'keep', or 'drop'"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WorktreeChild {
    /// Which backend produced this checkout.
    pub isolation: Isolation,
    /// The directory the child runs in.
    pub path: String,
    /// The checkout's repository root; for a clone this differs from `path`
    /// when the parent ran from a subdirectory.
    pub checkout: String,
    pub branch: String,
    pub parent_working_dir: String,
    /// HEAD in the parent checkout when this child was created; diffstats and
    /// the orphan sweep measure against it.
    pub base_head: String,
    pub apply: IsoApply,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WorktreeReport {
    Applied {
        branch: String,
        commit: String,
    },
    NothingToApply {
        branch: String,
    },
    Kept {
        path: String,
        branch: String,
        diff_stat: Option<String>,
    },
    Dropped,
    Conflict {
        path: String,
        branch: String,
        files: Vec<String>,
        message: String,
    },
    Dirty {
        path: String,
        reason: String,
    },
}

impl WorktreeReport {
    pub(super) fn describe_with(&self, prefix: &str) -> String {
        match self {
            Self::Applied { branch, commit } => {
                format!("{prefix}: merged branch {branch} into the parent worktree as {commit}.")
            }
            Self::NothingToApply { branch } => {
                format!("{prefix}: branch {branch} had no changes to merge.")
            }
            Self::Kept {
                path,
                branch,
                diff_stat,
            } => {
                let diff = match diff_stat {
                    Some(diff) => format!("\n{diff}"),
                    None => String::new(),
                };
                format!(
                    "{prefix}: left branch {branch} checked out at {path} for inspection; nothing was merged.{diff}"
                )
            }
            Self::Dropped => format!("{prefix}: removed the child checkout; its changes were discarded."),
            Self::Conflict {
                path,
                branch,
                files,
                message,
            } => {
                let files = if files.is_empty() {
                    "unreported files".to_string()
                } else {
                    files.join(", ")
                };
                format!(
                    "{prefix}: merging branch {branch} conflicted ({message}); the merge was aborted so the \
                     parent is unchanged. Conflicted files: {files}. The checkout is kept at {path} for inspection."
                )
            }
            Self::Dirty { path, reason } => {
                format!("{prefix}: {reason} The checkout is kept at {path} with its changes intact.")
            }
        }
    }

    pub(super) fn status_detail(&self) -> String {
        match self {
            Self::Applied { .. } => "worktree merged".to_string(),
            Self::NothingToApply { .. } => "worktree had no changes".to_string(),
            Self::Kept { .. } => "checkout kept".to_string(),
            Self::Dropped => "checkout dropped".to_string(),
            Self::Conflict { .. } => "checkout merge conflicted".to_string(),
            Self::Dirty { .. } => "checkout left dirty".to_string(),
        }
    }
}

fn branch_for(session_id: &str) -> String {
    format!("swarm/{session_id}")
}

pub(super) fn prepare(
    parent_working_dir: &str,
    session_id: &str,
    isolation: Isolation,
    apply: IsoApply,
) -> Result<WorktreeChild> {
    ensure!(
        isolation != Isolation::None,
        "Checkout isolation needs the 'worktree' or 'clone' backend"
    );
    ensure!(
        Path::new(parent_working_dir).is_dir(),
        "{} needs an existing working directory, got '{parent_working_dir}'",
        isolation.report_prefix()
    );
    if let Err(error) = git_clone_isolation::sweep_orphans(parent_working_dir) {
        crate::logging::warn(&format!("SWARM_ISOLATION_SWEEP_FAILED error={error}"));
    }
    let branch = branch_for(session_id);
    match isolation {
        Isolation::Clone => {
            let created = git_clone_isolation::create(parent_working_dir, &branch)
                .with_context(|| format!("Failed to create an isolated clone for {session_id}"))?;
            write_marker(Path::new(&created.root), "clone", &created.base_head, session_id);
            Ok(WorktreeChild {
                isolation,
                path: created.working_dir,
                checkout: created.root,
                branch,
                parent_working_dir: parent_working_dir.to_string(),
                base_head: created.base_head,
                apply,
            })
        }
        _ => {
            let base_head = git_worktree::head(parent_working_dir)?;
            let created = git_worktree::create(parent_working_dir, &branch)
                .with_context(|| format!("Failed to create an isolated worktree for {session_id}"))?;
            write_marker(Path::new(&created.path), "worktree", &base_head, session_id);
            Ok(WorktreeChild {
                isolation: Isolation::Worktree,
                path: created.path.clone(),
                checkout: created.path,
                branch,
                parent_working_dir: parent_working_dir.to_string(),
                base_head,
                apply,
            })
        }
    }
}

/// A marker failure only weakens orphan reaping, so it must not fail a spawn.
fn write_marker(checkout: &Path, kind: &str, base_head: &str, session_id: &str) {
    if let Err(error) = git_clone_isolation::write_owner_marker(checkout, kind, base_head, session_id) {
        crate::logging::warn(&format!(
            "SWARM_ISOLATION_MARKER_FAILED path={} error={error}",
            checkout.display()
        ));
    }
}

fn is_dirty(child: &WorktreeChild) -> Result<bool> {
    git_worktree::is_dirty(&child.path)
}

fn child_diff_stat(child: &WorktreeChild) -> Option<String> {
    git_worktree::diff_stat(&child.checkout, &child.base_head)
        .ok()
        .filter(|stat| !stat.trim().is_empty())
}

/// Remove a child's checkout and, for a linked worktree, its branch. The clone
/// backend keeps its branch inside the clone, so removing the directory removes
/// it too.
fn discard_checkout(child: &WorktreeChild) -> Result<()> {
    match child.isolation {
        Isolation::Clone => git_clone_isolation::remove(Path::new(&child.checkout)),
        _ => {
            git_worktree::remove(&child.parent_working_dir, &child.path)?;
            git_worktree::delete_branch(&child.parent_working_dir, &child.branch)
        }
    }
}

/// `apply` is the only mode that touches the parent, and only through a merge:
/// a conflict aborts it, so the parent's index and files stay as they were.
pub(super) fn finish(child: &WorktreeChild, commit_message: &str) -> Result<WorktreeReport> {
    match child.apply {
        IsoApply::Keep => Ok(WorktreeReport::Kept {
            path: child.path.clone(),
            branch: child.branch.clone(),
            diff_stat: child_diff_stat(child),
        }),
        IsoApply::Drop => {
            if is_dirty(child)? {
                return Ok(WorktreeReport::Dirty {
                    path: child.path.clone(),
                    reason: "refusing to drop a checkout that still has uncommitted changes.".to_string(),
                });
            }
            discard_checkout(child)?;
            Ok(WorktreeReport::Dropped)
        }
        IsoApply::Apply => match child.isolation {
            Isolation::Clone => apply_clone(child, commit_message),
            _ => apply_worktree(child, commit_message),
        },
    }
}

fn apply_worktree(child: &WorktreeChild, commit_message: &str) -> Result<WorktreeReport> {
    let commit = git_worktree::commit_all(&child.path, commit_message)?;
    let Some(commit) = commit else {
        discard_checkout(child)?;
        return Ok(WorktreeReport::NothingToApply {
            branch: child.branch.clone(),
        });
    };
    match git_worktree::merge_branch(&child.parent_working_dir, &child.branch)? {
        MergeOutcome::Clean { commit: merged } => {
            discard_checkout(child)?;
            Ok(WorktreeReport::Applied {
                branch: child.branch.clone(),
                commit: merged,
            })
        }
        MergeOutcome::Conflict { files, message } => Ok(WorktreeReport::Conflict {
            path: child.path.clone(),
            branch: child.branch.clone(),
            files,
            message: format!("{message} (child commit {commit})"),
        }),
    }
}

fn apply_clone(child: &WorktreeChild, commit_message: &str) -> Result<WorktreeReport> {
    let commit = git_worktree::commit_all(&child.path, commit_message)?;
    let Some(commit) = commit else {
        discard_checkout(child)?;
        return Ok(WorktreeReport::NothingToApply {
            branch: child.branch.clone(),
        });
    };
    // The clone's branch only exists inside the clone; fetch it into the parent
    // repository first so the merge has something to merge.
    git_clone_isolation::fetch_branch(&child.parent_working_dir, Path::new(&child.checkout), &child.branch)?;
    match git_worktree::merge_branch(&child.parent_working_dir, &child.branch)? {
        MergeOutcome::Clean { commit: merged } => {
            git_worktree::delete_branch(&child.parent_working_dir, &child.branch)?;
            discard_checkout(child)?;
            Ok(WorktreeReport::Applied {
                branch: child.branch.clone(),
                commit: merged,
            })
        }
        MergeOutcome::Conflict { files, message } => Ok(WorktreeReport::Conflict {
            path: child.path.clone(),
            branch: child.branch.clone(),
            files,
            message: format!("{message} (child commit {commit})"),
        }),
    }
}

fn with_report_text(completion_report: Option<String>, report: &WorktreeReport, prefix: &str) -> String {
    match completion_report {
        Some(text) if !text.trim().is_empty() => format!("{}\n\n{}", text.trim_end(), report.describe_with(prefix)),
        _ => report.describe_with(prefix),
    }
}

/// A child whose turn did not succeed is never merged: its checkout is left
/// exactly as the child left it, and the report says so.
pub(super) fn failure_report(child: &WorktreeChild, completion_report: Option<String>) -> String {
    let report = WorktreeReport::Dirty {
        path: child.path.clone(),
        reason: format!(
            "the child did not finish successfully, so nothing was merged (branch {}).",
            child.branch
        ),
    };
    crate::logging::info(&format!(
        "SWARM_WORKTREE_SETTLE branch={} apply={:?} result={}",
        child.branch,
        child.apply,
        report.status_detail()
    ));
    with_report_text(completion_report, &report, child.isolation.report_prefix())
}

/// Removes a freshly prepared checkout if the spawn fails before the child owns
/// it. A dirty checkout is never removed.
pub(super) struct WorktreeGuard(Option<WorktreeChild>);

impl WorktreeGuard {
    pub(super) fn new(child: WorktreeChild) -> Self {
        Self(Some(child))
    }

    pub(super) fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.take()
            && !is_dirty(&child).unwrap_or(true)
            && discard_checkout(&child).is_ok()
        {
            crate::logging::info(&format!("SWARM_WORKTREE_ABANDONED branch={}", child.branch));
        }
    }
}

pub(super) fn settle(child: &WorktreeChild, commit_message: &str, completion_report: Option<String>) -> String {
    let prefix = child.isolation.report_prefix();
    let report = match finish(child, commit_message) {
        Ok(report) => report,
        Err(error) => {
            let detail = format!(
                "{prefix}: settling the checkout at {} failed: {error}. Nothing was merged into the parent.",
                child.path
            );
            crate::logging::warn(&format!(
                "SWARM_WORKTREE_SETTLE_FAILED path={} error={error}",
                child.path
            ));
            WorktreeReport::Dirty {
                path: child.path.clone(),
                reason: detail,
            }
        }
    };
    crate::logging::info(&format!(
        "SWARM_WORKTREE_SETTLE branch={} apply={:?} result={}",
        child.branch,
        child.apply,
        report.status_detail()
    ));
    with_report_text(completion_report, &report, prefix)
}

#[cfg(test)]
#[path = "comm_worktree_tests.rs"]
mod worktree_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_isolation_and_apply_with_defaults() {
        assert_eq!(Isolation::parse(None).unwrap(), Isolation::None);
        assert_eq!(Isolation::parse(Some("")).unwrap(), Isolation::None);
        assert_eq!(Isolation::parse(Some("none")).unwrap(), Isolation::None);
        assert_eq!(Isolation::parse(Some("worktree")).unwrap(), Isolation::Worktree);
        assert_eq!(Isolation::parse(Some("clone")).unwrap(), Isolation::Clone);
        let error = Isolation::parse(Some("container")).unwrap_err().to_string();
        assert!(error.contains("'none', 'worktree', or 'clone'"), "{error}");
        assert_eq!(IsoApply::parse(None).unwrap(), IsoApply::Apply);
        assert_eq!(IsoApply::parse(Some("apply")).unwrap(), IsoApply::Apply);
        assert_eq!(IsoApply::parse(Some("keep")).unwrap(), IsoApply::Keep);
        assert_eq!(IsoApply::parse(Some("drop")).unwrap(), IsoApply::Drop);
        assert!(IsoApply::parse(Some("merge")).is_err());
    }

    #[test]
    fn conflict_report_names_files_and_kept_path() {
        let report = WorktreeReport::Conflict {
            path: "/tmp/wt".to_string(),
            branch: "swarm/child".to_string(),
            files: vec!["a.rs".to_string()],
            message: "CONFLICT".to_string(),
        };
        let text = report.describe_with("Worktree isolation");
        assert!(text.contains("a.rs"));
        assert!(text.contains("/tmp/wt"));
        assert!(text.contains("parent is unchanged"));
    }
}
