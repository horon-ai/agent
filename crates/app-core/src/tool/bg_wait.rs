//! `bg action=wait` with the extra event sources: child agents and IRC messages.
//!
//! The default wait (background tasks only) stays in [`super`] untouched, so
//! a call without the new flags behaves exactly as before. This module owns the
//! path where `children`, `messages`, or both are selected: one loop that
//! returns the first event across background tasks, child agents, and queued
//! peer messages, bounded by a single `max_wait_seconds` deadline.
//!
//! Three details make the loop safe:
//!
//! * Child terminals arrive as [`BusEvent::ChildAgentTerminal`], published by
//!   the result cache, so the wait never polls the cache.
//! * A queued soft interrupt arrives as [`BusEvent::SessionInputQueued`]. That is
//!   what ends the wait with reason `interrupted`, and it is only reachable on
//!   this path, so the default wait keeps today's behaviour.
//! * The consumed ledger below is process-global, so an event is delivered once:
//!   a second wait does not return the same child.

use super::{ToolContext, ToolOutput};
use crate::agent_result_cache;
use crate::background;
use crate::bus::{BackgroundTaskStatus, Bus, BusEvent, QueuedInputKind};
use anyhow::Result;
use horon_agent_irc_core::{WaitFilter, WaitOutcome, format_envelope, global as irc_global};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

/// How often the loop re-reads background task status.
///
/// Children and messages are event-driven; only tasks still need a status read,
/// and this matches the interval the default wait already polls at.
const TASK_POLL: Duration = Duration::from_millis(1000);

/// The two optional event sources of a wait.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WaitSources {
    /// Child agents of the calling session.
    pub(crate) children: bool,
    /// Peer messages queued for the calling session over IRC.
    pub(crate) messages: bool,
}

impl WaitSources {
    pub(crate) fn any(&self) -> bool {
        self.children || self.messages
    }

    fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::with_capacity(2);
        if self.children {
            names.push("children");
        }
        if self.messages {
            names.push("messages");
        }
        names
    }
}

/// One bounded wait over the selected sources.
pub(crate) struct WaitRequest {
    pub(crate) sources: WaitSources,
    /// Background task ids to watch, already resolved by the caller. Empty when
    /// the caller selected only the new sources.
    pub(crate) task_ids: Vec<String>,
    pub(crate) max_wait: Duration,
    /// `any`, `all`, or `first_failure`; `any` is the default.
    pub(crate) mode: String,
    pub(crate) return_on_progress: bool,
}

/// Child ids already handed to a caller, keyed by parent session.
///
/// Process-global on purpose: the result cache is process-global too, so a
/// second wait in the same daemon must not return a child the first one
/// reported. IRC needs no ledger because the mailbox is drained on read.
fn consumed() -> &'static Mutex<HashMap<String, HashSet<String>>> {
    static CONSUMED: LazyLock<Mutex<HashMap<String, HashSet<String>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
    &CONSUMED
}

fn consumed_guard() -> MutexGuard<'static, HashMap<String, HashSet<String>>> {
    consumed().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn already_delivered(parent: &str, child: &str) -> bool {
    consumed_guard()
        .get(parent)
        .is_some_and(|children| children.contains(child))
}

fn mark_delivered(parent: &str, child: &str) {
    consumed_guard()
        .entry(parent.to_string())
        .or_default()
        .insert(child.to_string());
}

/// What a child's cache status means for the wait.
///
/// The cache writes exactly `running`, `streaming`, and `thinking` for a live
/// child (`server/swarm.rs`); every other label is terminal. An unrecognised
/// label is treated as terminal so an unknown state can never park the wait
/// until its deadline.
fn child_is_running(status: &str) -> bool {
    matches!(status, "running" | "streaming" | "thinking")
}

/// The first terminal child that has not been reported to this parent yet.
///
/// The tick uses this so a terminal published while the listener was not yet
/// subscribed is still reported in mode `any` instead of sleeping to the
/// deadline.
fn first_finished_child(parent: &str) -> Option<(String, String)> {
    agent_result_cache::children_snapshot(parent)
        .into_iter()
        .find(|child| !child_is_running(&child.status) && !already_delivered(parent, &child.session_id))
        .map(|child| (child.session_id, child.status))
}

