//! Library runtime adapter. Wire requests and SDK endpoints belong to the API layer.
use crate::agent::Agent;
use crate::session::{Session, event_store_api as store};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;

/// Reset in place, excluding replacement and import until the durable reset finishes.
pub(crate) async fn clear_live_session(
    sessions: &SessionAgents,
    session_id: &str,
    agent: &Arc<Mutex<Agent>>,
) -> Result<bool> {
    let binding = Arc::clone(sessions).read_owned().await;
    anyhow::ensure!(
        binding
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, agent)),
        store::StoreError::busy("session binding changed; retry Clear")
    );
    let mut guard = Arc::clone(agent)
        .try_lock_owned()
        .map_err(|_| store::StoreError::busy("session is busy; retry Clear"))?;
    anyhow::ensure!(
        guard.session_id() == session_id,
        store::StoreError::busy("session identity changed")
    );
    tokio::task::spawn_blocking(move || {
        let _binding = binding;
        let preserve_debug = guard.is_debug();
        guard.clear_session_branch()?;
        Ok(preserve_debug)
    })
    .await
    .context("Clear worker stopped")?
}

/// Keep the Agent reservation across blocking import and all derived-state refresh.
pub async fn import_session_events(
    sessions: &SessionAgents,
    page: store::replication::SessionEventPage,
    retry_cursor: Option<String>,
) -> Result<store::replication::SessionImportResult> {
    let id = page.session_id.as_str().to_owned();
    // Take map -> Agent (try only) -> operation, with no Agent await.
    // Retain the read reservation in the blocking worker so replacement cannot
    // detach this same-ID Arc between lookup and the original commit.
    let binding = Arc::clone(sessions).read_owned().await;
    let agent = binding.get(&id).cloned();
    match agent {
        Some(agent) => {
            let mut guard = agent
                .try_lock_owned()
                .map_err(|_| store::StoreError::busy("session is running or saving"))?;
            if guard.session_id() != id {
                return Err(store::StoreError::busy("session binding changed; retry lookup").into());
            }
            tokio::task::spawn_blocking(move || {
                let _binding = binding;
                guard.import_session_replica(page, retry_cursor)
            })
            .await
            .context("replica worker stopped")?
        }
        None => {
            // A different live owner must not be bypassed just because it is absent
            // from this server's map. Markers are only an extra refusal, never a lock.
            if crate::storage::session_presence()
                .iter()
                .any(|owner| owner.session_id == id)
            {
                return Err(store::StoreError::busy("session belongs to another live owner").into());
            }
            tokio::task::spawn_blocking(move || {
                let _binding = binding;
                Session::import_replica_for_id(page, retry_cursor)
            })
            .await
            .context("replica worker stopped")?
        }
    }
}

#[cfg(test)]
#[path = "session_replication_tests.rs"]
mod tests;
