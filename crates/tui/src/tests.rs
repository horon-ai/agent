//! Rendering and key-handling tests for the whole client.
//!
//! These drive `draw` against a `TestBackend` and the app's key handling, so they
//! need the crate root's private helpers. They live in their own file because
//! `lib.rs` is at its code-size budget and production code keeps the room;
//! `streaming_tests.rs` is the same arrangement for streamed text.

use super::*;
use crate::app::Action;
use crate::transcript::ToolState;
use horon_agent_sdk::api::ApiEvent;
use ratatui::backend::TestBackend;

fn app_with(socket: Option<std::path::PathBuf>) -> App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    match socket {
        Some(path) => App::new(path.display().to_string(), receiver),
        None => App::new("default socket".to_string(), receiver),
    }
}

fn draw_to_string(app: &mut App) -> String {
    let backend = TestBackend::new(80, 24);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    format!("{}", terminal.backend())
}

fn text_event(text: &str) -> ApiEvent {
    ApiEvent::TextDelta {
        session_id: "s1".into(),
        text: text.into(),
        message_id: Some("m1".into()),
    }
}

/// The transcript only draws finished items, so a test that wants text on
/// screen closes the stream the way the harness does.
fn turn_done() -> ApiEvent {
    ApiEvent::TurnDone {
        session_id: "s1".into(),
        pending_soft_interrupts: None,
    }
}

#[test]
fn args_parse_socket_and_flags() {
    let parsed = Args::parse(["--socket".to_string(), "/tmp/a.sock".to_string()]);
    assert_eq!(
        parsed.as_ref().map(|args| args.socket.clone()),
        Ok(Some("/tmp/a.sock".into()))
    );
    let parsed = Args::parse(["--socket=/tmp/b.sock".to_string(), "--version".to_string()]);
    assert!(parsed.map(|args| args.version).unwrap_or(false));
    assert!(Args::parse(["--nope".to_string()]).is_err());
    assert!(Args::parse(["--socket".to_string()]).is_err());
}

#[test]
fn args_parse_session_and_cwd() {
    let parsed = Args::parse([
        "--session".to_string(),
        "sess-42".to_string(),
        "--cwd=/tmp/work".to_string(),
    ]);
    match parsed {
        Ok(args) => {
            assert_eq!(args.session.as_deref(), Some("sess-42"));
            assert_eq!(args.cwd, Some("/tmp/work".into()));
        }
        Err(message) => panic!("expected a parse: {message}"),
    }
    assert!(Args::parse(["--session".to_string()]).is_err());
    assert!(Args::parse(["--cwd".to_string()]).is_err());
}

#[test]
fn args_parse_repeated_config_files() {
    let parsed = Args::parse([
        "--config".to_string(),
        "/tmp/a.toml".to_string(),
        "--config=/tmp/b.toml".to_string(),
    ]);
    match parsed {
        Ok(args) => assert_eq!(
            args.config,
            vec![
                std::path::PathBuf::from("/tmp/a.toml"),
                std::path::PathBuf::from("/tmp/b.toml")
            ]
        ),
        Err(message) => panic!("expected a parse: {message}"),
    }
    assert!(Args::parse(["--config".to_string()]).is_err());
    assert!(Args::parse(["--config=".to_string()]).is_err());
}

#[test]
fn help_and_version_exit_without_connecting() {
    // Both paths return before any connection is attempted, which is what
    // makes them testable in CI where no daemon is running.
    let code = run([
        std::ffi::OsString::from("horon-tui"),
        std::ffi::OsString::from("--help"),
    ]);
    assert_eq!(code, 0);
    let code = run([
        std::ffi::OsString::from("horon-tui"),
        std::ffi::OsString::from("--version"),
    ]);
    assert_eq!(code, 0);
    let code = run([
        std::ffi::OsString::from("horon-tui"),
        std::ffi::OsString::from("--bogus"),
    ]);
    assert_eq!(code, 2);
}

#[test]
fn streams_text_and_finishes_turn() {
    let mut app = app_with(None);
    app.apply_event(&text_event("Hello"));
    app.apply_event(&text_event(", "));
    app.apply_event(&text_event("world"));
    app.apply_event(&ApiEvent::ToolStart {
        session_id: "s1".into(),
        call_id: "c1".into(),
        name: "bash".into(),
    });
    app.apply_event(&ApiEvent::ToolDone {
        session_id: "s1".into(),
        call_id: "c1".into(),
        name: "bash".into(),
        output: "ok".into(),
        error: None,
        verdict: None,
    });
    app.apply_event(&ApiEvent::TurnDone {
        session_id: "s1".into(),
        pending_soft_interrupts: None,
    });

    let screen = draw_to_string(&mut app);
    assert!(
        screen.contains("Hello, world"),
        "assistant text missing from:\n{screen}"
    );
    assert!(screen.contains("* bash"), "tool line missing from:\n{screen}");
    assert!(!app.pending_turn);
}

