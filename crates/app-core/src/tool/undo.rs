//! `undo` and `redo` over the edit journal.
//!
//! Every file-writing tool records one mutation unit per call in a
//! process-lifetime, session-keyed journal built on `horon-agent-edit-safety`.
//! The journal is deliberately not a field of [`super::Registry`]:
//! `Registry::clone` hands a subagent a fresh `CompactionManager`, so history
//! that travelled with the registry would be duplicated or lost by that clone.
//! A static map keyed by session id keeps one history per session for the life
//! of the process.
//!
//! Recording happens after the write succeeds, never before it, so a failed
//! write leaves no unit behind. `begin_file` and `commit` run in one critical
//! section with no await between them: the store counts an active unit's bytes
//! against the budget immediately, so a unit left open across an await would
//! push older history out and a cancelled call would strand it.
//!
//! `undo` restores the newest unit's before-state, but only while every file
//! still matches the recorded after-state; `force: true` overrides that check
//! and names each path it overwrote. `redo` re-applies the after-state of the
//! unit the last undo removed. The crate stores no redo history, so this module
//! keeps the undone unit's bytes and replays them.

use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use horon_agent_edit_safety::{BlobId, FileContent, FileState, MutationUnit, RestoreFile, RestoreState, SnapshotStore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

pub(crate) struct PendingFile {
    pub path: PathBuf,
    pub before: FileContent,
    pub after: FileContent,
}

struct RedoFile {
    path: PathBuf,
    before: FileContent,
    after: FileContent,
    recorded_before: FileState,
    recorded_after: FileState,
}

struct RedoEntry {
    tool_call_id: String,
    files: Vec<RedoFile>,
}

#[derive(Default)]
struct SessionJournal {
    store: SnapshotStore,
    redo: Vec<RedoEntry>,
}

type JournalMap = Mutex<HashMap<String, Arc<Mutex<SessionJournal>>>>;

fn journals() -> &'static JournalMap {
    static JOURNALS: OnceLock<JournalMap> = OnceLock::new();
    JOURNALS.get_or_init(Default::default)
}

fn session(session_id: &str) -> Arc<Mutex<SessionJournal>> {
    let mut map = journals().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    map.entry(session_id.to_string()).or_default().clone()
}

fn lock(journal: &Arc<Mutex<SessionJournal>>) -> MutexGuard<'_, SessionJournal> {
    journal.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) async fn capture(path: &Path) -> FileContent {
    match tokio::fs::read(path).await {
        Ok(bytes) => FileContent::Bytes(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileContent::Absent,
        Err(error) => FileContent::Unreadable {
            reason: error.to_string(),
        },
    }
}

pub(crate) fn bytes(content: &[u8]) -> FileContent {
    FileContent::Bytes(content.to_vec())
}

pub(crate) const ABSENT: FileContent = FileContent::Absent;

/// Record one tool call's file changes as one mutation unit.
///
/// Call this only after the writes succeeded. A path touched twice in one call
/// keeps its first before-state and its last after-state, so the unit still
/// describes the net change with one before/after pair.
pub(crate) fn record(ctx: &ToolContext, files: Vec<PendingFile>) {
    if files.is_empty() {
        return;
    }
    let mut merged: Vec<PendingFile> = Vec::with_capacity(files.len());
    for file in files {
        match merged.iter_mut().find(|existing| existing.path == file.path) {
            Some(existing) => existing.after = file.after,
            None => merged.push(file),
        }
    }

    let journal = session(&ctx.session_id);
    let mut journal = lock(&journal);
    for file in &merged {
        if let Err(error) =
            journal
                .store
                .begin_file(&ctx.tool_call_id, &file.path, file.before.clone(), file.after.clone())
        {
            crate::logging::warn(&format!(
                "undo journal: cannot record {} for {}: {error}",
                file.path.display(),
                ctx.tool_call_id
            ));
            let _ = journal.store.abort(&ctx.tool_call_id);
            return;
        }
    }
    match journal.store.commit(&ctx.tool_call_id) {
        Ok(_) => journal.redo.clear(),
        Err(error) => {
            crate::logging::warn(&format!("undo journal: cannot commit {}: {error}", ctx.tool_call_id));
            let _ = journal.store.abort(&ctx.tool_call_id);
        }
    }
}

pub(crate) fn redo_enabled() -> bool {
    crate::config::config().tools.redo
}

/// Collects the files one tool call changed and records them as a single
/// mutation unit when the call returns, whether it returned an error or not.
///
/// Multi-file tools write in a loop and can fail part-way, so the unit has to
/// describe every file the call actually wrote. Recording per write would put
/// one file per unit and make a single `undo` restore only the last of them.
pub(crate) struct Journal<'a> {
    ctx: &'a ToolContext,
    files: Vec<PendingFile>,
}

