use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use super::record::{
    ControlEntry, ControlPhase, InputEntry, InputPhase, InputReceipt, InputReceiptPhase, JournalRecord, MonitorEntry,
    MonitorStoppedEntry, Payload, RecordKind, RunEntry, RuntimeState, StopEntry, WakeEntry,
};
use super::{JournalError, JournalResult};

/// Where a scan starts.
pub enum ScanStart<'a> {
    /// The start of a file: the first row must be a checkpoint.
    Fresh { session_id: &'a str },
    /// Bytes appended after a state that is already replayed.
    Continuation {
        session_id: &'a str,
        state: &'a RuntimeState,
        generation: u64,
        last_seq: u64,
    },
}

/// The terminal-history caps a replay and a checkpoint enforce.
///
/// These caps only bound terminal history. Work that still has an outcome is
/// never capped: a live input, an input sealed in doubt, a nonterminal control,
/// a live run, a live monitor, and an undelivered wake are retained whatever
/// the terminal count is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionCaps {
    /// Most recent terminal (stopped) runs kept.
    pub terminal_runs: usize,
    /// Most recent terminal input receipts, terminal controls, stopped monitors,
    /// and acknowledged wakes kept.
    pub terminal_records: usize,
}

impl RetentionCaps {
    pub const fn new(terminal_runs: usize, terminal_records: usize) -> Self {
        Self {
            terminal_runs,
            terminal_records,
        }
    }
}

impl Default for RetentionCaps {
    fn default() -> Self {
        Self::new(super::TERMINAL_RUN_CAP, super::TERMINAL_RECEIPT_CAP)
    }
}

/// The state and boundaries a scan produced.
#[derive(Debug)]
pub struct ScanReport {
    pub state: RuntimeState,
    pub generation: u64,
    pub last_seq: u64,
    /// Complete rows read from the scanned bytes.
    pub records: u64,
    /// Bytes consumed by complete rows; a rewrite keeps exactly this prefix.
    pub valid_len: u64,
    /// Trailing bytes with no newline, preserved before any rewrite.
    pub torn_tail: Vec<u8>,
}

impl ScanReport {
    pub fn has_torn_tail(&self) -> bool {
        !self.torn_tail.is_empty()
    }
}

/// A transition the state machine refuses, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionViolation(String);

impl TransitionViolation {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TransitionViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Replay a byte range into typed state.
///
/// A fresh scan requires the checkpoint that starts the file; a continuation
/// requires the next row to be exactly one past the replayed `seq`. Anything
/// else is a blocking error, never a partial state.
pub fn scan(bytes: &[u8], start: ScanStart<'_>, retention: RetentionCaps) -> JournalResult<ScanReport> {
    let (session_id, mut state, mut generation, mut last_seq, fresh) = match start {
        ScanStart::Fresh { session_id } => (session_id, RuntimeState::default(), 0u64, 0u64, true),
        ScanStart::Continuation {
            session_id,
            state,
            generation,
            last_seq,
        } => (session_id, state.clone(), generation, last_seq, false),
    };
    let mut records = 0u64;
    let mut offset = 0usize;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        let Some(newline) = rest.iter().position(|byte| *byte == b'\n') else {
            break;
        };
        let line = &rest[..newline];
        if line.is_empty() {
            return Err(JournalError::corrupt(format!("empty journal row at byte {offset}")));
        }
        let record = JournalRecord::from_line(line)?;
        if record.session_id != session_id {
            return Err(JournalError::corrupt(format!(
                "row {} belongs to session {:?}, not {:?}",
                record.seq, record.session_id, session_id
            )));
        }
        if records == 0 && fresh {
            if record.kind != RecordKind::Checkpoint {
                return Err(JournalError::corrupt(format!(
                    "journal must begin with a checkpoint, found {}",
                    record.kind.as_str()
                )));
            }
            let Payload::Checkpoint(checkpoint) = &record.payload else {
                return Err(JournalError::corrupt("checkpoint row has a mismatched payload"));
            };
            if checkpoint.start_seq != record.seq || checkpoint.last_seq + 1 != record.seq {
                return Err(JournalError::corrupt(format!(
                    "checkpoint at seq {} claims start {} after {}",
                    record.seq, checkpoint.start_seq, checkpoint.last_seq
                )));
            }
            generation = checkpoint.generation;
            state = checkpoint.state.clone();
            last_seq = record.seq;
        } else {
            if record.kind == RecordKind::Checkpoint {
                return Err(JournalError::corrupt(format!(
                    "checkpoint at seq {} is not the first row",
                    record.seq
                )));
            }
            if record.seq != last_seq + 1 {
                return Err(JournalError::sequence_gap(format!(
                    "expected seq {} after {}, found {}",
                    last_seq + 1,
                    last_seq,
                    record.seq
                )));
            }
            apply_record(&mut state, &record, retention)
                .map_err(|violation| JournalError::corrupt(violation.to_string()))?;
            last_seq = record.seq;
        }
        records += 1;
        offset += newline + 1;
    }
    Ok(ScanReport {
        state,
        generation,
        last_seq,
        records,
        valid_len: offset as u64,
        torn_tail: bytes[offset..].to_vec(),
    })
}

