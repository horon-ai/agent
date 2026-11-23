//! Per-process switches that turn off optional workspace instruction discovery.
//!
//! The base system prompt and the policy-owned sections always apply. Only the
//! files a project or user adds on top can be skipped, and a skipped source is
//! simply not read, so no access beyond the default is ever granted.

/// Set to `1` to skip `AGENTS.md` discovery (project and global).
pub const NO_CONTEXT_FILES_ENV: &str = "HORON_AGENT_NO_CONTEXT_FILES";
/// Set to `1` to skip imported foreign rules, prompt overlays and preferred-tools guidance.
pub const NO_RULES_ENV: &str = "HORON_AGENT_NO_RULES";

fn flag(key: &str) -> bool {
    std::env::var(key).is_ok_and(|value| value == "1")
}

pub(crate) fn context_files_disabled() -> bool {
    flag(NO_CONTEXT_FILES_ENV)
}

pub(crate) fn rules_disabled() -> bool {
    flag(NO_RULES_ENV)
}
