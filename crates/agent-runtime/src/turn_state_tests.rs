use super::*;
use crate::turn_recovery::MAX_AUTO_CONTINUATIONS;
use crate::turn_state::MAX_RUN_STOP_HISTORY;

#[test]
fn run_identity_carries_the_minted_id_unchanged() {
    let identity = RunIdentity::new("0f8fad5b-d9cb-469f-a165-70867728950e");
    assert_eq!(identity.run_id(), "0f8fad5b-d9cb-469f-a165-70867728950e");
    assert_eq!(identity.clone(), identity, "a run identity is comparable and cloneable");
}

#[test]
fn stop_provenance_vocabulary_is_closed_and_parses_both_ways() {
    let labels: Vec<&str> = StopProvenance::ALL.iter().map(|cause| cause.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "completed",
            "user_cancelled",
            "shutdown",
            "provider_error",
            "timeout",
            "loop_guard",
            "limit",
            "interrupted_unknown",
        ]
    );
    for cause in StopProvenance::ALL {
        assert_eq!(StopProvenance::parse(cause.as_str()), Some(cause));
    }
    assert_eq!(StopProvenance::parse("Finished"), None);
}

#[test]
fn a_provider_failure_is_a_timeout_only_when_its_message_says_so() {
    assert_eq!(
        provider_failure_provenance("Initial response timeout: no response headers received within 30s"),
        StopProvenance::Timeout
    );
    assert_eq!(
        provider_failure_provenance("WebSocket connect timed out after 8s"),
        StopProvenance::Timeout
    );
    assert_eq!(
        provider_failure_provenance("connection reset by peer"),
        StopProvenance::ProviderError
    );
    assert_eq!(provider_failure_provenance(""), StopProvenance::ProviderError);
}

#[test]
fn a_recovered_stop_is_interrupted_unknown_and_never_persisted() {
    let stop = RunStop::recovered_interrupted("run-a");
    assert_eq!(stop.run_id(), "run-a");
    assert_eq!(stop.provenance(), StopProvenance::InterruptedUnknown);
    assert!(!stop.persisted());
    assert_ne!(stop.provenance(), StopProvenance::Completed);
}

#[test]
fn a_stop_is_unpersisted_until_the_journal_accepts_it() {
    let mut stop = RunStop::capture("run-a", StopProvenance::Completed);
    assert!(!stop.persisted());
    stop.mark_persisted();
    assert!(stop.persisted());
}

#[test]
fn a_late_stop_for_an_older_run_never_overwrites_the_current_one() {
    let mut ledger = RunLedger::default();
    ledger.begin_run(RunIdentity::new("run-a"), true);
    assert_eq!(
        ledger.record_stop(RunStop::capture("run-a", StopProvenance::Completed)),
        StopDisposition::Current
    );

    ledger.begin_run(RunIdentity::new("run-b"), true);
    assert_eq!(ledger.current().map(RunIdentity::run_id), Some("run-b"));
    assert!(ledger.current_stop().is_none(), "a new run starts with no stop");

    assert_eq!(
        ledger.record_stop(RunStop::capture("run-b", StopProvenance::ProviderError)),
        StopDisposition::Current
    );
    assert_eq!(
        ledger.record_stop(RunStop::capture("run-a", StopProvenance::Timeout)),
        StopDisposition::History,
        "run A's late stop cannot become current"
    );

    let current_stop = ledger.current_stop().expect("run B's stop survives");
    assert_eq!(current_stop.run_id(), "run-b");
    assert_eq!(current_stop.provenance(), StopProvenance::ProviderError);
    let history: Vec<&str> = ledger.history().iter().map(RunStop::run_id).collect();
    assert_eq!(
        history,
        vec!["run-a"],
        "run A has exactly one history entry, holding its late cause"
    );
    assert_eq!(
        ledger.history()[0].provenance(),
        StopProvenance::Timeout,
        "the late stop replaced A's earlier history record rather than adding a second one"
    );
}

#[test]
fn beginning_a_run_moves_the_previous_stop_to_history() {
    let mut ledger = RunLedger::default();
    ledger.begin_run(RunIdentity::new("run-a"), true);
    ledger.record_stop(RunStop::capture("run-a", StopProvenance::Completed));
    ledger.begin_run(RunIdentity::new("run-b"), false);
    assert_eq!(ledger.history().len(), 1);
    assert_eq!(ledger.history()[0].run_id(), "run-a");
    assert_eq!(ledger.history()[0].provenance(), StopProvenance::Completed);
}

