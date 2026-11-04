use crate::logging;
use base64::Engine as _;
use horon_agent_background_types::{BackgroundTaskCompleted, BackgroundTaskProgressEvent, BackgroundTaskStatus};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

pub use horon_agent_message_types::{
    CacheControl, ConnectionPhase, ContentBlock, InputShellResult, Message, Role, StreamEvent,
    TOOL_OUTPUT_MISSING_TEXT, ToolCall, ToolDefinition, cache_relevant_message_hashes, cache_relevant_message_value,
    cache_relevant_messages, ends_with_fresh_user_turn, extend_stable_hash, messages_with_dynamic_system_context,
    provider_native, replace_text_suffix, sanitize_tool_id, stable_message_hash,
};

mod notifications;

fn compile_static_regex(pattern: &str) -> Option<Regex> {
    match Regex::new(pattern) {
        Ok(regex) => Some(regex),
        Err(err) => {
            logging::error(&format!("failed to compile static message regex: {err}"));
            eprintln!("horon-agent: failed to compile static regex: {err}");
            None
        }
    }
}

pub use notifications::{
    ParsedBackgroundTaskNotification, ParsedBackgroundTaskProgressNotification,
    ParsedBackgroundTaskStartedNotification, background_task_display_label, background_task_status_notice,
    format_background_task_notification_markdown, format_background_task_progress_markdown,
    format_background_task_stalled_markdown, format_input_shell_result_markdown,
    format_model_refresh_progress_markdown, input_shell_status_notice, parse_background_task_notification_markdown,
    parse_background_task_progress_notification_markdown, parse_background_task_started_notification_markdown,
    strip_ansi_escape_sequences,
};

/// Redact likely secrets from persisted tool output.
///
/// This is a best-effort safeguard for local session history files. It targets
/// high-confidence token/key patterns and common `KEY=VALUE` assignments used by
/// auth flows. The pattern table itself lives in [`crate::secrets::rules`], shared
/// with the reversible wire-path obfuscation.
pub fn redact_secrets(text: &str) -> String {
    // Fast path to avoid regex work for most tool outputs.
    if !crate::secrets::may_contain_secret(text) {
        logging::debug("secret redaction fast path skipped regex scan");
        return text.to_string();
    }

    logging::debug(&format!("running secret redaction scan bytes={}", text.len()));

    let mut redacted = text.to_string();

    for (_, re) in crate::secrets::rules::direct_regexes() {
        redacted = re.replace_all(&redacted, "[REDACTED_SECRET]").into_owned();
    }

    for re in crate::secrets::rules::assignment_regexes() {
        redacted = re.replace_all(&redacted, "${1}[REDACTED_SECRET]").into_owned();
    }

    for (_, re) in crate::secrets::rules::custom_assignment_regexes() {
        redacted = re.replace_all(&redacted, "${1}[REDACTED_SECRET]").into_owned();
    }

    if redacted != text {
        logging::info("redacted secrets from message text");
    }

    redacted
}

pub const GENERATED_IMAGE_TOOL_NAME: &str = "image_generation";
pub const GENERATED_IMAGE_MAX_AUTO_VISION_BYTES: u64 = 20 * 1024 * 1024;

/// Persist the model's reasoning for an assistant turn.
///
/// This always keeps a readable, history-only copy of the reasoning in the
/// transcript (`ContentBlock::ReasoningTrace`) so the thinking can be recalled
/// or debugged later. When `store_replay_context` is set, it *additionally*
/// stores the provider-specific replay block (`AnthropicThinking` /
/// `Reasoning`) that the provider needs echoed back on subsequent turns. To
/// avoid storing the same readable text twice, the history trace is skipped
/// when the replay block already captured the identical readable reasoning.
pub fn push_reasoning_blocks(
    blocks: &mut Vec<ContentBlock>,
    provider_name: &str,
    reasoning_content: &str,
    reasoning_signature: Option<&str>,
    store_replay_context: bool,
) {
    if reasoning_content.is_empty() {
        return;
    }

    // Whether the replay block we stored already contains the readable text.
    let mut readable_replay_stored = false;
    if store_replay_context {
        if provider_name.eq_ignore_ascii_case("anthropic") {
            if let Some(signature) = reasoning_signature.filter(|s| !s.is_empty()) {
                blocks.push(ContentBlock::AnthropicThinking {
                    thinking: reasoning_content.to_string(),
                    signature: signature.to_string(),
                });
                readable_replay_stored = true;
            }
        } else if provider_name.eq_ignore_ascii_case("openai") {
            // OpenAI native reasoning items carry encrypted content, not readable
            // text, so a separate history trace is still required below.
        } else {
            blocks.push(ContentBlock::Reasoning {
                text: reasoning_content.to_string(),
            });
            readable_replay_stored = true;
        }
    }

    if !readable_replay_stored {
        blocks.push(ContentBlock::ReasoningTrace {
            text: reasoning_content.to_string(),
        });
    }
}

