//! IS-5 foreground bookkeeping: the state a v2 session last reported, and the
//! rules that turn daemon events into the `state_update`s it writes.
//!
//! One [`ForegroundTracker`] lives in each v2 session object; the session's
//! stream task (`state.rs`) drives it and writes whatever it returns, in
//! order. Nothing here touches a socket, so every rule below is a pure state
//! transition.

use super::super::*;
use super::shapes::{self, state_idle, state_requires_action, state_running};
use std::collections::{HashMap, HashSet};

/// The stop reasons a v2 idle `state_update` may carry; anything else the
/// daemon reports is omitted.
const STOP_REASONS: [&str; 6] = [
    "end_turn",
    "max_tokens",
    "max_turn_requests",
    "refusal",
    "cancelled",
    "error",
];

/// What a failed turn's idle update says when no daemon `Error` named a cause.
const TURN_FAILED: &str = "The turn failed";

/// The state this connection last reported for the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Reported {
    #[default]
    Idle,
    Running,
    RequiresAction,
}

/// IS-5 bookkeeping of one v2 session. Every method returns the updates to
/// write, in order; the stream task writes them before it reads the next event.
#[derive(Debug, Default)]
pub(crate) struct ForegroundTracker {
    pub(crate) reported: Reported,
    /// Permission and elicitation requests of the running turn this editor has
    /// not answered yet.
    pub(crate) pending_user_actions: usize,
    pub(crate) run_id: Option<String>,
    /// The start message id of the run reported as running, when it named one.
    run_start: Option<String>,
    /// Message ids of the prompts this editor inserted. Their `user_message`
    /// was written with the response, so a turn they start writes none.
    pub(crate) own_start_ids: HashSet<String>,
    /// Every request of a blocked turn was answered: the next mapped event
    /// reports `running` first.
    pub(crate) resume_running_before_next_event: bool,
    /// Own prompts inserted whose turn has not reported idle yet.
    open_own_starts: HashSet<String>,
    /// Daemon `message` id -> stored message id of those open own prompts.
    own_message_ids: HashMap<u64, String>,
    /// The last failure the daemon reported for the current turn.
    last_error: Option<String>,
}

impl ForegroundTracker {
    pub(crate) fn is_active(&self) -> bool {
        self.reported != Reported::Idle
    }

    /// One of this editor's prompts, daemon `message` `daemon_id`, was
    /// inserted as `message_id`.
    pub(crate) fn own_prompt_inserted(&mut self, daemon_id: u64, message_id: &str) {
        self.own_start_ids.insert(message_id.to_string());
        self.open_own_starts.insert(message_id.to_string());
        self.own_message_ids.insert(daemon_id, message_id.to_string());
    }

    /// The turn daemon `message` `daemon_id` started has not reported idle.
    pub(crate) fn own_turn_open(&self, daemon_id: u64) -> bool {
        self.own_message_ids
            .get(&daemon_id)
            .is_some_and(|message_id| self.open_own_starts.contains(message_id))
    }

    fn forget_ended_own_turns(&mut self) {
        let open = &self.open_own_starts;
        self.own_message_ids.retain(|_, message_id| open.contains(message_id));
    }

    /// The turn this editor's prompt `message_id` started has reported idle
    /// (or never will, because the stream ended).
    pub(crate) fn own_run_ended(&self, message_id: &str) -> bool {
        !self.open_own_starts.contains(message_id)
    }

    /// Foreground work a `session/cancel` must stop: a turn reported running,
    /// or one this editor's inserted prompt started that has not reported yet.
    pub(crate) fn has_cancellable_turn(&self) -> bool {
        self.is_active() || !self.open_own_starts.is_empty()
    }

    /// Whether a non-snapshot `ForegroundState { running: false }` of `run_id`
    /// ends the run this tracker reports. An idle state of any other run is
    /// stale: that run was already ended by [`Self::supersede`] when the run
    /// after it started, or it never reported running while this one runs.
    pub(crate) fn ends_current_run(&self, run_id: &str) -> bool {
        self.run_id.as_deref().is_none_or(|current| current == run_id)
    }

