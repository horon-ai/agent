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

use horon_agent_sdk::api::{ApiEvent, AskSelection, PermissionDecision};
use horon_agent_sdk::{ConnectOptions, Error, ErrorKind, EventStream, HoronAgentClient, RuntimeCommand};

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

/// Something the UI wants the harness to do.
///
/// Not `Eq`: an answer carries [`AskSelection`], which only guarantees
/// `PartialEq` because the protocol type does.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Send a user message to the attached session.
    Text(String),
    /// Cancel the running turn.
    Cancel,
    /// Answer a parked approval.
    Approval {
        request_id: String,
        approved: bool,
        always: bool,
    },
    /// Answer a structured question.
    Ask { ask_id: String, answers: Vec<AskSelection> },
    /// Run one of the harness's own commands.
    Runtime {
        command: RuntimeCommand,
        arguments: Vec<String>,
    },
    /// List the models, or switch to the one named.
    Model(Option<String>),
}

/// Something the harness told the UI, or a failure worth showing.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// One protocol event, forwarded unchanged.
    ///
    /// Boxed because a protocol event is an order of magnitude larger than the
    /// two string variants, and every channel slot would otherwise carry the
    /// largest event's footprint.
    Event(Box<ApiEvent>),
    /// A local notice that is not a protocol event.
    Notice(String),
    /// Output from a command, drawn as information rather than a warning.
    Info(String),
    /// The connection ended; the string says why.
    Closed(String),
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
    let client = HoronAgentClient::connect(options).map_err(|error| explain_connect(&error, &socket))?;
    let session = match attach {
        Some(session_id) => client
            .attach_session(session_id)
            .map_err(|error| explain_attach(&error, session_id))?,
        None => client
            .create_session(working_dir.map(str::to_owned))
            .map_err(|error| format!("the harness refused a new session: {error}"))?,
    };
    let session_id = session.session_id.clone();
    let stream = client.events(Some(&session_id));
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let description = format!("{} · {}", socket.display(), session_id);
    let worker_client = client.clone();
    thread::Builder::new()
        .name("horon-tui-bridge".to_string())
        .spawn(move || run_worker(worker_client, stream, session_id, command_rx, event_tx))
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
fn run_worker(
    client: HoronAgentClient,
    stream: EventStream,
    session_id: String,
    commands: Receiver<Command>,
    events: Sender<Incoming>,
) {
    loop {
        match pump(&events, |timeout| stream.next_result_timeout(timeout)) {
            Step::UiGone | Step::Closed => return,
            flowing => match take_command(&commands, flowing == Step::Flowing) {
                CommandPoll::Command(command) => match apply(&client, &session_id, command) {
                    Ok(None) => {}
                    Ok(Some(message)) => {
                        if events.send(Incoming::Info(message)).is_err() {
                            return;
                        }
                    }
                    Err(message) => {
                        if events.send(Incoming::Notice(message)).is_err() {
                            return;
                        }
                    }
                },
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
/// `next` is the stream read, generic so the batch can be exercised without a
/// daemon: `Ok(Some(event))` is a frame, `Ok(None)` is a timeout.
fn pump<F, E>(events: &Sender<Incoming>, mut next: F) -> Step
where
    F: FnMut(Duration) -> Result<Option<ApiEvent>, E>,
    E: std::fmt::Display,
{
    let mut timeout = EVENT_POLL;
    let mut forwarded = false;
    for _ in 0..EVENT_BATCH {
        match next(timeout) {
            Ok(Some(event)) => {
                if events.send(Incoming::Event(Box::new(event))).is_err() {
                    return Step::UiGone;
                }
                forwarded = true;
                timeout = Duration::ZERO;
            }
            Ok(None) => break,
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
/// draw the output; a failure returns the sentence explaining it.
fn apply(client: &HoronAgentClient, session_id: &str, command: Command) -> Result<Option<String>, String> {
    match command {
        Command::Text(text) => client
            .send_message(session_id, &text, Vec::new(), None)
            .map(|_| None)
            .map_err(|error| format!("could not send the message: {error}")),
        Command::Runtime { command, arguments } => client
            .runtime_command(session_id, command, arguments)
            .map(|output| Some(output.message))
            .map_err(|error| format!("could not run the command: {error}")),
        Command::Model(Some(name)) => client
            .set_model(session_id, &name)
            .map(|()| Some(format!("model set to {name}")))
            .map_err(|error| format!("could not switch the model: {error}")),
        Command::Model(None) => client
            .list_models(session_id)
            .map(|(models, current)| {
                let mut message = String::from("models:");
                if models.is_empty() {
                    message.push_str(" the harness reported none");
                }
                for model in &models {
                    message.push_str("\n  ");
                    message.push_str(model);
                    if current.as_deref() == Some(model.as_str()) {
                        message.push_str(" (current)");
                    }
                }
                Some(message)
            })
            .map_err(|error| format!("could not list the models: {error}")),
        Command::Cancel => client
            .cancel(session_id)
            .map(|()| None)
            .map_err(|error| format!("could not cancel the turn: {error}")),
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
                .map_err(|error| format!("could not answer the approval: {error}"))
        }
        Command::Ask { ask_id, answers } => client
            .respond_to_ask(session_id, &ask_id, answers)
            .map(|()| None)
            .map_err(|error| format!("could not answer the question: {error}")),
    }
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

        let step = pump(&events, |_| Ok::<_, Error>(queue.pop_front()));

        assert_eq!(step, Step::Flowing);
        let text = forwarded_text(&received);
        assert_eq!(text.len(), EVENT_BATCH, "one pass must stop at the bound");
        let expected: Vec<String> = (0..EVENT_BATCH).map(|index| index.to_string()).collect();
        assert_eq!(text, expected, "the pass reordered or dropped events");

        // The rest arrives on the next pass, in the same order.
        assert_eq!(pump(&events, |_| Ok::<_, Error>(queue.pop_front())), Step::Flowing);
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

        assert_eq!(pump(&events, |_| Ok::<_, Error>(queue.pop_front())), Step::Flowing);
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
        let step = pump(&events, |timeout| {
            timeouts.push(timeout);
            Ok::<_, Error>(None)
        });

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

        let step = pump(&events, |timeout| {
            timeouts.push(timeout);
            Ok::<_, Error>(queue.pop_front())
        });

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

        let step = pump(&events, |_| {
            Err::<Option<ApiEvent>, _>(Error::new(ErrorKind::EventBufferOverflow, "behind"))
        });

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

        let step = pump(&events, |_| Ok::<_, Error>(queue.pop_front()));

        assert_eq!(step, Step::UiGone, "no receiver is not a failure to report");
    }

    #[test]
    fn a_closed_command_channel_stops_the_worker() {
        let (commands, incoming) = mpsc::channel::<Command>();
        drop(commands);

        assert!(matches!(take_command(&incoming, true), CommandPoll::Disconnected));
        assert!(matches!(take_command(&incoming, false), CommandPoll::Disconnected));
    }
}
