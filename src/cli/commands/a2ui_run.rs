use std::io::Write;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::message::{ContentBlock, Role};
use crate::session::StoredMessage;

/// Construct [`crate::A2uiSessionCapabilities`] from `run --a2ui` and `--a2ui-catalog`.
///
/// Returns `Ok(None)` when neither flag is set so default runs never alter their
/// tool list or system prompt.
pub(in crate::cli) fn build_a2ui_capabilities(
    enabled: bool,
    catalogs: &[String],
) -> Result<Option<crate::A2uiSessionCapabilities>> {
    if !enabled && catalogs.is_empty() {
        return Ok(None);
    }
    let supported_catalog_ids = if catalogs.is_empty() {
        vec![crate::A2UI_BASIC_CATALOG_V1_0.to_string()]
    } else {
        let mut deduped = Vec::with_capacity(catalogs.len());
        for raw in catalogs {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                bail!("--a2ui-catalog URI must not be empty");
            }
            if !deduped.iter().any(|existing: &String| existing == trimmed) {
                deduped.push(trimmed.to_string());
            }
        }
        deduped
    };
    Ok(Some(crate::A2uiSessionCapabilities {
        version: crate::A2UI_VERSION_V1_0.to_string(),
        supported_catalog_ids,
        accepts_inline_catalogs: false,
    }))
}

/// Extract assistant `a2ui` content blocks created during a one-shot run for `--json`.
pub(super) fn collect_a2ui_json_blocks(messages: &[StoredMessage]) -> Vec<Value> {
    let mut blocks = Vec::new();
    for message in messages {
        if message.role != Role::Assistant {
            continue;
        }
        for block in &message.content {
            let ContentBlock::A2ui {
                envelopes,
                fallback_text,
            } = block
            else {
                continue;
            };
            let mut value = json!({
                "type": "a2ui",
                "envelopes": envelopes,
            });
            if let Some(text) = fallback_text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                value["fallback_text"] = Value::String(text.to_string());
            }
            blocks.push(value);
        }
    }
    blocks
}

/// Render human-readable cards for assistant `a2ui` blocks created during a plain `run`.
pub(super) fn collect_a2ui_text_cards(messages: &[StoredMessage]) -> Vec<String> {
    let mut cards = Vec::new();
    for message in messages {
        if message.role != Role::Assistant {
            continue;
        }
        for block in &message.content {
            if let Some(card) = block.a2ui_text_card() {
                let trimmed = card.trim();
                if !trimmed.is_empty() {
                    cards.push(trimmed.to_string());
                }
            }
        }
    }
    cards
}

/// Print readable A2UI fallback/rendered cards to stdout for plain `run --a2ui`.
pub(super) fn print_a2ui_text_cards(messages: &[StoredMessage]) -> std::io::Result<()> {
    let cards = collect_a2ui_text_cards(messages);
    if cards.is_empty() {
        return Ok(());
    }
    let mut out = std::io::stdout().lock();
    write_a2ui_text_cards(&mut out, &cards)?;
    out.flush()
}

pub(super) fn write_a2ui_text_cards(out: &mut impl Write, cards: &[String]) -> std::io::Result<()> {
    for card in cards {
        writeln!(out, "{card}")?;
    }
    Ok(())
}

/// Format one `ServerEvent::A2uiEnvelope` as an `--ndjson` line payload.
pub(super) fn format_ndjson_a2ui_frame(sequence: u32, envelope: &Value, fallback_text: Option<&str>) -> Value {
    let mut frame = json!({
        "type": "a2ui",
        "sequence": sequence,
        "envelope": envelope,
    });
    if let Some(text) = fallback_text.map(str::trim).filter(|t| !t.is_empty()) {
        frame["fallback_text"] = Value::String(text.to_string());
    }
    frame
}

/// Stream one `ServerEvent::A2uiEnvelope` onto `--ndjson` stdout.
pub(super) fn emit_ndjson_a2ui(
    stdout: &mut impl Write,
    sequence: u32,
    envelope: &Value,
    fallback_text: Option<&str>,
) -> Result<()> {
    let frame = format_ndjson_a2ui_frame(sequence, envelope, fallback_text);
    super::write_json_line(stdout, &frame)
}
