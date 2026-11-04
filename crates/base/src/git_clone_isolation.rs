//! Clone-based isolation for spawned swarm children.
//!
//! Unlike a linked worktree, a clone is a full copy of the repository (including
//! `.git` and ignored build directories) made with the filesystem's own
//! copy-on-write primitive [`clonefile(2)`](https://man.openbsd.org/clonefile)
//! on macOS and `cp -a --reflink=always` on Linux. The copy shares blocks with
//! the parent until either side writes, so creating one is cheap even for a
//! large build cache while the child still gets a private `.git` it can commit
//! to with no shared refs. Children start on a fresh `swarm/...` branch created
//! inside the clone; settling an `apply` fetches that branch back into the
//! parent repository and merges it.
//!
//! Every checkout this module creates (and, through the same helper, every
//! linked worktree the swarm spawner creates) carries an owner marker at
//! `<checkout>.owner.json`. [`sweep_orphans`] reaps checkouts whose owning
//! process is gone, that are clean, and that hold no commits beyond the
//! recorded base commit. A checkout with changes is never removed.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::git_worktree;

/// A prepared clone checkout. `root` is the clone's repository root and
/// `working_dir` is the directory inside it the child runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneChild {
    pub root: String,
    pub working_dir: String,
    pub branch: String,
    pub base_head: String,
}

/// Ownership record written beside a checkout so a later spawn can reap a
/// checkout whose process died. Unknown fields are ignored.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OwnerMarker {
    pub pid: u32,
    pub session_id: String,
    pub created_at: String,
    pub base_head: String,
    pub kind: String,
}

fn git_output(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run git {}", args.first().unwrap_or(&"")))
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git_output(dir, args)?;
    ensure!(
        output.status.success(),
        "git {} failed: {}",
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn git_stdout(dir: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git(dir, args)?).trim().to_string())
}

/// The main worktree's path, which names the clone root beside it.
fn main_worktree(working_dir: &str) -> Result<PathBuf> {
    let entries = git_worktree::list(working_dir)?;
    let main = entries.first().context("Repository has no worktrees")?;
    Ok(PathBuf::from(&main.path))
}

/// `<main parent>/<main dir name>-clones`, the shared root for a repository's
/// clones. Mirrors the `<name>-worktrees` convention.
fn destination_root(working_dir: &str) -> Result<PathBuf> {
    let main = main_worktree(working_dir)?;
    let parent = main.parent().context("Repository has no parent directory")?;
    let name = main
        .file_name()
        .and_then(|name| name.to_str())
        .context("Repository name is not UTF-8")?;
    Ok(parent.join(format!("{name}-clones")))
}

/// Marker path for a checkout: the checkout path with `.owner.json` appended,
/// so the marker sits beside the checkout and never inside it.
fn marker_path(checkout: &Path) -> PathBuf {
    let mut path = checkout.as_os_str().to_owned();
    path.push(".owner.json");
    PathBuf::from(path)
}

/// Record that this process owns `checkout`. Best-effort callers only warn on
/// failure; [`sweep_orphans`] simply skips checkouts without a marker.
pub fn write_owner_marker(checkout: &Path, kind: &str, base_head: &str, session_id: &str) -> Result<()> {
    let marker = OwnerMarker {
        pid: std::process::id(),
        session_id: session_id.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        base_head: base_head.to_string(),
        kind: kind.to_string(),
    };
    let text = serde_json::to_string(&marker).context("Cannot serialize the checkout owner marker")?;
    std::fs::write(marker_path(checkout), text).context("Cannot write the checkout owner marker")
}

/// Drop a checkout's owner marker. Missing markers are not an error.
pub fn remove_owner_marker(checkout: &Path) {
    let _ = std::fs::remove_file(marker_path(checkout));
}

/// Read a checkout's owner marker, or `None` when it is missing or malformed.
pub fn read_owner_marker(checkout: &Path) -> Option<OwnerMarker> {
    let text = std::fs::read_to_string(marker_path(checkout)).ok()?;
    serde_json::from_str(&text).ok()
}

