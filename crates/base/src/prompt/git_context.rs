//! The git lines of a session context: the current branch and a short status
//! summary of the session's working directory.
//!
//! A directory outside any repository starts no process: a walk up the
//! directory tree answers that from the filesystem. A work tree costs one
//! `git status --porcelain=v1 --branch`, whose header names the branch and
//! whose remaining lines are the changed paths. Everything the walk cannot
//! vouch for (a bare repository, a directory inside a git directory, a
//! repository selected through the environment, a status call that failed)
//! takes the three-call path this module started with, so those cases keep
//! exactly the lines they always produced.

use std::path::Path;
use std::process::Command;

/// How the git lines of one directory are obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    /// No repository encloses the directory, so no process is started.
    NoRepository,
    /// A work tree: one `git status` call.
    WorkTree,
    /// Only git can tell: the three-call path.
    AskGit,
}

/// The status call did not answer, so the three-call path decides.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StatusFailed;

/// Get git branch and status summary
pub(super) fn get_git_info(working_dir: Option<&Path>) -> Option<String> {
    let selected_by_environment = environment_selects_repository(|name| std::env::var_os(name).is_some());
    match route(working_dir, selected_by_environment) {
        Route::NoRepository => None,
        Route::WorkTree => match git_info_from_status(working_dir) {
            Ok(info) => info,
            Err(StatusFailed) => git_info_by_three_calls(working_dir),
        },
        Route::AskGit => git_info_by_three_calls(working_dir),
    }
}

/// Whether the environment, not the directory, names the repository. Git then
/// ignores what a walk up from the directory would find.
pub(super) fn environment_selects_repository(is_set: impl Fn(&str) -> bool) -> bool {
    ["GIT_DIR", "GIT_WORK_TREE"].into_iter().any(is_set)
}

/// Decides from the filesystem alone how `working_dir` is to be read.
///
/// The walk follows git's own discovery: the first ancestor holding a `.git`
/// entry is a work tree, and an ancestor that is itself a git directory is a
/// bare repository or the inside of one. A `.git` entry git would refuse (a
/// discovery ceiling, a foreign owner, a broken link) still routes to the
/// status call, whose failure falls back to the three-call path.
pub(super) fn route(working_dir: Option<&Path>, selected_by_environment: bool) -> Route {
    if selected_by_environment {
        return Route::AskGit;
    }
    // Git discovers from the physical directory, so links are resolved first.
    let start = match working_dir {
        Some(dir) => std::fs::canonicalize(dir),
        None => std::env::current_dir(),
    };
    let Ok(start) = start else {
        return Route::AskGit;
    };
    for dir in start.ancestors() {
        if dir.join(".git").symlink_metadata().is_ok() {
            return Route::WorkTree;
        }
        if is_git_directory(dir) {
            return Route::AskGit;
        }
    }
    Route::NoRepository
}

fn is_git_directory(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

fn git_in(working_dir: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }
    command
}

/// The git lines of a work tree from one status call. `Ok(None)` is a
/// repository with nothing to report: a detached HEAD and a clean tree.
pub(super) fn git_info_from_status(working_dir: Option<&Path>) -> Result<Option<String>, StatusFailed> {
    horon_agent_perf_counters::record_session_context_git_spawn();
    let Ok(output) = git_in(working_dir)
        .args(["status", "--porcelain=v1", "--branch"])
        .output()
    else {
        return Err(StatusFailed);
    };
    if !output.status.success() {
        return Err(StatusFailed);
    }
    let lossy = String::from_utf8_lossy(&output.stdout);
    let text: &str = &lossy;
    let (header, entries) = text.split_once('\n').unwrap_or((text, ""));
    let Some(header) = header.trim_end().strip_prefix("## ") else {
        return Err(StatusFailed);
    };

    let mut info = vec!["Git:".to_string()];
    if let Some(branch) = branch_from_header(header) {
        info.push(format!("  Branch: {}", branch));
    }
    push_modified(&mut info, entries);
    Ok(finish(info))
}

/// The branch a `## ` status header names, as `git branch --show-current`
/// prints it: nothing for a detached HEAD, the name for a branch with or
/// without commits. Porcelain output is never translated, and a branch name
/// holds neither a space nor `..`, so the fixed texts cannot be a branch.
pub(super) fn branch_from_header(header: &str) -> Option<&str> {
    if header == "HEAD (no branch)" {
        return None;
    }
    let name = header
        .strip_prefix("No commits yet on ")
        .or_else(|| header.strip_prefix("Initial commit on "))
        .unwrap_or(header);
    let name = name.split_once("...").map_or(name, |(name, _upstream)| name);
    (!name.is_empty()).then_some(name)
}

/// The three-call path: is this a repository, which branch, which changes.
pub(super) fn git_info_by_three_calls(working_dir: Option<&Path>) -> Option<String> {
    // Check if we're in a git repo
    horon_agent_perf_counters::record_session_context_git_spawn();
    let in_repo = git_in(working_dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !in_repo {
        return None;
    }

    let mut info = vec!["Git:".to_string()];

    // Current branch
    horon_agent_perf_counters::record_session_context_git_spawn();
    if let Ok(output) = git_in(working_dir).args(["branch", "--show-current"]).output()
        && output.status.success()
    {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !branch.is_empty() {
            info.push(format!("  Branch: {}", branch));
        }
    }

    // Short status (modified files count)
    horon_agent_perf_counters::record_session_context_git_spawn();
    if let Ok(output) = git_in(working_dir).args(["status", "--porcelain"]).output()
        && output.status.success()
    {
        push_modified(&mut info, &String::from_utf8_lossy(&output.stdout));
    }

    finish(info)
}

/// Appends the changed-path summary: the count, the first five paths, and a
/// marker when more follow. `entries` is porcelain status without a header.
fn push_modified(info: &mut Vec<String>, entries: &str) {
    let modified: Vec<&str> = entries.lines().take(5).collect();
    if !modified.is_empty() {
        info.push(format!("  Modified: {} files", entries.lines().count()));
        for file in modified {
            info.push(format!("    {}", file));
        }
        if entries.lines().count() > 5 {
            info.push("    ...".to_string());
        }
    }
}

fn finish(info: Vec<String>) -> Option<String> {
    if info.len() > 1 { Some(info.join("\n")) } else { None }
}
