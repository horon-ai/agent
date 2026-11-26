use horon_agent_sdk::RuntimeCommand;
use horon_agent_sdk::api::{ApiEvent, AskSelection};

use super::{AttachedSession, SessionRow, SkillRow};
use crate::context::ContextReport;

/// Something the UI wants the harness to do.
///
/// Not `Eq`: an answer carries [`AskSelection`], which only guarantees
/// `PartialEq` because the protocol type does.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Send a user message to the attached session, with the images its
    /// `[Image #N]` markers name as `(media type, base64)`.
    Text(String, Vec<(String, String)>),
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
    /// Run one command an installed package supplies.
    ///
    /// `display` is the line the user typed, carried so a template's expansion
    /// is drawn as that line rather than as the paragraph it expanded to, the
    /// same way a command file's is.
    PackageCommand {
        package_id: String,
        command: String,
        arguments: String,
        display: String,
    },
    /// Read stored token usage for the attached session.
    Stats,
    /// Read the attached session's details: identity, model, messages, tokens.
    SessionInfo,
    /// Clear or compact the model context of the session the bridge follows.
    Context(crate::commands::ContextCommand),
    /// Switch to the model named.
    ///
    /// The list the picker shows is its own command: `/model` with no argument
    /// asks for `Command::Models`, which `serve` answers with a structure, so a
    /// name is the only thing this arm carries.
    Model(String),
    /// Set how much the model deliberates before answering.
    ///
    /// The level is a name from the client's ladder rather than a type: which
    /// levels a provider has is its own decision, and one it does not have is
    /// refused with an error the transcript shows.
    Effort(String),
    /// List the models the picker offers, with the one in force.
    Models,
    /// List the sessions stored for a working directory.
    Sessions {
        working_dir: Option<String>,
        archived: bool,
    },
    /// Move a stored session between recent and archived lists.
    Archive { session_id: String, archived: bool },
    /// List the attached session's enabled tools without running any.
    Tools,
    /// Ask which of these session ids the harness no longer stores.
    ///
    /// The client asks per id rather than reading one list, because a list
    /// leaves archived sessions out and an archived session is not deleted:
    /// its notes must survive. Any failure other than an unknown id leaves the
    /// session out of the answer, so a harness that cannot be asked keeps the
    /// notes rather than losing them.
    CheckSessions(Vec<String>),
    /// Create a session and follow it; the directory is where it starts.
    /// `delete` names the session just left, deleted once the new one is
    /// attached (a daemon refuses to delete a session a client is attached to).
    NewSession {
        working_dir: Option<String>,
        delete: Option<String>,
    },
    /// Attach to another session; the bridge follows it from then on.
    Attach(String),
    /// Fork the attached session into a read-only child and ask it a question.
    ///
    /// The fork and the question run on a second connection, so the worker
    /// keeps following the parent: the parent's stream, transcript and tools
    /// are untouched while the child answers.
    SideQuestion {
        id: u64,
        question: String,
        boundary: Option<String>,
    },
    /// List the recorded boundaries this session can fork from.
    Forks,
    /// Fork a new session from the named stored message and follow it.
    Fork(String),
    /// Save a title for the session the bridge follows.
    Rename(String),
    /// List the background work the attached session is still running.
    BackgroundJobs,
    /// Write the attached session's transcript to this path as an HTML report.
    ///
    /// The document is the harness's renderer output, not the transcript the
    /// client drew: that is what keeps the report in step with the Markdown
    /// export, redaction and truncation included.
    Export { path: String },
    /// Stop one background terminal this session started.
    ///
    /// It names the tool call, not the turn: the session and every other job
    /// keep running, and the daemon answers a job that is not a terminal with
    /// `not_cancellable` rather than stopping something else.
    CancelBackgroundTerminal(String),
    /// List the skills the harness reports for the session.
    ///
    /// `announce` is the difference between a list the user asked for and one
    /// the client reads for itself: `/skills` shows every failure, while the
    /// list read after a connect or a session change stays quiet for a harness
    /// too old to know the command and reports any other failure once.
    Skills { announce: bool },
    /// Send the queued messages into the running turn as one soft interrupt.
    ///
    /// The messages arrive as one body rather than as a new turn, so the model
    /// reads them mid-turn; the answer echoes the text so the client settles
    /// exactly the messages it held.
    Steer(String),
    /// Continue the last failed run of the attached session.
    ///
    /// The run id is not carried here: only the daemon knows which run ended
    /// last and whether it may still be continued, so the bridge reads
    /// `last_stop` from the runtime state and asks it to continue that. A
    /// client-side id would be a guess, and the guess is exactly what a
    /// repeated click would turn into a second turn.
    Retry,

    /// Read the daemon values the settings screen displays.
    ///
    /// The rows are the daemon's answer to a request this client never composes
    /// itself, so there is nothing here for a caller to name: the session the
    /// worker follows is the one asked about.
    DaemonSettings,
    /// Change one todo item's status only if the stored list is still the
    /// revision the user saw.
    ///
    /// The session travels with it because the answer belongs to the list it
    /// named: a command that outlived an attach must not edit the session the
    /// worker moved to.
    SetTodoStatus {
        session_id: String,
        expected_revision: String,
        id: String,
        status: String,
    },
    /// List the swarm children of the session the worker follows, and the
    /// parent it reports back to.
    ///
    /// The rows are narrowed to that lineage by the bridge; a caller names
    /// nothing because the session in force is the only swarm asked about.
    Swarm,
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
    /// The current todo snapshot for the named session.
    Todos {
        session_id: String,
        items: Vec<horon_agent_sdk::todo::TodoItem>,
        /// The revision of the stored list these items came from, absent on a
        /// harness that does not report one.
        revision: Option<String>,
    },
    /// The answer to one revision-checked todo status change.
    ///
    /// The list and revision travel with it whether or not the write landed, so
    /// a stale editor refreshes from this one frame instead of asking again.
    TodoEdit {
        session_id: String,
        applied: bool,
        refusal: Option<horon_agent_sdk::todo::TodoEditRefusal>,
        items: Vec<horon_agent_sdk::todo::TodoItem>,
        revision: String,
        id: String,
        status: String,
    },
    /// A message the harness did not take: the text, the sentence saying why,
    /// and whether the connection itself is gone. The text travels with the
    /// failure so the client can put it back in the input line instead of
    /// losing what the user typed; `lost` lets the client stop pretending it
    /// can still send.
    SendFailed { text: String, message: String, lost: bool },
    /// Output from a command, drawn as information rather than a warning.
    Info(String),
    /// The `/session` report, drawn as a panel over the conversation.
    SessionInfo(String),
    /// The `/context` breakdown, drawn as a panel rather than its JSON text.
    Context(Box<ContextReport>),
    /// Another runtime command's structured answer, drawn as labelled rows.
    ///
    /// Only a command whose message repeats its payload as JSON gets here; one
    /// that reported in prose arrives as [`Incoming::Info`].
    Report(Box<crate::report::Report>),
    /// The models the harness offers, and the one serving the session.
    Models {
        models: Vec<String>,
        current: Option<String>,
    },
    /// Effective definitions for the attached session.
    Tools(Vec<horon_agent_sdk::api::SessionToolDefinition>),
    /// What the harness reports about the session it serves.
    ///
    /// `provider`, `model` and `effort` are what the status line shows, and each
    /// is absent when the harness does not name it. `level` is the reasoning
    /// effort this answer confirms, when it answers a request to set one: it
    /// travels with the answer rather than in client state, so a report that
    /// crossed a refusal cannot make the client claim a level the harness never
    /// took.
    RuntimeInfo {
        provider: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        level: Option<String>,
    },
    /// The daemon values a settings screen may display, or why it could not
    /// report them.
    DaemonSettings(crate::settings::DaemonSettings),
    /// The sessions the harness stored, newest first.
    Sessions(Vec<SessionRow>),
    /// The recorded message boundaries this session can fork from, newest
    /// first. Each row names a stored message id, not an index.
    Forks(Vec<super::ForkRow>),
    /// The swarm children of the attached session, and the parent row after
    /// them, already narrowed to that lineage by the bridge.
    Swarm(Vec<super::SwarmRow>),
    /// Only the archived sessions, newest first.
    ArchivedSessions(Vec<SessionRow>),
    /// Noted sessions that no longer exist, and how many could not be checked.
    SessionsChecked { gone: Vec<String>, unverified: usize },
    /// A requested retry ran nothing, and the sentence saying why.
    ///
    /// Separate from a notice because the press already claimed the turn on
    /// screen: the refusal has to give that claim back, or the client would keep
    /// saying a turn runs while nothing does.
    RetryRefused(String),
    /// The skills the harness reports for the session it serves.
    Skills(Vec<SkillRow>),
    /// The slash commands the installed packages supply for that session.
    ///
    /// Replaces the whole list, like [`Self::Skills`]: the answer names one
    /// session's packages, so a name the session left behind supplied stops
    /// resolving the moment this arrives.
    PackageCommands(Vec<crate::commands::PackageCommand>),
    /// One package command's answer: a template's message or an executable's
    /// output, with the line the user typed.
    PackageCommand(Box<super::PackageOutcome>),
    /// The background work the session is still running.
    BackgroundJobs(Vec<super::BackgroundJobRow>),
    /// One cancellation's verdict, and the list as it stands after it.
    ///
    /// The reply to a cancellation only says a stop was started, so the list
    /// that travels beside it is what says whether the job is gone. A refresh
    /// that could not be read travels as its sentence: the verdict still shows,
    /// the rows already on screen stay, and the client says why they are stale.
    BackgroundJobCancelled {
        call_id: String,
        accepted: bool,
        reason: Option<String>,
        jobs: Result<Vec<super::BackgroundJobRow>, horon_agent_sdk::Error>,
    },
    /// The running turn took the text a steer carried, echoed back so the
    /// client knows which held messages it settles.
    Steered { text: String },
    /// A steer the harness refused: the text it carried and the sentence
    /// saying why, so the client can put the messages back in front of the
    /// user instead of losing them.
    SteerFailed { text: String, message: String },
    /// What the runtime reported about the session's spending.
    ///
    /// Read when the client moves to a session and again when the model moves,
    /// never per frame: the report is a request, and the status line renders far
    /// faster than a socket round trip.
    Usage(crate::statusline::UsageSnapshot),
    /// The bridge moved to another session, with its stored conversation and
    /// the directory the harness reports for it.
    ///
    /// The directory travels beside the session record rather than inside it:
    /// it decides which folder a draft completes, and the client applies it in
    /// the handler that receives the switch.
    Attached {
        session: Box<AttachedSession>,
        working_dir: Option<String>,
    },
    /// A request the harness answered with an error; the client stops waiting.
    RequestFailed(String),
    /// The side session exists and is read-only; its id is the identity a
    /// later `/switch` reattaches by.
    SideStarted { id: u64, session_id: String },
    /// The answer as the child's stream has built it so far.
    SideAnswer { id: u64, answer: String },
    /// What the child is doing, on one line.
    SideProgress { id: u64, line: String },
    /// Something about the side turn worth keeping in view. The answer still
    /// arrives separately.
    SideNotice { id: u64, line: String },
    /// The turn ended; `answer` is the stored answer, not the streamed preview.
    SideDone { id: u64, answer: String },
    /// The side question could not be started or answered. A failure after
    /// `SideStarted` leaves the child stored, and its id is on the overlay.
    SideFailed { id: u64, message: String },
    /// The connection ended; the string says why.
    Closed(String),
}

