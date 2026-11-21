//! Terminal progress and status signals: which terminals get bytes at all, what
//! a state costs on the wire, and what the exit paths leave behind.

use crate::progress::{Capabilities, Environment, Kind, Options, Signal, Signals, bar_sequence, status_sequence};

/// Both channels on, as a user who turned them on would have them.
const ON: Options = Options {
    progress: true,
    program_status: true,
};

const BOTH: Capabilities = Capabilities {
    progress: true,
    program_status: true,
};

/// A signals writer over a buffer, standing in for the terminal.
fn signals() -> Signals<Vec<u8>> {
    Signals::with(BOTH, Vec::new(), true)
}

fn text(signals: &Signals<Vec<u8>>) -> String {
    String::from_utf8(signals.written().clone()).expect("the sequences are text")
}

#[test]
fn a_supported_terminal_gets_a_bounded_start_update_and_clear() {
    let mut signals = signals();
    signals.sync(Signal::working(None), ON).expect("write");
    let started = text(&signals);
    assert!(started.contains("\x1b]9;4;3\x07"), "no start indicator: {started:?}");
    assert!(
        started.contains("\x1b]7501;state=working:app=horon\x07"),
        "no working record: {started:?}"
    );

    // The same state again must not cost a second sequence.
    let before = text(&signals);
    signals.sync(Signal::working(None), ON).expect("write");
    assert_eq!(text(&signals), before, "an unchanged state was written again");

    signals.sync(Signal::working(Some(40)), ON).expect("write");
    let updated = text(&signals);
    assert!(
        updated.contains("\x1b]9;4;1;40\x07"),
        "no measured indicator: {updated:?}"
    );
    assert!(
        updated.contains("state=working:progress=40:app=horon"),
        "no measured record: {updated:?}"
    );

    signals.sync(Signal::idle(), ON).expect("write");
    let cleared = text(&signals);
    assert!(
        cleared.ends_with("\x1b]9;4;0\x07"),
        "the indicator was left on: {cleared:?}"
    );
    assert!(
        cleared.contains("\x1b]7501;state=clear\x07"),
        "the record was left behind: {cleared:?}"
    );

    // Already cleared: the exit path has nothing left to write.
    let before = text(&signals);
    signals.clear().expect("clear");
    assert_eq!(text(&signals), before, "clearing twice wrote again");
}

#[test]
fn a_turn_with_nothing_counted_reports_indeterminate() {
    // No percentage is the protocol's indeterminate state, not zero percent: a
    // spinner rather than a bar that never moves.
    assert_eq!(
        bar_sequence(crate::progress::bar_of(Signal::working(None))),
        "\x1b]9;4;3\x07"
    );
    assert!(!status_sequence(Signal::working(None)).contains("progress="));
}

#[test]
fn an_unsupported_terminal_gets_no_bytes() {
    let mut signals = Signals::with(Capabilities::default(), Vec::new(), true);
    for signal in [
        Signal::working(None),
        Signal::working(Some(50)),
        Signal::blocked(Kind::Permission),
        Signal::done(),
        Signal::error(),
        Signal::idle(),
    ] {
        signals.sync(signal, ON).expect("write");
    }
    signals.clear().expect("clear");
    assert_eq!(text(&signals), "", "an unsupported terminal was written to");
}

#[test]
fn a_channel_the_user_turned_off_writes_nothing() {
    let mut signals = signals();
    signals
        .sync(Signal::working(Some(10)), Options::default())
        .expect("write");
    assert_eq!(text(&signals), "", "a disabled channel wrote bytes");

    // Only the indicator: the record keeps no bytes.
    let mut signals = crate::progress::Signals::with(BOTH, Vec::new(), true);
    signals
        .sync(
            Signal::working(Some(10)),
            Options {
                progress: true,
                program_status: false,
            },
        )
        .expect("write");
    let written = text(&signals);
    assert!(written.contains("\x1b]9;4;1;10\x07"), "{written:?}");
    assert!(
        !written.contains("7501"),
        "a turned-off record was written: {written:?}"
    );
}

