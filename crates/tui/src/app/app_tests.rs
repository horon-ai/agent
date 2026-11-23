//! Terminal app behavior under harness events.

use super::*;

fn app() -> App {
    let (_sender, receiver) = mpsc::channel();
    App::new("socket · session".to_string(), receiver)
}

#[test]
fn status_line_shows_model_and_state() {
    let mut app = app();
    app.apply_event(&ApiEvent::ModelInfo {
        session_id: "s1".into(),
        provider: Some("anthropic".into()),
        model: Some("claude-sonnet-4".into()),
        reasoning_effort: None,
        auth_method: None,
    });
    assert_eq!(
        app.status_line(),
        "socket · session · claude-sonnet-4 · anthropic · idle".to_string()
    );
    app.pending_turn = true;
    assert!(app.status_line().ends_with("running"));
}

#[test]
fn approval_resolves_only_for_its_own_request() {
    let mut app = app();
    app.apply_event(&ApiEvent::ApprovalPending {
        session_id: "s1".into(),
        request_id: "r1".into(),
        action: "bash".into(),
        expires_at: None,
    });
    app.apply_event(&ApiEvent::ApprovalResolved {
        session_id: "s1".into(),
        request_id: "other".into(),
        approved: true,
        via: "file".into(),
        resolved_by: "tool".into(),
    });
    assert!(app.prompt.is_some(), "an unrelated resolution closed the prompt");
    app.apply_event(&ApiEvent::ApprovalResolved {
        session_id: "s1".into(),
        request_id: "r1".into(),
        approved: false,
        via: "inline".into(),
        resolved_by: "tool".into(),
    });
    assert!(app.prompt.is_none());
}

#[test]
fn turn_stopped_clears_the_pending_flag() {
    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&ApiEvent::TurnStopped {
        session_id: "s1".into(),
        reason: horon_agent_sdk::api::TurnStopReason::Failure,
        message: "provider failed".into(),
        provider_stop_reason: None,
    });
    assert!(!app.pending_turn);
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| item == &crate::transcript::Item::Warning("provider failed".to_string()))
    );
}

/// The settings a global `tui.toml` holding `text` resolves to.
fn settings_with(text: &str) -> crate::settings::Settings {
    let dir = tempfile::tempdir().expect("temporary directory");
    let global = dir.path().join("tui.toml");
    std::fs::write(&global, text).expect("write the fixture settings file");
    let sources = crate::settings::Sources {
        global,
        project: None,
        configs: Vec::new(),
    };
    let mut warnings = Vec::new();
    let mut layers = crate::settings::layers::LayerSet::new(&sources);
    layers.refresh(&mut warnings);
    let (settings, resolve_warnings) = layers.resolve(|_: &str| None);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(resolve_warnings.is_empty(), "{resolve_warnings:?}");
    settings
}

fn turn_usage_on(app: &mut App) {
    app.refresh_settings(&settings_with("[transcript]\nturn_usage = true\n"));
}

fn report_usage(app: &mut App, input: u64, output: u64) {
    app.apply_event(&ApiEvent::TokenUsage {
        session_id: "s1".into(),
        input,
        output,
        cache_read_input: None,
        cache_creation_input: None,
    });
}

fn end_turn(app: &mut App) {
    app.apply_event(&ApiEvent::TurnDone {
        session_id: "s1".into(),
        pending_soft_interrupts: None,
    });
}

/// The footers in the transcript, oldest first.
fn footers(app: &App) -> Vec<crate::transcript::TurnMetrics> {
    app.transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            crate::transcript::Item::TurnUsage(metrics) => Some(*metrics),
            _ => None,
        })
        .collect()
}

/// The text one drawn row carries.
fn line_text(line: &ratatui::text::Line<'static>) -> String {
    line.spans.iter().map(|span| span.content.as_ref()).collect()
}

