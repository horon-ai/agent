//! The bridge between the UI thread and the SDK.
//!
//! `horon-agent-sdk` is a synchronous client: `connect`, `create_session`,
//! `events` and `send_message` all block. A TUI that called them on its own
//! thread would freeze while a request is in flight, so this module moves them
//! to one background thread and speaks to the UI in two directions: commands
//! in, events out.
//!
//! Everything here is deliberately thin. It owns no rendering and no policy;
//! it maps a [`Command`] to the SDK call of the same name and forwards
//! [`ApiEvent`]s unchanged.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use horon_agent_sdk::api::{ApiEvent, PermissionDecision};
use horon_agent_sdk::{
    ConnectOptions, Error, ErrorKind, EventStream, HistoryMessage, HoronAgentClient, RuntimeCommand, SessionInfo,
};
use time::{OffsetDateTime, UtcOffset};

use crate::context::ContextReport;

mod messages;
mod runtime;
mod sessions;
mod skills;
mod steer;

#[cfg(test)]
mod runtime_tests;

#[cfg(test)]
mod steer_tests;

#[cfg(test)]
mod sessions_tests;

#[cfg(test)]
mod skills_tests;

pub use messages::{Command, Incoming};

/// How long the bridge waits for a command before it pumps the event stream
/// again. Short enough that keystrokes feel instant.
const COMMAND_POLL: Duration = Duration::from_millis(20);

/// How long one event read blocks. Long enough that an idle client is not a
/// busy loop; short enough that a command is picked up promptly.
const EVENT_POLL: Duration = Duration::from_millis(30);

/// How many ready events one pass forwards before it hands control back.
///
/// A stream faster than the UI must not be throttled to one event per pass,
/// but the pass still has to end somewhere: the bound is what keeps a burst
/// from starving the command queue and the exit conditions.
const EVENT_BATCH: usize = 256;

/// One row of the session picker, as the bridge built it.
///
/// The label is the session's title, the first thing the user said in it, or
/// its id, in that order: a session whose metadata never filled in is still a
/// row somebody can pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// What attaching to this row asks the harness for.
    pub id: String,
    /// What the row reads as.
    pub label: String,
    /// The time it was last written, or its status when it has no timestamp.
    pub detail: String,
    /// Whether this is the session the client is attached to.
    pub current: bool,
    /// The lines the picker's preview pane shows while this row is selected.
    ///
    /// Built once, when the row is: a preview assembled while drawing would
    /// read the stored conversation again on every frame.
    pub preview: Vec<String>,
}

/// One skill the harness reports, as the skill list shows it.
///
/// The fields are what the payload carries for every installed skill: an older
/// harness may omit any of them but the name, so the rest are optional here
/// rather than refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRow {
    /// The name the harness knows it by, and what `/skill:<name>` addresses.
    pub name: String,
    /// The description it gives it, on one line.
    pub description: String,
    /// The file the skill was read from, when the harness names one.
    pub path: Option<String>,
    /// The tools the skill may use, empty when the harness names none.
    pub allowed_tools: Vec<String>,
}

/// A session the bridge attached to, and what the client needs to show it.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachedSession {
    /// The status line's description, naming the new session.
    pub description: String,
    /// The session id the connection now follows.
    pub session_id: String,
    /// The lifecycle status the harness reported for it.
    pub status: String,
    /// The model serving it, when the catalog answered.
    pub model: Option<String>,
    /// The reasoning effort the harness reported for that model, trimmed.
    ///
    /// It belongs to the session being attached to, so it is filled from the
    /// session's own runtime report rather than left over from the one the
    /// client is leaving.
    pub effort: Option<String>,
    /// The stored conversation, oldest first.
    pub history: Vec<HistoryMessage>,
    /// The title the harness holds for it, trimmed, when it has one.
    ///
    /// It names the terminal window, so a stack of terminals says which session
    /// each one belongs to.
    pub title: Option<String>,
}

/// The handles one connected UI keeps.
pub struct Runtime {
    /// Queue to the bridge thread.
    pub commands: Sender<Command>,
    /// Queue from the bridge thread.
    pub events: Receiver<Incoming>,
    /// One-line description of what the client is attached to.
    pub description: String,
}