#[test]
fn turning_a_channel_off_clears_what_it_reported() {
    let mut signals = signals();
    signals.sync(Signal::working(Some(30)), ON).expect("write");
    // The user turns both off while a turn runs: the tab and the record must
    // not keep the state the settings no longer allow.
    signals
        .sync(Signal::working(Some(30)), Options::default())
        .expect("write");
    let written = text(&signals);
    assert!(
        written.ends_with("\x1b]9;4;0\x07"),
        "the indicator was kept: {written:?}"
    );
    assert!(
        written.contains("\x1b]7501;state=clear\x07"),
        "the record was kept: {written:?}"
    );
}

#[test]
fn exit_clears_what_a_supported_terminal_holds() {
    let mut signals = signals();
    signals.sync(Signal::done(), ON).expect("write");
    signals.clear().expect("clear");
    let written = text(&signals);
    assert!(
        written.ends_with("\x1b]9;4;0\x07"),
        "the indicator outlived the client: {written:?}"
    );
    assert!(
        written.contains("\x1b]7501;state=clear\x07"),
        "the record outlived the client: {written:?}"
    );
}

#[test]
fn a_redirected_stdout_gets_no_bytes() {
    // A redirected stdout writes a file, and escape bytes in it are noise.
    let mut signals = crate::progress::Signals::with(BOTH, Vec::new(), false);
    signals.sync(Signal::working(Some(20)), ON).expect("write");
    signals.clear().expect("clear");
    assert_eq!(text(&signals), "", "a redirected stdout was written to");
}

#[test]
fn the_protocols_name_the_state_the_screen_shows() {
    assert_eq!(
        status_sequence(Signal::blocked(Kind::Permission)),
        "\x1b]7501;state=blocked:kind=permission:app=horon\x07"
    );
    assert_eq!(
        status_sequence(Signal::blocked(Kind::Question)),
        "\x1b]7501;state=blocked:kind=question:app=horon\x07"
    );
    assert_eq!(status_sequence(Signal::done()), "\x1b]7501;state=done:app=horon\x07");
    assert_eq!(status_sequence(Signal::error()), "\x1b]7501;state=error:app=horon\x07");
    // A finished turn leaves a full bar; the removal state belongs to the exit
    // path, not to a completed turn.
    assert_eq!(
        bar_sequence(crate::progress::bar_of(Signal::done())),
        "\x1b]9;4;1;100\x07"
    );
    assert_eq!(
        bar_sequence(crate::progress::bar_of(Signal::blocked(Kind::Permission))),
        "\x1b]9;4;4\x07"
    );
    assert_eq!(bar_sequence(crate::progress::bar_of(Signal::error())), "\x1b]9;4;2\x07");
}

#[test]
fn the_environment_decides_which_protocols_exist() {
    let named = |term: &str, program: Option<&str>| Environment {
        term: Some(term.to_string()),
        term_program: program.map(str::to_string),
        ..Environment::default()
    };

    let ghostty = named("xterm-256color", Some("ghostty")).capabilities();
    assert!(ghostty.progress, "ghostty reads the indicator");
    assert!(ghostty.program_status, "ghostty reads the record");

    // A plain xterm names nothing that reads either protocol, so it gets no
    // bytes rather than a stray escape string.
    let xterm = named("xterm-256color", None).capabilities();
    assert!(!xterm.progress && !xterm.program_status);

    // `dumb` prints whatever it is given as text, whatever else it claims.
    let dumb = named("dumb", Some("ghostty")).capabilities();
    assert!(!dumb.progress && !dumb.program_status, "a dumb terminal was written to");

    // herdr-core stores the record but is not an emulator for the indicator.
    let herdr = Environment {
        term: Some("xterm-256color".to_string()),
        hc_terminal: Some("t1".to_string()),
        ..Environment::default()
    }
    .capabilities();
    assert!(herdr.program_status);
    assert!(!herdr.progress);

    let redeclared = Environment {
        term: Some("dumb".to_string()),
        wt_session: Some("session".to_string()),
        ..Environment::default()
    }
    .capabilities();
    assert!(
        !redeclared.progress,
        "a dumb terminal was written to through WT_SESSION"
    );
}

