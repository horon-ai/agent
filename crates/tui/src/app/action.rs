//! What the UI asks the bridge to do after a keystroke.
//!
//! Kept beside `app` rather than inside it because the enum is read by the
//! frame loop and every command handler, while `app.rs` is the state those
//! handlers change and has a size budget of its own.

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
    /// Start a new session in this working directory and follow it.
    NewSession,
    /// Attach to another session and replay its stored conversation.
    Attach(String),
    /// Save a title for the attached session.
    Rename(String),
    /// Ask the harness for the skills it reports for the session.
    Skills,
    /// Ask for the background work this session is still running.
    BackgroundJobs,
    /// Ask which noted sessions were deleted, including checks for archived sessions.
    CheckSessions(Vec<String>),
    /// Hand the draft to the user's editor; the loop answers this one itself,
    /// because the editor needs the terminal the loop is drawing on.
    EditDraft,
    /// Give the terminal to the editor with a private copy of a resolved artifact.
    OpenArtifact(std::path::PathBuf),
    /// Ask the daemon for the values the settings screen displays.
    DaemonSettings,
    /// Leave.
    Quit,
}
