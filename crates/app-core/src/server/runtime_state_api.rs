//! The daemon side of the stable harness API's durable runtime surfaces.
//!
//! The bridge that serves the API runs as its own process, so it cannot read
//! this process's in-memory state. What both processes do share is the durable
//! runtime journal on disk, which plan task 7 owns. These helpers answer the
//! three requests the API gates on `durable_runtime_v1` from that one owner:
//!
//! * `get_runtime_state` reads the journal and maps it into the API's
//!   `RuntimeStateSummary`.
//! * `apply_control` runs the operation under its durable receipt through
//!   [`control_receipts`](super::control_receipts), which is the only path that
//!   may apply it.
//! * `get_control_receipt` reads the receipt an earlier operation recorded.
//!
//! Nothing here invents a result. A journal that cannot be read answers with
//! the journal's own error, and a control operation the store has never seen
//! answers `unknown` rather than a fabricated success.

use super::control_receipts::{ControlAction, ControlOutcome};
use super::state::SessionControlHandle;
use crate::durable_runtime::{DurableRuntimeStore, RuntimeState};
use horon_agent_harness_api::{
    RuntimeJournalSummary, RuntimeMonitorSummary, RuntimeQueueSummary, RuntimeReceiptSummary, RuntimeRunSummary,
    RuntimeStateSummary, RuntimeStopSummary,
};
use serde_json::Value;

/// One snapshot answer for the stable API, built from this session's
/// observation owner.
///
/// The marker and the view are captured inside one critical section by
/// [`SessionObservation::take_snapshot`], so the `seq` a client receives is
/// exactly the boundary of the messages it receives. The view is mapped into
/// the API's own `SessionSnapshot` here because the API type is the contract the
/// client reads, and the observation module deliberately carries its own.
pub(super) async fn snapshot(
    session_id: &str,
    cursor: Option<String>,
    tail: Option<u32>,
    max_bytes: Option<u64>,
) -> Result<horon_agent_harness_api::SessionSnapshot, String> {
    let observation = super::session_observation::observation_for(session_id);
    // The marker FIFO is the connection's, and this reply reaches the client as
    // one frame that carries both halves, so a local channel is the right place
    // for the record: the pairing is preserved by the reply itself rather than
    // by two frames staying in order.
    let (marker_tx, _marker_rx) = tokio::sync::mpsc::unbounded_channel();
    let request = super::session_observation::SnapshotRequest {
        cursor,
        tail,
        max_bytes,
    };
    let view = observation
        .take_snapshot(&request, &marker_tx)
        .map_err(|_| "the snapshot marker could not be recorded".to_string())?;
    let state = runtime_state_summary(session_id).await?;
    Ok(horon_agent_harness_api::SessionSnapshot {
        generation: view.generation,
        seq: view.seq,
        marker_id: view.marker_id,
        state,
        messages: view
            .messages
            .into_iter()
            .map(|message| horon_agent_harness_api::HistoryMessage {
                response_stats: None,
                role: message.role,
                content: message.content,
            })
            .collect(),
        next_history_cursor: view.next_history_cursor,
        truncated: view.truncated,
    })
}

/// Read one session's journal and summarise it for the API.
///
/// Runs on a blocking thread because the store does synchronous file I/O, and
/// holds no lock across an await. A session with no journal answers an empty
/// summary with `exists: false`, which is a fact about that session rather than
/// an error.
pub(crate) async fn runtime_state_summary(session_id: &str) -> Result<RuntimeStateSummary, String> {
    let session_id = session_id.to_string();
    tokio::task::spawn_blocking(move || summarise(&session_id))
        .await
        .map_err(|error| format!("the runtime state task did not finish: {error}"))?
}

fn summarise(session_id: &str) -> Result<RuntimeStateSummary, String> {
    let mut store = match DurableRuntimeStore::open(session_id) {
        Ok(store) => store,
        // A journal that has never been written is not an error: the session
        // simply has no recorded runtime state yet.
        Err(error) if error.kind() == crate::durable_runtime::JournalErrorKind::Io => {
            return Ok(RuntimeStateSummary::default());
        }
        Err(error) => return Err(format!("the runtime journal could not be opened: {error}")),
    };
    let generation = store.generation();
    let last_seq = store.last_seq();
    let bytes = store.journal_bytes();
    let records = store.record_count();
    let read_only = store.is_read_only();
    let state = store
        .snapshot()
        .map_err(|error| format!("the runtime journal could not be replayed: {error}"))?;
    Ok(summarise_state(state, generation, last_seq, bytes, records, read_only))
}

