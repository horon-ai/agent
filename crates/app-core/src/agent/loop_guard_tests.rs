use super::loop_guard::{LoopGuard, LoopGuardVerdict, canonical_signature};
use serde_json::{Value, json};

fn read(path: &str, offset: usize, limit: usize) -> Value {
    json!({ "file_path": path, "offset": offset, "limit": limit })
}

fn assert_allow(guard: &mut LoopGuard, tool: &str, input: &Value) {
    let verdict = guard.record_and_decide(tool, input);
    assert_eq!(
        verdict,
        LoopGuardVerdict::Allow,
        "expected a clean call for {tool} {input}"
    );
}

#[test]
fn identical_run_warns_refuses_then_stops() {
    let mut guard = LoopGuard::new();
    let input = json!({ "file_path": "src/lib.rs" });

    assert_allow(&mut guard, "read", &input);
    assert_allow(&mut guard, "read", &input);

    match guard.record_and_decide("read", &input) {
        LoopGuardVerdict::Warn(message) => {
            assert!(message.starts_with("<system-reminder>"), "{message}");
            assert!(message.contains("\"read\""), "{message}");
            assert!(message.contains("3 times"), "{message}");
        }
        other => panic!("the third identical call must warn, got {other:?}"),
    }

    match guard.record_and_decide("read", &input) {
        LoopGuardVerdict::Refuse(message) => {
            assert!(message.contains("\"read\""), "{message}");
            assert!(message.contains("src/lib.rs"), "{message}");
            assert!(message.contains("4 times"), "{message}");
        }
        other => panic!("the fourth identical call must be refused, got {other:?}"),
    }

    match guard.record_and_decide("read", &input) {
        LoopGuardVerdict::Stop(message) => {
            assert_eq!(message.lines().count(), 1, "{message}");
            assert!(message.contains("\"read\""), "{message}");
            assert!(message.contains("5 times"), "{message}");
        }
        other => panic!("the second refusal must stop the turn, got {other:?}"),
    }
}

#[test]
fn a_different_signature_after_a_refusal_does_not_stop() {
    let mut guard = LoopGuard::new();
    let repeated = json!({ "file_path": "same.rs" });

    // The ladder: the third identical call warns and the fourth is refusal #1.
    assert_allow(&mut guard, "read", &repeated);
    assert_allow(&mut guard, "read", &repeated);
    match guard.record_and_decide("read", &repeated) {
        LoopGuardVerdict::Warn(message) => assert!(message.contains("3 times"), "{message}"),
        other => panic!("the third identical call must warn, got {other:?}"),
    }
    match guard.record_and_decide("read", &repeated) {
        LoopGuardVerdict::Refuse(message) => assert!(message.contains("4 times"), "{message}"),
        other => panic!("the fourth identical call must be refusal #1, got {other:?}"),
    }

    // A different target breaks the run, so it executes, and it is not the
    // second refusal of the original signature.
    assert_allow(&mut guard, "read", &json!({ "file_path": "other.rs" }));

    // The identical run restarts at one, so no stop carries over.
    assert_allow(&mut guard, "read", &repeated);
}

#[test]
fn refusal_counts_survive_unrelated_calls_between_refusals() {
    let mut guard = LoopGuard::new();
    let repeated = json!({ "file_path": "same.rs" });

    assert_allow(&mut guard, "read", &repeated);
    assert_allow(&mut guard, "read", &repeated);
    assert!(matches!(
        guard.record_and_decide("read", &repeated),
        LoopGuardVerdict::Warn(_)
    ));
    assert!(matches!(
        guard.record_and_decide("read", &repeated),
        LoopGuardVerdict::Refuse(_)
    ));

    // An unrelated call breaks the identical run but not the refusal count.
    assert_allow(&mut guard, "read", &json!({ "file_path": "other.rs" }));

    assert_allow(&mut guard, "read", &repeated);
    assert_allow(&mut guard, "read", &repeated);
    assert!(matches!(
        guard.record_and_decide("read", &repeated),
        LoopGuardVerdict::Warn(_)
    ));
    assert!(matches!(
        guard.record_and_decide("read", &repeated),
        LoopGuardVerdict::Stop(_)
    ));
}

#[test]
fn intent_does_not_change_the_signature() {
    let first = json!({ "file_path": "a.rs", "intent": "inspect the file" });
    let second = json!({ "intent": "a completely different reason", "file_path": "a.rs" });
    let third = json!({ "file_path": "a.rs" });

    assert_eq!(canonical_signature(&first), canonical_signature(&second));
    assert_eq!(canonical_signature(&first), canonical_signature(&third));
    assert_eq!(canonical_signature(&json!({ "b": 1, "a": 2 })), "{\"a\":2,\"b\":1}");

    let mut guard = LoopGuard::new();
    assert_allow(&mut guard, "read", &first);
    assert_allow(&mut guard, "read", &second);
    assert!(matches!(
        guard.record_and_decide("read", &third),
        LoopGuardVerdict::Warn(_)
    ));
}

