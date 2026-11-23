//! Managed Git worktrees for sessions: list them, move a session into one, move
//! it back, and remove one that a move created.
//!
//! A move is a preview until `--apply`. It creates a new branch and worktree
//! from the session's current HEAD with the existing worktree lifecycle, so
//! uncommitted changes in the original directory are neither copied nor touched.
//! The session's stored history and its recovery path do not change: only its
//! working directory does, and the previous one is recorded so `--undo` can
//! restore it. Removal is a separate explicit action: it needs `--apply`, only
//! accepts a worktree a move recorded, and never forces past uncommitted work.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::git_worktree;

#[derive(Debug, Serialize, Deserialize)]
struct MoveRecord {
    session_id: String,
    previous_working_dir: String,
    worktree_path: String,
    branch: String,
    /// Whether the session currently works in `worktree_path`.
    moved: bool,
}

#[derive(Debug)]
struct StoredRecord {
    file_path: PathBuf,
    record: MoveRecord,
}

fn records_dir(home: &Path) -> PathBuf {
    home.join("worktree-moves")
}

fn normalize_worktree_path(raw: &str) -> String {
    match super::canonical_output_path(PathBuf::from(raw)) {
        Ok(canonical) => canonical.display().to_string(),
        Err(_) => raw.to_owned(),
    }
}

fn same_worktree_path(left: &str, right: &str) -> bool {
    left == right || normalize_worktree_path(left) == normalize_worktree_path(right)
}

fn worktree_key(worktree_path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let normalized = normalize_worktree_path(worktree_path);
    let digest = Sha256::digest(normalized.as_bytes());
    let mut hex = String::with_capacity(32);
    for &byte in digest.iter().take(16) {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    hex
}

fn record_path(home: &Path, worktree_path: &str) -> PathBuf {
    records_dir(home).join(format!("wt-{}.json", worktree_key(worktree_path)))
}

fn all_records(home: &Path) -> Vec<StoredRecord> {
    let Ok(entries) = std::fs::read_dir(records_dir(home)) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|file_path| {
            let bytes = match std::fs::read(&file_path) {
                Ok(bytes) => bytes,
                Err(_) => return None,
            };
            let record = match serde_json::from_slice::<MoveRecord>(&bytes) {
                Ok(record) => record,
                Err(_) => return None,
            };
            Some(StoredRecord { file_path, record })
        })
        .collect()
}

fn active_record(home: &Path, session_id: &str) -> Option<StoredRecord> {
    all_records(home)
        .into_iter()
        .find(|stored| stored.record.session_id == session_id && stored.record.moved)
}

