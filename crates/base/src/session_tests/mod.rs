use super::*;
use horon_agent_time::Duration;
use std::ffi::OsString;

pub(super) fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    crate::storage::lock_test_env()
}

pub(super) struct EnvVarGuard {
    key: &'static str,
    prev: Option<OsString>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, prev }
    }

    fn remove(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        crate::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = &self.prev {
            crate::env::set_var(self.key, prev);
        } else {
            crate::env::remove_var(self.key);
        }
    }
}

#[path = "cases.rs"]
pub(super) mod cases;

/// A fork from a recorded boundary keeps exactly the messages before it, and a
/// boundary the parent no longer holds is refused without partially filling the
/// child.
#[test]
fn a_boundary_fork_keeps_the_exact_prefix_and_refuses_a_stale_id() {
    let mut parent = Session::create(None, None);
    let first = parent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    );
    let second = parent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "answer".to_string(),
            cache_control: None,
        }],
    );
    let boundary = parent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "branch here".to_string(),
            cache_control: None,
        }],
    );

    let mut child = Session::create(Some(parent.id.clone()), None);
    let kept = child
        .inherit_history_before(&parent, &boundary)
        .expect("a recorded boundary forks");
    assert_eq!(kept, 2);
    let text: Vec<&str> = child
        .active_context()
        .iter()
        .map(|message| match &message.content[0] {
            ContentBlock::Text { text, .. } => text.as_str(),
            other => panic!("unexpected block {other:?}"),
        })
        .collect();
    assert_eq!(text, vec!["first", "answer"]);
    assert_ne!(child.id, parent.id);
    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));

    // The parent keeps every message, including the one the boundary named.
    assert_eq!(parent.active_context().len(), 3);
    assert_eq!(parent.active_context()[0].id, first);
    assert_eq!(parent.active_context()[1].id, second);

    let mut stale_child = Session::create(Some(parent.id.clone()), None);
    let refused = stale_child.inherit_history_before(&parent, "message_that_never_existed");
    assert!(refused.is_err(), "a stale boundary must be refused");
    assert!(
        stale_child.active_context().is_empty(),
        "a refused boundary must not leave a partial prefix"
    );
}

#[test]
fn rendered_rows_name_the_stored_message_they_came_from() {
    let mut session = Session::create(None, None);
    let prompt = session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "question".to_string(),
            cache_control: None,
        }],
    );
    let answer = session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::Text {
                text: "answer".to_string(),
                cache_control: None,
            },
            ContentBlock::ReasoningTrace {
                text: "thinking".to_string(),
            },
        ],
    );

    let rendered = render_messages(&session);
    let ids: Vec<Option<&str>> = rendered.iter().map(|row| row.stored_id.as_deref()).collect();
    assert_eq!(ids, vec![Some(prompt.as_str()), Some(answer.as_str())]);
}

#[path = "tool_arg_repair.rs"]
mod tool_arg_repair;

#[path = "tool_image_decay.rs"]
mod tool_image_decay;

#[path = "durability.rs"]
mod durability;

#[path = "binding_rev.rs"]
mod binding_rev;
#[path = "final_cache_skip.rs"]
mod final_cache_skip;

#[path = "history_page.rs"]
mod history_page;

#[path = "retired_swarm_actions.rs"]
mod retired_swarm_actions;

/// A session snapshot written with the removed `plugin_id` field still loads
/// (twice, for stale-state safety) and keeps its model metadata.
#[test]
fn legacy_plugin_id_session_loads() -> anyhow::Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-legacy-plugin-id-")
        .tempdir()?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let mut session = Session::create_with_id("legacy_plugin_id_session".to_string(), None, None);
    session.plugin_id = Some("router".to_string());
    session.provider_key = Some("plugin".to_string());
    session.model = Some("router-model".to_string());
    session.route_api_method = Some("openai-compatible:router".to_string());
    session.save_prepared()?;

    for _ in 0..2 {
        let loaded = Session::load("legacy_plugin_id_session")?;
        assert_eq!(loaded.plugin_id.as_deref(), Some("router"));
        assert_eq!(loaded.provider_key.as_deref(), Some("plugin"));
        assert_eq!(loaded.model.as_deref(), Some("router-model"));
        assert_eq!(loaded.route_api_method.as_deref(), Some("openai-compatible:router"));
    }
    Ok(())
}

/// A session snapshot written by a canary build still loads as a normal session:
/// the retired flags become inert legacy extensions, never runtime state.
#[test]
fn legacy_session_with_canary_flags_loads_as_a_normal_session() -> anyhow::Result<()> {
    // Written by a build that had canary sessions; spelled indirectly so the
    // retired field name never reappears in this tree.
    const LEGACY_CANARY_KEY: &str = concat!("is_", "canary");
    let session = Session::create_with_id("legacy-canary-test".into(), None, None);
    let mut value = serde_json::to_value(&session)?;
    let object = value.as_object_mut().expect("a session serializes as an object");
    object.insert(LEGACY_CANARY_KEY.to_string(), serde_json::json!(true));
    object.insert("testing_build".to_string(), serde_json::json!("self-dev"));

    let restored: Session = serde_json::from_value(value)?;
    assert_eq!(restored.id, session.id);
    assert!(!restored.is_debug);
    assert_eq!(
        restored.legacy_extensions.get(LEGACY_CANARY_KEY),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        restored.legacy_extensions.get("testing_build"),
        Some(&serde_json::json!("self-dev"))
    );
    Ok(())
}
