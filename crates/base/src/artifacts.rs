//! Per-session artifact store for output that does not fit inline.
//!
//! A long tool result is kept whole on disk and identified by a small numeric
//! id, so the transcript can carry a bounded head and tail plus a
//! `artifact://{id}` pointer the model can read back with line selectors. Files
//! live under `~/.horon/artifacts/<session>/` and are bounded twice: one
//! file is cut to its first and last 8 MiB, and a session's total is capped so
//! a runaway command cannot fill the disk.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::storage;

/// A single artifact keeps this much of over-long text before the middle is
/// dropped: 8 MiB from the start plus 8 MiB from the end.
const ARTIFACT_KEEP_BYTES: usize = 8 * 1024 * 1024;
/// Per-session ceiling over every `*.log` artifact.
const SESSION_CAP_BYTES: u64 = 256 * 1024 * 1024;
const ARTIFACT_SUFFIX: &str = ".log";

/// The directory holding one session's artifacts.
///
/// `session_id` lands in a path, so it is validated to the alphabet the session
/// store itself uses; anything else could name a parent directory.
pub(crate) fn session_dir(session_id: &str) -> Result<PathBuf> {
    let valid = !session_id.is_empty()
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if !valid {
        anyhow::bail!("Invalid session id for artifacts: '{session_id}'");
    }
    Ok(storage::horon_agent_dir()?.join("artifacts").join(session_id))
}

/// Keep the id-safe prefix of a tool name so a file says where it came from.
fn sanitize_tool(tool: &str) -> String {
    let mut name: String = tool
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .take(64)
        .collect();
    if name.is_empty() {
        name.push_str("tool");
    }
    name
}

/// Save `text` as the next artifact for `session_id` and return its id.
///
/// Ids are allocated by incrementing the largest numeric prefix present, so a
/// restart continues after the artifacts already on disk instead of reusing an
/// id a transcript may still point at.
pub fn save(session_id: &str, tool: &str, text: &str) -> Result<u64> {
    let dir = session_dir(session_id)?;
    fs::create_dir_all(&dir).with_context(|| format!("create artifact directory {}", dir.display()))?;
    let tool = sanitize_tool(tool);
    let mut id = next_id(&dir);
    loop {
        let path = dir.join(format!("{id}.{tool}{ARTIFACT_SUFFIX}"));
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                write_bounded(&mut file, text).with_context(|| format!("write artifact {}", path.display()))?;
                enforce_session_cap(&dir);
                return Ok(id);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => id += 1,
            Err(error) => {
                return Err(error).with_context(|| format!("create artifact {}", path.display()));
            }
        }
    }
}

/// The path of artifact `id` for `session_id`.
pub fn path(session_id: &str, id: u64) -> Result<PathBuf> {
    let dir = session_dir(session_id)?;
    if let Ok(entries) = fs::read_dir(&dir) {
        let prefix = format!("{id}.");
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name.ends_with(ARTIFACT_SUFFIX) {
                return Ok(entry.path());
            }
        }
    }
    anyhow::bail!("Artifact {id} not found for this session")
}

/// Return `text` inline when it fits `inline_budget`; otherwise keep the head
/// and tail inline and point at the full saved artifact.
pub fn spill(session_id: &str, tool: &str, text: &str, inline_budget: usize) -> String {
    if text.len() <= inline_budget {
        return text.to_string();
    }
    let (clipped, _) = horon_agent_core::util::head_tail(text, inline_budget);
    match save(session_id, tool, text) {
        Ok(id) => format!("{clipped}\n[raw output: artifact://{id}]"),
        Err(error) => format!("{clipped}\n[Full output was not saved (artifact write failed: {error})]"),
    }
}

fn next_id(dir: &Path) -> u64 {
    let mut max: Option<u64> = None;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(ARTIFACT_SUFFIX) else {
                continue;
            };
            let Some(prefix) = stem.split('.').next() else {
                continue;
            };
            if let Ok(id) = prefix.parse::<u64>() {
                max = Some(max.map_or(id, |current: u64| current.max(id)));
            }
        }
    }
    max.map_or(0, |current| current + 1)
}

fn write_bounded(file: &mut fs::File, text: &str) -> Result<()> {
    use std::io::Write;

    if text.len() <= ARTIFACT_KEEP_BYTES * 2 {
        file.write_all(text.as_bytes())?;
        return Ok(());
    }
    let head_end = floor_char_boundary(text, ARTIFACT_KEEP_BYTES);
    let head_end = match text[..head_end].rfind('\n') {
        Some(newline) => newline + 1,
        None => head_end,
    };
    let mut tail_start = floor_char_boundary(text, text.len() - ARTIFACT_KEEP_BYTES);
    if let Some(offset) = text[tail_start..].find('\n') {
        tail_start += offset + 1;
    }
    let tail_start = tail_start.max(head_end);
    let dropped = text.len() - head_end - (text.len() - tail_start);
    file.write_all(&text.as_bytes()[..head_end])?;
    if !text[..head_end].ends_with('\n') {
        file.write_all(b"\n")?;
    }
    writeln!(
        file,
        "[ARTIFACT TRUNCATED: kept first 8 MiB + last 8 MiB of {}; {} elided from the middle]",
        text.len(),
        dropped
    )?;
    file.write_all(&text.as_bytes()[tail_start..])?;
    Ok(())
}

fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Delete the lowest-id artifacts until the session is back under its cap.
fn enforce_session_cap(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(u64, PathBuf, u64)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(ARTIFACT_SUFFIX)?;
            let id = stem.split('.').next()?.parse::<u64>().ok()?;
            let size = entry.metadata().ok()?.len();
            Some((id, entry.path(), size))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, _, size)| *size).sum();
    if total <= SESSION_CAP_BYTES {
        return;
    }
    files.sort_by_key(|(id, _, _)| *id);
    for (_, path, size) in files {
        if total <= SESSION_CAP_BYTES {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
