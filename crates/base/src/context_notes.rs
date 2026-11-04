//! Session-scoped working notes kept beside the session id.
//!
//! The notes are a small markdown file the model maintains through the
//! `context_notes` tool: a compact index of what it must not forget (a goal, a
//! checklist, a decision). They are read back into every request's dynamic
//! system prompt by `Agent::build_system_prompt_split`, so they cost no cached
//! prefix, and cleared with the conversation branch that produced them.

use crate::storage;
use anyhow::Result;
use std::path::PathBuf;

/// Session ids that may name a notes file. The id becomes a file name, so
/// anything outside this set could escape the notes directory.
fn validate_session_id(session_id: &str) -> Result<()> {
    let valid = !session_id.is_empty()
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    if !valid {
        anyhow::bail!("Invalid session id for context notes: {session_id}");
    }
    Ok(())
}

/// Path of a session's notes file.
pub fn notes_path(session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    Ok(storage::horon_agent_dir()?
        .join("context-notes")
        .join(format!("{session_id}.md")))
}

/// The stored notes, or `None` when this session has never written any.
pub fn load(session_id: &str) -> Result<Option<String>> {
    let path = notes_path(session_id)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Replaces the notes with `text`.
pub fn save(session_id: &str, text: &str) -> Result<()> {
    let path = notes_path(session_id)?;
    storage::write_bytes(&path, text.as_bytes())
}

/// Removes the notes file. A session without notes is not an error.
pub fn clear(session_id: &str) -> Result<()> {
    let path = notes_path(session_id)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_session_id_that_could_leave_the_notes_directory() {
        for id in ["", "../escape", "a/b", "a.b", "a b"] {
            assert!(notes_path(id).is_err(), "{id:?} must not name a notes file");
        }
        assert!(notes_path("session-1_ok").is_ok());
    }
}
