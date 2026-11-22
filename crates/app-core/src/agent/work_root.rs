//! `/workroot`: preview and change a saved session's working root.
//!
//! A session whose project moved on disk keeps pointing at the old path. The
//! preview shows the canonical target and which guidance file the change makes
//! the session read again; the change applies only after the same validation, so
//! a missing target, a non-directory or the current root changes nothing. The
//! caller already refuses a busy session, so a running turn is never re-rooted.
//! Stored messages are left as they are: only the session's root and the
//! guidance read from it change.
use super::Agent;
use anyhow::{Result, bail};
use serde_json::{Value, json};

/// What a change to `target` would do, without doing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootPreview {
    pub current: Option<String>,
    pub target: String,
    pub unchanged: bool,
    pub project_guidance_chars: Option<usize>,
}

impl Agent {
    /// Validate `raw` and describe the change. Reads the guidance at the target
    /// but writes nothing.
    pub(crate) fn work_root_preview(&self, raw: &str) -> Result<RootPreview> {
        let target = crate::workspace_dirs::canonical_root(raw)?;
        let target_text = target.to_string_lossy().into_owned();
        let current = self.working_dir().map(str::to_owned);
        let unchanged = current
            .as_deref()
            .is_some_and(|existing| crate::workspace_dirs::same_directory(existing, &target_text));
        let (_, info) = crate::prompt::load_agents_md_files_from_dir(Some(&target));
        Ok(RootPreview {
            current,
            target: target_text,
            unchanged,
            project_guidance_chars: info.has_project_agents_md.then_some(info.project_agents_md_chars),
        })
    }

    /// `/workroot [status | preview <path> | apply <path>]`.
    pub(crate) fn work_root_command(&mut self, args: &[String]) -> Result<(Value, String)> {
        match args {
            [] => Ok(self.work_root_status()),
            [action] if matches!(action.as_str(), "status" | "show") => Ok(self.work_root_status()),
            [action, path] if action == "preview" => {
                let preview = self.work_root_preview(path)?;
                let message = render(&preview, false);
                Ok((preview_json(&preview, false), message))
            }
            [action, path] if action == "apply" => {
                let preview = self.work_root_preview(path)?;
                if preview.unchanged {
                    bail!("The session already works in {}. Nothing changed.", preview.target);
                }
                self.set_working_dir(&preview.target);
                self.persist_session_best_effort("working root");
                let message = render(&preview, true);
                Ok((preview_json(&preview, true), message))
            }
            _ => bail!("Usage: /workroot [status | preview <path> | apply <path>]"),
        }
    }

    fn work_root_status(&self) -> (Value, String) {
        let current = self.working_dir().map(str::to_owned);
        let message = match &current {
            Some(root) => format!("This session works in {root}."),
            None => "This session has no working root.".to_owned(),
        };
        (json!({"working_dir": current}), message)
    }
}

fn preview_json(preview: &RootPreview, applied: bool) -> Value {
    json!({
        "applied": applied,
        "current": preview.current,
        "target": preview.target,
        "unchanged": preview.unchanged,
        "project_guidance_chars": preview.project_guidance_chars,
    })
}

fn render(preview: &RootPreview, applied: bool) -> String {
    let guidance = match preview.project_guidance_chars {
        Some(chars) => format!("{chars} characters of project AGENTS.md are read from the target"),
        None => "the target has no project AGENTS.md, so no project guidance is read".to_owned(),
    };
    let from = preview.current.as_deref().unwrap_or("(no root)");
    if applied {
        format!(
            "Moved the session root from {from} to {}. Guidance was read again: {guidance}. Stored messages are unchanged.",
            preview.target
        )
    } else if preview.unchanged {
        format!(
            "The session already works in {}. Applying would change nothing.",
            preview.target
        )
    } else {
        format!(
            "Would move the session root from {from} to {}. Guidance would be read again: {guidance}. Stored messages stay unchanged. Use `/workroot apply <path>` to apply.",
            preview.target
        )
    }
}

#[cfg(test)]
#[path = "work_root_tests.rs"]
mod tests;
