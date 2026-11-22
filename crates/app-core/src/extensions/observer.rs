//! Minimal observation events for installed package children.
//!
//! An installed package (extension or plugin) may subscribe to six host events:
//! `session.start`, `session.end`, `turn.start`, `turn.end`, `tool.before` and
//! `tool.after`. The subscription set comes from the extension declaration's
//! `events` and from the plugin's bundled hook files, and every event travels
//! the one path a package already speaks: an `observer/notify` notification on
//! the supervised child that tool and command calls use.
//!
//! Three properties shape this module:
//!
//! - **Never load-bearing.** Publishing is synchronous and never awaits a child.
//!   The event goes into a bounded queue; a full queue drops the event, counts
//!   it and returns, so a slow, stuck or misbehaving package cannot block or fail
//!   the turn that published it. Delivery runs on one worker task per session.
//! - **Minimal metadata.** A tool observation carries the tool name, call id,
//!   origin and sequence; `tool.after` carries the outcome class and duration.
//!   No prompt text, tool input or tool result is ever put in a notice.
//! - **Nothing before approval.** The tool dispatcher publishes `tool.before`
//!   only after the approval mode and the `pre_tool` gate allowed the call, and
//!   `tool.after` only for a call that ran. An observation therefore can never
//!   allow, deny or modify a call.
//!
//! Sequence numbers are per session and monotonic. `session.start` is published
//! once per session id even when a client reconnects and a new agent object is
//! built for the same session, because this state outlives the agent and is only
//! cleared when the session itself is removed. A disable or removal stops
//! delivery through the revocation registry, which the delivery path checks
//! before every notice.
//!
//! Plan mode is read-only, so an observation never starts a package child on its
//! own while a session is planning; a package tool or command call in the same
//! session still starts it through the ordinary approved path.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use horon_agent_extension_types::ObserverEvent;
use horon_agent_extension_types::wire::ObserverParams;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::packages::PackageStore;

/// Events one session may have waiting for delivery.
///
/// The turn never waits for this queue: reaching the bound drops the event and
/// counts it. The value matches the wire's own queued-frame bound so one
/// misbehaving package cannot make the worker fall further behind than the rest
/// of the protocol allows.
const MAX_OBSERVATIONS: usize = 128;

/// One queued event, with the workspace its session reported when it started.
struct Notice {
    params: ObserverParams,
    workspace: Option<PathBuf>,
}

/// One session's observation queue and its counters.
struct SessionObserver {
    sender: mpsc::Sender<Notice>,
    next_seq: u64,
    session_started: bool,
    /// Events a full queue or a closed worker discarded, for the debug line.
    dropped: u64,
}

impl SessionObserver {
    fn new() -> (Self, mpsc::Receiver<Notice>) {
        let (sender, receiver) = mpsc::channel(MAX_OBSERVATIONS);
        (
            Self {
                sender,
                next_seq: 0,
                session_started: false,
                dropped: 0,
            },
            receiver,
        )
    }

