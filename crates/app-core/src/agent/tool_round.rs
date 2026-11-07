//! One shared scheduler for a round of tool calls.
//!
//! Both turn loops call this module instead of each inventing its own
//! sequencing, so grouping, the group caps, result ordering, and cancellation
//! behave the same on the blocking path and on the streaming daemon path.
//!
//! The module is deliberately free of `Agent` state. It emits no client
//! events, writes no session history, and never swallows an error: it reports
//! each member's outcome and leaves the transcript to the caller. That is what
//! lets both loops share it while only they own events and history.
//!
//! # Grouping
//!
//! A round is split in request order by what each call declares it touches. A
//! call joins the open group only while its effects are compatible with the
//! group's accumulated ones; a call that declares a barrier effect (`write`,
//! `append`, `process`) therefore closes the open group and becomes a
//! one-member group of its own, so it never overlaps anything. Two caps apply
//! per group and they are independent: at most [`MAX_GROUP_MEMBERS`] calls in
//! total, and at most [`MAX_NETWORK_MEMBERS`] calls whose effects include
//! [`ToolEffects::networks`]. A mixed group therefore stops at four network
//! calls even when local reads would still fit, because every network call
//! spends a paid API request.
//!
//! # Cancellation
//!
//! [`run_group`] observes a shutdown/interrupt signal, and an urgent-interrupt
//! watch when the caller supplies one. When either fires, the members still in
//! flight are aborted and the outcome carries the partial results already
//! obtained plus the indexes that produced none, so the caller can write a skip
//! result for every one of them and keep the transcript valid. Completed work
//! is never discarded.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::stream::{FuturesUnordered, StreamExt};

use super::InterruptSignal;
use crate::message::ToolCall;
use crate::tool::{Registry, ToolContext, ToolEffects, ToolOutput};

/// Result text of a member the user cancelled through `cancel_tool_call`.
///
/// The text travels as the member's error so every caller that already writes
/// a result for a member that failed writes the same thing for a cancelled
/// one. It is not a failure report: it says the work was stopped on purpose.
pub(crate) const CANCELLED_RESULT: &str = crate::agent::turn_streaming_mpsc::CANCELLED_TOOL_RESULT;

/// Most calls one concurrent group may hold.
pub(crate) const MAX_GROUP_MEMBERS: usize = 10;

/// Most network calls one concurrent group may hold, whatever else it holds.
///
/// A call counts here when its declared effects include
/// [`ToolEffects::networks`], so a tool whose reach depends on its input is
/// counted only for the calls that actually spend a request.
pub(crate) const MAX_NETWORK_MEMBERS: usize = 4;

/// How often a running group re-reads the caller's urgent-interrupt watch.
///
/// The soft-interrupt queue carries no notification, so a bounded poll is the
/// only way to notice an interrupt while members are in flight. This bounds the
/// extra work a group does after the user's message arrives (one timer wake per
/// interval per running group) instead of making the group wait for members
/// that would otherwise all run to completion.
const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Everything that can cut a group short.
///
/// [`GroupCancel::new`] carries the caller's shutdown/reload signal, which is
/// the whole story on the blocking path. The streaming daemon also has to honor
/// an urgent interrupt: it arrives in a queue the turn cannot be woken by, so
/// the caller adds a predicate and the group polls it while members are in
/// flight. Without that, a group of paid network calls runs to completion
/// before the interrupt is noticed at the next group boundary.
pub(crate) struct GroupCancel {
    signal: InterruptSignal,
    interrupt: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl GroupCancel {
    pub(crate) fn new(signal: InterruptSignal) -> Self {
        Self {
            signal,
            interrupt: None,
        }
    }

    /// Also stop the group when `interrupt` reports an urgent interrupt.
    pub(crate) fn watching_interrupt(mut self, interrupt: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.interrupt = Some(interrupt);
        self
    }

    fn interrupt_pending(&self) -> bool {
        self.interrupt.as_ref().is_some_and(|interrupt| interrupt())
    }
}

/// What cut a group short, for a caller that reacts differently to each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupStop {
    /// The caller's shutdown/reload signal fired.
    Shutdown,
    /// The caller's interrupt watch reported an urgent interrupt.
    Interrupt,
}

