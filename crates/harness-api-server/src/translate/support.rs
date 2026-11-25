//! Small helpers shared by the translation arms.

use serde_json::Value;

/// Whether translating this daemon event may perform synchronous file I/O.
///
/// Keep in sync with the arms of `BridgeState::legacy_event_to_api` that read
/// persisted session records or side-panel files. Everything else is pure
/// in-memory translation and must not pay for `block_in_place`.
pub fn legacy_event_may_block(event: &Value) -> bool {
    matches!(event["type"].as_str(), Some("state" | "split_response" | "history"))
}

/// Serializes every test that changes `HORON_HOME` or the store it reads.
///
/// The store resolves its directory from the process environment, so two tests
/// running at once would read each other's sessions.
#[cfg(test)]
pub(crate) fn horon_agent_home_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone)]
pub(crate) struct PendingAttach {
    pub(crate) subscribe_id: u64,
    pub(crate) state_id: u64,
    pub(crate) catalog_id: u64,
    pub(crate) api_id: u64,
    pub(crate) requested_session: Option<String>,
    pub(crate) create_dir: Option<String>,
}

/// Populate `working_dir` or `target_session_id` on a subscribe frame and return the directory for `create_session`.
pub(crate) fn apply_session_subscribe_target(
    subscribe: &mut Value,
    request: &Value,
    is_create: bool,
) -> Option<String> {
    if is_create {
        subscribe["fresh_session"] = serde_json::json!(true);
        if let Some(prompt) = request["system_prompt"].as_str() {
            subscribe["system_prompt"] = serde_json::json!(prompt);
        }
        let working_dir = match request["working_dir"].as_str() {
            Some(dir) => Some(dir.to_string()),
            None => match std::env::current_dir() {
                Ok(dir) => Some(dir.display().to_string()),
                Err(error) => {
                    horon_agent_base::logging::warn(&format!("create_session could not read current_dir: {error}"));
                    None
                }
            },
        };
        subscribe["working_dir"] = serde_json::json!(working_dir);
        working_dir
    } else if let Some(target) = request["session_id"].as_str() {
        // The daemon owns both live and persisted session roots. Empty panels
        // have no file yet, and a cached disk cwd may be older than the live Agent.
        subscribe["target_session_id"] = serde_json::json!(target);
        None
    } else {
        None
    }
}
