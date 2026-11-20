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
    Sessions { working_dir: Option<String> },
    /// Create a session and follow it; the directory is where it starts.
    NewSession { working_dir: Option<String> },
    /// Attach to another session; the bridge follows it from then on.
    Attach(String),
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
    },
    /// A message the harness did not take: the text, the sentence saying why,
    /// and whether the connection itself is gone. The text travels with the
    /// failure so the client can put it back in the input line instead of
    /// losing what the user typed; `lost` lets the client stop pretending it
    /// can still send.
    SendFailed { text: String, message: String, lost: bool },
    /// Output from a command, drawn as information rather than a warning.
    Info(String),
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
    /// The sessions the harness stored, newest first.
    Sessions(Vec<SessionRow>),
    /// The skills the harness reports for the session it serves.
    Skills(Vec<SkillRow>),
    /// The running turn took the text a steer carried, echoed back so the
    /// client knows which held messages it settles.
    Steered { text: String },
    /// A steer the harness refused: the text it carried and the sentence
    /// saying why, so the client can put the messages back in front of the
    /// user instead of losing them.
    SteerFailed { text: String, message: String },
    /// The bridge moved to another session, with its stored conversation.
    Attached(Box<AttachedSession>),
    /// A request the harness answered with an error; the client stops waiting.
    RequestFailed(String),
    /// The connection ended; the string says why.
    Closed(String),
}