#[test]
fn paginated_reads_of_one_file_are_clean() {
    let mut guard = LoopGuard::new();
    for offset in [0, 100, 200, 300, 400] {
        assert_allow(&mut guard, "read", &read("big.rs", offset, 100));
    }
}

#[test]
fn paginating_one_file_in_repeating_pages_is_clean() {
    let mut guard = LoopGuard::new();
    for _ in 0..3 {
        for offset in [0, 100, 200] {
            assert_allow(&mut guard, "read", &read("big.rs", offset, 100));
        }
    }
}

#[test]
fn alternating_reads_of_two_files_are_clean() {
    let mut guard = LoopGuard::new();
    let first = read("a.rs", 0, 100);
    let second = read("b.rs", 0, 100);
    for _ in 0..3 {
        assert_allow(&mut guard, "read", &first);
        assert_allow(&mut guard, "read", &second);
    }
}

#[test]
fn reads_of_distinct_paths_are_clean() {
    let mut guard = LoopGuard::new();
    for name in ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"] {
        assert_allow(&mut guard, "read", &json!({ "file_path": name }));
    }
}

/// Pins the decision that a changed argument against one target is not a loop.
/// Polling one background id with a moving cursor, or reading one file at a
/// slightly different range, stays clean however long the run gets; only an
/// unchanged input is a repeat the guard has an opinion about.
#[test]
fn changed_arguments_against_one_target_are_clean() {
    let mut guard = LoopGuard::new();
    for cursor in 0..5 {
        assert_allow(&mut guard, "bash_output", &json!({ "id": "sh-7", "cursor": cursor }));
    }

    let mut guard = LoopGuard::new();
    for offset in [0, 10, 20, 30, 40] {
        assert_allow(&mut guard, "read", &read("one.rs", offset, 100));
    }
}

#[test]
fn records_outside_the_window_do_not_fire() {
    let mut guard = LoopGuard::new();
    let repeated = json!({ "file_path": "early.rs" });

    assert_allow(&mut guard, "read", &repeated);
    assert_allow(&mut guard, "read", &repeated);
    assert!(matches!(
        guard.record_and_decide("read", &repeated),
        LoopGuardVerdict::Warn(_)
    ));

    for index in 0..62 {
        assert_allow(
            &mut guard,
            "read",
            &json!({ "file_path": format!("distinct-{index}.rs") }),
        );
    }
    assert_eq!(guard.window_len(), 64);

    assert_allow(&mut guard, "read", &repeated);
}

#[test]
fn window_evicts_and_caps_the_reported_count() {
    let mut guard = LoopGuard::new();
    let input = json!({ "file_path": "same.rs" });
    for _ in 0..70 {
        guard.record_and_decide("read", &input);
    }
    assert_eq!(guard.window_len(), 64);

    match guard.record_and_decide("read", &input) {
        LoopGuardVerdict::Stop(message) => {
            assert!(message.contains("64 times"), "the window must cap the count: {message}");
            assert!(!message.contains("70 times"), "{message}");
        }
        other => panic!("a long identical run must stop, got {other:?}"),
    }
}

#[test]
fn three_period_cycle_with_non_distinct_targets_fires() {
    let mut guard = LoopGuard::new();
    let file = json!({ "file_path": "a.rs" });
    let listing = json!({ "path": "." });

    let mut verdicts = Vec::new();
    for _ in 0..3 {
        verdicts.push(guard.record_and_decide("read", &file));
        verdicts.push(guard.record_and_decide("bash", &listing));
        verdicts.push(guard.record_and_decide("read", &file));
    }

    assert!(
        verdicts[..8].iter().all(|verdict| *verdict == LoopGuardVerdict::Allow),
        "{verdicts:?}"
    );
    match &verdicts[8] {
        LoopGuardVerdict::Warn(message) => {
            assert!(message.contains("read -> bash -> read"), "{message}");
            assert!(message.contains("3 times"), "{message}");
        }
        other => panic!("the third repetition must warn, got {other:?}"),
    }
}

