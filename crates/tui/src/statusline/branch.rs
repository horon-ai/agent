//! The git branch the status line names, read from the repository's `HEAD`.
//!
//! No git process is started: the branch is the `ref: refs/heads/<name>` line
//! of the `HEAD` file of the nearest repository above the working directory,
//! and a detached head is named by its short commit. The answer is kept for a
//! couple of seconds per directory, so a frame drawn while text streams reads
//! no file.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long one reading of `HEAD` is trusted.
const FRESH: Duration = Duration::from_secs(2);

/// Characters of a detached head's commit that are shown.
const SHORT_COMMIT: usize = 7;

thread_local! {
    static CACHE: RefCell<Option<(String, Instant, Option<String>)>> = const { RefCell::new(None) };
}

/// The branch of the repository `cwd` is in, or `None` outside one.
pub(crate) fn git_branch(cwd: Option<&str>) -> Option<String> {
    let cwd = cwd?;
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((dir, read_at, branch)) = cache.as_ref()
            && dir == cwd
            && read_at.elapsed() < FRESH
        {
            return branch.clone();
        }
        let branch = read_branch(Path::new(cwd));
        *cache = Some((cwd.to_string(), Instant::now(), branch.clone()));
        branch
    })
}

/// Read the branch from the nearest repository at or above `start`.
fn read_branch(start: &Path) -> Option<String> {
    let head = head_file(start)?;
    // An unreadable `HEAD` names no branch, which hides the segment.
    let text = match std::fs::read_to_string(head) {
        Ok(text) => text,
        Err(_) => return None,
    };
    branch_of(&text)
}

/// The `HEAD` file of the nearest repository at or above `start`.
///
/// A worktree or a submodule has a `.git` file naming the real git directory.
fn head_file(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git.join("HEAD"));
        }
        if dot_git.is_file() {
            let text = match std::fs::read_to_string(&dot_git) {
                Ok(text) => text,
                Err(_) => return None,
            };
            let target = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?.trim();
            let git_dir = Path::new(target);
            let git_dir = if git_dir.is_absolute() {
                git_dir.to_path_buf()
            } else {
                dir.join(git_dir)
            };
            return Some(git_dir.join("HEAD"));
        }
    }
    None
}

/// The branch a `HEAD` file names, or its short commit when it is detached.
fn branch_of(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return (!name.is_empty()).then(|| name.to_string());
    }
    let commit: String = head.chars().take(SHORT_COMMIT).collect();
    (commit.len() == SHORT_COMMIT && commit.chars().all(|character| character.is_ascii_hexdigit())).then_some(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_names_the_branch_or_the_detached_commit() {
        assert_eq!(branch_of("ref: refs/heads/feat/tui\n").as_deref(), Some("feat/tui"));
        assert_eq!(
            branch_of("42f323623a1b2c3d4e5f60718293a4b5c6d7e8f9\n").as_deref(),
            Some("42f3236")
        );
        assert_eq!(branch_of("garbage"), None);
    }

    #[test]
    fn a_worktree_points_at_its_own_head() {
        let root = tempfile::tempdir().expect("dir");
        let real = root.path().join("main/.git/worktrees/wt");
        std::fs::create_dir_all(&real).expect("git dir");
        std::fs::write(real.join("HEAD"), "ref: refs/heads/wt-branch\n").expect("head");
        let worktree = root.path().join("wt");
        std::fs::create_dir_all(worktree.join("src")).expect("worktree");
        std::fs::write(worktree.join(".git"), format!("gitdir: {}\n", real.display())).expect("gitdir");
        assert_eq!(read_branch(&worktree.join("src")).as_deref(), Some("wt-branch"));

        let plain = tempfile::tempdir().expect("dir");
        // A directory outside any repository names no branch, unless the
        // temporary directory itself sits in one.
        if head_file(plain.path()).is_none() {
            assert_eq!(read_branch(plain.path()), None);
        }
    }
}
