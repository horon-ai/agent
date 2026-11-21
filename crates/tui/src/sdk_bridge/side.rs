//! The side question: a read-only consult child of the attached session, asked
//! from a second connection so the parent keeps its transcript, its tools and
//! its stream untouched.
//!
//! The child is created by the daemon as a consult fork (`ForkSession` with
//! `consult: true`). The classification is written to the child's stored record
//! before its id is answered, and every agent the daemon builds for it starts on
//! `CONSULT_TOOLS` (`agent::build_base`), so the child is never reachable with
//! the parent's tools — not in the moment after the fork, and not after a daemon
//! restart. Because the narrowing is the daemon's, this module never configures
//! tools and never widens the child; it attaches, asks, and reads the answer.
//!
//! The connection this module opens is the child's only reader. Its events are
//! reduced here into the few things the overlay draws, so the UI thread never
//! sees a raw protocol frame for a session it is not attached to.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use horon_agent_sdk::api::{ApiEvent, PermissionDecision};
use horon_agent_sdk::{EventStream, HistoryMessage, HoronAgentClient};

use super::sessions::Dial;
use super::Incoming;

/// How long one stream read waits before the loop checks the connection again.
///
/// A closed stream answers `Ok(None)` for ever, so an idle read must be paired
/// with `is_closed`; this is the pause between those two checks and nothing
/// else. It is short enough that a close is noticed promptly and long enough
/// that an idle side answer costs no measurable CPU.
const EVENT_POLL: Duration = Duration::from_millis(250);

/// How long a side answer may run before the client gives up on it.
///
/// Generous on purpose: a child may read a large file or think for a while, and
/// cutting a real answer short is worse than a thread that waits. It exists so
/// a daemon that stops answering without closing the socket cannot leave the
/// overlay "working" for the rest of the session.
const ANSWER_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// The hidden instruction that travels with the question.
///
/// The child already has the parent's system prompt and transcript; this only
/// says what the turn is for, so it does not silently continue the parent's
/// last task. It is a reminder rather than a message, so it never shows up in
/// the child's stored conversation.
const SIDE_REMINDER: &str = "This is a side question about the conversation above. Answer it from that \
     conversation and from what the read-only tools can show you. Do not try to change anything; the tools \
     that could write are not available in this session. Keep the answer self-contained and concise.";

/// Deliver one side event to the UI.
///
/// The only failure a delivery has is a closed receiver, which means the UI
/// exited while this question was in flight: there is nobody left to tell, and
/// nothing to clean up here. Every failure that still matters — connecting,
/// attaching, forking, asking, reading the answer — travels back to the caller
/// as a message instead of through this channel.
fn deliver(events: &Sender<Incoming>, incoming: Incoming) -> bool {
    events.send(incoming).is_ok()
}

/// Report a side question that could not be started at all.
pub(super) fn fail(events: &Sender<Incoming>, id: u64, message: &str) {
    deliver(events, Incoming::SideFailed { id, message: message.to_string() });
}

/// The steps a side question takes before its answer.
///
/// They are separated from their SDK calls so their order can be pinned: the
/// daemon must have created the narrow child before the question is sent, and
/// the child's id must not reach the screen before that.
pub(super) trait SideLink {
    /// Attach the connection to a session. Called for the parent first and for
    /// the child once it exists.
    fn attach(&self, session: &str) -> Result<(), String>;
    /// Create the read-only consult child of `parent`, stopping before
    /// `boundary` when one was chosen; answers its id.
    fn fork(&self, parent: &str, boundary: Option<&str>) -> Result<String, String>;
    /// The child's stored rows before the question. The answer read back at the
    /// end counts only the rows after this, so the forked parent transcript can
    /// never be mistaken for the child's answer.
    fn baseline(&self, child: &str) -> Result<usize, String>;
    fn ask(&self, child: &str, question: &str) -> Result<(), String>;
}

