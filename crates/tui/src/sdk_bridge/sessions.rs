//! The session flows of the bridge: creating one, attaching to one, and the
//! rows and previews the session list shows.
//!
//! These sit beside the worker loop rather than inside it for one reason: the
//! loop is about staying responsive, while everything here is one request in,
//! one answer out. None of it touches the UI queue, so a caller only has to
//! decide what a failure means for the client.

use std::path::PathBuf;
use std::time::Duration;

use horon_agent_sdk::api::ApiEvent;
use horon_agent_sdk::{ConnectOptions, Error, EventStream, HistoryMessage, HoronAgentClient, SessionInfo};

use super::{AttachedSession, cut, detail, runtime};

/// How to reach the harness, kept after the first connection took the options.
///
/// One connection gets one session from its first create: the daemon answers a
/// second create on a connection with the session it already has
/// (`crates/harness-api-server/src/translate.rs`, the `create_session |
/// attach_session` arm). A new session therefore needs a connection of its own,
/// and dialling it needs the settings the first connection consumed.
pub(super) struct Dial {
    socket_path: Option<PathBuf>,
    client_name: String,
    request_timeout: Option<Duration>,
    ensure_runtime: bool,
    supports_user_response: bool,
    supports_ask_requests: bool,
}

impl Dial {
    /// The settings a connection was made with.
    pub(super) fn of(options: &ConnectOptions) -> Self {
        Self {
            socket_path: options.socket_path.clone(),
            client_name: options.client_name.clone(),
            request_timeout: options.request_timeout,
            ensure_runtime: options.ensure_runtime,
            supports_user_response: options.supports_user_response,
            supports_ask_requests: options.supports_ask_requests,
        }
    }

    /// Options for one more connection to the same harness.
    pub(super) fn options(&self) -> ConnectOptions {
        ConnectOptions {
            socket_path: self.socket_path.clone(),
            client_name: self.client_name.clone(),
            request_timeout: self.request_timeout,
            ensure_runtime: self.ensure_runtime,
            supports_user_response: self.supports_user_response,
            supports_ask_requests: self.supports_ask_requests,
        }
    }
}

/// How many stored messages are read to find a label and a preview.
///
/// Enough to cover the six a preview shows plus the first message a label is
/// derived from, which are the two ends of a short conversation; a session
/// whose record never filled in a title is labelled from these same messages,
/// so the list costs one read per row either way.
const PEEK_MESSAGES: u32 = 20;

/// How many stored messages one preview carries.
const PREVIEW_MESSAGES: usize = 6;

/// Longest preview line, in characters.
const PREVIEW_CHARS: usize = 200;

/// What a preview's first line says when the session never named a directory.
const NO_DIRECTORY: &str = "(no directory)";

/// The tag the daemon wraps the context it injects in.
///
/// Those messages are stored under the user role, so a label or a preview that
/// read them would name a conversation after a system block the user never
/// wrote.
const INJECTED: &str = "<system-reminder>";

/// Create a session on a connection of its own and read it back.
///
/// The new client and the stream it follows come back with the answer because
/// the worker has to move to both: the create is what gives a connection its
/// session, so the session cannot be created on the connection the client is
/// already using. The caller drops the old client, which closes the old
/// connection; the session that connection served stays stored and resumable,
/// which is what `/switch` back relies on.
///
/// The stream is subscribed before the request, and unfiltered. The daemon
/// answers a create with a catalog probe, and that answer is the only
/// `ModelInfo` (provider, model, reasoning effort) the new session ever gets; a
/// stream opened afterwards would drop it and the status line would stay blank.
/// Unfiltered is right because this connection serves exactly the one session it
/// creates, and because the id it will have is not known until the create
/// answers.
///
/// The history is empty rather than read: a session the daemon just created has
/// stored no messages, and a history read would answer the same thing for one
/// request more.
pub(super) fn created(
    dial: &Dial,
    working_dir: Option<&str>,
    socket: &str,
) -> Result<(HoronAgentClient, EventStream, AttachedSession), Error> {
    let client = HoronAgentClient::connect(dial.options())?;
    let stream = client.events(None);
    let session = client.create_session(working_dir.map(str::to_owned))?;
    let (model, effort) = runtime::identity(&client, &session.session_id);
    Ok((client, stream, described(session, Vec::new(), model, effort, socket)))
}

/// Attach to a session and read back what the client has to show for it.
///
/// The stored conversation is read after the attach, so it is the one the
/// connection now follows.
pub(super) fn resumed(client: &HoronAgentClient, session_id: &str, socket: &str) -> Result<AttachedSession, Error> {
    let session = client.attach_session(session_id)?;
    // From here the connection already follows `session`, so a read that fails
    // cannot be the answer's failure: the caller moves the worker with
    // `follow` only once this returns, and an error would leave it filtering
    // events and naming requests with the session the daemon just left. The
    // conversation shows empty instead.
    let history = history_or_empty(client.get_history(&session.session_id));
    let (model, effort) = runtime::identity(client, &session.session_id);
    Ok(described(session, history, model, effort, socket))
}