/// A cycle that keeps repeating escalates on the same ladder an identical run
/// does: the first detection warns, the next is refused, and the second
/// refusal ends the turn. Keyed on the cycle itself, so the phase rotation of
/// the same loop does not restart the ladder.
#[test]
fn repeating_cycle_warns_then_refuses_then_stops() {
    let mut guard = LoopGuard::new();
    let file = json!({ "file_path": "a.rs" });
    let listing = json!({ "path": "." });

    let mut verdicts = Vec::new();
    // Three repetitions of the two-call cycle: the sixth call is the one that
    // first detects it.
    for _ in 0..3 {
        verdicts.push(guard.record_and_decide("read", &file));
        verdicts.push(guard.record_and_decide("bash", &listing));
    }

    assert!(
        verdicts[..5].iter().all(|verdict| *verdict == LoopGuardVerdict::Allow),
        "the first two repetitions are ordinary work: {verdicts:?}"
    );
    // Captured by default; `--nocapture` prints the ladder actually observed.
    println!("cycle ladder so far: {verdicts:?}");
    match &verdicts[5] {
        LoopGuardVerdict::Warn(message) => {
            assert!(message.contains("read -> bash"), "{message}");
            assert!(message.contains("3 times"), "{message}");
        }
        other => panic!("the third repetition must warn, got {other:?}"),
    }

    // The loop continues with its phase rotated: the same cycle, so the next
    // detection is a refusal rather than another warning.
    match guard.record_and_decide("read", &file) {
        LoopGuardVerdict::Refuse(message) => {
            assert!(message.contains("bash -> read"), "{message}");
            assert!(message.contains("Change the input"), "{message}");
        }
        other => panic!("the first repeat after the warning must be refused, got {other:?}"),
    }

    match guard.record_and_decide("bash", &listing) {
        LoopGuardVerdict::Stop(message) => {
            assert_eq!(message.lines().count(), 1, "{message}");
            assert!(message.contains("read -> bash"), "{message}");
            assert!(message.contains("refused twice"), "{message}");
        }
        other => panic!("the second cycle refusal must stop the turn, got {other:?}"),
    }
}

/// Changing one step of the cycle ends it: the next call executes instead of
/// inheriting the refusal, exactly as a changed signature does for an
/// identical run.
#[test]
fn changing_one_step_of_a_refused_cycle_is_allowed() {
    let mut guard = LoopGuard::new();
    let file = json!({ "file_path": "a.rs" });
    let listing = json!({ "path": "." });

    for _ in 0..3 {
        guard.record_and_decide("read", &file);
        guard.record_and_decide("bash", &listing);
    }
    assert!(matches!(
        guard.record_and_decide("read", &file),
        LoopGuardVerdict::Refuse(_)
    ));

    assert_allow(&mut guard, "bash", &json!({ "command": "ls -la" }));
}

/// The ladder is per turn: `reset` clears the cycle counters along with the
/// window, so the next turn starts clean instead of stopping on a loop the
/// model already broke.
#[test]
fn cycle_ladder_is_cleared_by_reset() {
    let mut guard = LoopGuard::new();
    let file = json!({ "file_path": "a.rs" });
    let listing = json!({ "path": "." });

    for _ in 0..3 {
        guard.record_and_decide("read", &file);
        guard.record_and_decide("bash", &listing);
    }
    assert!(matches!(
        guard.record_and_decide("read", &file),
        LoopGuardVerdict::Refuse(_)
    ));

    guard.reset();
    assert_eq!(guard.window_len(), 0);

    let mut verdicts = Vec::new();
    for _ in 0..3 {
        verdicts.push(guard.record_and_decide("read", &file));
        verdicts.push(guard.record_and_decide("bash", &listing));
    }
    assert!(
        verdicts[..5].iter().all(|verdict| *verdict == LoopGuardVerdict::Allow),
        "a fresh turn must not inherit the previous ladder: {verdicts:?}"
    );
    assert!(
        matches!(verdicts[5], LoopGuardVerdict::Warn(_)),
        "the new turn starts at the warning tier again: {:?}",
        verdicts[5]
    );
    assert!(
        matches!(guard.record_and_decide("read", &file), LoopGuardVerdict::Refuse(_)),
        "and then climbs the ladder in order"
    );
}

/// The verdicts of `count` identical calls, for the poll tests below.
fn verdicts_of(guard: &mut LoopGuard, tool: &str, input: &Value, count: usize) -> Vec<LoopGuardVerdict> {
    (0..count).map(|_| guard.record_and_decide(tool, input)).collect()
}

fn warnings(verdicts: &[LoopGuardVerdict]) -> Vec<&String> {
    verdicts
        .iter()
        .filter_map(|verdict| match verdict {
            LoopGuardVerdict::Warn(message) => Some(message),
            _ => None,
        })
        .collect()
}

fn assert_never_refused_or_stopped(verdicts: &[LoopGuardVerdict], what: &str) {
    for verdict in verdicts {
        assert!(
            matches!(verdict, LoopGuardVerdict::Allow | LoopGuardVerdict::Warn(_)),
            "{what} must keep running, got {verdict:?}"
        );
    }
}

/// Four identical status reads of one background task are how waiting looks:
/// the input does not change, the answer does. The third call is the single
/// reminder and every later one still executes. Before the poll exemption the
/// fourth was refused and the fifth ended the turn, orphaning the task.
#[test]
fn identical_bg_polls_warn_once_and_never_refuse_or_stop() {
    let mut guard = LoopGuard::new();
    let input = json!({ "action": "status", "task_id": "task-7" });

    let verdicts = verdicts_of(&mut guard, "bg", &input, 6);
    assert_eq!(verdicts[0], LoopGuardVerdict::Allow, "{verdicts:?}");
    assert_eq!(verdicts[1], LoopGuardVerdict::Allow, "{verdicts:?}");
    let warnings = warnings(&verdicts);
    assert_eq!(warnings.len(), 1, "exactly one reminder: {verdicts:?}");
    assert!(warnings[0].contains("\"bg\""), "{}", warnings[0]);
    assert!(warnings[0].contains("3 times"), "{}", warnings[0]);
    assert!(
        !warnings[0].contains("will be refused"),
        "a poll must not be threatened with a refusal it will not get: {}",
        warnings[0]
    );
    assert_never_refused_or_stopped(&verdicts[3..], "a later poll");
}

