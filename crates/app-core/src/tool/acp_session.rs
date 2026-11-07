//! Registry of the ACP connection that owns a session, and which connection
//! owns the turn running in it right now.
//!
//! Two questions need answering from deep inside tool code, where no connection
//! handle exists:
//!
//! - Is this session driven by an ACP client at all? (`is_acp`) Only then may
//!   ACP-only events such as plan or diff updates leave the daemon, and only to
//!   that connection's own sender (`sender_for`); every other client keeps the
//!   events it already had.
//! - Does the turn running now belong to the ACP connection that owns the
//!   session? (`turn_owned_by`) A session can be live in several attachments at
//!   once (`client_session.rs`), and the daemon also starts turns on its own
//!   (server, comm, relay, action paths). Presence of an ACP connection says
//!   nothing about who drove the current turn, so ownership is recorded per
//!   turn: [`claim_turn`] hands the accepted turn's starter a guard, and the
//!   guard withdraws the claim when that turn ends — normally, with an error, or
//!   by abort. A daemon-started turn claims nothing and therefore owns nothing.
//!
//! A turn that starts while another turn is already active on the same session
//! is refused the claim rather than overwriting it, so a second attachment
//! cannot steal an in-flight ACP turn's file and terminal routing.
//!
//! The registry is process-global for the same reason [`super::ask_bridge`] is:
//! `ToolContext` is built in more than sixty places. Keys are session ids.
//! Installs are token-guarded so a disconnecting connection cannot clear a newer
//! one, and installing again drops the recorded turn — a re-subscribe starts a
//! new attachment, not a continuation of the previous attachment's turns.

use crate::protocol::ServerEvent;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use tokio::sync::mpsc;

struct ActiveTurn {
    id: u64,
    /// Starter of the turn; `None` for a client that is not the ACP connection.
    connection_id: Option<String>,
}

struct Installed {
    token: u64,
    connection_id: String,
    tx: mpsc::UnboundedSender<ServerEvent>,
    turn: Option<ActiveTurn>,
    /// ACP protocol version the connection negotiated; `None` is v1.
    protocol_version: Option<u32>,
}

