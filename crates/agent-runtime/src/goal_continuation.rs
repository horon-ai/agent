//! Deterministic decision for whether an attached goal continues the run.
//!
//! Pure over the goal status, the shutdown signal, pending user input, and the
//! number of continuations already spent. The caller reads the goal, observes
//! the signals, and performs the continuation.

/// Maximum consecutive automatic continuations one goal run may spend.
pub const GOAL_CONTINUATION_CAP: u8 = 8;

/// Synthetic user message injected to continue work on an active goal.
pub const GOAL_CONTINUATION_MESSAGE: &str = "[auto] The goal is still active. Continue the work from where you left off, keep the goal and the todo list up to date, and do not reply conversationally or wait for the user.";

/// What the decision depends on, as observed by the caller.
///
/// There is deliberately no "last message was user input" field: at turn end
/// the session tail is always an assistant message, so such a value can never
/// be true. Pending user input arrives through [`pending_user_interrupt`](Self::pending_user_interrupt).
pub struct GoalContinuationInput<'a> {
    /// Attached goal status, or `None` when the session has no goal.
    pub goal_status: Option<&'a str>,
    /// A graceful shutdown was requested.
    pub is_graceful_shutdown: bool,
    /// User input is waiting to be handled by the next turn.
    pub pending_user_interrupt: bool,
    /// Continuations already spent for this goal run.
    pub consecutive_continuations: u8,
}

/// What the caller should do after a turn ends with an attached goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalContinuation {
    /// Inject [`GOAL_CONTINUATION_MESSAGE`] and run another turn.
    Continue,
    /// End the run; the payload names the condition that decided it.
    Stop(&'static str),
}

