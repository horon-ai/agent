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
mod effective_settings;
mod events;
mod handoff;
mod message_modes;
#[doc(hidden)]
pub use edit_stats::estimate_session_edit_stats;
pub use edit_stats::{
    SessionEditStats, enrich_sessions_from_edit_stats, enrich_sessions_from_local_edit_stats, record_session_edit,
};
pub use handoff::*;
pub mod replication;
mod requests;
mod runtime_commands;
pub use runtime_commands::{
    PackageCommandInfo, PackageCommandKind, PackageCommandList, PackageCommandOutcome, RuntimeCommand,
    RuntimeCommandOutput,
};
pub mod degradation;
mod sockets;
mod swarm_metadata;

pub use client::{FrameError, HarnessClient, MAX_FRAME_BYTES, read_frame, write_frame};
pub use effective_settings::{EffectiveSetting, SettingSource};
pub use events::*;
pub use horon_agent_session_types::TurnStopReason;
pub use horon_agent_tool_types::ToolVerdict;
pub use horon_agent_usage_types::{ModelUsage, compare_model_usage};
pub use message_modes::MessageModes;
pub use replication::*;
pub use requests::*;
pub use sockets::{api_socket_path, legacy_socket_path, runtime_dir};
pub use swarm_metadata::{enrich_sessions_from_local_swarm_state, enrich_sessions_from_swarm_state};

#[cfg(test)]
#[path = "harness_api_tests/effective_settings_snapshot.rs"]
mod effective_settings_snapshot_tests;
#[cfg(test)]
#[path = "harness_api_tests/message_modes_snapshot.rs"]
mod message_modes_snapshot_tests;
#[cfg(test)]
#[path = "harness_api_tests/schema_snapshot.rs"]
mod schema_snapshot_tests;

#[cfg(test)]
#[path = "harness_api_tests/todo_edit_snapshot.rs"]
mod todo_edit_snapshot_tests;

#[cfg(test)]
#[path = "harness_api_tests/cancellable_work_snapshot.rs"]
mod cancellable_work_snapshot_tests;

#[cfg(test)]
#[path = "harness_api_tests/capability_coverage.rs"]
mod capability_coverage_tests;

#[cfg(test)]
#[path = "harness_api_tests/optional_fields.rs"]
mod optional_field_tests;

#[cfg(test)]
#[path = "harness_api_tests/tool_done_verdict.rs"]
mod tool_done_verdict_tests;

/// The package-management request and receipt a client exchanges with the
/// daemon's catalog. Defined once, beside the manifest declarations the
/// installer validates, so the daemon, the bridge, both SDKs and the CLI cannot
/// drift apart.
pub use horon_agent_extension_types::control::{
    PackageControlOperation, PackageControlReceipt, PackageControlRequest, PackageControlScope, PackageControlState,
    PackageInboxSkip, PackageSummary,
};

