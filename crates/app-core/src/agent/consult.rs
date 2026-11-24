//! Read-only consult children.
//!
//! A consult answers from a copied transcript, so its stored session narrows
//! every agent built for it to [`crate::session_scope::CONSULT_TOOLS`] and
//! keeps the registered policy closed to client overlays. Both rules live
//! together here because they are one behavior: the child cannot widen after
//! the fork.

use super::Agent;
use crate::session::Session;
use std::collections::HashSet;

impl Agent {
    /// Mark this session as a read-only consult child.
    ///
    /// The flag is persisted with the session, so every later agent built for
    /// it starts on the consult tool list instead of the configured profile.
    pub(crate) fn mark_consult(&mut self) {
        self.session.consult = true;
    }
}

/// The allow-list a session may use, with a consult child forced read-only.
///
/// The dispatcher rule is applied after the consult narrowing, so a consult
/// child never lists an eager tool the profile would have offered it.
pub(crate) fn narrow(
    tools: &crate::config::ToolConfig,
    session: &Session,
    allowed_tools: Option<HashSet<String>>,
    core_listed: bool,
) -> Option<HashSet<String>> {
    let allowed_tools = if session.consult {
        Some(
            crate::session_scope::CONSULT_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
        )
    } else {
        allowed_tools
    };
    Agent::without_unreachable_dispatcher(tools, allowed_tools, core_listed)
}

/// Register the session's tool policy and keep a consult child's closed.
///
/// The policy is registered first and marked second, because the allow list
/// that build registered is the one no client overlay may replace.
pub(crate) fn register(
    session: &Session,
    allowed_tools: &Option<HashSet<String>>,
    disabled_tools: &HashSet<String>,
) -> crate::tool::SessionToolPolicyRegistration {
    let registration =
        crate::tool::register_session_tool_policy(&session.id, allowed_tools.clone(), disabled_tools.clone());
    if session.consult {
        crate::tool::mark_session_consult_policy(&session.id);
    }
    registration
}