/// Every `bg` action whose result moves on its own, not just `status`: the set
/// is the whole polling family, and the newest call is the one that decides.
#[test]
fn every_polling_bg_action_keeps_running() {
    for action in ["status", "wait", "output", "tail", "list"] {
        let mut guard = LoopGuard::new();
        let input = json!({ "action": action, "task_id": "task-7" });
        let verdicts = verdicts_of(&mut guard, "bg", &input, 6);

        assert_eq!(
            warnings(&verdicts).len(),
            1,
            "bg {action} must warn exactly once: {verdicts:?}"
        );
        assert_never_refused_or_stopped(&verdicts, &format!("bg {action}"));
    }
}

/// The two output readers of a background task, which grow between calls.
#[test]
fn identical_output_readers_keep_running() {
    for tool in ["bash_output", "task_output"] {
        let mut guard = LoopGuard::new();
        let input = json!({ "task_id": "task-7" });
        let verdicts = verdicts_of(&mut guard, tool, &input, 6);

        assert_eq!(
            warnings(&verdicts).len(),
            1,
            "{tool} must warn exactly once: {verdicts:?}"
        );
        assert_never_refused_or_stopped(&verdicts, tool);
    }
}

/// A remote job's status reached through the fixed MCP surface, the case the
/// default headless profile can actually hit: `mcp` is a core tool.
#[test]
fn identical_status_style_mcp_calls_keep_running() {
    let mut guard = LoopGuard::new();
    let input = json!({ "server": "jobs", "tool": "job_status", "arguments": { "id": "job-3" } });

    let verdicts = verdicts_of(&mut guard, "mcp_call", &input, 6);
    assert_eq!(warnings(&verdicts).len(), 1, "{verdicts:?}");
    assert_never_refused_or_stopped(&verdicts, "mcp_call job_status");
}

/// The same job reached natively, when deferred loading made the tool directly
/// callable: `mcp__<server>__<tool>`.
#[test]
fn a_native_mcp_status_call_keeps_running() {
    let mut guard = LoopGuard::new();
    let input = json!({ "id": "job-3" });

    let verdicts = verdicts_of(&mut guard, "mcp__jobs__job_status", &input, 6);
    assert_eq!(warnings(&verdicts).len(), 1, "{verdicts:?}");
    assert_never_refused_or_stopped(&verdicts, "mcp__jobs__job_status");
}

/// A `bg` action that changes state is not a poll, so it keeps the whole
/// ladder: the exemption must not become a hole for mutating calls.
#[test]
fn a_mutating_bg_action_keeps_the_full_ladder() {
    let mut guard = LoopGuard::new();
    let input = json!({ "action": "cancel", "task_id": "task-7" });

    let verdicts = verdicts_of(&mut guard, "bg", &input, 5);
    assert!(matches!(verdicts[2], LoopGuardVerdict::Warn(_)), "{verdicts:?}");
    assert!(matches!(verdicts[3], LoopGuardVerdict::Refuse(_)), "{verdicts:?}");
    assert!(matches!(verdicts[4], LoopGuardVerdict::Stop(_)), "{verdicts:?}");
}

/// `bg` `delivery`, `watch`, and `subscribe` are not reads. All three run the
/// same arm of the tool (`tool/bg.rs`, `"watch" | "delivery" | "subscribe"`),
/// which calls `update_delivery` on every invocation and re-arms the stall
/// watchdog when `stall_wake_seconds` is present, so a repeat is a write loop
/// and keeps the whole ladder. `status` is the contrast in the same test: its
/// input is constant and its answer moves, so it never refuses.
#[test]
fn bg_delivery_and_watch_keep_the_full_ladder_while_status_does_not() {
    for action in ["delivery", "watch", "subscribe"] {
        let mut guard = LoopGuard::new();
        let input = json!({ "action": action, "task_id": "task-7" });

        let verdicts = verdicts_of(&mut guard, "bg", &input, 5);
        assert!(
            matches!(verdicts[2], LoopGuardVerdict::Warn(_)),
            "bg {action} must warn at the third identical call: {verdicts:?}"
        );
        assert!(
            matches!(verdicts[3], LoopGuardVerdict::Refuse(_)),
            "bg {action} must be refused at the fourth: {verdicts:?}"
        );
        assert!(
            matches!(verdicts[4], LoopGuardVerdict::Stop(_)),
            "bg {action} must end the turn at the second refusal: {verdicts:?}"
        );
    }

    let mut guard = LoopGuard::new();
    let status = json!({ "action": "status", "task_id": "task-7" });
    let verdicts = verdicts_of(&mut guard, "bg", &status, 6);
    assert_eq!(
        warnings(&verdicts).len(),
        1,
        "a status poll leaves one reminder: {verdicts:?}"
    );
    assert_never_refused_or_stopped(&verdicts, "bg status");
}