fn summarise_state(
    state: RuntimeState,
    generation: u64,
    last_seq: u64,
    bytes: u64,
    records: u64,
    read_only: bool,
) -> RuntimeStateSummary {
    // Live runs, oldest first, so a status panel can render them in the order
    // they started rather than the order the map happens to hold them.
    let mut runs: Vec<RuntimeRunSummary> = state
        .live_runs()
        .map(|(run_id, run)| RuntimeRunSummary {
            run_id: run_id.clone(),
            started_at: run.started_at.to_rfc3339(),
            input_id: run.input_id.clone(),
        })
        .collect();
    runs.sort_by(|left, right| left.started_at.cmp(&right.started_at));

    // Terminal input receipts, most recent first: the newest is the one a
    // client is most likely asking about.
    let mut receipts: Vec<RuntimeReceiptSummary> = state
        .input_receipts
        .iter()
        .map(|(input_id, receipt)| RuntimeReceiptSummary {
            input_id: input_id.clone(),
            phase: match receipt.phase {
                crate::durable_runtime::InputReceiptPhase::Committed => "committed".to_string(),
                crate::durable_runtime::InputReceiptPhase::Cancelled => "cancelled".to_string(),
            },
            seq: receipt.seq,
            run_id: receipt.run_id.clone(),
        })
        .collect();
    receipts.sort_by_key(|receipt| std::cmp::Reverse(receipt.seq));

    let monitors: Vec<RuntimeMonitorSummary> = state
        .monitors
        .iter()
        .map(|(monitor_id, monitor)| RuntimeMonitorSummary {
            monitor_id: monitor_id.clone(),
            kind: monitor.monitor_kind.clone(),
            stopped: monitor.stopped.is_some(),
            // The checkpoint's own one-line summary, never its spec: a monitor
            // spec can carry a path or a command line.
            last_summary: monitor
                .last_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.summary.clone()),
        })
        .collect();

    let pending_wakes = u32::try_from(state.pending_wakes().count()).unwrap_or(u32::MAX);
    let pending_controls = u32::try_from(
        state
            .controls
            .values()
            .filter(|control| control.phase == crate::durable_runtime::ControlPhase::Applying)
            .count(),
    )
    .unwrap_or(u32::MAX);

    let last_stop = state.last_stop().map(|(run_id, stop)| RuntimeStopSummary {
        run_id: run_id.clone(),
        cause: stop_cause_label(stop.cause).to_string(),
        at: stop.at.to_rfc3339(),
    });

    RuntimeStateSummary {
        journal: RuntimeJournalSummary {
            exists: generation > 0 || last_seq > 0,
            generation,
            last_seq,
            bytes,
            records,
            read_only,
        },
        runs,
        queue: RuntimeQueueSummary {
            live_inputs: u32::try_from(state.live_inputs().count()).unwrap_or(u32::MAX),
            undetermined_inputs: u32::try_from(state.undetermined_inputs().count()).unwrap_or(u32::MAX),
            live_input_bytes: state.live_input_bytes(),
        },
        receipts,
        monitors,
        pending_wakes,
        pending_controls,
        last_stop,
    }
}

/// The wire word for a stop cause, matching the API's documented set.
fn stop_cause_label(cause: crate::durable_runtime::StopCause) -> &'static str {
    use crate::durable_runtime::StopCause;
    match cause {
        StopCause::Completed => "completed",
        StopCause::UserCancelled => "user_cancelled",
        StopCause::Shutdown => "shutdown",
        StopCause::ProviderError => "provider_error",
        StopCause::Timeout => "timeout",
        StopCause::LoopGuard => "loop_guard",
        StopCause::Limit => "limit",
        StopCause::InterruptedUnknown => "interrupted_unknown",
    }
}

/// Parse one control request's action and arguments.
///
/// The two actions this daemon can apply are the ones the plain cancel path
/// already exposes, so an unrecognized action is refused rather than guessed
/// at: a control request a client may resend must be one whose duplicate is
/// harmless.
pub(super) fn parse_control_action(action: &str, args: Option<&Value>) -> Result<ControlAction, String> {
    match action {
        "abort_run" => {
            let expected_run_id = args
                .and_then(|args| args.get("expected_run_id"))
                .and_then(Value::as_str)
                .filter(|run_id| !run_id.is_empty())
                .ok_or_else(|| "abort_run requires a non-empty `expected_run_id`".to_string())?;
            Ok(ControlAction::abort_run(expected_run_id))
        }
        "cancel_inputs" => {
            let ids = args
                .and_then(|args| args.get("input_ids"))
                .and_then(Value::as_array)
                .ok_or_else(|| "cancel_inputs requires an `input_ids` array".to_string())?;
            let mut input_ids = Vec::with_capacity(ids.len());
            for id in ids {
                let id = id
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| "every `input_ids` entry must be a non-empty string".to_string())?;
                input_ids.push(id.to_string());
            }
            Ok(ControlAction::cancel_inputs(&input_ids))
        }
        other => Err(format!(
            "unknown control action `{other}`; this daemon applies `abort_run` and `cancel_inputs`"
        )),
    }
}

