//! In-process IRC mailbox bus for agent-to-agent messaging.

pub mod bus;
pub mod envelope;
pub mod global;
pub mod mailbox;
pub mod message;

pub use bus::IrcBus;
pub use envelope::format_envelope;
pub use message::{
    AgentId, Deliver, DeliverResult, Directory, Endpoint, IrcError, IrcLimits, IrcMessage, IrcStats, MessageId,
    Outcome, Receipt, SendReport, SendRequest, Unanswered, WaitFilter, WaitOutcome,
};

#[cfg(test)]
#[path = "harness_tests.rs"]
mod harness_tests;

#[cfg(test)]
#[path = "bus_tests.rs"]
mod bus_tests;

#[cfg(test)]
#[path = "wait_tests.rs"]
mod wait_tests;

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod envelope_tests;

#[cfg(test)]
mod perf_tests;