/// Connect, attach to or create a session, and start the bridge thread.
///
/// `attach` is a session id to resume instead of creating a new session.
/// `working_dir` only applies when a session is created; attaching to an
/// existing session keeps the directory it already has.
///
/// The error is a sentence a user can act on, not a protocol code: it is drawn
/// in the failure banner exactly as it arrives.
///
/// The typed SDK error decides the sentence, because the three ways the first
/// connection fails need different fixes: a daemon that is not running, a
/// client and daemon built against different protocol versions, and a session
/// id the daemon does not have.
pub fn connect(options: ConnectOptions, attach: Option<&str>, working_dir: Option<&str>) -> Result<Runtime, String> {
    let socket = options
        .socket_path
        .clone()
        .unwrap_or_else(horon_agent_sdk::api::api_socket_path);
    // Kept before the connection takes the options: `/new` needs a connection
    // of its own, because one connection gets one session from its first
    // create.
    let dial = sessions::Dial::of(&options);
    let client = HoronAgentClient::connect(options).map_err(|error| explain_connect(&error, &socket))?;
    // Subscribed before the request, because the identity event (provider,
    // model, reasoning effort) is published while the request is in flight: the
    // daemon answers a create's catalog probe, and a stream opened after the
    // answer only catches it when that reply happens to arrive last.
    //
    // Unfiltered in both cases: the daemon rewrites a typed session prefix to
    // the full id, so a subscription filtered by what the user typed would drop
    // every event of the session it just attached to. The worker applies the
    // filter that matters, against the id the answer names.
    let stream = client.events(None);
    let session = match attach {
        Some(session_id) => client
            .attach_session(session_id)
            .map_err(|error| explain_attach(&error, session_id))?,
        None => client
            .create_session(working_dir.map(str::to_owned))
            .map_err(|error| format!("the harness refused a new session: {error}"))?,
    };
    let session_id = session.session_id.clone();
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let description = format!("{} · {}", socket.display(), session_id);
    let where_ = socket.display().to_string();
    let worker_client = client.clone();
    thread::Builder::new()
        .name("horon-tui-bridge".to_string())
        .spawn(move || run_worker(worker_client, stream, session_id, where_, dial, command_rx, event_tx))
        .map_err(|error| format!("could not start the harness bridge thread: {error}"))?;
    Ok(Runtime {
        commands: command_tx,
        events: event_rx,
        description,
    })
}

/// What to tell the user when the first connection fails.
fn explain_connect(error: &Error, socket: &Path) -> String {
    match &error.kind {
        ErrorKind::ConnectFailed { path, cause } => {
            let where_ = path.as_deref().unwrap_or(socket).display();
            format!("no harness answered at {where_}. Start the daemon (horon-agent serve) and try again. {cause}")
        }
        ErrorKind::UnsupportedVersion {
            client_major,
            server_major,
            ..
        } => match server_major {
            Some(server) => format!(
                "this UI speaks protocol v{client_major} but the running daemon speaks v{server}. \
                 Update the daemon and the UI to the same release."
            ),
            None => format!("the daemon did not complete the v{client_major} handshake. Restart it and try again."),
        },
        _ => format!("cannot reach the harness: {error}"),
    }
}

/// What to tell the user when attaching to a session fails.
fn explain_attach(error: &Error, session_id: &str) -> String {
    match &error.kind {
        ErrorKind::UnknownSession { session_id } => {
            format!("the harness does not have session {session_id}. Check the id, or start a new session.")
        }
        _ => format!("the harness refused to attach to {session_id}: {error}"),
    }
}

