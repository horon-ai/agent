//! Read-only consult policies.
//!
//! A consult child answers from a copied transcript. The allow-list its fork
//! registered is final: a client overlay may describe tools for an ordinary
//! session, but it cannot widen a consult for the whole stored life of the
//! session.

use super::SESSION_TOOL_POLICIES;
use std::sync::Arc;

/// Mark an existing policy as a consult child's.
///
/// Called where the agent is built from a session whose `consult` flag is set,
/// so the allow list that build registered is also the one no client can
/// replace. A session without an entry is left alone.
pub(crate) fn mark_session_consult_policy(session_id: &str) {
    let mut policies = SESSION_TOOL_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(policy) = policies.get_mut(session_id).map(Arc::make_mut) {
        policy.consult = true;
    }
}

/// Whether this session's stored policy is a read-only consult child's.
pub(crate) fn session_is_consult(session_id: &str) -> bool {
    SESSION_TOOL_POLICIES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .is_some_and(|policy| policy.consult)
}
