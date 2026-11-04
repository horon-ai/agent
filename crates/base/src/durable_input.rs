//! Durable queue transitions for pending soft-interrupt inputs.
//!
//! The durable runtime journal ([`crate::durable_runtime`]) owns the state; this
//! module is the only place that decides what a queue operation means for it.
//! Every live and offline enqueue, cancel, clear, and restore goes through one
//! store transaction here, in the order the plan fixes:
//!
//! * `input.accepted` is appended and synced **before** the message reaches the
//!   in-memory queue, so a caller that is told "queued" holds a durable row.
//!   A failed journal write returns an error and nothing is queued.
//! * `input.started` is appended and synced **before** the message is injected
//!   into the transcript.
//! * `input.committed` is appended after the turn's conversation save
//!   succeeded, and only then is the in-flight set cleared.
//! * `input.returned` records a failed or cancelled turn handing the message
//!   back to pending; the message stays live and is injected again by the next
//!   turn.
//!
//! # Recovery
//!
//! An `accepted` (or `returned`) input restores as pending, deduplicated by its
//! `input_id`, so restoring twice cannot queue the same input twice. An input
//! that is `started` with no terminal row is sealed `in_doubt` and is **not**
//! re-injected: the process cannot know whether its turn reached the provider,
//! and guessing would either duplicate a turn or drop a message. Only an
//! explicit [`DurableInputStore::resolve`] with `retry` or `cancel` settles it.
//!
//! This module never claims exactly-once execution of external tool effects. A
//! retried input runs again; what it guarantees is that the *queue* does not
//! silently drop, duplicate, or resurrect an input.
//!
//! # Legacy compatibility
//!
//! `pending-soft-interrupts/<session>.json` stays readable and writable:
//!
//! * Before the first acceptance of a session, the JSON is imported once as a
//!   baseline with a deterministic `input_id` per message, and a marker file
//!   records that the import happened so a later start never imports it again.
//! * After every normal transition the live set is mirrored back to that JSON,
//!   fsynced, so an older build reading the same home sees the same queue.
//! * A mirror failure never rolls back a journal accept. It is reported as a
//!   warning, marks the mirror dirty, and makes
//!   [`DurableInputStore::export_legacy`] refuse until a later mirror succeeds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use horon_agent_agent_runtime::{SoftInterruptMessage, SoftInterruptSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable_runtime::{
    DurableRuntimeStore, InputAccepted, InputCancelled, InputCommitted, InputInDoubt, InputPhase, InputReturned,
    InputStarted, JournalError, JournalErrorKind, Payload, RuntimeState,
};

/// Marker file recording that the legacy JSON baseline was imported.
pub const LEGACY_BASELINE_FILE: &str = "legacy-baseline.json";
/// Mirror bookkeeping: whether the legacy JSON is known to be out of date.
pub const MIRROR_STATE_FILE: &str = "mirror-state.json";
/// Which transcript message an injected input produced, for an explicit retry.
pub const INJECTED_COPIES_FILE: &str = "injected-copies.json";

/// How many injected-copy records are kept. A retry is an operator action on a
/// recent input, so a small bound is enough and keeps the file tiny.
pub const INJECTED_COPY_CAP: usize = 64;

/// What one acceptance produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedInput {
    /// The stable journal id the input now carries.
    pub input_id: String,
    /// Whether the id was already live, so nothing new was written.
    pub already_live: bool,
    /// Whether this acceptance was the legacy baseline import.
    pub from_legacy_baseline: bool,
}

/// What a recovery found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputRecovery {
    /// Inputs that restore as pending, in journal order.
    pub pending: Vec<SoftInterruptMessage>,
    /// Inputs sealed in doubt during this recovery, so they are not injected.
    pub sealed_in_doubt: Vec<String>,
    /// Inputs already in doubt from an earlier run.
    pub already_in_doubt: Vec<String>,
    /// How many legacy JSON messages the baseline import accepted.
    pub imported_from_legacy: usize,
}