/// A step that failed, whether the question had already been sent, and the
/// child when one exists.
///
/// The child is named in a failure so a reader can open it (it is read-only and
/// stored); a failure before the fork names none.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct AskError {
    pub(super) message: String,
    pub(super) asked: bool,
    pub(super) child: Option<String>,
}

/// One side connection's own client, in the [`SideLink`] shape.
struct LiveLink<'a> {
    client: &'a HoronAgentClient,
}

impl SideLink for LiveLink<'_> {
    fn attach(&self, session: &str) -> Result<(), String> {
        self.client
            .attach_session(session)
            .map(drop)
            .map_err(|error| format!("the session could not be opened: {error}"))
    }

    fn fork(&self, parent: &str, boundary: Option<&str>) -> Result<String, String> {
        let forked = match boundary {
            Some(boundary) => self.client.fork_consult_at(parent, boundary),
            None => self.client.fork_consult(parent),
        };
        forked
            .map(|child| child.session_id)
            .map_err(|error| format!("the harness refused a side session: {error}"))
    }

    fn baseline(&self, child: &str) -> Result<usize, String> {
        self.client
            .get_history(child)
            .map(|messages| messages.len())
            .map_err(|error| format!("the side session's transcript could not be read: {error}"))
    }

    fn ask(&self, child: &str, question: &str) -> Result<(), String> {
        self.client
            .send_message_with_reminder(child, question, Vec::new(), Some(SIDE_REMINDER), None)
            .map_err(|error| format!("the side question could not be sent: {error}"))
    }
}

/// Attach the parent, create the narrow child, note the boundary, announce the
/// id, then ask — in that order.
///
/// `announce` runs only after the daemon answered with a child, which is after
/// the child was persisted with its consult flag: a client that opens the id
/// from the screen lands on a session that is already read-only.
pub(super) fn open_with(
    link: &dyn SideLink,
    parent: &str,
    boundary: Option<&str>,
    question: &str,
    announce: &mut dyn FnMut(&str),
) -> Result<(usize, String), AskError> {
    link.attach(parent).map_err(|message| AskError {
        message,
        asked: false,
        child: None,
    })?;
    let child = link.fork(parent, boundary).map_err(|message| AskError {
        message,
        asked: false,
        child: None,
    })?;
    // The boundary and the question are read on the child, so the connection
    // moves to it now. A failure here leaves the stored child named.
    link.attach(&child).map_err(|message| AskError {
        message,
        asked: false,
        child: Some(child.clone()),
    })?;
    let baseline = link.baseline(&child).map_err(|message| AskError {
        message,
        asked: false,
        child: Some(child.clone()),
    })?;
    announce(&child);
    link.ask(&child, question).map_err(|message| AskError {
        message,
        asked: true,
        child: Some(child.clone()),
    })?;
    Ok((baseline, child))
}

/// Start the thread that asks one side question.
///
/// Called on the worker thread. A failure is reported as a side failure rather
/// than a refused command: the user asked for an answer, not for a command to
/// run, and the overlay is what explains it.
pub(super) fn start(
    parent: &str,
    id: u64,
    dial: Dial,
    question: String,
    boundary: Option<String>,
    events: Sender<Incoming>,
) {
    let spawned = thread::Builder::new()
        .name("horon-tui-side-question".to_string())
        .spawn({
            let parent = parent.to_string();
            let events = events.clone();
            move || follow(id, dial, parent, question, boundary, events)
        });
    if spawned.is_err() {
        deliver(&events, Incoming::SideFailed {
            id,
            message: "could not start the side question thread".to_string(),
        });
    }
}

/// The thread body: connect, open, ask, and reduce the answer's events.
fn follow(
    id: u64,
    dial: Dial,
    parent: String,
    question: String,
    boundary: Option<String>,
    events: Sender<Incoming>,
) {
    if let Err(message) = answer(id, &dial, &parent, &question, boundary.as_deref(), &events) {
        deliver(&events, Incoming::SideFailed { id, message });
    }
}

