//! Turn-end hook that keeps an active goal running.
//!
//! Both turn loops call [`Agent::maybe_continue_goal`] after their own recovery
//! branches have declined the turn end and before they settle the final answer.
//! The decision itself is the pure policy in
//! `horon_agent_agent_runtime::goal_continuation`; this module reads the state it
//! needs (the attached goal, the pending user input, the continuations already
//! spent) and performs the continuation.

use super::*;
use horon_agent_agent_runtime::goal_continuation::{
    GOAL_BUDGET_WRAP_UP_MESSAGE, GOAL_CONTINUATION_CAP, GOAL_CONTINUATION_MESSAGE, GoalContinuation,
    GoalContinuationInput, decide_goal_continuation,
};

pub(super) fn goal_continue_log_line(consecutive_continuations: u8, session_id: &str) -> String {
    format!(
        "[goal] auto-continue {}/{} session={}",
        consecutive_continuations + 1,
        GOAL_CONTINUATION_CAP,
        session_id
    )
}

pub(super) fn goal_stop_log_line(reason: &str, session_id: &str) -> String {
    format!("[goal] stop reason={reason} session={session_id}")
}

/// Tokens a session has spent, counting every class the provider reports.
///
/// Cache reads and writes are added rather than subtracted: a provider that
/// bills them at a lower rate still reports them, and a budget that ignored
/// them would let a cache-heavy goal run far past its ceiling. The sum is
/// saturating because a transcript is unbounded and this decision must never
/// be the thing that panics.
fn spent_tokens(totals: &crate::protocol::TokenUsageTotals) -> u64 {
    totals
        .input_tokens
        .saturating_add(totals.output_tokens)
        .saturating_add(totals.cache_read_input_tokens)
        .saturating_add(totals.cache_creation_input_tokens)
}

impl Agent {
    /// Continue an active goal with one synthetic message, or stop the run.
    ///
    /// `Ok(true)` means a continuation message was stored and the caller must
    /// start another provider request; `Ok(false)` means the run ends here.
    pub(super) fn maybe_continue_goal(&mut self) -> Result<bool> {
        let goal = crate::goal::load_attached_goal(
            self.session_id(),
            self.session.working_dir.as_deref().map(std::path::Path::new),
        )?;
        let goal_status = goal.as_ref().map(|goal| goal.status.as_str());
        let budget_tokens = goal.as_ref().and_then(|goal| goal.budget_tokens);
        let consecutive_continuations = self.consecutive_goal_continuations();
        let input = GoalContinuationInput {
            goal_status,
            is_graceful_shutdown: self.is_graceful_shutdown(),
            pending_user_interrupt: self.has_soft_interrupts() || self.is_graceful_shutdown(),
            consecutive_continuations,
            tokens_spent: spent_tokens(&self.token_usage_totals()),
            budget_tokens,
        };
        match decide_goal_continuation(&input) {
            GoalContinuation::Continue => {
                self.add_message_with_display_role(
                    Role::User,
                    vec![ContentBlock::Text {
                        text: GOAL_CONTINUATION_MESSAGE.to_string(),
                        cache_control: None,
                    }],
                    Some(crate::session::StoredDisplayRole::System),
                );
                self.save_session()?;
                logging::info(&goal_continue_log_line(consecutive_continuations, self.session_id()));
                Ok(true)
            }
            GoalContinuation::WrapUp => {
                // One bounded turn, then the goal leaves `active` so the next
                // turn end stops instead of wrapping up forever.
                //
                // The status is written before the message on purpose. The goal
                // file and the transcript are two writes, and a crash between
                // them must leave a goal that stops rather than one that keeps
                // spending. In this order the loss is the wrap-up message: the
                // status is already stored, so the next turn end sees
                // `budget_limited` and ends the run with a shorter summary.
                if let Some(goal) = goal.as_ref() {
                    crate::goal::update_goal(
                        &goal.id,
                        Some(goal.scope),
                        self.session.working_dir.as_deref().map(std::path::Path::new),
                        crate::goal::GoalUpdateInput {
                            status: Some(crate::goal::GoalStatus::BudgetLimited),
                            checkpoint_summary: Some("Stopped: the goal reached its token budget.".to_string()),
                            ..Default::default()
                        },
                    )?;
                }
                self.add_message_with_display_role(
                    Role::User,
                    vec![ContentBlock::Text {
                        text: GOAL_BUDGET_WRAP_UP_MESSAGE.to_string(),
                        cache_control: None,
                    }],
                    Some(crate::session::StoredDisplayRole::System),
                );
                self.save_session()?;
                logging::info(&goal_stop_log_line("budget", self.session_id()));
                Ok(true)
            }
            GoalContinuation::Stop(reason) => {
                logging::info(&goal_stop_log_line(reason, self.session_id()));
                Ok(false)
            }
        }
    }

    /// Count the continuation messages that end the stored transcript.
    ///
    /// The scan runs over the whole stored transcript
    /// ([`Session::messages_for_provider_uncached`]), not over
    /// `visible_conversation_messages`: continuation messages are stored with
    /// [`StoredDisplayRole::System`](crate::session::StoredDisplayRole), so the
    /// visible-conversation accessor would drop every one of them and the count
    /// would always be zero.
    ///
    /// Four kinds of message are passed over instead of ending the scan: every
    /// assistant message; every user message that carries a tool result; every
    /// todo auto-poke; and every internal `<system-reminder>` user message. A
    /// turn that ran tools ends on a tool-result message, and stopping there
    /// would pin the count at zero for ordinary tool-using work. The synthetic
    /// user turns (auto-pokes, generated-image visual context, the empty-response
    /// recovery prompt) are stored between continuations, so ending the scan on
    /// one of them would reset the count and defeat the cap. A text-only user
    /// message that is none of these ends the scan.
    pub(super) fn consecutive_goal_continuations(&self) -> u8 {
        let mut count: u8 = 0;
        for message in self.session.messages_for_provider_uncached().iter().rev() {
            match message.role {
                Role::Assistant => continue,
                Role::User => {}
            }
            if message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
            {
                continue;
            }
            let first_text = message.content.iter().find_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            });
            let Some(text) = first_text else {
                break;
            };
            if text.starts_with(GOAL_CONTINUATION_MESSAGE) {
                count = count.saturating_add(1);
                if count >= GOAL_CONTINUATION_CAP {
                    break;
                }
                continue;
            }
            // A todo auto-poke is a synthetic user turn stored between
            // continuations whenever open todos exist. Ending the scan there
            // would pin the count at zero and let the goal loop run forever.
            if crate::todo::is_auto_poke_message(text) {
                continue;
            }
            // Generated-image visual context and the empty-response recovery
            // prompt are synthetic user turns that open with a
            // `<system-reminder>` block. They arrive between continuations too,
            // so they get the same treatment as an auto-poke.
            if message.is_internal_system_reminder() {
                continue;
            }
            break;
        }
        count
    }
}