/// Protocol major version. Breaking changes only.
pub const API_VERSION_MAJOR: u32 = 1;
/// Protocol minor version. Additive changes.
pub const API_VERSION_MINOR: u32 = 28;

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
/// A client can stop one tool call or one background terminal, cancel the run
/// it saw start, and list the background work that survived a reconnect. Gates
/// `cancel_turn`, `cancel_tool_call`, `cancel_background_terminal` and
/// `list_background_work`. The feature needs a daemon that predates this
/// protocol version to do the work, not merely the bridge, which is why the
/// capability is advertised only when that daemon answered its probe.
pub const CAPABILITY_CANCELLABLE_WORK_V1: &str = "cancellable_work_v1";
/// A client can render live output notices from a finite background `bash` job
/// and asked for them at handshake. Gates the `background_output` event.
///
/// Opt-in rather than always-sent because the notice is raw process text: a
/// client that predates it has no branch for it and would either drop it
/// silently or show it as model output. A connection that never named the
/// capability receives no such frame at all, so an old client sees exactly the
/// traffic it saw before.
pub const CAPABILITY_BACKGROUND_OUTPUT_V1: &str = "background_output_v1";
/// Receive invalidations for the attached session's committed todo list.
pub const CAPABILITY_LIVE_TODO_V1: &str = "live_todo_v1";
/// A client can read the small allow-list of daemon values a settings screen
/// displays, each with the layer that supplied it. Gates
/// `get_effective_settings`.
///
/// The bridge advertises this unconditionally and forwards the request to the
/// daemon, which owns the configuration the session runs with. A daemon that
/// predates the request is answered `unsupported` with this capability named,
/// which the client turns into "this daemon does not report its settings" — the
/// same state an older bridge produces by not advertising the capability at
/// all. The reply never carries a value outside the allow-list, so no credential
/// and no raw file text can travel however the config file is written.
pub const CAPABILITY_EFFECTIVE_SETTINGS_V1: &str = "effective_settings_v1";
/// A client can manage the daemon's package catalog: install, disable, enable,
/// remove, sync the inbox and read the published generation. Gates
/// `package_control` and the `package_control` reply.
///
/// Advertised only when the daemon answered its capability probe with
/// `package_control`, because the daemon owns the catalog: a bridge alone cannot
/// serve the request, and forwarding it to a daemon that predates the request
/// would park the client until its own timeout.
pub const CAPABILITY_PACKAGE_CONTROL_V1: &str = "package_control_v1";
/// A client can list and run the slash commands installed packages supply.
/// Gates `runtime_command` with the `package_commands` and `package_command`
/// values and the matching `runtime_command_result` payloads.
///
/// The commands ride the runtime-command request rather than a request of their
/// own, so an older client never sends one and keeps its existing surface. The
/// bridge advertises this only when the daemon reported `runtime_commands`:
/// without that, the daemon answers the request with an error no client can
/// correlate, which reads as a timeout.
pub const CAPABILITY_PACKAGE_COMMANDS_V1: &str = "package_commands_v1";
/// Change one todo item's status in the attached session's stored list, with
/// the revision the client last read checked before the write. Gates
/// `set_todo_status`.
///
/// The write is the stored list itself, not a message to the agent: no turn
/// starts and no model prompt is injected. A server that does not advertise
/// this leaves the list readable and uneditable, which is what an older bridge
/// serves.
pub const CAPABILITY_TODO_EDIT_V1: &str = "todo_edit_v1";

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
    (
        CAPABILITY_CANCELLABLE_WORK_V1,
        &[
            "cancel_turn",
            "cancel_tool_call",
            "cancel_background_terminal",
            "list_background_work",
        ],
    ),
    (CAPABILITY_BACKGROUND_OUTPUT_V1, &["background_output"]),
    (CAPABILITY_LIVE_TODO_V1, &["todo_changed"]),
    (CAPABILITY_EFFECTIVE_SETTINGS_V1, &["get_effective_settings"]),
    (CAPABILITY_PACKAGE_CONTROL_V1, &["package_control"]),
    (
        CAPABILITY_PACKAGE_COMMANDS_V1,
        &["runtime_command.package_commands", "runtime_command.package_command"],
    ),
    (CAPABILITY_TODO_EDIT_V1, &["set_todo_status"]),
];

/// Every capability this protocol version defines, in advertisement order.
pub const ADVERTISED_CAPABILITIES: &[&str] = &[
    CAPABILITY_DURABLE_RUNTIME_V1,
    CAPABILITY_MONITOR_V1,
    CAPABILITY_SNAPSHOT_MARKER_V1,
    CAPABILITY_ASK_RECEIPTS_V1,
    CAPABILITY_CANCELLABLE_WORK_V1,
    CAPABILITY_BACKGROUND_OUTPUT_V1,
    CAPABILITY_LIVE_TODO_V1,
    CAPABILITY_EFFECTIVE_SETTINGS_V1,
    CAPABILITY_PACKAGE_CONTROL_V1,
    CAPABILITY_PACKAGE_COMMANDS_V1,
    CAPABILITY_TODO_EDIT_V1,
];

/// The event capabilities a connection opts into at handshake.
///
/// A client requests these in `hello.event_capabilities`. The bridge echoes
/// only capabilities that the daemon also supports.
pub const OPT_IN_EVENT_CAPABILITIES: &[&str] = &[CAPABILITY_BACKGROUND_OUTPUT_V1, CAPABILITY_LIVE_TODO_V1];

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