fn registry() -> &'static Mutex<HashMap<String, Installed>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Installed>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> MutexGuard<'static, HashMap<String, Installed>> {
    registry().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Record that `connection_id` is the ACP connection driving `session_id`.
///
/// Replaces any previous installation: the newest connection is the one the
/// user is looking at. Returns the token a later [`clear`] must present.
#[cfg(test)]
pub(crate) fn install(session_id: &str, connection_id: &str, tx: mpsc::UnboundedSender<ServerEvent>) -> u64 {
    install_with_version(session_id, connection_id, tx, None)
}

/// [`install`] for a connection that negotiated `protocol_version`.
pub(crate) fn install_with_version(
    session_id: &str,
    connection_id: &str,
    tx: mpsc::UnboundedSender<ServerEvent>,
    protocol_version: Option<u32>,
) -> u64 {
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    lock().insert(
        session_id.to_string(),
        Installed {
            token,
            connection_id: connection_id.to_string(),
            tx,
            turn: None,
            protocol_version,
        },
    );
    token
}

/// Withdraw an installation, but only the one this token created.
pub(crate) fn clear(session_id: &str, token: u64) {
    let mut installed = lock();
    if installed.get(session_id).is_some_and(|entry| entry.token == token) {
        installed.remove(session_id);
    }
}

/// Whether an ACP connection owns this session right now.
pub(crate) fn is_acp(session_id: &str) -> bool {
    lock().contains_key(session_id)
}

/// The ACP connection's event sender for this session, if one is installed.
pub(crate) fn sender_for(session_id: &str) -> Option<mpsc::UnboundedSender<ServerEvent>> {
    lock().get(session_id).map(|entry| entry.tx.clone())
}

/// Deliver an event that only the ACP connection may see.
///
/// Returns false when no ACP connection owns this session. Such an event never
/// reaches the shared turn broadcast, so a desktop, harness or CLI client
/// attached to the same session sees no plan or diff traffic it cannot render.
pub(crate) fn send(session_id: &str, event: ServerEvent) -> bool {
    sender_for(session_id).is_some_and(|sender| sender.send(event).is_ok())
}

/// [`send`] with an event shaped for the owning connection's negotiated
/// protocol version (`None` is v1). Version and sender come from one lookup,
/// so a re-subscribe cannot pair one connection's version with another's
/// sender.
pub(crate) fn send_for_version(session_id: &str, shape: impl FnOnce(Option<u32>) -> ServerEvent) -> bool {
    let owner = lock()
        .get(session_id)
        .map(|entry| (entry.protocol_version, entry.tx.clone()));
    owner.is_some_and(|(version, sender)| sender.send(shape(version)).is_ok())
}

/// Whether the ACP connection that owns this session negotiated protocol v2.
///
/// Tool code asks this once when a call starts and keeps the answer, so the
/// per-chunk path of a long-running command costs one bool test instead of a
/// registry lookup. A later re-subscribe that changes the version cannot make
/// that call report to the wrong connection: `send_v2` re-checks ownership on
/// every event.
pub(crate) fn owns_v2(session_id: &str) -> bool {
    lock()
        .get(session_id)
        .is_some_and(|entry| entry.protocol_version == Some(2))
}

/// [`send`], but only when the owning ACP connection negotiated protocol v2.
pub(crate) fn send_v2(session_id: &str, event: ServerEvent) -> bool {
    let sender = lock()
        .get(session_id)
        .filter(|entry| entry.protocol_version == Some(2))
        .map(|entry| entry.tx.clone());
    sender.is_some_and(|sender| sender.send(event).is_ok())
}

/// Claim the turn that is starting for `connection_id`, if it may have it.
///
/// Call this only after the daemon accepted the turn, and hold the returned
/// guard for exactly the turn's lifetime. A session with no ACP installation has
/// nothing to claim; a session whose turn is already running keeps the existing
/// owner, and the returned guard owns nothing.
pub(crate) fn claim_turn(session_id: &str, connection_id: Option<&str>) -> TurnOwnership {
    static NEXT_TURN: AtomicU64 = AtomicU64::new(1);
    let mut installed = lock();
    let Some(entry) = installed.get_mut(session_id) else {
        return TurnOwnership::unclaimed(session_id);
    };
    if entry.turn.is_some() {
        return TurnOwnership::unclaimed(session_id);
    }
    let id = NEXT_TURN.fetch_add(1, Ordering::Relaxed);
    entry.turn = Some(ActiveTurn {
        id,
        connection_id: connection_id.map(str::to_string),
    });
    TurnOwnership {
        session_id: session_id.to_string(),
        turn_id: Some(id),
    }
}

/// Whether the turn running in `session_id` was started by the ACP connection
/// that owns the session and by `connection_id`.
///
/// False when no turn is recorded, when another connection started it, and when
/// the asked connection is not the installed one. Every false answer means "use
/// local behaviour", which is what every non-ACP client must keep.
pub(crate) fn turn_owned_by(session_id: &str, connection_id: &str) -> bool {
    lock().get(session_id).is_some_and(|entry| {
        entry.connection_id == connection_id
            && entry
                .turn
                .as_ref()
                .is_some_and(|turn| turn.connection_id.as_deref() == Some(connection_id))
    })
}

/// The claim [`claim_turn`] handed to one turn.
///
/// Dropping it withdraws the claim, which is what binds ownership to the turn
/// rather than to the session: the guard lives inside the spawned turn task, so
/// a finished, failed or aborted turn leaves no owner behind for the daemon's own
/// later turns to inherit.
#[must_use = "ownership lasts exactly as long as the guard is held"]
pub(crate) struct TurnOwnership {
    session_id: String,
    turn_id: Option<u64>,
}

impl TurnOwnership {
    fn unclaimed(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            turn_id: None,
        }
    }

    /// Whether this guard actually claimed the turn.
    #[cfg(test)]
    pub(crate) fn is_owning(&self) -> bool {
        self.turn_id.is_some()
    }
}

