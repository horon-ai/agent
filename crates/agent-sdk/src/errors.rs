//! Errors the SDK reports.
//!
//! Mirrors the TypeScript SDK's `HarnessError`: one error type with a machine
//! readable code, so a caller can branch on the cause instead of matching on
//! prose that is free to change.
//!
//! The three failures every caller has to tell apart carry their identifying
//! fields on the kind itself: which socket could not be dialed, which protocol
//! versions each side asked for, and which session the harness does not know.
//! A caller therefore never parses the sentence to answer "was the daemon down,
//! was it too old, or is my session id wrong?".

use std::fmt;
use std::path::PathBuf;

/// What went wrong, in a form a caller can branch on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// The socket could not be dialed. Usually: the harness is not running.
    ConnectFailed {
        /// The socket the dial targeted. Absent for a transport that has none,
        /// such as an SSH channel.
        path: Option<PathBuf>,
        /// The OS error, kept separate from the actionable sentence.
        cause: String,
    },
    /// The server refused the version handshake.
    UnsupportedVersion {
        /// Major version this client asked for.
        client_major: u32,
        /// Major version the server reported, when the refusal named it.
        server_major: Option<u32>,
    },
    /// The handshake ended with a frame that is neither `hello_ok` nor an
    /// error, so no version was ever agreed.
    HandshakeFailed,
    /// The harness does not know this session id.
    UnknownSession {
        /// The id the client sent, echoed back so the caller can see a typo.
        session_id: String,
    },
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
            Self::ConnectFailed { .. } => "connect_failed",
            Self::UnsupportedVersion { .. } => "unsupported_version",
            Self::HandshakeFailed => "handshake_failed",
            Self::UnknownSession { .. } => "unknown_session",
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
                horon_agent_harness_api::ErrorCode::Unsupported => "unsupported",
            },
        }
    }

    /// The session this failure is about, when it names one.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::UnknownSession { session_id } => Some(session_id),
            _ => None,
        }
    }
}

/// An SDK failure: a typed cause plus a sentence that says what to do about it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}: {message}", .kind.code())]
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

    /// A connection failure that names the socket it dialed and keeps the OS
    /// cause beside the actionable sentence.
    pub(crate) fn connect_failed(
        path: Option<&std::path::Path>,
        cause: impl fmt::Display,
        message: impl Into<String>,
    ) -> Self {
        Self::new(
            ErrorKind::ConnectFailed {
                path: path.map(std::path::Path::to_path_buf),
                cause: cause.to_string(),
            },
            message,
        )
    }

    /// The socket dial that failed, when this is a connection failure.
    pub fn failed_path(&self) -> Option<&std::path::Path> {
        match &self.kind {
            ErrorKind::ConnectFailed { path, .. } => path.as_deref(),
            _ => None,
        }
    }

    /// The server major version named in a refusal sentence, when it names one.
    ///
    /// The bridge answers `bridge speaks API v1; this client asked for v0..=v0`.
    /// Reading it here lets the typed error carry the number a caller needs to
    /// decide whether to upgrade itself or the daemon, without parsing prose.
    pub(crate) fn version_from_refusal(message: &str) -> Option<u32> {
        let rest = message.split("speaks API v").nth(1)?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }

    /// Interpret the `hello` handshake's error frame.
    ///
    /// A refused version becomes the typed variant with both majors; any other
    /// refusal keeps its wire code so no caller loses information.
    pub(crate) fn from_handshake(
        code: horon_agent_harness_api::ErrorCode,
        message: String,
        details: Option<horon_agent_harness_api::StoreErrorDetail>,
    ) -> Self {
        let kind = if code == horon_agent_harness_api::ErrorCode::UnsupportedVersion {
            ErrorKind::UnsupportedVersion {
                client_major: horon_agent_harness_api::API_VERSION_MAJOR,
                server_major: Self::version_from_refusal(&message),
            }
        } else {
            ErrorKind::Harness(code)
        };
        Self::new(kind, message).with_details(details)
    }

    /// Whether this failure says the harness does not know a session id.
    pub fn is_unknown_session(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::UnknownSession { .. }
                | ErrorKind::Harness(horon_agent_harness_api::ErrorCode::UnknownSession)
        )
    }

    /// Whether this failure is a protocol version mismatch.
    pub fn is_version_mismatch(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::UnsupportedVersion { .. }
                | ErrorKind::Harness(horon_agent_harness_api::ErrorCode::UnsupportedVersion)
        )
    }

    /// Translate an error frame into its typed kind, keeping the wire code.
    ///
    /// The three failures a caller branches on become their own variants; every
    /// other wire code stays `Harness` so no existing caller loses information.
    pub(crate) fn from_frame(
        code: horon_agent_harness_api::ErrorCode,
        message: String,
        details: Option<horon_agent_harness_api::StoreErrorDetail>,
        session_id: Option<&str>,
    ) -> Self {
        let kind = match (code, session_id) {
            (horon_agent_harness_api::ErrorCode::UnknownSession, Some(session_id)) => ErrorKind::UnknownSession {
                session_id: session_id.to_string(),
            },
            _ => ErrorKind::Harness(code),
        };
        Self::new(kind, message).with_details(details)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl From<horon_agent_harness_api::FrameError> for Error {
    fn from(error: horon_agent_harness_api::FrameError) -> Self {
        let kind = match error {
            horon_agent_harness_api::FrameError::Eof => ErrorKind::Disconnected,
            _ => ErrorKind::Transport,
        };
        Self::new(kind, error.to_string())
    }
}

/// Result alias used throughout the SDK. The error type is defaulted, so a
/// call site that needs a different error stays possible.
pub type Result<T, E = Error> = std::result::Result<T, E>;
