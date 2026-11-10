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
use horon_agent_sdk::{ConnectOptions, Error, ErrorKind, EventStream, HoronAgentClient};

/// How long the bridge waits for a command before it pumps the event stream
/// again. Short enough that keystrokes feel instant.
const COMMAND_POLL: Duration = Duration::from_millis(20);

/// How long one event read blocks. Long enough that an idle client is not a
/// busy loop; short enough that a command is picked up promptly.
const EVENT_POLL: Duration = Duration::from_millis(30);

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
        match stream.next_result_timeout(EVENT_POLL) {
            Ok(Some(event)) => {
                if events.send(Incoming::Event(Box::new(event))).is_err() {
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                // A dropped receiver means the UI already exited, which is the
                // one case where nobody needs to be told why the stream ended.
                drop(events.send(Incoming::Closed(format!("the event stream ended: {error}"))));
                return;
            }
        }
        match commands.recv_timeout(COMMAND_POLL) {
            Ok(command) => {
                if let Err(message) = apply(&client, &session_id, command)
                    && events.send(Incoming::Notice(message)).is_err()
                {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Perform one command against the SDK.
fn apply(client: &HoronAgentClient, session_id: &str, command: Command) -> Result<(), String> {
    match command {
        Command::Text(text) => client
            .send_message(session_id, &text, Vec::new(), None)
            .map_err(|error| format!("could not send the message: {error}")),
        Command::Cancel => client
            .cancel(session_id)
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
                .map_err(|error| format!("could not answer the approval: {error}"))
        }
        Command::Ask { ask_id, answers } => client
            .respond_to_ask(session_id, &ask_id, answers)
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
}
