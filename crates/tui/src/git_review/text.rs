//! The lines `/diff` and `/commit` leave, and the two commands that leave them.
//!
//! The texts are plain functions over what the git layer read, so a test can
//! assert what a review or a diff says without a terminal, and the handlers only
//! decide which text belongs on screen and which review is kept for `/commit
//! --yes`.

use std::path::PathBuf;

use crate::app::{Action, App};

use super::{Committed, Diffs, Refusal, Review, Scope, Section, commit, diffs, review, short_id};

/// Most products of a listing drawn before the rest is summarized.
const LIST_ROWS: usize = 24;

/// `branch <name>`, or the two words a detached `HEAD` earns.
pub(crate) fn branch_label(branch: Option<&str>) -> String {
    match branch {
        Some(name) => format!("branch {name}"),
        None => "detached HEAD".to_string(),
    }
}

/// The text `/diff` prints: one section per side, and the untracked paths.
pub(crate) fn diff_text(diffs: &Diffs) -> String {
    let mut lines = vec![format!(
        "diff · {} · {}",
        diffs.root.display(),
        branch_label(diffs.branch.as_deref())
    )];
    if let Some(section) = &diffs.staged {
        push_section(&mut lines, "staged (what /commit would include)", section);
    }
    if let Some(section) = &diffs.unstaged {
        push_section(&mut lines, "unstaged (left alone by /commit)", section);
        if !diffs.untracked.is_empty() || diffs.untracked_more {
            let mut row = format!(
                "untracked, in neither diff ({}): {}",
                diffs.untracked.len(),
                diffs.untracked.join(", ")
            );
            if diffs.untracked_more {
                row.push_str(", …");
            }
            lines.push(row);
        }
    }
    lines.join("\n")
}

/// One section of `/diff`: the summary, the paths and the patch.
fn push_section(lines: &mut Vec<String>, name: &str, section: &Section) {
    lines.push(format!("{name}:"));
    if section.files.is_empty() {
        lines.push("  nothing".to_string());
        lines.push(format!("--- {name} diff: none ---"));
        return;
    }
    lines.push(format!("  {}", section.summary));
    for change in section.files.iter().take(LIST_ROWS) {
        lines.push(format!("  {}", change.line()));
    }
    if section.files.len() > LIST_ROWS {
        lines.push(format!("  … {} more", section.files.len() - LIST_ROWS));
    }
    lines.push(format!("--- {name} diff ---"));
    if section.patch.is_empty() {
        lines.push("  (no text diff; the change is binary or only a mode)".to_string());
    } else {
        lines.extend(section.patch.lines().map(str::to_string));
    }
    lines.push(format!("--- end of {name} diff ---"));
    if section.truncated {
        lines.push("  (the diff is longer than this pane keeps; read it with git locally)".to_string());
    }
}

/// The text `/commit <message>` prints: the exact set, and what commits it.
pub(crate) fn review_text(review: &Review) -> String {
    let mut lines = vec![format!(
        "commit review {} · tree {} · {} · branch {}",
        review.id,
        short_id(&review.tree),
        review.root.display(),
        review.branch
    )];
    let on_top_of = match review.head.as_deref() {
        Some(head) => format!(" on top of {}", short_id(head)),
        None => " as the first commit".to_string(),
    };
    lines.push(format!(
        "staged, exactly these {} file(s){on_top_of}:",
        review.files.len()
    ));
    for change in &review.files {
        lines.push(format!("  {}", change.line()));
    }
    lines.push(format!("message: {}", review.message.replace('\n', " ⏎ ")));
    lines.push("unstaged changes are not in this commit and are left alone.".to_string());
    lines.push(
        "repository hooks are not run: this commit is written from the tree above with git plumbing.".to_string(),
    );
    lines.push(
        "nothing was committed yet. `/commit --yes` commits exactly these files; `/commit --cancel` discards the review."
            .to_string(),
    );
    lines.push(
        "if the index, HEAD or the branch changes first, the commit refuses; `/commit <message>` reviews the new set."
            .to_string(),
    );
    lines.join("\n")
}

/// The text a finished commit leaves.
///
/// The caveats in `Committed::notes` are warnings, not part of this line:
/// they name something the review did not say, and the caller draws them where
/// warnings go.
pub(crate) fn commit_text(committed: &Committed) -> String {
    vec![
        format!(
            "committed {} on {}: {}",
            short_id(&committed.id),
            branch_label(committed.branch.as_deref()),
            committed.subject
        ),
        format!(
            "  {} file(s), as reviewed; not pushed, because this client never pushes",
            committed.files
        ),
    ]
    .join("\n")
}

