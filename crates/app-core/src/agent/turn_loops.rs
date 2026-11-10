use super::response_recovery::MalformedToolCallInfo;
use super::*;
use crate::{terminal_eprintln as eprintln, terminal_print as print, terminal_println as println};
use futures::FutureExt;
use std::panic::AssertUnwindSafe;

use super::turn_failover::ProviderErrorAction;
use horon_agent_agent_runtime::{
    RunIdentity, RunStop, StopDisposition, StopProvenance, current_run, current_stop, note_conversation_save,
    observe_cause, observed_cause, provider_failure_provenance, record_stop,
};
use horon_agent_base::durable_runtime::{
    DurableRuntimeStore, Payload, RunStarted, RunStopped, StopCause as JournalStopCause,
};

/// How long a reload marker still describes this process's own shutdown.
const RELOAD_MARKER_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(120);

/// The session journals this process already replayed.
///
/// Reopening a journal replays it, and a run boundary is once per turn, so
/// reopening per turn would turn every turn into a full journal replay. The
/// store is kept instead and reused: it already catches up on rows other writers
/// appended before every append, so a cached handle never serves stale state.
static RUN_JOURNALS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, DurableRuntimeStore>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Dispatch one run row to `session_id`'s journal on a dedicated OS thread.
///
/// The handle is returned rather than awaited, because this entry point must
/// reach its turn and publish its terminal event without yielding: an await in
/// the turn's tail lets the caller's own timers run before that event lands.
/// The write still starts before the turn does and is joined before the stop is
/// recorded, so the journal keeps start-before-stop order, and the join blocks a
/// few milliseconds instead of yielding the worker.
fn dispatch_run_row(session_id: &str, run_id: &str, payload: Payload) -> std::thread::JoinHandle<bool> {
    let session_id = session_id.to_string();
    let run_id = run_id.to_string();
    std::thread::spawn(move || append_run_row(&session_id, &run_id, payload))
}

/// `run.started` writes dispatched but not yet joined, keyed by session.
///
/// The handle is held here rather than on `Agent` because the entry point must
/// reach its turn without yielding first, and this registry lets the write start
/// before the turn while its join waits until the turn ends.
static PENDING_RUN_STARTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::thread::JoinHandle<bool>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Append one run row to `session_id`'s journal, on the calling (blocking) thread.
fn append_run_row(session_id: &str, run_id: &str, payload: Payload) -> bool {
    let Ok(root) = horon_agent_base::storage::horon_agent_dir() else {
        logging::warn(&format!("run row not recorded for {session_id}: no agent home"));
        return false;
    };
    let key = root
        .join(horon_agent_base::durable_runtime::JOURNAL_DIR_NAME)
        .join(session_id);
    let mut journals = RUN_JOURNALS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let store = match journals.entry(key.clone()) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => match DurableRuntimeStore::open(session_id) {
            Ok(store) => entry.insert(store),
            Err(error) => {
                logging::warn(&format!("run row not recorded for {session_id}: {error}"));
                return false;
            }
        },
    };
    match store.append(run_id, payload) {
        Ok(_) => true,
        Err(error) => {
            logging::warn(&format!("run row not recorded for {session_id}: {error}"));
            // A corrupt or gapped journal must not be retried as transient, so
            // the handle is dropped and the next attempt replays it afresh.
            journals.remove(&key);
            false
        }
    }
}

/// The journal's spelling of a stop cause.
fn journal_cause(provenance: StopProvenance) -> JournalStopCause {
    match provenance {
        StopProvenance::Completed => JournalStopCause::Completed,
        StopProvenance::UserCancelled => JournalStopCause::UserCancelled,
        StopProvenance::Shutdown => JournalStopCause::Shutdown,
        StopProvenance::ProviderError => JournalStopCause::ProviderError,
        StopProvenance::Timeout => JournalStopCause::Timeout,
        StopProvenance::LoopGuard => JournalStopCause::LoopGuard,
        StopProvenance::Limit => JournalStopCause::Limit,
        StopProvenance::InterruptedUnknown => JournalStopCause::InterruptedUnknown,
    }
}

/// The stop cause a failed turn names, read from the error it returned.
///
/// A context, token, or plan limit ends a turn through the same error path as a
/// provider fault, so the message decides between them. Every other failure is
/// what the provider reported.
pub(super) fn cause_for_turn_failure(message: &str) -> StopProvenance {
    let lower = message.to_ascii_lowercase();
    if lower.contains("context limit") || lower.contains("token limit") || lower.contains("plan limit") {
        StopProvenance::Limit
    } else {
        provider_failure_provenance(message)
    }
}

impl Agent {
    /// Mint the run id a newly triggered input executes under.
    ///
    /// The id is a UUID v4 because the journal rejects any other spelling, so
    /// this is the one place a run id is created.
    pub(super) fn mint_run_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Start the run this turn executes, recording `run.started` when it begins.
    ///
    /// The journal write is dispatched to a blocking thread and never performed
    /// while an async lock is held. A failed write leaves the run unrecorded and
    /// the turn still runs: an audit row is worth less than the answer the user
    /// asked for.
    pub(super) fn begin_run(&mut self) -> RunIdentity {
        let identity = RunIdentity::new(Self::mint_run_id());
        // Dispatched, not awaited: the streaming entry point must reach its turn
        // without yielding first. The write still starts before the turn runs and
        // is joined before the stop is recorded, so start precedes stop on disk.
        let handle = dispatch_run_row(
            &self.session.id,
            identity.run_id(),
            Payload::RunStarted(RunStarted { input_id: None }),
        );
        PENDING_RUN_STARTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(self.session.id.clone(), handle);
        identity
    }