/// Apply one row to state.
///
/// The row's `id` is the entity it refers to: the input, run, operation,
/// monitor, or wake identity. Content arrives once with `input.accepted`; every
/// later transition references the id alone.
pub fn apply_record(
    state: &mut RuntimeState,
    record: &JournalRecord,
    retention: RetentionCaps,
) -> Result<(), TransitionViolation> {
    let id = record.id.as_str();
    match &record.payload {
        Payload::Checkpoint(checkpoint) => {
            *state = checkpoint.state.clone();
        }
        Payload::InputAccepted(accepted) => {
            if state.inputs.contains_key(id) {
                return Err(TransitionViolation::new(format!("input {id} is already live")));
            }
            if state.input_receipts.contains_key(id) {
                return Err(TransitionViolation::new(format!("input {id} is already terminal")));
            }
            state.inputs.insert(
                id.to_string(),
                InputEntry {
                    phase: InputPhase::Accepted,
                    seq: record.seq,
                    accepted: accepted.clone(),
                    run_id: None,
                    note: None,
                },
            );
        }
        Payload::InputStarted(started) => {
            let entry = live_input_mut(state, id, "started")?;
            match entry.phase {
                InputPhase::Accepted | InputPhase::Returned => {
                    entry.phase = InputPhase::Started;
                    entry.seq = record.seq;
                    entry.run_id = started.run_id.clone();
                    entry.note = None;
                }
                InputPhase::Started => {
                    return Err(TransitionViolation::new(format!("input {id} started twice")));
                }
                InputPhase::InDoubt => {
                    return Err(TransitionViolation::new(format!(
                        "input {id} is sealed in doubt and needs an explicit decision"
                    )));
                }
            }
        }
        Payload::InputCommitted(committed) => {
            let entry = live_input_mut(state, id, "committed")?;
            if entry.phase != InputPhase::Started {
                return Err(TransitionViolation::new(format!(
                    "input {id} committed before it started"
                )));
            }
            state.inputs.remove(id);
            state.input_receipts.insert(
                id.to_string(),
                InputReceipt {
                    phase: InputReceiptPhase::Committed,
                    seq: record.seq,
                    run_id: committed.run_id.clone(),
                },
            );
        }
        Payload::InputCancelled(_) => {
            let entry = live_input_mut(state, id, "cancelled")?;
            if entry.phase == InputPhase::Started {
                return Err(TransitionViolation::new(format!(
                    "input {id} is running; record it in_doubt or returned first"
                )));
            }
            state.inputs.remove(id);
            state.input_receipts.insert(
                id.to_string(),
                InputReceipt {
                    phase: InputReceiptPhase::Cancelled,
                    seq: record.seq,
                    run_id: None,
                },
            );
        }
        Payload::InputReturned(returned) => {
            let entry = live_input_mut(state, id, "returned")?;
            if entry.phase != InputPhase::Started {
                return Err(TransitionViolation::new(format!(
                    "input {id} can only be returned after it started"
                )));
            }
            entry.phase = InputPhase::Returned;
            entry.seq = record.seq;
            entry.run_id = None;
            entry.note = Some(returned.reason.clone());
        }
        Payload::InputInDoubt(in_doubt) => {
            let entry = live_input_mut(state, id, "in_doubt")?;
            if entry.phase != InputPhase::Started {
                return Err(TransitionViolation::new(format!(
                    "input {id} can only be sealed in doubt while started"
                )));
            }
            entry.phase = InputPhase::InDoubt;
            entry.seq = record.seq;
            entry.note = in_doubt.cause.clone();
        }
        Payload::RunStarted(started) => {
            if state.runs.contains_key(id) {
                return Err(TransitionViolation::new(format!("run {id} started twice")));
            }
            state.runs.insert(
                id.to_string(),
                RunEntry {
                    seq: record.seq,
                    started_at: record.at,
                    input_id: started.input_id.clone(),
                    stop: None,
                },
            );
            if let Some(binding) = started.binding.clone() {
                state.observe_binding(binding);
            }
        }
        Payload::RunStopped(stopped) => {
            let run = state
                .runs
                .get_mut(id)
                .ok_or_else(|| TransitionViolation::new(format!("stop row for unknown run {id}")))?;
            if run.stop.is_some() {
                return Err(TransitionViolation::new(format!("run {id} stopped twice")));
            }
            run.stop = Some(StopEntry {
                cause: stopped.cause,
                at: record.at,
                seq: record.seq,
            });
            if let Some(binding) = stopped.binding.clone() {
                state.observe_binding(binding);
            }
        }
        Payload::ControlApplying(applying) => {
            if state.controls.contains_key(id) {
                return Err(TransitionViolation::new(format!("operation {id} is already recorded")));
            }
            state.controls.insert(
                id.to_string(),
                ControlEntry {
                    seq: record.seq,
                    action: applying.action.clone(),
                    digest: applying.digest.clone(),
                    phase: ControlPhase::Applying,
                    result: None,
                    terminal_seq: None,
                },
            );
        }
        Payload::ControlTerminal(terminal) => {
            let entry = state
                .controls
                .get_mut(id)
                .ok_or_else(|| TransitionViolation::new(format!("terminal row for unknown operation {id}")))?;
            if entry.phase == ControlPhase::Terminal {
                return Err(TransitionViolation::new(format!("operation {id} is already terminal")));
            }
            entry.phase = ControlPhase::Terminal;
            entry.result = Some(terminal.result.clone());
            entry.terminal_seq = Some(record.seq);
        }
        Payload::MonitorUpsert(upsert) => {
            let checkpoint = state.monitors.get(id).and_then(|entry| entry.last_checkpoint.clone());
            state.monitors.insert(
                id.to_string(),
                MonitorEntry {
                    seq: record.seq,
                    monitor_kind: upsert.monitor_kind.clone(),
                    spec: upsert.spec.clone(),
                    last_checkpoint: checkpoint,
                    stopped: None,
                },
            );
        }
        Payload::MonitorCheckpoint(checkpoint) => {
            let entry = state
                .monitors
                .get_mut(id)
                .ok_or_else(|| TransitionViolation::new(format!("checkpoint for unknown monitor {id}")))?;
            if entry.stopped.is_some() {
                return Err(TransitionViolation::new(format!(
                    "monitor {id} is stopped; a checkpoint cannot revive it"
                )));
            }
            entry.seq = record.seq;
            entry.last_checkpoint = Some(checkpoint.clone());
        }
        Payload::MonitorStopped(stopped) => {
            let entry = state
                .monitors
                .get_mut(id)
                .ok_or_else(|| TransitionViolation::new(format!("stop row for unknown monitor {id}")))?;
            if entry.stopped.is_some() {
                return Err(TransitionViolation::new(format!("monitor {id} stopped twice")));
            }
            entry.seq = record.seq;
            entry.stopped = Some(MonitorStoppedEntry {
                reason: stopped.reason.clone(),
                seq: record.seq,
            });
        }
        Payload::WakePending(pending) => {
            if state.wakes.contains_key(id) {
                return Err(TransitionViolation::new(format!("wake {id} is already recorded")));
            }
            state.wakes.insert(
                id.to_string(),
                WakeEntry {
                    seq: record.seq,
                    monitor_id: pending.monitor_id.clone(),
                    monitor_seq: pending.monitor_seq,
                    summary: pending.summary.clone(),
                    acked: false,
                    delivered_input_id: None,
                },
            );
        }
        Payload::WakeAcked(acked) => {
            let entry = state
                .wakes
                .get_mut(id)
                .ok_or_else(|| TransitionViolation::new(format!("ack for unknown wake {id}")))?;
            if entry.acked {
                return Err(TransitionViolation::new(format!("wake {id} acknowledged twice")));
            }
            entry.acked = true;
            entry.seq = record.seq;
            entry.delivered_input_id = acked.delivered_input_id.clone();
        }
    }
    prune(state, retention, false);
    Ok(())
}