/// The line `/diff <scope>` prints for a scope nothing names.
pub(crate) fn diff_usage() -> String {
    "`/diff` shows both sides, `/diff staged` the index and `/diff unstaged` the working tree".to_string()
}

/// The line `/commit` prints when it has no message or an unknown flag.
pub(crate) fn commit_usage() -> String {
    "`/commit <message>` previews the staged set with that message, `/commit --yes` commits exactly it, `/commit --cancel` discards the review".to_string()
}

impl App {
    /// The directory git should describe, or why there is none.
    ///
    /// A session that runs behind an explicit socket or a remote bridge reports
    /// a directory on the other machine, and one the daemon never named leaves
    /// only the launch directory, which is not known to be the workspace. Both
    /// are refused before git is started, so a local repository that happens to
    /// share a name is never read for a session that does not work there.
    fn git_directory(&self) -> Result<PathBuf, Refusal> {
        if !self.workspace_is_local {
            return Err(Refusal::RemoteSession);
        }
        if !self.workspace_known {
            return Err(Refusal::UnknownWorkspace);
        }
        match self.cwd.as_deref() {
            Some(directory) if !directory.is_empty() => Ok(PathBuf::from(directory)),
            _ => Err(Refusal::UnknownWorkspace),
        }
    }

    /// Answer `/diff`.
    pub(crate) fn run_diff(&mut self, arguments: &str) -> Option<Action> {
        let scope = match arguments.trim() {
            "" | "all" => Scope::Both,
            "staged" | "index" => Scope::Staged,
            "unstaged" | "worktree" => Scope::Unstaged,
            other => {
                self.warn(format!("diff: `{other}` is not a scope; {}", diff_usage()));
                return None;
            }
        };
        let directory = match self.git_directory() {
            Ok(directory) => directory,
            Err(refusal) => {
                self.warn(format!("diff: {refusal}"));
                return None;
            }
        };
        match diffs(&directory, scope) {
            Ok(found) => {
                self.transcript.push_notice(diff_text(&found));
                // The sections are taller than most panes, so the next frame
                // measures the new block and starts it at the top rather than
                // drawing its end.
                self.scroll.hold_newest();
            }
            Err(refusal) => self.warn(format!("diff: {refusal}")),
        }
        None
    }

    /// Answer `/commit`.
    pub(crate) fn run_commit(&mut self, arguments: &str) -> Option<Action> {
        let trimmed = arguments.trim();
        match trimmed {
            "" => {
                self.warn(format!("commit: {}", commit_usage()));
                return None;
            }
            "--cancel" => {
                if self.commit_review.take().is_some() {
                    self.transcript
                        .push_notice("commit: the review was discarded; nothing was committed");
                } else {
                    self.warn("commit: no review is open, so there is nothing to cancel");
                }
                return None;
            }
            "--yes" => {
                let Some(review) = self.commit_review.take() else {
                    self.warn(format!("commit: {}", Refusal::NoReview));
                    return None;
                };
                match commit(&review) {
                    Ok(committed) => {
                        self.transcript.push_notice(commit_text(&committed));
                        // The review above may be longer than the pane, so the
                        // line that says what happened is followed to its end.
                        self.scroll.to_bottom();
                        for note in &committed.notes {
                            self.warn(format!("commit: {note}"));
                        }
                    }
                    Err(refusal) => {
                        self.warn(format!("commit: {refusal}"));
                        // A refusal is the answer to the key just pressed; it
                        // is read at the end of the review it names.
                        self.scroll.to_bottom();
                    }
                }
                return None;
            }
            other if other.starts_with("--yes") || other.starts_with("--cancel") => {
                // The two confirmation words take nothing after them; text
                // typed there was meant as a message, and taking it as a flag
                // would confirm a review the user was still describing.
                self.warn(format!(
                    "commit: `--yes` and `--cancel` take no text; {}",
                    commit_usage()
                ));
                return None;
            }
            other if other.starts_with("--") => {
                // A flag that is not one of the two above would otherwise
                // become a commit message that looks like an instruction.
                self.warn(format!("commit: `{other}` is not a flag; {}", commit_usage()));
                return None;
            }
            _ => {}
        }
        if self.commit_review.is_some() {
            self.warn(format!("commit: {}", Refusal::ReviewOpen));
            return None;
        }
        let directory = match self.git_directory() {
            Ok(directory) => directory,
            Err(refusal) => {
                self.warn(format!("commit: {refusal}"));
                return None;
            }
        };
        match review(&directory, trimmed) {
            Ok(review) => {
                self.transcript.push_notice(review_text(&review));
                self.scroll.hold_newest();
                self.commit_review = Some(review);
            }
            Err(refusal) => self.warn(format!("commit: {refusal}")),
        }
        None
    }
}
