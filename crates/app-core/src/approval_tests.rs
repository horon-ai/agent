use super::*;
use crate::config::ApprovalMode;

fn no_dual() -> Vec<String> {
    Vec::new()
}

#[test]
fn off_mode_never_asks() {
    for effects in [
        ToolEffects::read(),
        ToolEffects::write(),
        ToolEffects::process(),
        ToolEffects::write().union(ToolEffects::process()),
    ] {
        let evaluation = evaluate("bash", effects, ApprovalMode::Off, &no_dual(), None, None);
        assert!(
            evaluation.is_auto_approved(),
            "off mode must never ask, got {evaluation:?}"
        );
        assert_eq!(evaluation.reason(), "approval disabled");
    }
}

#[test]
fn write_mode_auto_approves_file_mutation_only() {
    let dual = no_dual();

    for effects in [
        ToolEffects::write(),
        ToolEffects::append(),
        ToolEffects::write().union(ToolEffects::append()),
    ] {
        let evaluation = evaluate("write", effects, ApprovalMode::Write, &dual, None, None);
        assert!(
            evaluation.is_auto_approved(),
            "a plain file mutation must run in write mode, got {evaluation:?}"
        );
    }

    for (tool, effects) in [
        ("bash", ToolEffects::write().union(ToolEffects::process())),
        ("bash", ToolEffects::process()),
        ("webfetch_write", ToolEffects::write().union(ToolEffects::network())),
    ] {
        let evaluation = evaluate(tool, effects, ApprovalMode::Write, &dual, None, None);
        assert!(
            matches!(evaluation, ApprovalEvaluation::RequiresApproval { .. }),
            "{tool} with {effects:?} must ask in write mode, got {evaluation:?}"
        );
        assert!(
            !matches!(
                evaluation,
                ApprovalEvaluation::RequiresApproval {
                    is_dual_confirm: true,
                    ..
                }
            ),
            "an ordinary write-mode refusal is not a dual-confirm refusal"
        );
    }
}

#[test]
fn all_mode_auto_approves_execution() {
    for effects in [
        ToolEffects::process(),
        ToolEffects::write().union(ToolEffects::process()),
        ToolEffects::network(),
        ToolEffects::read(),
    ] {
        let evaluation = evaluate("bash", effects, ApprovalMode::All, &no_dual(), None, None);
        assert!(
            evaluation.is_auto_approved(),
            "all mode must auto-approve {effects:?}, got {evaluation:?}"
        );
    }
}

#[test]
fn read_only_calls_never_ask_in_any_mode() {
    for mode in [ApprovalMode::Off, ApprovalMode::Write, ApprovalMode::All] {
        let evaluation = evaluate("read", ToolEffects::read(), mode, &no_dual(), None, None);
        assert!(
            evaluation.is_auto_approved(),
            "{mode:?} must not ask for a pure read, got {evaluation:?}"
        );
    }
}

#[test]
fn network_only_call_asks_in_write_mode() {
    let evaluation = evaluate(
        "webfetch",
        ToolEffects::read().union(ToolEffects::network()),
        ApprovalMode::Write,
        &no_dual(),
        None,
        None,
    );
    assert!(
        matches!(evaluation, ApprovalEvaluation::RequiresApproval { .. }),
        "write mode covers file mutation only, so a network call asks: {evaluation:?}"
    );
}

#[test]
fn dual_confirm_wins_over_every_mode() {
    let dual = vec![DESTRUCTIVE_SHELL.to_string()];

    for mode in [ApprovalMode::Off, ApprovalMode::Write, ApprovalMode::All] {
        let evaluation = evaluate("bash", ToolEffects::read(), mode, &dual, Some(DESTRUCTIVE_SHELL), None);
        assert_eq!(
            evaluation,
            ApprovalEvaluation::RequiresApproval {
                reason: "destructive-shell is a dual-confirm class".to_string(),
                is_dual_confirm: true,
            },
            "{mode:?} must ask when the class is listed for dual confirmation"
        );
    }

    let unrelated = evaluate(
        "bash",
        ToolEffects::process(),
        ApprovalMode::All,
        &dual,
        Some("noisy-shell"),
        None,
    );
    assert!(
        unrelated.is_auto_approved(),
        "a class that is not listed must not trigger dual confirmation: {unrelated:?}"
    );
}