/// The bridge thread: alternate between pumping events and reading commands.
///
/// `session_id` and `stream` are the session the thread follows, so they move
/// when the session changes, and everything after the move (sends, cancels,
/// answers) names the session that is now current. The two changes differ in
/// what they move: an attach retargets the one connection the SDK already
/// holds, while a new session opens a second connection, creates its session
/// there and leaves the first — a connection gets one session, from its first
/// create.
fn run_worker(
    mut client: HoronAgentClient,
    mut stream: EventStream,
    mut session_id: String,
    socket: String,
    dial: sessions::Dial,
    commands: Receiver<Command>,
    events: Sender<Incoming>,
) {
    // One answer before the first keystroke, so the first frame already names
    // the model and the effort instead of waiting for a message to learn them.
    // A failure is one notice and nothing else: the identity event that follows
    // the connect fills the same fields, and a status line that stays blank is
    // better than a client that cannot start.
    match runtime::read(&client, &session_id) {
        Ok(None) => {}
        Ok(Some(incoming)) => {
            if events.send(incoming).is_err() {
                return;
            }
        }
        Err(error) => {
            let notice = Incoming::Notice(format!("could not read the runtime info: {error}"));
            if events.send(notice).is_err() {
                return;
            }
        }
    }
    // The list behind `/skills`, read once so the first `/skills` already has
    // rows and so a listed name resolves before the user opens it.
    if !report_skills(&events, skills::read(&client, &session_id, false)) {
        return;
    }
    if !report_todos(&events, &client, &session_id) {
        return;
    }
    loop {
        // The connection flag is read by the pump so a dead transport is
        // reported even while the client sits idle with nothing to send.
        let mut refresh_todos = false;
        let step = pump(
            &events,
            |timeout| {
                stream.next_result_timeout(timeout).inspect(|event| {
                    refresh_todos |=
                        matches!(event, Some(ApiEvent::TodoChanged { session_id: owner }) if owner == &session_id);
                })
            },
            || client.is_closed(),
            &session_id,
        );
        if !matches!(step, Step::UiGone | Step::Closed) && refresh_todos && !report_todos(&events, &client, &session_id)
        {
            return;
        }
        match step {
            Step::UiGone | Step::Closed => return,
            flowing => match take_command(&commands, flowing == Step::Flowing) {
                CommandPoll::Command(command) => {
                    // The text is captured before the command is moved, so a
                    // failure can hand it back to the client.
                    let sent_text = match &command {
                        Command::Text(text) => Some(text.clone()),
                        _ => None,
                    };
                    let description = describe(&command);
                    // The two commands that change the session answer here
                    // rather than in `serve`, because each also moves the
                    // connection the worker holds. Both subscribe before the
                    // request leaves: the daemon publishes the new session's
                    // identity (provider, model, reasoning effort) while the
                    // request is in flight, and a stream opened after the
                    // answer would drop it. An attach retargets the one
                    // connection; a create gets a connection of its own,
                    // because the daemon answers a second subscribe on one
                    // connection with the session it already has. A failed dial
                    // or create moves nothing, which keeps the session the
                    // client shows attached and answering.

                    // Whether the answer moves the worker to another session,
                    // which is also when the skills of the one left behind stop
                    // being the client's.
                    let resets_session = matches!(&command, Command::NewSession { .. } | Command::Attach(_));
                    let served = match command {
                        Command::NewSession { working_dir } => {
                            match sessions::created(&dial, working_dir.as_deref(), &socket) {
                                Ok((fresh, next, attached)) => {
                                    client = fresh;
                                    sessions::follow(&attached, next, &mut session_id, &mut stream);
                                    Ok(Some(Incoming::Attached(Box::new(attached))))
                                }
                                Err(error) => Err(error),
                            }
                        }
                        Command::Attach(id) => {
                            // Unfiltered for the same reason as the start-up
                            // subscription: a typed prefix is rewritten to the
                            // full id, and a filter built from the typed text
                            // would drop the identity event and the turn's own
                            // output. `pump` keeps only the answer's session.
                            let next = client.events(None);
                            match sessions::resumed(&client, &id, &socket) {
                                Ok(attached) => {
                                    sessions::follow(&attached, next, &mut session_id, &mut stream);
                                    Ok(Some(Incoming::Attached(Box::new(attached))))
                                }
                                Err(error) => Err(error),
                            }
                        }
                        other => serve(&client, &session_id, other),
                    };
                    match served {
                        Ok(None) => {}
                        Ok(Some(incoming)) => {
                            if events.send(incoming).is_err() {
                                return;
                            }
                            // Read against the connection and the id the worker
                            // holds now, never the ones captured before the
                            // request: a create replaced the client, and an
                            // attach moved the id.
                            if resets_session
                                && (!report_skills(&events, skills::read(&client, &session_id, false))
                                    || !report_todos(&events, &client, &session_id))
                            {
                                return;
                            }
                        }
                        Err(error) => {
                            let lost = lost_connection(&error);
                            let message = format!("{description}: {error}");
                            let failure = match sent_text {
                                Some(text) => Incoming::SendFailed { text, message, lost },
                                None => Incoming::RequestFailed(message),
                            };
                            if events.send(failure).is_err() {
                                return;
                            }
                        }
                    }
                }
                CommandPoll::Idle => {}
                CommandPoll::Disconnected => return,
            },
        }
    }
}

/// What one pump pass decided for the worker loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Nothing was ready; the caller may block waiting for a command.
    Idle,
    /// At least one event was forwarded, so more may be ready right now.
    Flowing,
    /// The stream failed; [`Incoming::Closed`] has already been sent.
    Closed,
    /// The UI dropped its receiver, so nobody is left to tell.
    UiGone,
}

/// What the client is told when the transport is gone but the stream never
/// reported an error.
const CONNECTION_CLOSED: &str = "the harness connection closed";