fn live_input_mut<'a>(
    state: &'a mut RuntimeState,
    id: &str,
    row: &str,
) -> Result<&'a mut InputEntry, TransitionViolation> {
    state
        .inputs
        .get_mut(id)
        .ok_or_else(|| TransitionViolation::new(format!("{row} row for unknown input {id}")))
}

/// Bound the terminal history a replay and a checkpoint keep.
///
/// Terminal history is the only part of state that grows with session length: a
/// long session would otherwise carry every completed run and every answered
/// control into each checkpoint until the 32 MiB ceiling refuses compaction.
/// Pruning keeps the most recent `caps.terminal_runs` stopped runs and the most
/// recent `caps.terminal_records` terminal input receipts, terminal controls,
/// stopped monitors, and acknowledged wakes.
///
/// Work that is not finished is never pruned: live inputs (accepted, started,
/// returned), inputs sealed in doubt, nonterminal controls, live runs, monitors,
/// and undelivered wakes all survive with no cap.
/// A pruned id is simply unknown to a later lookup; a pruned input id may be
/// accepted again, which is why the receipt caps stay at the contract value.
///
/// `force` re-applies the caps even when the cheap counters say state is inside
/// them, which is what a checkpoint write does so that every checkpoint this
/// build writes satisfies the documented bounds.
pub fn prune(state: &mut RuntimeState, caps: RetentionCaps, force: bool) {
    let terminal_controls = state
        .controls
        .values()
        .filter(|entry| entry.phase == ControlPhase::Terminal)
        .count();
    let stopped_runs = state.runs.values().filter(|run| run.stop.is_some()).count();
    let stopped_monitors = state
        .monitors
        .values()
        .filter(|monitor| monitor.stopped.is_some())
        .count();
    let acked_wakes = state.wakes.values().filter(|wake| wake.acked).count();
    let records = caps.terminal_records;
    let runs = caps.terminal_runs;
    if force || state.input_receipts.len() > records {
        prune_by_seq(&mut state.input_receipts, records, |receipt| receipt.seq, |_| false);
    }
    if force || terminal_controls > records {
        prune_by_seq(
            &mut state.controls,
            records,
            |entry| entry.terminal_seq.unwrap_or(0),
            |entry| entry.terminal_seq.is_none(),
        );
    }
    if force || stopped_runs > runs {
        prune_by_seq(
            &mut state.runs,
            runs,
            |entry| entry.stop.as_ref().map(|stop| stop.seq).unwrap_or(0),
            |entry| entry.stop.is_none(),
        );
    }
    if force || stopped_monitors > records {
        prune_by_seq(
            &mut state.monitors,
            records,
            |entry| entry.stopped.as_ref().map(|stop| stop.seq).unwrap_or(0),
            |entry| entry.stopped.is_none(),
        );
    }
    if force || acked_wakes > records {
        prune_by_seq(&mut state.wakes, records, |wake| wake.seq, |wake| !wake.acked);
    }
}

fn prune_by_seq<T, Key, KeyFn, KeepFn>(map: &mut BTreeMap<String, T>, cap: usize, key: KeyFn, keep: KeepFn)
where
    Key: Ord,
    KeyFn: Fn(&T) -> Key,
    KeepFn: Fn(&T) -> bool,
{
    let mut prunable: Vec<(Key, String)> = map
        .iter()
        .filter(|(_, value)| !keep(value))
        .map(|(id, value)| (key(value), id.clone()))
        .collect();
    if prunable.len() <= cap {
        return;
    }
    prunable.sort();
    let remove = prunable.len() - cap;
    for (_, id) in prunable.into_iter().take(remove) {
        map.remove(&id);
    }
}

/// Read every complete row of a journal file, for diagnostics and tests.
pub fn read_journal_records(path: &Path) -> JournalResult<Vec<JournalRecord>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(JournalError::io_context("read the journal", path, error)),
    };
    let mut records = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        let Some(newline) = rest.iter().position(|byte| *byte == b'\n') else {
            break;
        };
        records.push(JournalRecord::from_line(&rest[..newline])?);
        offset += newline + 1;
    }
    Ok(records)
}