impl Drop for TurnOwnership {
    fn drop(&mut self) {
        let Some(turn_id) = self.turn_id else {
            return;
        };
        let mut installed = lock();
        if let Some(entry) = installed.get_mut(&self.session_id)
            && entry.turn.as_ref().is_some_and(|turn| turn.id == turn_id)
        {
            entry.turn = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> (mpsc::UnboundedSender<ServerEvent>, mpsc::UnboundedReceiver<ServerEvent>) {
        mpsc::unbounded_channel()
    }

    #[test]
    fn it_records_the_installing_connection_and_its_sender() {
        let (tx, _rx) = channel();
        let token = install("acp-session-basic", "conn-a", tx);
        assert!(is_acp("acp-session-basic"));
        assert!(sender_for("acp-session-basic").is_some());
        assert!(!is_acp("acp-session-other"));
        assert!(sender_for("acp-session-other").is_none());
        clear("acp-session-basic", token);
        assert!(!is_acp("acp-session-basic"));
    }

    #[test]
    fn an_older_connection_cannot_clear_a_newer_install() {
        let (old_tx, _old_rx) = channel();
        let old = install("acp-session-replace", "conn-old", old_tx);
        let (new_tx, _new_rx) = channel();
        let new = install("acp-session-replace", "conn-new", new_tx);
        clear("acp-session-replace", old);
        assert!(is_acp("acp-session-replace"));
        clear("acp-session-replace", new);
        assert!(!is_acp("acp-session-replace"));
    }

    #[test]
    fn a_turn_belongs_only_to_the_connection_that_started_it() {
        let (tx, _rx) = channel();
        let token = install("acp-session-turns", "conn-acp", tx);
        assert!(
            !turn_owned_by("acp-session-turns", "conn-acp"),
            "no turn has started yet, so nothing is owned"
        );
        let turn = claim_turn("acp-session-turns", Some("conn-acp"));
        assert!(turn.is_owning());
        assert!(turn_owned_by("acp-session-turns", "conn-acp"));
        assert!(!turn_owned_by("acp-session-turns", "conn-desktop"));
        drop(turn);
        clear("acp-session-turns", token);
    }

    #[test]
    fn turn_owner_is_cleared_when_the_turn_ends() {
        let (tx, _rx) = channel();
        let token = install("acp-session-turn-end", "conn-acp", tx);
        let turn = claim_turn("acp-session-turn-end", Some("conn-acp"));
        assert!(turn_owned_by("acp-session-turn-end", "conn-acp"));

        drop(turn);
        assert!(
            !turn_owned_by("acp-session-turn-end", "conn-acp"),
            "a finished turn must leave no owner behind"
        );

        let _daemon_turn = claim_turn("acp-session-turn-end", None);
        assert!(
            !turn_owned_by("acp-session-turn-end", "conn-acp"),
            "a daemon-started turn on the same session owns nothing for the editor"
        );
        clear("acp-session-turn-end", token);
    }

    #[test]
    fn a_second_turn_cannot_steal_an_active_turn() {
        let (tx, _rx) = channel();
        let token = install("acp-session-steal", "conn-acp", tx);
        let acp_turn = claim_turn("acp-session-steal", Some("conn-acp"));
        assert!(turn_owned_by("acp-session-steal", "conn-acp"));

        let desktop_turn = claim_turn("acp-session-steal", Some("conn-desktop"));
        assert!(!desktop_turn.is_owning(), "an active turn keeps its owner");
        assert!(
            turn_owned_by("acp-session-steal", "conn-acp"),
            "a concurrent turn must not take the editor's turn away mid-flight"
        );
        assert!(!turn_owned_by("acp-session-steal", "conn-desktop"));

        drop(desktop_turn);
        assert!(
            turn_owned_by("acp-session-steal", "conn-acp"),
            "releasing the refused turn must not release the active one"
        );

        drop(acp_turn);
        assert!(!turn_owned_by("acp-session-steal", "conn-acp"));
        clear("acp-session-steal", token);
    }

    #[test]
    fn a_turn_starts_unowned_again_after_a_resubscribe() {
        let (first_tx, _first_rx) = channel();
        let first = install("acp-session-resubscribe", "conn-first", first_tx);
        let first_turn = claim_turn("acp-session-resubscribe", Some("conn-first"));
        let (second_tx, _second_rx) = channel();
        let second = install("acp-session-resubscribe", "conn-second", second_tx);
        assert!(
            !turn_owned_by("acp-session-resubscribe", "conn-first"),
            "a re-subscribe must not inherit the previous attachment's turn"
        );
        assert!(!turn_owned_by("acp-session-resubscribe", "conn-second"));
        drop(first_turn);
        assert!(
            !turn_owned_by("acp-session-resubscribe", "conn-second"),
            "the previous attachment's guard must not touch the new installation"
        );
        clear("acp-session-resubscribe", second);
        clear("acp-session-resubscribe", first);
        assert!(!is_acp("acp-session-resubscribe"));
    }

    #[test]
    fn acp_session_cleared_on_disconnect() {
        let (tx, _rx) = channel();
        let token = install("acp-session-disconnect", "conn-dropped", tx);
        let turn = claim_turn("acp-session-disconnect", Some("conn-dropped"));
        assert!(turn_owned_by("acp-session-disconnect", "conn-dropped"));
        clear("acp-session-disconnect", token);
        assert!(!is_acp("acp-session-disconnect"));
        assert!(sender_for("acp-session-disconnect").is_none());
        assert!(!turn_owned_by("acp-session-disconnect", "conn-dropped"));
        drop(turn);
        assert!(!turn_owned_by("acp-session-disconnect", "conn-dropped"));
    }
}
