//! Blocking local Git worktree operations, re-exported from `horon-agent-base`.
//!
//! Creation checks out a new branch from the invoking worktree's HEAD, without
//! copying staged, unstaged, or untracked changes. Existing branches and paths
//! are never reused. Git must be available on PATH.

pub use horon_agent_base::git_worktree::{
    MergeOutcome, Worktree, commit_all, create, delete_branch, is_dirty, list, merge_branch, prune, remove,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_surface_creates_merges_and_removes_a_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let repo = repo.to_str().unwrap().to_string();
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.name", "SDK Test"]);
        run(&["config", "user.email", "sdk-test@example.invalid"]);
        std::fs::write(std::path::Path::new(&repo).join("tracked"), "original").unwrap();
        run(&["add", "tracked"]);
        run(&["commit", "-m", "initial"]);

        let created = create(&repo, "sdk/one").unwrap();
        assert_eq!(created.branch.as_deref(), Some("refs/heads/sdk/one"));
        assert_eq!(list(&repo).unwrap().len(), 2);
        std::fs::write(std::path::Path::new(&created.path).join("tracked"), "changed").unwrap();
        assert!(is_dirty(&created.path).unwrap());
        commit_all(&created.path, "sdk child").unwrap();
        assert!(matches!(
            merge_branch(&repo, "sdk/one").unwrap(),
            MergeOutcome::Clean { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&repo).join("tracked")).unwrap(),
            "changed"
        );
        remove(&repo, &created.path).unwrap();
        delete_branch(&repo, "sdk/one").unwrap();
        prune(&repo).unwrap();
        assert_eq!(list(&repo).unwrap().len(), 1);
    }
}