fn checkout_is_clean(path: &Path) -> bool {
    git_stdout(path, &["status", "--porcelain"]).is_ok_and(|status| status.is_empty())
}

fn has_no_new_commits(path: &Path, base_head: &str) -> bool {
    git_stdout(path, &["rev-list", "--count", &format!("{base_head}..HEAD")]).is_ok_and(|count| count == "0")
}

/// Copy `src` to `dst` with the filesystem's reflink primitive. `dst` must not
/// exist. The error text ends up in the caller's user-facing message.
#[cfg(target_os = "macos")]
fn clone_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        // Declared here because the pinned `libc` does not expose `clonefile`.
        fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32) -> libc::c_int;
    }

    let src = CString::new(src.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path contains a NUL byte"))?;
    let dst = CString::new(dst.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "destination path contains a NUL byte"))?;
    // SAFETY: both pointers are valid NUL-terminated strings for the duration
    // of the call, and `dst` does not exist (checked by the caller).
    let rc = unsafe { clonefile(src.as_ptr(), dst.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn clone_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let status = Command::new("cp")
        .arg("-a")
        .arg("--reflink=always")
        .arg(src)
        .arg(dst)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "cp -a --reflink=always exited with {status}"
        )))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_tree(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this platform has no copy-on-write directory clone",
    ))
}

/// Create a clone of the repository that owns `working_dir` on a new
/// `branch`, checked out in a sibling `<repo>-clones/<encoded-branch>`
/// directory.
///
/// The copy always starts from the repository's main worktree, because a
/// linked worktree has no repository of its own. The clone is then checked out
/// at the parent's HEAD, which drops staged and unstaged changes, and cleaned
/// with `git clean -fd` (without `-x`, so ignored build caches stay warm). The
/// child runs in the clone at the same relative path the parent used, or at
/// the clone root when the parent lives in a linked worktree. Errors never
/// trigger destructive cleanup of the parent.
pub fn create(working_dir: &str, branch: &str) -> Result<CloneChild> {
    let working = Path::new(working_dir);
    ensure!(
        working.is_dir(),
        "Clone isolation needs an existing working directory, got '{working_dir}'"
    );
    ensure!(!branch.is_empty() && !branch.starts_with('-'), "Invalid branch name");

    let repo_root = PathBuf::from(git_stdout(working, &["rev-parse", "--show-toplevel"])?);
    let base_head = git_stdout(&repo_root, &["rev-parse", "HEAD"])?;
    // Copy the main worktree, not a linked worktree: a linked worktree's `.git`
    // is a file pointing at the shared repository, and copying that would make
    // every git command in the clone mutate the original repository.
    let source = main_worktree(working_dir)?;
    ensure!(
        source.join(".git").is_dir(),
        "Clone isolation needs a repository with its own .git directory; use isolation=worktree"
    );
    let root_dir = destination_root(working_dir)?;
    if let Ok(meta) = std::fs::symlink_metadata(&root_dir) {
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "Clone root is not a real directory"
        );
    }
    std::fs::create_dir_all(&root_dir).context("Cannot create the clone root directory")?;
    let dest = root_dir.join(git_worktree::safe_name(branch));
    match std::fs::symlink_metadata(&dest) {
        Ok(_) => bail!("Clone destination already exists: {}", dest.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Cannot inspect clone destination"),
    }

    if let Err(error) = clone_tree(&source, &dest) {
        let _ = std::fs::remove_dir_all(&dest);
        bail!("Clone isolation is not available on this filesystem ({error}); use isolation=worktree.");
    }
    let prepared = (|| -> Result<()> {
        git(&dest, &["checkout", "-B", branch, &base_head])?;
        git(&dest, &["reset", "--hard", &base_head])?;
        git(&dest, &["clean", "-fd"])?;
        Ok(())
    })();
    if let Err(error) = prepared {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(error).context("Failed to prepare the clone checkout");
    }

    let root = dest.to_str().context("Clone path is not UTF-8")?.to_string();
    // A linked worktree parent has no matching subtree in the main-worktree
    // clone, so those children run at the clone root.
    let same_root = source
        .canonicalize()
        .ok()
        .zip(repo_root.canonicalize().ok())
        .is_some_and(|(source, root)| source == root);
    let relative = if same_root {
        working
            .canonicalize()
            .ok()
            .and_then(|working| {
                repo_root
                    .canonicalize()
                    .ok()
                    .and_then(|root| working.strip_prefix(root).ok().map(Path::to_path_buf))
            })
            .unwrap_or_default()
    } else {
        PathBuf::new()
    };
    let child_working_dir = if relative.as_os_str().is_empty() {
        dest.clone()
    } else {
        let mapped = dest.join(&relative);
        if mapped.is_dir() { mapped } else { dest.clone() }
    };
    if let Err(error) = write_owner_marker(&dest, "clone", &base_head, branch) {
        crate::logging::warn(&format!(
            "SWARM_CLONE_MARKER_FAILED path={} error={error}",
            dest.display()
        ));
    }
    Ok(CloneChild {
        root,
        working_dir: child_working_dir
            .to_str()
            .context("Clone working directory is not UTF-8")?
            .to_string(),
        branch: branch.to_string(),
        base_head,
    })
}