#[test]
fn approval_pending_shows_prompt_and_resolves() {
    let mut app = app_with(None);
    app.apply_event(&ApiEvent::ApprovalPending {
        session_id: "s1".into(),
        request_id: "r1".into(),
        action: "bash: rm -rf build".into(),
        expires_at: None,
    });
    let screen = draw_to_string(&mut app);
    assert!(screen.contains("approval needed"), "prompt not drawn:\n{screen}");
    assert!(screen.contains("rm -rf build"), "action missing:\n{screen}");

    let action = app.on_key(key(crossterm::event::KeyCode::Char('y')));
    assert!(
        matches!(action, Some(Action::Approval { approved: true, .. })),
        "y did not answer the prompt: {action:?}"
    );

    app.apply_event(&ApiEvent::ApprovalResolved {
        session_id: "s1".into(),
        request_id: "r1".into(),
        approved: true,
        via: "inline".into(),
        resolved_by: "tool".into(),
    });
    let screen = draw_to_string(&mut app);
    assert!(!screen.contains("approval needed"), "prompt still drawn:\n{screen}");
    assert!(!screen.contains("rm -rf build"), "stale action still drawn:\n{screen}");
}

#[test]
fn daemon_unreachable_shows_error() {
    // A path with no listener is the real failure shape: connect fails
    // without a daemon, and the app must render the sentence instead of
    // panicking, with exit code 1.
    let missing = std::env::temp_dir().join("horon-tui-test-no-such-socket.sock");
    let options = horon_agent_sdk::ConnectOptions {
        socket_path: Some(missing),
        ensure_runtime: false,
        request_timeout: Some(Duration::from_millis(200)),
        ..horon_agent_sdk::ConnectOptions::default()
    };
    let failure = sdk_bridge::connect(options, None, None);
    assert!(failure.is_err(), "connecting to a missing socket must fail");

    let mut app = match failure {
        Ok(_) => panic!("expected a connection failure"),
        Err(message) => App::failure(&message),
    };
    let screen = draw_to_string(&mut app);
    assert!(screen.contains("harness"), "error banner missing:\n{screen}");
    assert_eq!(app.exit_code, 1);
}

#[test]
fn turn_done_ends_the_turn_and_replace_retracts() {
    let mut app = app_with(None);
    app.apply_event(&text_event("draft"));
    app.apply_event(&ApiEvent::TextReplace {
        session_id: "s1".into(),
        text: String::new(),
        message_id: Some("m1".into()),
    });
    app.apply_event(&ApiEvent::TurnDone {
        session_id: "s1".into(),
        pending_soft_interrupts: None,
    });
    let screen = draw_to_string(&mut app);
    assert!(!screen.contains("draft"), "retracted text still drawn:\n{screen}");
}

#[test]
fn tool_failure_is_visible() {
    let mut app = app_with(None);
    app.apply_event(&ApiEvent::ToolStart {
        session_id: "s1".into(),
        call_id: "c9".into(),
        name: "write".into(),
    });
    app.apply_event(&ApiEvent::ToolDone {
        session_id: "s1".into(),
        call_id: "c9".into(),
        name: "write".into(),
        output: String::new(),
        error: Some("permission denied".into()),
        verdict: None,
    });
    assert_eq!(
        app.transcript.items().last(),
        Some(&Item::Tool {
            name: "write".into(),
            state: ToolState::Failed("permission denied".into())
        })
    );
}

#[test]
fn model_info_reaches_the_status_line() {
    let mut app = app_with(None);
    app.apply_event(&ApiEvent::ModelInfo {
        session_id: "s1".into(),
        provider: Some("anthropic".into()),
        model: Some("claude-sonnet-4".into()),
        reasoning_effort: None,
        auth_method: None,
    });
    let status = app.status_line();
    assert!(status.contains("claude-sonnet-4"), "status was {status}");
    assert!(status.contains("anthropic"), "status was {status}");
}

