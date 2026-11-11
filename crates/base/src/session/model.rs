use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Extra non-conversation UI/state events persisted for replay fidelity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredReplayEvent {
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub kind: StoredReplayEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event")]
pub enum StoredReplayEventKind {
    /// A non-provider display message shown in the UI (e.g. swarm/system notice).
    #[serde(rename = "display_message")]
    DisplayMessage {
        role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        content: String,
    },
    /// Historical swarm member status snapshot.
    #[serde(rename = "swarm_status")]
    SwarmStatus {
        members: Vec<crate::protocol::SwarmMemberStatus>,
    },
    /// Historical swarm plan snapshot.
    #[serde(rename = "swarm_plan")]
    SwarmPlan {
        swarm_id: String,
        version: u64,
        items: Vec<crate::plan::PlanItem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        participants: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Plan-mode state this session entered, so a replay can explain why a call
    /// was refused at that point.
    #[serde(rename = "plan_mode")]
    PlanMode { mode: String },
    /// The one-shot `[prewalk]` handoff this session decided, so a replay can
    /// name both models and a resumed session never repeats it.
    ///
    /// `refused` is set when the handoff was abandoned, which keeps the reason
    /// from being re-derived (and re-warned) on every later round.
    #[serde(rename = "prewalk")]
    Prewalk {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        into: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused: Option<String>,
    },
}

pub(super) const SESSION_CONTEXT_PREFIX: &str = "<system-reminder>\n# Session Context";
