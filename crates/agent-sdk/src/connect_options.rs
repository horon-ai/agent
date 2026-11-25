//! The knobs a client sets before it connects.
//!
//! Kept beside [`crate::HoronAgentClient`] rather than inside `client.rs`, which
//! is already the largest file in the crate and is tracked by the code-size
//! budget. `client` re-exports the type, so both `horon_agent_sdk::ConnectOptions`
//! and `horon_agent_sdk::client::ConnectOptions` name the same struct.

use std::time::Duration;

/// How a client reaches the harness.
pub struct ConnectOptions {
    /// Defaults to the resolved harness API socket path.
    pub socket_path: Option<std::path::PathBuf>,
    /// Client identity sent in the handshake, e.g. "my-app/1.0".
    pub client_name: String,
    /// How long a request waits for its reply. `None` disables the timeout.
    pub request_timeout: Option<Duration>,
    /// Start the daemon and bridge if they are not already listening.
    ///
    /// On by default: an app that only works once the user has launched two
    /// daemons by hand is indistinguishable from a broken one.
    pub ensure_runtime: bool,
    /// Desktop inline Markdown response renderer. Other clients leave false.
    pub supports_user_response: bool,
    /// Whether this client can render the `ask` tool's questions and answer
    /// them. Off by default: the daemon parks a turn on the person only for a
    /// connection that says it can ask one, so a client without a question
    /// screen must not claim one.
    pub supports_ask_requests: bool,
    /// Whether this client renders `approval_pending` interactively and answers
    /// via `respond_to_permission`, so gated tools park and run on approval.
    pub supports_permission_requests: bool,
    /// Optional A2UI v1.0 renderer capabilities offered as the connection default.
    ///
    /// `None` by default so standard SDK sessions keep their prompt cache unchanged.
    pub a2ui: Option<horon_agent_harness_api::A2uiSessionCapabilities>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            socket_path: None,
            client_name: concat!("horon-agent-sdk-rs/", env!("CARGO_PKG_VERSION")).to_string(),
            request_timeout: Some(Duration::from_secs(30)),
            ensure_runtime: true,
            supports_user_response: false,
            supports_ask_requests: false,
            supports_permission_requests: false,
            a2ui: None,
        }
    }
}
