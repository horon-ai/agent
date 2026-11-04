//! Async facade over the durable input queue.
//!
//! [`horon_agent_base::durable_input`] owns the transitions; this module is how
//! the async server and agent layers reach them without doing journal I/O on a
//! Tokio worker. Every call hands the blocking store to
//! `tokio::task::spawn_blocking` and awaits its result, so no fsync and no
//! writer-lock wait ever happens on a runtime thread, and no lock is held across
//! the await.
//!
//! A session whose journal cannot be opened keeps the pre-journal behaviour: the
//! caller is told the durable layer is unavailable, and the legacy JSON path is
//! used exactly as before.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use horon_agent_agent_runtime::SoftInterruptMessage;
use horon_agent_base::durable_input::{
    AcceptedInput, DurableInputStore, InputError, InputErrorKind, InputRecovery, LegacyExport, ResolveAction,
    ResolvedInput,
};
use horon_agent_base::durable_runtime::{InputPhase, JournalErrorKind};

/// Stores kept across calls, keyed by journal directory.
///
/// Reopening replays the whole journal, so the handle is reused; every call
/// still catches up on rows other writers appended before it appends.
static INPUT_STORES: LazyLock<Mutex<HashMap<PathBuf, DurableInputStore>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Drop the cached store for one session, so a closed session's handle and its
/// file descriptors are released. The next call reopens and replays.
pub fn forget_session(session_id: &str) {
    let Ok(home) = horon_agent_base::storage::horon_agent_dir() else {
        return;
    };
    let key = home
        .join(horon_agent_base::durable_runtime::JOURNAL_DIR_NAME)
        .join(session_id);
    INPUT_STORES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&key);
}

fn with_store<T>(
    session_id: &str,
    body: impl FnOnce(&mut DurableInputStore) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let home = horon_agent_base::storage::horon_agent_dir()?;
    let key = home
        .join(horon_agent_base::durable_runtime::JOURNAL_DIR_NAME)
        .join(session_id);
    let mut stores = INPUT_STORES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !stores.contains_key(&key) {
        stores.insert(key.clone(), DurableInputStore::open(session_id)?);
    }
    let Some(store) = stores.get_mut(&key) else {
        return Err(anyhow::anyhow!("the input store for {session_id} disappeared"));
    };
    let outcome = body(store);
    if journal_is_broken(&outcome) {
        stores.remove(&key);
    }
    outcome
}

/// A corrupt, unknown-version, or gapped journal must not be retried as a
/// transient failure, so its handle is dropped and the next call replays afresh.
fn journal_is_broken<T>(outcome: &anyhow::Result<T>) -> bool {
    let Err(error) = outcome else {
        return false;
    };
    let Some(failure) = error.downcast_ref::<InputError>() else {
        return false;
    };
    let InputErrorKind::Journal(kind) = failure.kind() else {
        return false;
    };
    matches!(
        kind,
        JournalErrorKind::Corrupt | JournalErrorKind::UnsupportedVersion | JournalErrorKind::SequenceGap
    )
}

fn join_error(error: tokio::task::JoinError) -> anyhow::Error {
    anyhow::anyhow!("the durable input task did not finish: {error}")
}

fn flatten<T>(outcome: std::result::Result<anyhow::Result<T>, tokio::task::JoinError>) -> anyhow::Result<T> {
    outcome.map_err(join_error)?
}

fn warn_unavailable(what: &str, session_id: &str, error: &anyhow::Error) {
    crate::logging::warn(&format!(
        "DURABLE_INPUT_{what}_UNAVAILABLE session={session_id} error={error}"
    ));
}

/// Restore the pending set from the synchronous session-restore path.
///
/// Session restore already reads and parses the whole conversation from disk in
/// the same critical section, once per attach, so the journal replay and the
/// few seal rows it may write are the same class of work as the read beside
/// them. Every per-message path (accept, start, commit, cancel, clear) goes
/// through `spawn_blocking` instead.
///
/// `None` means the journal could not be opened, so the caller must fall back to
/// the legacy file exactly as it did before this layer existed.
pub fn recover_blocking(session_id: &str) -> Option<InputRecovery> {
    let replay = || {
        with_store(session_id, |store| {
            let report = store.recover(&[])?;
            let _ = store.mirror();
            Ok(report)
        })
    };
    // This function is called from the async session-restore path while the
    // session mutex is held. Replaying the journal and writing the seal rows is
    // blocking file I/O, so on a Tokio worker it must not run on that worker:
    // block_in_place hands the worker core to the blocking pool for the
    // duration. Off a runtime (a plain sync caller, a test) it runs directly.
    let outcome = match tokio::runtime::Handle::try_current() {
        // block_in_place is only legal on a multi-threaded runtime; a
        // current-thread runtime (the common shape in tests) would panic, and
        // there the blocking work cannot starve anything else because nothing
        // else runs on that thread.
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(replay)
        }
        _ => replay(),
    };
    match outcome {
        Ok(report) => Some(report),
        Err(error) => {
            crate::logging::warn(&format!(
                "DURABLE_INPUT_RECOVER_UNAVAILABLE session={session_id} error={error}"
            ));
            None
        }
    }
}