/// Forward the events that are already ready, up to [`EVENT_BATCH`].
///
/// One event per pass was the old cost: every pass then waited
/// [`COMMAND_POLL`] on the command queue, so a stream faster than about fifty
/// events a second fell further behind for as long as it lasted, and the SDK's
/// bounded subscription eventually closed with `event_buffer_overflow`. Here
/// the first read may block, which is what keeps an idle client off the CPU,
/// and every read after a hit is a non-blocking poll, so a burst drains at the
/// speed the queue can be written.
///
/// The batch bound is what keeps the pass finite: without it a stream that
/// never pauses would starve the command queue, including the `Cancel` that a
/// user presses during exactly that burst. A stream error is still terminal and
/// still reported as [`Incoming::Closed`].
///
/// A closed connection is not a stream error: the SDK answers `Ok(None)` on a
/// closed stream for ever, so `closed` is asked after every empty read. Without
/// it an idle client would keep saying it is ready and poll a dead channel as
/// fast as the loop goes round.
///
/// The stream is subscribed unfiltered because the daemon rewrites a typed
/// session prefix to the full id: a subscription filtered by what the user typed
/// would drop every event of the session it attached to. The filter therefore
/// lives here, against `session_id`, the id the answer named — borrowed, so an
/// event that is forwarded costs no copy and the check itself allocates
/// nothing.
///
/// `next` is the stream read, generic so the batch can be exercised without a
/// daemon: `Ok(Some(event))` is a frame, `Ok(None)` is a timeout.
fn pump<F, E>(events: &Sender<Incoming>, mut next: F, closed: impl Fn() -> bool, session_id: &str) -> Step
where
    F: FnMut(Duration) -> Result<Option<ApiEvent>, E>,
    E: std::fmt::Display,
{
    let mut timeout = EVENT_POLL;
    let mut forwarded = false;
    for _ in 0..EVENT_BATCH {
        match next(timeout) {
            Ok(Some(event)) => {
                // The read succeeded, so the next one may poll rather than
                // block, whether or not this event is kept.
                timeout = Duration::ZERO;
                if !sessions::belongs(&event, session_id) || matches!(event, ApiEvent::TodoChanged { .. }) {
                    continue;
                }
                if events.send(Incoming::Event(Box::new(event))).is_err() {
                    return Step::UiGone;
                }
                forwarded = true;
            }
            Ok(None) => {
                if closed() {
                    drop(events.send(Incoming::Closed(CONNECTION_CLOSED.to_string())));
                    return Step::Closed;
                }
                break;
            }
            Err(error) => {
                // A dropped receiver means the UI already exited, which is the
                // one case where nobody needs to be told why the stream ended.
                drop(events.send(Incoming::Closed(format!("the event stream ended: {error}"))));
                return Step::Closed;
            }
        }
    }
    if forwarded { Step::Flowing } else { Step::Idle }
}

fn report_todos(events: &Sender<Incoming>, client: &HoronAgentClient, session_id: &str) -> bool {
    if !client.supports("todo_state") || !client.supports(horon_agent_sdk::api::CAPABILITY_LIVE_TODO_V1) {
        return true;
    }
    let incoming = match client.todo_state(session_id) {
        Ok(snapshot) => Incoming::Todos {
            session_id: session_id.to_owned(),
            items: snapshot.todos,
        },
        Err(error) => Incoming::Notice(format!("could not read the todo list: {error}")),
    };
    events.send(incoming).is_ok()
}

/// Send the skill list the client did not ask for, reporting whether the UI is
/// still listening.
///
/// A list read on the client's own behalf is not a command the user gave, so a
/// failure is one notice rather than a refused command: `request_failed` also
/// settles state that belongs to the user's own requests. A harness too old to
/// know the command is silent, which `skills::read` decided by answering `None`.
fn report_skills(events: &Sender<Incoming>, result: Result<Option<Incoming>, Error>) -> bool {
    match result {
        Ok(Some(incoming)) => events.send(incoming).is_ok(),
        Ok(None) => true,
        Err(error) => {
            let notice = Incoming::Notice(format!("could not list the skills: {error}"));
            events.send(notice).is_ok()
        }
    }
}

/// What waiting on the command queue produced.
#[derive(Debug)]
enum CommandPoll {
    Command(Command),
    /// Nothing queued, or the timeout passed with nothing queued.
    Idle,
    /// The UI dropped the sender, so the worker has nothing left to serve.
    Disconnected,
}

/// Take one command, blocking only when the stream is not flowing.
///
/// While events are arriving the check must not sleep, or a burst would still
/// delay a keystroke by the timer; [`pump`]'s batch bound then guarantees the
/// check runs often. When the stream is idle this is where the thread parks,
/// which is why an idle client costs nothing.
fn take_command(commands: &Receiver<Command>, flowing: bool) -> CommandPoll {
    if flowing {
        return match commands.try_recv() {
            Ok(command) => CommandPoll::Command(command),
            Err(mpsc::TryRecvError::Empty) => CommandPoll::Idle,
            Err(mpsc::TryRecvError::Disconnected) => CommandPoll::Disconnected,
        };
    }
    match commands.recv_timeout(COMMAND_POLL) {
        Ok(command) => CommandPoll::Command(command),
        Err(mpsc::RecvTimeoutError::Timeout) => CommandPoll::Idle,
        Err(mpsc::RecvTimeoutError::Disconnected) => CommandPoll::Disconnected,
    }
}

