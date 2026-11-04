//! Session bootstrap for a new Agent.
//!
//! A connection first builds a provisional Agent before it knows whether the
//! client wants a fresh conversation or is reattaching to an existing session.
//! Building the session context and recording the environment are the costly
//! parts (both read the working tree), and a reattachment discards the
//! placeholder entirely, so they wait here until the connection proves it owns
//! a fresh session.

use super::Agent;

/// Finish a newly built Agent whose tools and session identity already exist.
///
/// `confirmed` is true for the public constructors: those own a brand-new
/// session immediately. A provisional connection instead leaves the bootstrap
/// pending so [`Agent::materialize_provisional_session`] can run it exactly once
/// if the connection starts a real conversation.
pub(super) fn finish_new_agent(agent: &mut Agent, confirmed: bool) {
    if confirmed {
        materialize_new_session(agent);
    } else {
        agent.provisional_init_pending = true;
    }
    agent.fire_session_lifecycle_hook("session_start", "create");
}

/// Build and record everything that depends on the session being real: the
/// active marker, the first immutable session-context message, and the initial
/// environment snapshot.
fn materialize_new_session(agent: &mut Agent) {
    agent.session.mark_active();
    agent.session.ensure_initial_session_context_message();
    agent.seed_compaction_from_session();
    agent.log_env_snapshot("create");
}

impl Agent {
    /// Commit a provisional placeholder to a real conversation exactly once.
    ///
    /// Returns true when this call performed the deferred bootstrap. Returns
    /// false for an Agent that was never provisional or was already
    /// materialized, so repeated Subscribe/Message requests cannot duplicate
    /// the session context or count a session twice.
    pub(crate) fn materialize_provisional_session(&mut self) -> bool {
        if !self.provisional_init_pending {
            return false;
        }
        self.provisional_init_pending = false;
        materialize_new_session(self);
        true
    }

    /// Install a restored session in place of this Agent's provisional session.
    ///
    /// A restored session is already a real conversation, so it must never be
    /// bootstrapped again by `materialize_provisional_session`.
    pub(super) fn install_restored_session(&mut self, session: crate::session::Session) {
        self.session = session;
        self.provisional_init_pending = false;
        // A restored session starts with no deferred commit of its own; a stale
        // barrier from an earlier restore must not survive the replacement.
        self.clear_pending_resume();
    }
}
