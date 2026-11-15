//! A stored session written before the swarm chat surface was cut still
//! resumes: the retired `swarm` actions survive as history the model can read.
//!
//! Removing topic channels and the shared-context store deleted actions
//! (`share`, `share_append`, `read`, `channel`, `list_channels`,
//! `channel_members`, `subscribe_channel`, `unsubscribe_channel`) and the
//! `channel` argument of `CommMessage`. Sessions written before that removal
//! hold `swarm` tool calls that name them. The two halves of the contract are
//! tested apart because they live in different crates:
//!
//! - here: replay. A stored call keeps its name and arguments through load,
//!   projection and render, so resuming an old session loses no transcript;
//! - `crates/app-core/src/tool/communicate_tests/input_format.rs`: a *new*
//!   call with a retired action is rejected with a message naming the
//!   replacement, so the old transcript can never be re-executed by accident.

use super::*;
use anyhow::Result;

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.into(),
        cache_control: None,
    }
}

fn retired_swarm_calls() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "read",
            serde_json::json!({"action": "read", "key": "plan_proposal:sess-1"}),
        ),
        (
            "share",
            serde_json::json!({
                "action": "share",
                "key": "build_state",
                "value": "green",
            }),
        ),
        (
            "channel",
            serde_json::json!({
                "action": "channel",
                "channel": "parser",
                "message": "edge case in foo.rs",
            }),
        ),
        (
            "subscribe_channel",
            serde_json::json!({"action": "subscribe_channel", "channel": "parser"}),
        ),
        (
            "message_with_channel",
            serde_json::json!({
                "action": "message",
                "channel": "parser",
                "message": "old form that addressed a channel",
            }),
        ),
    ]
}

#[test]
fn session_written_before_the_chat_removal_still_replays_its_swarm_calls() -> Result<()> {
    let _lock = tests::lock_env();
    let (temp_home, _home_path) = cases::canonical_tempdir("horon-agent-retired-swarm-replay-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let mut session = Session::create_with_id("retired-swarm-actions".to_string(), None, None);
    session.add_message(Role::User, vec![text("bring up the swarm")]);

    let calls = retired_swarm_calls();
    let mut blocks = Vec::new();
    for (index, (_, input)) in calls.iter().enumerate() {
        blocks.push(ContentBlock::ToolUse {
            id: format!("call_{index}"),
            name: "swarm".to_string(),
            input: input.clone(),
            thought_signature: None,
        });
        blocks.push(ContentBlock::tool_result(
            &format!("call_{index}"),
            "stored reply from the build that still had the action",
            None,
        ));
    }
    session.add_message(Role::Assistant, blocks);
    session.save_prepared()?;

    let mut loaded = Session::load("retired-swarm-actions")?;

    // The provider projection is what the model reads on the next turn: every
    // retired call must still be there, byte for byte.
    let projected = loaded.messages_for_provider();
    let projected_calls: Vec<(String, serde_json::Value)> = projected
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { name, input, .. } => Some((name.clone(), input.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        projected_calls.len(),
        calls.len(),
        "projection dropped a retired tool call"
    );
    for ((_, expected), (_, actual)) in calls.iter().zip(&projected_calls) {
        assert_eq!(actual, expected, "projection rewrote a retired tool call");
    }

    // Rendering must also keep them: a resumed session shows its history.
    let (rendered, _images) = render_messages_and_images(&loaded);
    let rendered_swarm_calls = rendered
        .iter()
        .filter(|message| message.tool_data.as_ref().is_some_and(|tool| tool.name == "swarm"))
        .count();
    assert_eq!(rendered_swarm_calls, calls.len(), "render dropped a retired tool call");
    assert!(
        rendered.iter().any(|message| message.content.contains("stored reply")),
        "the stored swarm replies must render on resume"
    );

    Ok(())
}