/// Perform one command against the SDK.
///
/// A command that produced something worth reading returns it, so the UI can
/// draw the output. A failure is returned as the SDK error rather than as a
/// sentence: the bridge has to tell "the harness refused this command" apart
/// from "the connection is gone", and only the error kind says which.
///
/// The commands the pickers need carry a structure rather than a line, so they
/// are answered here; the two that change the session are answered by the worker
/// itself, which also moves the connection and the stream they retarget.
fn serve(client: &HoronAgentClient, session_id: &str, command: Command) -> Result<Option<Incoming>, Error> {
    match command {
        Command::Models => {
            let (models, current) = client.list_models(session_id)?;
            Ok(Some(Incoming::Models { models, current }))
        }
        Command::Sessions { working_dir } => {
            let rows = session_rows(client, session_id, working_dir.as_deref())?;
            Ok(Some(Incoming::Sessions(rows)))
        }
        // `/skills` asked for this list, so every failure is shown: a harness
        // that does not know the command is an answer the user can act on,
        // unlike the quiet read the client makes for itself.
        Command::Skills { announce } => skills::read(client, session_id, announce),
        // The effort is answered here rather than in `apply` because the level
        // on the status line comes from the runtime report, not from the set
        // request: the harness stores the level and answers with the state it
        // now reports.
        Command::Effort(level) => runtime::set(client, session_id, &level),
        // A steer answers with the text it carried rather than a line: the
        // client matches that against the messages it is holding, so one
        // refusal restores exactly the messages it was about.
        Command::Steer(text) => Ok(Some(steer::send(client, session_id, &text))),
        Command::Runtime { command, arguments } => runtime::command(client, session_id, command, arguments),
        other => apply(client, session_id, other).map(|message| message.map(Incoming::Info)),
    }
}

/// Perform one command whose result is a line of information.
fn apply(client: &HoronAgentClient, session_id: &str, command: Command) -> Result<Option<String>, Error> {
    match command {
        Command::Text(text) => client.send_message(session_id, &text, Vec::new(), None).map(|_| None),
        Command::Model(name) => client
            .set_model(session_id, &name)
            .map(|()| Some(format!("model set to {name}"))),
        // Answered before they reach this point: they carry a structure rather
        // than a line. The attach retargets the connection the worker holds and
        // a create replaces it, so neither can produce a line here.
        Command::Effort(_)
        | Command::Models
        | Command::Sessions { .. }
        | Command::Skills { .. }
        | Command::Steer(_)
        | Command::Runtime { .. }
        | Command::NewSession { .. }
        | Command::Attach(_) => Ok(None),
        Command::Cancel => client.cancel(session_id).map(|()| None),
        Command::Approval {
            request_id,
            approved,
            always,
        } => {
            let decision = match (approved, always) {
                (true, true) => PermissionDecision::AllowAlways,
                (true, false) => PermissionDecision::Allow,
                (false, _) => PermissionDecision::Deny,
            };
            client
                .respond_to_permission(session_id, &request_id, decision)
                .map(|()| None)
        }
        Command::Ask { ask_id, answers } => client.respond_to_ask(session_id, &ask_id, answers).map(|()| None),
    }
}

/// The typed report a runtime command answers with, when it carries one.
///
/// `/context` is the only command that sends a breakdown today. A payload
/// without the numbers (a daemon older than the field) yields `None`, and the
/// caller shows the message the command sent instead of an empty panel.
fn context_report(command: RuntimeCommand, data: &serde_json::Value) -> Option<ContextReport> {
    match command {
        RuntimeCommand::Context => ContextReport::from_data(data),
        _ => None,
    }
}

/// How a command's failure is worded in the transcript.
fn describe(command: &Command) -> &'static str {
    match command {
        Command::Text(_) => "could not send the message",
        Command::Runtime { .. } => "could not run the command",
        Command::Model(_) => "could not switch the model",
        Command::Effort(_) => "could not set the reasoning effort",
        Command::Models => "could not list the models",
        Command::Sessions { .. } => "could not list the sessions",
        Command::Skills { .. } => "could not list the skills",
        Command::Steer(_) => "could not send the queued messages into the turn",
        Command::NewSession { .. } => "could not start a new session",
        Command::Attach(_) => "could not attach to the session",
        Command::Cancel => "could not cancel the turn",
        Command::Approval { .. } => "could not answer the approval",
        Command::Ask { .. } => "could not answer the question",
    }
}

/// How much of a derived label a row keeps.
const LABEL_CHARS: usize = 80;

/// One row per stored session in the client's directory, newest first.
///
/// The stored history is read once per session: a row's label, when the record
/// carries no title, and its preview both come from those messages, so the list
/// costs one read per row rather than one per thing a row shows.
fn session_rows(client: &HoronAgentClient, current: &str, working_dir: Option<&str>) -> Result<Vec<SessionRow>, Error> {
    let mut sessions = client.list_sessions()?;
    sessions.retain(|session| in_directory(session, working_dir));
    sessions.sort_by_key(|session| std::cmp::Reverse(activity(session)));
    let mut rows = Vec::with_capacity(sessions.len());
    for session in &sessions {
        let messages = sessions::peek(client, &session.session_id);
        let label = match title_of(session) {
            Some(title) => title,
            None => match sessions::first_user_text(&messages) {
                Some(line) => line,
                None => session.session_id.clone(),
            },
        };
        rows.push(SessionRow {
            id: session.session_id.clone(),
            label,
            detail: detail(session),
            current: session.session_id == current,
            preview: sessions::preview(session, &messages),
        });
    }
    Ok(rows)
}