#[test]
fn statusline_custom_preset_draws_left_then_right() {
    let mut app = app_with(None);
    app.apply_event(&ApiEvent::ModelInfo {
        session_id: "s1".into(),
        provider: Some("anthropic".into()),
        model: Some("claude-sonnet-4".into()),
        reasoning_effort: None,
        auth_method: None,
    });
    app.apply_event(&ApiEvent::TokenUsage {
        session_id: "s1".into(),
        input: 200,
        output: 50,
        cache_read_input: None,
        cache_creation_input: None,
    });
    app.cwd = Some("/home/u/proj".to_string());
    app.home = Some("/home/u".to_string());
    app.statusline = statusline::Layout {
        preset: statusline::Preset::Custom,
        left: vec![statusline::SegmentId::Model, statusline::SegmentId::Path],
        right: vec![statusline::SegmentId::ContextPct, statusline::SegmentId::Time],
        path_max: 40,
        context_window: 1000,
    };

    let screen = draw_to_string(&mut app);
    // `TestBackend`'s Display wraps each row in quotes.
    let row = screen
        .lines()
        .nth(22)
        .unwrap_or_default()
        .trim_matches('"')
        .trim_end()
        .to_string();
    assert!(
        row.starts_with("anthropic/claude-sonnet-4 · ~/proj"),
        "left side wrong: {row:?}\n{screen}"
    );
    assert!(row.contains("25% · "), "context percentage missing: {row:?}");
    let time = row.rsplit(" · ").next().unwrap_or_default();
    assert!(time.len() == 5 && time.as_bytes()[2] == b':', "clock missing: {row:?}");
}

#[test]
fn submit_sends_and_quit_leaves() {
    let mut app = app_with(None);
    for character in "hi".chars() {
        app.on_key(key(crossterm::event::KeyCode::Char(character)));
    }
    assert_eq!(
        app.on_key(key(crossterm::event::KeyCode::Enter)),
        Some(Action::Send("hi".into()))
    );
    assert!(app.pending_turn);
    assert_eq!(app.transcript.items().last(), Some(&Item::User("hi".into())));
    assert!(app.input.text().is_empty());

    for character in "/quit".chars() {
        app.on_key(key(crossterm::event::KeyCode::Char(character)));
    }
    assert_eq!(app.on_key(key(crossterm::event::KeyCode::Enter)), Some(Action::Quit));
}

/// Resolve settings with `env` as the only layer above the defaults.
///
/// The real resolver is used rather than a setter the product does not have,
/// so a key that stops resolving fails here instead of passing against a
/// value the test injected.
fn settings_with(env: &[(&str, &str)]) -> crate::settings::Settings {
    let sources = crate::settings::Sources {
        global: std::path::PathBuf::from("/nonexistent/global.toml"),
        project: None,
        configs: Vec::new(),
    };
    let set = crate::settings::layers::LayerSet::new(&sources);
    let (settings, warnings) = set.resolve(|name| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string())
    });
    assert!(warnings.is_empty(), "no layer warns: {warnings:?}");
    settings
}

#[test]
fn enter_inserts_nothing_when_the_editor_does_not_submit_on_it() {
    let mut app = app_with(None);
    app.refresh_settings(&settings_with(&[("HORON_TUI_EDITOR_SUBMIT_ON_ENTER", "false")]));

    for character in "hi".chars() {
        app.on_key(key(crossterm::event::KeyCode::Char(character)));
    }
    assert_eq!(
        app.on_key(key(crossterm::event::KeyCode::Enter)),
        None,
        "the key the user disconnected must not send"
    );
    assert_eq!(
        app.input.text(),
        "hi",
        "and the draft is kept, not cleared: {:?}",
        app.input.text()
    );
    assert!(!app.pending_turn, "no turn started");
}

#[test]
fn enter_still_sends_when_the_setting_is_on() {
    let mut app = app_with(None);
    app.refresh_settings(&settings_with(&[("HORON_TUI_EDITOR_SUBMIT_ON_ENTER", "true")]));
    for character in "hi".chars() {
        app.on_key(key(crossterm::event::KeyCode::Char(character)));
    }
    assert_eq!(
        app.on_key(key(crossterm::event::KeyCode::Enter)),
        Some(Action::Send("hi".into()))
    );
}

#[test]
fn the_reasoning_setting_reaches_the_transcript() {
    let mut app = app_with(None);
    app.refresh_settings(&settings_with(&[("HORON_TUI_TRANSCRIPT_SHOW_REASONING", "true")]));
    assert!(app.transcript.show_reasoning());

    app.apply_event(&ApiEvent::ReasoningDelta {
        session_id: "s1".into(),
        text: "a thought".into(),
    });
    app.apply_event(&ApiEvent::ReasoningDone {
        session_id: "s1".into(),
        duration_secs: None,
    });
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Reasoning(text) if text == "a thought")),
        "the event reaches the transcript once the setting is on: {:?}",
        app.transcript.items()
    );
}

#[test]
fn reasoning_events_are_inert_while_the_setting_is_off() {
    let mut app = app_with(None);
    app.apply_event(&ApiEvent::ReasoningDelta {
        session_id: "s1".into(),
        text: "a thought".into(),
    });
    app.apply_event(&ApiEvent::ReasoningDone {
        session_id: "s1".into(),
        duration_secs: None,
    });
    assert!(
        !app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::Reasoning(_))),
        "the default keeps nothing: {:?}",
        app.transcript.items()
    );
}

