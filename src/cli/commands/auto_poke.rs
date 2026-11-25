use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use super::thoughts;

pub(in crate::cli) const DEFAULT_RUN_AUTO_POKE_MAX_TURNS: usize = 6;

pub(super) fn run_command_auto_poke_enabled() -> bool {
    match std::env::var("HORON_AGENT_RUN_AUTO_POKE") {
        Ok(value) => {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "0" | "false" | "off" | "no")
        }
        Err(_) => crate::config::config().features.auto_poke,
    }
}

pub(in crate::cli) fn run_command_auto_poke_max_turns() -> Option<usize> {
    match std::env::var("HORON_AGENT_RUN_AUTO_POKE_MAX_TURNS") {
        Ok(value) => match value.trim().parse::<usize>() {
            Ok(count) if count > 0 => Some(count),
            Ok(_) => Some(DEFAULT_RUN_AUTO_POKE_MAX_TURNS),
            Err(error) => {
                crate::logging::warn(&format!(
                    "Ignoring invalid HORON_AGENT_RUN_AUTO_POKE_MAX_TURNS={value:?}: {error}"
                ));
                Some(DEFAULT_RUN_AUTO_POKE_MAX_TURNS)
            }
        },
        Err(_) => Some(DEFAULT_RUN_AUTO_POKE_MAX_TURNS),
    }
}

pub(super) fn run_command_auto_poke_limit_reached(turns_completed: usize, max_turns: Option<usize>) -> bool {
    max_turns.is_some_and(|max_turns| turns_completed >= max_turns)
}

#[derive(Debug, Default)]
pub(super) struct AutoPokeStallGuard {
    last_incomplete_fingerprint: Option<String>,
}

impl AutoPokeStallGuard {
    pub(super) fn is_stalled_incomplete(&mut self, todos: &[crate::todo::TodoItem]) -> bool {
        let incomplete: Vec<&crate::todo::TodoItem> = todos
            .iter()
            .filter(|todo| {
                !crate::todo::todo_status_is_completed(&todo.status)
                    && !crate::todo::todo_status_is_cancelled(&todo.status)
            })
            .collect();
        let fingerprint = match serde_json::to_string(&incomplete) {
            Ok(encoded) => encoded,
            Err(_) => format!("incomplete:{}", incomplete.len()),
        };
        if self.last_incomplete_fingerprint.as_ref() == Some(&fingerprint) {
            return true;
        }
        self.last_incomplete_fingerprint = Some(fingerprint);
        false
    }

