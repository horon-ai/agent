//! What the live work summary promises: one session at a time, a bounded
//! number of lines, a default-off switch, and counters that agree with what
//! was drawn.

use horon_agent_sdk::api::ApiEvent;

use super::{MAX_LIVE_NOTES, WorkNotes};

fn tool_start(session: &str, call: &str) -> ApiEvent {
    ApiEvent::ToolStart {
        session_id: session.to_string(),
        call_id: call.to_string(),
        name: "bash".to_string(),
    }
}

fn tool_done(session: &str, call: &str) -> ApiEvent {
    ApiEvent::ToolDone {
        session_id: session.to_string(),
        call_id: call.to_string(),
        name: "bash".to_string(),
        output: String::new(),
        error: None,
        verdict: None,
    }
}

fn progress(session: &str, task: &str, summary: &str) -> ApiEvent {
    ApiEvent::BackgroundProgress {
        session_id: session.to_string(),
        task_id: task.to_string(),
        label: "build".to_string(),
        percent: None,
        summary: summary.to_string(),
        done: false,
    }
}

fn turn_done(session: &str) -> ApiEvent {
    ApiEvent::TurnDone {
        session_id: session.to_string(),
        pending_soft_interrupts: None,
    }
}

#[test]
fn work_notes_are_off_by_default() {
    let defaults = crate::settings::Settings::defaults();
    assert_eq!(
        defaults.boolean("transcript.work_notes"),
        Some(false),
        "the work summary must stay off until a settings layer turns it on"
    );
    assert!(!WorkNotes::default().enabled, "the summary itself starts off");
}

#[test]
fn a_different_session_starts_the_counters_over() {
    let mut notes = WorkNotes::default();
    notes.set_enabled(true);

    notes.observe(&tool_start("session_a", "one"));
    notes.observe(&tool_done("session_a", "one"));
    notes.observe(&tool_start("session_a", "two"));
    notes.observe(&tool_done("session_a", "two"));

    // The session the client moved to has done nothing, so its turn has no
    // summary to draw and must not inherit the two calls above.
    assert_eq!(notes.observe(&turn_done("session_b")), None);

    notes.observe(&tool_start("session_b", "three"));
    notes.observe(&tool_done("session_b", "three"));
    let closing = notes.observe(&turn_done("session_b")).expect("its own turn summary");
    assert!(closing.contains("1 tool calls"), "session A's calls leaked into B: {closing}");
}

#[test]
fn the_cap_draws_four_notes_and_counts_the_rest() {
    let mut notes = WorkNotes::default();
    notes.set_enabled(true);
    let shown = (0..MAX_LIVE_NOTES + 2)
        .map(|index| notes.observe(&progress("session_a", "build", &format!("step {index}"))))
        .filter(|line| line.is_some())
        .count();
    assert_eq!(shown, MAX_LIVE_NOTES as usize);

    let closing = notes.observe(&turn_done("session_a")).expect("closing counters");
    assert!(
        closing.contains("2 notes omitted"),
        "the closing line must account for what the cap did not draw: {closing}"
    );
}

#[test]
fn nothing_is_read_or_drawn_while_the_setting_is_off() {
    let mut notes = WorkNotes::default();
    assert_eq!(notes.observe(&progress("session_a", "build", "step 1")), None);
    assert_eq!(notes.observe(&turn_done("session_a")), None);
    assert_eq!(notes.jobs.len(), 0, "the off path must not even track the job");

    // Turning it on mid-turn starts from the next event, with nothing stale.
    notes.set_enabled(true);
    let shown = notes.observe(&progress("session_a", "build", "step 2"));
    assert_eq!(shown.as_deref(), Some("work · build · step 2"));
}

#[test]
fn a_repeated_progress_tick_does_not_take_another_line() {
    let mut notes = WorkNotes::default();
    notes.set_enabled(true);
    assert!(notes.observe(&progress("session_a", "build", "42% · Running tests")).is_some());
    assert_eq!(
        notes.observe(&progress("session_a", "build", "42% · Running tests")),
        None,
        "the same tick twice is one visible change"
    );
    let closing = notes.observe(&turn_done("session_a")).expect("closing counters");
    assert!(!closing.contains("notes omitted"), "nothing was omitted: {closing}");
}

#[test]
fn turning_the_summary_off_drops_the_partial_turn() {
    let mut notes = WorkNotes::default();
    notes.set_enabled(true);
    notes.observe(&tool_start("session_a", "one"));
    notes.observe(&tool_done("session_a", "one"));

    notes.set_enabled(false);
    notes.set_enabled(true);
    // The next turn starts empty: a tool that finished while off is not
    // reported as this turn's work.
    assert_eq!(notes.observe(&turn_done("session_a")), None);
}
