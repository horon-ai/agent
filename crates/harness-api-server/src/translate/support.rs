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