fn answer(
    id: u64,
    dial: &Dial,
    parent: &str,
    question: &str,
    boundary: Option<&str>,
    events: &Sender<Incoming>,
) -> Result<(), String> {
    let client = HoronAgentClient::connect(dial.options())
        .map_err(|error| format!("the side session could not connect: {error}"))?;
    // Unfiltered: the child's id is not known until the fork answers, and an
    // attach retargets this connection to the child before the question.
    let stream = client.events(None);
    let link = LiveLink { client: &client };
    let mut announce = |child: &str| {
        deliver(events, Incoming::SideStarted { id, session_id: child.to_string() });
    };
    match open_with(&link, parent, boundary, question, &mut announce) {
        Ok((baseline, child)) => drain(id, &client, &stream, &child, baseline, events),
        Err(AskError { message, child, .. }) => Err(match child {
            Some(child) => format!("{message}; the side session is {child}"),
            None => message,
        }),
    }
}

/// Read the child's events until its turn ends, reporting what the overlay shows.
fn drain(
    id: u64,
    client: &HoronAgentClient,
    stream: &EventStream,
    child: &str,
    baseline: usize,
    events: &Sender<Incoming>,
) -> Result<(), String> {
    let mut preview = Preview::default();
    let started = Instant::now();
    loop {
        if started.elapsed() > ANSWER_DEADLINE {
            return Err(match client.cancel(child) {
                Ok(()) => "the side question did not finish in time; press enter to open it later".to_string(),
                Err(error) => format!(
                    "the side question did not finish in time and could not be cancelled ({error}); \
                     press enter to open it later"
                ),
            });
        }
        match stream.next_result_timeout(EVENT_POLL) {
            Ok(Some(event)) => {
                let (effects, progress) = observe(&event, id, child, &mut preview);
                for effect in effects {
                    match effect {
                        Effect::Send(incoming) => {
                            if events.send(incoming).is_err() {
                                return Ok(());
                            }
                        }
                        Effect::Deny(request_id) => {
                            if let Err(error) =
                                client.respond_to_permission(child, &request_id, PermissionDecision::Deny)
                            {
                                let line = format!("could not refuse a side approval: {error}");
                                deliver(events, Incoming::SideNotice { id, line });
                            }
                        }
                    }
                }
                if progress == Progress::Finished {
                    // The stream already holds the answer, so a history read
                    // that fails is not worth replacing a finished answer with
                    // an error: the stored row is preferred only because it is
                    // what a provider retry settled on.
                    let answer = match client.get_history(child) {
                        Ok(messages) => stored_answer(&messages, baseline).unwrap_or_else(|| preview.text()),
                        Err(_) => preview.text(),
                    };
                    deliver(events, Incoming::SideDone { id, answer });
                    return Ok(());
                }
            }
            Ok(None) => {
                if client.is_closed() {
                    return Err("the side session's connection closed before it answered".to_string());
                }
            }
            Err(error) => return Err(format!("the side session's event stream ended: {error}")),
        }
    }
}

/// One change the drain loop has to perform.
#[derive(Debug, PartialEq)]
pub(super) enum Effect {
    /// Forward something the overlay draws.
    Send(Incoming),
    /// Answer a parked approval with a refusal. A side question cannot collect
    /// a permission decision, and granting one silently would bypass the
    /// standing policy that asked for it.
    Deny(String),
}

/// What one event says about the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Progress {
    Continue,
    Finished,
}

