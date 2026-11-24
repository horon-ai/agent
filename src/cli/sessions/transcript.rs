//! `sessions import-transcript`: preview a Claude Code or Codex transcript file,
//! and import it only with `--apply`. The source file is only ever read.
use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::import::{TranscriptFormat, TranscriptPreview, preview_and_import_transcript, preview_transcript};

fn parse_format(raw: Option<&str>) -> Result<Option<TranscriptFormat>> {
    match raw {
        None => Ok(None),
        Some("claude") => Ok(Some(TranscriptFormat::Claude)),
        Some("codex") => Ok(Some(TranscriptFormat::Codex)),
        Some(other) => bail!("unknown transcript format {other:?}; use claude or codex"),
    }
}

fn describe(preview: &TranscriptPreview) -> Value {
    json!({
        "format": preview.format.label(),
        "source_id": preview.source_id,
        "session_id": preview.session_id,
        "already_imported": preview.already_imported,
        "title": preview.title,
        "working_dir": preview.working_dir,
        "model": preview.model,
        "user_messages": preview.user_messages,
        "assistant_messages": preview.assistant_messages,
        "first_prompt": preview.first_prompt,
        "kept": "text only: provider reasoning, tool authorizations and command definitions are not imported",
    })
}

pub(super) fn run(file: &str, format: Option<&str>, apply: bool, json_output: bool) -> Result<()> {
    let path = super::resolve_path(file)?;
    let format = parse_format(format)?;
    let (preview, imported_session) = if apply {
        let (preview, session) = preview_and_import_transcript(&path, format)?;
        (preview, Some(session))
    } else {
        (preview_transcript(&path, format)?, None)
    };
    let mut report = describe(&preview);
    if let Some(session) = imported_session {
        report["imported"] = json!(true);
        report["session_id"] = json!(session.id);
        report["resume"] = json!(format!("horon --resume {}", session.id));
    } else {
        report["imported"] = json!(false);
        report["next"] = json!(format!("rerun with --apply to import as {}", preview.session_id));
    }
    if json_output {
        return super::emit(true, &report);
    }
    let field = |key: &str| report[key].as_str().unwrap_or("-").to_string();
    println!("format: {} (source id {})", field("format"), field("source_id"));
    println!("horon session: {}", field("session_id"));
    println!("title: {}", field("title"));
    println!("working dir: {}", field("working_dir"));
    println!("model: {}", field("model"));
    println!(
        "messages: {} user, {} assistant",
        preview.user_messages, preview.assistant_messages
    );
    println!("first prompt: {}", field("first_prompt"));
    println!("kept: {}", field("kept"));
    match (apply, preview.already_imported) {
        (true, _) => println!("imported. resume with: {}", field("resume")),
        (false, true) => println!("already imported; --apply reuses the stored session"),
        (false, false) => println!("preview only; {}", field("next")),
    }
    Ok(())
}
