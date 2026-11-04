//! Errors the SDK reports.
//!
//! Mirrors the TypeScript SDK's `HarnessError`: one error type with a machine
//! readable code, so a caller can branch on the cause instead of matching on
//! prose that is free to change.

use std::fmt;

/// What went wrong, in a form a caller can branch on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// The socket could not be dialed. Usually: the harness is not running.
    ConnectFailed,
    /// The version handshake was refused or answered with the wrong frame.
    HandshakeFailed,
    /// No reply arrived within the request timeout.
    Timeout,
    /// The connection is closed; nothing further can be sent on it.
    Disconnected,
    /// The reply was a different event kind than the request implies.
    UnexpectedReply,
    /// The harness answered with an error frame. Carries the wire code.
    Harness(horon_agent_harness_api::ErrorCode),
    /// I/O or JSON trouble on the transport.
    Transport,
    /// Starting a runtime (daemon or bridge) failed.
    LaunchFailed,
    /// The configured horon-agent executable could not be started.
    HoronAgentNotFound,
    /// The private bridge exited before its socket became ready.
    StartupFailed,
    /// The private bridge or SSH startup/handshake missed its deadline.
    StartupTimeout,
    /// An instance home was unsafe or aliased the user's own home.
    InvalidInstanceHome,
    /// An option was outside its documented range.
    InvalidOption,
    /// An operation requires a reconnectable native socket or SSH transport.
    UnsupportedTransport,
    /// An operation requires a capability this server did not advertise.
    Unsupported,
    /// A global event consumer fell behind its bounded queue.
    EventBufferOverflow,
}

impl ErrorKind {
    /// Stable, lowercase identifier. Matches the TS SDK's code strings so the
    /// two SDKs describe the same failure by the same name.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ConnectFailed => "connect_failed",
            Self::HandshakeFailed => "handshake_failed",
            Self::Timeout => "timeout",
            Self::Disconnected => "disconnected",
            Self::UnexpectedReply => "unexpected_reply",
            Self::Transport => "transport",
            Self::LaunchFailed => "launch_failed",
            Self::HoronAgentNotFound => "horon_agent_not_found",
            Self::StartupFailed => "startup_failed",
            Self::StartupTimeout => "startup_timeout",
            Self::InvalidInstanceHome => "invalid_instance_home",
            Self::InvalidOption => "invalid_option",
            Self::UnsupportedTransport => "unsupported_transport",
            Self::Unsupported => "unsupported",
            Self::EventBufferOverflow => "event_buffer_overflow",
            Self::Harness(code) => match code {
                horon_agent_harness_api::ErrorCode::UnsupportedVersion => "unsupported_version",
                horon_agent_harness_api::ErrorCode::UnknownRequest => "unknown_request",
                horon_agent_harness_api::ErrorCode::UnknownSession => "unknown_session",
                horon_agent_harness_api::ErrorCode::InvalidRequest => "invalid_request",
                horon_agent_harness_api::ErrorCode::Internal => "internal",
                horon_agent_harness_api::ErrorCode::BrowserUnavailable => "browser_unavailable",
                horon_agent_harness_api::ErrorCode::Unsupported => "unsupported",
            },
        }
    }
}

/// An SDK failure: a code plus a sentence that says what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    /// Finer cause from the server, when it supplied one. Old servers omit it.
    /// Boxed so adding it does not grow every `Result` in this crate.
    pub details: Option<Box<horon_agent_harness_api::StoreErrorDetail>>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Option<horon_agent_harness_api::StoreErrorDetail>) -> Self {
        self.details = details.map(Box::new);
        self
    }

    /// The stable code for this failure.
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.code(), self.message)
    }
}

impl std::error::Error for Error {}

impl From<horon_agent_harness_api::FrameError> for Error {
    fn from(error: horon_agent_harness_api::FrameError) -> Self {
        let kind = match error {
            horon_agent_harness_api::FrameError::Eof => ErrorKind::Disconnected,
            _ => ErrorKind::Transport,
        };
        Self::new(kind, error.to_string())
    }
}

/// Result alias used throughout the SDK.
pub type Result<T> = std::result::Result<T, Error>;