impl<'a> Journal<'a> {
    pub(crate) fn begin(ctx: &'a ToolContext) -> Self {
        Self { ctx, files: Vec::new() }
    }

    /// Call only after the write to `file.path` succeeded.
    pub(crate) fn wrote(&mut self, file: PendingFile) {
        self.files.push(file);
    }
}

impl Drop for Journal<'_> {
    fn drop(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let files = std::mem::take(&mut self.files);
        record(self.ctx, files);
    }
}

fn materialize(store: &SnapshotStore, unit: &MutationUnit) -> Vec<RedoFile> {
    unit.files
        .iter()
        .map(|file| RedoFile {
            path: file.path.clone(),
            before: content_of(store, &file.before),
            after: content_of(store, &file.after),
            recorded_before: file.before.clone(),
            recorded_after: file.after.clone(),
        })
        .collect()
}

fn content_of(store: &SnapshotStore, state: &FileState) -> FileContent {
    match state {
        FileState::Absent => FileContent::Absent,
        FileState::Present(id) => match store.blob(id) {
            Some(bytes) => FileContent::Bytes(bytes.to_vec()),
            None => FileContent::Unreadable {
                reason: format!("blob {id} was released"),
            },
        },
        FileState::Unreadable { reason } => FileContent::Unreadable { reason: reason.clone() },
    }
}

fn state_matches(recorded: &FileState, current: &FileContent) -> bool {
    match (recorded, current) {
        (FileState::Unreadable { .. }, _) => true,
        (FileState::Absent, FileContent::Absent) => true,
        (FileState::Present(id), FileContent::Bytes(bytes)) => BlobId::of(bytes) == *id,
        _ => false,
    }
}

fn describe_expected(recorded: &FileState) -> String {
    match recorded {
        FileState::Absent => "absent".to_string(),
        FileState::Present(id) => format!("sha256 {id}"),
        FileState::Unreadable { reason } => format!("unreadable ({reason})"),
    }
}

fn describe_found(current: &FileContent) -> String {
    match current {
        FileContent::Absent => "absent".to_string(),
        FileContent::Bytes(bytes) => format!("sha256 {}", BlobId::of(bytes)),
        FileContent::Unreadable { reason } => format!("unreadable ({reason})"),
    }
}

fn refusal(direction: &str, unit: &str, stale: &[(PathBuf, FileState, FileContent)], force_hint: bool) -> String {
    let mut message = format!(
        "{direction} refused: {} file{} changed after the {}.\n",
        stale.len(),
        if stale.len() == 1 { "" } else { "s" },
        if direction == "Undo" { "recorded write" } else { "undo" }
    );
    for (path, expected, found) in stale {
        message.push_str(&format!(
            "  {}: expected {}, found {}\n",
            path.display(),
            describe_expected(expected),
            describe_found(found)
        ));
    }
    message.push_str(&format!("Unit: {unit}"));
    if force_hint {
        message.push_str("\nRe-run with force: true to overwrite these files anyway.");
    }
    message
}

fn restore_label(file: &RestoreFile) -> String {
    match &file.state {
        RestoreState::Absent => format!("{} (removed)", file.path.display()),
        RestoreState::Bytes(bytes) => format!("{} ({}) bytes restored", file.path.display(), bytes.len()),
    }
}

async fn apply(files: &[RestoreFile]) -> Result<Vec<String>> {
    let mut applied = Vec::with_capacity(files.len());
    for file in files {
        match &file.state {
            RestoreState::Absent => match tokio::fs::remove_file(&file.path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("removing {}", file.path.display()));
                }
            },
            RestoreState::Bytes(bytes) => {
                tokio::fs::write(&file.path, bytes)
                    .await
                    .with_context(|| format!("writing {}", file.path.display()))?;
            }
        }
        applied.push(restore_label(file));
    }
    Ok(applied)
}

pub struct UndoTool;

impl UndoTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct UndoInput {
    #[serde(default)]
    force: bool,
}

#[async_trait]
impl Tool for UndoTool {
    fn name(&self) -> &str {
        "undo"
    }