#[test]
fn ask_prompt_collects_one_answer_per_question() {
    let mut app = app_with(None);
    let questions = vec![
        horon_agent_tool_types::AskQuestion {
            id: Some("scope".into()),
            question: "Which scope?".into(),
            header: None,
            options: vec![
                horon_agent_tool_types::AskOption {
                    label: "one file".into(),
                    description: None,
                },
                horon_agent_tool_types::AskOption {
                    label: "whole repo".into(),
                    description: None,
                },
            ],
            recommended: Some(0),
            multi: false,
        },
        horon_agent_tool_types::AskQuestion {
            id: Some("style".into()),
            question: "Which style?".into(),
            header: None,
            options: vec![horon_agent_tool_types::AskOption {
                label: "minimal".into(),
                description: None,
            }],
            recommended: None,
            multi: false,
        },
    ];
    app.apply_event(&ApiEvent::AskRequested {
        session_id: "s1".into(),
        ask_id: "a1".into(),
        questions,
    });
    let screen = draw_to_string(&mut app);
    assert!(screen.contains("Which scope?"), "question missing:\n{screen}");

    assert_eq!(app.on_key(key(crossterm::event::KeyCode::Char('2'))), None);
    let action = app.on_key(key(crossterm::event::KeyCode::Char('1')));
    match action {
        Some(Action::Ask { ask_id, answers }) => {
            assert_eq!(ask_id, "a1");
            assert_eq!(answers.len(), 2);
            assert_eq!(answers[0].question_id, "scope");
            assert_eq!(answers[0].selected, vec!["whole repo".to_string()]);
            assert_eq!(answers[1].selected, vec!["minimal".to_string()]);
        }
        other => panic!("unexpected action {other:?}"),
    }
}

#[test]
fn ctrl_c_cancels_a_turn_and_ctrl_d_leaves() {
    let mut app = app_with(None);
    app.pending_turn = true;
    let cancel = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('c'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    assert_eq!(app.on_key(cancel), Some(Action::Cancel));

    let quit = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('d'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    assert_eq!(app.on_key(quit), Some(Action::Quit));
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}

fn draw_at(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    format!("{}", terminal.backend())
}

#[test]
fn a_long_line_keeps_its_tail_visible_in_a_narrow_pane() {
    // One logical line far wider than the pane: the newest text is at its
    // end, so the pane must show the end, not the beginning.
    let mut app = app_with(None);
    let text = format!("{}TAILMARK", "x".repeat(480));
    app.apply_event(&text_event(&text));
    app.apply_event(&turn_done());

    let screen = draw_at(&mut app, 40, 12);
    assert!(
        screen.contains("TAILMARK"),
        "the end of a wrapped line was pushed out of the pane:\n{screen}"
    );
}

#[test]
fn the_newest_message_wins_over_the_oldest_after_wrapping() {
    let mut app = app_with(None);
    app.apply_event(&ApiEvent::TextDelta {
        session_id: "s1".into(),
        text: format!("OLDEST {}", "a".repeat(300)),
        message_id: Some("m-old".into()),
    });
    app.apply_event(&ApiEvent::TextDelta {
        session_id: "s1".into(),
        text: format!("NEWEST {}", "b".repeat(300)),
        message_id: Some("m-new".into()),
    });
    app.apply_event(&turn_done());

    let screen = draw_at(&mut app, 40, 12);
    assert!(screen.contains("NEWEST"), "newest text missing:\n{screen}");
    assert!(!screen.contains("OLDEST"), "the pane kept the oldest rows:\n{screen}");
}

#[test]
fn a_long_input_scrolls_instead_of_leaving_the_pane() {
    let mut app = app_with(None);
    for character in "q".repeat(120).chars() {
        app.on_key(key(crossterm::event::KeyCode::Char(character)));
    }
    let backend = TestBackend::new(40, 12);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = draw(&mut terminal, &mut app) {
        panic!("draw failed: {message}");
    }
    let position = terminal.backend().cursor_position();
    let (column, row) = (position.x, position.y);
    assert!(
        column < 40,
        "the caret was placed outside the input area: column {column}"
    );
    assert!(column > 0, "the caret collapsed to the left edge: column {column}");
    // The bottom line, under the status line and the transcript pane.
    assert_eq!(row, 11, "the caret left the input row");
}

#[test]
fn a_closed_bridge_ends_the_session_instead_of_spinning() {
    // `app_with(None)` drops the sender, so the channel reports
    // `Disconnected` exactly as it does when the bridge thread exits.
    let mut app = app_with(None);
    assert!(app.running);
    drain_events(&mut app);
    assert!(!app.running, "a closed bridge must stop the loop");
    assert_eq!(app.exit_code, 1);
    match app.banner.as_deref() {
        Some(banner) => assert!(
            banner.contains("bridge"),
            "the banner did not explain the closed bridge: {banner}"
        ),
        None => panic!("no banner for the closed bridge"),
    }
}