/// The second column of a session row: when it was last written, or what it is
/// doing when it has no timestamp.
pub(super) fn detail(session: &SessionInfo) -> String {
    match activity(session).and_then(moment) {
        Some(time) => time,
        None => session.status.clone(),
    }
}

/// When a session last changed, for the newest-first order.
fn activity(session: &SessionInfo) -> Option<i64> {
    session.updated_at_ms.or(session.last_active_at_ms)
}

/// A stored millisecond timestamp as the local date and time.
///
/// Milliseconds need no leap-second fix: a stored leap keeps them on `:59`.
fn moment(ms: i64) -> Option<String> {
    let Ok(at) = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000) else {
        // Out of the calendar the clock can name: no time, as the old lookup did.
        return None;
    };
    // The offset is read for that instant, so a past daylight-saving period
    // keeps its own wall clock; a host with no zone leaves the record in UTC.
    let offset = match UtcOffset::local_offset_at(at) {
        Ok(offset) => offset,
        Err(_) => UtcOffset::UTC,
    };
    let local = at.to_offset(offset);
    let (month, day, hour, minute) = (u8::from(local.month()), local.day(), local.hour(), local.minute());
    Some(format!("{month:02}-{day:02} {hour:02}:{minute:02}"))
}

/// Whether a session belongs to the directory the client is working in.
///
/// A record that does not name its directory cannot be placed in another one,
/// so it is listed rather than hidden. The comparison goes through the
/// filesystem because the stored path is resolved while the client may have
/// started in a symlinked one.
fn in_directory(session: &SessionInfo, working_dir: Option<&str>) -> bool {
    let Some(wanted) = working_dir else {
        return true;
    };
    match session.working_dir.as_deref() {
        None => true,
        Some(dir) => same_path(dir, wanted),
    }
}

/// Whether two paths name one directory.
fn same_path(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// The title a row shows, when the record has one worth showing.
fn title_of(session: &SessionInfo) -> Option<String> {
    let title = session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())?;
    Some(cut(title))
}

/// A label cut to what a row can carry.
pub(super) fn cut(text: &str) -> String {
    text.chars().take(LABEL_CHARS).collect()
}

/// Whether the failure means the connection itself is gone.
///
/// A refused message leaves the session usable; a closed socket does not, and
/// the client has to know the difference to stop saying it can still send.
fn lost_connection(error: &Error) -> bool {
    matches!(
        error.kind,
        ErrorKind::Disconnected | ErrorKind::Transport | ErrorKind::ConnectFailed { .. }
    )
}