/// A remote tool that writes is not a status reader either.
#[test]
fn a_non_status_mcp_tool_keeps_the_full_ladder() {
    let mut guard = LoopGuard::new();
    let input = json!({ "server": "fs", "tool": "write_file", "arguments": { "path": "a.txt" } });

    let verdicts = verdicts_of(&mut guard, "mcp_call", &input, 5);
    assert!(matches!(verdicts[2], LoopGuardVerdict::Warn(_)), "{verdicts:?}");
    assert!(matches!(verdicts[3], LoopGuardVerdict::Refuse(_)), "{verdicts:?}");
    assert!(matches!(verdicts[4], LoopGuardVerdict::Stop(_)), "{verdicts:?}");
}

/// Two different tools aimed at one path carry the same canonical input, so a
/// run keyed on the signature alone counted them as one call: `read`, then
/// `lsp_diagnostics`, then `read` warned that "read" had been called three
/// times in a row, and the fourth call was refused. A run is the same tool
/// with the same input.
#[test]
fn alternating_tools_on_one_path_never_warn() {
    let mut guard = LoopGuard::new();
    let input = json!({ "file_path": "src/lib.rs" });

    let mut verdicts = Vec::new();
    for _ in 0..4 {
        verdicts.push(guard.record_and_decide("read", &input));
        verdicts.push(guard.record_and_decide("lsp_diagnostics", &input));
    }

    assert!(
        verdicts.iter().all(|verdict| *verdict == LoopGuardVerdict::Allow),
        "alternating two tools over one path is comparison, not a loop: {verdicts:?}"
    );
}

/// A refusal belongs to the tool that earned it: it must not climb another
/// tool's ladder into a stop, and it must still end the turn for its own tool.
#[test]
fn one_tools_refusal_does_not_stop_another_tool() {
    let mut guard = LoopGuard::new();
    let input = json!({ "file_path": "src/lib.rs" });

    // `read`'s ladder: the third warns and the fourth is refusal #1.
    assert_allow(&mut guard, "read", &input);
    assert_allow(&mut guard, "read", &input);
    assert!(matches!(
        guard.record_and_decide("read", &input),
        LoopGuardVerdict::Warn(_)
    ));
    assert!(matches!(
        guard.record_and_decide("read", &input),
        LoopGuardVerdict::Refuse(_)
    ));

    // The same signature from the other tool starts at its own first refusal
    // instead of inheriting `read`'s count and stopping the turn.
    assert_allow(&mut guard, "lsp_diagnostics", &input);
    assert_allow(&mut guard, "lsp_diagnostics", &input);
    assert!(matches!(
        guard.record_and_decide("lsp_diagnostics", &input),
        LoopGuardVerdict::Warn(_)
    ));
    match guard.record_and_decide("lsp_diagnostics", &input) {
        LoopGuardVerdict::Refuse(message) => {
            assert!(message.contains("lsp_diagnostics"), "{message}");
            assert!(message.contains("4 times"), "{message}");
        }
        other => panic!("the other tool's first refusal must refuse, got {other:?}"),
    }

    // `read`'s own count survived the interleaved calls, so its next refusal
    // still ends the turn.
    assert_allow(&mut guard, "read", &input);
    assert_allow(&mut guard, "read", &input);
    assert!(matches!(
        guard.record_and_decide("read", &input),
        LoopGuardVerdict::Warn(_)
    ));
    assert!(matches!(
        guard.record_and_decide("read", &input),
        LoopGuardVerdict::Stop(_)
    ));
}

/// A cycle whose every member is a poll is the same waiting in a different
/// shape: one reminder, then the guard steps aside instead of refusing and
/// stopping the turn that is waiting for a changing answer.
#[test]
fn a_cycle_of_polls_warns_once_and_keeps_running() {
    let mut guard = LoopGuard::new();
    let status = json!({ "action": "status", "task_id": "task-7" });
    let output = json!({ "id": "task-7" });

    let mut verdicts = Vec::new();
    for _ in 0..5 {
        verdicts.push(guard.record_and_decide("bg", &status));
        verdicts.push(guard.record_and_decide("bash_output", &output));
    }

    assert!(
        verdicts[..5].iter().all(|verdict| *verdict == LoopGuardVerdict::Allow),
        "the first two repetitions are ordinary waiting: {verdicts:?}"
    );
    match &verdicts[5] {
        LoopGuardVerdict::Warn(message) => {
            assert!(message.contains("bg -> bash_output"), "{message}");
            assert!(message.contains("will not refuse"), "{message}");
        }
        other => panic!("the third repetition must leave one reminder, got {other:?}"),
    }
    assert_eq!(
        warnings(&verdicts[6..]).len(),
        0,
        "one reminder is the whole response: {verdicts:?}"
    );
    assert_never_refused_or_stopped(&verdicts[6..], "a polling cycle after its reminder");
}