#[test]
fn carrying_a_ledger_over_keeps_its_stops_but_not_its_current_run() {
    let mut previous = RunLedger::default();
    previous.begin_run(RunIdentity::new("run-a"), true);
    previous.record_stop(RunStop::capture("run-a", StopProvenance::Completed));
    previous.begin_run(RunIdentity::new("run-b"), true);

    let mut fresh = RunLedger::default();
    fresh.carry_over(&previous);
    fresh.begin_run(RunIdentity::new("run-c"), true);

    let carried: Vec<&str> = fresh.history().iter().map(RunStop::run_id).collect();
    assert_eq!(
        carried,
        vec!["run-a"],
        "a stop that happened survives the reset; a run that never stopped has none to carry"
    );
    assert_eq!(fresh.current().map(RunIdentity::run_id), Some("run-c"));
    assert!(fresh.current_stop().is_none());
}

#[test]
fn the_first_terminal_observation_is_the_recorded_cause() {
    let mut ledger = RunLedger::default();
    ledger.begin_run(RunIdentity::new("run-a"), true);
    assert_eq!(ledger.observed_cause(), None);
    ledger.observe_cause(StopProvenance::LoopGuard);
    ledger.observe_cause(StopProvenance::ProviderError);
    assert_eq!(ledger.observed_cause(), Some(StopProvenance::LoopGuard));
    ledger.begin_run(RunIdentity::new("run-b"), true);
    assert_eq!(ledger.observed_cause(), None, "a new run observes its own cause");
}

#[test]
fn a_second_stop_for_the_current_run_goes_to_history() {
    let mut ledger = RunLedger::default();
    ledger.begin_run(RunIdentity::new("run-a"), true);
    assert_eq!(
        ledger.record_stop(RunStop::capture("run-a", StopProvenance::Completed)),
        StopDisposition::Current
    );
    assert_eq!(
        ledger.record_stop(RunStop::capture("run-a", StopProvenance::Timeout)),
        StopDisposition::History
    );
    assert_eq!(
        ledger.current_stop().map(|stop| stop.provenance()),
        Some(StopProvenance::Completed)
    );
}

#[test]
fn history_keeps_one_record_per_run_and_stays_bounded() {
    let mut ledger = RunLedger::default();
    ledger.begin_run(RunIdentity::new("current"), true);
    for index in 0..(MAX_RUN_STOP_HISTORY + 8) {
        ledger.record_stop(RunStop::capture(format!("run-{index}"), StopProvenance::Completed));
    }
    assert_eq!(ledger.history().len(), MAX_RUN_STOP_HISTORY);
    ledger.record_stop(RunStop::capture("run-0", StopProvenance::ProviderError));
    assert_eq!(
        ledger.history().len(),
        MAX_RUN_STOP_HISTORY,
        "re-recording an existing run replaces rather than appends"
    );
}

#[test]
fn auto_continuations_start_at_zero_and_stop_at_the_cap() {
    let mut state = TurnState::new();
    assert_eq!(state.auto_continuations(), 0);
    for expected in 1..=MAX_AUTO_CONTINUATIONS {
        assert_eq!(state.record_auto_continuation(), expected);
    }
    assert_eq!(state.auto_continuations(), MAX_AUTO_CONTINUATIONS);
    assert_eq!(
        state.record_auto_continuation(),
        MAX_AUTO_CONTINUATIONS,
        "the automatic-continuation budget cannot be exceeded"
    );
}

#[test]
fn a_new_turn_resets_the_auto_continuation_budget() {
    let mut state = TurnState::new();
    state.record_auto_continuation();
    state.record_auto_continuation();
    let fresh = TurnState::new();
    assert_eq!(fresh.auto_continuations(), 0);
    assert_eq!(state.auto_continuations(), MAX_AUTO_CONTINUATIONS);
}

#[test]
fn counters_advance_and_reset_independently() {
    let mut state = TurnState::new();
    assert_eq!(state.context_limit_retries(), 0);
    assert_eq!(state.record_context_limit_retry(), 1);
    assert_eq!(state.record_context_limit_retry(), 2);
    state.reset_context_limit_retries();
    assert_eq!(state.context_limit_retries(), 0);
    assert_eq!(state.record_rate_limit_retry(), 1);
    assert_eq!(state.record_incomplete_continuation(), 1);
    assert_eq!(state.record_empty_post_tool_continuation(), 1);
    assert_eq!(state.record_fable_guardrail_reconsideration(), 1);
    assert_eq!(state.record_unparsed_tool_markup_nudge(), 1);
    assert_eq!(state.rate_limit_retries(), 1);
    assert_eq!(state.incomplete_continuations(), 1);
    assert_eq!(state.empty_post_tool_continuations(), 1);
    assert_eq!(state.fable_guardrail_reconsiderations(), 1);
    assert_eq!(state.unparsed_tool_markup_nudges(), 1);
}