    /// A run other than the one reported started. A session runs one
    /// foreground turn at a time, so the reported run is over and its own idle
    /// state is still on its way: report its end now, with no stop reason
    /// (unknown here), and let [`Self::ends_current_run`] drop the late one.
    /// `None` when `run_id` is the run reported, or none is reported; else the
    /// updates and whether the ended run was one this editor's prompt started.
    pub(crate) fn supersede(&mut self, run_id: &str) -> Option<(Vec<Value>, bool)> {
        let current = self.run_id.as_deref()?;
        if !self.is_active() || current == run_id {
            return None;
        }
        let start = self.run_start.clone();
        let own = start.as_deref().is_some_and(|start| !self.own_run_ended(start));
        Some((self.turn_ended(start.as_deref(), None), own))
    }

    /// The late idle state of a run [`Self::ends_current_run`] rejected: the
    /// run reported now goes on. True when it ended a turn this editor's
    /// prompt started, which no longer waits for its end.
    pub(crate) fn stale_run_ended(&mut self, start_message_id: Option<&str>) -> bool {
        let own = start_message_id.is_some_and(|start| self.open_own_starts.remove(start));
        self.forget_ended_own_turns();
        own
    }

    /// A non-snapshot `ForegroundState { running: true }`.
    pub(crate) fn turn_started(
        &mut self,
        run_id: &str,
        start_message_id: Option<&str>,
        user_content: Option<&str>,
    ) -> Vec<Value> {
        let mut updates = Vec::new();
        if self.reported == Reported::Idle {
            self.reported = Reported::Running;
            updates.push(state_running());
        }
        self.run_id = Some(run_id.to_string());
        self.run_start = start_message_id.map(str::to_string);
        self.last_error = None;
        if let (Some(start), Some(text)) = (start_message_id, user_content)
            && !self.own_start_ids.contains(start)
        {
            updates.push(shapes::user_message(start, &[json!({ "type": "text", "text": text })]));
        }
        updates
    }

    /// A non-snapshot `ForegroundState { running: false }`: idle once per run.
    /// A repeat for a session already idle writes nothing, except when it ends
    /// a turn this editor's prompt started, which always reports its end.
    pub(crate) fn turn_ended(&mut self, start_message_id: Option<&str>, stop_reason: Option<&str>) -> Vec<Value> {
        let own = start_message_id.is_some_and(|start| self.open_own_starts.remove(start));
        self.forget_ended_own_turns();
        if self.reported == Reported::Idle && !own {
            return Vec::new();
        }
        let stop_reason = stop_reason.filter(|reason| STOP_REASONS.contains(reason));
        let error = (stop_reason == Some("error")).then(|| {
            let message = self.last_error.take().unwrap_or_else(|| TURN_FAILED.to_string());
            (error_code(&message), message)
        });
        self.settle_idle();
        vec![state_idle(stop_reason, error)]
    }

    /// The attach or resume snapshot: the tracker follows it and writes
    /// nothing; `session/resume` then reports [`Self::current_state`]. A turn
    /// waiting on an answer this connection never saw reports `running` again
    /// before its next mapped event, which only an answered wait produces.
    pub(crate) fn snapshot(&mut self, running: bool, awaiting_user: bool, run_id: &str) {
        if !running {
            self.settle_idle();
            return;
        }
        self.reported = if awaiting_user {
            Reported::RequiresAction
        } else {
            Reported::Running
        };
        self.resume_running_before_next_event = awaiting_user;
        self.run_id = (!run_id.is_empty()).then(|| run_id.to_string());
        self.run_start = None;
    }

    /// IS-5: the one `state_update` a `session/resume` writes, the state last
    /// reported. An idle snapshot carries no stop reason: it reports no
    /// completion.
    pub(crate) fn current_state(&self) -> Value {
        match self.reported {
            Reported::Idle => state_idle(None, None),
            Reported::Running => state_running(),
            Reported::RequiresAction => state_requires_action(),
        }
    }

