//! `sessions move`: preview, and with `--apply` change, a saved session's working root.

use crate::session;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Project files a session reads from its working root on every request.
const ROOT_SOURCES: [&str; 6] = [
    "AGENTS.md",
    ".horon/prompt-overlay.md",
    ".horon/preferred-tools.md",
    ".horon/skills",
    ".claude/skills",
    ".agents/skills",
];

#[derive(Serialize)]
struct MoveReport {
    session_id: String,
    from: Option<String>,
    to: String,
    applied: bool,
    /// Project guidance present at the target; it applies from the next request.
    found_at_target: Vec<&'static str>,
    notes: Vec<&'static str>,
}

pub(crate) fn run_session_move_command(session_ref: &str, target: &Path, apply: bool, json: bool) -> Result<()> {
    let id = session::find_session_by_name_or_id(session_ref)?;
    let mut stored = session::Session::load(&id)?;
    let to = canonical_directory(target)?;
    let to_text = to.to_string_lossy().into_owned();
    if apply && writer_is_active(&id)? {
        bail!("session {id} has a live writer; end it before changing its working root");
    }
    let report = MoveReport {
        session_id: id.clone(),
        from: stored.working_dir.clone(),
        found_at_target: ROOT_SOURCES
            .iter()
            .copied()
            .filter(|name| to.join(name).exists())
            .collect(),
        to: to_text.clone(),
        applied: apply && stored.working_dir.as_deref() != Some(to_text.as_str()),
        notes: vec![
            "AGENTS.md, prompt overlays and project skills are read from the working root on every request, so the new root's files apply from the next request",
            "scoped approval grants belong to the old workspace root and cover nothing under the new one; calls ask again",
        ],
    };
    if report.applied {
        stored.working_dir = Some(to_text);
        stored.save()?;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report, apply);
    }
    Ok(())
}

fn canonical_directory(target: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(target).with_context(|| format!("{} does not exist", target.display()))?;
    if !canonical.is_dir() {
        bail!("{} is not a directory", canonical.display());
    }
    Ok(canonical)
}

/// A writer owns the session while it holds the `.writer.lock` flock.
fn writer_is_active(id: &str) -> Result<bool> {
    let path = crate::storage::horon_agent_dir()?
        .join("sessions")
        .join(id)
        .join(".writer.lock");
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

fn print_report(report: &MoveReport, apply: bool) {
    println!("session {}", report.session_id);
    println!(
        "  from  {}",
        report.from.as_deref().unwrap_or("(no working root stored)")
    );
    println!("  to    {}", report.to);
    if report.found_at_target.is_empty() {
        println!("  project guidance at the target: none");
    } else {
        println!(
            "  project guidance at the target: {}",
            report.found_at_target.join(", ")
        );
    }
    for note in &report.notes {
        println!("  note: {note}");
    }
    if report.applied {
        println!("Moved the session's working root.");
    } else if apply {
        println!("The session already has this working root; nothing changed.");
    } else {
        println!("Preview only; run again with --apply to change it.");
    }
}
