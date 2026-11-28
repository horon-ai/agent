//! Micro-benchmarks for round-2 optimization paths in `horon-tui`.
//!
//! Exercises public surfaces (`notes`, `statusline`, `recording`, `draw`) under
//! a release build with a thread-local counting allocator and repeated runs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::HistoryMessage;
use horon_tui::app::App;
use horon_tui::notes::{MessageKey, Role};
use horon_tui::recording::{Event, MAX_BYTES, Outcome, Recorder, Recording};
use horon_tui::sdk_bridge::{AttachedSession, Incoming};
use horon_tui::statusline::{self, Preset, SegmentId, StatusData, TokenTotals, UsageTotals};
use horon_tui::theme::{Symbols, Theme};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

struct CountingAlloc;

impl CountingAlloc {
    fn record(bytes: usize) {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    }
}

// SAFETY: forwards every call to `System` and updates only atomics/thread-local bool.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

const RUNS: usize = 7;

fn measure<F: FnMut() -> Result<(), Box<dyn Error>>>(name: &str, mut work: F) -> Result<(), Box<dyn Error>> {
    // Warmup run outside timing.
    work()?;
    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _ in 0..RUNS {
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.with(|c| c.set(true));
        let start = Instant::now();
        let res = work();
        let elapsed = start.elapsed();
        COUNTING.with(|c| c.set(false));
        res?;
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
        times_us.push(elapsed.as_micros());
    }
    times_us.sort_unstable();
    let min = times_us[0];
    let med = times_us[RUNS / 2];
    let max = times_us[RUNS - 1];
    println!(
        "BENCH {name}: runs={RUNS} min_us={min} med_us={med} max_us={max} allocs={last_allocs} alloc_bytes={last_bytes}"
    );
    Ok(())
}

fn sample_messages(count: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        if i % 3 == 0 {
            out.push(format!(
                "  Message {i}: 한국어 본문과   여러   공백,\t탭,\n개행이 섞인 긴 대화 응답입니다.   \
                 The quick brown fox jumps over the lazy dog 0123456789 repeated context block {i}.  "
            ));
        } else {
            out.push(format!(
                "Assistant reply #{i}: verified patch hunk application, package digest checking, \
                 and statusline formatting without changing observable behavior across edge cases ({i})."
            ));
        }
    }
    out
}

fn build_noted_app(count: usize) -> App {
    let (_tx, rx) = mpsc::channel::<Incoming>();
    let mut app = App::new("socket · bench".to_string(), rx);
    let messages = sample_messages(count);
    let mut history = Vec::with_capacity(count);
    for (i, content) in messages.into_iter().enumerate() {
        history.push(HistoryMessage {
            response_stats: None,
            role: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
            content,
            answer: None,
            message_id: Some(format!("msg-{i:04}")),
            tool: None,
        });
    }
    app.attach_session(
        AttachedSession {
            description: "socket · bench".to_string(),
            session_id: "bench-session".to_string(),
            status: "idle".to_string(),
            model: Some("claude-opus".to_string()),
            effort: Some("high".to_string()),
            history,
            title: Some("bench".to_string()),
        },
        false,
    );
    app.input.set_text("/note add a1 pinned note on first answer");
    drop(app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    app.scroll.up(40);
    app
}

fn main() -> Result<(), Box<dyn Error>> {
    let messages = sample_messages(400);
    measure("tui_notes_fingerprint_400x25", || {
        let mut last = None;
        for _ in 0..25 {
            for (i, msg) in messages.iter().enumerate() {
                let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
                last = Some(black_box(MessageKey::of(role, msg, 1)));
            }
        }
        black_box(last);
        Ok(())
    })?;

    let mut steady_app = build_noted_app(300);
    let mut steady_term = Terminal::new(TestBackend::new(120, 36))?;
    horon_tui::draw(&mut steady_term, &mut steady_app)?;
    measure("tui_draw_held_notes_steady_200f", || {
        for _ in 0..200 {
            horon_tui::draw(&mut steady_term, &mut steady_app)?;
        }
        black_box(steady_term.backend().buffer().content().len());
        Ok(())
    })?;

    let mut miss_term = Terminal::new(TestBackend::new(120, 36))?;
    measure("tui_draw_held_notes_miss_40f", || {
        let mut app = build_noted_app(300);
        horon_tui::draw(&mut miss_term, &mut app)?;
        for i in 0..40 {
            app.transcript
                .push_user(format!("follow-up question #{i} with extra whitespace   and 한국어"));
            horon_tui::draw(&mut miss_term, &mut app)?;
        }
        black_box(miss_term.backend().buffer().content().len());
        Ok(())
    })?;

    let theme = Theme::default();
    let symbols = Symbols::default();
    let layout = statusline::Layout {
        preset: Preset::Custom,
        left: vec![SegmentId::Status, SegmentId::Model, SegmentId::Path],
        right: vec![SegmentId::Tokens, SegmentId::Usage, SegmentId::ContextPct],
        ..Default::default()
    };
    let data = StatusData {
        provider: Some("anthropic"),
        model: Some("claude-opus-4-6"),
        effort: Some("high"),
        session_id: Some("bench-session-1234"),
        pending_turn: false,
        disconnected: false,
        session_status: Some("idle"),
        prompt: None,
        cwd: Some("/Users/alpox/dev/horon-ai/agent"),
        home: Some("/Users/alpox"),
        tps: None,
        tokens: Some(TokenTotals {
            input: 12_340,
            output: 4_560,
            cache_read: 67_890,
            cache_creation: 1_200,
        }),
        capacity: Some(200_000),
        usage: Some(UsageTotals {
            input: Some(125_400),
            output: Some(34_800),
            cache_read: Some(1_250_000),
            cache_creation: Some(45_000),
        }),
        cost: Some(0.42),
        spinner: None,
        now: None,
        elapsed_ms: None,
        branch: None,
    };
    measure("tui_statusline_render_20000", || {
        let mut total = 0usize;
        for _ in 0..20_000 {
            let line = statusline::render(&data, &layout, &theme, symbols, 120);
            total = total.wrapping_add(line.spans.len());
        }
        black_box(total);
        Ok(())
    })?;

    let events: Vec<Event> = (0..500)
        .map(|i| match i % 5 {
            0 => Event::User(format!("user prompt #{i} asking about package verification and tests")),
            1 => Event::Assistant(format!(
                "assistant answer #{i} with normal prose, numbers 12345, and 한국어 설명이 포함된 응답입니다."
            )),
            2 => Event::Assistant(format!(
                "escaped \u{1b}[31mred\u{1b}[0m and \u{1b}]0;title\u{7} text #{i}\nsecond line"
            )),
            3 => Event::Tool {
                name: "apply_patch".to_string(),
                outcome: Outcome::Done,
            },
            _ => Event::Warning,
        })
        .collect();

    let rec_dir = tempfile::tempdir()?;
    let mut rec_run = 0usize;
    measure("tui_recording_push_500", || {
        rec_run += 1;
        let path = rec_dir.path().join(format!("rec-{rec_run}.jsonl"));
        let mut recorder = Recorder::create(&path, MAX_BYTES).map_err(|e| e.to_string())?;
        for event in &events {
            recorder.push(event).map_err(|e| e.to_string())?;
        }
        black_box(recorder.events());
        Ok(())
    })?;

    let load_path = rec_dir.path().join("rec-1.jsonl");
    measure("tui_recording_load_500", || {
        let loaded = Recording::load(&load_path).map_err(|e| e.to_string())?;
        black_box(loaded.events.len());
        Ok(())
    })?;

    Ok(())
}