/// The socket path the client would use with no `--socket`.
pub fn default_socket() -> PathBuf {
    horon_agent_sdk::api::api_socket_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connecting_to_a_missing_socket_reports_a_sentence() {
        let missing = std::env::temp_dir().join("horon-tui-bridge-missing.sock");
        let options = ConnectOptions {
            socket_path: Some(missing),
            ensure_runtime: false,
            request_timeout: Some(Duration::from_millis(200)),
            ..ConnectOptions::default()
        };
        let message = match connect(options, None, None) {
            Ok(_) => panic!("a missing socket must not connect"),
            Err(message) => message,
        };
        assert!(!message.is_empty());
    }

    #[test]
    fn default_socket_is_named() {
        assert!(!default_socket().as_os_str().is_empty());
    }

    fn delta(index: usize) -> ApiEvent {
        ApiEvent::TextDelta {
            session_id: "s1".into(),
            text: index.to_string(),
            message_id: Some("m1".into()),
        }
    }

    fn burst(count: usize) -> std::collections::VecDeque<ApiEvent> {
        (0..count).map(delta).collect()
    }

    /// The text of every forwarded event, in the order the UI receives them.
    fn forwarded_text(received: &Receiver<Incoming>) -> Vec<String> {
        received
            .try_iter()
            .map(|incoming| match incoming {
                Incoming::Event(event) => match *event {
                    ApiEvent::TextDelta { text, .. } => text,
                    other => panic!("unexpected event: {other:?}"),
                },
                other => panic!("unexpected incoming: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn one_pass_forwards_a_burst_in_order_up_to_the_batch_bound() {
        let (events, received) = mpsc::channel();
        let mut queue = burst(EVENT_BATCH * 2);

        let step = pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1");

        assert_eq!(step, Step::Flowing);
        let text = forwarded_text(&received);
        assert_eq!(text.len(), EVENT_BATCH, "one pass must stop at the bound");
        let expected: Vec<String> = (0..EVENT_BATCH).map(|index| index.to_string()).collect();
        assert_eq!(text, expected, "the pass reordered or dropped events");

        // The rest arrives on the next pass, in the same order.
        assert_eq!(
            pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1"),
            Step::Flowing
        );
        let text = forwarded_text(&received);
        assert_eq!(text.len(), EVENT_BATCH);
        let expected: Vec<String> = (EVENT_BATCH..EVENT_BATCH * 2).map(|index| index.to_string()).collect();
        assert_eq!(text, expected);
    }

    #[test]
    fn a_burst_never_blocks_a_keystroke_for_a_command_poll() {
        // A command queued before the burst must still be taken as soon as the
        // pass ends, rather than after one COMMAND_POLL per forwarded event.
        let (events, received) = mpsc::channel();
        let (commands, incoming) = mpsc::channel();
        commands.send(Command::Cancel).expect("queue the command");
        let mut queue = burst(EVENT_BATCH * 3);

        assert_eq!(
            pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1"),
            Step::Flowing
        );
        assert_eq!(forwarded_text(&received).len(), EVENT_BATCH);

        match take_command(&incoming, true) {
            CommandPoll::Command(Command::Cancel) => {}
            other => panic!("a queued command was not taken: {other:?}"),
        }
    }

    #[test]
    fn an_idle_pass_waits_on_the_stream_and_forwards_nothing() {
        // With nothing ready the pass must wait for the stream rather than poll
        // it forever; this one timeout is what keeps an idle client off the CPU.
        let (events, received) = mpsc::channel();
        let mut timeouts = Vec::new();
        let step = pump(
            &events,
            |timeout| {
                timeouts.push(timeout);
                Ok::<_, Error>(None)
            },
            || false,
            "s1",
        );

        assert_eq!(step, Step::Idle);
        assert_eq!(timeouts, vec![EVENT_POLL]);
        assert!(forwarded_text(&received).is_empty());
    }

    #[test]
    fn a_burst_polls_instead_of_blocking_after_the_first_event() {
        // Blocking again between events would cap throughput at one event per
        // EVENT_POLL, which is the defect the batch exists to remove.
        let (events, _received) = mpsc::channel();
        let mut queue = burst(3);
        let mut timeouts = Vec::new();

        let step = pump(
            &events,
            |timeout| {
                timeouts.push(timeout);
                Ok::<_, Error>(queue.pop_front())
            },
            || false,
            "s1",
        );

        assert_eq!(step, Step::Flowing);
        assert_eq!(
            timeouts,
            vec![EVENT_POLL, Duration::ZERO, Duration::ZERO, Duration::ZERO],
            "reads after the first event must not block"
        );
    }

    #[test]
    fn a_stream_error_still_ends_the_bridge() {
        let (events, received) = mpsc::channel();

        let step = pump(
            &events,
            |_| Err::<Option<ApiEvent>, _>(Error::new(ErrorKind::EventBufferOverflow, "behind")),
            || false,
            "s1",
        );

        assert_eq!(step, Step::Closed);
        let messages: Vec<Incoming> = received.try_iter().collect();
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            Incoming::Closed(reason) => assert!(reason.contains("behind"), "reason was {reason}"),
            other => panic!("expected a closed bridge, got {other:?}"),
        }
    }

    #[test]
    fn the_ui_dropping_its_receiver_stops_the_worker_quietly() {
        let (events, received) = mpsc::channel();
        drop(received);
        let mut queue = burst(2);

        let step = pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1");

        assert_eq!(step, Step::UiGone, "no receiver is not a failure to report");
    }

    #[test]
    fn a_closed_connection_is_reported_once_without_polling_the_dead_stream() {
        // The SDK answers `Ok(None)` for ever on a closed stream, so without
        // the closed check this pass would return Idle and the worker would go
        // round again: a dead connection must cost exactly one read and one
        // notice, not a spin.
        let (events, received) = mpsc::channel();
        let mut reads = 0;

        let step = pump(
            &events,
            |_| {
                reads += 1;
                Ok::<_, Error>(None)
            },
            || true,
            "s1",
        );

        assert_eq!(step, Step::Closed);
        assert_eq!(reads, 1, "the dead stream was read again");
        let messages: Vec<Incoming> = received.try_iter().collect();
        assert_eq!(messages.len(), 1, "the closed notice was sent more than once");
        match &messages[0] {
            Incoming::Closed(reason) => assert_eq!(reason, CONNECTION_CLOSED),
            other => panic!("expected a closed connection, got {other:?}"),
        }
    }

    #[test]
    fn a_live_connection_that_timed_out_is_not_mistaken_for_a_closed_one() {
        let (events, received) = mpsc::channel();

        let step = pump(&events, |_| Ok::<_, Error>(None), || false, "s1");

        assert_eq!(step, Step::Idle);
        assert!(received.try_iter().next().is_none(), "a timeout is not a disconnection");
    }

    #[test]
    fn an_event_of_another_session_is_not_forwarded() {
        // The subscription is unfiltered because the daemon rewrites a typed
        // prefix to the full id; the pass applies the filter against the id the
        // answer named. An event that names no session (a pong, an error) is
        // about the connection and still reaches the client.
        let (events, received) = mpsc::channel();
        let mut queue = burst(0);
        queue.push_back(ApiEvent::TextDelta {
            session_id: "s2".into(),
            text: "another session".into(),
            message_id: None,
        });
        queue.push_back(ApiEvent::Pong);
        queue.push_back(delta(0));

        let step = pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1");

        assert_eq!(step, Step::Flowing);
        let forwarded: Vec<Incoming> = received.try_iter().collect();
        assert_eq!(forwarded.len(), 2, "a foreign event was forwarded: {forwarded:?}");
        match &forwarded[0] {
            Incoming::Event(event) => {
                assert!(matches!(event.as_ref(), ApiEvent::Pong), "unexpected event: {event:?}");
            }
            other => panic!("unexpected incoming: {other:?}"),
        }
        match &forwarded[1] {
            Incoming::Event(event) => match &**event {
                ApiEvent::TextDelta { session_id, text, .. } => {
                    assert_eq!(session_id.as_str(), "s1");
                    assert_eq!(text.as_str(), "0");
                }
                other => panic!("unexpected event: {other:?}"),
            },
            other => panic!("unexpected incoming: {other:?}"),
        }
    }

    #[test]
    fn a_closed_command_channel_stops_the_worker() {
        let (commands, incoming) = mpsc::channel::<Command>();
        drop(commands);

        assert!(matches!(take_command(&incoming, true), CommandPoll::Disconnected));
        assert!(matches!(take_command(&incoming, false), CommandPoll::Disconnected));
    }

    /// A session record as the daemon serialises one; only the two fields
    /// without a default are required, so this is what a record whose metadata
    /// never filled in looks like on the wire.
    fn record(json: &str) -> SessionInfo {
        serde_json::from_str(json).expect("a session record")
    }

    #[test]
    fn a_session_with_no_metadata_still_reads_as_a_row() {
        let bare = record(r#"{"session_id":"session_ant_1794809866522","status":"idle"}"#);
        assert_eq!(title_of(&bare), None, "a record with no title named one");
        assert_eq!(detail(&bare), "idle", "a record with no time said nothing about itself");
        assert_eq!(activity(&bare), None);
        // It cannot be placed in another directory, so it is not hidden from
        // the list of the directory the client is in.
        assert!(in_directory(&bare, Some("/tmp")));
    }

    #[test]
    fn a_blank_title_is_no_title_and_a_long_one_is_cut() {
        let blank = record(r#"{"session_id":"a","status":"idle","title":"   "}"#);
        assert_eq!(title_of(&blank), None);
        let long = record(&format!(
            r#"{{"session_id":"a","status":"idle","title":"{}"}}"#,
            "t".repeat(LABEL_CHARS + 20)
        ));
        let title = title_of(&long).expect("a titled session");
        assert_eq!(title.chars().count(), LABEL_CHARS);
    }

    #[test]
    fn the_newest_session_comes_first_and_undated_ones_last() {
        let older = record(r#"{"session_id":"older","status":"idle","updated_at_ms":1000}"#);
        let newer = record(r#"{"session_id":"newer","status":"idle","last_active_at_ms":2000}"#);
        let bare = record(r#"{"session_id":"bare","status":"idle"}"#);
        let mut sessions = [bare.clone(), older.clone(), newer.clone()];
        sessions.sort_by_key(|session| std::cmp::Reverse(activity(session)));
        let order: Vec<&str> = sessions.iter().map(|session| session.session_id.as_str()).collect();
        assert_eq!(order, vec!["newer", "older", "bare"]);
    }

    #[test]
    fn a_row_time_reads_as_a_local_moment() {
        let stamped = record(r#"{"session_id":"a","status":"idle","updated_at_ms":1794809866522}"#);
        let detail = detail(&stamped);
        assert_eq!(detail.len(), 11, "the row time is not a date and a time: {detail}");
        assert!(detail.contains(':'), "the row time has no clock: {detail}");
        assert_eq!(moment(1794809866522).as_deref(), Some(detail.as_str()));
    }

    #[test]
    fn a_directory_is_matched_through_another_spelling_and_only_then() {
        let temp = tempfile::tempdir().expect("a directory");
        let real = temp.path().join("work");
        std::fs::create_dir_all(&real).expect("create the directory");
        // The same directory under a different string, on every OS: only
        // resolving both sides can tell that the two are one place.
        let alias = real.join("..").join("work");

        assert!(same_path(
            real.to_str().expect("a path"),
            real.to_str().expect("a path")
        ));
        assert!(!same_path("/one/place", "/another/place"));
        assert!(
            same_path(real.to_str().expect("a path"), alias.to_str().expect("a path")),
            "a working directory spelled another way was treated as another one"
        );

        let here: SessionInfo = serde_json::from_value(serde_json::json!({
            "session_id": "a",
            "status": "idle",
            "working_dir": real.to_str().expect("a path"),
        }))
        .expect("a session record");
        let elsewhere = record(r#"{"session_id":"b","status":"idle","working_dir":"/somewhere/else"}"#);
        let wanted = real.to_str().expect("a path");
        assert!(in_directory(&here, Some(wanted)));
        assert!(!in_directory(&elsewhere, Some(wanted)));
        assert!(
            in_directory(&elsewhere, None),
            "a client with no directory hid sessions"
        );
    }
}