/// Decide whether an attached goal continues the run.
///
/// First match wins: graceful shutdown, no goal, a status other than `active`,
/// pending user input, the continuation cap, then continue.
pub fn decide_goal_continuation(input: &GoalContinuationInput<'_>) -> GoalContinuation {
    if input.is_graceful_shutdown {
        return GoalContinuation::Stop("shutting-down");
    }
    let Some(status) = input.goal_status else {
        return GoalContinuation::Stop("no-goal");
    };
    if status != "active" {
        return GoalContinuation::Stop("goal-not-active");
    }
    if input.pending_user_interrupt {
        return GoalContinuation::Stop("user-input");
    }
    if input.consecutive_continuations >= GOAL_CONTINUATION_CAP {
        return GoalContinuation::Stop("cap");
    }
    GoalContinuation::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_with(consecutive_continuations: u8) -> GoalContinuationInput<'static> {
        GoalContinuationInput {
            goal_status: Some("active"),
            is_graceful_shutdown: false,
            pending_user_interrupt: false,
            consecutive_continuations,
        }
    }

    #[test]
    fn active_below_cap_continues() {
        for spent in [0, 1, 7] {
            assert_eq!(
                decide_goal_continuation(&active_with(spent)),
                GoalContinuation::Continue,
                "spent {spent} of {GOAL_CONTINUATION_CAP} must continue"
            );
        }
    }

    #[test]
    fn cap_boundary_splits_seven_from_eight() {
        assert_eq!(decide_goal_continuation(&active_with(7)), GoalContinuation::Continue);
        assert_eq!(decide_goal_continuation(&active_with(8)), GoalContinuation::Stop("cap"));
    }

    #[test]
    fn cap_reached_stops() {
        assert_eq!(
            decide_goal_continuation(&active_with(GOAL_CONTINUATION_CAP)),
            GoalContinuation::Stop("cap")
        );
        assert_eq!(
            decide_goal_continuation(&active_with(u8::MAX)),
            GoalContinuation::Stop("cap")
        );
    }

    #[test]
    fn pending_user_interrupt_stops() {
        let input = GoalContinuationInput {
            pending_user_interrupt: true,
            ..active_with(0)
        };
        assert_eq!(decide_goal_continuation(&input), GoalContinuation::Stop("user-input"));
    }

    #[test]
    fn no_goal_stops() {
        let input = GoalContinuationInput {
            goal_status: None,
            ..active_with(0)
        };
        assert_eq!(decide_goal_continuation(&input), GoalContinuation::Stop("no-goal"));
    }

    #[test]
    fn graceful_shutdown_stops() {
        let input = GoalContinuationInput {
            is_graceful_shutdown: true,
            ..active_with(0)
        };
        assert_eq!(
            decide_goal_continuation(&input),
            GoalContinuation::Stop("shutting-down")
        );
    }

    #[test]
    fn every_non_active_status_stops() {
        let statuses = ["completed", "blocked", "paused", "archived", "abandoned", "draft"];
        for status in statuses {
            let input = GoalContinuationInput {
                goal_status: Some(status),
                ..active_with(0)
            };
            assert_eq!(
                decide_goal_continuation(&input),
                GoalContinuation::Stop("goal-not-active"),
                "status {status} must stop"
            );
        }
    }

    #[test]
    fn shutdown_beats_user_input() {
        let input = GoalContinuationInput {
            is_graceful_shutdown: true,
            pending_user_interrupt: true,
            ..active_with(0)
        };
        assert_eq!(
            decide_goal_continuation(&input),
            GoalContinuation::Stop("shutting-down")
        );
    }

    #[test]
    fn user_input_beats_cap() {
        let input = GoalContinuationInput {
            pending_user_interrupt: true,
            ..active_with(GOAL_CONTINUATION_CAP)
        };
        assert_eq!(decide_goal_continuation(&input), GoalContinuation::Stop("user-input"));
    }

    #[test]
    fn branch_table_reports_every_decision() {
        let cases: [(&str, GoalContinuationInput<'_>, GoalContinuation); 13] = [
            ("active/spent=0", active_with(0), GoalContinuation::Continue),
            ("active/spent=7 (cap-1)", active_with(7), GoalContinuation::Continue),
            ("active/spent=8 (cap)", active_with(8), GoalContinuation::Stop("cap")),
            (
                "active/spent=0/user-interrupt",
                GoalContinuationInput {
                    pending_user_interrupt: true,
                    ..active_with(0)
                },
                GoalContinuation::Stop("user-input"),
            ),
            (
                "active/spent=0/shutdown",
                GoalContinuationInput {
                    is_graceful_shutdown: true,
                    ..active_with(0)
                },
                GoalContinuation::Stop("shutting-down"),
            ),
            (
                "no-goal",
                GoalContinuationInput {
                    goal_status: None,
                    ..active_with(0)
                },
                GoalContinuation::Stop("no-goal"),
            ),
            (
                "completed",
                GoalContinuationInput {
                    goal_status: Some("completed"),
                    ..active_with(0)
                },
                GoalContinuation::Stop("goal-not-active"),
            ),
            (
                "blocked",
                GoalContinuationInput {
                    goal_status: Some("blocked"),
                    ..active_with(0)
                },
                GoalContinuation::Stop("goal-not-active"),
            ),
            (
                "paused",
                GoalContinuationInput {
                    goal_status: Some("paused"),
                    ..active_with(0)
                },
                GoalContinuation::Stop("goal-not-active"),
            ),
            (
                "archived",
                GoalContinuationInput {
                    goal_status: Some("archived"),
                    ..active_with(0)
                },
                GoalContinuation::Stop("goal-not-active"),
            ),
            (
                "abandoned",
                GoalContinuationInput {
                    goal_status: Some("abandoned"),
                    ..active_with(0)
                },
                GoalContinuation::Stop("goal-not-active"),
            ),
            (
                "draft",
                GoalContinuationInput {
                    goal_status: Some("draft"),
                    ..active_with(0)
                },
                GoalContinuation::Stop("goal-not-active"),
            ),
            (
                "precedence: shutdown+user-interrupt",
                GoalContinuationInput {
                    is_graceful_shutdown: true,
                    pending_user_interrupt: true,
                    ..active_with(GOAL_CONTINUATION_CAP)
                },
                GoalContinuation::Stop("shutting-down"),
            ),
        ];

        println!("| {:<36} | {:<24} |", "input", "decision");
        for (label, input, expected) in cases {
            let actual = decide_goal_continuation(&input);
            println!("| {label:<36} | {actual:<24?} |");
            assert_eq!(actual, expected, "case: {label}");
        }
    }

    #[test]
    fn continuation_message_is_a_real_instruction() {
        assert!(!GOAL_CONTINUATION_MESSAGE.is_empty());
        assert!(!GOAL_CONTINUATION_MESSAGE.starts_with("TODO_"));
    }
}
