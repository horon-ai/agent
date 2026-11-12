//! Permanent session deletion, served on the lightweight control path.
//!
//! Archiving keeps a session loadable and only drops it from the list; this
//! handler is the destructive counterpart an operator reaches for when the
//! transcript itself must go. The rules that make it safe:
//!
//! - the caller has to say `confirm: true`, so a client that meant to archive
//!   cannot erase a transcript by sending the wrong request;
//! - a session that is loaded, streaming, or attached to a client is refused,
//!   because deleting under a live writer would race a turn still appending to
//!   the file being removed;
//! - only files keyed by this id are removed, and the id is validated before
//!   any path is built.

use super::{ClientConnectionInfo, SessionAgents};
use crate::protocol::ServerEvent;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

/// Send one reply, naming a dropped transport in the log.
///
/// A failed send means the connection closed before the answer could be
/// delivered. There is nobody left to tell and nothing to retry, but a delete
/// that answered nothing must still be visible in the log.
fn reply(client_event_tx: &mpsc::UnboundedSender<ServerEvent>, event: ServerEvent) {
    if client_event_tx.send(event).is_err() {
        crate::logging::warn("a session delete reply could not be delivered: the connection closed first");
    }
}

/// Why a delete was refused before any file was touched.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DeleteRefusal {
    Unconfirmed,
    Open,
    Invalid(String),
}

impl DeleteRefusal {
    fn message(&self) -> String {
        match self {
            DeleteRefusal::Unconfirmed => {
                "delete_session requires confirm: true; the transcript cannot be restored".to_string()
            }
            DeleteRefusal::Open => {
                "session is open: a turn is running, another process is streaming, or a client is attached".to_string()
            }
            DeleteRefusal::Invalid(detail) => detail.clone(),
        }
    }
}

/// Refuse a delete that would race a live writer, and plan the close otherwise.
///
/// A session is open when something can still append to it: a streaming marker
/// on disk (a turn owned by another process), a client attached to it, or a turn
/// running on it. Each of those makes the purge a race against a writer, so the
/// delete is refused rather than half-applied.
///
/// A resident agent is not open. An idle session stays in the map between
/// turns, and after its client goes it stays for the reconnect grace with
/// nothing attached and no turn running. Purging under it would let a dropped
/// writer flush into a directory that no longer exists, so that entry is closed
/// first: the plan carries it, and `close_evicted` joins its writer before the
/// caller removes anything.
pub(super) async fn plan_delete(
    session_id: &str,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
) -> Result<DeletePlan, DeleteRefusal> {
    if crate::storage::streaming_session_ids()
        .iter()
        .any(|id| id == session_id)
    {
        return Err(DeleteRefusal::Open);
    }
    // Attachment is read before the session map and under its write lock, so a
    // resume cannot attach between the two reads: `claim_live_target_agent`
    // takes the same lock first.
    let connections = client_connections.write().await;
    if connections.values().any(|info| info.session_id == session_id) {
        return Err(DeleteRefusal::Open);
    }
    // A turn can still append even when the entry is gone from the map, so this
    // is checked before residency. A resident agent is not open by itself: an
    // idle session stays in the map between turns, and after its client
    // detaches it stays for the reconnect grace with nothing writing to it.
    if crate::turn_cancel_registry::has_active_turn(session_id) {
        return Err(DeleteRefusal::Open);
    }
    if !sessions.read().await.contains_key(session_id) {
        drop(connections);
        return Ok(DeletePlan::Purge);
    }
    // Publish the close barrier before the entry leaves the map, so a resumer
    // that arrives after this point waits for the close instead of attaching to
    // an entry that is about to be purged.
    let guard = super::client_disconnect_cleanup::SessionCloseGuard::register(session_id).await;
    let removed = super::remove_session_entry(sessions, session_id).await;
    drop(connections);
    match removed {
        Some(agent) => Ok(DeletePlan::Evict { agent, guard }),
        None => Ok(DeletePlan::Purge),
    }
}

/// What a validated delete does next.
pub(super) enum DeletePlan {
    /// Nothing is loaded: purge the files.
    Purge,
    /// This daemon owned the idle entry; close it before purging.
    Evict {
        agent: Arc<tokio::sync::Mutex<crate::agent::Agent>>,
        guard: super::client_disconnect_cleanup::SessionCloseGuard,
    },
}