#[test]
fn sequential_rounds_owe_a_nudge_only_after_the_threshold() {
    let mut state = TurnState::new();
    state.record_tool_round(1, false, 3);
    assert_eq!(state.sequential_single_tool_rounds(), 1);
    assert!(!state.batch_nudge_pending());
    state.record_tool_round(1, false, 3);
    assert!(!state.batch_nudge_pending());
    state.record_tool_round(1, false, 3);
    assert_eq!(state.sequential_single_tool_rounds(), 3);
    assert!(state.batch_nudge_pending());

    assert!(!state.take_batch_nudge(false));
    assert!(state.batch_nudge_pending());
    assert!(state.take_batch_nudge(true));
    assert!(!state.batch_nudge_pending());
    assert_eq!(state.sequential_single_tool_rounds(), 0);
}

#[test]
fn a_parallel_or_batch_round_clears_the_sequential_streak() {
    let mut state = TurnState::new();
    state.record_tool_round(1, false, 3);
    state.record_tool_round(1, false, 3);
    state.record_tool_round(2, false, 3);
    assert_eq!(state.sequential_single_tool_rounds(), 0);
    state.record_tool_round(1, false, 3);
    state.record_tool_round(1, true, 3);
    assert_eq!(state.sequential_single_tool_rounds(), 0);
    assert!(!state.batch_nudge_pending());
}

#[test]
fn malformed_rounds_advance_and_reset() {
    let mut state = TurnState::new();
    assert_eq!(state.record_malformed_tool_round(), 1);
    assert_eq!(state.record_malformed_tool_round(), 2);
    state.reset_consecutive_malformed_tool_rounds();
    assert_eq!(state.consecutive_malformed_tool_rounds(), 0);
}

#[test]
fn every_outcome_is_recorded_once() {
    for outcome in [
        TurnOutcome::Stop,
        TurnOutcome::Length,
        TurnOutcome::Error,
        TurnOutcome::Aborted,
    ] {
        let mut state = TurnState::new();
        assert_eq!(state.outcome(), None);
        state.finish(outcome).expect("the first outcome is accepted");
        assert_eq!(state.outcome(), Some(outcome));
        assert_eq!(
            state.finish(TurnOutcome::Stop),
            Err(TurnStateError::AlreadyFinished(outcome))
        );
    }
}

#[test]
fn stop_reasons_map_to_stop_or_length() {
    for reason in [None, Some(""), Some("end_turn"), Some("stop"), Some("tool_use")] {
        assert_eq!(outcome_for_stop_reason(reason), TurnOutcome::Stop, "{reason:?}");
    }
    for reason in ["length", "max_tokens", "max_output_tokens", "truncated"] {
        assert_eq!(outcome_for_stop_reason(Some(reason)), TurnOutcome::Length, "{reason}");
    }
}

#[test]
fn final_text_survives_a_completed_turn_and_is_refused_on_error() {
    let mut state = TurnState::new();
    state.set_final_text("partial");
    assert_eq!(state.final_text(), "partial");
    state.set_final_text("done");
    assert_eq!(state.final_text(), "done");
    state.finish(TurnOutcome::Stop).expect("text is allowed on Stop");

    let mut failed = TurnState::new();
    failed.set_final_text("half an answer");
    assert_eq!(
        failed.finish(TurnOutcome::Error),
        Err(TurnStateError::FinalTextNotAllowed(TurnOutcome::Error))
    );
    assert_eq!(failed.outcome(), None);
    failed.set_final_text("");
    failed.finish(TurnOutcome::Error).expect("an empty failure is accepted");
    assert_eq!(failed.outcome(), Some(TurnOutcome::Error));
}

#[test]
fn aborted_and_length_turns_may_carry_partial_text() {
    for outcome in [TurnOutcome::Length, TurnOutcome::Aborted] {
        let mut state = TurnState::new();
        state.set_final_text("partial");
        state.finish(outcome).expect("partial text is allowed");
        assert_eq!(state.outcome(), Some(outcome));
        assert_eq!(state.final_text(), "partial");
    }
}
