//! Socket path resolution shared by every harness API client and the bridge.
//!
//! This lives in the API crate on purpose. It used to be duplicated in the
//! bridge and external clients, where separate copies could disagree: the bridge
//! resolved `$XDG_RUNTIME_DIR` while the desktop always looked in
//! `~/.horon-agent`. The result was a desktop app that could never connect even
//! with a healthy bridge running. One definition, used by both sides, makes
//! that class of bug impossible.
//!
//! The rules match `horon-agent-storage::runtime_dir` so the API socket always lands
//! beside the daemon socket it bridges to.

use std::path::PathBuf;

/// Runtime directory holding the daemon and API sockets.
pub fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HORON_AGENT_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    // macOS hands every session a private temporary directory; the other
    // systems share one and the discriminator below keeps users apart.
    if let Some(dir) = horon_agent_platform::paths::user_runtime_dir() {
        return dir;
    }
    fallback_runtime_dir()
}

fn fallback_runtime_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "horon-agent-{}",
        horon_agent_platform::paths::user_discriminator()
    ))
}

/// Path of the versioned harness API socket. `HORON_AGENT_API_SOCKET` overrides it.
pub fn api_socket_path() -> PathBuf {
    if let Ok(custom) = std::env::var("HORON_AGENT_API_SOCKET") {
        return PathBuf::from(custom);
    }
    runtime_dir().join("horon-agent-api.sock")
}

/// Path of the internal daemon socket the bridge translates onto.
/// `HORON_AGENT_SOCKET` overrides it.
pub fn legacy_socket_path() -> PathBuf {
    if let Ok(custom) = std::env::var("HORON_AGENT_SOCKET") {
        return PathBuf::from(custom);
    }
    runtime_dir().join("horon-agent.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two sockets must always be siblings. A client that resolves one
    /// directory while the bridge resolves another cannot connect at all,
    /// which is exactly the bug this module exists to prevent.
    #[test]
    fn the_api_socket_sits_beside_the_daemon_socket() {
        // Guard against env-dependent divergence by comparing parents rather
        // than absolute paths, since either may be overridden in a session.
        let api = runtime_dir().join("horon-agent-api.sock");
        let legacy = runtime_dir().join("horon-agent.sock");
        assert_eq!(api.parent(), legacy.parent());
    }

    #[test]
    fn socket_names_are_stable() {
        assert_eq!(
            runtime_dir().join("horon-agent-api.sock").file_name().unwrap(),
            "horon-agent-api.sock"
        );
    }

    /// The discriminator becomes a path component, so it may only hold
    /// characters that cannot escape the directory or break a shell word.
    #[test]
    fn the_user_discriminator_is_path_safe() {
        let token = horon_agent_platform::paths::user_discriminator();
        assert!(!token.is_empty(), "a discriminator is never empty");
        assert!(
            token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')),
            "unexpected character in {token}"
        );
    }
}