/// The stored conversation an attach reports when the read itself failed.
///
/// Once the attach answered, the daemon has this connection on the new
/// session, and nothing here can move it back. The read's error is spent as an
/// empty conversation rather than as the answer's failure.
pub(super) fn history_or_empty<E>(read: Result<Vec<HistoryMessage>, E>) -> Vec<HistoryMessage> {
    let Ok(messages) = read else {
        return Vec::new();
    };
    messages
}

/// Move the worker to the session a change answered with.
///
/// The id and the stream always move together: a send, a cancel or an answer
/// names the session the connection follows, and the events of the session just
/// left must stop arriving for it. `next` is the stream the caller subscribed
/// before the request left, which is what catches the identity event the daemon
/// publishes while the request is in flight; the id comes from the answer, not
/// from the request, because the daemon's record is the one that counts.
///
/// Generic over the stream so the move can be exercised without a daemon.
pub(super) fn follow<S>(attached: &AttachedSession, next: S, session_id: &mut String, stream: &mut S) {
    *stream = next;
    *session_id = attached.session_id.clone();
}

/// The session an event belongs to, when it names one.
///
/// Exhaustive on purpose, with no `_` arm: a variant added to the protocol later
/// has to be classified here, instead of being forwarded to the wrong session or
/// dropped by a wildcard nobody revisits.
pub(super) fn session_of(event: &ApiEvent) -> Option<&str> {
    use ApiEvent::*;
    match event {
        RuntimeCommandResult { session_id, .. }
        | History { session_id, .. }
        | TextDelta { session_id, .. }
        | TextDone { session_id, .. }
        | TextReplace { session_id, .. }
        | ReasoningDelta { session_id, .. }
        | ReasoningDone { session_id, .. }
        | Tools { session_id, .. }
        | ToolCall { session_id, .. }
        | ToolStart { session_id, .. }
        | ToolInputDelta { session_id, .. }
        | ToolExec { session_id, .. }
        | ToolDone { session_id, .. }
        | SidePaneImages { session_id, .. }
        | ToolArgsRepaired { session_id, .. }
        | TokenUsage { session_id, .. }
        | KvCacheMiss { session_id, .. }
        | TurnStopped { session_id, .. }
        | TurnDone { session_id, .. }
        | SoftInterruptInjected { session_id, .. }
        | WakeRequested { session_id, .. }
        | BackgroundProgress { session_id, .. }
        | MessageAccepted { session_id }
        | PermissionRequest { session_id, .. }
        | ApprovalPending { session_id, .. }
        | ApprovalResolved { session_id, .. }
        | FailoverSwitched { session_id, .. }
        | RunNeedsHumanResume { session_id, .. }
        | AskRequested { session_id, .. }
        | SessionRecovery { session_id, .. }
        | SessionStatus { session_id, .. }
        | ConnectionPhase { session_id, .. }
        | ModelInfo { session_id, .. }
        | Models { session_id, .. }
        | RuntimeInfo { session_id, .. }
        | FileContent { session_id, .. }
        | Files { session_id, .. }
        | TextMatches { session_id, .. }
        | FileStatus { session_id, .. }
        | TodoState { session_id, .. }
        | TodoChanged { session_id }
        | Artifacts { session_id, .. }
        | Compacted { session_id, .. }
        | SessionRenamed { session_id, .. }
        | SessionHeads { session_id, .. }
        | SessionHistoryPage { session_id, .. }
        | SessionMarkdown { session_id, .. }
        | SessionEventPage { session_id, .. }
        | SessionImportResult { session_id, .. }
        | SessionBlobChunk { session_id, .. }
        | SessionBlobStatus { session_id, .. }
        | SessionRecordBlobStatus { session_id, .. }
        | RuntimeState { session_id, .. }
        | ControlReceipt { session_id, .. }
        | SnapshotMarker { session_id, .. }
        | Snapshot { session_id, .. }
        | CancelTurnResult { session_id, .. }
        | ToolCallCancelResult { session_id, .. }
        | BackgroundWorkList { session_id, .. }
        | MessageModes { session_id, .. } => Some(session_id.as_str()),
        Attached { session } | SessionForked { session } => Some(session.session_id.as_str()),
        // A report narrowed to one session names it; a full report names none,
        // so it belongs to the connection rather than to a conversation.
        UsageReport { session_id, .. } => session_id.as_deref(),
        HelloOk { .. }
        // Qualified because the prelude's `Result::Ok` has the same name; the
        // rest of the arm list comes from the import above.
        | ApiEvent::Ok
        | Error { .. }
        | Sessions { .. }
        | Pong
        | DegradationReceipts(_)
        | SoftInterruptsCancelled { .. }
        | BackgroundToolResult { .. }
        | ApprovalAck { .. }
        | FailoverAck { .. }
        | CredentialUpdated { .. }
        | SessionSearchResults { .. }
        | Unknown => None,
    }
}