    pub(super) fn reset_incomplete(&mut self) {
        self.last_incomplete_fingerprint = None;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::cli) enum RunAutoPokeFollowUp {
    Incomplete {
        count: usize,
        message: String,
    },
    ConfidenceSummary {
        total_todos: usize,
        message: String,
        confidence_spike_challenge: bool,
    },
    /// Deferred quality-check reminder for the points this turn flagged and
    /// never resolved. Delivered once, ahead of the confidence summary.
    GateDigest {
        message: String,
    },
}

pub(super) fn run_todos(session_id: &str) -> Vec<crate::todo::TodoItem> {
    let Ok(todos) = crate::todo::load_todos(session_id) else {
        return Vec::new();
    };
    todos
}

/// Build the deferred quality-check reminder for a headless run, consuming the
/// turn's observation log.
///
/// The log is cleared whether or not a reminder results, so one turn's points
/// cannot be raised again against the next turn's work. Returns `None` only when
/// the turn recorded nothing.
pub(in crate::cli) fn take_run_gate_digest(session_id: &str, already_delivered: bool) -> Option<String> {
    if already_delivered {
        return None;
    }
    let Ok(observations) = crate::todo::load_gate_observations(session_id) else {
        return None;
    };
    if observations.is_empty() {
        return None;
    }
    let plan = match crate::todo::load_plan(session_id) {
        Ok(plan) => plan,
        Err(error) => {
            crate::logging::warn(&format!(
                "auto_poke: failed to load todo plan for {session_id}: {error}"
            ));
            Default::default()
        }
    };
    let goals = match crate::todo::load_goals(session_id) {
        Ok(goals) => goals,
        Err(error) => {
            crate::logging::warn(&format!("auto_poke: failed to load goals for {session_id}: {error}"));
            Vec::new()
        }
    };
    let digest = crate::todo::build_gate_digest(&observations, &plan, &goals);
    if let Err(err) = crate::todo::clear_gate_observations(session_id) {
        crate::logging::warn(&format!(
            "Failed to clear todo gate observations for session {session_id}: {err}"
        ));
    }
    digest
}

/// Consume the observation log only once the turn has actually ended.
pub(super) fn take_run_gate_digest_if_turn_ended(
    session_id: &str,
    already_delivered: bool,
    todos: &[crate::todo::TodoItem],
) -> Option<String> {
    let work_remains = todos.iter().any(|todo| {
        !crate::todo::todo_status_is_completed(&todo.status) && !crate::todo::todo_status_is_cancelled(&todo.status)
    });
    if work_remains {
        return None;
    }
    take_run_gate_digest(session_id, already_delivered)
}

pub(in crate::cli) fn build_run_auto_poke_follow_up_from_todos(
    todos: &[crate::todo::TodoItem],
    confidence_spike_challenged: bool,
    gate_digest: Option<String>,
) -> Option<RunAutoPokeFollowUp> {
    let incomplete: Vec<_> = todos
        .iter()
        .filter(|todo| {
            !crate::todo::todo_status_is_completed(&todo.status) && !crate::todo::todo_status_is_cancelled(&todo.status)
        })
        .cloned()
        .collect();
    if !incomplete.is_empty() {
        return Some(RunAutoPokeFollowUp::Incomplete {
            count: incomplete.len(),
            message: build_run_poke_message(&incomplete),
        });
    }
    if let Some(message) = gate_digest {
        return Some(RunAutoPokeFollowUp::GateDigest { message });
    }
    if !todos.is_empty()
        && let Some((message, confidence_spike_challenge)) =
            build_run_todo_validation_message(todos, !confidence_spike_challenged)
    {
        return Some(RunAutoPokeFollowUp::ConfidenceSummary {
            total_todos: todos.len(),
            message,
            confidence_spike_challenge,
        });
    }
    None
}

fn build_run_poke_message(incomplete: &[crate::todo::TodoItem]) -> String {
    crate::todo::build_auto_poke_message(incomplete.len())
}

pub(super) fn build_run_todo_validation_message(
    todos: &[crate::todo::TodoItem],
    allow_confidence_spike_challenge: bool,
) -> Option<(String, bool)> {
    let completed: Vec<&crate::todo::TodoItem> = todos
        .iter()
        .filter(|todo| crate::todo::todo_status_is_completed(&todo.status))
        .collect();
    if completed.is_empty() {
        return None;
    }

    let completion_confidence_needs_validation = completed
        .iter()
        .any(|todo| !crate::todo::completion_confidence_passes(todo.completion_confidence));
    let confidence_spike_detected =
        allow_confidence_spike_challenge && !crate::todo::spike_completed_todos(todos).is_empty();

    if !completion_confidence_needs_validation && !confidence_spike_detected {
        return None;
    }

    if completion_confidence_needs_validation {
        Some((crate::todo::build_todo_completion_continuation_message(todos), false))
    } else {
        Some((
            crate::todo::build_todo_confidence_spike_continuation_message(todos),
            true,
        ))
    }
}

pub(super) async fn run_single_message_command_plain_with_auto_poke(
    agent: &mut crate::agent::Agent,
    message: &str,
    print_thoughts: bool,
    deadline_elapsed: &AtomicBool,
) -> Result<()> {
    let mut next_message = message.to_string();
    let max_turns = run_command_auto_poke_max_turns();
    let mut turns_completed = 0usize;
    let mut confidence_spike_challenged = false;
    let mut gate_digest_delivered = false;
    let mut stall_guard = AutoPokeStallGuard::default();
    loop {
        thoughts::run_plain_turn(agent, &next_message, print_thoughts, deadline_elapsed).await?;
        if deadline_elapsed.load(Ordering::Relaxed) {
            break;
        }
        turns_completed += 1;
        if !run_command_auto_poke_enabled() {
            break;
        }
        let todos = run_todos(agent.session_id());
        let gate_digest = take_run_gate_digest_if_turn_ended(agent.session_id(), gate_digest_delivered, &todos);
        match build_run_auto_poke_follow_up_from_todos(&todos, confidence_spike_challenged, gate_digest) {
            Some(RunAutoPokeFollowUp::GateDigest { message }) => {
                stall_guard.reset_incomplete();
                if run_command_auto_poke_limit_reached(turns_completed, max_turns) {
                    if let Some(max_turns) = max_turns {
                        eprintln!(
                            "We stopped poking after {max_turns} turn(s); some quality-review points are still open."
                        );
                    }
                    break;
                }
                gate_digest_delivered = true;
                next_message = message;
                eprintln!(
                    "We asked the agent to double-check this turn's weak points. Set HORON_AGENT_RUN_AUTO_POKE=0 to disable."
                );
                continue;
            }
            Some(RunAutoPokeFollowUp::ConfidenceSummary {
                message,
                confidence_spike_challenge,
                ..
            }) => {
                stall_guard.reset_incomplete();
                if run_command_auto_poke_limit_reached(turns_completed, max_turns) {
                    if let Some(max_turns) = max_turns {
                        eprintln!(
                            "We stopped poking after {max_turns} turn(s); the agent's completion confidence still needs validation."
                        );
                    }
                    break;
                }
                confidence_spike_challenged |= confidence_spike_challenge;
                next_message = message;
                eprintln!(
                    "Todos are done. Asking the agent for a final confidence check. Set HORON_AGENT_RUN_AUTO_POKE=0 to disable."
                );
                continue;
            }
            Some(RunAutoPokeFollowUp::Incomplete { count, message }) => {
                if stall_guard.is_stalled_incomplete(&todos) {
                    eprintln!("Stopped auto-poke: {count} incomplete todo(s) remained unchanged after follow-up.");
                    break;
                }
                if run_command_auto_poke_limit_reached(turns_completed, max_turns) {
                    if let Some(max_turns) = max_turns {
                        eprintln!(
                            "We stopped poking after {max_turns} turn(s); {} todo(s) are still unfinished.",
                            count
                        );
                    }
                    break;
                }
                next_message = message;
                eprintln!(
                    "{} incomplete todo(s). We poked the agent for you. Set HORON_AGENT_RUN_AUTO_POKE=0 to disable.",
                    count
                );
            }
            None => break,
        }
    }
    Ok(())
}

pub(super) async fn run_single_message_command_capture_with_auto_poke(
    agent: &mut crate::agent::Agent,
    message: &str,
) -> Result<String> {
    let mut next_message = message.to_string();
    let max_turns = run_command_auto_poke_max_turns();
    let mut outputs = Vec::new();
    let mut turns_completed = 0usize;
    let mut confidence_spike_challenged = false;
    let mut gate_digest_delivered = false;
    let mut stall_guard = AutoPokeStallGuard::default();
    loop {
        outputs.push(agent.run_once_capture(&next_message).await?);
        turns_completed += 1;
        if !run_command_auto_poke_enabled() {
            break;
        }
        let todos = run_todos(agent.session_id());
        let gate_digest = take_run_gate_digest_if_turn_ended(agent.session_id(), gate_digest_delivered, &todos);
        match build_run_auto_poke_follow_up_from_todos(&todos, confidence_spike_challenged, gate_digest) {
            Some(RunAutoPokeFollowUp::GateDigest { message }) => {
                stall_guard.reset_incomplete();
                if run_command_auto_poke_limit_reached(turns_completed, max_turns) {
                    if let Some(max_turns) = max_turns {
                        eprintln!(
                            "We stopped poking after {max_turns} turn(s); some quality-review points are still open."
                        );
                    }
                    break;
                }
                gate_digest_delivered = true;
                next_message = message;
                eprintln!(
                    "We asked the agent to double-check this turn's weak points. Set HORON_AGENT_RUN_AUTO_POKE=0 to disable."
                );
                continue;
            }
            Some(RunAutoPokeFollowUp::ConfidenceSummary {
                message,
                confidence_spike_challenge,
                ..
            }) => {
                stall_guard.reset_incomplete();
                if run_command_auto_poke_limit_reached(turns_completed, max_turns) {
                    if let Some(max_turns) = max_turns {
                        outputs.push(format!(
                            "We stopped poking after {max_turns} turn(s); the agent's completion confidence still needs validation."
                        ));
                    }
                    break;
                }
                confidence_spike_challenged |= confidence_spike_challenge;
                next_message = message;
                continue;
            }
            Some(RunAutoPokeFollowUp::Incomplete { count, message }) => {
                if stall_guard.is_stalled_incomplete(&todos) {
                    outputs.push(format!(
                        "Stopped auto-poke: {count} incomplete todo(s) remained unchanged after follow-up."
                    ));
                    break;
                }
                if run_command_auto_poke_limit_reached(turns_completed, max_turns) {
                    if let Some(max_turns) = max_turns {
                        outputs.push(format!(
                            "We stopped poking after {max_turns} turn(s); {} todo(s) are still unfinished.",
                            count
                        ));
                    }
                    break;
                }
                next_message = message;
            }
            None => break,
        }
    }
    Ok(outputs.join("\n\n"))
}

#[cfg(test)]
#[path = "auto_poke_tests.rs"]
mod tests;
