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
//!
//! The output is a single line so a caller can read it with a plain split:
//!
//! ```text
//! {"scroll_input_p50_us":2.1,"scroll_frame_p50_ms":0.71,...}
//! ```
//!
//! Failures go to stderr with a non-zero exit; nothing here panics, because the
//! benchmark harness treats a panic message as an unreadable measurement.

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::ApiEvent;
use horon_tui::app::App;
use horon_tui::draw;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

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
    fill(&mut app);

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
    // The real key, so the output kept for expanding is drawn in full: this is
    // the long output the scroll steps run through.
    app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    if let Err(message) = draw(&mut terminal, &mut app) {
        eprintln!("scroll_bench: expanded draw failed: {message}");
        return None;
    }

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
         \"content_rows\":{content_rows},\"items\":{},\"tool_output_bytes\":{},\
         \"width\":{WIDTH},\"height\":{HEIGHT}}}",
        percentile(&inputs, 50.0) * 1000.0,
        percentile(&inputs, 95.0) * 1000.0,
        percentile(&frames, 50.0),
        percentile(&frames, 95.0),
        percentile(&input_to_frames, 50.0),
        percentile(&input_to_frames, 95.0),
        percentile(&redraws, 50.0),
        percentile(&redraws, 95.0),
        app.transcript.items().len(),
        tool_output().len(),
    ))
}

/// Commit `TURNS` turns and one long tool output through the transcript's own
/// entry points, the same ones the client's event and send paths use.
fn fill(app: &mut App) {
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
        app.transcript.tool_start(&call, "bash");
        app.transcript
            .tool_input(&call, "{\"command\":\"make test\",\"file_path\":\"build.log\"}");
        app.transcript.tool_done(&call, "bash", Some(&output), None);
    }
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
