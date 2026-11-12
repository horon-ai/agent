use crate::message::{ContentBlock, ToolCall};
use crate::terminal_println as println;
use crate::tool::ToolOutput;

pub(super) const MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY: usize = 512 * 1024;

pub(super) fn cap_tool_output_for_history(tool_name: &str, session_id: &str, mut output: ToolOutput) -> ToolOutput {
    if output.output.len() <= MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY {
        return output;
    }
    output.output = history_cap_text(tool_name, session_id, &output.output);
    output.verdict = Some(crate::tool::ToolVerdict::after_cut(output.verdict));
    output
}

pub(super) fn cap_sdk_tool_content_for_history(tool_name: &str, session_id: &str, content: String) -> String {
    if content.len() <= MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY {
        return content;
    }
    history_cap_text(tool_name, session_id, &content)
}

fn history_cap_text(tool_name: &str, session_id: &str, content: &str) -> String {
    let spilled = crate::artifacts::spill(session_id, tool_name, content, MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY);
    format!(
        "{}\n\n[Tool output truncated by horon-agent: tool `{}` produced {} bytes; kept {} bytes of head and tail to protect the remote protocol, session history, and prompt cache. Read the artifact for the full output.]",
        spilled,
        tool_name,
        content.len(),
        MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY,
    )
}

/// Build rendered side-pane images from a tool output's attached images.
///
/// This mirrors how `render_messages_and_images` derives images from persisted
/// session history (source = ToolResult), so live-streamed images match what a
/// later History reload would produce. `tool_name` and `tool_input` provide the
/// label fallback (e.g. the `read` tool's `file_path`); `tool_call_id` anchors
/// the image to its tool message in the transcript.
pub(super) fn tool_output_side_pane_images(
    tool_call_id: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
    output: &ToolOutput,
) -> Vec<horon_agent_session_types::RenderedImage> {
    if output.images.is_empty() {
        return Vec::new();
    }
    let fallback_label = tool_input
        .get("file_path")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    output
        .images
        .iter()
        .map(|img| horon_agent_session_types::RenderedImage {
            history_message_index: None,
            media_type: img.media_type.clone(),
            data: img.data.clone(),
            label: img
                .label
                .as_ref()
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .or_else(|| fallback_label.clone()),
            source: horon_agent_session_types::RenderedImageSource::ToolResult {
                tool_name: tool_name.to_string(),
            },
            anchor: Some(horon_agent_session_types::RenderedImageAnchor::ToolCall {
                id: tool_call_id.to_string(),
            }),
        })
        .collect()
}

/// Metadata key a tool sets to load deferred tool definitions into context.
/// Value: array of registry tool names (e.g. `mcp__github__create_issue`).
pub(crate) const TOOL_REFERENCES_METADATA_KEY: &str = "tool_references";