/// What an explicit decision did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedInput {
    /// The id the decision applied to.
    pub input_id: String,
    /// The phase the input held before the decision.
    pub previous_phase: InputPhase,
    /// The live id that now holds the message, when the action requeued it.
    pub requeued_as: Option<String>,
    /// Whether a previously injected transcript copy was located and removed.
    pub cleaned_injected_copy: bool,
    /// Anything the operator should know, such as a copy that could not be found.
    pub warning: Option<String>,
}

/// An explicit decision about an undetermined input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveAction {
    /// Hand the message back to the queue under a fresh id.
    Retry,
    /// Drop the message for good.
    Cancel,
}

impl ResolveAction {
    /// Parse the CLI spelling.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "retry" => Some(Self::Retry),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Cancel => "cancel",
        }
    }
}

/// What a legacy export wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyExport {
    /// Messages written to the legacy JSON.
    pub exported: usize,
    /// The file that was written.
    pub path: PathBuf,
}

/// Why an export was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportRefused {
    /// One reason per blocking condition.
    pub reasons: Vec<String>,
}

/// A failure in this layer, keeping the journal's kind so callers can branch.
#[derive(Debug)]
pub struct InputError {
    kind: InputErrorKind,
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputErrorKind {
    /// The journal refused the write; nothing changed.
    Journal(JournalErrorKind),
    /// The input is not live, or the state machine refuses the transition.
    NotLive,
    /// Local bookkeeping around the journal failed.
    Bookkeeping,
}

impl InputError {
    fn journal(error: JournalError) -> Self {
        Self {
            kind: InputErrorKind::Journal(error.kind()),
            message: error.to_string(),
        }
    }

    fn not_live(message: impl Into<String>) -> Self {
        Self {
            kind: InputErrorKind::NotLive,
            message: message.into(),
        }
    }

