//! Runtime-backed commands, shared by all non-terminal clients.
use serde::{Deserialize, Serialize};

/// Commands executed by the attached runtime, never submitted as an agent prompt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCommand {
    Skills,
    Context,
    Info,
    Memory,
    Swarm,
    Agents,
    SubagentModel,
    Autoreview,
    Autojudge,
    Transport,
    Fast,
    Overnight,
    Productivity,
    Initiatives,
    RefreshModelList,
    Continue,
    ResumeAll,
    Remote,
    ServerReload,
    Review,
    Judge,
    Subagent,
    Auth,
    Logout,
    Plan,
    /// Bounded notes about this session's visible work and lifecycle hooks.
    ///
    /// The reply is the observation view for the session that asked, never
    /// another session's, and it is a client report: nothing in it is added to
    /// the conversation or sent to the model.
    Observations,
    Mcp,
}

/// Structured runtime data plus a human-readable rendering. Data is command
/// specific and additive. Clients must ignore unknown fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeCommandOutput {
    pub session_id: String,
    pub command: RuntimeCommand,
    pub data: serde_json::Value,
    pub message: String,
}