/// Tool names a tool output asks to load via provider-native tool references.
pub(crate) fn tool_output_references(output: &ToolOutput) -> Vec<String> {
    output
        .metadata
        .as_ref()
        .and_then(|meta| meta.get(TOOL_REFERENCES_METADATA_KEY))
        .and_then(|refs| refs.as_array())
        .map(|refs| {
            refs.iter()
                .filter_map(|name| name.as_str())
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn tool_output_to_content_blocks(tool_use_id: String, output: ToolOutput) -> Vec<ContentBlock> {
    let references = tool_output_references(&output);
    // A `post_tool_feedback` hook may have rewritten the text and asked for an
    // error flag or a reminder; the round reads all three from the output.
    let is_error = crate::tool::post_tool_feedback::is_error(&output).then_some(true);
    let note = crate::tool::post_tool_feedback::note(&output);
    let mut blocks = vec![ContentBlock::ToolResult {
        tool_use_id: tool_use_id.clone(),
        content: output.output,
        is_error,
        // The layer that produced this output already said what became of the
        // call; storing it here is what makes the verdict travel with the
        // result instead of being re-read from the text later.
        verdict: output.verdict,
    }];
    for img in output.images {
        blocks.push(ContentBlock::Image {
            media_type: img.media_type,
            data: img.data,
        });
        if let Some(label) = img.label.filter(|label| !label.trim().is_empty()) {
            blocks.push(ContentBlock::Text {
                text: format!("[Attached image associated with the preceding tool result: {}]", label),
                cache_control: None,
            });
        }
    }
    // References come last so image folding (which expects an image right
    // after its tool_result) is unaffected. Providers without native deferred
    // loading ignore them; the result text still names the tools.
    blocks.extend(references.into_iter().map(|tool_name| ContentBlock::ToolReference {
        tool_use_id: tool_use_id.clone(),
        tool_name,
    }));
    // The reminder comes last so it reads after the result and its images,
    // without disturbing the image that must stay right after its tool_result.
    if let Some(note) = note {
        blocks.push(ContentBlock::Text {
            text: format!("<system-reminder>\n{note}\n</system-reminder>"),
            cache_control: None,
        });
    }
    blocks
}

/// The only entry point history uses for a tool result.
///
/// Masking lives here rather than at each round site so a new round path cannot
/// append an unmasked credential to the transcript.
pub(super) fn tool_output_to_history_blocks(
    tool_use_id: String,
    output: ToolOutput,
    session_id: &str,
) -> Vec<ContentBlock> {
    let mode = crate::config::config().tools.secrets_mode;
    let mut output = output;
    output.output = crate::secrets::mask_for_history(&output.output, session_id, mode);
    tool_output_to_content_blocks(tool_use_id, output)
}

pub(super) fn print_tool_summary(tool: &ToolCall) {
    match tool.name.as_str() {
        "bash" => {
            if let Some(cmd) = tool.input.get("command").and_then(|v| v.as_str()) {
                let short = if cmd.len() > 60 {
                    format!("{}...", crate::util::truncate_str(cmd, 60))
                } else {
                    cmd.to_string()
                };
                println!("$ {}", short);
            }
        }
        "read" | "write" | "edit" => {
            if let Some(path) = tool.input.get("file_path").and_then(|v| v.as_str()) {
                println!("{}", path);
            }
        }
        "glob" | "grep" => {
            if let Some(pattern) = tool.input.get("pattern").and_then(|v| v.as_str()) {
                println!("'{}'", pattern);
            }
        }
        "ls" => {
            let path = tool.input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            println!("{}", path);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_blocks_mask_a_credential_when_obfuscation_is_on() {
        let _env_lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("temp home");
        let previous_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        let config_path = crate::config::Config::path().expect("config path");
        std::fs::create_dir_all(config_path.parent().expect("config parent")).expect("config dir");
        std::fs::write(&config_path, "[tools]\nsecrets_mode = \"obfuscate\"\n").expect("write config");
        crate::config::invalidate_config_cache();

        let session_id = "tools-history-secret-test";
        let registration = crate::secrets::register_session_vault(session_id);
        let real = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";
        let blocks = tool_output_to_history_blocks(
            "call_1".to_string(),
            ToolOutput::new(format!("token {real}")),
            session_id,
        );
        let rendered = format!("{blocks:?}");

        drop(registration);
        match previous_home {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::invalidate_config_cache();

        assert!(
            !rendered.contains(real),
            "history must not hold the credential: {rendered}"
        );
        assert!(rendered.contains("«SECRET:github:"), "{rendered}");
    }

    #[test]
    fn authoritative_diff_text_survives_history_conversion_and_serialization() {
        let text = "Edited f\n\nFile diff:\n```diff\n--- f\n+++ f\n@@ -39,1 +39,1 @@\n-old\n+new\n```\n";
        let blocks = tool_output_to_content_blocks(
            "edit-call".into(),
            cap_tool_output_for_history("edit", "test-session", ToolOutput::new(text)),
        );
        let serialized = serde_json::to_string(&blocks).unwrap();
        let restored: Vec<ContentBlock> = serde_json::from_str(&serialized).unwrap();
        assert!(
            matches!(&restored[0], ContentBlock::ToolResult { content, tool_use_id, .. }
            if content == text && tool_use_id == "edit-call")
        );
    }

    #[test]
    fn cap_tool_output_leaves_small_output_unchanged() {
        let output = ToolOutput::new("short output");
        let capped = cap_tool_output_for_history("bash", "", output.clone());
        assert_eq!(capped.output, output.output);
    }

    #[test]
    fn cap_tool_output_adds_visible_truncation_notice() {
        let output = ToolOutput::new("x".repeat(MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY + 10));
        let capped = cap_tool_output_for_history("bash", "", output);
        assert!(capped.output.len() < MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY + 1_000);
        assert!(capped.output.contains("Tool output truncated by horon-agent"));
        assert!(capped.output.contains("tool `bash` produced"));
        assert!(capped.output.contains("Read the artifact for the full output"));
    }

    #[test]
    fn cap_counts_bytes_for_multibyte_output() {
        // 200,000 Hangul syllables are 600,000 UTF-8 bytes but only 200,000 chars;
        // a char-based cap would have let the whole payload through.
        let capped = cap_tool_output_for_history("bash", "", ToolOutput::new("가".repeat(200_000)));
        assert!(
            capped
                .output
                .contains("produced 600000 bytes; kept 524288 bytes of head and tail"),
            "{}",
            &capped.output[capped.output.len().saturating_sub(400)..]
        );
        assert!(capped.output.len() <= MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY + 1_000);
    }

    #[test]
    fn cap_marks_the_cut_and_leaves_a_whole_output_unmarked() {
        let whole = cap_tool_output_for_history("bash", "", ToolOutput::new("short output"));
        assert_eq!(whole.verdict, None, "an output under the cap is not cut");

        let cut = cap_tool_output_for_history(
            "bash",
            "",
            ToolOutput::new("x".repeat(MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY + 10)),
        );
        assert_eq!(cut.verdict, Some(crate::tool::ToolVerdict::Truncated));

        let partial = ToolOutput::new("x".repeat(MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY + 10))
            .with_verdict(crate::tool::ToolVerdict::Partial);
        assert_eq!(
            cap_tool_output_for_history("bash", "", partial).verdict,
            Some(crate::tool::ToolVerdict::Partial),
            "a cut does not hide that the call only finished in part"
        );
    }

    #[test]
    fn the_model_reads_the_same_blocks_whatever_the_verdict() {
        use crate::tool::post_tool_feedback::{ERROR_METADATA_KEY, NOTE_METADATA_KEY};
        let build = || {
            ToolOutput::new("Edited f")
                .with_metadata(serde_json::json!({
                    TOOL_REFERENCES_METADATA_KEY: ["mcp__github__create_issue"],
                    ERROR_METADATA_KEY: true,
                    NOTE_METADATA_KEY: "Check the diff before continuing.",
                }))
                .with_labeled_image("image/png", "data", "shot.png")
        };
        let render = |output: ToolOutput| {
            serde_json::to_string(&tool_output_to_content_blocks("call".into(), output))
                .expect("content blocks serialize")
        };
        let plain = render(build());
        // Guard the fixture itself: the flags and attachments the verdict must
        // not disturb have to be present in the baseline.
        assert!(plain.contains("Check the diff before continuing."), "{plain}");
        assert!(plain.contains("<system-reminder>"), "{plain}");
        assert!(plain.contains("shot.png"), "{plain}");
        assert!(plain.contains("tool_reference"), "{plain}");
        assert!(plain.contains("\"is_error\":true"), "{plain}");

        let verdicts = [
            crate::tool::ToolVerdict::Ok,
            crate::tool::ToolVerdict::Partial,
            crate::tool::ToolVerdict::Truncated,
            crate::tool::ToolVerdict::Withheld,
            crate::tool::ToolVerdict::Rejected,
            crate::tool::ToolVerdict::Failed,
            crate::tool::ToolVerdict::Interrupted,
        ];
        for verdict in verdicts {
            assert_eq!(
                render(build().with_verdict(verdict)),
                plain,
                "a {verdict} verdict must not change what the model receives"
            );
        }
    }

    #[test]
    fn cap_sdk_tool_content_adds_same_notice() {
        let capped = cap_sdk_tool_content_for_history("custom", "", "y".repeat(MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY + 10));
        assert!(capped.contains("Tool output truncated by horon-agent"));
        assert!(capped.contains("tool `custom` produced"));
    }
}

#[cfg(test)]
mod image_anchor_tests {
    use super::*;

    #[test]
    fn live_batch_images_anchor_to_parent_and_have_no_history_boundary() {
        let output = ToolOutput::new("batch results")
            .with_labeled_image("image/png", "one", "first.png")
            .with_labeled_image("image/png", "two", "second.png");
        let images = tool_output_side_pane_images("parent-batch", "batch", &serde_json::json!({}), &output);
        assert_eq!(images.len(), 2);
        for image in &images {
            assert_eq!(
                image.anchor,
                Some(horon_agent_session_types::RenderedImageAnchor::ToolCall {
                    id: "parent-batch".into()
                })
            );
            assert_eq!(image.history_message_index, None);
        }
        assert_eq!(images[0].data, "one");
        assert_eq!(images[1].data, "two");
        assert_eq!(images[0].label.as_deref(), Some("first.png"));
    }
}
