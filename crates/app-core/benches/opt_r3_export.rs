//! Micro-benchmarks for round-3 session export optimization paths in `horon-agent-app-core`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use horon_agent_app_core::message::{ContentBlock, Role};
use horon_agent_app_core::session::StoredMessage;
use horon_agent_app_core::session_export::render_markdown;
use horon_agent_app_core::session_export_html::render_html;
use serde_json::json;

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

fn measure<F: FnMut()>(name: &str, mut work: F) {
    work();
    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _ in 0..RUNS {
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.with(|c| c.set(true));
        let start = Instant::now();
        work();
        let elapsed = start.elapsed();
        COUNTING.with(|c| c.set(false));
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
        times_us.push(elapsed.as_micros());
    }
    times_us.sort_unstable();
    println!(
        "BENCH {name}: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );
}

fn msg(id: usize, role: Role, content: Vec<ContentBlock>) -> StoredMessage {
    StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        id: format!("m-{id}"),
        role,
        content,
        display_role: None,
        turn_start: false,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }
}

fn build_messages(turns: usize) -> Vec<StoredMessage> {
    let mut out = Vec::with_capacity(turns * 4);
    let big_result = "line output with <tag> & \"quotes\" and 'single' chars\n".repeat(160);
    let fake_key = concat!("gh", "p_0123456789abcdefghijklmnopqrstuvwxyz");
    for i in 0..turns {
        out.push(msg(
            i * 4,
            Role::User,
            vec![ContentBlock::Text {
                text: format!("Turn {i}: inspect <config> & verify \"path/to/file_{i}.rs\"?"),
                cache_control: None,
            }],
        ));
        out.push(msg(
            i * 4 + 1,
            Role::Assistant,
            vec![
                ContentBlock::AnthropicThinking {
                    thinking: format!("Step {i}: checking files & dependencies before running bash tool."),
                    signature: "sig".to_string(),
                },
                ContentBlock::ToolUse {
                    id: format!("call_{i}"),
                    name: "bash".to_string(),
                    input: json!({
                        "command": format!("echo run {i} && export TOKEN={fake_key}"),
                        "workdir": "/tmp/project",
                        "flags": ["--verbose", "--check"],
                        "timeout_ms": 5000
                    }),
                    thought_signature: None,
                },
            ],
        ));
        let result_text = if i % 5 == 0 {
            big_result.clone()
        } else {
            format!("ok {i}: <b>clean</b> & ready")
        };
        out.push(msg(
            i * 4 + 2,
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: format!("call_{i}"),
                content: result_text,
                is_error: (i % 7 == 0).then_some(true),
                verdict: None,
            }],
        ));
        out.push(msg(
            i * 4 + 3,
            Role::Assistant,
            vec![
                ContentBlock::Text {
                    text: format!("Completed step {i} cleanly with `<script>alert('x')</script>` escaped."),
                    cache_control: None,
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "AAAA".to_string(),
                },
            ],
        ));
    }
    out
}

fn main() -> Result<(), Box<dyn Error>> {
    let messages = build_messages(100);
    measure("export_render_markdown_400_msgs", || {
        let md = render_markdown(&messages);
        black_box(md.len());
    });
    measure("export_render_html_400_msgs", || {
        let html = render_html(&messages);
        black_box(html.len());
    });
    Ok(())
}
