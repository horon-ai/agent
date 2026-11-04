use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{JournalError, JournalResult};

/// The record schema version this build writes and reads.
pub const JOURNAL_VERSION: u32 = 1;

/// The row kinds the journal understands.
///
/// The set is closed: a row with any other kind is corrupt, because a build
/// that cannot interpret a transition must not skip it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecordKind {
    Checkpoint,
    InputAccepted,
    InputStarted,
    InputCommitted,
    InputCancelled,
    InputReturned,
    InputInDoubt,
    RunStarted,
    RunStopped,
    ControlApplying,
    ControlTerminal,
    MonitorUpsert,
    MonitorCheckpoint,
    MonitorStopped,
    WakePending,
    WakeAcked,
}

impl RecordKind {
    pub const ALL: [RecordKind; 16] = [
        RecordKind::Checkpoint,
        RecordKind::InputAccepted,
        RecordKind::InputStarted,
        RecordKind::InputCommitted,
        RecordKind::InputCancelled,
        RecordKind::InputReturned,
        RecordKind::InputInDoubt,
        RecordKind::RunStarted,
        RecordKind::RunStopped,
        RecordKind::ControlApplying,
        RecordKind::ControlTerminal,
        RecordKind::MonitorUpsert,
        RecordKind::MonitorCheckpoint,
        RecordKind::MonitorStopped,
        RecordKind::WakePending,
        RecordKind::WakeAcked,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::InputAccepted => "input.accepted",
            Self::InputStarted => "input.started",
            Self::InputCommitted => "input.committed",
            Self::InputCancelled => "input.cancelled",
            Self::InputReturned => "input.returned",
            Self::InputInDoubt => "input.in_doubt",
            Self::RunStarted => "run.started",
            Self::RunStopped => "run.stopped",
            Self::ControlApplying => "control.applying",
            Self::ControlTerminal => "control.terminal",
            Self::MonitorUpsert => "monitor.upsert",
            Self::MonitorCheckpoint => "monitor.checkpoint",
            Self::MonitorStopped => "monitor.stopped",
            Self::WakePending => "wake.pending",
            Self::WakeAcked => "wake.acked",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == raw)
    }

    /// Whether the row brings new work into the session.
    ///
    /// Only a genuinely new inbound item is an admission: accepted input
    /// content, and a monitor create or update. Those two are the only kinds
    /// refused with [`super::JournalErrorKind::Busy`] when the live-input or
    /// journal budget is full.
    ///
    /// Every other kind is a lifecycle row over work that is already admitted
    /// and can never be refused for capacity: a cancellation, abort, commit,
    /// return, or in-doubt seal must be recordable while the queue is full, and
    /// so must run, monitor, wake, and checkpoint rows. Lifecycle rows may be
    /// written inside the [`super::LIFECYCLE_RESERVE_BYTES`] tail that
    /// admissions never enter.
    pub const fn is_admission(self) -> bool {
        matches!(self, Self::InputAccepted | Self::MonitorUpsert)
    }
}

/// The content of one row, typed by its kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Payload {
    Checkpoint(Checkpoint),
    InputAccepted(InputAccepted),
    InputStarted(InputStarted),
    InputCommitted(InputCommitted),
    InputCancelled(InputCancelled),
    InputReturned(InputReturned),
    InputInDoubt(InputInDoubt),
    RunStarted(RunStarted),
    RunStopped(RunStopped),
    ControlApplying(ControlApplying),
    ControlTerminal(ControlTerminal),
    MonitorUpsert(MonitorUpsert),
    MonitorCheckpoint(MonitorCheckpoint),
    MonitorStopped(MonitorStopped),
    WakePending(WakePending),
    WakeAcked(WakeAcked),
}