/// A cycle that mixes a poll with a real repeat keeps the full ladder: only a
/// cycle made entirely of polls is waiting.
#[test]
fn a_cycle_with_a_non_poll_member_still_escalates() {
    let mut guard = LoopGuard::new();
    let status = json!({ "action": "status", "task_id": "task-7" });
    let shell = json!({ "command": "true" });

    let mut verdicts = Vec::new();
    for _ in 0..3 {
        verdicts.push(guard.record_and_decide("bg", &status));
        verdicts.push(guard.record_and_decide("bash", &shell));
    }

    assert!(matches!(verdicts[5], LoopGuardVerdict::Warn(_)), "{verdicts:?}");
    assert!(matches!(
        guard.record_and_decide("bg", &status),
        LoopGuardVerdict::Refuse(_)
    ));
    assert!(matches!(
        guard.record_and_decide("bash", &shell),
        LoopGuardVerdict::Stop(_)
    ));
}

/// A poll run in the window must not soften the ladder for the calls the guard
/// exists to stop: the repeated read after the polls still warns, refuses, and
/// ends the turn.
#[test]
fn polls_before_a_repeated_read_do_not_soften_it() {
    let mut guard = LoopGuard::new();
    let poll = json!({ "action": "status", "task_id": "task-7" });
    for _ in 0..5 {
        guard.record_and_decide("bg", &poll);
    }

    let input = json!({ "file_path": "src/lib.rs" });
    assert_allow(&mut guard, "read", &input);
    assert_allow(&mut guard, "read", &input);
    assert!(matches!(
        guard.record_and_decide("read", &input),
        LoopGuardVerdict::Warn(_)
    ));
    assert!(matches!(
        guard.record_and_decide("read", &input),
        LoopGuardVerdict::Refuse(_)
    ));
    assert!(matches!(
        guard.record_and_decide("read", &input),
        LoopGuardVerdict::Stop(_)
    ));
}

/// The turn boundary clears the poll state with everything else, so the next
/// turn starts its ladder at the warning tier again.
#[test]
fn reset_clears_the_poll_ladder() {
    let mut guard = LoopGuard::new();
    let poll = json!({ "action": "status", "task_id": "task-7" });
    let verdicts = verdicts_of(&mut guard, "bg", &poll, 6);
    assert_eq!(warnings(&verdicts).len(), 1, "{verdicts:?}");

    guard.reset();
    assert_eq!(guard.window_len(), 0);

    let verdicts = verdicts_of(&mut guard, "bg", &poll, 3);
    assert!(
        matches!(verdicts[2], LoopGuardVerdict::Warn(_)),
        "a fresh turn warns again instead of staying silent: {verdicts:?}"
    );
}

/// Todo 34(a): a mutating tool whose name uses a verb the list was missing was
/// classified as a status reader, so it warned once and then ran forever, while
/// `create_task` and `delete_task` were correctly refused. That asymmetry was
/// the bug: every verb that changes something must veto the polling reading.
#[test]
fn mutating_tools_named_with_a_missing_verb_keep_the_full_ladder() {
    let input = json!({ "id": "task-7" });
    let names = [
        "assign_task",
        "mark_task_done",
        "add_task",
        "append_log",
        "rotate_logs",
        "insert_row",
        "clear_cache",
        "flush_buffer",
        "purge_queue",
        "replace_file",
        "truncate_file",
        "ack_message",
    ];

    for name in names {
        let mut guard = LoopGuard::new();
        let tool = format!("mcp__jobs__{name}");
        let verdicts = verdicts_of(&mut guard, &tool, &input, 5);
        assert!(
            matches!(verdicts[2], LoopGuardVerdict::Warn(_)),
            "{name} must warn at the third identical call: {verdicts:?}"
        );
        assert!(
            matches!(verdicts[3], LoopGuardVerdict::Refuse(_)),
            "{name} must be refused at the fourth: {verdicts:?}"
        );
        assert!(
            matches!(verdicts[4], LoopGuardVerdict::Stop(_)),
            "{name} must end the turn at the second refusal: {verdicts:?}"
        );
    }
}

/// The same two names through the fixed MCP surface, which is the path the
/// default headless profile can actually hit.
#[test]
fn a_mutating_mcp_tool_through_the_fixed_surface_keeps_the_full_ladder() {
    for tool in ["assign_task", "mark_task_done"] {
        let mut guard = LoopGuard::new();
        let input = json!({ "server": "jobs", "tool": tool, "arguments": { "id": "task-7" } });
        let verdicts = verdicts_of(&mut guard, "mcp_call", &input, 5);
        std::println!("QA ladder for {tool}: {verdicts:?}");
        assert!(
            matches!(verdicts[2], LoopGuardVerdict::Warn(_)),
            "{tool} must warn at the third identical call: {verdicts:?}"
        );
        assert!(
            matches!(verdicts[3], LoopGuardVerdict::Refuse(_)),
            "{tool} must be refused at the fourth: {verdicts:?}"
        );
        assert!(
            matches!(verdicts[4], LoopGuardVerdict::Stop(_)),
            "{tool} must end the turn at the second refusal: {verdicts:?}"
        );
    }
}