    fn description(&self) -> &str {
        "Undo the most recent file mutation this session's file tools made."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "force": {
                    "type": "boolean",
                    "description": "Restore even when a file changed after the recorded write."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: UndoInput = serde_json::from_value(input).unwrap_or(UndoInput { force: false });
        let journal = session(&ctx.session_id);

        let (unit_id, recorded) = {
            let journal = lock(&journal);
            let Some(unit) = journal.store.history().last() else {
                anyhow::bail!("Nothing to undo: this session recorded no file mutation.");
            };
            (unit.tool_call_id.clone(), materialize(&journal.store, unit))
        };

        let mut stale = Vec::new();
        for file in &recorded {
            let current = capture(&file.path).await;
            if !state_matches(&file.recorded_after, &current) {
                stale.push((file.path.clone(), file.recorded_after.clone(), current));
            }
        }
        if !stale.is_empty() && !params.force {
            anyhow::bail!("{}", refusal("Undo", &unit_id, &stale, true));
        }

        let outcome = {
            let mut journal = lock(&journal);
            let outcome = journal.store.undo().map_err(|error| anyhow::anyhow!("{error}"))?;
            journal.redo.push(RedoEntry {
                tool_call_id: unit_id.clone(),
                files: recorded,
            });
            outcome
        };

        let applied = apply(&outcome.files).await?;
        let mut body = format!("Undid {unit_id}: {} file(s)\n", applied.len());
        for line in &applied {
            body.push_str(&format!("  - {line}\n"));
        }
        if !stale.is_empty() {
            body.push_str("\nOverwrote files that changed after the recorded write:\n");
            for (path, _, _) in &stale {
                body.push_str(&format!("  - {}\n", path.display()));
            }
        }
        Ok(ToolOutput::new(body.trim_end().to_string()).with_title(unit_id))
    }
}

pub struct RedoTool;

impl RedoTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct RedoInput {
    #[serde(default)]
    force: bool,
}

#[async_trait]
impl Tool for RedoTool {
    fn name(&self) -> &str {
        "redo"
    }

    fn description(&self) -> &str {
        "Re-apply the file mutation the last undo removed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "force": {
                    "type": "boolean",
                    "description": "Re-apply even when a file changed after the undo."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: RedoInput = serde_json::from_value(input).unwrap_or(RedoInput { force: false });
        let journal = session(&ctx.session_id);

        let entry = {
            let mut journal = lock(&journal);
            journal.redo.pop()
        };
        let Some(entry) = entry else {
            anyhow::bail!("Nothing to redo: no undone mutation is waiting in this session.");
        };

        let mut stale = Vec::new();
        for file in &entry.files {
            let current = capture(&file.path).await;
            if !state_matches(&file.recorded_before, &current) {
                stale.push((file.path.clone(), file.recorded_before.clone(), current));
            }
        }
        if !stale.is_empty() && !params.force {
            let message = refusal("Redo", &entry.tool_call_id, &stale, true);
            lock(&journal).redo.push(entry);
            anyhow::bail!("{message}");
        }

        let applied = apply_redo(&entry).await?;

        {
            let mut journal = lock(&journal);
            for file in &entry.files {
                journal
                    .store
                    .begin_file(&entry.tool_call_id, &file.path, file.before.clone(), file.after.clone())
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
            }
            journal
                .store
                .commit(&entry.tool_call_id)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
        }

        let mut body = format!("Redid {}: {} file(s)\n", entry.tool_call_id, applied.len());
        for line in &applied {
            body.push_str(&format!("  - {line}\n"));
        }
        if !stale.is_empty() {
            body.push_str("\nOverwrote files that changed after the undo:\n");
            for (path, _, _) in &stale {
                body.push_str(&format!("  - {}\n", path.display()));
            }
        }
        Ok(ToolOutput::new(body.trim_end().to_string()).with_title(entry.tool_call_id))
    }
}

async fn apply_redo(entry: &RedoEntry) -> Result<Vec<String>> {
    let mut applied = Vec::with_capacity(entry.files.len());
    for file in &entry.files {
        match &file.after {
            FileContent::Bytes(bytes) => {
                tokio::fs::write(&file.path, bytes)
                    .await
                    .with_context(|| format!("writing {}", file.path.display()))?;
                applied.push(format!("{} ({} bytes re-applied)", file.path.display(), bytes.len()));
            }
            FileContent::Absent => match tokio::fs::remove_file(&file.path).await {
                Ok(()) => applied.push(format!("{} (removed again)", file.path.display())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    applied.push(format!("{} (already absent)", file.path.display()));
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("removing {}", file.path.display()));
                }
            },
            FileContent::Unreadable { reason } => {
                anyhow::bail!(
                    "Redo refused: {} was unreadable when recorded ({reason}).",
                    file.path.display()
                );
            }
        }
    }
    Ok(applied)
}

#[cfg(test)]
#[path = "undo_tests.rs"]
mod undo_tests;