/// Accept one message, syncing it before the caller queues it.
///
/// An error means the journal did not take the row, so the caller must not
/// queue anything and must not acknowledge success.
pub async fn accept(session_id: &str, mut message: SoftInterruptMessage) -> anyhow::Result<AcceptedInput> {
    let owned = session_id.to_string();
    let (accepted, client_id) = tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            let accepted = store.accept(&mut message)?;
            if !accepted.already_live {
                let _ = store.mirror();
            }
            Ok((accepted, message.client_id.clone()))
        })
    })
    .await
    .map_err(join_error)??;
    if !accepted.already_live {
        crate::logging::info(&format!(
            "DURABLE_INPUT_ACCEPTED session={} input_id={} client_id={}",
            session_id,
            accepted.input_id,
            client_id.as_deref().unwrap_or("-")
        ));
    }
    Ok(accepted)
}

/// Accept and hand back the message carrying its journal id.
pub async fn accept_message(session_id: &str, message: SoftInterruptMessage) -> anyhow::Result<SoftInterruptMessage> {
    let owned = session_id.to_string();
    tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            let mut message = message;
            store.accept(&mut message)?;
            let _ = store.mirror();
            Ok(message)
        })
    })
    .await
    .map_err(join_error)?
}

/// Record that these inputs are about to be injected, before the injection.
///
/// The returned ids are the ones that may be injected; an id whose `started`
/// row failed is left out, because injecting it would put a message in the
/// transcript that no journal row accounts for.
pub async fn start(session_id: &str, input_ids: Vec<String>, run_id: Option<String>) -> Vec<String> {
    let owned = session_id.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            let mut started = Vec::new();
            for input_id in &input_ids {
                match store.start(input_id, run_id.as_deref()) {
                    Ok(()) => started.push(input_id.clone()),
                    Err(error) => crate::logging::warn(&format!(
                        "DURABLE_INPUT_START_FAILED session={owned} input_id={input_id} error={error}"
                    )),
                }
            }
            Ok(started)
        })
    })
    .await;
    match flatten(outcome) {
        Ok(started) => started,
        Err(error) => {
            warn_unavailable("START", session_id, &error);
            Vec::new()
        }
    }
}

/// Remember which transcript message each input's injection produced.
pub async fn note_injected_copies(session_id: &str, input_ids: Vec<String>, message_id: String) {
    let owned = session_id.to_string();
    let logged = message_id.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            for input_id in &input_ids {
                store.note_injected_copy(input_id, &message_id)?;
            }
            Ok(())
        })
    })
    .await;
    if let Err(error) = flatten(outcome) {
        crate::logging::warn(&format!(
            "DURABLE_INPUT_COPY_NOT_RECORDED session={session_id} message_id={logged} error={error}"
        ));
    }
}

/// Commit the inputs a successful turn carried, after its conversation save.
pub async fn commit(session_id: &str, input_ids: Vec<String>, run_id: Option<String>) {
    let owned = session_id.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            for input_id in &input_ids {
                if let Err(error) = store.commit(input_id, run_id.as_deref()) {
                    crate::logging::warn(&format!(
                        "DURABLE_INPUT_COMMIT_FAILED session={owned} input_id={input_id} error={error}"
                    ));
                }
            }
            let _ = store.mirror();
            Ok(())
        })
    })
    .await;
    if let Err(error) = flatten(outcome) {
        warn_unavailable("COMMIT", session_id, &error);
    }
}

/// Hand a failed turn's inputs back to the pending set.
pub async fn return_to_pending(session_id: &str, input_ids: Vec<String>, reason: &str) {
    let owned = session_id.to_string();
    let reason = reason.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            for input_id in &input_ids {
                if let Err(error) = store.return_to_pending(input_id, &reason) {
                    crate::logging::warn(&format!(
                        "DURABLE_INPUT_RETURN_FAILED session={owned} input_id={input_id} error={error}"
                    ));
                }
            }
            let _ = store.mirror();
            Ok(())
        })
    })
    .await;
    if let Err(error) = flatten(outcome) {
        warn_unavailable("RETURN", session_id, &error);
    }
}