/// The rows the pane draws for the footer at `index`.
fn footer_text(app: &App, index: usize) -> String {
    let look = crate::item_lines::Look::new(app, 80);
    let item = app
        .transcript
        .items()
        .iter()
        .filter(|item| matches!(item, crate::transcript::Item::TurnUsage(_)))
        .nth(index)
        .expect("the turn footer is in the transcript");
    let rows = crate::item_lines::lines(item, &look, None);
    rows.iter()
        .map(|row| line_text(row.line()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn completed_turns_draw_their_own_time_and_usage_once_enabled() {
    let mut app = app();
    turn_usage_on(&mut app);
    for (index, (input, output)) in [(100, 10), (200, 20)].into_iter().enumerate() {
        app.transcript.begin_turn();
        app.apply_event(&ApiEvent::TextDelta {
            session_id: "s1".into(),
            text: format!("answer {index}"),
            message_id: Some(format!("m{index}")),
        });
        report_usage(&mut app, input, output);
        end_turn(&mut app);
    }

    let shape: Vec<&str> = app
        .transcript
        .items()
        .iter()
        .map(|item| match item {
            crate::transcript::Item::Assistant(_) => "answer",
            crate::transcript::Item::TurnUsage(_) => "footer",
            _ => "other",
        })
        .collect();
    assert_eq!(shape, ["answer", "footer", "answer", "footer"]);
    let footers = footers(&app);
    assert_eq!(footers.len(), 2, "each turn draws its own footer");
    assert_eq!(footers[0].tokens, Some((100, 10)));
    assert_eq!(footers[1].tokens, Some((200, 20)));
    assert!(
        footers.iter().all(|footer| footer.elapsed_ms < 60_000),
        "a test turn does not run for a minute"
    );
    assert_eq!(
        footer_text(&app, 0),
        format!("{} · ↑100 ↓10", short_time(footers[0].elapsed_ms))
    );
    assert_eq!(
        footer_text(&app, 1),
        format!("{} · ↑200 ↓20", short_time(footers[1].elapsed_ms))
    );
}

/// The seconds form `format_elapsed` uses below one minute.
fn short_time(millis: u64) -> String {
    format!("{:.1}s", millis as f64 / 1000.0)
}

#[test]
fn turn_usage_is_off_until_the_setting_enables_it() {
    let mut app = app();
    app.refresh_settings(&crate::settings::Settings::defaults());
    app.transcript.begin_turn();
    report_usage(&mut app, 100, 10);
    end_turn(&mut app);

    assert!(footers(&app).is_empty(), "the footer is off by default");
}

#[test]
fn a_turn_without_reported_usage_draws_only_its_time() {
    let mut app = app();
    turn_usage_on(&mut app);
    app.transcript.begin_turn();
    end_turn(&mut app);

    let footers = footers(&app);
    assert_eq!(footers.len(), 1);
    assert_eq!(footers[0].tokens, None, "no call reported usage");
    let text = footer_text(&app, 0);
    assert!(!text.contains('↑') && !text.contains('↓'), "{text}");
    assert!(
        !text.contains("·"),
        "a missing count leaves no separator behind: {text}"
    );
}

#[test]
fn usage_that_moves_backwards_never_subtracts() {
    let mut app = app();
    turn_usage_on(&mut app);
    app.transcript.begin_turn();
    report_usage(&mut app, 500, 100);
    report_usage(&mut app, 200, 50);
    end_turn(&mut app);
    assert_eq!(
        footers(&app)[0].tokens,
        Some((700, 150)),
        "a smaller second call is added, never subtracted"
    );

    // A provider that reports the largest possible count twice saturates
    // rather than wrapping to a smaller number.
    app.transcript.begin_turn();
    report_usage(&mut app, u64::MAX, u64::MAX);
    report_usage(&mut app, 1, 1);
    end_turn(&mut app);
    assert_eq!(footers(&app)[1].tokens, Some((u64::MAX, u64::MAX)));
}

#[test]
fn a_turn_the_client_did_not_start_draws_no_footer() {
    let mut app = app();
    turn_usage_on(&mut app);
    end_turn(&mut app);

    assert!(
        footers(&app).is_empty(),
        "a turn another client ran has no start this client measured"
    );
}

#[test]
fn clearing_the_conversation_drops_the_turn_in_progress() {
    let mut app = app();
    turn_usage_on(&mut app);
    app.transcript.begin_turn();
    app.transcript.clear();
    end_turn(&mut app);

    assert!(
        footers(&app).is_empty(),
        "the turn's footer would sit under a conversation that is gone"
    );
}

#[test]
fn a_long_turn_draws_minutes_and_short_counts() {
    let app = app();
    let item = crate::transcript::Item::TurnUsage(crate::transcript::TurnMetrics {
        elapsed_ms: 65_500,
        tokens: Some((1_234, 56)),
    });
    let look = crate::item_lines::Look::new(&app, 80);
    let rows = crate::item_lines::lines(&item, &look, None);
    let text = rows.iter().map(|row| line_text(row.line())).collect::<String>();
    assert_eq!(text, "1m 05s · ↑1.2k ↓56");
}

#[test]
fn args_parse_goal_flag_accepts_space_and_equals_forms() {
    let spaced = crate::Args::parse([
        "--goal".to_string(),
        "Ship".to_string(),
        "--goal-budget".to_string(),
        "1200".to_string(),
    ])
    .expect("space-separated goal flags parse");
    assert_eq!(spaced.goal.as_deref(), Some("Ship"));
    assert_eq!(spaced.goal_budget, Some(1200));

    let equals = crate::Args::parse(["--goal=Ship=v2".to_string(), "--goal-budget=1200".to_string()])
        .expect("equals-form goal flags parse");
    assert_eq!(equals.goal.as_deref(), Some("Ship=v2"));
    assert_eq!(equals.goal_budget, Some(1200));

    assert_eq!(
        crate::Args::parse(["--goal=".to_string(), "--goal-budget=1200".to_string()]),
        Err("--goal needs a nonempty objective".to_string())
    );
    assert_eq!(
        crate::Args::parse(["--goal=   ".to_string(), "--goal-budget=1200".to_string()]),
        Err("--goal needs a nonempty objective".to_string())
    );
    assert_eq!(
        crate::Args::parse(["--goal=Ship".to_string(), "--goal-budget=0".to_string()]),
        Err("--goal-budget needs a positive token count".to_string())
    );
    assert_eq!(
        crate::Args::parse(["--goal=Ship".to_string(), "--goal-budget=abc".to_string()]),
        Err("--goal-budget needs a positive token count".to_string())
    );
    assert_eq!(
        crate::Args::parse(["--goal=Ship".to_string()]),
        Err("--goal and --goal-budget must be used together".to_string())
    );
}