/// The still-pending and already-finished work of one instant.
#[derive(Default)]
struct Pending {
    running_tasks: Vec<String>,
    running_children: Vec<String>,
    finished_children: Vec<(String, String)>,
    unread_messages: usize,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.running_tasks.is_empty()
            && self.running_children.is_empty()
            && self.finished_children.is_empty()
            && self.unread_messages == 0
    }
}

/// `nothing to wait for` is the outcome `irc wait` already uses, so a caller
/// learns the same thing from either wait when no source holds anything.
pub(crate) const NOTHING_TO_WAIT_FOR: &str =
    "nothing to wait for: no background task, child agent, or queued message is pending";

const INTERRUPTED: &str = "interrupted: new input is queued for this session";

async fn collect(manager: &background::BackgroundTaskManager, parent: &str, request: &WaitRequest) -> Pending {
    let mut pending = Pending::default();
    for task_id in &request.task_ids {
        match manager.status(task_id).await {
            Some(task) if task.status == BackgroundTaskStatus::Running => pending.running_tasks.push(task_id.clone()),
            Some(_) => {}
            // A vanished task is not something to wait for; the caller sees it
            // in the timeout summary as absent rather than forever running.
            None => {}
        }
    }
    if request.sources.children {
        let snapshot = agent_result_cache::children_snapshot(parent);
        // Keep the ledger bounded by the cache: a child the cache has dropped
        // can never be reported again, so its id need not be remembered.
        let live: HashSet<&str> = snapshot.iter().map(|child| child.session_id.as_str()).collect();
        {
            let mut ledger = consumed_guard();
            if let Some(delivered) = ledger.get_mut(parent) {
                delivered.retain(|child| live.contains(child.as_str()));
                if delivered.is_empty() {
                    ledger.remove(parent);
                }
            }
        }
        for child in snapshot {
            if already_delivered(parent, &child.session_id) {
                continue;
            }
            if child_is_running(&child.status) {
                pending.running_children.push(child.session_id);
            } else {
                pending.finished_children.push((child.session_id, child.status));
            }
        }
    }
    if request.sources.messages
        && let Some(bus) = irc_global::get()
    {
        pending.unread_messages = bus.unread_count(parent);
    }
    pending
}

/// Whether the message source can block at all: the switch must be on and a bus
/// installed, exactly as `irc wait` requires.
fn messages_armed(request: &WaitRequest) -> bool {
    request.sources.messages && crate::config::config().tools.irc && irc_global::get().is_some()
}

fn child_output(child_id: &str, status: &str, request: &WaitRequest) -> ToolOutput {
    let text = format!("Child agent {child_id} finished: {status}\n\nRead its result with `agent://{child_id}`.");
    ToolOutput::new(text).with_title("bg wait child").with_metadata(json!({
        "wait_reason": "child_finished",
        "timed_out": false,
        "child_session_id": child_id,
        "child_status": status,
        "sources": request.sources.names(),
    }))
}

fn message_output(envelope: &str, request: &WaitRequest) -> ToolOutput {
    ToolOutput::new(envelope.to_string())
        .with_title("bg wait message")
        .with_metadata(json!({
            "wait_reason": "message",
            "timed_out": false,
            "sources": request.sources.names(),
        }))
}

fn interrupted_output(request: &WaitRequest) -> ToolOutput {
    ToolOutput::new(INTERRUPTED.to_string())
        .with_title("bg wait interrupted")
        .with_metadata(json!({
            "wait_reason": "interrupted",
            "timed_out": false,
            "sources": request.sources.names(),
        }))
}

fn nothing_output(request: &WaitRequest) -> ToolOutput {
    ToolOutput::new(NOTHING_TO_WAIT_FOR.to_string())
        .with_title("bg wait")
        .with_metadata(json!({
            "wait_reason": "nothing_to_wait_for",
            "timed_out": false,
            "sources": request.sources.names(),
        }))
}