pub fn generated_image_tool_input(
    path: &str,
    metadata_path: Option<&str>,
    output_format: &str,
    revised_prompt: Option<&str>,
) -> serde_json::Value {
    logging::debug(&format!(
        "building generated image tool input path={path} format={output_format} has_metadata={} has_revised_prompt={}",
        metadata_path.is_some(),
        revised_prompt.is_some()
    ));
    serde_json::json!({
        "path": path,
        "metadata_path": metadata_path,
        "output_format": output_format,
        "revised_prompt": revised_prompt,
    })
}

pub fn generated_image_summary(
    path: &str,
    metadata_path: Option<&str>,
    output_format: &str,
    revised_prompt: Option<&str>,
) -> String {
    logging::debug(&format!(
        "building generated image summary path={path} format={output_format} has_metadata={} has_revised_prompt={}",
        metadata_path.is_some(),
        revised_prompt.is_some()
    ));
    let mut summary = format!("Generated image ({}) saved to `{}`.", output_format, path);
    if let Some(metadata_path) = metadata_path {
        summary.push_str(&format!("\nMetadata saved to `{}`.", metadata_path));
    }
    if let Some(revised_prompt) = revised_prompt.filter(|prompt| !prompt.trim().is_empty()) {
        summary.push_str("\n\nRevised prompt:\n");
        summary.push_str(revised_prompt.trim());
    }
    summary
}

pub fn generated_image_visual_context_blocks(
    path: &str,
    metadata_path: Option<&str>,
    output_format: &str,
    revised_prompt: Option<&str>,
) -> Option<Vec<ContentBlock>> {
    logging::debug(&format!(
        "building generated image visual context path={path} format={output_format}"
    ));
    let (media_type, data_b64) = generated_image_payload(path, output_format)?;
    let mut reminder = format!(
        "<system-reminder>\nA provider-native image generation call created `{}`. HoronAgent attached the image pixels as visual context for future turns because the active provider supports image input and the file is under the safe {} MB limit.\nFormat: {}",
        path,
        GENERATED_IMAGE_MAX_AUTO_VISION_BYTES / 1024 / 1024,
        output_format,
    );
    if let Some(metadata_path) = metadata_path.filter(|value| !value.trim().is_empty()) {
        reminder.push_str(&format!("\nMetadata: {}", metadata_path));
    }
    if let Some(revised_prompt) = revised_prompt.filter(|value| !value.trim().is_empty()) {
        reminder.push_str("\nRevised prompt:\n");
        reminder.push_str(revised_prompt.trim());
    }
    reminder.push_str("\n</system-reminder>");

    Some(vec![
        ContentBlock::Text {
            text: reminder,
            cache_control: None,
        },
        ContentBlock::Image {
            media_type,
            data: data_b64,
        },
    ])
}

/// Convert a provider-native generated image into the same rendered-image
/// representation used by image-producing tools. The tool-call anchor keeps
/// the image beside the synthetic `image_generation` row in the transcript.
pub fn generated_image_rendered_image(
    id: &str,
    path: &str,
    output_format: &str,
) -> Option<horon_agent_session_types::RenderedImage> {
    let (media_type, data) = generated_image_payload(path, output_format)?;
    Some(horon_agent_session_types::RenderedImage {
        history_message_index: None,
        media_type,
        data,
        label: Some(path.to_string()),
        source: horon_agent_session_types::RenderedImageSource::ToolResult {
            tool_name: GENERATED_IMAGE_TOOL_NAME.to_string(),
        },
        anchor: Some(horon_agent_session_types::RenderedImageAnchor::ToolCall { id: id.to_string() }),
    })
}

fn generated_image_payload(path: &str, output_format: &str) -> Option<(String, String)> {
    let path_ref = Path::new(path);
    let metadata = std::fs::metadata(path_ref).ok()?;
    if !metadata.is_file() || metadata.len() > GENERATED_IMAGE_MAX_AUTO_VISION_BYTES {
        logging::warn(&format!(
            "skipping generated image payload path={path} is_file={} bytes={} limit={}",
            metadata.is_file(),
            metadata.len(),
            GENERATED_IMAGE_MAX_AUTO_VISION_BYTES
        ));
        return None;
    }

    let data = match std::fs::read(path_ref) {
        Ok(data) => data,
        Err(err) => {
            logging::error(&format!("failed to read generated image path={path}: {err}"));
            return None;
        }
    };
    // The media type comes from the bytes, not the extension or the requested
    // output format, and non-provider formats are converted (#1712).
    let image = match crate::image_normalize::normalize_image_bytes(data) {
        Ok(image) => image,
        Err(reason) => {
            logging::warn(&format!(
                "skipping generated image payload path={path} format={output_format}: {reason}"
            ));
            return None;
        }
    };
    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&image.data);
    Some((image.media_type.to_string(), data_b64))
}

#[cfg(test)]
mod tests;