/// Cancel the pending inputs whose caller id is in `client_ids`.
///
/// Returns the ids that were cancelled and the ones that were not live, so the
/// caller can report each requested id exactly once.
pub async fn cancel_by_client_ids(session_id: &str, client_ids: Vec<String>) -> (Vec<String>, Vec<String>) {
    let owned = session_id.to_string();
    let requested = client_ids.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            let live: Vec<(String, Option<String>)> = {
                let state = store.state()?;
                state
                    .inputs
                    .iter()
                    .map(|(id, entry)| (id.clone(), entry.accepted.client_id.clone()))
                    .collect()
            };
            let mut cancelled = Vec::new();
            let mut not_live = Vec::new();
            for client_id in &requested {
                let matches: Vec<String> = live
                    .iter()
                    .filter(|(_, live_client)| live_client.as_deref() == Some(client_id.as_str()))
                    .map(|(id, _)| id.clone())
                    .collect();
                if matches.is_empty() {
                    not_live.push(client_id.clone());
                    continue;
                }
                let mut all_cancelled = true;
                for input_id in matches {
                    if store.cancel(&input_id, "cancelled_by_client").is_err() {
                        all_cancelled = false;
                    }
                }
                if all_cancelled {
                    cancelled.push(client_id.clone());
                } else {
                    not_live.push(client_id.clone());
                }
            }
            let _ = store.mirror();
            Ok((cancelled, not_live))
        })
    })
    .await;
    match flatten(outcome) {
        Ok(result) => result,
        Err(error) => {
            warn_unavailable("CANCEL", session_id, &error);
            (Vec::new(), client_ids)
        }
    }
}

/// Cancel every input that has not started, for a queue clear.
pub async fn clear_pending(session_id: &str) -> usize {
    let owned = session_id.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            let pending: Vec<String> = {
                let state = store.state()?;
                state
                    .inputs
                    .iter()
                    .filter(|(_, entry)| matches!(entry.phase, InputPhase::Accepted | InputPhase::Returned))
                    .map(|(id, _)| id.clone())
                    .collect()
            };
            let mut cleared = 0;
            for input_id in pending {
                if store.cancel(&input_id, "queue_cleared").is_ok() {
                    cleared += 1;
                }
            }
            let _ = store.mirror();
            Ok(cleared)
        })
    })
    .await;
    match flatten(outcome) {
        Ok(cleared) => cleared,
        Err(error) => {
            warn_unavailable("CLEAR", session_id, &error);
            0
        }
    }
}

/// Restore the pending set after a restart.
pub async fn recover(session_id: &str, in_flight: Vec<String>) -> anyhow::Result<InputRecovery> {
    let owned = session_id.to_string();
    tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            let report = store.recover(&in_flight)?;
            let _ = store.mirror();
            Ok(report)
        })
    })
    .await
    .map_err(join_error)?
}

/// Mirror the live set to the legacy JSON file.
pub async fn mirror(session_id: &str) {
    let owned = session_id.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            store.mirror()?;
            Ok(())
        })
    })
    .await;
    if let Err(error) = flatten(outcome) {
        crate::logging::warn(&format!(
            "DURABLE_INPUT_MIRROR_FAILED session={session_id} error={error}"
        ));
    }
}

/// The transcript message an input's injection produced, if one is recorded.
pub async fn injected_copy(session_id: &str, input_id: &str) -> Option<String> {
    let owned = session_id.to_string();
    let wanted = input_id.to_string();
    let outcome =
        tokio::task::spawn_blocking(move || with_store(&owned, |store| Ok(store.injected_copy_of(&wanted)?))).await;
    match flatten(outcome) {
        Ok(copy) => copy,
        Err(error) => {
            crate::logging::warn(&format!(
                "DURABLE_INPUT_COPY_LOOKUP_FAILED session={session_id} input_id={input_id} error={error}"
            ));
            None
        }
    }
}

/// Apply an explicit decision to an undetermined input.
///
/// `remove_injected_copy` runs before the transition and reports whether the
/// transcript copy it named was removed.
pub async fn resolve(
    session_id: &str,
    input_id: &str,
    action: ResolveAction,
    remove_injected_copy: impl FnOnce(&str) -> bool + Send + 'static,
) -> anyhow::Result<ResolvedInput> {
    let owned = session_id.to_string();
    let wanted = input_id.to_string();
    tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            let resolved = store.resolve(&wanted, action, remove_injected_copy)?;
            let _ = store.mirror();
            Ok(resolved)
        })
    })
    .await
    .map_err(join_error)?
}

/// Write the pending set to the legacy JSON, or report why that is refused.
pub async fn export_legacy(session_id: &str) -> anyhow::Result<std::result::Result<LegacyExport, Vec<String>>> {
    let owned = session_id.to_string();
    tokio::task::spawn_blocking(move || {
        with_store(&owned, |store| {
            Ok(store.export_legacy()?.map_err(|refused| refused.reasons))
        })
    })
    .await
    .map_err(join_error)?
}

/// The live inputs of a session, in journal order, for a state view.
pub async fn live_messages(session_id: &str) -> Vec<SoftInterruptMessage> {
    let owned = session_id.to_string();
    let outcome = tokio::task::spawn_blocking(move || with_store(&owned, |store| Ok(store.live_messages()?))).await;
    match flatten(outcome) {
        Ok(messages) => messages,
        Err(error) => {
            warn_unavailable("LIST", session_id, &error);
            Vec::new()
        }
    }
}

/// Whether the durable layer is usable for this session.
pub async fn is_available(session_id: &str) -> bool {
    let owned = session_id.to_string();
    let outcome = tokio::task::spawn_blocking(move || with_store(&owned, |_| Ok(()))).await;
    matches!(outcome, Ok(Ok(())))
}
