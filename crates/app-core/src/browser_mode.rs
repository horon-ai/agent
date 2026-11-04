//! Which side is driving a session's mirrored browser.
//!
//! The harness API bridge owns the authoritative mode: its `browser_mirror`
//! `takeover` and `resume` actions are what a user clicks. The bridge is a
//! separate process, so it pushes the mode here and the daemon caches it. A
//! cache read never crosses the socket, which is what keeps a session whose
//! panel vanished from deadlocking its turn.
//!
//! The cache is a lease, not a flag: a bridge that stopped pushing (because its
//! panel closed) must not leave the browser locked. An entry older than
//! [`LEASE`] means the agent drives again.

use crate::protocol::BrowserMode;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How long a pushed mode survives without being renewed.
///
/// Matches the bridge's own takeover lease, so both sides let go of a takeover
/// at the same point after a panel stops polling.
pub const LEASE: Duration = Duration::from_secs(60);

/// Ceiling on cached sessions. A push names a session id, and a client that
/// invents ids must not grow this map without bound.
const MAX_SESSIONS: usize = 1024;

struct Entry {
    mode: BrowserMode,
    at: Instant,
}

static MODES: LazyLock<Mutex<HashMap<String, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn modes() -> std::sync::MutexGuard<'static, HashMap<String, Entry>> {
    MODES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Record the mode the bridge pushed.
///
/// `Agent` clears the entry outright, so a resume allows the next tool call
/// immediately instead of waiting for a lease to run out.
pub fn set(session_id: &str, mode: BrowserMode) {
    let mut modes = modes();
    if mode == BrowserMode::Agent {
        modes.remove(session_id);
        return;
    }
    if modes.len() >= MAX_SESSIONS {
        modes.retain(|_, entry| entry.at.elapsed() <= LEASE);
    }
    modes.insert(
        session_id.to_string(),
        Entry {
            mode,
            at: Instant::now(),
        },
    );
}

/// The mode that applies right now.
///
/// No entry (no bridge ever pushed, or this session never took over) and a
/// stale entry both mean the agent drives, which is the permissive default an
/// older bridge relies on.
pub fn current(session_id: &str) -> BrowserMode {
    let mut modes = modes();
    match modes.get(session_id) {
        Some(entry) if entry.at.elapsed() <= LEASE => entry.mode,
        Some(_) => {
            modes.remove(session_id);
            BrowserMode::Agent
        }
        None => BrowserMode::Agent,
    }
}

/// Insert an entry with an age, so a test can prove the lease expires without
/// sleeping for a minute.
#[cfg(test)]
pub fn set_at_for_test(session_id: &str, mode: BrowserMode, at: Instant) {
    modes().insert(session_id.to_string(), Entry { mode, at });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str) -> String {
        format!("browser-mode-{name}-{}", std::process::id())
    }

    #[test]
    fn a_pushed_user_mode_applies_until_the_bridge_clears_it() {
        let id = session("cleared");

        set(&id, BrowserMode::User);
        assert_eq!(current(&id), BrowserMode::User);

        set(&id, BrowserMode::Agent);
        assert_eq!(
            current(&id),
            BrowserMode::Agent,
            "a resume takes effect on the next read, not after the lease"
        );
    }

    #[test]
    fn an_entry_older_than_the_lease_falls_back_to_the_agent() {
        let id = session("stale");
        set_at_for_test(&id, BrowserMode::User, Instant::now() - LEASE - Duration::from_secs(1));

        assert_eq!(
            current(&id),
            BrowserMode::Agent,
            "a vanished panel must not lock the browser forever"
        );
        assert!(
            modes().get(&id).is_none(),
            "an expired entry is dropped rather than kept for the next read"
        );
    }

    #[test]
    fn one_sessions_takeover_does_not_gate_another() {
        let taken = session("taken");
        let other = session("other");

        set(&taken, BrowserMode::User);
        assert_eq!(current(&taken), BrowserMode::User);
        assert_eq!(current(&other), BrowserMode::Agent);
    }
}
