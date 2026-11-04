//! Shared-checkout `git` guard.
//!
//! Several sessions routinely work in the same repository. A linked worktree
//! gives each one its own index and HEAD, but a command run in the main checkout
//! moves the branch every other session is reading and can discard work none of
//! them committed. This guard refuses the history-rewriting subset there and
//! leaves it alone in a linked worktree, which is exactly the isolation the
//! command needs.
//!
//! Only commands that change refs or discard uncommitted files are classified;
//! everything else is allowed, because a guard that refuses ordinary work gets
//! turned off.

use std::path::{Path, PathBuf};
use std::process::Command;

const GIT_DIR_REFUSAL: &str = "git with --git-dir/--work-tree is blocked while [tools] git_guard is on: the guard cannot verify which checkout it changes.";

/// Refuse a history-rewriting `git` segment when it targets a shared checkout.
pub(crate) fn review(command: &str, cwd: &Path) -> Option<String> {
    for segment in horon_agent_command_risk::split_segments(command) {
        let words: Vec<&str> = segment
            .iter()
            .map(|token| token.text.as_str())
            .filter(|word| !word.is_empty())
            .collect();
        let Some(program) = words.first() else {
            continue;
        };
        if basename(program) != "git" {
            continue;
        }
        if let Some(refusal) = review_invocation(&words[1..], cwd) {
            return Some(refusal);
        }
    }
    None
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn review_invocation(words: &[&str], cwd: &Path) -> Option<String> {
    let mut dir = cwd.to_path_buf();
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        if word == "--git-dir"
            || word == "--work-tree"
            || word.starts_with("--git-dir=")
            || word.starts_with("--work-tree=")
        {
            return Some(GIT_DIR_REFUSAL.to_string());
        }
        if word == "-C" {
            let value = words.get(index + 1)?;
            dir = join_dir(&dir, value);
            index += 2;
            continue;
        }
        if let Some(value) = word.strip_prefix("-C")
            && !value.is_empty()
        {
            dir = join_dir(&dir, value);
            index += 1;
            continue;
        }
        if word.starts_with('-') {
            index += if global_option_takes_value(word) { 2 } else { 1 };
            continue;
        }
        return classify(word, &words[index + 1..], &dir);
    }
    None
}

fn join_dir(dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    }
}

/// Global options that consume the following word.
fn global_option_takes_value(word: &str) -> bool {
    matches!(
        word,
        "-c" | "--config-env" | "--namespace" | "--attr-source" | "--super-prefix" | "--list-cmds"
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Allow,
    Stash,
    Destructive,
    BranchMove,
    CommitMove,
}

fn classify(sub: &str, args: &[&str], dir: &Path) -> Option<String> {
    let verdict = match sub {
        "stash" => match args.iter().copied().find(|arg| !arg.starts_with('-')) {
            Some("list" | "show" | "create") => Verdict::Allow,
            _ => Verdict::Stash,
        },
        "reset" => reset_verdict(args),
        "checkout" | "switch" | "restore" => checkout_verdict(sub, args, dir),
        _ => Verdict::Allow,
    };
    if verdict == Verdict::Allow {
        return None;
    }
    (place(dir) == Place::Shared).then(|| shared_refusal(sub))
}

fn reset_verdict(args: &[&str]) -> Verdict {
    if args.contains(&"--hard") {
        return Verdict::Destructive;
    }
    if args.contains(&"--") {
        return Verdict::Allow;
    }
    match operands(args).as_slice() {
        [rev] if *rev != "HEAD" && *rev != "@" => Verdict::CommitMove,
        _ => Verdict::Allow,
    }
}

fn checkout_verdict(sub: &str, args: &[&str], dir: &Path) -> Verdict {
    // `restore --staged` changes the index only; no working file is lost.
    let staged_only = sub == "restore" && args.contains(&"--staged") && !args.contains(&"--worktree");
    if staged_only {
        return Verdict::Allow;
    }
    let ops = operands(args);
    if has_force(args) || ops.iter().any(|op| is_whole_tree(op)) {
        return Verdict::Destructive;
    }
    match sub {
        "switch" => {
            if ops.is_empty() {
                Verdict::Allow
            } else {
                Verdict::BranchMove
            }
        }
        "checkout" => {
            if args
                .iter()
                .any(|arg| matches!(*arg, "-b" | "-B" | "--orphan" | "--detach"))
            {
                return Verdict::BranchMove;
            }
            if args.contains(&"--") {
                // Everything after `--` is a path, never a ref.
                return Verdict::Allow;
            }
            match ops.as_slice() {
                [op] if resolves_as_ref(dir, op) => Verdict::BranchMove,
                _ => Verdict::Allow,
            }
        }
        _ => Verdict::Allow,
    }
}

fn operands<'a>(args: &'a [&'a str]) -> Vec<&'a str> {
    args.iter().copied().filter(|arg| !arg.starts_with('-')).collect()
}

fn has_force(args: &[&str]) -> bool {
    args.iter().any(|arg| {
        matches!(*arg, "--force" | "-f" | "--discard-changes")
            || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('f'))
    })
}

fn is_whole_tree(operand: &str) -> bool {
    matches!(operand, "." | ":/" | "*" | ":(top)")
}

fn resolves_as_ref(dir: &Path, reference: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--verify", "--quiet", reference])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Shared,
    Private,
    Unknown,
}

fn place(dir: &Path) -> Place {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--git-dir", "--git-common-dir"])
        .output();
    let Ok(output) = output else {
        return Place::Unknown;
    };
    if !output.status.success() {
        return Place::Unknown;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let (Some(git_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
        return Place::Unknown;
    };
    if canonical(dir, git_dir) == canonical(dir, common_dir) {
        Place::Shared
    } else {
        Place::Private
    }
}

fn canonical(dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    };
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

fn shared_refusal(sub: &str) -> String {
    format!(
        "git {sub} is blocked in the shared main checkout: other sessions may have uncommitted work here. Use a linked worktree (communicate spawn with isolation=worktree) or commit your changes first. Set [tools] git_guard = false to disable this guard."
    )
}

#[cfg(all(test, not(windows)))]
#[path = "git_guard_tests.rs"]
mod tests;
