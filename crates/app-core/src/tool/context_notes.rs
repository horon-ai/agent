//! `context_notes`: a small working-notes file the model keeps for one session.
//!
//! The notes are the model's own short-term index — a goal, a checklist, a
//! decision it must not lose after a compaction. They are stored per session
//! (`~/.horon-agent/context-notes/<session-id>.md`), read back into every
//! request's dynamic system prompt by `Agent::build_system_prompt_split`, and
//! deleted with the conversation branch. `history://current` (read) exposes
//! the full stored transcript beside them.

use super::{Tool, ToolContext, ToolEffects, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

/// Largest notes file this tool accepts, in UTF-8 bytes. The notes ride in
/// every request's dynamic prompt, so a large file would be paid for on every
/// turn; anything bigger belongs in the transcript, which `history://current`
/// already exposes.
const MAX_NOTE_BYTES: usize = 16_384;

/// Whether `[tools] context_notes` is on.
pub(crate) fn enabled() -> bool {
    crate::config::config().tools.context_notes
}

pub struct ContextNotesTool;

impl ContextNotesTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct ContextNotesInput {
    /// Omitted reads the notes back; an empty string deletes them.
    #[serde(default)]
    text: Option<String>,
}

#[async_trait]
impl Tool for ContextNotesTool {
    fn name(&self) -> &str {
        "context_notes"
    }

    fn description(&self) -> &str {
        "Read, replace, or delete this session's working notes. The notes are small and reappear in every request."
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::write()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "text": {
                    "type": "string",
                    "description": "Notes to store (replaces the old text). Omit to read them; an empty string deletes them."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: ContextNotesInput = serde_json::from_value(input)?;
        match params.text {
            None => match crate::context_notes::load(&ctx.session_id)? {
                Some(notes) if !notes.trim().is_empty() => Ok(ToolOutput::new(notes).with_title("Context notes")),
                _ => Ok(ToolOutput::new("No context notes are stored for this session.")),
            },
            Some(text) if text.is_empty() => {
                crate::context_notes::clear(&ctx.session_id)?;
                Ok(ToolOutput::new("Context notes cleared.").with_title("Context notes"))
            }
            Some(text) => {
                let bytes = text.len();
                if bytes > MAX_NOTE_BYTES {
                    return Err(anyhow::anyhow!(
                        "Notes are {bytes} bytes; the limit is {MAX_NOTE_BYTES}. Keep a compact index and read history://current for details."
                    ));
                }
                let path = crate::context_notes::notes_path(&ctx.session_id)?;
                crate::context_notes::save(&ctx.session_id, &text)?;
                Ok(
                    ToolOutput::new(format!("Stored {bytes} bytes of context notes at {}.", path.display()))
                        .with_title("Context notes"),
                )
            }
        }
    }
}

#[cfg(test)]
#[path = "context_notes_tests.rs"]
mod tests;