/// Apply one control operation under its durable receipt.
pub(super) async fn apply_control(
    handle: &SessionControlHandle,
    operation_id: &str,
    action: ControlAction,
) -> ControlOutcome {
    handle.apply_control(operation_id, &action).await
}

/// The receipt an earlier operation recorded, if the journal still holds it.
///
/// A receipt is read, never created: asking for an operation this session never
/// applied answers `unknown`, which is exactly what the client must hear so it
/// does not conclude the operation ran.
pub(super) async fn control_receipt(session_id: &str, operation_id: &str) -> Result<(String, ControlOutcome), String> {
    let session_id = session_id.to_string();
    let operation_id = operation_id.to_string();
    tokio::task::spawn_blocking(move || read_receipt(&session_id, &operation_id))
        .await
        .map_err(|error| format!("the control receipt task did not finish: {error}"))?
}

fn read_receipt(session_id: &str, operation_id: &str) -> Result<(String, ControlOutcome), String> {
    let mut store = DurableRuntimeStore::open(session_id)
        .map_err(|error| format!("the control journal could not be opened: {error}"))?;
    let state = store
        .snapshot()
        .map_err(|error| format!("the control journal could not be replayed: {error}"))?;
    let record_id = control_record_id(session_id, operation_id);
    match state.controls.get(&record_id) {
        Some(entry) if entry.phase == crate::durable_runtime::ControlPhase::Terminal => Ok((
            entry.action.clone(),
            ControlOutcome::Replayed {
                result: entry.result.clone().unwrap_or(Value::Null),
            },
        )),
        Some(entry) => Ok((
            entry.action.clone(),
            ControlOutcome::Pending {
                reason: format!("operation `{operation_id}` is recorded but has no terminal receipt yet"),
            },
        )),
        None => Ok((
            String::new(),
            ControlOutcome::Unknown {
                reason: format!("operation `{operation_id}` was never applied to session `{session_id}`"),
            },
        )),
    }
}

/// The journal record id one operation always maps to.
///
/// This must stay byte-identical to the derivation in
/// [`control_receipts`](super::control_receipts), because both processes read
/// the same rows: a receipt written by the daemon has to be findable by the
/// bridge's own read path.
pub(super) fn control_record_id(session_id: &str, operation_id: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("horon-control/{session_id}/{operation_id}").as_bytes(),
    )
    .to_string()
}

/// A control outcome as the API's ready-to-display one-line status.
pub(super) fn control_message(outcome: &ControlOutcome) -> String {
    match outcome {
        ControlOutcome::Applied { .. } => "the operation was applied".to_string(),
        ControlOutcome::Replayed { .. } => {
            "the operation had already been applied; its recorded result is replayed".to_string()
        }
        ControlOutcome::Pending { reason } => format!("the operation is still applying: {reason}"),
        ControlOutcome::Conflict { reason } => format!("conflict: {reason}"),
        ControlOutcome::Indeterminate { reason } => format!("indeterminate: {reason}"),
        ControlOutcome::StaleRun {
            expected_run_id,
            current_run_id,
        } => match current_run_id {
            Some(current) => format!(
                "stale_run: the session is running `{current}`, not `{expected_run_id}`, so nothing was cancelled"
            ),
            None => format!("stale_run: `{expected_run_id}` is no longer the current run, so nothing was cancelled"),
        },
        ControlOutcome::Failed { reason } => format!("failed: {reason}"),
        ControlOutcome::Unknown { reason } => format!("unknown: {reason}"),
        ControlOutcome::Invalid { reason } => format!("invalid: {reason}"),
        ControlOutcome::Busy { reason } => format!("busy: {reason}"),
        ControlOutcome::JournalFailed { reason } => format!("journal_failed: {reason}"),
    }
}

/// The API's outcome word for one control result.
pub(super) fn control_outcome_label(outcome: &ControlOutcome) -> &'static str {
    match outcome {
        ControlOutcome::Applied { .. } => "terminal",
        ControlOutcome::Replayed { .. } => "replay",
        ControlOutcome::Pending { .. } => "applying",
        ControlOutcome::Conflict { .. } => "conflict",
        ControlOutcome::Indeterminate { .. } => "indeterminate",
        ControlOutcome::StaleRun { .. } | ControlOutcome::Failed { .. } => "terminal",
        ControlOutcome::Unknown { .. }
        | ControlOutcome::Invalid { .. }
        | ControlOutcome::Busy { .. }
        | ControlOutcome::JournalFailed { .. } => "unknown",
    }
}

/// One control result as the daemon's `control_receipt` reply.
pub(super) fn control_receipt_event(
    id: u64,
    operation_id: String,
    action: String,
    outcome: &ControlOutcome,
) -> crate::protocol::ServerEvent {
    crate::protocol::ServerEvent::ControlReceipt {
        id,
        operation_id,
        action,
        outcome: control_outcome_label(outcome).to_string(),
        result: outcome.result().cloned(),
        message: control_message(outcome),
    }
}
