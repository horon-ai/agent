//! Time one drawn frame of the interactive client with a full transcript.
//!
//! `scripts/perf/bench_tui.py` runs this binary several times and reads one JSON
//! object from its last line, so the TUI suite reports the cost of drawing a
//! frame rather than only the cost of starting the process. The transcript is
//! filled with 1000 streamed deltas, which is roughly a long answer on a
//! 120-column terminal, and then the same frame is drawn `FRAMES` times on a
//! `TestBackend`: no terminal, no daemon, no model.
//!
//! The output is a single line so a caller can read it with a plain split:
//!
//! ```text
//! {"render_p50_ms":0.42,"render_p95_ms":0.71,"frames":200,"width":120,"height":40}
//! ```
//!
//! Failures go to stderr with a non-zero exit; nothing here panics, because the
//! benchmark harness treats a panic message as an unreadable measurement.

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Instant;

use horon_agent_sdk::api::ApiEvent;
use horon_tui::app::App;
use horon_tui::draw;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Terminal size the measurement assumes; the default TUI window in a CI runner.
const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;
/// Streamed deltas that fill the transcript before timing starts.
const MESSAGES: usize = 1000;
/// Frames timed. Enough for a stable p95 without making the run slow.
const FRAMES: usize = 200;
/// Bytes of text per delta, so the transcript holds a realistic amount.
const DELTA: &str = "the quick brown fox jumps over the lazy dog and keeps running ";

fn main() -> ExitCode {
    match run() {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("render_bench: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let (_sender, receiver) = mpsc::channel();
    let mut app = App::new("render bench".to_string(), receiver);
    for index in 0..MESSAGES {
        app.apply_event(&ApiEvent::TextDelta {
            session_id: "bench".to_string(),
            text: format!("{index:04} {DELTA}"),
            message_id: Some("bench".to_string()),
        });
    }
    let backend = TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = Terminal::new(backend).map_err(|error| format!("test backend: {error}"))?;
    let mut samples = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let started = Instant::now();
        draw(&mut terminal, &mut app)?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|left, right| left.total_cmp(right));
    let p50 = percentile(&samples, 50.0);
    let p95 = percentile(&samples, 95.0);
    Ok(format!(
        "{{\"render_p50_ms\":{p50:.4},\"render_p95_ms\":{p95:.4},\"frames\":{FRAMES},\"width\":{WIDTH},\"height\":{HEIGHT}}}"
    ))
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