#[test]
fn danger_class_reuses_the_command_risk_verdict() {
    let catastrophic = serde_json::json!({ "command": "rm -rf /" });
    assert_eq!(
        danger_class("bash", &catastrophic, None),
        Some(DESTRUCTIVE_SHELL),
        "a command the classifier refuses must name a class"
    );

    let harmless = serde_json::json!({ "command": "ls" });
    assert_eq!(danger_class("bash", &harmless, None), None);

    let write = serde_json::json!({ "path": "a.txt", "content": "x" });
    assert_eq!(
        danger_class("write", &write, None),
        None,
        "only a shell command names a risk class"
    );
}

#[test]
fn auto_approval_is_audited_with_its_mode() {
    let call_id = "audit-write-mode-call";
    let evaluation = evaluate_and_audit(
        "write",
        ToolEffects::write(),
        ApprovalMode::Write,
        &no_dual(),
        None,
        None,
        call_id,
    );
    assert!(evaluation.is_auto_approved());

    let records = audit_records_for(call_id);
    assert_eq!(records.len(), 1, "one auto-approval must write one audit record");
    let record = &records[0];
    assert_eq!(record.tool, "write");
    assert_eq!(record.mode, "write");
    assert_eq!(record.reason, "file mutation in write mode");
    assert_eq!(record.tool_call_id, call_id);
}

#[test]
fn off_mode_writes_no_audit_line_for_reads_or_writes() {
    for (index, effects) in [ToolEffects::read(), ToolEffects::write()].into_iter().enumerate() {
        let call_id = format!("off-mode-call-{index}");
        let evaluation = evaluate_and_audit("write", effects, ApprovalMode::Off, &no_dual(), None, None, &call_id);
        assert!(evaluation.is_auto_approved());
        assert!(
            audit_records_for(&call_id).is_empty(),
            "off mode must write no audit line, but {call_id} produced one"
        );
    }
}

/// The approval gate and the shell gate must rate the same command the same way.
///
/// N1: `danger_class` built its `RiskContext` without `scratch_dir` while
/// `bash_destructive_gate` sets it, so one command could rate `Confirm` in the
/// approval gate and `Low` at the shell gate that runs it.
///
/// `scratch_dir` is consumed in exactly one place - `expand_known_prefix`
/// (command-risk/src/paths.rs:103) expands a leading `$HORON_AGENT_SCRATCH_DIR` -
/// so the command has to use that variable for the setting to matter. An absolute
/// literal path is rated the same with or without the context, which is why an
/// earlier version of this test passed even with the fix removed.
///
/// This asserts the difference directly: with the scratch path set the command
/// resolves inside a bounded directory and is not destructive; with it unset the
/// variable cannot be expanded and the classifier cannot bound it.
#[test]
fn the_approval_gate_assesses_the_same_scratch_path_the_shell_receives() {
    // The comparison is only meaningful where the shell is handed a scratch
    // directory at all.
    if !horon_agent_platform::term::passes_scratch_dir_to_shell() {
        return;
    }
    // `RiskContext::from_env` reads `HORON_AGENT_SCRATCH_DIR` directly, so if the
    // test process has it exported the control below agrees with the wired case
    // and the assertion goes vacuous: deleting the fix would still pass. Clear it
    // for the duration so the two branches cannot coincide.
    let previous_scratch = std::env::var_os("HORON_AGENT_SCRATCH_DIR");
    crate::env::remove_var("HORON_AGENT_SCRATCH_DIR");
    struct RestoreScratch(Option<std::ffi::OsString>);
    impl Drop for RestoreScratch {
        fn drop(&mut self) {
            if let Some(value) = self.0.take() {
                crate::env::set_var("HORON_AGENT_SCRATCH_DIR", value);
            }
        }
    }
    let _restore = RestoreScratch(previous_scratch);

    let scratch = super::super::tool::tool_scratch_dir_for_risk();
    assert!(
        scratch.is_some(),
        "a scratch directory must resolve, or both gates see None and the \
         disagreement N1 named cannot be tested"
    );

    let input = serde_json::json!({
        "command": "rm -rf $HORON_AGENT_SCRATCH_DIR/old-run"
    });

    // The gate's verdict: with the scratch path wired in, the target resolves
    // under a bounded directory.
    let wired = danger_class("bash", &input, None);
    assert_eq!(
        wired, None,
        "with the scratch path set, a write under it is bounded and must not be \
         classed destructive; got {wired:?}. If this returns Some, danger_class \
         stopped reading the scratch path and N1 is back."
    );

    // The control: the same command with the context the classifier builds when
    // scratch_dir is absent. This is what the gate saw before the fix, and it has
    // to differ - otherwise the assertion above proves nothing.
    let unbounded = {
        let mut context = horon_agent_command_risk::RiskContext::from_env(None);
        context.scratch_dir = None;
        horon_agent_command_risk::assess("rm -rf $HORON_AGENT_SCRATCH_DIR/old-run", &context)
    };
    assert!(
        !unbounded.level.runs_immediately(),
        "without the scratch path the same command must be treated as unbounded, \
         which is the disagreement the fix removes; got {:?}",
        unbounded.level
    );
}

