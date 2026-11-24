//! Micro-benchmarks for chunk-5 optimization paths in `horon-tui`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_tui::overlays::side::{SideOverlay, draw as draw_side};
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

fn build_side_frames() -> Vec<String> {
    let mut frames = Vec::with_capacity(40);
    let mut acc = String::new();
    for step in 0..40 {
        for sub in 0..4 {
            acc.push_str(&format!(
                "Paragraph {step}.{sub}: the store separates durable markdown pages from the derived sqlite index so incremental updates stay localized and safe.\n"
            ));
        }
        acc.push('\n');
        frames.push(acc.clone());
    }
    frames
}

fn main() -> Result<(), Box<dyn Error>> {
    let theme = Theme::default();
    let symbols = Symbols::default();
    let stream_frames = build_side_frames();
    let mut term = Terminal::new(TestBackend::new(100, 32))?;

    measure("tui_side_stream_and_scroll_120", || {
        let mut overlay = SideOverlay::starting(
            1,
            "How does the memory store separate durable pages from derived indexes across turns?".to_string(),
            Some("turn-14".to_string()),
        );
        overlay.started("child-bench".to_string());
        for answer in &stream_frames {
            overlay.stream(answer.clone());
            term.draw(|frame| draw_side(frame, frame.area(), &mut overlay, &theme, symbols))?;
        }
        for i in 0..80 {
            let key = if i % 3 == 0 {
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)
            } else {
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
            };
            let _ = overlay.on_key(key);
            term.draw(|frame| draw_side(frame, frame.area(), &mut overlay, &theme, symbols))?;
        }
        black_box(term.backend().buffer().content().len());
        Ok(())
    })?;

    Ok(())
}