/// An app with no connection, as the state tests need only the app.
fn app() -> crate::app::App {
    let (_sender, receiver) = std::sync::mpsc::channel();
    crate::app::App::new("socket · session_a".to_string(), receiver)
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}

#[test]
fn the_app_signal_follows_the_state_the_screen_shows() {
    use crate::progress::State;

    let mut app = app();
    assert_eq!(app.terminal_signal().state, State::Idle, "a fresh client is at rest");

    // A turn with nothing counted reports the indeterminate state, not 0%: the
    // tab spins rather than freezing on a share that does not exist.
    app.pending_turn = true;
    let working = app.terminal_signal();
    assert_eq!(working.state, State::Working);
    assert_eq!(working.percent, None, "a turn with no counted list claimed a share");

    // A prompt is what is blocking the turn, so it outranks the running state
    // and says what kind of answer it needs.
    app.show_prompt(crate::prompts::Prompt::approval("r1".into(), "bash".into()));
    assert_eq!(app.terminal_signal().state, State::Blocked(Kind::Permission));
    app.prompt = None;

    // The turn ends: the tab carries the outcome until the user acts on it.
    app.pending_turn = false;
    app.apply_event(&horon_agent_sdk::api::ApiEvent::TurnDone {
        session_id: "session_a".into(),
        pending_soft_interrupts: None,
    });
    assert_eq!(app.terminal_signal().state, State::Done);
    app.on_key(key(crossterm::event::KeyCode::Char('x')));
    assert_eq!(
        app.terminal_signal().state,
        State::Idle,
        "a key did not clear the mark a finished turn left"
    );
}

#[test]
fn a_failed_turn_keeps_its_mark_and_a_lost_connection_outranks_it() {
    use crate::progress::State;

    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&horon_agent_sdk::api::ApiEvent::TurnStopped {
        session_id: "session_a".into(),
        reason: horon_agent_sdk::api::TurnStopReason::Failure,
        message: "provider failed".into(),
        provider_stop_reason: None,
    });
    assert_eq!(app.terminal_signal().state, State::Error, "a failure was not flagged");

    // The daemon follows a stop with the done event; that must not turn a
    // broken turn into a finished one under the user's nose.
    app.apply_event(&horon_agent_sdk::api::ApiEvent::TurnDone {
        session_id: "session_a".into(),
        pending_soft_interrupts: None,
    });
    assert_eq!(
        app.terminal_signal().state,
        State::Error,
        "a done event overwrote the failure"
    );

    app.disconnect("the harness bridge stopped");
    assert_eq!(app.terminal_signal().state, State::Error);
}

#[test]
fn a_cancel_is_the_user_watching_so_it_leaves_no_mark() {
    use crate::progress::State;

    let mut app = app();
    app.pending_turn = true;
    app.apply_event(&horon_agent_sdk::api::ApiEvent::TurnStopped {
        session_id: "session_a".into(),
        reason: horon_agent_sdk::api::TurnStopReason::Interrupted,
        message: "turn cancelled by user".into(),
        provider_stop_reason: None,
    });
    assert_eq!(
        app.terminal_signal().state,
        State::Idle,
        "the user's own cancel was reported as a failure"
    );
}

#[test]
fn a_running_turn_reports_the_todo_share() {
    use horon_agent_sdk::todo::TodoItem;

    let item = |content: &str, status: &str| TodoItem {
        content: content.into(),
        status: status.into(),
        ..TodoItem::default()
    };
    let mut app = app();
    app.todos = crate::todo_panel::TodoPanel::from_items(&[
        item("one", "completed"),
        item("two", "in_progress"),
        item("three", "completed"),
        item("four", "pending"),
    ]);
    app.pending_turn = true;
    assert_eq!(
        app.terminal_signal().percent,
        Some(50),
        "the completed half was not the share"
    );

    // A cancelled item is work nobody still expects, so it is outside the count.
    app.todos = crate::todo_panel::TodoPanel::from_items(&[item("one", "completed"), item("two", "cancelled")]);
    assert_eq!(app.terminal_signal().percent, Some(100));
}