/// Take an evicted entry's agent lock and drop the entry, so its writer thread
/// and file handle are gone before the caller purges the files.
///
/// Nothing is saved here: the session is being erased, so a final save would
/// write a transcript the purge then deletes, and it would fail outright for a
/// session that was never persisted. Dropping the `Agent` drops its writer,
/// whose `Drop` flushes and joins the writer thread.
///
/// The close barrier stays published for the caller's purge, so a resume of this
/// id either waits here or, once released, finds no transcript to load. A busy
/// agent (a background task holding the lock past the deadline) returns `None`
/// and the caller reports it as open instead of deleting under it.
pub(super) async fn close_evicted(
    agent: Arc<tokio::sync::Mutex<crate::agent::Agent>>,
    guard: super::client_disconnect_cleanup::SessionCloseGuard,
) -> Option<super::client_disconnect_cleanup::SessionCloseGuard> {
    let Ok(owned) = tokio::time::timeout(std::time::Duration::from_secs(2), Arc::clone(&agent).lock_owned()).await
    else {
        drop(guard);
        return None;
    };
    drop(owned);
    // The close owns the last reference, and dropping it joins the writer
    // thread, so that join runs off the runtime worker.
    super::client_disconnect_cleanup::blocking_session_io(move || drop(agent));
    Some(guard)
}

/// Files app-core owns that `purge_session_state` cannot reach.
///
/// The reload context and the reload recovery record are written here rather
/// than in base, so their paths are not part of the base purge. Both name the
/// session id in the filename and would otherwise outlive it.
fn remove_daemon_only_state(session_id: &str) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut removed = Vec::new();
    let context = super::reload_recovery::ReloadContext::path_for_session(session_id)?;
    for candidate in [context.clone(), context.with_extension("bak")] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => removed.push(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let recovery = super::reload_recovery::path_for_session(session_id);
    if let Ok(recovery) = recovery {
        for candidate in [recovery.clone(), recovery.with_extension("bak")] {
            match std::fs::remove_file(&candidate) {
                Ok(()) => removed.push(candidate),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(removed)
}

/// Handle one `DeleteSession` request, answering `Done` or `Error`.
pub(super) async fn handle_delete_session(
    id: u64,
    session_id: String,
    confirm: bool,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let started = std::time::Instant::now();
    let plan = if !confirm {
        Err(DeleteRefusal::Unconfirmed)
    } else {
        match horon_agent_session_store::SessionId::parse(&session_id) {
            Ok(_) => plan_delete(&session_id, sessions, client_connections).await,
            Err(error) => Err(DeleteRefusal::Invalid(format!("invalid session id: {error}"))),
        }
    };

    let plan = match plan {
        Ok(plan) => plan,
        Err(refusal) => {
            crate::logging::event_warn(
                "SESSION_LIFECYCLE",
                vec![
                    ("phase", "delete_refused".to_string()),
                    ("request_id", id.to_string()),
                    ("session_id", session_id.clone()),
                    ("reason", refusal.message()),
                ],
            );
            reply(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message: refusal.message(),
                    retry_after_secs: None,
                },
            );
            return;
        }
    };

    // An idle entry this daemon owned is closed first, and its close barrier is
    // held across the purge so a resume cannot reattach to files being removed.
    let _close_guard = match plan {
        DeletePlan::Purge => None,
        DeletePlan::Evict { agent, guard } => match close_evicted(agent, guard).await {
            Some(guard) => Some(guard),
            None => {
                crate::logging::event_warn(
                    "SESSION_LIFECYCLE",
                    vec![
                        ("phase", "delete_refused".to_string()),
                        ("request_id", id.to_string()),
                        ("session_id", session_id),
                        ("reason", DeleteRefusal::Open.message()),
                    ],
                );
                reply(
                    client_event_tx,
                    ServerEvent::Error {
                        id,
                        message: DeleteRefusal::Open.message(),
                        retry_after_secs: None,
                    },
                );
                return;
            }
        },
    };

    let purged = crate::session::purge_session_state(&session_id);
    let result = purged.and_then(|report| {
        let mut removed = report.removed_count();
        match remove_daemon_only_state(&session_id) {
            Ok(extra) => removed += extra.len(),
            Err(error) => return Err(error),
        }
        Ok(removed)
    });

    match result {
        Ok(removed) => {
            crate::session_list_cache::invalidate();
            crate::logging::event_info(
                "SESSION_LIFECYCLE",
                vec![
                    ("phase", "delete_done".to_string()),
                    ("request_id", id.to_string()),
                    ("session_id", session_id),
                    ("removed", removed.to_string()),
                    ("elapsed_ms", started.elapsed().as_millis().to_string()),
                ],
            );
            reply(client_event_tx, ServerEvent::Done { id });
        }
        Err(error) => {
            crate::logging::event_warn(
                "SESSION_LIFECYCLE",
                vec![
                    ("phase", "delete_error".to_string()),
                    ("request_id", id.to_string()),
                    ("session_id", session_id),
                    ("error", crate::util::format_error_chain(&error)),
                    ("elapsed_ms", started.elapsed().as_millis().to_string()),
                ],
            );
            reply(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message: crate::util::format_error_chain(&error),
                    retry_after_secs: None,
                },
            );
        }
    }
}

#[cfg(test)]
#[path = "client_session_delete_tests.rs"]
mod tests;