    fn bookkeeping(message: impl Into<String>) -> Self {
        Self {
            kind: InputErrorKind::Bookkeeping,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> InputErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InputError {}

impl From<JournalError> for InputError {
    fn from(error: JournalError) -> Self {
        Self::journal(error)
    }
}

type InputResult<T> = std::result::Result<T, InputError>;

/// One session's durable input queue, over its runtime journal.
///
/// The store is synchronous like the journal it wraps: callers on an async
/// worker must run it on a blocking thread.
pub struct DurableInputStore {
    session_id: String,
    store: DurableRuntimeStore,
}

impl DurableInputStore {
    /// Open the queue for one session under the agent home.
    pub fn open(session_id: &str) -> Result<Self> {
        let store = DurableRuntimeStore::open(session_id).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Self {
            session_id: session_id.to_string(),
            store,
        })
    }

    /// Open the queue for one session under an explicit journal root.
    pub fn open_in(root: &Path, session_id: &str) -> Result<Self> {
        let store =
            DurableRuntimeStore::open_in(root, session_id).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Self {
            session_id: session_id.to_string(),
            store,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The journal directory this session owns.
    pub fn directory(&self) -> &Path {
        self.store.directory()
    }

    /// Replay and hand back the journal state.
    pub fn state(&mut self) -> InputResult<&RuntimeState> {
        self.store.replay()?;
        Ok(self.store.state())
    }

    /// Accept one message, syncing `input.accepted` before the caller queues it.
    ///
    /// `input.input_id` is used when it is already set, which is how a restored
    /// or retried message keeps its identity; otherwise a fresh id is minted and
    /// written back into `input`. An id that is already live or already
    /// terminal is not appended twice: the call reports `already_live` so a
    /// repeated restore cannot duplicate an input.
    pub fn accept(&mut self, input: &mut SoftInterruptMessage) -> InputResult<AcceptedInput> {
        let requested = input.input_id.clone().or_else(|| {
            input
                .client_id
                .as_ref()
                .map(|client_id| derived_input_id(&self.session_id, client_id, &input.content))
        });
        let input_id = match requested {
            Some(id) => id,
            None => crate::durable_runtime::new_id(),
        };
        input.input_id = Some(input_id.clone());

        let already_recorded = {
            let state = self.state()?;
            state.inputs.contains_key(&input_id) || state.input_receipts.contains_key(&input_id)
        };
        if already_recorded {
            return Ok(AcceptedInput {
                input_id,
                already_live: true,
                from_legacy_baseline: false,
            });
        }

        let payload = Payload::InputAccepted(InputAccepted {
            client_id: input.client_id.clone(),
            content: input.content.clone(),
            images: input.images.clone(),
            urgent: input.urgent,
            source: source_name(input.source).to_string(),
        });
        self.store.append(&input_id, payload)?;
        Ok(AcceptedInput {
            input_id,
            already_live: false,
            from_legacy_baseline: false,
        })
    }

    /// Record that this input is about to be injected into a turn.
    ///
    /// An input that is already `started` is returned to pending first, because
    /// a turn that unwound without a terminal row leaves it started; the state
    /// machine spells that as `input.returned` and it keeps the retry honest
    /// instead of failing the injection.
    pub fn start(&mut self, input_id: &str, run_id: Option<&str>) -> InputResult<()> {
        let phase = self.phase_of(input_id)?;
        match phase {
            InputPhase::Accepted | InputPhase::Returned => {}
            InputPhase::Started => {
                self.store.append(
                    input_id,
                    Payload::InputReturned(InputReturned {
                        reason: "reinjected_after_unwound_turn".to_string(),
                    }),
                )?;
            }
            InputPhase::InDoubt => {
                return Err(InputError::not_live(format!(
                    "input {input_id} is sealed in doubt; resolve it with retry or cancel before injecting it"
                )));
            }
        }
        self.store.append(
            input_id,
            Payload::InputStarted(InputStarted {
                run_id: run_id.map(str::to_string),
            }),
        )?;
        Ok(())
    }

    /// Record that the turn carrying this input saved its conversation.
    pub fn commit(&mut self, input_id: &str, run_id: Option<&str>) -> InputResult<()> {
        let phase = self.phase_of(input_id)?;
        if phase != InputPhase::Started {
            return Err(InputError::not_live(format!(
                "input {input_id} is {phase:?}; only a started input commits"
            )));
        }
        self.store.append(
            input_id,
            Payload::InputCommitted(InputCommitted {
                run_id: run_id.map(str::to_string),
            }),
        )?;
        Ok(())
    }

    /// Record that a turn handed this input back to the pending queue.
    pub fn return_to_pending(&mut self, input_id: &str, reason: &str) -> InputResult<()> {
        let phase = self.phase_of(input_id)?;
        if phase != InputPhase::Started {
            return Err(InputError::not_live(format!(
                "input {input_id} is {phase:?}; only a started input returns to pending"
            )));
        }
        self.store.append(
            input_id,
            Payload::InputReturned(InputReturned {
                reason: reason.to_string(),
            }),
        )?;
        Ok(())
    }

    /// Drop one pending input for good.
    pub fn cancel(&mut self, input_id: &str, reason: &str) -> InputResult<()> {
        let phase = self.phase_of(input_id)?;
        if phase == InputPhase::Started {
            return Err(InputError::not_live(format!(
                "input {input_id} is running; seal it in doubt or return it before cancelling"
            )));
        }
        self.store.append(
            input_id,
            Payload::InputCancelled(InputCancelled {
                reason: Some(reason.to_string()),
            }),
        )?;
        Ok(())
    }

    /// Seal a started input whose outcome this process cannot know.
    pub fn seal_in_doubt(&mut self, input_id: &str, cause: &str) -> InputResult<()> {
        let phase = self.phase_of(input_id)?;
        if phase != InputPhase::Started {
            return Ok(());
        }
        self.store.append(
            input_id,
            Payload::InputInDoubt(InputInDoubt {
                cause: Some(cause.to_string()),
            }),
        )?;
        Ok(())
    }

    /// The phase one input currently holds.
    pub fn phase_of(&mut self, input_id: &str) -> InputResult<InputPhase> {
        let state = self.state()?;
        if let Some(entry) = state.inputs.get(input_id) {
            return Ok(entry.phase);
        }
        Err(InputError::not_live(format!(
            "input {input_id} is not live in session {}",
            self.session_id
        )))
    }

    /// Restore the pending queue after a restart.
    ///
    /// `in_flight` names the inputs this process still holds injected, so a
    /// live turn's started input is not sealed while it is running. Everything
    /// else that is started has no terminal row and is sealed in doubt.
    pub fn recover(&mut self, in_flight: &[String]) -> InputResult<InputRecovery> {
        let mut report = InputRecovery {
            imported_from_legacy: self.import_legacy_baseline()?,
            ..InputRecovery::default()
        };

        let started: Vec<(String, InputPhase)> = {
            let state = self.state()?;
            state
                .inputs
                .iter()
                .map(|(id, entry)| (id.clone(), entry.phase))
                .collect()
        };
        for (input_id, phase) in started {
            match phase {
                InputPhase::Started => {
                    if in_flight.iter().any(|id| id == &input_id) {
                        continue;
                    }
                    if self.seal_in_doubt(&input_id, "restart_before_terminal").is_ok() {
                        report.sealed_in_doubt.push(input_id);
                    }
                }
                InputPhase::InDoubt => report.already_in_doubt.push(input_id),
                InputPhase::Accepted | InputPhase::Returned => {}
            }
        }

        let pending = {
            let state = self.state()?;
            state
                .inputs
                .iter()
                .filter(|(_, entry)| matches!(entry.phase, InputPhase::Accepted | InputPhase::Returned))
                .map(|(id, entry)| (entry.seq, message_from_entry(id, entry)))
                .collect::<BTreeMap<u64, SoftInterruptMessage>>()
        };
        report.pending = pending.into_values().collect();
        Ok(report)
    }

    /// Apply an explicit decision to an input whose outcome is unknown.
    ///
    /// `retry` first removes the transcript copy this input previously produced
    /// when one is recorded, then cancels the undetermined id and admits the
    /// same message under a fresh id, so the queue holds exactly one copy again.
    /// It does not promise that a tool the previous attempt ran will not run
    /// again.
    pub fn resolve(
        &mut self,
        input_id: &str,
        action: ResolveAction,
        remove_injected_copy: impl FnOnce(&str) -> bool,
    ) -> InputResult<ResolvedInput> {
        let state = self.state()?;
        let entry = state
            .inputs
            .get(input_id)
            .cloned()
            .ok_or_else(|| InputError::not_live(format!("input {input_id} is not live")))?;
        // An operator reaches for this command precisely when the process that
        // started the input is gone, so nothing sealed it. Sealing it here is
        // the honest reading - the outcome is unknown - and it is what lets the
        // decision below apply to a `started` input at all.
        if entry.phase == InputPhase::Started {
            self.seal_in_doubt(input_id, "resolved_without_terminal")?;
        }
        let mut cleaned_injected_copy = false;
        let mut warning = None;
        if action == ResolveAction::Retry {
            match self.injected_copy_of(input_id)? {
                Some(message_id) => {
                    cleaned_injected_copy = remove_injected_copy(&message_id);
                    if !cleaned_injected_copy {
                        warning = Some(format!(
                            "the transcript copy {message_id} of input {input_id} was already gone"
                        ));
                    }
                }
                None => {
                    warning = Some(format!(
                        "no transcript copy was recorded for input {input_id}, so only the queue entry is settled"
                    ));
                }
            }
        }

        self.store.append(
            input_id,
            Payload::InputCancelled(InputCancelled {
                reason: Some(format!("resolved_{}", action.as_str())),
            }),
        )?;

        let requeued_as = match action {
            ResolveAction::Cancel => None,
            ResolveAction::Retry => {
                let fresh = crate::durable_runtime::new_id();
                self.store
                    .append(&fresh, Payload::InputAccepted(entry.accepted.clone()))?;
                self.record_injected_copy(&fresh, None)?;
                Some(fresh)
            }
        };
        self.forget_injected_copy(input_id)?;
        Ok(ResolvedInput {
            input_id: input_id.to_string(),
            previous_phase: entry.phase,
            requeued_as,
            cleaned_injected_copy,
            warning,
        })
    }

    /// Every input still holding work, in journal order.
    pub fn live_messages(&mut self) -> InputResult<Vec<SoftInterruptMessage>> {
        let state = self.state()?;
        let mut ordered = state
            .inputs
            .iter()
            .map(|(id, entry)| (entry.seq, message_from_entry(id, entry)))
            .collect::<Vec<_>>();
        ordered.sort_by_key(|(seq, _)| *seq);
        Ok(ordered.into_iter().map(|(_, message)| message).collect())
    }

    /// Mirror the live set to the legacy JSON, fsynced.
    ///
    /// Inputs sealed in doubt are left out on purpose: an older build reading
    /// this file would inject them, and the whole point of the seal is that
    /// nobody injects them without an explicit decision.
    pub fn mirror(&mut self) -> InputResult<usize> {
        let messages: Vec<SoftInterruptMessage> = {
            let state = self.state()?;
            let mut ordered = state
                .inputs
                .iter()
                .filter(|(_, entry)| entry.phase != InputPhase::InDoubt)
                .map(|(id, entry)| (entry.seq, message_from_entry(id, entry)))
                .collect::<Vec<_>>();
            ordered.sort_by_key(|(seq, _)| *seq);
            ordered.into_iter().map(|(_, message)| message).collect()
        };
        match crate::soft_interrupt_store::overwrite_durable(&self.session_id, &messages) {
            Ok(()) => {
                self.clear_mirror_dirty()?;
                Ok(messages.len())
            }
            Err(error) => {
                let _ = self.mark_mirror_dirty(&error.to_string());
                Err(InputError::bookkeeping(format!(
                    "the legacy mirror for session {} was not written: {error}",
                    self.session_id
                )))
            }
        }
    }

    /// Write the pending set to the legacy JSON so an older build can take over.
    ///
    /// Refused unless the session is quiescent: no started input, no input
    /// sealed in doubt, and no known-stale mirror. Anything else would either
    /// lose an undetermined message or hand an older build a queue this one
    /// knows is wrong.
    pub fn export_legacy(&mut self) -> Result<Result<LegacyExport, ExportRefused>> {
        let state = self.state()?;
        let mut reasons = Vec::new();
        let started: Vec<&String> = state
            .inputs
            .iter()
            .filter(|(_, entry)| entry.phase == InputPhase::Started)
            .map(|(id, _)| id)
            .collect();
        if !started.is_empty() {
            reasons.push(format!(
                "{} input(s) are still running: {}",
                started.len(),
                started.iter().map(|id| id.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        let in_doubt: Vec<&String> = state
            .inputs
            .iter()
            .filter(|(_, entry)| entry.phase == InputPhase::InDoubt)
            .map(|(id, _)| id)
            .collect();
        if !in_doubt.is_empty() {
            reasons.push(format!(
                "{} input(s) are sealed in doubt: {}",
                in_doubt.len(),
                in_doubt.iter().map(|id| id.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        if let Some(error) = self.mirror_dirty_reason()? {
            reasons.push(format!("the legacy mirror is known to be stale: {error}"));
        }
        if !reasons.is_empty() {
            return Ok(Err(ExportRefused { reasons }));
        }

        let messages = self.live_messages()?;
        crate::soft_interrupt_store::overwrite_durable(&self.session_id, &messages)
            .context("write the legacy pending-input file")?;
        self.clear_mirror_dirty()?;
        let path = crate::soft_interrupt_store::path_for_session(&self.session_id)?;
        Ok(Ok(LegacyExport {
            exported: messages.len(),
            path,
        }))
    }

    /// Import the legacy JSON once, as the baseline for a session that predates
    /// the journal.
    fn import_legacy_baseline(&mut self) -> InputResult<usize> {
        if self.baseline_imported()? {
            return Ok(0);
        }
        let legacy = crate::soft_interrupt_store::load(&self.session_id).unwrap_or_default();
        let mut imported = 0usize;
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();
        for (index, mut message) in legacy.into_iter().enumerate() {
            if message.input_id.is_none() {
                message.input_id = Some(legacy_baseline_id(&self.session_id, index, &message));
            }
            let input_id = message.input_id.clone().unwrap_or_default();
            if seen.insert(input_id.clone(), ()).is_some() {
                continue;
            }
            let already = {
                let state = self.state()?;
                state.inputs.contains_key(&input_id) || state.input_receipts.contains_key(&input_id)
            };
            if already {
                continue;
            }
            self.store.append(
                &input_id,
                Payload::InputAccepted(InputAccepted {
                    client_id: message.client_id.clone(),
                    content: message.content.clone(),
                    images: message.images.clone(),
                    urgent: message.urgent,
                    source: source_name(message.source).to_string(),
                }),
            )?;
            imported += 1;
        }
        self.write_baseline_marker(imported as u64)?;
        Ok(imported)
    }

    fn baseline_imported(&self) -> InputResult<bool> {
        let path = self.directory().join(LEGACY_BASELINE_FILE);
        Ok(path.exists())
    }

    fn write_baseline_marker(&self, imported: u64) -> InputResult<()> {
        let path = self.directory().join(LEGACY_BASELINE_FILE);
        let marker = BaselineMarker {
            v: 1,
            imported,
            at: chrono::Utc::now(),
        };
        crate::storage::write_json(&path, &marker)
            .map_err(|error| InputError::bookkeeping(format!("record the legacy baseline: {error}")))
    }

    fn mirror_dirty_reason(&self) -> InputResult<Option<String>> {
        let path = self.directory().join(MIRROR_STATE_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let state: MirrorState = crate::storage::read_json(&path)
            .map_err(|error| InputError::bookkeeping(format!("read the mirror state: {error}")))?;
        Ok(state
            .dirty
            .then(|| state.error.unwrap_or_else(|| "unknown".to_string())))
    }

    fn mark_mirror_dirty(&self, error: &str) -> InputResult<()> {
        let path = self.directory().join(MIRROR_STATE_FILE);
        let state = MirrorState {
            v: 1,
            dirty: true,
            at: chrono::Utc::now(),
            error: Some(error.to_string()),
        };
        crate::storage::write_json(&path, &state)
            .map_err(|write| InputError::bookkeeping(format!("record the mirror failure: {write}")))
    }

    fn clear_mirror_dirty(&self) -> InputResult<()> {
        let path = self.directory().join(MIRROR_STATE_FILE);
        if !path.exists() {
            return Ok(());
        }
        let state = MirrorState {
            v: 1,
            dirty: false,
            at: chrono::Utc::now(),
            error: None,
        };
        crate::storage::write_json(&path, &state)
            .map_err(|error| InputError::bookkeeping(format!("clear the mirror state: {error}")))
    }

    /// Record which transcript message an input's injection produced.
    fn record_injected_copy(&self, input_id: &str, message_id: Option<&str>) -> InputResult<()> {
        let Some(message_id) = message_id else {
            return Ok(());
        };
        let mut book = self.read_injected_copies()?;
        book.entries.retain(|entry| entry.input_id != input_id);
        book.entries.push(InjectedCopy {
            input_id: input_id.to_string(),
            message_id: message_id.to_string(),
            at: chrono::Utc::now(),
        });
        while book.entries.len() > INJECTED_COPY_CAP {
            book.entries.remove(0);
        }
        self.write_injected_copies(&book)
    }

    /// Remember the transcript message one input's injection produced.
    pub fn note_injected_copy(&self, input_id: &str, message_id: &str) -> InputResult<()> {
        self.record_injected_copy(input_id, Some(message_id))
    }

    pub fn injected_copy_of(&self, input_id: &str) -> InputResult<Option<String>> {
        let book = self.read_injected_copies()?;
        Ok(book
            .entries
            .into_iter()
            .find(|entry| entry.input_id == input_id)
            .map(|entry| entry.message_id))
    }

    fn forget_injected_copy(&self, input_id: &str) -> InputResult<()> {
        let mut book = self.read_injected_copies()?;
        let before = book.entries.len();
        book.entries.retain(|entry| entry.input_id != input_id);
        if book.entries.len() == before {
            return Ok(());
        }
        self.write_injected_copies(&book)
    }

    fn read_injected_copies(&self) -> InputResult<InjectedCopyBook> {
        let path = self.directory().join(INJECTED_COPIES_FILE);
        if !path.exists() {
            return Ok(InjectedCopyBook::default());
        }
        crate::storage::read_json(&path)
            .map_err(|error| InputError::bookkeeping(format!("read the injected-copy book: {error}")))
    }

    fn write_injected_copies(&self, book: &InjectedCopyBook) -> InputResult<()> {
        let path = self.directory().join(INJECTED_COPIES_FILE);
        crate::storage::write_json(&path, book)
            .map_err(|error| InputError::bookkeeping(format!("write the injected-copy book: {error}")))
    }
}

#[derive(Serialize, Deserialize)]
struct BaselineMarker {
    v: u32,
    imported: u64,
    at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize, Deserialize)]
struct MirrorState {
    v: u32,
    dirty: bool,
    at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct InjectedCopyBook {
    #[serde(default)]
    entries: Vec<InjectedCopy>,
}

#[derive(Serialize, Deserialize)]
struct InjectedCopy {
    input_id: String,
    message_id: String,
    at: chrono::DateTime<chrono::Utc>,
}

/// The source label the journal stores.
pub fn source_name(source: SoftInterruptSource) -> &'static str {
    match source {
        SoftInterruptSource::User => "user",
        SoftInterruptSource::System => "system",
        SoftInterruptSource::BackgroundTask => "background_task",
    }
}

fn source_from_name(name: &str) -> SoftInterruptSource {
    match name {
        "system" => SoftInterruptSource::System,
        "background_task" => SoftInterruptSource::BackgroundTask,
        _ => SoftInterruptSource::User,
    }
}

fn message_from_entry(input_id: &str, entry: &crate::durable_runtime::InputEntry) -> SoftInterruptMessage {
    SoftInterruptMessage {
        content: entry.accepted.content.clone(),
        images: entry.accepted.images.clone(),
        urgent: entry.accepted.urgent,
        source: source_from_name(&entry.accepted.source),
        client_id: entry.accepted.client_id.clone(),
        input_id: Some(input_id.to_string()),
    }
}

/// The deterministic id a legacy JSON message gets on its one baseline import.
///
/// It depends on the session, the message's position, its caller id, and its
/// content, so a second import of the same file maps to the same ids and the
/// journal refuses to duplicate them even if the marker file is lost.
pub fn legacy_baseline_id(session_id: &str, index: usize, message: &SoftInterruptMessage) -> String {
    let digest = Sha256::digest(message.content.as_bytes());
    let client = message.client_id.as_deref().unwrap_or("-");
    let name = format!(
        "horon-input/{session_id}/{index}/{client}/{}/{}",
        hex::encode(digest),
        message.urgent
    );
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).to_string()
}

/// The id a caller-supplied id maps to when a message carries no journal id.
pub fn derived_input_id(session_id: &str, client_id: &str, content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let name = format!("horon-client-input/{session_id}/{client_id}/{}", hex::encode(digest));
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).to_string()
}

#[cfg(test)]
#[path = "durable_input_tests.rs"]
mod durable_input_tests;
