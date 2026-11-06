//! Regression tests for #1680: inline tool-result images (`read`) must not
//! accumulate without bound in the persisted transcript.

use super::*;
use anyhow::{Result, anyhow};

const SCREENSHOT_CHARS: usize = 64 * 1024;

fn add_screenshot(session: &mut Session, index: usize) {
    session.add_message(
        Role::User,
        vec![
            ContentBlock::ToolUse {
                id: format!("call_{index}"),
                name: "read".into(),
                input: serde_json::json!({}),
                thought_signature: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: format!("call_{index}"),
                content: format!("Captured main display #{index}"),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "A".repeat(SCREENSHOT_CHARS),
            },
        ],
    );
}

fn inline_image_count(messages: &[StoredMessage]) -> usize {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter(|block| matches!(block, ContentBlock::Image { .. }))
        .count()
}

#[test]
fn computer_use_screenshots_stay_bounded_in_saved_transcript() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-tool-image-decay-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_AGENT_HOME", temp_home.path().as_os_str());

    let session_id = "session_tool_image_decay";
    let mut session = Session::create_with_id(session_id.to_string(), None, None);
    let screenshots = 100;
    for index in 0..screenshots {
        add_screenshot(&mut session, index);
        session.save()?;
        assert!(
            session
                .provider_messages()
                .iter()
                .flat_map(|m| &m.content)
                .filter(|b| matches!(b, ContentBlock::Image { .. }))
                .count()
                <= horon_agent_compaction_core::TOOL_IMAGE_DECAY_TRIGGER_COUNT
        );
    }

    // Every tool_result survives so tool_use pairing stays valid.
    assert_eq!(session.active_context().len(), screenshots);
    // The newest screenshot is still inline and reaches the provider.
    let provider_messages = session.provider_messages();
    let last = provider_messages.last().expect("provider message");
    assert!(
        last.content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))
    );

    let mut loaded = Session::load(session_id)?;
    assert_eq!(loaded.active_context().len(), screenshots);
    let originals = loaded.history_messages()?;
    assert_eq!(inline_image_count(&originals), screenshots);
    for (index, message) in originals.iter().enumerate() {
        assert!(message.content.iter().any(
            |b| matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == &format!("call_{index}"))
        ));
        assert!(
            message
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Image { data, .. } if data == &"A".repeat(SCREENSHOT_CHARS)))
        );
    }
    let inline = loaded
        .provider_messages()
        .iter()
        .flat_map(|m| &m.content)
        .filter(|b| matches!(b, ContentBlock::Image { .. }))
        .count();
    assert!(inline >= horon_agent_compaction_core::TOOL_IMAGE_DECAY_KEEP_COUNT);
    assert!(inline <= horon_agent_compaction_core::TOOL_IMAGE_DECAY_TRIGGER_COUNT);

    let original_bytes = std::fs::metadata(loaded.event_log_path()?)?.len();
    let projection_bytes = serde_json::to_vec(loaded.provider_messages())?.len();
    assert!(original_bytes >= u64::try_from(screenshots * SCREENSHOT_CHARS)?);
    assert!(projection_bytes < (horon_agent_compaction_core::TOOL_IMAGE_DECAY_TRIGGER_COUNT + 2) * SCREENSHOT_CHARS);
    println!("ORIGINAL_BYTES={original_bytes} PROJECTION_BYTES={projection_bytes} ORIGINAL_IMAGES={screenshots}");
    Ok(())
}

#[test]
fn loading_legacy_transcript_decays_old_screenshots() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-tool-image-legacy-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_AGENT_HOME", temp_home.path().as_os_str());

    // Simulate a transcript written before decay existed by appending the raw
    // messages without going through the decaying append path.
    let session_id = "session_tool_image_legacy";
    let mut session = Session::create_with_id(session_id.to_string(), None, None);
    let screenshots = 30;
    for index in 0..screenshots {
        add_screenshot(&mut session, index);
    }
    let mut legacy = Session::create_with_id(session_id.to_string(), None, None);
    let mut legacy_messages = session.active_context().to_vec();
    for message in &mut legacy_messages {
        if let [ContentBlock::ToolUse { .. }, ContentBlock::ToolResult { .. }, block] = message.content.as_mut_slice() {
            *block = ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "A".repeat(SCREENSHOT_CHARS),
            };
        }
    }
    legacy.replace_messages(legacy_messages);
    assert_eq!(inline_image_count(legacy.active_context()), screenshots);
    crate::storage::ensure_dir(session_path(session_id)?.parent().unwrap())?;
    legacy.status = SessionStatus::Closed;
    legacy.last_pid = None;
    crate::storage::write_json(&session_path(session_id)?, &legacy)?;
    let before = std::fs::read(session_path(session_id)?)?;

    let mut loaded = Session::load(session_id)?;
    assert_eq!(loaded.active_context().len(), screenshots);
    assert_eq!(
        loaded
            .provider_messages()
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| matches!(b, ContentBlock::Image { .. }))
            .count(),
        horon_agent_compaction_core::TOOL_IMAGE_DECAY_KEEP_COUNT
    );
    assert_eq!(inline_image_count(&loaded.history_messages()?), screenshots);
    assert_eq!(std::fs::read(session_path(session_id)?)?, before);
    Ok(())
}