/// A gate-queued approval record must not be picked up by the ambient resume
/// sweep.
///
/// F2 raised this as unmeasured: `bd05ac3` writes a `BlockedApproval` so the CLI
/// can resolve a gate request, and the ambient runner sweeps that same store.
/// `classify_blocked_approval` returns `Resume` for a recorded approval, which
/// runs a whole resume turn - re-running the model's turn for a call the user
/// approved once. A terminal `outcome` short-circuits it to `Idle`.
///
/// The record under test is read back from the store after the production
/// function wrote it. Constructing it here would assert this test's own fields
/// rather than the gate's, which is the defect class this round kept finding.
#[test]
fn a_gate_approval_record_is_not_resumed_by_the_ambient_sweep() {
    use crate::ambient::BlockedApprovalStore;
    use crate::ambient::resume::{ResumeVerdict, classify_blocked_approval};
    use crate::safety::Decision;
    use chrono::Utc;

    let _env = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let previous_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", home.path());
    crate::config::invalidate_config_cache();

    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => crate::env::set_var("HORON_HOME", value),
                None => crate::env::remove_var("HORON_HOME"),
            }
            crate::config::invalidate_config_cache();
        }
    }
    let _guard = RestoreHome(previous_home);

    let session = "gate-resume-sweep";
    let request_id = crate::safety::new_request_id();
    crate::tool::Registry::record_blocked_approval_for_test(session, &request_id);

    let store = BlockedApprovalStore::open_default().expect("the store the gate wrote to");
    let written = store
        .get(&request_id)
        .expect("the store read succeeds")
        .expect("the gate must have written a record");

    assert!(
        matches!(
            written.outcome.as_deref(),
            Some("resumed") | Some("denied") | Some("timeout_denied") | Some("expired")
        ),
        "the gate's record needs a terminal outcome or the sweep will resume it; \
         got {:?}",
        written.outcome
    );

    let now = Utc::now();
    let decision = Decision {
        request_id: request_id.clone(),
        approved: true,
        decided_at: now,
        decided_via: "api_client".to_string(),
        message: None,
    };
    assert_eq!(
        classify_blocked_approval(&written, Some(&decision), true, None, now),
        ResumeVerdict::Idle,
        "a gate record must not be resumed even when its decision is approved"
    );

    // The control: the same record with the outcome cleared is what the gate
    // wrote before the fix, and it IS resumed. The two verdicts have to differ,
    // or the assertion above proves nothing.
    let unmarked = crate::ambient::BlockedApproval {
        outcome: None,
        ..written.clone()
    };
    assert!(
        matches!(
            classify_blocked_approval(&unmarked, Some(&decision), true, None, now),
            ResumeVerdict::Resume { .. }
        ),
        "without the terminal outcome the sweep would resume it; this is the \
         behaviour the marker prevents"
    );
}

fn pattern_rule(pattern: &str, approval: BashPatternApproval) -> BashPatternRule {
    BashPatternRule {
        pattern: pattern.to_string(),
        approval,
    }
}

