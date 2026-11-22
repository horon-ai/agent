//! Preview and import of a foreign transcript file named by its path.
//!
//! The source file is only ever read. A preview runs the same parser as the
//! import, but nothing is written: the importer's final save is skipped for
//! the calling thread. An import keeps text only (the shared importers drop
//! provider reasoning and turn tool traffic into text), so no provider token,
//! tool authorization or runnable command definition crosses over.

use super::{
    ClaudeCodeEntry, Session, import_claude_entries, import_codex_session_from_path, imported_claude_code_session_id,
    imported_codex_session_id, load_claude_code_entries, ordered_claude_code_message_entries,
};
use crate::message::{ContentBlock, Role};
use anyhow::{Context, Result, bail};
use std::cell::Cell;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

thread_local! {
    static PREVIEW_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// Whether the importer running on this thread must stop before saving.
pub(super) fn preview_only() -> bool {
    PREVIEW_ONLY.with(Cell::get)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    Claude,
    Codex,
}

impl TranscriptFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "openai-codex",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TranscriptPreview {
    pub format: TranscriptFormat,
    pub source_id: String,
    /// The Horon session the import creates, or already created.
    pub session_id: String,
    pub already_imported: bool,
    pub title: Option<String>,
    pub working_dir: Option<String>,
    pub model: Option<String>,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub first_prompt: Option<String>,
}

/// The format and source id of the transcript at `path`, from its first records.
///
/// A file that is not JSON lines, or whose records match neither format, fails
/// with the parser's own error so the caller can show exactly what was read.
fn detect(path: &Path) -> Result<(TranscriptFormat, String)> {
    let file = File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut first_keys = None;
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("failed to read {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| anyhow::anyhow!("{} line {} is not a JSON record: {error}", path.display(), index + 1))?;
        let kind = value.get("type").and_then(|kind| kind.as_str());
        if kind == Some("session_meta") {
            let meta = value.get("payload").unwrap_or(&value);
            if let Some(id) = meta.get("id").and_then(|id| id.as_str()).filter(|id| !id.is_empty()) {
                return Ok((TranscriptFormat::Codex, id.to_string()));
            }
        }
        if let Some(id) = value
            .get("sessionId")
            .and_then(|id| id.as_str())
            .filter(|id| !id.is_empty())
        {
            return Ok((TranscriptFormat::Claude, id.to_string()));
        }
        if first_keys.is_none() {
            first_keys = Some(match kind {
                Some(kind) => format!("type {kind:?}"),
                None => "no type field".to_string(),
            });
        }
        // A Codex rollout may open with a bare header that has an id and no type.
        if kind.is_none()
            && let Some(id) = value.get("id").and_then(|id| id.as_str()).filter(|id| !id.is_empty())
        {
            return Ok((TranscriptFormat::Codex, id.to_string()));
        }
        if index >= 50 {
            break;
        }
    }
    bail!(
        "{} is not a Claude Code or Codex transcript (first record has {})",
        path.display(),
        first_keys.unwrap_or_else(|| "nothing: the file is empty".to_string())
    )
}

fn with_preview_only<T>(run: impl FnOnce() -> T) -> T {
    PREVIEW_ONLY.with(|flag| flag.set(true));
    let result = run();
    PREVIEW_ONLY.with(|flag| flag.set(false));
    result
}

fn parse(path: &Path, format: TranscriptFormat, source_id: &str) -> Result<Session> {
    match format {
        TranscriptFormat::Claude => {
            let entries: Vec<ClaudeCodeEntry> = load_claude_code_entries(path)?;
            import_claude_entries(
                &ordered_claude_code_message_entries(&entries),
                source_id,
                imported_claude_code_session_id(source_id),
            )
        }
        TranscriptFormat::Codex => import_codex_session_from_path(path, Some(source_id)),
    }
}

fn resolve(path: &Path, forced: Option<TranscriptFormat>) -> Result<(TranscriptFormat, String)> {
    let (detected, source_id) = detect(path)?;
    match forced {
        Some(format) if format != detected => bail!(
            "{} looks like a {} transcript, not {}",
            path.display(),
            detected.label(),
            format.label()
        ),
        _ => Ok((detected, source_id)),
    }
}

/// Read `path` and describe what importing it would create. Writes nothing.
pub fn preview(path: &Path, forced: Option<TranscriptFormat>) -> Result<TranscriptPreview> {
    let (format, source_id) = resolve(path, forced)?;
    let session = with_preview_only(|| parse(path, format, &source_id))?;
    let text_of = |blocks: &[ContentBlock]| {
        blocks
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.trim().to_string()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
    };
    let messages = session.active_context();
    Ok(TranscriptPreview {
        format,
        session_id: session.id.clone(),
        already_imported: crate::session::session_exists(&session.id),
        title: session.title.clone(),
        working_dir: session.working_dir.clone(),
        model: session.model.clone(),
        user_messages: messages.iter().filter(|message| message.role == Role::User).count(),
        assistant_messages: messages
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .count(),
        first_prompt: messages
            .iter()
            .filter(|message| message.role == Role::User)
            .find_map(|message| text_of(&message.content)),
        source_id,
    })
}

/// Import `path` under its own Horon id with the source recorded on the session.
/// Importing the same transcript again reuses the stored session.
pub fn import(path: &Path, forced: Option<TranscriptFormat>) -> Result<Session> {
    let (format, source_id) = resolve(path, forced)?;
    let target = match format {
        TranscriptFormat::Claude => imported_claude_code_session_id(&source_id),
        TranscriptFormat::Codex => imported_codex_session_id(&source_id),
    };
    if crate::session::session_exists(&target) {
        return Session::load(&target);
    }
    parse(path, format, &source_id)
}