impl Payload {
    pub const fn kind(&self) -> RecordKind {
        match self {
            Self::Checkpoint(_) => RecordKind::Checkpoint,
            Self::InputAccepted(_) => RecordKind::InputAccepted,
            Self::InputStarted(_) => RecordKind::InputStarted,
            Self::InputCommitted(_) => RecordKind::InputCommitted,
            Self::InputCancelled(_) => RecordKind::InputCancelled,
            Self::InputReturned(_) => RecordKind::InputReturned,
            Self::InputInDoubt(_) => RecordKind::InputInDoubt,
            Self::RunStarted(_) => RecordKind::RunStarted,
            Self::RunStopped(_) => RecordKind::RunStopped,
            Self::ControlApplying(_) => RecordKind::ControlApplying,
            Self::ControlTerminal(_) => RecordKind::ControlTerminal,
            Self::MonitorUpsert(_) => RecordKind::MonitorUpsert,
            Self::MonitorCheckpoint(_) => RecordKind::MonitorCheckpoint,
            Self::MonitorStopped(_) => RecordKind::MonitorStopped,
            Self::WakePending(_) => RecordKind::WakePending,
            Self::WakeAcked(_) => RecordKind::WakeAcked,
        }
    }

    /// The payload as the row stores it: a plain JSON object, never a tag.
    pub fn to_value(&self) -> JournalResult<Value> {
        let value = match self {
            Self::Checkpoint(payload) => serde_json::to_value(payload),
            Self::InputAccepted(payload) => serde_json::to_value(payload),
            Self::InputStarted(payload) => serde_json::to_value(payload),
            Self::InputCommitted(payload) => serde_json::to_value(payload),
            Self::InputCancelled(payload) => serde_json::to_value(payload),
            Self::InputReturned(payload) => serde_json::to_value(payload),
            Self::InputInDoubt(payload) => serde_json::to_value(payload),
            Self::RunStarted(payload) => serde_json::to_value(payload),
            Self::RunStopped(payload) => serde_json::to_value(payload),
            Self::ControlApplying(payload) => serde_json::to_value(payload),
            Self::ControlTerminal(payload) => serde_json::to_value(payload),
            Self::MonitorUpsert(payload) => serde_json::to_value(payload),
            Self::MonitorCheckpoint(payload) => serde_json::to_value(payload),
            Self::MonitorStopped(payload) => serde_json::to_value(payload),
            Self::WakePending(payload) => serde_json::to_value(payload),
            Self::WakeAcked(payload) => serde_json::to_value(payload),
        };
        value.map_err(|error| JournalError::invalid(format!("payload is not serializable: {error}")))
    }

    /// Decode a stored payload into the shape its kind declares.
    pub fn from_value(kind: RecordKind, value: Value) -> JournalResult<Self> {
        fn decode<T: for<'de> Deserialize<'de>>(kind: RecordKind, value: Value) -> JournalResult<T> {
            serde_json::from_value(value).map_err(|error| {
                JournalError::corrupt(format!("{} payload does not match its kind: {error}", kind.as_str()))
            })
        }
        Ok(match kind {
            RecordKind::Checkpoint => Self::Checkpoint(decode(kind, value)?),
            RecordKind::InputAccepted => Self::InputAccepted(decode(kind, value)?),
            RecordKind::InputStarted => Self::InputStarted(decode(kind, value)?),
            RecordKind::InputCommitted => Self::InputCommitted(decode(kind, value)?),
            RecordKind::InputCancelled => Self::InputCancelled(decode(kind, value)?),
            RecordKind::InputReturned => Self::InputReturned(decode(kind, value)?),
            RecordKind::InputInDoubt => Self::InputInDoubt(decode(kind, value)?),
            RecordKind::RunStarted => Self::RunStarted(decode(kind, value)?),
            RecordKind::RunStopped => Self::RunStopped(decode(kind, value)?),
            RecordKind::ControlApplying => Self::ControlApplying(decode(kind, value)?),
            RecordKind::ControlTerminal => Self::ControlTerminal(decode(kind, value)?),
            RecordKind::MonitorUpsert => Self::MonitorUpsert(decode(kind, value)?),
            RecordKind::MonitorCheckpoint => Self::MonitorCheckpoint(decode(kind, value)?),
            RecordKind::MonitorStopped => Self::MonitorStopped(decode(kind, value)?),
            RecordKind::WakePending => Self::WakePending(decode(kind, value)?),
            RecordKind::WakeAcked => Self::WakeAcked(decode(kind, value)?),
        })
    }

