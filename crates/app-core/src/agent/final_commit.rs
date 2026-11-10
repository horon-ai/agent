//! The commit that ends a turn, and who pays for its derived cache.
//!
//! Every commit publishes originals first and then rebuilds the derived
//! checkpoint and catalog row from them. For the last commit of a one-shot run
//! nobody reads that rebuild: the process exits, and the next reader validates
//! the cached stamp against the log anyway, so an unrebuilt cache costs it the
//! same one rebuild it would have paid regardless. `horon-agent run` therefore
//! leaves that single rebuild to the next reader; every other caller keeps the
//! ordinary commit-and-publish path.

use super::Agent;
use crate::provider::Provider;
use crate::tool::Registry;
use anyhow::Result;
use std::sync::Arc;

impl Agent {
    /// Build an Agent for a one-shot run, which leaves the derived cache of its
    /// final commit to the next reader.
    ///
    /// A long-lived session must keep publishing between turns, because its next
    /// reader is a listing that should not pay a rebuild there.
    pub fn for_one_shot_run(provider: Arc<dyn Provider>, registry: Registry) -> Self {
        let mut agent = Self::new(provider, registry);
        agent.session.set_defer_final_cache(true);
        agent
    }

    /// Restore a stored session for a one-shot run, keeping the cache deferral.
    ///
    /// A restore swaps in a freshly loaded session, so the flag set by
    /// [`Agent::for_one_shot_run`] is applied to the session that runs the turn.
    pub fn restore_session_for_one_shot_run(&mut self, id: &str) -> Result<()> {
        self.restore_session(id)?;
        self.session.set_defer_final_cache(true);
        Ok(())
    }

    /// Save the final answer of a turn, deferring the derived cache rebuild when
    /// this session allows it.
    ///
    /// The mark is consumed by this commit whatever its outcome, so a failure
    /// here cannot silence the rebuild of the commit that retries it.
    pub(crate) fn save_final_answer(&mut self) -> Result<()> {
        self.session.mark_final_commit();
        self.save_session()
    }
}