/// How a command's failure is worded in the transcript.
pub(super) fn describe(command: &Command) -> &'static str {
    match command {
        Command::Text(..) => "could not send the message",
        Command::Runtime { .. } => "could not run the command",
        Command::PackageCommand { .. } => "could not run the package command",
        Command::Stats => "could not read session token usage",
        Command::SessionInfo => "could not read the session details",
        Command::Context(crate::commands::ContextCommand::Clear) => "could not clear model context",
        Command::Context(crate::commands::ContextCommand::Compact) => "could not compact model context",
        Command::Model(_) => "could not switch the model",
        Command::Effort(_) => "could not set the reasoning effort",
        Command::Models => "could not list the models",
        Command::Sessions { .. } => "could not list the sessions",
        Command::Tools => "could not list the session tools",
        Command::Forks => "could not list the recorded messages",
        Command::Archive { archived: true, .. } => "could not archive the session",
        Command::Archive { archived: false, .. } => "could not restore the session",
        Command::CheckSessions(_) => "could not check the noted sessions",
        Command::Swarm => "could not list the swarm children",
        Command::Skills { .. } => "could not list the skills",
        Command::BackgroundJobs => "could not list the background jobs",
        Command::Export { .. } => "could not write the HTML report",
        Command::CancelBackgroundTerminal(_) => "could not cancel the background terminal",
        Command::DaemonSettings => "could not read the daemon settings",
        Command::Steer(_) => "could not send the queued messages into the turn",
        Command::SetTodoStatus { .. } => "could not change the todo list",
        Command::NewSession { .. } => "could not start a new session",
        Command::Attach(_) => "could not attach to the session",
        Command::SideQuestion { .. } => "could not start the side question",
        Command::Fork(_) => "could not fork the session",
        Command::Rename(_) => "could not rename the session",
        Command::Cancel => "could not cancel the turn",
        Command::Retry => "could not continue the failed turn",
        Command::Approval { .. } => "could not answer the approval",
        Command::Ask { .. } => "could not answer the question",
    }
}