    /// Bytes this payload retains in live state, counted against the 16 MiB
    /// live-input budget. Only accepted input content is retained; transitions
    /// reference the input id alone.
    pub fn retained_bytes(&self) -> u64 {
        match self {
            Self::InputAccepted(payload) => serde_json::to_vec(payload).map(|bytes| bytes.len() as u64).unwrap_or(0),
            _ => 0,
        }
    }
}

/// One journal row.
#[derive(Clone, Debug, PartialEq)]
pub struct JournalRecord {
    pub v: u32,
    pub seq: u64,
    pub session_id: String,
    pub kind: RecordKind,
    pub id: String,
    pub at: DateTime<Utc>,
    pub payload: Payload,
}

#[derive(Serialize)]
struct RowRef<'a> {
    v: u32,
    seq: u64,
    session_id: &'a str,
    kind: &'static str,
    id: &'a str,
    at: DateTime<Utc>,
    payload: Value,
}

#[derive(Deserialize)]
struct RawRow {
    v: u32,
    seq: u64,
    session_id: String,
    kind: String,
    id: String,
    at: DateTime<Utc>,
    payload: Value,
}

impl JournalRecord {
    /// The one-line JSON form, newline included.
    pub fn to_line(&self) -> JournalResult<String> {
        let row = RowRef {
            v: self.v,
            seq: self.seq,
            session_id: &self.session_id,
            kind: self.kind.as_str(),
            id: &self.id,
            at: self.at,
            payload: self.payload.to_value()?,
        };
        let mut line = serde_json::to_string(&row)
            .map_err(|error| JournalError::invalid(format!("record is not serializable: {error}")))?;
        line.push('\n');
        Ok(line)
    }

    /// Parse one stored line, refusing an unknown version or kind.
    pub fn from_line(line: &[u8]) -> JournalResult<Self> {
        let row: RawRow = serde_json::from_slice(line)
            .map_err(|error| JournalError::corrupt(format!("row is not valid JSON: {error}")))?;
        if row.v != JOURNAL_VERSION {
            return Err(JournalError::unsupported_version(format!(
                "row version {} is not supported; this build reads and writes version {JOURNAL_VERSION}",
                row.v
            )));
        }
        let kind = RecordKind::parse(&row.kind).ok_or_else(|| {
            JournalError::corrupt(format!(
                "unknown record kind {:?}; skipping a transition is not allowed",
                row.kind
            ))
        })?;
        let payload = Payload::from_value(kind, row.payload)?;
        Ok(Self {
            v: row.v,
            seq: row.seq,
            session_id: row.session_id,
            kind,
            id: row.id,
            at: row.at,
            payload,
        })
    }
}

