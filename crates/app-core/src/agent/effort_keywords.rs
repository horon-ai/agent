//! Per-turn effort keyword resolution.
//!
//! The scanner lives in [`super::magic_keywords`]. This module holds the agent
//! plumbing: the arming entry point, the turn-start resolver, and the ladder
//! rule that maps a keyword to a concrete rung. Keeping it out of `agent.rs`
//! keeps that file's size ratchet flat.

use super::Agent;
use super::magic_keywords::{self, EffortKeyword};

impl Agent {
    /// Arm effort keywords from a user-authored turn text.
    ///
    /// Stores only what the scanner found in a one-shot slot. The ladder
    /// resolution happens at turn start in [`Agent::resolve_armed_effort`], so it
    /// sees the provider and session state that is current for that turn.
    ///
    /// Call only where the text is the user's own prompt: the client message
    /// turn, the REPL user prompt, and the first iteration of CLI `run`. Never
    /// call for synthetic or agent-initiated messages (swarm assignments, reload
    /// recovery, live turns, overnight, advisor, ambient, debug, swarm planner,
    /// auto-poke continuations).
    pub(crate) fn arm_effort_keywords(&mut self, text: &str) {
        self.armed_effort_keyword = magic_keywords::scan(text);
    }

    /// Consume the armed keyword and resolve this turn's effort override.
    ///
    /// Called at the start of every turn. Whenever the turn was not armed, the
    /// override is cleared so an unarmed turn can never inherit an override from
    /// an earlier one. Resolution order per the design:
    ///
    /// 1. Feature off → no override.
    /// 2. Explicit effort wins: pinned session effort or a swarm sentinel → no
    ///    override.
    /// 3. Ladder = `available_efforts()` minus swarm rungs; empty → no override.
    /// 4. `ultrathink` → last rung; `chillax` → lowest rung excluding `"none"`.
    /// 5. Never move the wrong way relative to the current effort's ladder index.
    pub(super) fn resolve_armed_effort(
        &mut self,
        continues_turn: bool,
        display_role: Option<crate::session::StoredDisplayRole>,
    ) {
        self.turn_effort_override = None;
        let Some(keyword) = self.armed_effort_keyword.take() else {
            return;
        };
        // A `[auto]` reminder-only continuation and any turn displayed under a
        // role other than the user's never carry the user's own prose.
        if continues_turn || display_role.is_some() {
            return;
        }
        if !crate::config::config().features.effort_keywords {
            return;
        }
        if self.reasoning_effort_pinned {
            return;
        }
        let current = self
            .provider
            .reasoning_effort()
            .or_else(|| self.session.reasoning_effort.clone());
        if current.as_deref().is_some_and(crate::prompt::is_swarm_effort) {
            return;
        }

        let ladder: Vec<&'static str> = self
            .provider
            .available_efforts()
            .into_iter()
            .filter(|effort: &&'static str| !crate::prompt::is_swarm_effort(effort))
            .collect();
        let Some(target) = pick_effort_target(&ladder, keyword, current.as_deref()) else {
            return;
        };
        self.turn_effort_override = Some(target.to_string());
    }
}

/// Pick the effort rung a keyword moves to within `ladder`.
///
/// `ultrathink` targets the highest rung, `chillax` the lowest rung that is not
/// `"none"` (reasoning off is a different mode, not low effort). A keyword never
/// moves the wrong way: `chillax` returns no target when it would raise the
/// effort, and `ultrathink` returns no target when it would lower it. A `current`
/// effort that is not on the ladder gets the target rung regardless of direction.
fn pick_effort_target(ladder: &[&'static str], keyword: EffortKeyword, current: Option<&str>) -> Option<&'static str> {
    let ladder_index = |effort: &str| ladder.iter().position(|rung| *rung == effort);
    let current_index = current.and_then(ladder_index);
    match keyword {
        EffortKeyword::Ultrathink => {
            let target = *ladder.last()?;
            let target_index = ladder_index(target)?;
            match current_index {
                Some(index) if target_index < index => None,
                _ => Some(target),
            }
        }
        EffortKeyword::Chillax => {
            let target = *ladder.iter().find(|rung| **rung != "none")?;
            let target_index = ladder_index(target)?;
            match current_index {
                Some(index) if target_index > index => None,
                _ => Some(target),
            }
        }
    }
}
