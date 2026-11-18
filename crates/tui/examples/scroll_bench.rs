//! Time the frames a held view draws while it scrolls a large session.
//!
//! `render_bench` times the frame that follows the end of one long streamed
//! answer, so its number never covers the scroll path: a held view measures the
//! conversation, skips whole blocks by their counted height, wraps the block at
//! the bottom edge of the view again, and spends a row on the follow indicator.
//! This example is that path, and it is kept apart from `render_bench` so the
//! two numbers are never read as one.
//!
//! It fills a session with many turns and one long tool output, expands that
//! output with the real `ctrl+o` key, holds the view with the real `shift+up`
//! key, and times for every step the key handling and the frame that follows it.
//! The screens are checked as they are drawn: a frame that is blank or repeats
//! the screen before it is an error, so a number is never taken from a view that
//! did not move, and every timed screen carries rows of the expanded output.
//! After the timer stops, each draw contributes a fingerprint of all cells.
//! Paired runs compare them in order, including the untimed display changes.
//!
//! The output is a single line so a caller can read it with a plain split:
//!
//! ```text
//! {"scroll_input_p50_us":2.1,"scroll_frame_p50_ms":0.71,...}
//! ```
//!
//! Failures go to stderr with a non-zero exit; nothing here panics, because the
//! benchmark harness treats a panic message as an unreadable measurement.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::ApiEvent;
use horon_tui::app::App;
use horon_tui::draw;
use horon_tui::sdk_bridge::AttachedSession;
use horon_tui::statusline::SegmentId;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

/// Terminal size the measurement assumes; the default TUI window in a CI runner.
const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;
/// Turns committed before anything is measured, so the session is far taller
/// than the pane and has to be counted before a held view can be drawn.
const TURNS: usize = 24;
/// Rows of the one tool output the expand key draws in full. Only the head is
/// previewed, so this is what the long-output scroll runs through.
const OUTPUT_LINES: usize = 4000;
/// One-row scroll steps timed. Each step moves the view by one row, so every
/// timed frame is a different screen.
const STEPS: usize = 120;
/// Frames redrawn at the held view with no input between them, which is the
/// draw cost alone.
const REDRAWS: usize = 20;
const SESSION: &str = "scroll-bench";
const MESSAGE: &str = "scroll-bench";
const TERMINAL_MARKER: &str = "output row";