fn write_record_for_worktree(home: &Path, record: &MoveRecord, old_path: Option<&Path>) -> Result<()> {
    super::create_private_dir(&records_dir(home))?;
    let path = record_path(home, &record.worktree_path);
    super::write_file(&path, &serde_json::to_vec_pretty(record)?)?;
    if let Some(old) = old_path
        && old != path
    {
        match std::fs::remove_file(old) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// A live writer owns the session while it holds the `.writer.lock` flock.
fn writer_is_active(home: &Path, id: &str) -> Result<bool> {
    let path = home.join("sessions").join(id).join(".writer.lock");
    if !path.exists() {
        return Ok(false);
    }
    let file = std::fs::OpenOptions::new().read(true).write(true).open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

fn uncommitted_paths(dir: &str) -> Result<usize> {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(["status", "--porcelain"])
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        bail!("{dir} is not inside a Git worktree");
    }
    Ok(output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count())
}

fn finish(json_output: bool, report: Value, lines: Vec<String>) -> Result<()> {
    if json_output {
        return super::emit(true, &report);
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// `sessions worktrees`: the worktrees of the repository around the current directory.
pub(super) fn list(home: &Path, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir()?.display().to_string();
    let recorded = all_records(home);
    let mut rows = Vec::new();
    for (index, tree) in git_worktree::list(&cwd)?.into_iter().enumerate() {
        let owners: Vec<&str> = recorded
            .iter()
            .filter(|stored| stored.record.moved && same_worktree_path(&stored.record.worktree_path, &tree.path))
            .map(|stored| stored.record.session_id.as_str())
            .collect();
        let dirty = !tree.bare && git_worktree::is_dirty(&tree.path).unwrap_or(false);
        rows.push(json!({
            "path": tree.path,
            "branch": tree.branch,
            "main": index == 0,
            "managed": !owners.is_empty(),
            "sessions": owners,
            "dirty": dirty,
            "locked": tree.locked.is_some(),
        }));
    }
    let lines = rows
        .iter()
        .map(|row| {
            format!(
                "{}{} {} [{}]",
                if row["main"] == json!(true) { "* " } else { "  " },
                row["path"].as_str().unwrap_or("-"),
                row["branch"].as_str().unwrap_or("(detached)"),
                [
                    (row["managed"] == json!(true)).then_some("session worktree"),
                    (row["dirty"] == json!(true)).then_some("uncommitted changes"),
                    (row["locked"] == json!(true)).then_some("locked"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ")
            )
        })
        .collect();
    finish(json_output, json!({ "worktrees": rows }), lines)
}

/// `sessions move-to-worktree`: preview, apply, or undo a move.
pub(super) fn move_session(
    home: &Path,
    session_ref: &str,
    branch: Option<&str>,
    undo: bool,
    apply: bool,
    json_output: bool,
) -> Result<()> {
    let id = crate::session::find_session_by_name_or_id(session_ref)?;
    let mut session = crate::session::Session::load(&id)?;
    let current = session
        .working_dir
        .clone()
        .with_context(|| format!("session {id} has no working directory"))?;
    let active = active_record(home, &id);

    if undo {
        let Some(stored) = active else {
            bail!("session {id} is not in a worktree this command created");
        };
        let record = stored.record;
        let mut report = json!({
            "session_id": id, "action": "undo", "from": current, "to": record.previous_working_dir,
            "applied": apply,
            "worktree_kept": record.worktree_path,
        });
        let mut lines = vec![
            format!(
                "session {id}: working directory {current} -> {}",
                record.previous_working_dir
            ),
            format!(
                "the worktree {} and its branch are kept; remove it with `sessions remove-worktree`",
                record.worktree_path
            ),
        ];
        if apply {
            if writer_is_active(home, &id)? {
                bail!("session {id} has a live writer; end it before moving its working directory");
            }
            session.working_dir = Some(record.previous_working_dir.clone());
            session.save()?;
            write_record_for_worktree(home, &MoveRecord { moved: false, ..record }, Some(&stored.file_path))?;
            lines.push("moved back.".into());
        } else {
            lines.push("preview only; add --apply to move back".into());
            report["next"] = json!("add --apply");
        }
        return finish(json_output, report, lines);
    }

    let branch = branch.context("a new branch name is required: --branch <name>")?;
    if active.is_some() {
        bail!("session {id} already works in a managed worktree; run with --undo first");
    }
    let changed = uncommitted_paths(&current)?;
    let head = git_worktree::head(&current)?;
    let planned = git_worktree::planned_path(&current, branch)?;
    let mut report = json!({
        "session_id": id, "action": "move", "from": current, "branch": branch,
        "worktree": planned.display().to_string(), "base_commit": head,
        "uncommitted_paths_left_behind": changed, "applied": apply,
        "undo": format!("horon sessions move-to-worktree {id} --undo --apply"),
    });
    let mut lines = vec![
        format!(
            "session {id}: {current} -> {} (new branch {branch}, from commit {head})",
            planned.display()
        ),
        format!(
            "{changed} uncommitted path(s) in {current} stay there: they are not copied to the worktree and not touched"
        ),
        format!("undo: horon sessions move-to-worktree {id} --undo --apply"),
    ];
    if !apply {
        lines.push("preview only; add --apply to create the worktree and move the session".into());
        return finish(json_output, report, lines);
    }
    if writer_is_active(home, &id)? {
        bail!("session {id} has a live writer; end it before moving its working directory");
    }
    let created = git_worktree::create(&current, branch)?;
    write_record_for_worktree(
        home,
        &MoveRecord {
            session_id: id.clone(),
            previous_working_dir: current.clone(),
            worktree_path: created.path.clone(),
            branch: branch.to_string(),
            moved: true,
        },
        None,
    )?;
    session.working_dir = Some(created.path.clone());
    if let Err(error) = session.save() {
        write_record_for_worktree(
            home,
            &MoveRecord {
                session_id: id.clone(),
                previous_working_dir: current,
                worktree_path: created.path.clone(),
                branch: branch.to_string(),
                moved: false,
            },
            None,
        )?;
        return Err(error.context(format!(
            "the worktree {} was created but the session was not moved",
            created.path
        )));
    }
    report["worktree"] = json!(created.path);
    lines.push(format!("moved. resume with: horon --resume {id}"));
    finish(json_output, report, lines)
}

/// `sessions remove-worktree`: remove a worktree a move created, never past uncommitted work.
pub(super) fn remove(home: &Path, path: &str, apply: bool, json_output: bool) -> Result<()> {
    let resolved = super::resolve_path(path)?;
    let target = super::canonical_output_path(resolved)?.display().to_string();
    let matches: Vec<StoredRecord> = all_records(home)
        .into_iter()
        .filter(|stored| same_worktree_path(&stored.record.worktree_path, &target))
        .collect();
    let Some(first) = matches.first() else {
        bail!("{target} was not created by `sessions move-to-worktree`; it is not removed");
    };
    if let Some(active) = matches.iter().find(|stored| stored.record.moved) {
        bail!(
            "session {} still works in {target}; run `sessions move-to-worktree {} --undo --apply` first",
            active.record.session_id,
            active.record.session_id
        );
    }
    let record = &first.record;
    let dirty = git_worktree::is_dirty(&target)?;
    let report = json!({
        "worktree": target, "branch": record.branch, "uncommitted_changes": dirty, "applied": apply,
    });
    if dirty {
        bail!("{target} has uncommitted changes; commit or discard them yourself, nothing was removed");
    }
    let mut lines = vec![format!("would remove {target} (branch {} is kept)", record.branch)];
    if apply {
        git_worktree::remove(&record.previous_working_dir, &target)?;
        for stored in &matches {
            match std::fs::remove_file(&stored.file_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        lines = vec![format!("removed {target}; branch {} is kept", record.branch)];
    } else {
        lines.push("preview only; add --apply to remove it".into());
    }
    finish(json_output, report, lines)
}