/// Todo 33: the poll exemption is bounded. A model that keeps asking a
/// status-style tool is waiting, not stuck, so the identical-run ladder never
/// refuses it - and without a second net it never stops it either, so an
/// unattended daemon pays one full request per poll forever. The cap is that
/// net: it counts the consecutive polls themselves, ends the turn when they run
/// out, and names the cap in its notice.
#[test]
fn consecutive_polls_are_capped_and_the_cap_ends_the_turn() {
    let mut guard = LoopGuard::new();
    let poll = json!({ "action": "status", "task_id": "task-7" });

    let verdicts = verdicts_of(&mut guard, "bg", &poll, 25);

    // Todo 29 holds: four identical polls are ordinary waiting, and the ladder
    // still never refuses one.
    assert_never_refused_or_stopped(&verdicts[..24], "polls up to the cap");
    assert_eq!(
        warnings(&verdicts).len(),
        1,
        "one reminder, not one per poll: {verdicts:?}"
    );

    match &verdicts[24] {
        LoopGuardVerdict::Stop(message) => {
            std::println!("QA polling cap notice:\n{message}");
            assert!(message.contains("Loop guard ended the turn"), "{message}");
            assert!(message.contains("cap is 24"), "the notice names the cap: {message}");
            assert!(message.contains("25 times"), "the notice names the count: {message}");
            assert!(message.contains("\"bg\""), "the notice names the tool: {message}");
        }
        other => panic!("the 25th consecutive poll must end the turn, got {other:?}"),
    }
}

/// The cap counts polls, not identical polls: a model that changes one field on
/// every call still spends a full request per poll, so the ladder (which keys on
/// the signature) would never see it. This is the case the second net exists
/// for.
#[test]
fn the_polling_cap_counts_polls_whose_input_changes() {
    let mut guard = LoopGuard::new();
    let mut verdicts = Vec::new();
    for index in 0..25 {
        let poll = json!({ "action": "status", "task_id": format!("task-{index}") });
        verdicts.push(guard.record_and_decide("bg", &poll));
    }

    assert_never_refused_or_stopped(&verdicts[..24], "polls with distinct targets");
    assert!(
        matches!(verdicts[24], LoopGuardVerdict::Stop(_)),
        "distinct inputs must not evade the cap: {verdicts:?}"
    );
}

/// A call that is not a poll is real work, so it ends the run of polls: a turn
/// that alternates waiting with reading or running commands is not the pattern
/// the cap exists to stop.
#[test]
fn a_non_polling_call_resets_the_consecutive_poll_count() {
    let mut guard = LoopGuard::new();
    let poll = json!({ "action": "status", "task_id": "task-7" });

    let before = verdicts_of(&mut guard, "bg", &poll, 24);
    assert_never_refused_or_stopped(&before, "polls up to the cap");

    let read = json!({ "file_path": "src/lib.rs" });
    assert_eq!(guard.record_and_decide("read", &read), LoopGuardVerdict::Allow);

    // The allowance starts over, which is what proves the count was cleared
    // rather than merely not yet spent: the next poll is the first of a new run,
    // and the run's own 25th poll still ends the turn.
    let after = verdicts_of(&mut guard, "bg", &poll, 24);
    assert_never_refused_or_stopped(&after, "polls after real work");
    assert!(
        matches!(guard.record_and_decide("bg", &poll), LoopGuardVerdict::Stop(_)),
        "the fresh run must spend its own full allowance and then stop"
    );
}

/// The turn boundary clears the poll count with everything else, so a later
/// turn starts with its full allowance instead of inheriting a spent one.
#[test]
fn the_polling_cap_clears_at_the_turn_boundary() {
    let mut guard = LoopGuard::new();
    let poll = json!({ "action": "status", "task_id": "task-7" });

    let verdicts = verdicts_of(&mut guard, "bg", &poll, 25);
    assert!(matches!(verdicts[24], LoopGuardVerdict::Stop(_)), "{verdicts:?}");

    guard.reset();

    let verdicts = verdicts_of(&mut guard, "bg", &poll, 4);
    assert_never_refused_or_stopped(&verdicts, "the next turn's polls");
    assert_eq!(warnings(&verdicts).len(), 1, "{verdicts:?}");
}

/// `cycles` repetitions of a two-call alternation, oldest verdict first.
fn alternating_verdicts(
    guard: &mut LoopGuard,
    tool: &str,
    first: &Value,
    second: &Value,
    cycles: usize,
) -> Vec<LoopGuardVerdict> {
    let mut verdicts = Vec::with_capacity(cycles * 2);
    for _ in 0..cycles {
        verdicts.push(guard.record_and_decide(tool, first));
        verdicts.push(guard.record_and_decide(tool, second));
    }
    verdicts
}