fn all_finished_output(tasks: &[background::TaskStatusFile], finished_children: &[(String, String)]) -> ToolOutput {
    let mut text = String::from("All selected work finished.\n\n");
    for task in tasks {
        text.push_str(&task_line(task));
    }
    for (child_id, status) in finished_children {
        text.push_str(&format!("- child {child_id} · {status}\n"));
    }
    ToolOutput::new(text).with_title("bg wait all").with_metadata(json!({
        "wait_reason": "all_finished",
        "timed_out": false,
    }))
}

fn task_line(task: &background::TaskStatusFile) -> String {
    let label = crate::message::background_task_display_label(&task.tool_name, task.display_name.as_deref());
    format!(
        "- task {} · {} · {}\n",
        task.task_id,
        label,
        task_status_label(&task.status)
    )
}

/// The same four labels `bg action=list` prints, so a wait line reads like a
/// list row. `bg.rs` keeps its copy private; this one exists only because the
/// wait module is a sibling rather than a child.
fn task_status_label(status: &BackgroundTaskStatus) -> &'static str {
    match status {
        BackgroundTaskStatus::Running => "running",
        BackgroundTaskStatus::Completed => "completed",
        BackgroundTaskStatus::Superseded => "superseded",
        BackgroundTaskStatus::Failed => "failed",
    }
}

/// The timeout summary names every id that is still pending, which is what makes
/// a capped wait useful rather than a bare "nothing happened".
fn timeout_output(pending: &Pending, request: &WaitRequest) -> ToolOutput {
    let seconds = request.max_wait.as_secs();
    let mut text = format!("No event before max wait of {seconds}s.\n\n");
    if pending.running_tasks.is_empty() && pending.running_children.is_empty() {
        text.push_str("Nothing is still pending.\n");
    } else {
        text.push_str("Still pending:\n");
        for task_id in &pending.running_tasks {
            text.push_str(&format!("- task {task_id} · running\n"));
        }
        for child_id in &pending.running_children {
            text.push_str(&format!("- child {child_id} · running\n"));
        }
    }
    ToolOutput::new(text)
        .with_title("bg wait timeout")
        .with_metadata(json!({
            "wait_reason": "timeout",
            "timed_out": true,
            "max_wait_seconds": seconds,
            "pending_task_ids": pending.running_tasks,
            "pending_child_session_ids": pending.running_children,
            "sources": request.sources.names(),
        }))
}