fn main() -> ExitCode {
    match measurement() {
        Some(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        None => ExitCode::FAILURE,
    }
}

/// The one-line result, or `None` after the reason was printed to stderr.
fn measurement() -> Option<String> {
    let (_sender, receiver) = mpsc::channel();
    let mut app = App::new("scroll bench".to_string(), receiver);
    // Draw a deterministic status line even when a pair straddles a minute.
    app.statusline.right.retain(|segment| *segment != SegmentId::Time);
    let tool_output_bytes = fill(&mut app);
    let items = app.transcript.items().len();

    let backend = TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("scroll_bench: test backend: {error}");
            return None;
        }
    };
    // The first frame settles the pane height and the following view; nothing
    // about it is timed.
    if let Err(message) = draw(&mut terminal, &mut app) {
        eprintln!("scroll_bench: warm-up draw failed: {message}");
        return None;
    }
    let mut screens = Vec::with_capacity(2 + STEPS + REDRAWS + 8);
    screens.push(screen_hash(&terminal));
    // The real key, so the output kept for expanding is drawn in full: this is
    // the long output the scroll steps run through.
    app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    if let Err(message) = draw(&mut terminal, &mut app) {
        eprintln!("scroll_bench: expanded draw failed: {message}");
        return None;
    }
    screens.push(screen_hash(&terminal));

    let mut inputs: Vec<f64> = Vec::with_capacity(STEPS);
    let mut frames: Vec<f64> = Vec::with_capacity(STEPS);
    let mut input_to_frames: Vec<f64> = Vec::with_capacity(STEPS);
    let mut previous = String::new();
    let mut first_held = None;
    for step in 0..STEPS {
        let input_started = Instant::now();
        app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        let draw_started = Instant::now();
        if let Err(message) = draw(&mut terminal, &mut app) {
            eprintln!("scroll_bench: draw failed: {message}");
            return None;
        }
        let finished = Instant::now();
        let elapsed = millis(finished.duration_since(draw_started));
        let input_to_frame = millis(finished.duration_since(input_started));
        inputs.push(millis(draw_started.duration_since(input_started)));
        input_to_frames.push(input_to_frame);
        frames.push(elapsed);
        if step == 0 {
            // Named on its own as well: this is the frame that measured the
            // conversation, which only the first held frame pays for.
            first_held = Some((elapsed, input_to_frame, rows(&app)));
        }

        screens.push(screen_hash(&terminal));
        let screen = format!("{}", terminal.backend());
        if screen.trim().is_empty() {
            eprintln!("scroll_bench: step {step} drew a blank screen");
            return None;
        }
        if screen == previous {
            eprintln!("scroll_bench: step {step} drew the screen before it again");
            return None;
        }
        if !screen.contains(TERMINAL_MARKER) {
            eprintln!("scroll_bench: step {step} did not draw the expanded output");
            return None;
        }
        previous = screen;
    }

    let mut redraws: Vec<f64> = Vec::with_capacity(REDRAWS);
    for _ in 0..REDRAWS {
        let started = Instant::now();
        if let Err(message) = draw(&mut terminal, &mut app) {
            eprintln!("scroll_bench: redraw failed: {message}");
            return None;
        }
        redraws.push(millis(started.elapsed()));
        screens.push(screen_hash(&terminal));
    }

    let rows_floor = usize::from(HEIGHT) * 10;
    let Some((first_held, first_input_to_frame, content_rows)) = first_held else {
        eprintln!("scroll_bench: no held frame was drawn");
        return None;
    };
    if content_rows < rows_floor {
        eprintln!("scroll_bench: the session measured {content_rows} rows, under the {rows_floor} the scroll needs");
        return None;
    }

    let timed_fixture_frames = screens.len();
    check_transitions(&mut terminal, &mut app, &mut screens)?;
    let transition_frames = screens.len() - timed_fixture_frames;
    inputs.sort_by(|left, right| left.total_cmp(right));
    frames.sort_by(|left, right| left.total_cmp(right));
    input_to_frames.sort_by(|left, right| left.total_cmp(right));
    redraws.sort_by(|left, right| left.total_cmp(right));
    Some(format!(
        "{{\"scroll_input_p50_us\":{:.1},\"scroll_input_p95_us\":{:.1},\
         \"scroll_frame_p50_ms\":{:.4},\"scroll_frame_p95_ms\":{:.4},\
         \"scroll_input_to_frame_p50_ms\":{:.4},\"scroll_input_to_frame_p95_ms\":{:.4},\
         \"first_input_to_frame_ms\":{first_input_to_frame:.4},\
         \"held_redraw_p50_ms\":{:.4},\"held_redraw_p95_ms\":{:.4},\
         \"first_held_frame_ms\":{first_held:.4},\"steps\":{STEPS},\"redraws\":{REDRAWS},\
         \"content_rows\":{content_rows},\"items\":{items},\"tool_output_bytes\":{tool_output_bytes},\
         \"width\":{WIDTH},\"height\":{HEIGHT},\"transition_frames\":{transition_frames},\
         \"screen_hashes\":{screens:?}}}",
        percentile(&inputs, 50.0) * 1000.0,
        percentile(&inputs, 95.0) * 1000.0,
        percentile(&frames, 50.0),
        percentile(&frames, 95.0),
        percentile(&input_to_frames, 50.0),
        percentile(&input_to_frames, 95.0),
        percentile(&redraws, 50.0),
        percentile(&redraws, 95.0),
    ))
}