/// One group of a round, as an index range into the round's `ToolCall` slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolGroup {
    /// Index of the group's first call in the round.
    pub start: usize,
    /// One past the group's last call in the round.
    pub end: usize,
}

impl ToolGroup {
    pub(crate) fn len(&self) -> usize {
        self.end - self.start
    }

    pub(crate) fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }
}

/// Split a round into groups, in request order.
///
/// `effects` is the per-call declared effect set, index-aligned with the
/// round's calls, so its length is the round's length. A call joins the open
/// group while the group's accumulated effects stay parallel-safe beside it
/// ([`ToolEffects::compatible_with`]) and neither cap is exceeded; otherwise
/// the group is closed first and a new one starts at this call.
///
/// This function is pure: no registry, no async, no state. The split and its
/// caps are decided from the declared effects alone, which is what makes the
/// grouping testable without a running agent.
pub(crate) fn plan_groups(effects: &[ToolEffects]) -> Vec<ToolGroup> {
    let mut groups: Vec<ToolGroup> = Vec::new();
    let mut open_start = 0usize;
    let mut open_len = 0usize;
    let mut open_network = 0usize;
    let mut active = ToolEffects::read();

    for (index, candidate) in effects.iter().copied().enumerate() {
        let network = usize::from(candidate.networks());
        let fits = open_len > 0
            && active.compatible_with(candidate)
            && open_len < MAX_GROUP_MEMBERS
            && open_network + network <= MAX_NETWORK_MEMBERS;

        if !fits {
            if open_len > 0 {
                groups.push(ToolGroup {
                    start: open_start,
                    end: index,
                });
            }
            open_start = index;
            open_len = 0;
            open_network = 0;
            active = candidate;
        } else {
            active = active.union(candidate);
        }
        open_len += 1;
        open_network += network;
    }

    if open_len > 0 {
        groups.push(ToolGroup {
            start: open_start,
            end: effects.len(),
        });
    }

    groups
}

/// Effects of one call as `Registry::execute` would resolve it.
///
/// A session-scoped SDK custom tool is a callback, not a registry tool, so it is
/// a barrier even when its name matches a built-in alias:
/// `Registry::effects_for_call` would answer for the built-in in that case,
/// while `execute` would run the callback.
pub(crate) async fn classify_call(registry: &Registry, session_id: &str, call: &ToolCall) -> ToolEffects {
    let resolved = Registry::resolve_tool_name_for_session(session_id, &call.name);
    if crate::tool::sdk::custom(session_id, resolved) {
        return ToolEffects::write();
    }
    registry.effects_for_call(&call.name, &call.input).await
}

/// Classify a whole round and split it into groups.
pub(crate) async fn plan_round(registry: &Registry, session_id: &str, calls: &[ToolCall]) -> Vec<ToolGroup> {
    let mut effects = Vec::with_capacity(calls.len());
    for call in calls {
        effects.push(classify_call(registry, session_id, call).await);
    }
    plan_groups(&effects)
}

/// Outcome of one group.
#[derive(Debug, Default)]
pub(crate) struct GroupOutcome {
    /// Members that produced a result, sorted by their index in the group
    /// slice. Errors are per member and are never swallowed.
    pub results: Vec<(usize, Result<ToolOutput>)>,
    /// Members the user cancelled through `cancel_tool_call`. Their work was
    /// aborted and they produced no result; the caller writes the cancellation
    /// result it also reports as `tool_cancelled`.
    pub cancelled: Vec<usize>,
    /// Members that produced no result: the group was already cancelled before
    /// they were dispatched, or they were aborted in flight. The caller writes
    /// a skip result for each of them.
    pub unstarted: Vec<usize>,
    /// Set when a cancellation cut the group short. `None` means every member
    /// produced a result.
    pub stopped: Option<GroupStop>,
}