/// Block until the first selected event, the deadline, or an interrupt.
pub(crate) async fn wait_with_sources(
    manager: &background::BackgroundTaskManager,
    ctx: &ToolContext,
    request: WaitRequest,
) -> Result<ToolOutput> {
    let me = ctx.session_id.clone();
    let armed_messages = messages_armed(&request);
    // Subscribe before the preflight read: a terminal that lands between the
    // read and the loop would otherwise be missed in mode `any` and the call
    // would sleep to its deadline.
    let mut receiver = Bus::global().subscribe();

    // Preflight: anything already waiting is returned without sleeping, and a
    // selection with nothing in it at all reports `nothing_to_wait_for` instead
    // of parking until the deadline.
    let pending = collect(manager, &me, &request).await;
    if request.mode != "all"
        && let Some((child_id, status)) = pending.finished_children.first()
    {
        mark_delivered(&me, child_id);
        return Ok(child_output(child_id, status, &request));
    }
    if armed_messages
        && let Some(bus) = irc_global::get()
        && let Some(message) = bus.take(&me, None)
    {
        return Ok(message_output(&format_envelope(&message, None), &request));
    }
    // A selection with nothing to wait for answers `nothing_to_wait_for` in
    // every mode, before the `all` branch can report an empty "all finished".
    let tasks = tasks_of(manager, &request.task_ids).await;
    if pending.is_empty() && !armed_messages && tasks.is_empty() {
        return Ok(nothing_output(&request));
    }
    if request.mode == "all" {
        if pending.running_tasks.is_empty() && pending.running_children.is_empty() {
            for (child_id, _) in &pending.finished_children {
                mark_delivered(&me, child_id);
            }
            return Ok(all_finished_output(&tasks, &pending.finished_children));
        }
    } else if tasks.iter().any(|task| task.status != BackgroundTaskStatus::Running) {
        // A task named explicitly and already finished is an event, not
        // "nothing to wait for": the preflight reports it the way the tick does.
        let failed = tasks.iter().any(|task| task.status == BackgroundTaskStatus::Failed);
        let reason = if request.mode == "first_failure" && failed {
            "first_failure"
        } else {
            "any_finished"
        };
        return Ok(task_event_output(reason, &tasks));
    }

    let deadline = tokio::time::Instant::now() + request.max_wait;
    let mut tick = tokio::time::interval(TASK_POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_progress: HashMap<String, Option<crate::bus::BackgroundTaskProgress>> = HashMap::new();
    for task in &tasks {
        last_progress.insert(task.task_id.clone(), task.progress.clone());
    }
    // `IrcBus::wait` returns immediately when nobody owes this session a reply,
    // so it is polled at most once; later messages arrive as a bus event or on
    // the tick's mailbox read.
    let mut message_wait_live = armed_messages;
    let shutdown = ctx.graceful_shutdown_signal.clone();
    let shutdown_signal = async move {
        match shutdown {
            Some(signal) => signal.notified().await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(shutdown_signal);
    let deadline_signal = tokio::time::sleep_until(deadline);
    tokio::pin!(deadline_signal);

    loop {
        // A dropped IRC wait unregisters its parked waiter, so recreating this
        // future on each iteration cannot strand or lose a message.
        let message_wait = async {
            match irc_global::get() {
                Some(bus) => {
                    bus.wait(
                        &me,
                        WaitFilter {
                            from: None,
                            direct_only: false,
                        },
                        request.max_wait,
                        std::future::pending::<()>(),
                    )
                    .await
                }
                None => WaitOutcome::NothingToWaitFor,
            }
        };
        tokio::pin!(message_wait);

        tokio::select! {
            biased;
            event = receiver.recv() => {
                match event {
                    Ok(BusEvent::ChildAgentTerminal { parent_session_id, session_id, status })
                        if parent_session_id == me && !already_delivered(&me, &session_id) =>
                    {
                        // In mode `all` the event only wakes the completion
                        // check. Consuming the child here would lose it when the
                        // deadline arrives first, since the timeout summary names
                        // running ids only.
                        if request.mode == "all" {
                            continue;
                        }
                        mark_delivered(&me, &session_id);
                        return Ok(child_output(&session_id, &status, &request));
                    }
                    Ok(BusEvent::SessionInputQueued { session_id, kind }) if session_id == me => {
                        // Only reachable when a new source is selected, which is
                        // what keeps the default wait's behaviour unchanged.
                        return Ok(match kind {
                            QueuedInputKind::Message => match irc_global::get().and_then(|bus| bus.take(&me, None)) {
                                Some(message) => message_output(&format_envelope(&message, None), &request),
                                None => ToolOutput::new(INTERRUPTED.to_string())
                                    .with_title("bg wait message")
                                    .with_metadata(json!({
                                        "wait_reason": "message_queued",
                                        "timed_out": false,
                                        "sources": request.sources.names(),
                                    })),
                            },
                            QueuedInputKind::Interrupt => interrupted_output(&request),
                        });
                    }
                    Ok(_) => {}
                    // A lagged receiver still lets the tick re-read task, child,
                    // and mailbox state instead of failing the wait.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Ok(nothing_output(&request));
                    }
                }
            }
            _ = &mut shutdown_signal => return Ok(interrupted_output(&request)),
            outcome = message_wait, if message_wait_live => {
                match outcome {
                    WaitOutcome::Message(message) => {
                        return Ok(message_output(&format_envelope(&message, None), &request));
                    }
                    WaitOutcome::PendingInput => return Ok(interrupted_output(&request)),
                    WaitOutcome::Unanswerable(targets) => {
                        let ids: Vec<&str> = targets.iter().map(|id| id.as_ref()).collect();
                        return Ok(ToolOutput::new(format!(
                            "unanswerable: {} are waiting on you; send them what they need",
                            ids.join(", ")
                        ))
                        .with_title("bg wait message")
                        .with_metadata(json!({"wait_reason": "unanswerable", "timed_out": false})));
                    }
                    // Nobody owes a reply, so the parked wait cannot help; stop
                    // polling it and let the event arm and the tick pick up a
                    // message that arrives later.
                    WaitOutcome::NothingToWaitFor | WaitOutcome::Timeout => {
                        message_wait_live = false;
                    }
                    WaitOutcome::Cancelled => return Ok(interrupted_output(&request)),
                }
            }
            _ = &mut deadline_signal => {
                let pending = collect(manager, &me, &request).await;
                return Ok(timeout_output(&pending, &request));
            }
            _ = tick.tick() => {
                let tasks = tasks_of(manager, &request.task_ids).await;
                if armed_messages
                    && let Some(bus) = irc_global::get()
                    && let Some(message) = bus.take(&me, None)
                {
                    return Ok(message_output(&format_envelope(&message, None), &request));
                }
                if request.sources.children
                    && request.mode != "all"
                    && let Some((child_id, status)) = first_finished_child(&me)
                {
                    mark_delivered(&me, &child_id);
                    return Ok(child_output(&child_id, &status, &request));
                }
                if request.return_on_progress && request.mode != "all" {
                    for task in &tasks {
                        if task.status != BackgroundTaskStatus::Running {
                            continue;
                        }
                        let previous = last_progress.get(&task.task_id).cloned().unwrap_or(None);
                        if task.progress != previous {
                            let mut text = String::from("Background task emitted a progress event.\n\n");
                            for task in &tasks {
                                text.push_str(&task_line(task));
                            }
                            return Ok(ToolOutput::new(text)
                                .with_title("bg wait progress")
                                .with_metadata(json!({
                                    "wait_reason": "progress",
                                    "timed_out": false,
                                    "tasks": tasks.iter().map(|task| task.task_id.clone()).collect::<Vec<_>>(),
                                })));
                        }
                    }
                }
                for task in &tasks {
                    last_progress.insert(task.task_id.clone(), task.progress.clone());
                }
                let failed = tasks
                    .iter()
                    .any(|task| task.status == BackgroundTaskStatus::Failed);
                if request.mode == "first_failure" && failed {
                    return Ok(task_event_output("first_failure", &tasks));
                }
                if request.mode == "all" {
                    // Take the finished children from the snapshot instead of a
                    // local list: an event dropped by a lagged receiver still
                    // shows up here, and nothing is marked delivered until the
                    // whole selection is reported, so a deadline cannot consume
                    // a child that was never handed to the caller.
                    let snapshot = collect(manager, &me, &request).await;
                    if tasks.iter().all(|task| task.status != BackgroundTaskStatus::Running)
                        && snapshot.running_children.is_empty()
                    {
                        for (child_id, _) in &snapshot.finished_children {
                            mark_delivered(&me, child_id);
                        }
                        return Ok(all_finished_output(&tasks, &snapshot.finished_children));
                    }
                } else if tasks.iter().any(|task| task.status != BackgroundTaskStatus::Running) {
                    return Ok(task_event_output("any_finished", &tasks));
                }
            }
        }
    }
}

fn task_event_output(reason: &str, tasks: &[background::TaskStatusFile]) -> ToolOutput {
    let mut text = format!("Wait returned: {reason}\n\n");
    for task in tasks {
        text.push_str(&task_line(task));
    }
    ToolOutput::new(text).with_title("bg wait").with_metadata(json!({
        "wait_reason": reason,
        "timed_out": false,
        "tasks": tasks.iter().map(|task| task.task_id.clone()).collect::<Vec<_>>(),
    }))
}

async fn tasks_of(manager: &background::BackgroundTaskManager, task_ids: &[String]) -> Vec<background::TaskStatusFile> {
    let mut tasks = Vec::with_capacity(task_ids.len());
    for task_id in task_ids {
        if let Some(task) = manager.status(task_id).await {
            tasks.push(task);
        }
    }
    tasks
}

#[cfg(test)]
#[path = "bg_wait_tests.rs"]
mod bg_wait_tests;