    /// Queue one event. Returns whether it was queued.
    ///
    /// `session.start` is refused after the first one, and the flag is only set
    /// when the notice really reached the queue, so a drop cannot silently cost
    /// the session its one start event.
    fn push(&mut self, event: ObserverEvent, workspace: Option<PathBuf>, metadata: impl FnOnce(u64) -> Value) -> bool {
        if event == ObserverEvent::SessionStart && self.session_started {
            return false;
        }
        let seq = self.next_seq + 1;
        let params = ObserverParams {
            event,
            metadata: metadata(seq),
        };
        match self.sender.try_send(Notice { params, workspace }) {
            Ok(()) => {
                self.next_seq = seq;
                if event == ObserverEvent::SessionStart {
                    self.session_started = true;
                }
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped += 1;
                crate::logging::debug(&format!(
                    "package observation {event:?} was dropped: {} event(s) are waiting",
                    self.dropped
                ));
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

type Sessions = BTreeMap<String, Arc<Mutex<SessionObserver>>>;

static SESSIONS: LazyLock<Mutex<Sessions>> = LazyLock::new(|| Mutex::new(Sessions::new()));

fn lock_sessions() -> MutexGuard<'static, Sessions> {
    SESSIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The state of one session, created with its worker on first use.
///
/// `None` when the caller is not running on a tokio runtime (no worker could
/// deliver anything, and silently queueing into a channel nobody drains would
/// only hide the event) and when the package area does not exist yet (nothing is
/// installed, so nothing can subscribe).
fn state_for(session_id: &str) -> Option<Arc<Mutex<SessionObserver>>> {
    let mut sessions = lock_sessions();
    if let Some(state) = sessions.get(session_id) {
        return Some(Arc::clone(state));
    }
    let runtime = match tokio::runtime::Handle::try_current() {
        Ok(runtime) => runtime,
        Err(_) => return None,
    };
    let store = match PackageStore::for_current_user() {
        Ok(store) => store,
        Err(error) => {
            crate::logging::debug(&format!(
                "package observations for session {session_id} have no package area: {error:#}"
            ));
            return None;
        }
    };
    if !store.root().exists() {
        // Nothing is installed, so nothing can subscribe. Skipping the state
        // keeps sessions on a package-free home from starting a worker at all.
        return None;
    }
    let (state, receiver) = SessionObserver::new();
    let state = Arc::new(Mutex::new(state));
    sessions.insert(session_id.to_owned(), Arc::clone(&state));
    runtime.spawn(dispatch_loop(session_id.to_owned(), store, receiver));
    Some(state)
}

fn publish(session_id: &str, event: ObserverEvent, workspace: Option<PathBuf>, metadata: impl FnOnce(u64) -> Value) {
    let Some(state) = state_for(session_id) else {
        return;
    };
    let mut state = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    state.push(event, workspace, metadata);
}

async fn dispatch_loop(session_id: String, store: PackageStore, mut receiver: mpsc::Receiver<Notice>) {
    while let Some(notice) = receiver.recv().await {
        deliver(&session_id, &store, notice).await;
    }
    // The session is gone: a delivery that raced its cleanup may have reopened the
    // package area, so release whatever it holds now that nothing else will.
    crate::extensions::package_snapshot::forget_session(&session_id);
}

async fn deliver(session_id: &str, store: &PackageStore, notice: Notice) {
    let targets = crate::extensions::invoke::observation_targets(
        store,
        session_id,
        notice.params.event,
        notice.workspace.as_deref(),
    )
    .await;
    for target in targets {
        let delivered =
            crate::extensions::invoke::deliver_observation(store, session_id, &target, &notice.params).await;
        if let Err(error) = delivered {
            crate::logging::debug(&format!(
                "package {} observation for session {session_id} was not delivered: {error:#}",
                target.package_id()
            ));
        }
    }
}

/// Publish a session lifecycle event for one session.
///
/// `event` is the lifecycle hook name (`session_start`/`session_end`), so the
/// one table in `hooks` maps command hook names and package observations alike.
pub(crate) fn session_lifecycle(session_id: &str, event: &str, source: &str, workspace: Option<&str>) {
    let Some(event) = crate::hooks::observation_event(event) else {
        return;
    };
    let workspace = workspace.map(PathBuf::from);
    publish(
        session_id,
        event,
        workspace,
        |seq| json!({"seq": seq, "source": source}),
    );
}

/// Whether a turn or tool event has a package generation to resolve against.
///
/// Every turn entry point captures a package snapshot before model generation
/// and before the first tool call, so a turn event without one means the package
/// surface was never opened for this session. There is nothing to observe then,
/// and creating a queue (or resolving a store) would be work with no reader.
fn turn_is_observable(session_id: &str) -> bool {
    crate::extensions::package_snapshot::for_session(session_id).is_some_and(|session| session.current().is_some())
}

/// Publish `turn.start` before model generation and the first tool call.
pub(crate) fn turn_started(session_id: &str, source: &str) {
    if !turn_is_observable(session_id) {
        return;
    }
    publish(
        session_id,
        ObserverEvent::TurnStart,
        None,
        |seq| json!({"seq": seq, "source": source}),
    );
}

/// Publish `turn.end` with the turn's outcome class and duration.
pub(crate) fn turn_ended(session_id: &str, ok: bool, elapsed: Duration) {
    if !turn_is_observable(session_id) {
        return;
    }
    let duration_ms = millis(elapsed);
    publish(session_id, ObserverEvent::TurnEnd, None, |seq| {
        json!({
            "seq": seq,
            "status": if ok { "ok" } else { "error" },
            "duration_ms": duration_ms,
        })
    });
}

/// Publish `tool.before` for an approved call.
///
/// The caller is the dispatcher, after the approval mode and the `pre_tool` gate
/// allowed the call, so a refused or blocked call never reaches a package. The
/// notice names the tool, its call id and the surface it came from; it cannot
/// carry the input because none is passed here.
pub(crate) fn tool_before(ctx: &crate::tool::ToolContext, tool: &str) {
    if tool == crate::config::TOOLBOX_TOOL {
        // The dispatcher is a wrapper, not a call: its target publishes its own
        // pair through the inner dispatch.
        return;
    }
    if !turn_is_observable(&ctx.session_id) {
        return;
    }
    publish(&ctx.session_id, ObserverEvent::ToolBefore, None, |seq| {
        tool_before_metadata(tool, &ctx.tool_call_id, seq)
    });
}

/// Publish `tool.after` with the outcome class and the executed duration.
pub(crate) fn tool_after(ctx: &crate::tool::ToolContext, tool: &str, ok: bool, duration_ms: u64) {
    if tool == crate::config::TOOLBOX_TOOL {
        return;
    }
    if !turn_is_observable(&ctx.session_id) {
        return;
    }
    publish(&ctx.session_id, ObserverEvent::ToolAfter, None, |seq| {
        tool_after_metadata(ok, duration_ms, seq)
    });
}

/// The notice a package sees before an approved call.
///
/// Deliberately takes no input, prompt or result: the only facts here are the
/// identity of the call, the surface it came from and its sequence, so a later
/// change cannot leak payload through this path without changing this signature.
fn tool_before_metadata(tool: &str, call_id: &str, seq: u64) -> Value {
    json!({
        "seq": seq,
        "tool": tool,
        "call_id": call_id,
        "origin": origin(tool),
    })
}

/// The notice a package sees after an approved call ran.
fn tool_after_metadata(ok: bool, duration_ms: u64, seq: u64) -> Value {
    json!({
        "seq": seq,
        "outcome": if ok { "ok" } else { "error" },
        "duration_ms": duration_ms,
    })
}

/// Drop one session's observation state, called when the session is removed.
///
/// A client reconnect drops the agent but keeps the session, so this is
/// deliberately not part of the agent's teardown: `session.start` stays delivered
/// once and the sequence keeps advancing. A removed session starts from scratch.
pub(crate) fn forget_session(session_id: &str) {
    lock_sessions().remove(session_id);
}

fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// The surface a resolved tool name came from.
fn origin(tool: &str) -> &'static str {
    if tool.starts_with("ext__") {
        "package"
    } else if tool.starts_with("mcp__") {
        "mcp"
    } else {
        "builtin"
    }
}

#[cfg(test)]
#[path = "observer/tests.rs"]
mod tests;
