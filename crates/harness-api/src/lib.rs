//! Stable, versioned client API for the horon-agent harness (agent runtime).
//!
//! This crate defines the *public* boundary between the harness and any UI
//! (TUI, desktop, web, scripts). It is deliberately smaller than the internal
//! `horon-agent-protocol`: only curated, stable surface lives here.
//!
//! Design rules (see docs/protocol/HARNESS_API.md and docs/protocol/SESSION_STORE.md):
//! - Every frame is one JSON object on one line (NDJSON).
//! - Every frame carries `v`, the protocol major version.
//! - Clients must ignore unknown fields and skip unknown event kinds
//!   (both enums carry an `Unknown` catch-all via `#[serde(other)]`-style
//!   fallback on deserialization).
//! - Additive changes bump `API_VERSION_MINOR`; breaking changes bump
//!   `API_VERSION_MAJOR` and must be negotiated in the handshake.

use serde::{Deserialize, Serialize};

mod client;
mod edit_stats;
mod events;
#[doc(hidden)]
pub use edit_stats::estimate_session_edit_stats;
pub use edit_stats::{
    SessionEditStats, enrich_sessions_from_edit_stats, enrich_sessions_from_local_edit_stats, record_session_edit,
};
pub mod replication;
mod requests;
mod runtime_commands;
pub use runtime_commands::{RuntimeCommand, RuntimeCommandOutput};
mod sockets;
mod swarm_metadata;

pub use client::{FrameError, HarnessClient, MAX_FRAME_BYTES, read_frame, write_frame};
pub use events::*;
pub use horon_agent_session_types::TurnStopReason;
pub use horon_agent_usage_types::{ModelUsage, compare_model_usage};
pub use replication::*;
pub use requests::*;
pub use sockets::{api_socket_path, legacy_socket_path, runtime_dir};
pub use swarm_metadata::{enrich_sessions_from_local_swarm_state, enrich_sessions_from_swarm_state};

#[cfg(test)]
#[path = "harness_api_tests/schema_snapshot.rs"]
mod schema_snapshot_tests;

#[cfg(test)]
#[path = "harness_api_tests/capability_coverage.rs"]
mod capability_coverage_tests;

/// Protocol major version. Breaking changes only.
pub const API_VERSION_MAJOR: u32 = 1;
/// Protocol minor version. Additive changes.
pub const API_VERSION_MINOR: u32 = 15;

/// The session's durable runtime journal survives a restart: queued input,
/// runs, control operations, and monitor checkpoints are readable afterwards.
/// Gates `get_runtime_state`, `apply_control`, and `get_control_receipt`.
pub const CAPABILITY_DURABLE_RUNTIME_V1: &str = "durable_runtime_v1";
/// Monitors are a runtime fact rather than an ordinary tool result. No method
/// is gated by this capability: a monitor starts and stops through the
/// existing tool path, and this string only tells a client how to read what
/// comes back. `structured_compaction_v1` sets the same precedent.
pub const CAPABILITY_MONITOR_V1: &str = "monitor_v1";
/// A `get_snapshot` reply carries a generation, a sequence, and a marker id,
/// and the connection receives the matching `snapshot_marker` before it. Gates
/// `get_snapshot` and the `snapshot_marker` event.
pub const CAPABILITY_SNAPSHOT_MARKER_V1: &str = "snapshot_marker_v1";
/// `ask_response` accepts an `answer_id`, so a client can retry an answer
/// without resolving one question twice. Gates only that optional field: an
/// `ask_response` without an id is the legacy request and needs no
/// negotiation.
pub const CAPABILITY_ASK_RECEIPTS_V1: &str = "ask_receipts_v1";

/// Every capability this protocol version defines, with the API surfaces each
/// one gates.
///
/// One table, read by both SDKs and by the bridge's coverage tests. A method
/// that gates on a capability missing here is unreachable, and a capability
/// listed here that no bridge advertises is refused by the client's own
/// `supports` check before the request leaves the process.
///
/// An empty surface list is deliberate and not an omission: `monitor_v1`
/// describes how to read an existing result rather than gating a method.
pub const CAPABILITY_SURFACES: &[(&str, &[&str])] = &[
    (
        CAPABILITY_DURABLE_RUNTIME_V1,
        &["get_runtime_state", "apply_control", "get_control_receipt"],
    ),
    (CAPABILITY_MONITOR_V1, &[]),
    (CAPABILITY_SNAPSHOT_MARKER_V1, &["get_snapshot", "snapshot_marker"]),
    (CAPABILITY_ASK_RECEIPTS_V1, &["ask_response.answer_id"]),
];

/// Every capability this protocol version defines, in advertisement order.
pub const ADVERTISED_CAPABILITIES: &[&str] = &[
    CAPABILITY_DURABLE_RUNTIME_V1,
    CAPABILITY_MONITOR_V1,
    CAPABILITY_SNAPSHOT_MARKER_V1,
    CAPABILITY_ASK_RECEIPTS_V1,
];

/// The structured refusal for a request this server cannot serve.
///
/// One shape for both causes, because a client acts the same way on either: the
/// daemon and bridge need updating, or this build does not have the feature.
/// The message names the capability so an operator can see what to upgrade,
/// and the code is `unsupported` rather than a generic `invalid_request` so a
/// client can tell "this server is too old" from "I sent nonsense".
pub fn unsupported_reply(reply_to: u64, capability: &str, surface: &str) -> ServerFrame {
    ServerFrame::reply(
        reply_to,
        ApiEvent::Error {
            code: ErrorCode::Unsupported,
            message: format!(
                "{surface} requires the `{capability}` capability, which this server does not provide; \
                 update and restart the daemon and the API bridge, or stop calling it here"
            ),
            details: None,
        },
    )
}

/// Envelope wrapping every client-to-server frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientFrame {
    /// Protocol major version.
    pub v: u32,
    /// Client-chosen id echoed in acks/replies. Monotonic per connection.
    pub id: u64,
    #[serde(flatten)]
    pub request: ApiRequest,
}

/// Envelope wrapping every server-to-client frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerFrame {
    /// Protocol major version.
    pub v: u32,
    /// The request id this frame replies to, if any. Streaming events that
    /// are not direct replies omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<u64>,
    #[serde(flatten)]
    pub event: ApiEvent,
}

impl ClientFrame {
    pub fn new(id: u64, request: ApiRequest) -> Self {
        Self {
            v: API_VERSION_MAJOR,
            id,
            request,
        }
    }
}

impl ServerFrame {
    pub fn event(event: ApiEvent) -> Self {
        Self {
            v: API_VERSION_MAJOR,
            reply_to: None,
            event,
        }
    }

    pub fn reply(reply_to: u64, event: ApiEvent) -> Self {
        Self {
            v: API_VERSION_MAJOR,
            reply_to: Some(reply_to),
            event,
        }
    }
}