/// Reduce one event of the side child.
///
/// Events of another session are ignored rather than trusted: the subscription
/// is filtered, but a fan-out bug in any layer must not put another
/// conversation's words into a side answer.
pub(super) fn observe(event: &ApiEvent, id: u64, child: &str, preview: &mut Preview) -> (Vec<Effect>, Progress) {
    let mut effects = Vec::new();
    let mut progress = Progress::Continue;
    match event {
        ApiEvent::TextDelta {
            session_id,
            text,
            message_id,
        } if session_id.as_str() == child => {
            preview.delta(message_id.as_deref(), text);
            effects.push(Effect::Send(Incoming::SideAnswer {
                id,
                answer: preview.text(),
            }));
        }
        ApiEvent::TextReplace {
            session_id,
            text,
            message_id,
        } if session_id.as_str() == child => {
            preview.replace(message_id.as_deref(), text);
            effects.push(Effect::Send(Incoming::SideAnswer {
                id,
                answer: preview.text(),
            }));
        }
        ApiEvent::ToolStart { session_id, name, .. } if session_id.as_str() == child => {
            effects.push(Effect::Send(Incoming::SideProgress {
                id,
                line: format!("using {name}"),
            }));
        }
        ApiEvent::PermissionRequest {
            session_id,
            request_id,
            tool_name,
            ..
        } if session_id.as_str() == child => {
            effects.push(Effect::Deny(request_id.clone()));
            effects.push(Effect::Send(Incoming::SideNotice {
                id,
                line: format!("denied a request to use {tool_name}: a side question cannot collect an approval"),
            }));
        }
        ApiEvent::ApprovalPending {
            session_id,
            request_id,
            action,
            ..
        } if session_id.as_str() == child => {
            effects.push(Effect::Deny(request_id.clone()));
            effects.push(Effect::Send(Incoming::SideNotice {
                id,
                line: format!("denied a request to {action}: a side question cannot collect an approval"),
            }));
        }
        ApiEvent::TurnStopped { session_id, message, .. } if session_id.as_str() == child => {
            effects.push(Effect::Send(Incoming::SideNotice {
                id,
                line: message.clone(),
            }));
        }
        ApiEvent::TurnDone { session_id, .. } if session_id.as_str() == child => progress = Progress::Finished,
        _ => {}
    }
    (effects, progress)
}

/// The answer the child stored, counting only rows after `baseline`.
///
/// `baseline` is the forked transcript's length before the question, so the
/// parent's last answer can never stand in for a child that produced none. The
/// stored row is preferred over the streamed preview because a provider retry
/// can retract streamed text, and the row is what the daemon settled on.
pub(super) fn stored_answer(messages: &[HistoryMessage], baseline: usize) -> Option<String> {
    messages.iter().skip(baseline).rev().find_map(|message| {
        if message.role != "assistant" {
            return None;
        }
        let text = message
            .answer
            .clone()
            .filter(|answer| !answer.trim().is_empty())
            .unwrap_or_else(|| message.content.clone());
        (!text.trim().is_empty()).then_some(text)
    })
}

/// The answer text as the stream has built it so far.
///
/// Each assistant message of the turn is one segment: a new `message_id` starts
/// a new one, and a replacement rewrites the segment it names. A provider retry
/// that retracts its message therefore cannot leave the retracted words behind
/// in the next event's rendering.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Preview {
    segments: Vec<(Option<String>, String)>,
}

impl Preview {
    pub(super) fn delta(&mut self, message_id: Option<&str>, text: &str) {
        if text.is_empty() {
            return;
        }
        // An id-less delta is the legacy single stream: it belongs to the
        // message that is open, whatever id that message was given.
        match self.segments.last_mut() {
            Some((id, body)) if message_id.is_none() || id.as_deref() == message_id => body.push_str(text),
            _ => self.segments.push((message_id.map(str::to_string), text.to_string())),
        }
    }

    pub(super) fn replace(&mut self, message_id: Option<&str>, text: &str) {
        let found = match message_id {
            None => self.segments.last_mut(),
            Some(id) => self.segments.iter_mut().rev().find(|(known, _)| known.as_deref() == Some(id)),
        };
        match found {
            Some((_, body)) => *body = text.to_string(),
            None => self.segments.push((message_id.map(str::to_string), text.to_string())),
        }
    }

    pub(super) fn text(&self) -> String {
        let mut answer = String::new();
        for (_, body) in &self.segments {
            if body.is_empty() {
                continue;
            }
            if !answer.is_empty() {
                answer.push_str("\n\n");
            }
            answer.push_str(body);
        }
        answer
    }
}