#[test]
fn bash_patterns_match_the_whole_command_and_each_segment() {
    let rules = [pattern_rule("git push *", BashPatternApproval::Deny)];
    assert_eq!(
        match_bash_pattern("git push origin main", &rules).map(|rule| rule.approval),
        Some(BashPatternApproval::Deny)
    );
    assert!(
        match_bash_pattern("cd repo && git push origin main", &rules).is_some(),
        "a rule must match a segment, not only the whole line"
    );
    assert!(
        match_bash_pattern("git pull", &rules).is_none(),
        "an unrelated command must not match"
    );

    let typed = [pattern_rule("git  push\torigin *", BashPatternApproval::Prompt)];
    assert_eq!(
        match_bash_pattern("git push origin main", &typed).map(|rule| rule.approval),
        Some(BashPatternApproval::Prompt),
        "pattern and command whitespace are normalized before matching"
    );

    let literal = [pattern_rule("cargo publish", BashPatternApproval::Deny)];
    assert!(
        match_bash_pattern("cargo publishx", &literal).is_none(),
        "a pattern without * must match the whole command, not a prefix"
    );
}

#[test]
fn bash_pattern_allow_rejects_commands_that_can_start_new_syntax() {
    let rules = [pattern_rule("git status*", BashPatternApproval::Allow)];
    assert!(match_bash_pattern("git status --short", &rules).is_some());
    for command in [
        "git status && rm -rf build",
        "git status; ls",
        "git status | cat",
        "git status > out.txt",
        "git status $(whoami)",
        "git status `whoami`",
        "git status\nls",
        "git status \\",
    ] {
        assert!(
            match_bash_pattern(command, &rules).is_none(),
            "allow must not cover a command carrying shell syntax: {command:?}"
        );
    }
}

#[test]
fn the_first_matching_bash_pattern_wins() {
    let rules = [
        pattern_rule("cargo publish*", BashPatternApproval::Allow),
        pattern_rule("cargo publish*", BashPatternApproval::Deny),
    ];
    assert_eq!(
        match_bash_pattern("cargo publish --dry-run", &rules).map(|rule| rule.approval),
        Some(BashPatternApproval::Allow),
        "config order decides, not severity"
    );
}

#[test]
fn a_prompt_pattern_asks_even_in_off_mode() {
    let verdict = BashPatternVerdict {
        pattern: "git push *".to_string(),
        approval: BashPatternApproval::Prompt,
    };
    assert_eq!(
        evaluate(
            "bash",
            ToolEffects::process(),
            ApprovalMode::Off,
            &no_dual(),
            None,
            Some(&verdict)
        ),
        ApprovalEvaluation::RequiresApproval {
            reason: "bash pattern 'git push *' requires approval".to_string(),
            is_dual_confirm: false,
        }
    );
}

#[test]
fn an_allow_pattern_approves_a_bash_call() {
    let verdict = BashPatternVerdict {
        pattern: "git status*".to_string(),
        approval: BashPatternApproval::Allow,
    };
    let evaluation = evaluate(
        "bash",
        ToolEffects::write().union(ToolEffects::process()),
        ApprovalMode::Write,
        &no_dual(),
        None,
        Some(&verdict),
    );
    assert_eq!(
        evaluation,
        ApprovalEvaluation::AutoApproved {
            reason: "bash pattern 'git status*' allows it".to_string(),
        }
    );
}

#[test]
fn a_bash_pattern_cannot_override_dual_confirm() {
    let dual = vec![DESTRUCTIVE_SHELL.to_string()];
    let verdict = BashPatternVerdict {
        pattern: "rm *".to_string(),
        approval: BashPatternApproval::Allow,
    };
    let evaluation = evaluate(
        "bash",
        ToolEffects::process(),
        ApprovalMode::All,
        &dual,
        Some(DESTRUCTIVE_SHELL),
        Some(&verdict),
    );
    assert_eq!(
        evaluation,
        ApprovalEvaluation::RequiresApproval {
            reason: "destructive-shell is a dual-confirm class".to_string(),
            is_dual_confirm: true,
        },
        "a dual-confirm class outranks an allow pattern"
    );
}
