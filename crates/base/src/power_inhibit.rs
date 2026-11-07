//! Process-wide "keep the machine awake while horon-agent is working".
//!
//! The mechanism lives in the platform adapter ([`horon_agent_platform::power`]):
//! each operating system supplies its own inhibitor (a `systemd-inhibit` helper
//! on Linux, `caffeinate` on macOS, an execution-state thread on Windows) and
//! the shared policy - bounded helper lifetime, refresh, handover and retry -
//! runs there too. This module only names the type for this platform, so the
//! crate has no operating-system branch of its own and its callers keep the same
//! name.

/// The sleep inhibitor for the current operating system.
pub type PowerInhibitor = horon_agent_platform::power::PowerInhibitor<horon_agent_platform::power::Inhibitor>;