/// One word per call, so the manual QA can show two ladders side by side.
fn verdict_sequence(verdicts: &[LoopGuardVerdict]) -> String {
    verdicts
        .iter()
        .map(|verdict| match verdict {
            LoopGuardVerdict::Allow => "Allow",
            LoopGuardVerdict::Warn(_) => "Warn",
            LoopGuardVerdict::Refuse(_) => "Refuse",
            LoopGuardVerdict::Stop(_) => "Stop",
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Todo 36 / IS-8: the cycle exemption is not "alternating two targets is
/// always fine". It applies only when every member of the cycle has a distinct
/// `target_identity`, and only `read`, `lsp_diagnostics`, `bash_output`,
/// `task_output` and `bg` supply one. The same shape - two distinct targets,
/// three repetitions - must therefore fire for `ls` and stay clean for `read`.
/// That difference is the documented boundary: widening `target_identity` to
/// `ls` breaks the first half, and dropping `read`'s target breaks the second.
#[test]
fn alternating_two_targets_fires_for_ls_but_stays_clean_for_read() {
    let ls_first = json!({ "path": "crates/app-core/src" });
    let ls_second = json!({ "path": "crates/app-core/tests" });
    let mut ls_guard = LoopGuard::new();
    let mut ls_verdicts = alternating_verdicts(&mut ls_guard, "ls", &ls_first, &ls_second, 3);

    assert!(
        ls_verdicts[..5]
            .iter()
            .all(|verdict| *verdict == LoopGuardVerdict::Allow),
        "the first two ls cycles are ordinary exploration: {ls_verdicts:?}"
    );
    match &ls_verdicts[5] {
        LoopGuardVerdict::Warn(message) => {
            assert!(message.contains("ls -> ls"), "{message}");
            assert!(message.contains("3 times"), "{message}");
        }
        other => panic!("`ls` has no target identity, so its third cycle must warn, got {other:?}"),
    }

    ls_verdicts.push(ls_guard.record_and_decide("ls", &ls_first));
    ls_verdicts.push(ls_guard.record_and_decide("ls", &ls_second));
    assert!(
        matches!(ls_verdicts[6], LoopGuardVerdict::Refuse(_)),
        "the next `ls` cycle must be refused: {ls_verdicts:?}"
    );
    assert!(
        matches!(ls_verdicts[7], LoopGuardVerdict::Stop(_)),
        "the second refusal must end the turn: {ls_verdicts:?}"
    );

    let read_first = json!({ "file_path": "crates/app-core/src/agent/loop_guard.rs" });
    let read_second = json!({ "file_path": "crates/app-core/src/agent/loop_guard_tests.rs" });
    let mut read_guard = LoopGuard::new();
    let read_verdicts = alternating_verdicts(&mut read_guard, "read", &read_first, &read_second, 4);

    assert!(
        read_verdicts.iter().all(|verdict| *verdict == LoopGuardVerdict::Allow),
        "two distinct file targets are comparison, not a loop: {read_verdicts:?}"
    );

    println!("QA alternating ls:   {}", verdict_sequence(&ls_verdicts));
    println!("QA alternating read: {}", verdict_sequence(&read_verdicts));
}

/// The boundary belongs to the exemption's scope, not to `ls`: a second tool
/// without a `target_identity` fires on the same shape. `webfetch` is included
/// because its input is a single `url`, the cheapest second witness available.
#[test]
fn alternating_webfetch_urls_fire_on_the_third_cycle() {
    let first = json!({ "url": "https://example.com/alpha" });
    let second = json!({ "url": "https://example.com/beta" });
    let mut guard = LoopGuard::new();
    let mut verdicts = alternating_verdicts(&mut guard, "webfetch", &first, &second, 3);

    assert!(
        verdicts[..5].iter().all(|verdict| *verdict == LoopGuardVerdict::Allow),
        "the first two webfetch cycles are ordinary exploration: {verdicts:?}"
    );
    match &verdicts[5] {
        LoopGuardVerdict::Warn(message) => {
            assert!(message.contains("webfetch -> webfetch"), "{message}");
            assert!(message.contains("3 times"), "{message}");
        }
        other => panic!("`webfetch` has no target identity, so its third cycle must warn, got {other:?}"),
    }

    verdicts.push(guard.record_and_decide("webfetch", &first));
    verdicts.push(guard.record_and_decide("webfetch", &second));
    assert!(
        matches!(verdicts[6], LoopGuardVerdict::Refuse(_)),
        "the next webfetch cycle must be refused: {verdicts:?}"
    );
    assert!(
        matches!(verdicts[7], LoopGuardVerdict::Stop(_)),
        "the second refusal must end the turn: {verdicts:?}"
    );

    println!("QA alternating webfetch: {}", verdict_sequence(&verdicts));
}