/// Whether an event of the connection belongs to the session the worker follows.
///
/// An event that names no session — an error, a handshake, a session list — is
/// about the connection and is always forwarded.
pub(super) fn belongs(event: &ApiEvent, session_id: &str) -> bool {
    session_of(event).is_none_or(|named| named == session_id)
}

/// A session the bridge moved to, in the shape the client shows.
pub(super) fn described(
    session: SessionInfo,
    history: Vec<HistoryMessage>,
    model: Option<String>,
    effort: Option<String>,
    socket: &str,
) -> AttachedSession {
    let title = session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string);
    AttachedSession {
        description: format!("{socket} · {}", session.session_id),
        session_id: session.session_id,
        status: session.status,
        model,
        effort,
        history,
        title,
    }
}

/// The stored messages of a session, newest last, or none when nothing answers.
///
/// Two reads exist because the daemon stores two shapes. A current session is a
/// folder whose conversation only the history page reports, while a single-file
/// record written by an earlier release and a daemon without the
/// `session_history_cursor_v1` capability are only reachable through the tail
/// (`peek_session`). The page is asked first, and the tail answers when the page
/// fails or is empty; a session neither read can answer shows its title, or its
/// id when it has none, and a head-only preview.
pub(super) fn peek(client: &HoronAgentClient, session_id: &str) -> Vec<HistoryMessage> {
    let page = client
        .get_history_page(session_id, None, None, Some(PEEK_MESSAGES))
        .map(|page| page.messages);
    messages(page, || client.peek_session(session_id, Some(PEEK_MESSAGES)))
}

/// The newest stored messages, preferring the history page.
///
/// Split from the two requests so the order can be exercised without a daemon:
/// `tail` is asked only when the page failed or held no message, and each error
/// is spent in the one place that can use the other read rather than dropped.
pub(super) fn messages<P, T, F>(page: Result<Vec<HistoryMessage>, P>, tail: F) -> Vec<HistoryMessage>
where
    F: FnOnce() -> Result<Vec<HistoryMessage>, T>,
{
    if let Ok(messages) = page
        && !messages.is_empty()
    {
        return messages;
    }
    let Ok(messages) = tail() else {
        return Vec::new();
    };
    messages
}

/// What the user said first in a session, from the messages already read.
pub(super) fn first_user_text(messages: &[HistoryMessage]) -> Option<String> {
    messages
        .iter()
        .filter(|message| message.role == "user" && !injected(message))
        .find_map(|message| {
            message
                .content
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(cut)
        })
}

/// Whether a stored message is context the daemon injected rather than speech.
fn injected(message: &HistoryMessage) -> bool {
    message.content.trim_start().starts_with(INJECTED)
}

/// The lines the picker shows for one session.
///
/// The first line names the directory and the last write, because a row's
/// second column is a time alone: two sessions in different projects can carry
/// the same clock reading. The rest is the conversation's own last words, which
/// is what tells one session from another when the titles are similar.
pub(super) fn preview(session: &SessionInfo, messages: &[HistoryMessage]) -> Vec<String> {
    let directory = session.working_dir.as_deref().unwrap_or(NO_DIRECTORY);
    let mut lines = Vec::with_capacity(PREVIEW_MESSAGES + 1);
    lines.push(format!("{directory} · {}", detail(session)));
    // The newest messages, oldest first: the tail of a conversation says what
    // it was about, and the head is already in the label.
    let mut body: Vec<String> = messages
        .iter()
        .rev()
        .filter_map(turn_line)
        .take(PREVIEW_MESSAGES)
        .collect();
    body.reverse();
    lines.extend(body);
    lines
}

/// One stored message as a preview line, for the two roles a reader speaks in.
fn turn_line(message: &HistoryMessage) -> Option<String> {
    let who = match message.role.as_str() {
        "user" if injected(message) => return None,
        "user" => "you",
        "assistant" => "assistant",
        _ => return None,
    };
    let text = single_line(&message.content);
    if text.is_empty() {
        return None;
    }
    Some(format!("{who}: {text}"))
}

/// One line of a stored message, cut to what a preview can show.
///
/// A stored message may hold a whole file's worth of lines and escape
/// sequences, and a preview that carried them would be drawn as a paragraph
/// the list never planned for. The cut counts characters rather than bytes, so
/// a line that is three bytes a character does not end in half of one.
fn single_line(text: &str) -> String {
    // At most four bytes per kept character, and one space between words.
    let mut line = String::with_capacity(text.len().min(PREVIEW_CHARS * 5));
    let mut kept = 0;
    let mut gap = false;
    for character in text.chars() {
        if character.is_control() || character.is_whitespace() {
            gap = true;
            continue;
        }
        if kept == PREVIEW_CHARS {
            break;
        }
        if gap && kept > 0 {
            line.push(' ');
        }
        gap = false;
        line.push(character);
        kept += 1;
    }
    line
}
