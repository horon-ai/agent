//! RAII guard that marks a session as actively streaming.

/// RAII guard that marks a session as actively streaming for its lifetime.
///
/// Wraps the on-disk streaming marker from `horon-agent-storage` (cleared on every
/// exit path so presence UIs never show a phantom streaming session) and
/// additionally holds a macOS power assertion so the system does not
/// idle-sleep in the middle of a streaming model response.
pub struct StreamingGuard {
    _marker: crate::storage::StreamingGuard,
    #[allow(dead_code)]
    sleep_assertion: horon_agent_platform::PowerAssertion,
}

impl StreamingGuard {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            _marker: crate::storage::StreamingGuard::new(session_id),
            sleep_assertion: horon_agent_platform::PowerAssertion::prevent_user_idle_system_sleep(
                "HoronAgent streaming model response",
            ),
        }
    }

    /// Whether this guard currently holds a sleep-prevention assertion.
    ///
    /// Test-only: lets a test assert the in-process state directly instead of
    /// inferring it from the machine-global `pmset` table.
    #[cfg(test)]
    pub fn assertion_is_active(&self) -> bool {
        self.sleep_assertion.is_active()
    }

    /// Release the sleep-prevention assertion through the same path `Drop`
    /// uses, so a test can observe the release on a live value (a dropped
    /// value cannot be queried).
    #[cfg(test)]
    pub fn release_sleep_assertion(&mut self) {
        self.sleep_assertion.release();
    }
}
