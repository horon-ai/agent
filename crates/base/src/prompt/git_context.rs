//! The git lines of a session context: the current branch and a short status
//! summary, read by running `git` in the session's working directory.

use std::path::Path;
use std::process::Command;

/// Get git branch and status summary
pub(super) fn get_git_info(working_dir: Option<&Path>) -> Option<String> {
    let mut command = Command::new("git");
    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }
    // Check if we're in a git repo
    horon_agent_perf_counters::record_session_context_git_spawn();
    let in_repo = command
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
    let mut branch_command = Command::new("git");
    if let Some(dir) = working_dir {
        branch_command.current_dir(dir);
    }
    horon_agent_perf_counters::record_session_context_git_spawn();
    if let Ok(output) = branch_command.args(["branch", "--show-current"]).output()
        && output.status.success()
    {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !branch.is_empty() {
            info.push(format!("  Branch: {}", branch));
        }
    }

    // Short status (modified files count)
    let mut status_command = Command::new("git");
    if let Some(dir) = working_dir {
        status_command.current_dir(dir);
    }
    horon_agent_perf_counters::record_session_context_git_spawn();
    if let Ok(output) = status_command.args(["status", "--porcelain"]).output()
        && output.status.success()
    {
        let status = String::from_utf8_lossy(&output.stdout);
        let modified: Vec<&str> = status.lines().take(5).collect();
        if !modified.is_empty() {
            info.push(format!("  Modified: {} files", status.lines().count()));
            for file in modified {
                info.push(format!("    {}", file));
            }
            if status.lines().count() > 5 {
                info.push("    ...".to_string());
            }
        }
    }

    if info.len() > 1 { Some(info.join("\n")) } else { None }
}