/// Per-member slot before the outcome is assembled.
enum MemberSlot {
    Finished(Result<ToolOutput>),
    Cancelled,
    Aborted,
}

/// Run one group's calls concurrently and return their results in request
/// order.
///
/// Each member runs in its own `tokio::spawn`, so a panicking tool becomes that
/// call's error instead of losing the group. The member's `ToolContext` keeps
/// the call's own `tc.id`: [`ToolContext::for_subcall`] rewrites the id into a
/// parent-scoped label, which would break the missing-output repair path
/// (`tool::inflight`) and Alt+B registration, both of which key on the
/// provider's `tool_use` id.
///
/// When `cancel` fires, members still in flight are aborted and drained; every
/// index without a result is reported in [`GroupOutcome::unstarted`], and
/// [`GroupOutcome::stopped`] names what fired.
///
/// An urgent interrupt that was already queued when the group started keeps the
/// per-call reaction the sequential loop had: the first member still runs, and
/// every member after it is reported unstarted. Dispatching the whole group
/// there would spend the paid requests the interrupt was meant to save.
pub(crate) async fn run_group(
    registry: Registry,
    calls: &[ToolCall],
    base_ctx: &ToolContext,
    cancel: Option<GroupCancel>,
) -> GroupOutcome {
    if cancel.as_ref().is_some_and(|cancel| cancel.signal.is_set()) {
        return GroupOutcome {
            results: Vec::new(),
            cancelled: Vec::new(),
            unstarted: (0..calls.len()).collect(),
            stopped: Some(GroupStop::Shutdown),
        };
    }

    let interrupt_pending_at_dispatch = cancel.as_ref().is_some_and(GroupCancel::interrupt_pending);
    let dispatch_len = if interrupt_pending_at_dispatch {
        calls.len().min(1)
    } else {
        calls.len()
    };

    let registry = Arc::new(registry);
    let mut in_flight = FuturesUnordered::new();
    let mut aborts = Vec::with_capacity(dispatch_len);

    for (index, call) in calls[..dispatch_len].iter().enumerate() {
        let ctx = member_context(base_ctx, call);
        let registry = Arc::clone(&registry);
        let name = call.name.clone();
        let input = call.input.clone();
        let handle = tokio::spawn(async move { registry.execute(&name, input, ctx).await });
        let abort = handle.abort_handle();
        aborts.push(abort.clone());
        // Each member is cancellable by its own call id while it runs, which
        // is what lets a `cancel_tool_call` name one member of a concurrent
        // group. The guard lives exactly as long as the member's future.
        let cancel = InterruptSignal::new();
        let cancel_guard = crate::tool_cancel_registry::register(&base_ctx.session_id, &call.id, cancel.clone());
        in_flight.push(async move {
            let _cancel_guard = cancel_guard;
            let slot = tokio::select! {
                biased;
                result = handle => match result {
                    Ok(result) => MemberSlot::Finished(result),
                    Err(error) if error.is_cancelled() => MemberSlot::Aborted,
                    // Mirrors the streaming loop's handling: a panicking tool is
                    // that call's error, not a lost group.
                    Err(error) => MemberSlot::Finished(Err(anyhow::anyhow!("Tool task panicked: {error}"))),
                },
                // The user cancelled this member only: abort its work and keep
                // the rest of the group running.
                _ = cancel.notified() => {
                    abort.abort();
                    MemberSlot::Cancelled
                }
            };
            (index, slot)
        });
    }

    let mut results: Vec<(usize, Result<ToolOutput>)> = Vec::with_capacity(calls.len());
    let mut stop: Option<GroupStop> = None;

    // The interrupt watch is polled rather than awaited: the queue it reads
    // carries no notification. A caller without a watch pays nothing for it.
    // An interrupt that was already queued at dispatch is not polled for: it
    // already decided that only the first member runs, so aborting that member
    // would drop work the sequential loop always completed.
    let interrupt = cancel
        .as_ref()
        .filter(|_| !interrupt_pending_at_dispatch)
        .and_then(|cancel| cancel.interrupt.clone());
    let mut interrupt_poll = interrupt.as_ref().map(|_| {
        let mut poll = tokio::time::interval(INTERRUPT_POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        poll
    });
    let watches_signal = cancel.is_some();
    let watches_interrupt = interrupt_poll.is_some();
    let mut cancelled: Vec<usize> = Vec::new();

    while !in_flight.is_empty() {
        tokio::select! {
            biased;
            Some((index, slot)) = in_flight.next() => {
                match slot {
                    MemberSlot::Finished(result) => results.push((index, result)),
                    MemberSlot::Cancelled => cancelled.push(index),
                    MemberSlot::Aborted => {}
                }
            }
            _ = wait_for_cancel(cancel.as_ref().map(|cancel| &cancel.signal)), if watches_signal && stop.is_none() => {
                stop = Some(GroupStop::Shutdown);
            }
            _ = poll_for_interrupt(interrupt_poll.as_mut(), interrupt.as_deref()), if watches_interrupt && stop.is_none() => {
                stop = Some(GroupStop::Interrupt);
            }
        }

        if stop.is_some() {
            for abort in &aborts {
                abort.abort();
            }
            // Drain what is left so a member that finished around the cancel is
            // recorded rather than dropped. Aborted members resolve at their
            // next await point and land in `unstarted` instead.
            while let Some((index, slot)) = in_flight.next().await {
                match slot {
                    MemberSlot::Finished(result) => results.push((index, result)),
                    MemberSlot::Cancelled => cancelled.push(index),
                    MemberSlot::Aborted => {}
                }
            }
            break;
        }
    }

    cancelled.sort_unstable();

    // A cancelled member also owes one result, so it is recorded as an error
    // carrying the cancellation text: every caller writes that result and the
    // transcript stays valid, exactly as for a member that failed.
    for index in &cancelled {
        results.push((*index, Err(anyhow::anyhow!(CANCELLED_RESULT))));
    }
    results.sort_by_key(|(index, _)| *index);

    let mut produced = vec![false; calls.len()];
    for (index, _) in &results {
        produced[*index] = true;
    }
    let unstarted = produced
        .iter()
        .enumerate()
        .filter_map(|(index, done)| (!done).then_some(index))
        .collect();

    GroupOutcome {
        results,
        cancelled,
        unstarted,
        stopped: stop.or(interrupt_pending_at_dispatch.then_some(GroupStop::Interrupt)),
    }
}

/// Context for one group member, carrying that call's own id.
fn member_context(base: &ToolContext, call: &ToolCall) -> ToolContext {
    ToolContext {
        session_id: base.session_id.clone(),
        message_id: base.message_id.clone(),
        tool_call_id: call.id.clone(),
        working_dir: base.working_dir.clone(),
        stdin_request_tx: base.stdin_request_tx.clone(),
        graceful_shutdown_signal: base.graceful_shutdown_signal.clone(),
        execution_mode: base.execution_mode,
    }
}

/// Wait for the cancel signal, or forever when the caller supplied none.
async fn wait_for_cancel(cancel: Option<&InterruptSignal>) {
    match cancel {
        Some(signal) => signal.notified().await,
        None => std::future::pending::<()>().await,
    }
}

/// Wait until the caller's interrupt watch reports an urgent interrupt.
///
/// The watch is a predicate over a queue nothing notifies, so this re-reads it
/// every [`INTERRUPT_POLL_INTERVAL`]. With no watch configured it pends
/// forever, which leaves the select to the completion and shutdown branches.
async fn poll_for_interrupt(
    poll: Option<&mut tokio::time::Interval>,
    interrupt: Option<&(dyn Fn() -> bool + Send + Sync)>,
) {
    match (poll, interrupt) {
        (Some(poll), Some(interrupt)) => loop {
            poll.tick().await;
            horon_agent_perf_counters::record_interrupt_poll_wakeup();
            if interrupt() {
                return;
            }
        },
        _ => std::future::pending::<()>().await,
    }
}
