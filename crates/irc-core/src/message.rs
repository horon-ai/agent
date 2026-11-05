//! Message types exchanged on the bus.

use futures::future::BoxFuture;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// Shared, cheaply cloned session identifier.
pub type AgentId = Arc<str>;

/// Identity of one `send` call, shared by all of its delivery legs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(pub u64);

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One delivered message, with the per-leg ordering sequence.
#[derive(Clone, Debug)]
pub struct IrcMessage {
    pub id: MessageId,
    pub seq: u64,
    pub from: AgentId,
    pub to: AgentId,
    pub body: Arc<str>,
    pub ts_ms: u64,
    pub reply_to: Option<MessageId>,
    pub broadcast: bool,
    pub progress: bool,
    pub wake_relay: bool,
}

/// A caller's request to send one message.
pub struct SendRequest {
    pub from: AgentId,
    pub to: String,
    pub body: String,
    pub reply_to: Option<MessageId>,
    pub progress: bool,
    pub wake_relay: bool,
}

/// How a single leg of a send reached its target.
pub enum DeliverResult {
    Injected,
    Woken,
    Unavailable(String),
    Errored(String),
}

/// A receipt's outcome after mapping [`DeliverResult`] onto success or failure.
#[derive(Debug)]
pub enum Outcome {
    Injected,
    Woken,
    Failed(String),
}

/// Per-target outcome of one send leg.
#[derive(Debug)]
pub struct Receipt {
    pub to: AgentId,
    pub outcome: Outcome,
}

/// Result of a `send` call: one message id and one receipt per target.
#[derive(Debug)]
pub struct SendReport {
    pub message_id: MessageId,
    pub receipts: Vec<Receipt>,
}

impl SendReport {
    /// Number of legs that were not failures.
    pub fn delivered(&self) -> usize {
        self.receipts
            .iter()
            .filter(|receipt| !matches!(receipt.outcome, Outcome::Failed(_)))
            .count()
    }
}

/// A resolved messaging peer.
pub struct Endpoint {
    pub id: AgentId,
    pub handle: Option<Arc<str>>,
    pub parent: Option<AgentId>,
    pub status: Arc<str>,
}

/// Address resolution and visibility.
pub trait Directory: Send + Sync {
    fn resolve<'a>(&'a self, sender: &'a str, target: &'a str) -> BoxFuture<'a, Result<Endpoint, String>>;
    fn broadcast_targets<'a>(&'a self, sender: &'a str) -> BoxFuture<'a, Result<Vec<Endpoint>, String>>;
    fn roster<'a>(&'a self, sender: &'a str) -> BoxFuture<'a, Vec<Endpoint>>;
}

/// Side-effecting hand-off into a live session.
pub trait Deliver: Send + Sync {
    fn deliver<'a>(
        &'a self,
        target: &'a Endpoint,
        message: &'a IrcMessage,
        from_parent: bool,
    ) -> BoxFuture<'a, DeliverResult>;
    fn has_pending_input<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, bool>;
    fn can_respond<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, bool>;
}

/// Bus limits taken from configuration.
pub struct IrcLimits {
    pub mailbox_cap: usize,
    pub max_body_bytes: usize,
    pub wait_max: Duration,
    pub broadcast_concurrency: usize,
}

impl Default for IrcLimits {
    fn default() -> Self {
        Self {
            mailbox_cap: 100,
            max_body_bytes: 65536,
            wait_max: Duration::from_secs(300),
            broadcast_concurrency: 16,
        }
    }
}

/// Which messages a [`crate::IrcBus::wait`] consumes.
pub struct WaitFilter {
    pub from: Option<AgentId>,
    pub direct_only: bool,
}

impl WaitFilter {
    pub(crate) fn accepts(&self, message: &IrcMessage) -> bool {
        if let Some(from) = self.from.as_ref()
            && message.from != *from
        {
            return false;
        }
        !self.direct_only || (!message.broadcast && !message.progress)
    }
}

/// Why a wait returned.
pub enum WaitOutcome {
    Message(IrcMessage),
    Timeout,
    NothingToWaitFor,
    Unanswerable(Vec<AgentId>),
    PendingInput,
    Cancelled,
}

/// A direct send with no reply yet.
pub struct Unanswered {
    pub to: AgentId,
    pub seq: u64,
    pub promised: bool,
}

/// A rejected send.
#[derive(Debug)]
pub enum IrcError {
    EmptyBody,
    BodyTooLarge { bytes: usize, max: usize },
    SelfSend,
    ProgressBroadcast,
    Directory(String),
}

impl fmt::Display for IrcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBody => write!(f, "message body is empty"),
            Self::BodyTooLarge { bytes, max } => {
                write!(f, "message body is {bytes} bytes, over the {max} byte limit")
            }
            Self::SelfSend => write!(f, "cannot send a message to yourself"),
            Self::ProgressBroadcast => write!(f, "progress messages cannot be broadcast"),
            Self::Directory(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for IrcError {}

/// Bus counters.
#[derive(Clone, Copy, Debug, Default)]
pub struct IrcStats {
    pub sent: u64,
    pub body_allocations: u64,
    pub mailbox_high_water: u64,
    pub mailbox_dropped: u64,
    pub waiters_live: u64,
    pub broadcast_inflight_high_water: u64,
}
