//! Per-session approval mode chosen by the ACP connection that drives a session.
//!
//! An editor can switch between "ask before shell and network calls" and "never
//! ask" while a session is open, without touching the daemon configuration. The
//! choice is owned by the connection that made it, the same way
//! [`super::ask_bridge`] and [`super::permission_bridge`] installations are: a
//! token guards every clear, and the lifecycle withdraws the entry on that
//! connection's disconnect or re-subscribe.
//!
//! The override applies only to turns the owning connection started
//! ([`super::acp_session::turn_owned_by`]). A desktop or daemon turn on the same
//! session keeps the configured mode, and so does every turn after the ACP
//! connection drops.
//!
//! Only `off` and `write` are accepted. `all` auto-approves every call
//! (crates/app-core/src/approval.rs), which would make an editor's "ask" mode
//! silently stop asking, so it is refused rather than mapped.

use super::acp_session;
use crate::config::ApprovalMode;
use crate::protocol::ServerEvent;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// The refusal a connection without an ACP installation gets for this request.
pub(crate) const NOT_ACP_MESSAGE: &str = "This request is only served on an ACP bridge connection";

struct Installed {
    token: u64,
    connection_id: String,
    mode: ApprovalMode,
}

fn registry() -> &'static Mutex<HashMap<String, Installed>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Installed>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> MutexGuard<'static, HashMap<String, Installed>> {
    registry().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Parse a mode an ACP client asked for.
pub(crate) fn parse_acp_mode(value: &str) -> Result<ApprovalMode, String> {
    match ApprovalMode::parse(value) {
        Some(mode @ (ApprovalMode::Off | ApprovalMode::Write)) => Ok(mode),
        Some(ApprovalMode::All) => Err(format!(
            "approval mode `{value}` is not available to ACP sessions; use `off` or `write`"
        )),
        None => Err(format!("unknown approval mode `{value}`; use `off` or `write`")),
    }
}

/// Record `mode` for `session_id`, owned by `connection_id`.
///
/// Replaces any previous entry and returns the token a later [`clear`] must
/// present.
pub(crate) fn set(session_id: &str, connection_id: &str, mode: ApprovalMode) -> u64 {
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    lock().insert(
        session_id.to_string(),
        Installed {
            token,
            connection_id: connection_id.to_string(),
            mode,
        },
    );
    token
}

/// Withdraw an entry, but only the one this token created.
pub(crate) fn clear(session_id: &str, token: u64) {
    let mut installed = lock();
    if installed.get(session_id).is_some_and(|entry| entry.token == token) {
        installed.remove(session_id);
    }
}

/// The mode the running turn in `session_id` must use instead of the
/// configured one, if any.
///
/// `None` unless the turn was started by the connection that set the mode.
pub(crate) fn get(session_id: &str) -> Option<ApprovalMode> {
    let (connection_id, mode) = {
        let installed = lock();
        let entry = installed.get(session_id)?;
        (entry.connection_id.clone(), entry.mode)
    };
    acp_session::turn_owned_by(session_id, &connection_id).then_some(mode)
}

/// Whether any entry exists for `session_id`, whoever owns the running turn.
#[cfg(test)]
pub(crate) fn is_installed(session_id: &str) -> bool {
    lock().contains_key(session_id)
}

/// Serve one `set_session_approval_mode` request for a connection.
///
/// `acp_attached` is whether this connection holds the ACP installation for
/// `session_id`; `installation` is the connection's own record of the entry it
/// set, which this updates. `None` clears the override so the configured mode
/// applies again.
pub(crate) fn handle_request(
    id: u64,
    mode: Option<String>,
    session_id: &str,
    connection_id: &str,
    acp_attached: bool,
    installation: &mut Option<(String, u64)>,
) -> ServerEvent {
    let error = |message: String| ServerEvent::Error {
        id,
        message,
        retry_after_secs: None,
    };
    if !acp_attached {
        return error(NOT_ACP_MESSAGE.to_string());
    }
    let parsed = match mode.as_deref().map(parse_acp_mode).transpose() {
        Ok(parsed) => parsed,
        Err(message) => return error(message),
    };
    if let Some((session, token)) = installation.take() {
        clear(&session, token);
    }
    if let Some(mode) = parsed {
        *installation = Some((session_id.to_string(), set(session_id, connection_id, mode)));
    }
    ServerEvent::Ack { id }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn set_session_approval_mode_rejects_unknown() {
        let mut installation = None;
        for bad in ["all", "ALL", "bogus", ""] {
            let event = handle_request(
                7,
                Some(bad.to_string()),
                "override-reject",
                "conn-acp",
                true,
                &mut installation,
            );
            match event {
                ServerEvent::Error { id, message, .. } => {
                    assert_eq!(id, 7);
                    assert!(message.contains("use `off` or `write`"), "{message}");
                }
                other => panic!("`{bad}` must be refused, got {other:?}"),
            }
            assert!(installation.is_none(), "a refused mode must not install anything");
            assert!(!is_installed("override-reject"));
        }
        assert_eq!(parse_acp_mode(" Write "), Ok(ApprovalMode::Write));
        assert_eq!(parse_acp_mode("off"), Ok(ApprovalMode::Off));
    }

    #[test]
    fn set_session_approval_mode_needs_an_acp_installation() {
        let mut installation = None;
        let event = handle_request(
            8,
            Some("write".to_string()),
            "override-plain",
            "conn-plain",
            false,
            &mut installation,
        );
        assert!(
            matches!(event, ServerEvent::Error { id: 8, ref message, .. } if message == NOT_ACP_MESSAGE),
            "{event:?}"
        );
        assert!(!is_installed("override-plain"));
    }

    #[test]
    fn approval_override_applies_only_to_the_owning_connections_turn() {
        let session = "override-owned-turn";
        let (tx, _rx) = mpsc::unbounded_channel();
        let acp_token = acp_session::install(session, "conn-acp", tx);
        let mut installation = None;
        let ack = handle_request(
            9,
            Some("write".to_string()),
            session,
            "conn-acp",
            true,
            &mut installation,
        );
        assert!(matches!(ack, ServerEvent::Ack { id: 9 }), "{ack:?}");
        assert_eq!(get(session), None, "no turn is running yet");

        let desktop_turn = acp_session::claim_turn(session, Some("conn-desktop"));
        assert_eq!(get(session), None, "a desktop turn keeps the configured mode");
        drop(desktop_turn);

        let daemon_turn = acp_session::claim_turn(session, None);
        assert_eq!(get(session), None, "a daemon turn keeps the configured mode");
        drop(daemon_turn);

        let acp_turn = acp_session::claim_turn(session, Some("conn-acp"));
        assert_eq!(get(session), Some(ApprovalMode::Write));

        let cleared = handle_request(10, None, session, "conn-acp", true, &mut installation);
        assert!(matches!(cleared, ServerEvent::Ack { id: 10 }));
        assert!(installation.is_none());
        assert_eq!(get(session), None, "None clears the override");

        drop(acp_turn);
        acp_session::clear(session, acp_token);
    }

    #[test]
    fn override_cleared_on_acp_disconnect() {
        let session = "override-disconnect";
        let (tx, _rx) = mpsc::unbounded_channel();
        let acp_token = acp_session::install(session, "conn-acp", tx);
        let mut installation = None;
        let _ = handle_request(
            11,
            Some("off".to_string()),
            session,
            "conn-acp",
            true,
            &mut installation,
        );
        let turn = acp_session::claim_turn(session, Some("conn-acp"));
        assert_eq!(get(session), Some(ApprovalMode::Off));

        // What the lifecycle does on disconnect: withdraw exactly what this
        // connection installed.
        let (owned_session, token) = installation.take().expect("the connection recorded its entry");
        clear(&owned_session, token);
        acp_session::clear(session, acp_token);
        assert!(!is_installed(session));
        assert_eq!(get(session), None);
        drop(turn);

        // A plain client attached afterwards never inherits the dropped mode.
        let (plain_tx, _plain_rx) = mpsc::unbounded_channel();
        let plain_token = acp_session::install(session, "conn-next", plain_tx);
        let next_turn = acp_session::claim_turn(session, Some("conn-next"));
        assert_eq!(
            get(session),
            None,
            "a later attachment must not inherit the dropped override"
        );
        drop(next_turn);
        acp_session::clear(session, plain_token);
    }

    #[test]
    fn an_older_connection_cannot_clear_a_newer_override() {
        let session = "override-replace";
        let old = set(session, "conn-old", ApprovalMode::Write);
        let new = set(session, "conn-new", ApprovalMode::Off);
        clear(session, old);
        assert!(is_installed(session));
        clear(session, new);
        assert!(!is_installed(session));
    }
}
