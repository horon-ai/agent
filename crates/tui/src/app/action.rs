//! What a keystroke asks the bridge to do.
//!
//! The list lives apart from [`super::App`] so the action boundary — the one
//! shape every key, command and picker produces — stays readable on its own.
//! It is re-exported from `crate::app`, so callers keep naming `Action` there.
use horon_agent_sdk::api::AskSelection;

/// One request for the bridge thread: the client's half of a command.
///
/// Not `Eq`: an answer carries protocol selections, which only compare with
/// `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send this text as a user message.
    Send(String),
    /// Send this text into the running turn as one soft interrupt.
    ///
    /// It carries the queued messages joined for the harness; what the
    /// transcript shows is each message's own row, settled when the bridge
    /// answers.
    Steer(String),
    /// Cancel the running turn.
    Cancel,
    /// Cancel one background terminal this session started.
    ///
    /// It names the tool call that started the job, not the turn: the session
    /// and every other job keep running.
    CancelBackgroundTerminal(String),
    /// Answer a parked approval.
    Approval {
        request_id: String,
        approved: bool,
        always: bool,
    },
    /// Answer a structured question.
    Ask { ask_id: String, answers: Vec<AskSelection> },
    /// Ask the harness to run one of its own commands.
    Runtime {
        command: horon_agent_sdk::RuntimeCommand,
        arguments: Vec<String>,
    },
    /// Ask the harness to run one command an installed package supplies.
    ///
    /// `display` is the line the user typed: a template's expansion is drawn as
    /// that line, and an executable command's report needs no line at all.
    PackageCommand {
        package_id: String,
        command: String,
        arguments: String,
        display: String,
    },
    /// Read the attached session's token usage.
    Stats,
    /// Clear or compact the model context on the daemon.
    Context(crate::commands::ContextCommand),
    /// Switch to the model named.
    Model(String),
    /// Set the reasoning effort to the level named.
    Effort(String),
    /// Ask for the model list the picker offers.
    ///
    /// The answer fills an open list and is also the catalog a cycle checks
    /// `models.shortlist` against, so a cycle pressed before the list was read
    /// asks for it the same way and opens nothing.
    Models,
    /// Ask for the sessions stored for this working directory.
    Sessions,
    /// Ask for the swarm children of the attached session, and its parent row.
    Swarm,
    /// Ask for the sessions stored for this working directory, archived ones included.
    ArchivedSessions,
    /// Archive the session named.
    Archive(String),
    /// Restore the archived session named.
    Restore(String),
    /// Ask for the recorded message boundaries this session can fork from.
    Forks,
    /// Fork a new session from the named stored message and follow it.
    Fork(String),
    /// Start a new session in this working directory and follow it.
    NewSession,
    /// Attach to another session and replay its stored conversation.
    Attach(String),
    /// Ask a read-only child of this conversation a question.
    SideQuestion {
        id: u64,
        question: String,
        boundary: Option<String>,
    },
    /// Save a title for the attached session.
    Rename(String),
    /// Ask the harness for the skills it reports for the session.
    Skills,
    /// Ask the harness for the tools enabled for the session.
    Tools,
    /// Ask for the background work this session is still running.
    BackgroundJobs,
    /// Ask which noted sessions were deleted, including checks for archived sessions.
    CheckSessions(Vec<String>),
    /// Hand the draft to the user's editor; the loop answers this one itself,
    /// because the editor needs the terminal the loop is drawing on.
    EditDraft,
    /// Give the terminal to the editor with a private copy of a resolved artifact.
    OpenArtifact(std::path::PathBuf),
    /// Write the attached session's transcript as a standalone HTML report.
    ///
    /// The path is the destination the reader typed, kept exactly as typed so
    /// a relative one resolves against the directory the client runs in.
    Export(String),
    /// Ask the daemon for the values the settings screen displays.
    DaemonSettings,
    /// Commit one revision-checked todo status change for the attached session.
    TodoStatus(crate::todo_panel::TodoStatusRequest),
    /// Leave.
    Quit,
}
