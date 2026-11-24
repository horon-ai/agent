//! Todo state and revision-checked status edits.
//!
//! The methods live here rather than in `client.rs` so the connected-client
//! module stays inside its size budget; the impl block is the same type and the
//! behavior is unchanged.

use crate::client::{HoronAgentClient, unexpected};
use crate::errors::Result;
use horon_agent_harness_api::{ApiEvent, ApiRequest};

/// The result of one revision-checked todo status edit.
///
/// `applied` is false for a refusal, and the state carried beside it is the
/// list as it stands now — a stale editor refreshes from this answer instead of
/// asking again. The edit never starts a turn: it changes the stored list the
/// agent reads, not the conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct TodoEdit {
    /// Whether the stored list was written.
    pub applied: bool,
    /// Which check refused the write, when it was not applied.
    pub refusal: Option<horon_agent_todo_policy::TodoEditRefusal>,
    /// The session's todo state after the reply: the new list when applied,
    /// the unchanged current one otherwise.
    pub state: horon_agent_todo_policy::TodoSnapshot,
    /// Revision of the stored list this state was read from.
    pub revision: String,
}

impl HoronAgentClient {
    /// The session's persisted todo state for the auto-poke and quality-gate
    /// policy. Requires the `todo_state` capability.
    pub fn todo_state(&self, session_id: &str) -> Result<horon_agent_todo_policy::TodoSnapshot> {
        match self
            .request_ok(ApiRequest::GetTodoState {
                session_id: session_id.to_string(),
            })?
            .event
        {
            ApiEvent::TodoState { state, .. } => Ok(state),
            other => Err(unexpected("todo_state", &other)),
        }
    }

    /// The persisted todo state with the revision that names the stored list
    /// it was read from.
    ///
    /// The revision is absent on a server that predates it; such a server also
    /// cannot apply a revision-checked edit, so an editor treats an absent
    /// revision as read-only. Requires the `todo_state` capability.
    pub fn todo_snapshot(&self, session_id: &str) -> Result<(horon_agent_todo_policy::TodoSnapshot, Option<String>)> {
        self.require_capability("todo_state", "todo_snapshot")?;
        match self
            .request_ok(ApiRequest::GetTodoState {
                session_id: session_id.to_string(),
            })?
            .event
        {
            ApiEvent::TodoState { state, revision, .. } => Ok((state, revision)),
            other => Err(unexpected("todo_state", &other)),
        }
    }

    /// Change one todo item's status only if the stored list is still the
    /// revision the caller read.
    ///
    /// A stale list writes nothing: the returned [`TodoEdit`] carries
    /// `applied: false`, the refusal `stale_revision`, and the current state and
    /// revision, which is what an editor shows after refreshing. The edit is
    /// the stored list itself, not a message to the agent, so no turn starts.
    /// Requires the `todo_edit_v1` capability.
    pub fn set_todo_status(
        &self,
        session_id: &str,
        expected_revision: &str,
        id: &str,
        status: &str,
    ) -> Result<TodoEdit> {
        self.require_capability(horon_agent_harness_api::CAPABILITY_TODO_EDIT_V1, "set_todo_status")?;
        match self
            .request_ok(ApiRequest::SetTodoStatus {
                session_id: session_id.to_string(),
                expected_revision: expected_revision.to_string(),
                id: id.to_string(),
                status: status.to_string(),
            })?
            .event
        {
            ApiEvent::TodoEditResult {
                applied,
                refusal,
                state,
                revision,
                ..
            } => Ok(TodoEdit {
                applied,
                refusal,
                state,
                revision,
            }),
            other => Err(unexpected("todo_edit_result", &other)),
        }
    }

    /// Apply a follow-up decision's persistent effects. A no-op when empty.
    pub fn ack_todo_follow_up(&self, session_id: &str, effects: horon_agent_todo_policy::TodoEffects) -> Result<()> {
        if effects.is_empty() {
            return Ok(());
        }
        self.request_ok(ApiRequest::AckTodoFollowUp {
            session_id: session_id.to_string(),
            effects,
        })
        .map(drop)
    }
}