    /// Await this turn's `run.started` write and adopt its result.
    ///
    /// The join is synchronous on purpose: this runs at the turn's end, where a
    /// yield would let a caller's timers fire before the terminal event is
    /// published. It blocks for the few milliseconds the write takes.
    pub(super) async fn settle_run_start(&mut self, identity: &RunIdentity) {
        let handle = PENDING_RUN_STARTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.session.id);
        let Some(handle) = handle else {
            return;
        };
        let persisted = handle.join().unwrap_or(false);
        horon_agent_agent_runtime::begin_run(&self.session.id, identity.clone(), persisted);
    }

    /// Record a cause the loop observed at a point that ends the turn.
    ///
    /// Only a terminal observation belongs here: a retryable provider failure the
    /// turn recovered from is not a cause, so no path that continues the loop
    /// records one.
    pub(super) fn observe_stop_cause(&self, provenance: StopProvenance) {
        observe_cause(&self.session.id, provenance);
    }

    /// Record whether this turn's conversation save succeeded.
    pub(super) fn note_turn_conversation_save(&self, saved: bool) {
        note_conversation_save(&self.session.id, saved);
    }

    /// The cause this turn's run stopped for, from what the turn observed.
    ///
    /// An observation the loop made at the exact point the turn ended wins,
    /// because the outcome alone cannot tell a loop-guard stop from a normal one.
    /// Otherwise the recorded outcome names it: a truncation that spent its
    /// continuation budget is a limit, an abort is a shutdown only when this
    /// process is reloading and a cancel otherwise, and a failure is whatever its
    /// own error says.
    pub(super) fn stop_cause_for(&self, error: Option<&str>) -> StopProvenance {
        if let Some(observed) = observed_cause(&self.session.id) {
            return observed;
        }
        match self.turn_state().outcome() {
            Some(TurnOutcome::Stop) => StopProvenance::Completed,
            Some(TurnOutcome::Length) => StopProvenance::Limit,
            Some(TurnOutcome::Aborted) => {
                if crate::server::reload_marker_active(RELOAD_MARKER_MAX_AGE) {
                    StopProvenance::Shutdown
                } else {
                    StopProvenance::UserCancelled
                }
            }
            Some(TurnOutcome::Error) => match error {
                Some(message) => cause_for_turn_failure(message),
                None => StopProvenance::ProviderError,
            },
            None => StopProvenance::InterruptedUnknown,
        }
    }

    /// End the run captured at turn start, after the conversation save.
    ///
    /// The caller passes the run id and cause captured when the turn ended, so
    /// this writes what happened rather than what is current now. The in-memory
    /// stop is recorded and kept even when the journal write fails; only a
    /// successful write marks it persisted.
    ///
    /// Returns where the stop landed, so a caller can tell a late stop recorded
    /// as history from the current run's own end.
    pub(super) async fn finish_run(&self, run_id: &str, provenance: StopProvenance) -> StopDisposition {
        let mut stop = RunStop::capture(run_id, provenance);
        let session_id = self.session.id.clone();
        let cause = journal_cause(provenance);
        let handle = dispatch_run_row(
            &self.session.id,
            run_id,
            Payload::RunStopped(RunStopped { cause, input_id: None }),
        );
        // Synchronous join, for the same reason as the start: the turn's tail
        // must not yield, and the write itself already runs off this thread.
        let persisted = handle.join().unwrap_or(false);
        if persisted {
            stop.mark_persisted();
        }
        let conversation_saved = horon_agent_agent_runtime::conversation_saved(&session_id);
        let disposition = record_stop(&session_id, stop);
        logging::info(&format!(
            "run.stopped session={session_id} run={run_id} cause={} persisted={persisted} conversation_saved={conversation_saved:?} disposition={disposition:?}",
            provenance.as_str()
        ));
        disposition
    }

    /// The run executing now, if a run was started.
    pub fn current_run(&self) -> Option<RunIdentity> {
        current_run(&self.session.id)
    }

    /// The current run's stop, if it ended.
    pub fn current_stop(&self) -> Option<RunStop> {
        current_stop(&self.session.id)
    }

    /// Stops this session kept because their run was no longer current.
    pub fn stop_history(&self) -> Vec<RunStop> {
        horon_agent_agent_runtime::stop_history(&self.session.id)
    }

    /// Speculatively prewarm the provider while a newly created session is idle.
    ///
    /// This deliberately bypasses `tool_definitions`, whose cache lock is only
    /// appropriate once an actual turn starts. Late MCP registration or user
    /// customization can therefore still change the foreground tool snapshot;
    /// the provider is responsible for discarding an incompatible warmup.
    /// Everything a prewarm needs, taken in one short borrow.
    ///
    /// The caller must drop the session lock before awaiting the provider with
    /// this value: holding it across that await would make the first real
    /// request queue behind a speculative warmup, which is the wait this
    /// prewarm exists to avoid.
    pub(crate) async fn prewarm_snapshot(
        &self,
    ) -> (
        Arc<dyn crate::provider::Provider>,
        Vec<crate::message::ToolDefinition>,
        String,
        horon_agent_provider_core::ProviderRequestContext,
    ) {
        // A warmup must send the same static prefix the first real turn sends,
        // so the index is built here exactly as `tool_definitions` builds it.
        self.ensure_listed_tools_index().await;
        let tools = self.tool_definitions_for_debug().await;
        let split_prompt = self.build_system_prompt_split(None);
        (
            Arc::clone(&self.provider),
            tools,
            split_prompt.static_part,
            self.provider_request_context(),
        )
    }

    /// Run turns until no more tool calls
    /// Maximum number of context-limit compaction retries before giving up.
    pub(super) const MAX_CONTEXT_LIMIT_RETRIES: u32 = 5;
    pub(super) const MAX_INCOMPLETE_CONTINUATION_ATTEMPTS: u32 = 3;
    /// Retries allowed when the provider returns an empty response right after
    /// tool results. This is a transient provider hiccup, not a signal that the
    /// task is finished, so a single retry is too few: one empty response
    /// observed once in 43 turns silently ended a 20-hour benchmark run with the
    /// task half-done. The counter is per turn-loop, so a genuinely finished
    /// agent still exits promptly.
    pub(crate) const MAX_EMPTY_POST_TOOL_CONTINUATION_ATTEMPTS: u32 = 5;
    const SEQUENTIAL_TOOL_ROUNDS_BEFORE_BATCH_NUDGE: u32 = 3;
    const BATCH_NUDGE: &str = "<system-reminder>Several tool calls have been made one at a time. If the next independent operations can run concurrently, use the batch tool instead of making more sequential calls. Keep sequential calls when one result is required to decide the next operation.</system-reminder>";

    /// Result written for a call of a round that never ran because the loop
    /// guard ended the turn at an earlier call.
    pub(crate) const LOOP_GUARD_SKIP: &str = "[Skipped: the loop guard ended the turn]";

    /// Group one round of tool calls for the shared scheduler.
    ///
    /// `parallel_tools = false` collapses the round into one-member groups, so
    /// the round runs exactly as it did before concurrency existed. Tests call
    /// this with the flag explicit to observe the disabled path without timing.
    pub(super) async fn plan_tool_round_groups(
        &self,
        calls: &[ToolCall],
        parallel_tools: bool,
    ) -> Vec<super::tool_round::ToolGroup> {
        if !parallel_tools {
            // A barrier effect per call collapses the round into one-member
            // groups, which is exactly the pre-concurrency behavior.
            let barriers = vec![crate::tool::ToolEffects::write(); calls.len()];
            return super::tool_round::plan_groups(&barriers);
        }
        super::tool_round::plan_round(&self.registry, &self.session.id, calls).await
    }

    /// Loop-guard verdicts for one dispatch decision, in the order the calls
    /// were handed in.
    ///
    /// `calls` are the calls this decision is about to dispatch: one `Ordered`
    /// call, or every member of a concurrent group. A group is ONE decision, so
    /// every member is recorded here before any of them runs and no verdict is
    /// ever produced while a call is in flight.
    ///
    /// A `Warn` verdict is merged into `current_turn_system_reminder`, which
    /// `build_system_prompt_split` carries into the next request of this same
    /// turn. Merging rather than assigning keeps a plan-limit notice or a batch
    /// nudge that was already set, and the merge is bounded so a turn that
    /// wanders through several cycles cannot grow the system prompt on every
    /// request. With `features.loop_guard` off the guard is never consulted and
    /// nothing is recorded.
    pub(super) fn consult_loop_guard<'a>(
        &mut self,
        calls: impl IntoIterator<Item = &'a ToolCall>,
    ) -> Vec<super::loop_guard::LoopGuardVerdict> {
        use super::loop_guard::LoopGuardVerdict;

        let calls: Vec<&ToolCall> = calls.into_iter().collect();
        if !crate::config::config().features.loop_guard {
            return vec![LoopGuardVerdict::Allow; calls.len()];
        }

        let mut verdicts = Vec::with_capacity(calls.len());
        for call in calls {
            let verdict = self.loop_guard.record_and_decide(&call.name, &call.input);
            if let LoopGuardVerdict::Warn(message) = &verdict {
                Self::merge_system_reminder(&mut self.current_turn_system_reminder, message);
            }
            verdicts.push(verdict);
        }
        verdicts
    }

    /// Header line that stands in for the reminders a bounded merge dropped.
    ///
    /// The slot rides the system prompt (`build_system_prompt_split`), so a
    /// turn that wanders through several cycles used to grow the request on
    /// every warning and move the cache prefix. The slot now keeps the
    /// reminders whose removal would cost the most within a fixed size and
    /// replaces the rest with this count.
    const DROPPED_REMINDER_HEADER: &str = "[earlier reminders dropped: ";

    /// Header appended when the slot still exceeds its size cap after every
    /// reminder it could drop is gone.
    ///
    /// That happens when one reminder alone is larger than the cap, and the
    /// slot keeps it whole rather than cutting it. Saying so is the difference
    /// between a slot that is over its budget and one that claims to be inside
    /// it: without this line the frame reads as an ordinary bounded slot and a
    /// reader has no way to know the budget was not met.
    const OVER_CAP_REMINDER_HEADER: &str = "[slot over the character cap: kept whole]";

    /// Opening line of a framed slot. It names how many reminders the frame
    /// holds, so a reader knows how many entries follow instead of inferring
    /// boundaries from blank lines.
    const REMINDER_FRAME_OPEN: &str = "[system reminders: ";

    /// Opening of one framed reminder's own line, which carries its position
    /// and its byte length.
    ///
    /// The length is what makes the frame unambiguous. A reminder is arbitrary
    /// text: it may hold blank lines, and it may even hold a line that looks
    /// like a frame. Splitting on a separator therefore guesses, and guessing
    /// is what shredded one twelve-paragraph reminder into twelve. The reader
    /// consumes exactly the declared number of bytes, so no reminder's content
    /// can be mistaken for a boundary.
    const REMINDER_ENTRY_OPEN: &str = "[system reminder ";

    /// Reminders kept at once. Small reminders from different subsystems are
    /// all worth reading (a plan-limit notice and one guard warning), so the
    /// cap counts reminders rather than keeping only the newest one. Which one
    /// goes when the count is exceeded is [`Self::bound_reminders`]'s policy:
    /// the cheapest, so a burst of warnings cannot evict the turn's instruction.
    const MAX_RETAINED_REMINDERS: usize = 3;

    /// Characters the slot may hold before a reminder is dropped.
    ///
    /// Which reminder goes is [`Self::bound_reminders`]'s policy: the cheapest
    /// one to lose, measured by length alone. One reminder is never split: the
    /// cap drops whole reminders only, so no reminder's wording changes.
    const MAX_REMINDER_SLOT_CHARS: usize = 2_000;

    /// Split a slot entry into the count a previous merge reported and the
    /// reminders it still holds.
    ///
    /// A framed slot is parsed by its declared count and per-entry lengths. An
    /// entry no frame wrote - the turn-level reminder a reload or a soft
    /// interrupt assigns directly, which is ordinary prose and may hold any
    /// number of blank lines - is ONE reminder, exactly as it was before the
    /// slot was bounded at all.
    fn parse_reminder_slot(entry: &str) -> (usize, Vec<String>) {
        if entry.trim().is_empty() {
            return (0, Vec::new());
        }
        match Self::parse_framed_reminder_slot(entry) {
            Some(parsed) => parsed,
            None => (0, vec![entry.to_string()]),
        }
    }

    /// Parse a slot written by [`Self::render_reminder_slot`].
    ///
    /// Every step is validated against the text itself: the declared count,
    /// each entry's position and byte length, and that the entry ends exactly
    /// where its length says. A frame that does not validate is not a frame,
    /// so a reminder that merely starts with the frame's opening cannot be
    /// read as several reminders.
    fn parse_framed_reminder_slot(entry: &str) -> Option<(usize, Vec<String>)> {
        let rest = entry.strip_prefix(Self::REMINDER_FRAME_OPEN)?;
        let (kept, rest) = rest.split_once(']')?;
        let kept: usize = kept.strip_suffix(" kept")?.parse().ok()?;
        if kept == 0 {
            return None;
        }
        let (dropped, rest) = match rest.strip_prefix(' ') {
            Some(after) => match after.strip_prefix(Self::DROPPED_REMINDER_HEADER) {
                Some(after) => {
                    let (count, after) = after.split_once(']')?;
                    (count.trim().parse().ok()?, after)
                }
                // The over-cap notice can follow the count directly, so a space
                // does not imply a dropped-count header is present.
                None => (0, rest),
            },
            None => (0, rest),
        };
        // The over-cap notice is a statement about the entries that follow, not
        // a count, so it is skipped rather than returned: the caller re-derives
        // it from the rendered length.
        let rest = match rest.strip_prefix(' ') {
            Some(after) => after.strip_prefix(Self::OVER_CAP_REMINDER_HEADER).unwrap_or(rest),
            None => rest,
        };
        let mut rest = rest.strip_prefix('\n')?;

        // No capacity hint from `kept`: it is attacker-supplied text, and the
        // loop below stops at the first entry that does not validate.
        let mut reminders = Vec::new();
        for index in 0..kept {
            let line = rest.strip_prefix(Self::REMINDER_ENTRY_OPEN)?;
            let (meta, after) = line.split_once(']')?;
            let (position, total, length) = Self::parse_reminder_entry_meta(meta)?;
            if position != index + 1 || total != kept {
                return None;
            }
            let after = after.strip_prefix('\n')?;
            let text = after.get(..length)?;
            reminders.push(text.to_string());
            rest = &after[length..];
            if index + 1 < kept {
                rest = rest.strip_prefix('\n')?;
            } else if !rest.is_empty() {
                return None;
            }
        }
        Some((dropped, reminders))
    }

    /// Read `"1 of 2, 214 bytes"` from an entry's own line.
    fn parse_reminder_entry_meta(meta: &str) -> Option<(usize, usize, usize)> {
        let (position, rest) = meta.split_once(" of ")?;
        let (total, rest) = rest.split_once(", ")?;
        let length = rest.strip_suffix(" bytes")?;
        Some((position.parse().ok()?, total.parse().ok()?, length.parse().ok()?))
    }

    /// Join reminders back into one slot entry, naming how many were dropped.
    ///
    /// One reminder with nothing dropped is written bare: it is the whole slot,
    /// there is nothing to tell apart, and a directly assigned reminder already
    /// has that shape, so the common path adds no text at all.
    ///
    /// Two cases are framed instead. A reminder that would itself read as a
    /// frame must be, because writing it bare would make the slot ambiguous -
    /// the reader would take the reminder's own opening line for the slot's. Its
    /// bytes are then carried by the entry's declared length, which is the one
    /// thing a reminder's text cannot imitate, and the round trip stays lossless
    /// because rendering the parsed result produces the same frame again. A
    /// reminder larger than the slot's cap is framed for the same reason of
    /// honesty: the frame carries [`Self::OVER_CAP_REMINDER_HEADER`], so a slot
    /// that could not meet the cap says so instead of looking like one that did.
    fn render_reminder_slot(dropped: usize, reminders: &[String]) -> Option<String> {
        if reminders.is_empty() {
            return None;
        }
        if dropped == 0
            && reminders.len() == 1
            && reminders[0].chars().count() <= Self::MAX_REMINDER_SLOT_CHARS
            && Self::parse_framed_reminder_slot(&reminders[0]).is_none()
        {
            return Some(reminders[0].clone());
        }
        let mut header = format!("{}{} kept]", Self::REMINDER_FRAME_OPEN, reminders.len());
        if dropped > 0 {
            header.push_str(&format!(" {}{dropped}]", Self::DROPPED_REMINDER_HEADER));
        }
        let mut body = String::new();
        for (index, reminder) in reminders.iter().enumerate() {
            body.push_str(&format!(
                "{}{} of {}, {} bytes]\n",
                Self::REMINDER_ENTRY_OPEN,
                index + 1,
                reminders.len(),
                reminder.len()
            ));
            body.push_str(reminder);
            if index + 1 < reminders.len() {
                body.push('\n');
            }
        }
        // Measured on the frame without the notice: the notice states that the
        // entries are over the cap, so it must not be what makes them so. A slot
        // inside the cap renders without it, and one over the cap keeps it.
        let slot = format!("{header}\n{body}");
        if slot.chars().count() > Self::MAX_REMINDER_SLOT_CHARS {
            return Some(format!("{header} {}\n{body}", Self::OVER_CAP_REMINDER_HEADER));
        }
        Some(slot)
    }

    /// Trim a reminder list to the slot's caps, counting what it dropped.
    ///
    /// Applied both when a reminder is merged and when the slot is read, so a
    /// writer that assigns the slot directly (the turn-level reminder a reload
    /// or a soft interrupt supplies) is bounded on the way out too.
    ///
    /// The policy, in order:
    ///
    /// 1. Nothing is dropped while the slot is inside both caps.
    /// 2. When it overflows, the reminder whose removal costs the least is
    ///    dropped: the shortest one. Length is the only signal the slot reads,
    ///    so no reminder's wording can make it look disposable.
    /// 3. Equal lengths tie to the oldest, so the newest stays when the choice
    ///    is otherwise free and the outcome is deterministic.
    /// 4. A reminder is never split, and one reminder always survives: the loop
    ///    stops at a single entry, whichever entry that is. One reminder larger
    ///    than the cap is kept whole, and the frame says the cap was exceeded.
    ///
    /// Why shortest rather than oldest: the slot is shared by the text that
    /// frames a whole turn (the mission continuation, roughly 4 KB) and one-line
    /// guard warnings that fire routinely - three identical tool calls produce
    /// one. Removing the oldest made the routine warning the reason the turn's
    /// instructions vanished, and the header then reported one dropped reminder
    /// with no hint that the instruction was the one. Removing the shortest drops
    /// the warning instead, which is the reminder a reader can lose without
    /// losing the task.
    ///
    /// The rule is local and greedy: it removes one reminder at a time instead
    /// of searching for the combination that fits best. That is enough for the
    /// guarantee this needed - a one-line warning can never outlive the
    /// instruction it was appended to - and it stays cheap on a path that runs
    /// once per request. Two long instructions and a short warning lose the
    /// warning first and then the older instruction; three short reminders that
    /// still overflow lose the oldest short one. Every case ends with the same
    /// invariant: each drop was the cheapest one available, one reminder always
    /// survived, and the dropped count is true.
    fn bound_reminders(dropped: usize, mut reminders: Vec<String>) -> (usize, Vec<String>) {
        let mut dropped = dropped;
        // One loop for both caps, so the count cap cannot keep a short warning
        // while evicting the long instruction: whichever cap is exceeded, the
        // cheapest reminder goes. One reminder always survives, so a single
        // oversized reminder stays whole.
        while reminders.len() > 1
            && (reminders.len() > Self::MAX_RETAINED_REMINDERS
                || Self::render_reminder_slot(dropped, &reminders)
                    .is_some_and(|slot| slot.chars().count() > Self::MAX_REMINDER_SLOT_CHARS))
        {
            reminders.remove(Self::cheapest_to_drop(&reminders));
            dropped += 1;
        }
        (dropped, reminders)
    }

    /// Position of the reminder whose removal costs the least.
    ///
    /// Cost is length: the shortest reminder carries the least, so dropping it
    /// takes the least away. Ties go to the oldest, which leaves the newest in
    /// place when the choice is otherwise free, and the scan is stable, so the
    /// same input always yields the same victim. Characters are counted, matching
    /// the cap.
    fn cheapest_to_drop(reminders: &[String]) -> usize {
        let mut cheapest = 0;
        for (index, reminder) in reminders.iter().enumerate() {
            if reminder.chars().count() < reminders[cheapest].chars().count() {
                cheapest = index;
            }
        }
        cheapest
    }

    /// The reminder text this turn's request carries, bounded no matter which
    /// writer last set the slot.
    pub(super) fn bounded_turn_reminder(&self) -> Option<String> {
        let entry = self.current_turn_system_reminder.as_deref()?;
        if entry.trim().is_empty() {
            return None;
        }
        let (dropped, reminders) = Self::parse_reminder_slot(entry);
        let (dropped, reminders) = Self::bound_reminders(dropped, reminders);
        Self::render_reminder_slot(dropped, &reminders)
    }

    /// Keep this turn's reminders bounded without discarding what another
    /// subsystem already set.
    ///
    /// A plan-limit notice, a batch nudge, and a guard warning can all land in
    /// the same turn, and the model needs each of them, so reminders are merged
    /// rather than overwritten. The cycle ladder, though, emits a fresh warning
    /// for every new cycle, and a broken cycle resets the ladder so a later
    /// cycle warns again: a turn that wanders through several cycles appended
    /// one reminder per request and grew the system prompt with it. The slot
    /// therefore keeps [`Self::MAX_RETAINED_REMINDERS`] reminders within
    /// [`Self::MAX_REMINDER_SLOT_CHARS`], dropping the cheapest one to lose
    /// ([`Self::bound_reminders`]'s policy), and reports how many it dropped, so
    /// the slot stays bounded and readable. `run_turn` still clears the slot at
    /// the turn boundary, so nothing accumulates across turns.
    pub(crate) fn merge_system_reminder(slot: &mut Option<String>, message: &str) {
        let message = message.trim();
        if message.is_empty() {
            return;
        }
        let (dropped, reminders) = Self::parse_reminder_slot(slot.as_deref().unwrap_or_default());
        if reminders.last().is_some_and(|last| last == message) {
            return;
        }
        let mut reminders = reminders;
        reminders.push(message.to_string());
        let (dropped, reminders) = Self::bound_reminders(dropped, reminders);
        *slot = Self::render_reminder_slot(dropped, &reminders);
    }

    /// Store the one-line notice a loop-guard stop leaves behind.
    ///
    /// The notice is an assistant-role text message displayed as a system
    /// notice, so the model reads it on the next turn and the user reads it in
    /// the transcript.
    pub(super) fn record_loop_guard_stop(&mut self, notice: &str) {
        // The loop guard ends the turn through the ordinary stop path, so the
        // outcome alone cannot name it. This is the one place that observes it.
        self.observe_stop_cause(StopProvenance::LoopGuard);
        self.add_message_with_display_role(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: notice.to_string(),
                cache_control: None,
            }],
            Some(crate::session::StoredDisplayRole::System),
        );
    }

    pub(super) async fn run_turn(&mut self, print_output: bool) -> Result<String> {
        // Loop-detection state belongs to one turn, and a system reminder this
        // turn's guard set must not leak into the next one. The streaming entry
        // point cleared the reminder before this change; the non-streaming one
        // never did, which leaked a warn message across turns.
        self.loop_guard.reset();
        self.turn_state = TurnState::new();
        // A new run begins only here, where new external input starts executing.
        // Provider retries, failover switches, and automatic continuations stay
        // inside `run_turn_inner`, so they keep this id.
        let run = self.begin_run();
        let _turn_cancel_guard =
            crate::turn_cancel_registry::register_active_turn(&self.session.id, self.graceful_shutdown.clone());
        let mut in_flight = Some(InFlightGuard::new(
            self.session.id.clone(),
            self.soft_interrupt_queue.clone(),
        ));
        let outcome = AssertUnwindSafe(self.run_turn_inner(print_output, in_flight.as_mut()))
            .catch_unwind()
            .await;
        self.current_turn_system_reminder = None;
        match outcome {
            Ok(result) => {
                self.settle_soft_interrupt_turn(&mut in_flight, result.is_ok() && !self.is_graceful_shutdown())
                    .await;
                self.settle_turn_outcome(result.is_err());
                // The run id and cause are captured from the turn that just
                // ended, so a late stop can never describe a newer run.
                let cause = self.stop_cause_for(result.as_ref().err().map(|error| error.to_string()).as_deref());
                self.settle_run_start(&run).await;
                self.note_turn_conversation_save(result.is_ok());
                self.finish_run(run.run_id(), cause).await;
                result
            }
            Err(payload) => {
                // A panic never reaches the normal settle point. Roll the
                // injected messages back here, then let the unwind continue
                // so the caller still observes the panic it caused. The run is
                // left open on purpose: recovery reports it as
                // `interrupted_unknown`, never as completion.
                self.settle_soft_interrupt_turn(&mut in_flight, false).await;
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Record the turn's exit reason when its loop left one open.
    ///
    /// The loop records `Stop`/`Length` itself at its terminal path. Every other
    /// exit - an early break, a cancel, a propagated error - arrives here with
    /// no outcome yet, and this is the one place that names it.
    fn settle_turn_outcome(&mut self, failed: bool) {
        if self.turn_state().outcome().is_some() {
            return;
        }
        let outcome = if failed {
            TurnOutcome::Error
        } else if self.is_graceful_shutdown() {
            TurnOutcome::Aborted
        } else {
            TurnOutcome::Stop
        };
        if let Err(error) = self.turn_state.finish(outcome) {
            logging::warn(&format!("Turn outcome not recorded: {error}"));
        }
    }

    async fn run_turn_inner(
        &mut self,
        print_output: bool,
        mut in_flight: Option<&mut InFlightGuard>,
    ) -> Result<String> {
        self.ensure_session_lease()?;
        // A restored session may still owe its own commit. Land it first, so
        // provider or tool work never runs ahead of the state it depends on.
        self.finish_resume_persistence()?;
        self.set_log_context();
        self.begin_failover_chain_for_prompt();
        let usage_turn_id = self.model_usage_turn_id();
        crate::session_metrics::record_turn(&self.session.id);
        // Mark this session as actively streaming for presence UIs (e.g. the
        // macOS menu bar indicator). Cleared automatically on every exit path.
        let _streaming_guard = crate::session::StreamingGuard::new(self.session.id.clone());
        // A takeover mark belongs to the turn that was handed over. Clearing it
        // here keeps a finished handover from relabelling this turn's cancel.
        crate::tool::superseded::clear(&self.session.id);
        let trace = trace_enabled();
        // Tool-scoped stream rules are decided per finalized call, with repeat
        // policies read from the session ledger.
        let mut stream_rule_state = super::stream_rules::TurnStreamRules::from_config(
            &self.session.stream_rule_ledger,
            super::stream_rules::user_turn_index(&self.session),
        );

        loop {
            // Do not start another provider request once a cancel has been
            // observed; the loop is re-entered by several recovery paths
            // (issue #732, regression of #428).
            if self.is_graceful_shutdown() {
                logging::info("Cancel observed at turn-loop head - not starting another request");
                break;
            }
            if let Some(block) = self.session.migration_lease_block() {
                logging::info(&format!(
                    "Session migrated mid-turn - stopping local turn loop: {block}"
                ));
                break;
            }
            let repaired = self.repair_missing_tool_outputs();
            if repaired > 0 {
                logging::warn(&format!(
                    "Recovered {} missing tool output(s) before API call",
                    repaired
                ));
            }
            // Start provider transport setup before deriving and potentially
            // compacting the request history. This is the first point where the
            // stable request settings are available.
            let mut tools = self.tool_definitions().await;
            let mut split_prompt = self.build_system_prompt_split(None);
            self.provider
                .prewarm_with_context(&tools, &split_prompt.static_part, &self.provider_request_context())
                .await;
            let (messages, compaction_event) = self.messages_for_provider();
            if let Some(event) = compaction_event {
                // Reset cache tracker and tool lock on compaction since the message history changes
                self.cache_tracker.reset();
                self.kv_cache_monitor.reset();
                self.locked_tools = None;
                if print_output {
                    let tokens_str = event.pre_tokens.map(|t| format!(" ({} tokens)", t)).unwrap_or_default();
                    crate::terminal_println!("📦 Context compacted ({}){}", event.trigger, tokens_str);
                }
                // Compaction clears the tool lock, so rebuild the foreground
                // request metadata rather than relying on the pre-compaction snapshot.
                tools = self.tool_definitions().await;
                split_prompt = self.build_system_prompt_split(None);
            }

            // `messages` is this turn's single owned request snapshot. Memory and
            // the nudges below derive a small suffix from it instead of copying
            // the whole transcript again.
            // Non-blocking memory: uses pending result from last turn, spawns check for next turn
            let memory_pending = self.build_memory_prompt_nonblocking_shared(std::sync::Arc::clone(&messages), None);
            // Use split prompt for better caching - static content cached, dynamic not
            self.log_prompt_prefix_accounting(&split_prompt, &tools);

            // Check for client-side cache violations before memory injection.
            // Memory is an ephemeral suffix that changes each turn; tracking it would cause
            // false-positive violations every turn (prior turn's memory ≠ current history prefix).
            self.record_client_cache_request(&messages);

            // The request snapshot now owns everything the provider needs. Drop
            // the session's derived transcript copy before the network wait.
            self.session.release_provider_messages_cache();

            // Only the appended tail is owned; the transcript prefix stays borrowed.
            let mut request_suffix: Vec<Message> = Vec::new();
            // Inject memory as a user message at the end (preserves cache prefix)
            if let Some(memory) = memory_pending.as_ref() {
                let memory_count = memory.count.max(1);
                let age_ms = memory.computed_at.elapsed().as_millis() as u64;
                crate::memory::record_injected_prompt(&memory.prompt, memory_count, age_ms);
                self.record_memory_injection_in_session(memory);
                logging::info(&format!("Memory injected as message ({} chars)", memory.prompt.len()));
                let (memory_msg, _persisted) = self.prepare_memory_injection_message(memory);
                request_suffix.push(memory_msg);
            }
            if self
                .turn_state
                .take_batch_nudge(tools.iter().any(|tool| tool.name == "batch"))
            {
                request_suffix.push(Message::user(Self::BATCH_NUDGE));
            }
            logging::info(&format!(
                "API call starting: {} messages, {} tools",
                messages.len() + request_suffix.len(),
                tools.len()
            ));
            let api_start = Instant::now();

            // Publish status for TUI to show during Task execution
            Bus::global().publish(BusEvent::SubagentStatus(SubagentStatus {
                session_id: self.session.id.clone(),
                status: "calling API".to_string(),
                model: Some(self.provider.model()),
            }));

            // A suffix (memory, batch nudge, plan-limit notice) is the only reason
            // the request grows; an unmodified request stays a borrow of the
            // snapshot.
            let joined_request =
                (!request_suffix.is_empty()).then(|| join_request_messages(&messages, &request_suffix));
            if joined_request.is_some() {
                record_request_extra_clone(&messages);
            }
            let request_base = joined_request.as_deref().unwrap_or(&messages);
            let stamped = crate::config::config()
                .features
                .message_timestamps
                .then(|| Message::with_timestamps(request_base));
            if stamped.is_some() {
                record_request_extra_clone(request_base);
            }
            let send_messages = stamped.as_deref().unwrap_or(request_base);
            let model = self.provider_model();
            let request_context = crate::hooks::pre_request::RequestContext {
                session_id: &self.session.id,
                cwd: self.working_dir(),
                turn: super::stream_rules::user_turn_index(&self.session),
                model: &model,
                system: &split_prompt.static_part,
            };
            let outbound = super::request_messages::project(&request_context, send_messages).await?;
            let send_messages = outbound.as_deref().unwrap_or(send_messages);
            let prompt_has_recent_tool_result = Self::messages_end_with_tool_result(send_messages);
            self.last_status_detail = None;
            let completion = self
                .provider
                .complete_split_with_context(
                    send_messages,
                    &tools,
                    &split_prompt.static_part,
                    &split_prompt.dynamic_part,
                    self.provider_session_id.as_deref(),
                    &self.provider_request_context(),
                )
                .await;
            let mut stream = match completion {
                Ok(stream) => stream,
                Err(e) => {
                    self.record_failed_credential_route(&e.to_string());
                    let attached_client = crate::server::session_has_attached_client(&self.session.id);
                    match self.handle_provider_error(&e, attached_client).await {
                        ProviderErrorAction::Resend { .. } => continue,
                        ProviderErrorAction::Stop(error) => {
                            self.observe_stop_cause(cause_for_turn_failure(&error.to_string()));
                            return Err(error);
                        }
                        ProviderErrorAction::Proceed => {}
                    }
                    if self.try_auto_compact_after_context_limit(&e.to_string()) {
                        let retries = self.turn_state.record_context_limit_retry();
                        if retries > Self::MAX_CONTEXT_LIMIT_RETRIES {
                            logging::warn("Context-limit compaction retry limit reached; giving up");
                            self.observe_stop_cause(StopProvenance::Limit);
                            return Err(anyhow::anyhow!(
                                "Context limit exceeded after {} compaction retries",
                                Self::MAX_CONTEXT_LIMIT_RETRIES
                            ));
                        }
                        continue;
                    }
                    self.observe_stop_cause(cause_for_turn_failure(&e.to_string()));
                    return Err(e);
                }
            };

            // The provider returned an owned stream, so the request transcript
            // copies are no longer needed while the response is consumed.
            drop(stamped);
            drop(joined_request);
            drop(request_suffix);
            drop(memory_pending);
            drop(messages);
            drop(split_prompt);

            // Successful API call - reset retry counter
            self.turn_state.reset_context_limit_retries();

            logging::info(&format!(
                "API stream opened in {:.2}s",
                api_start.elapsed().as_secs_f64()
            ));
            log_agent_provider_stream_lifecycle(
                logging::LogLevel::Info,
                self,
                "stream_opened",
                api_start,
                vec![("mode", "blocking".to_string())],
            );

            Bus::global().publish(BusEvent::SubagentStatus(SubagentStatus {
                session_id: self.session.id.clone(),
                status: "streaming".to_string(),
                model: Some(self.provider.model()),
            }));

            let mut text_content = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut current_tool: Option<String> = None;
            let mut streaming_tools: HashMap<String, (ToolCall, String)> = HashMap::new();
            let mut generated_image_contexts: Vec<Vec<ContentBlock>> = Vec::new();
            let mut usage_input: Option<u64> = None;
            let mut usage_output: Option<u64> = None;
            let mut usage_cache_read: Option<u64> = None;
            let mut usage_cache_creation: Option<u64> = None;
            let mut saw_message_end = false;
            let mut stop_reason: Option<String> = None;
            let mut _thinking_start: Option<Instant> = None;
            let provider_name = self.provider.name().to_string();
            let store_reasoning_content = crate::provider::stores_reasoning_content_for_context(&provider_name);
            let mut reasoning_content = String::new();
            let mut reasoning_signature = String::new();
            let mut openai_reasoning_items: Vec<ContentBlock> = Vec::new();
            // Provider-executed tool items (e.g. native web search), stored
            // verbatim at their position in the response for exact replay.
            let mut provider_native_items = horon_agent_message_types::provider_native::ProviderNativeItems::default();
            // Track tool results from provider (already executed by Claude Code CLI)
            let mut sdk_tool_results: std::collections::HashMap<String, (String, bool)> =
                std::collections::HashMap::new();
            let mut openai_native_compaction: Option<(Vec<serde_json::Value>, usize)> = None;

            let mut retry_after_compaction = false;
            let mut retry_after_recovery = false;
            while let Some(event) = stream.next().await {
                let event = match event {
                    Ok(event) => event,
                    Err(e) => {
                        let err_str = e.to_string();
                        self.record_failed_credential_route(&err_str);
                        let attached_client = crate::server::session_has_attached_client(&self.session.id);
                        match self.handle_provider_error(&e, attached_client).await {
                            ProviderErrorAction::Resend { .. } => {
                                // A resend restarts the response, and the partial already
                                // printed to stdout cannot be retracted there. History is
                                // unaffected: the resend discards this attempt's text.
                                if print_output && !text_content.is_empty() {
                                    println!("\n[connection interrupted, retrying response from the top]");
                                    io::stdout().flush()?;
                                }
                                retry_after_recovery = true;
                                break;
                            }
                            ProviderErrorAction::Stop(error) => {
                                self.observe_stop_cause(cause_for_turn_failure(&error.to_string()));
                                return Err(error);
                            }
                            ProviderErrorAction::Proceed => {}
                        }
                        if self.try_auto_compact_after_context_limit(&err_str) {
                            log_agent_provider_stream_lifecycle(
                                logging::LogLevel::Warn,
                                self,
                                "stream_error_retry_after_compaction",
                                api_start,
                                vec![
                                    ("mode", "blocking".to_string()),
                                    ("error", err_str.clone()),
                                    (
                                        "context_limit_retries",
                                        (self.turn_state.context_limit_retries() + 1).to_string(),
                                    ),
                                ],
                            );
                            let retries = self.turn_state.record_context_limit_retry();
                            if retries > Self::MAX_CONTEXT_LIMIT_RETRIES {
                                logging::warn("Context-limit compaction retry limit reached; giving up");
                                self.observe_stop_cause(StopProvenance::Limit);
                                return Err(anyhow::anyhow!(
                                    "Context limit exceeded after {} compaction retries",
                                    Self::MAX_CONTEXT_LIMIT_RETRIES
                                ));
                            }
                            retry_after_compaction = true;
                            break;
                        }
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Error,
                            self,
                            "stream_error",
                            api_start,
                            vec![("mode", "blocking".to_string()), ("error", err_str)],
                        );
                        self.observe_stop_cause(cause_for_turn_failure(&e.to_string()));
                        return Err(e);
                    }
                };

                let input_tool_id = match &event {
                    StreamEvent::ToolInputDeltaFor { id, .. } | StreamEvent::ToolUseEndFor { id } => Some(id.clone()),
                    _ => current_tool.clone(),
                };
                match event {
                    StreamEvent::ThinkingStart => {
                        // Track start but don't print - wait for ThinkingDone
                        _thinking_start = Some(Instant::now());
                    }
                    StreamEvent::ThinkingDelta(thinking_text) => {
                        // Display reasoning content only if enabled
                        if print_output && crate::config::config().display.show_thinking {
                            crate::terminal_println!("💭 {}", thinking_text);
                        }
                        // Always capture reasoning text so it can be persisted as a
                        // history-only trace, regardless of provider replay support.
                        reasoning_content.push_str(&thinking_text);
                    }
                    StreamEvent::ThinkingSignatureDelta(signature) => {
                        if store_reasoning_content {
                            reasoning_signature.push_str(&signature);
                        }
                    }
                    StreamEvent::ThinkingEnd => {
                        // Don't print here - ThinkingDone has accurate timing
                        _thinking_start = None;
                    }
                    StreamEvent::ThinkingDone { duration_secs } => {
                        // Bridge provides accurate wall-clock timing
                        if print_output {
                            println!("Thought for {:.1}s\n", duration_secs);
                        }
                    }
                    StreamEvent::TextDelta(text) => {
                        if print_output {
                            crate::terminal_print!("{}", text);
                            io::stdout().flush()?;
                        }
                        text_content.push_str(&text);
                    }
                    StreamEvent::TextSuffixReplace { old_text, new_text } => {
                        if provider_native_items.replace_text_suffix(&mut text_content, &old_text, &new_text)
                            && print_output
                        {
                            // Plain stdout cannot retract already printed deltas.
                            // Still surface the real source links to CLI users.
                            crate::terminal_print!("\n{}", new_text);
                            io::stdout().flush()?;
                        }
                    }
                    StreamEvent::ToolUseStart { id, name } => {
                        if streaming_tools.contains_key(&id) || tool_calls.iter().any(|tool: &ToolCall| tool.id == id) {
                            continue;
                        }
                        if trace {
                            eprintln!("\n[trace] tool_use_start name={} id={}", name, id);
                        }
                        if print_output {
                            print!("\n[{}] ", name);
                            io::stdout().flush()?;
                        }
                        current_tool = Some(id.clone());
                        streaming_tools.entry(id.clone()).or_insert_with(|| {
                            (
                                ToolCall {
                                    id,
                                    name,
                                    input: serde_json::Value::Null,
                                    intent: None,
                                    thought_signature: None,
                                },
                                String::new(),
                            )
                        });
                    }
                    StreamEvent::ToolInputDelta(delta) | StreamEvent::ToolInputDeltaFor { delta, .. } => {
                        if let Some((_, input)) = input_tool_id.as_ref().and_then(|id| streaming_tools.get_mut(id)) {
                            input.push_str(&delta);
                        }
                    }
                    StreamEvent::ToolUseEnd | StreamEvent::ToolUseEndFor { .. } => {
                        if let Some((mut tool, current_tool_input)) =
                            input_tool_id.as_ref().and_then(|id| streaming_tools.remove(id))
                        {
                            // Parse the accumulated JSON
                            let tool_input = ToolCall::parse_streamed_input_to_object(&current_tool_input);
                            tool.input = tool_input.clone();
                            tool.intent = ToolCall::intent_from_input(&tool_input);

                            if trace {
                                if current_tool_input.trim().is_empty() {
                                    eprintln!("[trace] tool_input {} (empty)", tool.name);
                                } else if tool_input == serde_json::Value::Null {
                                    eprintln!("[trace] tool_input {} (raw) {}", tool.name, current_tool_input);
                                } else {
                                    let pretty = serde_json::to_string_pretty(&tool_input)
                                        .unwrap_or_else(|_| tool_input.to_string());
                                    eprintln!("[trace] tool_input {} {}", tool.name, pretty);
                                }
                            }

                            if print_output {
                                // Show brief tool info
                                print_tool_summary(&tool);
                            }

                            tool_calls.push(tool);
                            if current_tool == input_tool_id {
                                current_tool = None;
                            }
                        }
                    }
                    StreamEvent::ToolUseSignatureFor { id, signature } => {
                        if !signature.is_empty() {
                            if let Some((tool, _)) = streaming_tools.get_mut(&id) {
                                tool.thought_signature = Some(signature);
                            } else if let Some(tool) = tool_calls.iter_mut().find(|tool| tool.id == id) {
                                tool.thought_signature = Some(signature);
                            }
                        }
                    }
                    StreamEvent::ToolUseSignature(signature) => {
                        // Attach Gemini 3 thought signature to the most recent
                        // tool call so it can be persisted and replayed.
                        if let Some(tool) = tool_calls.last_mut()
                            && !signature.is_empty()
                        {
                            tool.thought_signature = Some(signature);
                        }
                    }
                    StreamEvent::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        // SDK already executed this tool, store the result
                        if trace {
                            eprintln!(
                                "[trace] sdk_tool_result id={} is_error={} content_len={}",
                                tool_use_id,
                                is_error,
                                content.len()
                            );
                        }
                        sdk_tool_results.insert(tool_use_id, (content, is_error));
                    }
                    StreamEvent::GeneratedImage {
                        id,
                        path,
                        metadata_path,
                        output_format,
                        revised_prompt,
                    } => {
                        if trace {
                            eprintln!(
                                "[trace] generated_image id={} format={} path={} metadata={}",
                                id,
                                output_format,
                                path,
                                metadata_path.as_deref().unwrap_or("none")
                            );
                        }
                        if print_output {
                            let summary = crate::message::generated_image_summary(
                                &path,
                                metadata_path.as_deref(),
                                &output_format,
                                revised_prompt.as_deref(),
                            );
                            eprintln!("\n[{}] {}", crate::message::GENERATED_IMAGE_TOOL_NAME, summary);
                        }
                        if self.provider.supports_image_input() {
                            if let Some(blocks) = crate::message::generated_image_visual_context_blocks(
                                &path,
                                metadata_path.as_deref(),
                                &output_format,
                                revised_prompt.as_deref(),
                            ) {
                                generated_image_contexts.push(blocks);
                            } else {
                                crate::logging::warn(&format!(
                                    "Generated image was not attached as visual context: {}",
                                    path
                                ));
                            }
                        }
                    }
                    StreamEvent::TokenUsage {
                        input_tokens,
                        output_tokens,
                        cache_read_input_tokens,
                        cache_creation_input_tokens,
                    } => {
                        if let Some(input) = input_tokens {
                            usage_input = Some(input);
                        }
                        if let Some(output) = output_tokens {
                            usage_output = Some(output);
                        }
                        if cache_read_input_tokens.is_some() {
                            usage_cache_read = cache_read_input_tokens;
                        }
                        if cache_creation_input_tokens.is_some() {
                            usage_cache_creation = cache_creation_input_tokens;
                        }
                        if let Some(input) = usage_input {
                            self.update_compaction_usage_from_stream(input, usage_cache_read, usage_cache_creation);
                        }
                        if trace {
                            eprintln!(
                                "[trace] token_usage input={} output={} cache_read={} cache_write={}",
                                usage_input.unwrap_or(0),
                                usage_output.unwrap_or(0),
                                usage_cache_read.unwrap_or(0),
                                usage_cache_creation.unwrap_or(0)
                            );
                        }
                    }
                    StreamEvent::ConnectionType { connection } => {
                        if trace {
                            eprintln!("[trace] connection_type={}", connection);
                        }
                        self.last_connection_type = Some(connection);
                    }
                    StreamEvent::ConnectionPhase { phase } => {
                        if trace {
                            eprintln!("[trace] connection_phase={}", phase);
                        }
                    }
                    StreamEvent::StatusDetail { detail } => {
                        if trace {
                            eprintln!("[trace] status_detail={}", detail);
                        }
                        self.last_status_detail = Some(detail);
                    }
                    StreamEvent::RetryRollback { attempt, max } => {
                        // Transient transport fault mid-stream; the provider is
                        // replaying the request. Discard this attempt's partial
                        // output so the replay doesn't duplicate it in history.
                        logging::warn(&format!(
                            "Mid-stream retry rollback (attempt {}/{}): discarding partial output ({} text chars, {} tool calls)",
                            attempt,
                            max,
                            text_content.len(),
                            tool_calls.len(),
                        ));
                        if print_output && !text_content.is_empty() {
                            // Already-printed text can't be unprinted on a plain
                            // stdout stream; mark the discontinuity instead.
                            println!("\n[connection interrupted, retrying response from the top]");
                            io::stdout().flush()?;
                        }
                        text_content.clear();
                        tool_calls.clear();
                        current_tool = None;
                        streaming_tools.clear();
                        sdk_tool_results.clear();
                        generated_image_contexts.clear();
                        reasoning_content.clear();
                        reasoning_signature.clear();
                        openai_reasoning_items.clear();
                        provider_native_items.clear();
                        openai_native_compaction = None;
                        saw_message_end = false;
                        stop_reason = None;
                    }
                    StreamEvent::ProviderNative { provider, item } => {
                        if print_output
                            && let Some(display) =
                                horon_agent_message_types::provider_native::provider_native_display(&provider, &item)
                        {
                            match display.output {
                                None => print!("\n[{}] ", display.name),
                                Some(output) => {
                                    println!("\n[{}] {}", display.name, crate::util::truncate_str(&output, 200))
                                }
                            }
                            io::stdout().flush()?;
                        }
                        provider_native_items.push(text_content.len(), provider, item);
                    }
                    StreamEvent::TextDone => {}
                    StreamEvent::MessageEnd { stop_reason: reason } => {
                        saw_message_end = true;
                        if reason.is_some() {
                            stop_reason = reason;
                        }
                        // Don't break yet - wait for SessionId which comes after MessageEnd
                        // (but stream close will also end the loop for providers without SessionId)
                    }
                    StreamEvent::SessionId(sid) => {
                        if trace {
                            eprintln!("[trace] session_id {}", sid);
                        }
                        self.provider_session_id = Some(sid.clone());
                        self.session.provider_session_id = Some(sid);
                        // We've received session_id, can exit the loop now
                        if saw_message_end {
                            break;
                        }
                    }
                    StreamEvent::UpstreamProvider { provider } => {
                        // Log upstream provider for local trace output
                        if trace {
                            eprintln!("[trace] upstream_provider={}", provider);
                        }
                        self.last_upstream_provider = Some(provider);
                    }
                    StreamEvent::OpenAIReasoning {
                        id,
                        summary,
                        encrypted_content,
                        status,
                    } => {
                        if store_reasoning_content {
                            openai_reasoning_items.push(ContentBlock::OpenAIReasoning {
                                id,
                                summary,
                                encrypted_content,
                                status,
                            });
                        }
                    }
                    StreamEvent::Compaction {
                        trigger,
                        pre_tokens,
                        openai_encrypted_content,
                    } => {
                        if let Some(encrypted_content) = openai_encrypted_content {
                            // The auto path emits one compaction item per
                            // response; it becomes the first item of a window.
                            openai_native_compaction.get_or_insert((
                                vec![serde_json::json!({
                                    "type": "compaction",
                                    "encrypted_content": encrypted_content,
                                })],
                                self.session.active_context().len(),
                            ));
                        }
                        if print_output {
                            let tokens_str = pre_tokens.map(|t| format!(" ({} tokens)", t)).unwrap_or_default();
                            crate::terminal_println!("📦 Context compacted ({}){}", trigger, tokens_str);
                        }
                    }
                    StreamEvent::NativeToolCall {
                        request_id,
                        tool_name,
                        input,
                    } => {
                        // Execute native tool and send result back to SDK bridge
                        if trace {
                            eprintln!("[trace] native_tool_call request_id={} tool={}", request_id, tool_name);
                        }
                        let ctx = ToolContext {
                            session_id: self.session.id.clone(),
                            message_id: self.session.id.clone(),
                            tool_call_id: request_id.clone(),
                            working_dir: self.working_dir().map(PathBuf::from),
                            stdin_request_tx: self.stdin_request_tx.clone(),
                            graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
                            execution_mode: ToolExecutionMode::AgentTurn,
                        };
                        let tool_result = self
                            .registry
                            .execute(&tool_name, ToolCall::normalize_input_to_object(input), ctx)
                            .await;
                        let native_result = match tool_result {
                            Ok(output) => NativeToolResult::success(request_id, output.output),
                            Err(e) => NativeToolResult::error(request_id, e.to_string()),
                        };
                        // Send result back to SDK bridge
                        if let Some(sender) = self.provider.native_result_sender() {
                            let _ = sender.send(native_result).await;
                        }
                    }
                    StreamEvent::Error {
                        message,
                        retry_after_secs,
                    } => {
                        if trace {
                            eprintln!("[trace] stream_error {}", message);
                        }
                        let stream_error: anyhow::Error = StreamError::new(message.clone(), retry_after_secs).into();
                        let attached_client = crate::server::session_has_attached_client(&self.session.id);
                        match self.handle_provider_error(&stream_error, attached_client).await {
                            ProviderErrorAction::Resend { .. } => {
                                if print_output && !text_content.is_empty() {
                                    println!("\n[connection interrupted, retrying response from the top]");
                                    io::stdout().flush()?;
                                }
                                retry_after_recovery = true;
                                break;
                            }
                            ProviderErrorAction::Stop(error) => {
                                self.observe_stop_cause(cause_for_turn_failure(&error.to_string()));
                                return Err(error);
                            }
                            ProviderErrorAction::Proceed => {}
                        }
                        if self.try_auto_compact_after_context_limit(&message) {
                            log_agent_provider_stream_lifecycle(
                                logging::LogLevel::Warn,
                                self,
                                "stream_event_retry_after_compaction",
                                api_start,
                                vec![
                                    ("mode", "blocking".to_string()),
                                    ("error", message.clone()),
                                    (
                                        "context_limit_retries",
                                        (self.turn_state.context_limit_retries() + 1).to_string(),
                                    ),
                                ],
                            );
                            let retries = self.turn_state.record_context_limit_retry();
                            if retries > Self::MAX_CONTEXT_LIMIT_RETRIES {
                                logging::warn("Context-limit compaction retry limit reached; giving up");
                                return Err(anyhow::anyhow!(
                                    "Context limit exceeded after {} compaction retries",
                                    Self::MAX_CONTEXT_LIMIT_RETRIES
                                ));
                            }
                            retry_after_compaction = true;
                            break;
                        }
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Error,
                            self,
                            "stream_event_error",
                            api_start,
                            vec![
                                ("mode", "blocking".to_string()),
                                ("error", message.clone()),
                                (
                                    "retry_after_secs",
                                    retry_after_secs
                                        .map(|seconds| seconds.to_string())
                                        .unwrap_or_else(|| "none".to_string()),
                                ),
                            ],
                        );
                        self.observe_stop_cause(cause_for_turn_failure(&message));
                        return Err(StreamError::new(message, retry_after_secs).into());
                    }
                }
            }

            if retry_after_recovery {
                logging::info("Retrying the turn after a provider recovery - resending the request");
                continue;
            }

            if retry_after_compaction {
                log_agent_provider_stream_lifecycle(
                    logging::LogLevel::Info,
                    self,
                    "retry_after_compaction",
                    api_start,
                    vec![("mode", "blocking".to_string())],
                );
                continue;
            }

            let api_elapsed = api_start.elapsed();
            logging::info(&format!(
                "API call complete in {:.2}s (input={} output={} cache_read={} cache_write={})",
                api_elapsed.as_secs_f64(),
                usage_input.unwrap_or(0),
                usage_output.unwrap_or(0),
                usage_cache_read.unwrap_or(0),
                usage_cache_creation.unwrap_or(0),
            ));
            log_agent_provider_stream_lifecycle(
                logging::LogLevel::Info,
                self,
                "stream_complete",
                api_start,
                vec![
                    ("mode", "blocking".to_string()),
                    ("saw_message_end", saw_message_end.to_string()),
                    ("input_tokens", usage_input.unwrap_or(0).to_string()),
                    ("output_tokens", usage_output.unwrap_or(0).to_string()),
                    ("cache_read", usage_cache_read.unwrap_or(0).to_string()),
                    ("cache_write", usage_cache_creation.unwrap_or(0).to_string()),
                ],
            );

            if print_output
                && (usage_input.is_some()
                    || usage_output.is_some()
                    || usage_cache_read.is_some()
                    || usage_cache_creation.is_some())
            {
                let input = usage_input.unwrap_or(0);
                let output = usage_output.unwrap_or(0);
                let cache_read = usage_cache_read.unwrap_or(0);
                let cache_creation = usage_cache_creation.unwrap_or(0);
                let cache_str = if usage_cache_read.is_some() || usage_cache_creation.is_some() {
                    format!(" cache_read: {} cache_write: {}", cache_read, cache_creation)
                } else {
                    String::new()
                };
                print!("\n[Tokens] upload: {} download: {}{}\n", input, output, cache_str);
                io::stdout().flush()?;
            }

            // Store usage for debug queries
            self.last_usage = TokenUsage {
                input_tokens: usage_input.unwrap_or(0),
                output_tokens: usage_output.unwrap_or(0),
                cache_read_input_tokens: usage_cache_read,
                cache_creation_input_tokens: usage_cache_creation,
            };

            let text_tool_recovery = self.recover_text_wrapped_tool_call(&mut text_content, &mut tool_calls);

            let visible_text_is_empty = text_content.trim().is_empty();

            // Add assistant message to history. Avoid persisting whitespace-only text as a
            // successful visible answer: some OpenRouter/Kimi tool continuations can finish
            // cleanly with only spaces despite non-zero output tokens. Persisting that makes the
            // UI look like the agent stopped after tools with no explanation.
            let mut content_blocks = Vec::new();
            if !provider_native_items.is_empty() {
                content_blocks.extend(provider_native_items.interleave(&text_content));
            } else if !text_content.is_empty() && !visible_text_is_empty {
                content_blocks.push(ContentBlock::Text {
                    text: text_content.clone(),
                    cache_control: None,
                });
            }
            crate::message::push_reasoning_blocks(
                &mut content_blocks,
                &provider_name,
                &reasoning_content,
                Some(&reasoning_signature),
                store_reasoning_content,
            );
            if store_reasoning_content {
                content_blocks.extend(openai_reasoning_items.iter().cloned());
            }
            for tc in &tool_calls {
                content_blocks.push(tc.to_tool_use_block());
            }

            let assistant_message_id = if !content_blocks.is_empty() {
                let token_usage = Some(crate::session::StoredTokenUsage {
                    prompt_tokens: Some(self.effective_context_tokens_from_usage(
                        self.last_usage.input_tokens,
                        self.last_usage.cache_read_input_tokens,
                        self.last_usage.cache_creation_input_tokens,
                    )),
                    input_tokens: self.last_usage.input_tokens,
                    output_tokens: self.last_usage.output_tokens,
                    cache_read_input_tokens: self.last_usage.cache_read_input_tokens,
                    cache_creation_input_tokens: self.last_usage.cache_creation_input_tokens,
                });
                let message_id = self.add_message_ext(Role::Assistant, content_blocks, None, token_usage);
                self.push_embedding_snapshot_if_semantic(&text_content);
                self.save_final_answer()?;
                self.record_model_turn_usage(&usage_turn_id);
                Some(message_id)
            } else {
                None
            };

            if let Some((window, compacted_count)) = openai_native_compaction.take() {
                // A compaction item that arrived mid-stream belongs to a
                // response the provider never finished. Applying it would move
                // the compacted boundary over that response, so the originals
                // stay active until a completed response produces a window.
                if saw_message_end {
                    self.apply_openai_native_compaction_window(window, compacted_count)?;
                } else {
                    logging::warn(
                        "[OI] native compaction item arrived on an unfinished stream; keeping the compacted boundary unchanged",
                    );
                }
            }

            // If stop_reason indicates truncation (e.g. max_tokens), discard tool calls
            // with null/empty inputs since they were likely truncated mid-generation.
            // This prevents executing broken tool calls and instead requests a continuation.
            self.filter_truncated_tool_calls(stop_reason.as_deref(), &mut tool_calls, assistant_message_id.as_ref());

            if tool_calls.is_empty() && !generated_image_contexts.is_empty() {
                for blocks in generated_image_contexts.drain(..) {
                    self.add_message(Role::User, blocks);
                }
                self.save_session()?;
                logging::info("Continuing turn so model can inspect generated image visual context");
                continue;
            }

            // If no tool calls, we're done
            if tool_calls.is_empty() {
                // Unparseable tool-call markup in the text (issue #1702):
                // surface it and ask the model to re-issue the call natively.
                if let Some(notice) =
                    self.maybe_nudge_unparsed_tool_markup(&text_tool_recovery, stop_reason.as_deref())?
                {
                    if print_output {
                        println!("\n{}", notice);
                    }
                    continue;
                }
                if self.maybe_reconsider_fable_guardrail(stop_reason.as_deref())? {
                    continue;
                }
                if self.maybe_continue_empty_post_tool_response(
                    visible_text_is_empty,
                    prompt_has_recent_tool_result,
                    stop_reason.as_deref(),
                )? {
                    continue;
                }
                if saw_message_end
                    && let Some(outcome) =
                        self.maybe_auto_continue_unexpected_stop(stop_reason.as_deref(), &text_content)?
                {
                    match outcome {
                        AutoContinuation::Continued => continue,
                        AutoContinuation::Exhausted(notice) => {
                            if print_output {
                                println!("\n{}", notice);
                            }
                            let final_text = if text_content.trim().is_empty() {
                                notice
                            } else {
                                format!("{}\n\n{}", text_content, notice)
                            };
                            self.turn_state.set_final_text(&final_text);
                            self.turn_state
                                .finish(outcome_for_stop_reason(stop_reason.as_deref()))?;
                            break;
                        }
                    }
                }
                if self.maybe_continue_incomplete_response(stop_reason.as_deref())? {
                    continue;
                }
                if self.maybe_continue_goal()? {
                    continue;
                }
                // Surface silent guardrail/refusal stops instead of returning
                // an empty final answer with no explanation.
                if let Some(notice) = Self::provider_guardrail_notice(
                    stop_reason.as_deref(),
                    visible_text_is_empty,
                    !reasoning_content.trim().is_empty(),
                ) {
                    logging::warn(&format!(
                        "{}: turn ended with no visible output (stop_reason={:?})",
                        Self::empty_turn_log_event(stop_reason.as_deref()),
                        stop_reason
                    ));
                    if print_output {
                        println!("\n[provider guardrail] {}", notice);
                    }
                    if text_content.trim().is_empty() {
                        text_content = format!("[provider guardrail] {}", notice);
                    }
                }
                logging::info("Turn complete - no tool calls, returning");
                if print_output {
                    println!();
                }
                self.turn_state.set_final_text(&text_content);
                let outcome = if self.is_graceful_shutdown() {
                    TurnOutcome::Aborted
                } else {
                    outcome_for_stop_reason(stop_reason.as_deref())
                };
                self.turn_state.finish(outcome)?;
                break;
            }

            logging::info(&format!("Turn has {} tool calls to execute", tool_calls.len()));

            // If provider handles tools internally (like Claude Code CLI), only run native tools locally
            if self.provider.handles_tools_internally() {
                tool_calls.retain(|tc| HORON_AGENT_NATIVE_TOOLS.contains(&tc.name.as_str()));
                if tool_calls.is_empty() {
                    if !generated_image_contexts.is_empty() {
                        for blocks in generated_image_contexts.drain(..) {
                            self.add_message(Role::User, blocks);
                        }
                        self.save_session()?;
                        logging::info("Continuing turn so model can inspect generated image visual context");
                        continue;
                    }
                    logging::info("Provider handles tools internally - task complete");
                    break;
                }
                logging::info("Provider handles tools internally - executing native tools locally");
            }

            let used_batch = tool_calls.iter().any(|tc| tc.name == "batch");
            self.turn_state.record_tool_round(
                tool_calls.len(),
                used_batch,
                Self::SEQUENTIAL_TOOL_ROUNDS_BEFORE_BATCH_NUDGE,
            );

            // Execute tools and add results.
            //
            // The round is planned once through the shared scheduler
            // (`agent::tool_round`) so grouping, the group caps, and the
            // ordering rules live in one place and cannot drift from the
            // streaming path. Results are applied strictly in request order
            // whatever order the members finish in: providers pair each
            // `tool_use` with its `tool_result` by id, so a reordered round
            // corrupts the transcript.
            //
            // This loop has NO mid-round interrupt. The only soft-interrupt
            // point on this path is `inject_soft_interrupts()` after the whole
            // round below, so `run_group` is called without a cancel signal.
            // Do not read that as a lost interrupt.
            let parallel_tools = crate::config::config().features.parallel_tools;
            let groups = self.plan_tool_round_groups(&tool_calls, parallel_tools).await;
            // Seeded from the transcript as it stands before this round's
            // results land, which is the same view the tool layer's own guard
            // has. `session.messages` are stored messages, so they are read
            // through the provider transcript the compaction estimate counts.
            let round_messages = self.session.messages_for_provider_ref();
            // One budget for the whole round, seeded once from THIS agent's
            // compaction manager (the groups execute with a clone that shares
            // it) and never re-seeded between groups: each group's outputs are
            // counted against what the earlier groups already applied.
            let mut round_budget =
                super::round_budget::RoundTokenBudget::seed(&self.registry.compaction(), round_messages).await;

            let mut malformed_calls: Vec<MalformedToolCallInfo> = Vec::new();
            let mut executed_valid_call = false;
            let mut tool_results_dirty = false;
            // Set when the loop guard's second refusal on one signature ends
            // the turn; holds the one-line notice for the transcript.
            let mut loop_guard_stop: Option<String> = None;
            // Set when a tool handed the turn to another owner. That turn is
            // already dead, so nothing after the handing-over call is recorded.
            let mut superseded_stop = false;

            for group in groups {
                let message_id = assistant_message_id.clone().unwrap_or_else(|| self.session.id.clone());

                // Resolve every member of the group in request order before
                // dispatching it. A malformed call and an SDK-provided result
                // never reach a tool, but each still owes one tool result and
                // one terminal event at its own index, so they are decided here
                // and applied below in the same index order.
                let mut members: Vec<RoundMember> = Vec::with_capacity(group.len());
                let mut validation_error: Option<anyhow::Error> = None;
                for index in group.range() {
                    let tc = &tool_calls[index];

                    if let Some(error_msg) = tc.validation_error() {
                        members.push(RoundMember::Malformed { error: error_msg });
                        continue;
                    }

                    // A call that passed validation is genuinely valid even if
                    // the tool itself later fails: it resets the malformed-round
                    // bound in handle_malformed_tool_round below.
                    executed_valid_call = true;
                    if let Err(error) = self.validate_tool_allowed(&tc.name) {
                        // A sequential round stopped here too. The members
                        // resolved before this one still run and land first.
                        validation_error = Some(error);
                        break;
                    }

                    // Check if SDK already executed this tool
                    if let Some((sdk_content, sdk_is_error)) = sdk_tool_results.remove(&tc.id) {
                        let is_native_tool = HORON_AGENT_NATIVE_TOOLS.contains(&tc.name.as_str());
                        // For native tools, ignore SDK errors and execute locally
                        if !(is_native_tool && sdk_is_error) {
                            if trace {
                                eprintln!(
                                    "[trace] using_sdk_result name={} id={} is_error={}",
                                    tc.name, tc.id, sdk_is_error
                                );
                            }
                            members.push(RoundMember::Sdk {
                                content: sdk_content,
                                is_error: sdk_is_error,
                            });
                            continue;
                        }
                        if trace {
                            eprintln!(
                                "[trace] sdk_error_for_native_tool name={} id={}, executing locally",
                                tc.name, tc.id
                            );
                        }
                    }

                    members.push(RoundMember::Execute);
                }

                let dispatch: Vec<(usize, ToolCall)> = members
                    .iter()
                    .enumerate()
                    .filter(|(_, member)| matches!(member, RoundMember::Execute))
                    .map(|(offset, _)| {
                        let index = group.start + offset;
                        (index, tool_calls[index].clone())
                    })
                    .collect();

                // One loop-guard decision for the whole group, taken before
                // any member runs: every member is recorded here, so a verdict
                // is never produced while a call is in flight. A refused member
                // is dropped from the dispatch set and still owes its own
                // result; a stop ends the turn once this round's results are
                // written.
                let verdicts = self.consult_loop_guard(dispatch.iter().map(|(_, tc)| tc));
                let mut executable: Vec<(usize, ToolCall)> = Vec::with_capacity(dispatch.len());
                for ((index, tc), verdict) in dispatch.into_iter().zip(verdicts) {
                    // Once this round has stopped, nothing after it runs: the
                    // turn is over and the call only owes a valid transcript.
                    if loop_guard_stop.is_some() {
                        members[index - group.start] = RoundMember::Blocked {
                            message: Self::LOOP_GUARD_SKIP.to_string(),
                        };
                        continue;
                    }
                    match verdict {
                        super::loop_guard::LoopGuardVerdict::Allow | super::loop_guard::LoopGuardVerdict::Warn(_) => {
                            executable.push((index, tc))
                        }
                        super::loop_guard::LoopGuardVerdict::Refuse(message) => {
                            members[index - group.start] = RoundMember::Blocked { message };
                        }
                        super::loop_guard::LoopGuardVerdict::Stop(message) => {
                            members[index - group.start] = RoundMember::Blocked {
                                message: message.clone(),
                            };
                            loop_guard_stop = Some(message);
                        }
                    }
                }
                let dispatch = executable;

                // A tool-scoped stream rule decides each finalized call before
                // it runs. A blocked call owes a refusal result at its own
                // index but is not a malformed call; a passive hit runs and
                // carries the reminder in its result.
                let mut rule_passive: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
                let mut rule_blocked: std::collections::HashSet<usize> = std::collections::HashSet::new();
                for (index, tc) in &dispatch {
                    match stream_rule_state.check_tool(&tc.name, &tc.input) {
                        Some(super::stream_rules::ToolRuleAction::Block(hit)) => {
                            let message = super::stream_rules::blocked_message(&hit);
                            logging::warn(&format!("Stream rule blocked tool '{}': {message}", tc.name));
                            members[*index - group.start] = RoundMember::RuleBlocked { message };
                            rule_blocked.insert(*index);
                        }
                        Some(super::stream_rules::ToolRuleAction::Passive(hit)) => {
                            rule_passive.insert(*index, super::stream_rules::tool_passive_reminder(&hit));
                        }
                        None => {}
                    }
                }
                stream_rule_state.flush_ledger(&mut self.session.stream_rule_ledger);
                let dispatch: Vec<(usize, ToolCall)> = dispatch
                    .into_iter()
                    .filter(|(index, _)| !rule_blocked.contains(index))
                    .collect();

                // Announce every member that will run before dispatching the
                // group, in request order, so a client renders all rows as
                // started at once instead of one row per completion.
                for (_, tc) in &dispatch {
                    Bus::global().publish(BusEvent::ToolUpdated(ToolEvent {
                        session_id: self.session.id.clone(),
                        message_id: message_id.clone(),
                        tool_call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        status: ToolStatus::Running,
                        intent: tc.intent.clone(),
                        title: None,
                    }));
                    logging::info(&format!("Tool starting: {}", tc.name));
                    // Publish status for TUI to show during Task execution
                    Bus::global().publish(BusEvent::SubagentStatus(SubagentStatus {
                        session_id: self.session.id.clone(),
                        status: format!("running {}", tc.name),
                        model: Some(self.provider.model()),
                    }));
                }

                let mut results: HashMap<usize, Result<crate::tool::ToolOutput>> = HashMap::new();
                let mut tool_elapsed = Duration::ZERO;
                if !dispatch.is_empty() {
                    let ctx = ToolContext {
                        session_id: self.session.id.clone(),
                        message_id: message_id.clone(),
                        tool_call_id: String::new(),
                        working_dir: self.working_dir().map(PathBuf::from),
                        stdin_request_tx: self.stdin_request_tx.clone(),
                        graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
                        execution_mode: ToolExecutionMode::AgentTurn,
                    };
                    let calls: Vec<ToolCall> = dispatch.iter().map(|(_, tc)| tc.clone()).collect();
                    let tool_start = Instant::now();
                    // The scheduler runs the group's members concurrently.
                    // Every member of a one-member group (and every group when
                    // `parallel_tools` is off) therefore behaves exactly like
                    // the old inline call. The clone shares this session's
                    // compaction manager, so the context-overflow guard inside
                    // `Registry::execute` reads the session's budget and token
                    // count instead of a fresh manager's overhead floor.
                    let outcome =
                        super::tool_round::run_group(self.registry.clone_sharing_compaction(), &calls, &ctx, None)
                            .await;
                    tool_elapsed = tool_start.elapsed();
                    for (position, result) in outcome.results {
                        results.insert(dispatch[position].0, result);
                    }
                    // No cancel signal is passed here, so this is unreachable
                    // today; keeping it means a future cancellation cannot
                    // leave a `tool_use` without a `tool_result`.
                    for position in outcome.unstarted {
                        results.insert(dispatch[position].0, Err(anyhow::anyhow!("Tool call was not executed")));
                    }
                }

                // Apply results in request order. Terminal events, stored
                // durations and history messages all follow this order even
                // when the members finished in a different one.
                for (offset, member) in members.iter().enumerate() {
                    let index = group.start + offset;
                    let tc = &tool_calls[index];
                    match member {
                        RoundMember::Malformed { error } => {
                            malformed_calls.push(MalformedToolCallInfo {
                                name: tc.name.clone(),
                                error: error.clone(),
                            });
                            logging::warn(error);
                            Bus::global().publish(BusEvent::ToolUpdated(ToolEvent {
                                session_id: self.session.id.clone(),
                                message_id: message_id.clone(),
                                tool_call_id: tc.id.clone(),
                                tool_name: tc.name.clone(),
                                status: ToolStatus::Error,
                                intent: tc.intent.clone(),
                                title: None,
                            }));
                            if print_output {
                                println!("\n  → {}", error);
                            }
                            self.add_message(
                                Role::User,
                                vec![ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content: error.clone(),
                                    is_error: Some(true),
                                }],
                            );
                            tool_results_dirty = true;
                        }
                        RoundMember::Sdk { content, is_error } => {
                            // The provider answered this call, so no tool layer
                            // capped or admitted it. It is still an output this
                            // round applies, so it takes the round's own
                            // decision at its own place in request order.
                            let content = round_budget.admit_sdk_result(
                                &self.session.id,
                                &tc.name,
                                &tc.input,
                                content.clone(),
                                *is_error,
                            );
                            if print_output {
                                print!("\n  → ");
                                let preview = if content.len() > 200 {
                                    format!("{}...", crate::util::truncate_str(&content, 200))
                                } else {
                                    content.clone()
                                };
                                println!("{}", preview.lines().next().unwrap_or("(done via SDK)"));
                            }
                            Bus::global().publish(BusEvent::ToolUpdated(ToolEvent {
                                session_id: self.session.id.clone(),
                                message_id: message_id.clone(),
                                tool_call_id: tc.id.clone(),
                                tool_name: tc.name.clone(),
                                status: if *is_error {
                                    ToolStatus::Error
                                } else {
                                    ToolStatus::Completed
                                },
                                intent: tc.intent.clone(),
                                title: None,
                            }));
                            self.add_message(
                                Role::User,
                                vec![ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content,
                                    is_error: if *is_error { Some(true) } else { None },
                                }],
                            );
                            tool_results_dirty = true;
                        }
                        RoundMember::RuleBlocked { message } => {
                            // A tool-scoped stream rule refused this call. It
                            // owes one terminal event and one tool result like
                            // any refused call, but it is not a malformed call:
                            // the round's malformed bound must not move.
                            Bus::global().publish(BusEvent::ToolUpdated(ToolEvent {
                                session_id: self.session.id.clone(),
                                message_id: message_id.clone(),
                                tool_call_id: tc.id.clone(),
                                tool_name: tc.name.clone(),
                                status: ToolStatus::Error,
                                intent: tc.intent.clone(),
                                title: None,
                            }));
                            if print_output {
                                println!("\n  → {}", message);
                            }
                            self.add_message(
                                Role::User,
                                vec![ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content: message.clone(),
                                    is_error: Some(true),
                                }],
                            );
                            tool_results_dirty = true;
                        }
                        RoundMember::Blocked { message } => {
                            // The guard refused this call, so it never reached a
                            // tool. It still owes one terminal event and one
                            // tool result, both at its own index.
                            Bus::global().publish(BusEvent::ToolUpdated(ToolEvent {
                                session_id: self.session.id.clone(),
                                message_id: message_id.clone(),
                                tool_call_id: tc.id.clone(),
                                tool_name: tc.name.clone(),
                                status: ToolStatus::Error,
                                intent: tc.intent.clone(),
                                title: None,
                            }));
                            if print_output {
                                println!("\n  → {}", message);
                            }
                            self.add_message(
                                Role::User,
                                vec![ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content: message.clone(),
                                    is_error: Some(true),
                                }],
                            );
                            tool_results_dirty = true;
                        }
                        RoundMember::Execute => {
                            let result = results.remove(&index).expect("every dispatched member has a result");
                            self.unlock_tools_if_needed(&tc.name, &tc.input);
                            logging::info(&format!(
                                "Tool finished: {} in {:.2}s",
                                tc.name,
                                tool_elapsed.as_secs_f64()
                            ));

                            match result {
                                Ok(output) => {
                                    let output = cap_tool_output_for_history(&tc.name, &self.session.id, output);
                                    // The tool layer checked this output against
                                    // the context size from BEFORE the round; the
                                    // round's own running total is what decides
                                    // once a group has already spent the window.
                                    let mut output = round_budget.admit(&tc.name, output);
                                    if let Some(reminder) = rule_passive.get(&index) {
                                        output.output.push_str(reminder);
                                    }
                                    Bus::global().publish(BusEvent::ToolUpdated(ToolEvent {
                                        session_id: self.session.id.clone(),
                                        message_id: message_id.clone(),
                                        tool_call_id: tc.id.clone(),
                                        tool_name: tc.name.clone(),
                                        status: ToolStatus::Completed,
                                        intent: tc.intent.clone(),
                                        title: output.title.clone(),
                                    }));

                                    if trace {
                                        eprintln!(
                                            "[trace] tool_exec_done name={} id={}\n{}",
                                            tc.name, tc.id, output.output
                                        );
                                    }
                                    if print_output {
                                        print!("\n  → ");
                                        let preview = if output.output.len() > 200 {
                                            format!("{}...", crate::util::truncate_str(&output.output, 200))
                                        } else {
                                            output.output.clone()
                                        };
                                        println!("{}", preview.lines().next().unwrap_or("(done)"));
                                    }

                                    let blocks = tool_output_to_history_blocks(tc.id.clone(), output, &self.session.id);
                                    self.add_message_with_duration(
                                        Role::User,
                                        blocks,
                                        Some(tool_elapsed.as_millis() as u64),
                                    );
                                    tool_results_dirty = true;
                                }
                                Err(e) => {
                                    // A takeover is not a tool failure: the tool
                                    // already fired this turn's cancel signal.
                                    // Appending its error would let the model
                                    // answer it while the resumed turn works, and
                                    // the siblings in this message would be
                                    // recorded too, so the round stops before
                                    // either happens.
                                    if e.downcast_ref::<crate::tool::SupersededToolCall>().is_some() {
                                        crate::tool::superseded::clear(&self.session.id);
                                        logging::info(&format!(
                                            "Tool '{}' handed the turn to another owner; stopping without recording a result",
                                            tc.name
                                        ));
                                        superseded_stop = true;
                                        break;
                                    }
                                    Bus::global().publish(BusEvent::ToolUpdated(ToolEvent {
                                        session_id: self.session.id.clone(),
                                        message_id: message_id.clone(),
                                        tool_call_id: tc.id.clone(),
                                        tool_name: tc.name.clone(),
                                        status: ToolStatus::Error,
                                        intent: tc.intent.clone(),
                                        title: None,
                                    }));

                                    let error_msg = format!("Error: {}", e);
                                    if trace {
                                        eprintln!(
                                            "[trace] tool_exec_error name={} id={} {}",
                                            tc.name, tc.id, error_msg
                                        );
                                    }
                                    if print_output {
                                        print!("\n  → ");
                                        println!("{}", error_msg);
                                    }
                                    self.add_message_with_duration(
                                        Role::User,
                                        vec![ContentBlock::ToolResult {
                                            tool_use_id: tc.id.clone(),
                                            content: error_msg,
                                            is_error: Some(true),
                                        }],
                                        Some(tool_elapsed.as_millis() as u64),
                                    );
                                    tool_results_dirty = true;
                                }
                            }
                        }
                    }
                }

                // A takeover outranks a later member's validation error: the
                // sequential loop stopped at the handing-over call and never
                // evaluated the calls after it.
                if superseded_stop {
                    break;
                }

                if let Some(error) = validation_error {
                    return Err(error);
                }

                if loop_guard_stop.is_some() {
                    // The turn is over. Every remaining call of the round still
                    // owes one result, or the next request would be formed from
                    // an invalid transcript.
                    for skipped_tc in &tool_calls[group.end..] {
                        Bus::global().publish(BusEvent::ToolUpdated(ToolEvent {
                            session_id: self.session.id.clone(),
                            message_id: message_id.clone(),
                            tool_call_id: skipped_tc.id.clone(),
                            tool_name: skipped_tc.name.clone(),
                            status: ToolStatus::Error,
                            intent: skipped_tc.intent.clone(),
                            title: None,
                        }));
                        self.add_message(
                            Role::User,
                            vec![ContentBlock::ToolResult {
                                tool_use_id: skipped_tc.id.clone(),
                                content: Self::LOOP_GUARD_SKIP.to_string(),
                                is_error: Some(true),
                            }],
                        );
                    }
                    tool_results_dirty = true;
                    break;
                }
            }

            if let Some(notice) = loop_guard_stop.take() {
                self.record_loop_guard_stop(&notice);
                self.save_session()?;
                if print_output {
                    println!("\n{}", notice);
                }
                break;
            }

            if tool_results_dirty {
                self.save_session()?;
            }

            if !generated_image_contexts.is_empty() {
                for blocks in generated_image_contexts.drain(..) {
                    self.add_message(Role::User, blocks);
                }
                self.save_session()?;
            }

            // Bounded recovery for repeated malformed tool calls (e.g. a
            // model emitting null arguments): correct with the expected
            // schema, then end the turn with an actionable error instead of
            // retrying the same malformed round forever.
            self.handle_malformed_tool_round(&malformed_calls, executed_valid_call, &tools)?;

            // Feed the opted-in advisor and deliver what it already filed. This
            // runs before soft-interrupt injection so a blocker it queues here
            // is injected at this boundary, not one round later.
            self.advisor_after_round().await;

            if print_output {
                println!();
            }

            // Check for soft interrupts (e.g. client replies) and inject them for the next turn
            let injected = self
                .inject_soft_interrupts(super::interrupts::reborrow_in_flight(&mut in_flight))
                .await;
            if !injected.is_empty() {
                let total_chars: usize = injected.iter().map(|item| item.content.len()).sum();
                logging::info(&format!(
                    "Soft interrupt injected into headless turn ({} message(s), {} chars)",
                    injected.len(),
                    total_chars
                ));
            }
        }

        Ok(self.turn_state.final_text().to_string())
    }
}

/// How one call of a tool round is resolved: executed locally, answered by the
/// SDK, or rejected before it reaches a tool. The variant decides where the
/// call's single tool result and terminal event come from, never the order:
/// members are always applied by request index.
enum RoundMember {
    Execute,
    Sdk {
        content: String,
        is_error: bool,
    },
    Malformed {
        error: String,
    },
    /// The loop guard refused this call, so it never reached a tool. The
    /// message is the refusal itself, returned as the call's error result.
    Blocked {
        message: String,
    },
    /// A tool-scoped stream rule refused this call. It owes the same error
    /// result as `Blocked`, but it is not a loop-guard decision and must not
    /// count as a malformed call.
    RuleBlocked {
        message: String,
    },
}

#[cfg(test)]
#[path = "turn_loops_round_tests.rs"]
mod round_tests;
#[cfg(test)]
#[path = "turn_loops_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "run_stop_provenance_tests.rs"]
mod run_stop_provenance_tests;

#[cfg(test)]
#[path = "turn_loops_failover_tests.rs"]
mod failover_tests;