    /// A daemon `Error` (or failing `TurnStopped`) seen while a turn runs.
    pub(crate) fn record_error(&mut self, message: &str) {
        self.last_error = Some(message.to_string());
    }

    /// A failing `TurnStopped` names the cause only when no `Error` did.
    pub(crate) fn record_failure_note(&mut self, message: &str) {
        self.last_error.get_or_insert_with(|| message.to_string());
    }

    /// A permission or elicitation request went to this editor.
    pub(crate) fn user_action_started(&mut self) -> Vec<Value> {
        match self.reported {
            // A request outside foreground work leaves the session idle.
            Reported::Idle => Vec::new(),
            Reported::RequiresAction => {
                self.pending_user_actions += 1;
                Vec::new()
            }
            Reported::Running => {
                self.pending_user_actions += 1;
                self.resume_running_before_next_event = false;
                self.reported = Reported::RequiresAction;
                vec![state_requires_action()]
            }
        }
    }

    /// This editor answered one of those requests. Nothing is written: the
    /// turn reports `running` once its progress shows, before its next event.
    pub(crate) fn user_action_finished(&mut self) {
        if self.pending_user_actions == 0 {
            return;
        }
        self.pending_user_actions -= 1;
        if self.pending_user_actions == 0 && self.reported == Reported::RequiresAction {
            self.resume_running_before_next_event = true;
        }
    }

    /// Before the updates one stream event maps to.
    pub(crate) fn before_mapped_event(&mut self) -> Vec<Value> {
        if !self.resume_running_before_next_event || self.pending_user_actions > 0 {
            return Vec::new();
        }
        self.resume_running_before_next_event = false;
        self.reported = Reported::Running;
        vec![state_running()]
    }

    /// `session/close` gave up waiting for the turn: report it cancelled here.
    /// A turn this editor's inserted prompt started reports its end even when
    /// it never reported `running`.
    pub(crate) fn synthesize_cancelled_idle(&mut self) -> Vec<Value> {
        let own_open = !self.open_own_starts.is_empty();
        self.open_own_starts.clear();
        self.own_message_ids.clear();
        if self.reported == Reported::Idle && !own_open {
            return Vec::new();
        }
        self.settle_idle();
        vec![state_idle(Some("cancelled"), None)]
    }

    /// The daemon went away while a turn was reported.
    pub(crate) fn disconnected(&mut self) -> Vec<Value> {
        self.open_own_starts.clear();
        self.own_message_ids.clear();
        if self.reported == Reported::Idle {
            return Vec::new();
        }
        self.settle_idle();
        vec![state_idle(
            Some("error"),
            Some((JSONRPC_INTERNAL_ERROR, DAEMON_DISCONNECTED.to_string())),
        )]
    }

    fn settle_idle(&mut self) {
        self.reported = Reported::Idle;
        self.pending_user_actions = 0;
        self.resume_running_before_next_event = false;
        self.run_id = None;
        self.run_start = None;
        self.last_error = None;
    }
}

/// -32000 for a failure the provider login caused, -32603 for every other.
pub(crate) fn error_code(message: &str) -> i64 {
    if is_auth_failure(message) {
        ACP_AUTH_REQUIRED
    } else {
        JSONRPC_INTERNAL_ERROR
    }
}

/// The daemon `Error` carries only text, so the provider login failures are
/// recognized by whole words: `request 4011 failed` is no HTTP 401.
fn is_auth_failure(message: &str) -> bool {
    const MARKERS: [&[&str]; 7] = [
        &["401"],
        &["unauthorized"],
        &["not", "logged", "in"],
        &["login", "required"],
        &["invalid", "api", "key"],
        &["no", "credentials"],
        &["authentication", "failed"],
    ];
    let message = message.to_ascii_lowercase();
    let words: Vec<&str> = message
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    MARKERS
        .iter()
        .any(|marker| words.windows(marker.len()).any(|window| window == *marker))
}