/// Fetch a clone's branch into the parent repository's same-named branch so
/// the parent can merge it. The clone is only a source: the parent repository
/// is the only one this modifies.
pub fn fetch_branch(parent_repo: &str, clone_root: &Path, branch: &str) -> Result<()> {
    let source = clone_root.to_str().context("Clone path is not UTF-8")?;
    git(
        Path::new(parent_repo),
        &["fetch", "--no-tags", source, &format!("{branch}:{branch}")],
    )?;
    Ok(())
}

/// Remove a clone checkout, refusing one that still holds uncommitted changes.
/// The owner marker is removed with it.
pub fn remove(clone_root: &Path) -> Result<()> {
    ensure!(
        checkout_is_clean(clone_root),
        "Clone has uncommitted changes: {}",
        clone_root.display()
    );
    std::fs::remove_dir_all(clone_root)
        .with_context(|| format!("Cannot remove the clone at {}", clone_root.display()))?;
    remove_owner_marker(clone_root);
    Ok(())
}

/// Reap checkouts under this repository's `-worktrees` and `-clones` roots
/// whose owning process is gone, that are clean, and that hold no commits
/// beyond their recorded base commit. Checkouts with changes, unknown markers,
/// or live owners are never touched. Returns the removed paths.
pub fn sweep_orphans(working_dir: &str) -> Result<Vec<String>> {
    let main = main_worktree(working_dir)?;
    let main_str = main.to_str().context("Repository path is not UTF-8")?;
    let parent = main.parent().context("Repository has no parent directory")?;
    let name = main
        .file_name()
        .and_then(|name| name.to_str())
        .context("Repository name is not UTF-8")?;
    let mut reaped = Vec::new();
    for suffix in ["-worktrees", "-clones"] {
        let root = parent.join(format!("{name}{suffix}"));
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut paths = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        paths.sort();
        for checkout in paths {
            let Some(marker) = read_owner_marker(&checkout) else {
                continue;
            };
            if crate::platform::is_process_running(marker.pid) {
                continue;
            }
            if !checkout_is_clean(&checkout) || !has_no_new_commits(&checkout, &marker.base_head) {
                continue;
            }
            let removed = match marker.kind.as_str() {
                "worktree" => {
                    let path = match checkout.to_str() {
                        Some(path) => path,
                        None => continue,
                    };
                    git_worktree::remove(main_str, path).is_ok()
                }
                "clone" => std::fs::remove_dir_all(&checkout).is_ok(),
                _ => false,
            };
            if removed {
                remove_owner_marker(&checkout);
                crate::logging::info(&format!("SWARM_ISOLATION_REAPED path={}", checkout.display()));
                reaped.push(checkout.to_string_lossy().into_owned());
            }
        }
    }
    Ok(reaped)
}

#[cfg(test)]
#[path = "git_clone_isolation_tests.rs"]
mod tests;