/// Both binaries use this example and the same Rust toolchain on one runner.
fn screen_hash(terminal: &Terminal<TestBackend>) -> u64 {
    let buffer = terminal.backend().buffer();
    let mut state = DefaultHasher::new();
    buffer.area.hash(&mut state);
    for cell in buffer.content() {
        cell.symbol().hash(&mut state);
        cell.style().hash(&mut state);
        cell.diff_option.hash(&mut state);
    }
    state.finish()
}

/// Exercise changes that can invalidate a renderer's wrapped output.
fn check_transitions(terminal: &mut Terminal<TestBackend>, app: &mut App, screens: &mut Vec<u64>) -> Option<()> {
    let mut capture = |terminal: &mut Terminal<TestBackend>, app: &mut App| {
        draw(terminal, app)
            .inspect_err(|error| eprintln!("scroll_bench: transition draw failed: {error}"))
            .ok()?;
        screens.push(screen_hash(terminal));
        Some(())
    };
    terminal.backend_mut().resize(43, HEIGHT);
    capture(terminal, app)?;
    app.theme.transcript.notice = Color::LightCyan;
    capture(terminal, app)?;
    let toggle = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    app.on_key(toggle);
    capture(terminal, app)?;
    app.on_key(toggle);
    capture(terminal, app)?;

    app.attach_session(
        AttachedSession {
            description: "second scroll session".into(),
            session_id: "scroll-next".into(),
            status: "idle".into(),
            model: None,
            effort: None,
            history: Vec::new(),
            title: None,
        },
        true,
    );
    capture(terminal, app)?;
    let reused_id = format!("call{:03}", TURNS - 1);
    let output = "새 출력 · 全角 · e\u{301} · 👩‍💻\n\n오른쪽 경계를 넘는 긴 줄과 공백\t끝\n".repeat(32);
    app.transcript.tool_done(&reused_id, "bash", Some(&output), None);
    capture(terminal, app)?;
    app.on_key(toggle);
    capture(terminal, app)?;
    terminal.backend_mut().resize(19, HEIGHT);
    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    capture(terminal, app)
}

/// Commit `TURNS` turns and one long tool output through the transcript's own
/// entry points, the same ones the client's event and send paths use.
fn fill(app: &mut App) -> usize {
    let output = tool_output();
    for turn in 0..TURNS {
        app.transcript
            .push_user(format!("turn {turn:03}: show me the build log"));
        let answer: String = (0..8)
            .map(|line| format!("the fox keeps running turn {turn:03} line {line}\n"))
            .collect();
        app.apply_event(&ApiEvent::TextDelta {
            session_id: SESSION.to_string(),
            text: answer,
            message_id: Some(MESSAGE.to_string()),
        });
        app.apply_event(&ApiEvent::TextDone {
            session_id: SESSION.to_string(),
            message_id: Some(MESSAGE.to_string()),
        });
        let call = format!("call{turn:03}");
        // A restored result has no live start time to vary between binaries.
        app.transcript.tool_done(&call, "bash", Some(&output), None);
        app.transcript
            .tool_input(&call, "{\"command\":\"make test\",\"file_path\":\"build.log\"}");
    }
    output.len()
}

/// The long output every tool call returns. The rows are named so a drawn
/// screen can be checked for them.
fn tool_output() -> String {
    let mut text = String::with_capacity(OUTPUT_LINES * 48);
    for line in 0..OUTPUT_LINES {
        text.push_str(&format!("output row {line:04} of the build log line\n"));
    }
    text
}

/// Wrapped rows of the conversation the held view last measured.
fn rows(app: &App) -> usize {
    let committed: usize = app.scroll.heights().map_or(0, |heights| heights.iter().sum());
    let open = app.scroll.open_heights();
    committed + open[0] + open[1]
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Nearest-rank percentile over a sorted sample, matching `bench_tui.percentile`.
fn percentile(sorted: &[f64], percent: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percent / 100.0 * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}