/// A checkpoint folds the replayed state into one row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub generation: u64,
    pub start_seq: u64,
    pub last_seq: u64,
    pub state: RuntimeState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAccepted {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<(String, String)>,
    #[serde(default)]
    pub urgent: bool,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputStarted {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputCommitted {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputCancelled {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputReturned {
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputInDoubt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStarted {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStopped {
    pub cause: StopCause,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_id: Option<String>,
}

/// Why a run stopped, recorded as observed and never inferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    Completed,
    UserCancelled,
    Shutdown,
    ProviderError,
    Timeout,
    LoopGuard,
    Limit,
    InterruptedUnknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlApplying {
    pub action: String,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlTerminal {
    pub result: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorUpsert {
    pub monitor_kind: String,
    pub spec: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorCheckpoint {
    pub monitor_seq: u64,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorStopped {
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakePending {
    pub monitor_id: String,
    pub monitor_seq: u64,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeAcked {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_input_id: Option<String>,
}

/// Where an input stands in the journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputPhase {
    Accepted,
    Started,
    Returned,
    InDoubt,
}

/// Why an input left live state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputReceiptPhase {
    Committed,
    Cancelled,
}

/// A live input, with its full content kept exactly once at acceptance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputEntry {
    pub phase: InputPhase,
    pub seq: u64,
    pub accepted: InputAccepted,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The bounded receipt left behind by a terminal input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputReceipt {
    pub phase: InputReceiptPhase,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopEntry {
    pub cause: StopCause,
    pub at: DateTime<Utc>,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunEntry {
    pub seq: u64,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<StopEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlPhase {
    Applying,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlEntry {
    pub seq: u64,
    pub action: String,
    pub digest: String,
    pub phase: ControlPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorStoppedEntry {
    pub reason: String,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorEntry {
    pub seq: u64,
    pub monitor_kind: String,
    pub spec: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checkpoint: Option<MonitorCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<MonitorStoppedEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeEntry {
    pub seq: u64,
    pub monitor_id: String,
    pub monitor_seq: u64,
    pub summary: String,
    pub acked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_input_id: Option<String>,
}

/// The typed state a replay produces and a checkpoint carries.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RuntimeState {
    #[serde(default)]
    pub inputs: BTreeMap<String, InputEntry>,
    #[serde(default)]
    pub input_receipts: BTreeMap<String, InputReceipt>,
    #[serde(default)]
    pub runs: BTreeMap<String, RunEntry>,
    #[serde(default)]
    pub controls: BTreeMap<String, ControlEntry>,
    #[serde(default)]
    pub monitors: BTreeMap<String, MonitorEntry>,
    #[serde(default)]
    pub wakes: BTreeMap<String, WakeEntry>,
}

impl RuntimeState {
    /// Inputs that still hold work: accepted, returned, started, or sealed in
    /// doubt.
    pub fn live_inputs(&self) -> impl Iterator<Item = (&String, &InputEntry)> {
        self.inputs.iter()
    }

    /// Inputs whose outcome is not settled: everything still started, plus
    /// everything explicitly sealed in doubt. A started input with no terminal
    /// row is undetermined, not complete.
    pub fn undetermined_inputs(&self) -> impl Iterator<Item = (&String, &InputEntry)> {
        self.inputs
            .iter()
            .filter(|(_, entry)| matches!(entry.phase, InputPhase::Started | InputPhase::InDoubt))
    }

    /// Wakes produced but not yet acknowledged by a delivered input.
    pub fn pending_wakes(&self) -> impl Iterator<Item = (&String, &WakeEntry)> {
        self.wakes.iter().filter(|(_, wake)| !wake.acked)
    }

    /// Runs still marked started.
    pub fn live_runs(&self) -> impl Iterator<Item = (&String, &RunEntry)> {
        self.runs.iter().filter(|(_, run)| run.stop.is_none())
    }

    /// The most recent stop across all runs, by recorded sequence.
    pub fn last_stop(&self) -> Option<(&String, &StopEntry)> {
        self.runs
            .iter()
            .filter_map(|(id, run)| run.stop.as_ref().map(|stop| (stop.seq, id, stop)))
            .max_by_key(|(seq, _, _)| *seq)
            .map(|(_, id, stop)| (id, stop))
    }

    /// Bytes of accepted input content still retained in live state.
    pub fn live_input_bytes(&self) -> u64 {
        self.inputs
            .values()
            .map(|entry| {
                serde_json::to_vec(&entry.accepted)
                    .map(|bytes| bytes.len() as u64)
                    .unwrap_or(0)
            })
            .sum()
    }
}
