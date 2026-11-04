use super::*;
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

#[path = "tool_image_decay.rs"]
mod tool_image_decay;

#[path = "history_page.rs"]
mod history_page;

/// A session snapshot written with the removed `plugin_id` field still loads
/// (twice, for stale-state safety) and keeps its model metadata.
#[test]
fn legacy_plugin_id_session_loads() -> anyhow::Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-legacy-plugin-id-")
        .tempdir()?;
    let _home = EnvVarGuard::set("HORON_AGENT_HOME", temp_home.path().as_os_str());

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
