//! Micro-benchmarks for round-2 optimization paths in `horon-agent-app-core`.
//!
//! Exercises the real public `Registry::execute("apply_patch", ...)` path on
//! multi-hunk unified patches over 4,000-line LF and CRLF files.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use horon_agent_app_core::message::{Message, ToolDefinition};
use horon_agent_app_core::provider::{EventStream, Provider};
use horon_agent_app_core::tool::{Registry, ToolContext, ToolExecutionMode};
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

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Err(anyhow::anyhow!("stub provider"))
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(StubProvider)
    }
}

const RUNS: usize = 7;

fn build_file_and_patch(lines: usize, crlf: bool) -> (String, String) {
    let sep = if crlf { "\r\n" } else { "\n" };
    let mut content = String::with_capacity(lines * 64);
    for i in 1..=lines {
        content.push_str(&format!(
            "line {i:05}: the quick brown fox jumps over the lazy dog in module_{i}\n"
        ));
    }
    if crlf {
        content = content.replace('\n', sep);
    }
    let mut patch = String::from("--- a/sample.txt\n+++ b/sample.txt\n");
    for hunk_line in [10usize, 1000, 2000, 3000, 3990] {
        patch.push_str(&format!(
            "@@ -{start},3 +{start},3 @@\n \
             line {prev:05}: the quick brown fox jumps over the lazy dog in module_{prev}\n\
             -line {curr:05}: the quick brown fox jumps over the lazy dog in module_{curr}\n\
             +line {curr:05}: updated content for benchmark hunk at module_{curr}\n \
             line {next:05}: the quick brown fox jumps over the lazy dog in module_{next}\n",
            start = hunk_line - 1,
            prev = hunk_line - 1,
            curr = hunk_line,
            next = hunk_line + 1
        ));
    }
    (content, patch)
}

fn main() -> Result<(), Box<dyn Error>> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn Error>> {
    let home = tempfile::tempdir()?;
    let work = tempfile::tempdir()?;
    horon_agent_app_core::env::set_var("HORON_HOME", home.path());
    horon_agent_app_core::env::set_var("HOME", home.path());
    horon_agent_app_core::config::invalidate_config_cache();

    let registry = Registry::new(Arc::new(StubProvider)).await;
    for (label, crlf) in [("patch_apply_4000_lf_x10", false), ("patch_apply_4000_crlf_x10", true)] {
        let (original, patch_text) = build_file_and_patch(4_000, crlf);
        let file_path = work.path().join("sample.txt");

        // Warmup
        fs::write(&file_path, &original)?;
        let ctx = ToolContext {
            session_id: "bench-patch".to_string(),
            message_id: "bench-msg".to_string(),
            tool_call_id: "warmup".to_string(),
            working_dir: Some(work.path().to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        };
        let out = registry
            .execute("apply_patch", json!({ "patch_text": patch_text }), ctx)
            .await?;
        black_box(out.output.len());

        let mut times_us = Vec::with_capacity(RUNS);
        let mut last_allocs = 0u64;
        let mut last_bytes = 0u64;
        for run in 0..RUNS {
            let mut run_us = 0u128;
            let mut run_allocs = 0u64;
            let mut run_bytes = 0u64;
            for iter in 0..10 {
                fs::write(&file_path, &original)?;
                let input = json!({ "patch_text": patch_text });
                let ctx = ToolContext {
                    session_id: "bench-patch".to_string(),
                    message_id: "bench-msg".to_string(),
                    tool_call_id: format!("{label}-{run}-{iter}"),
                    working_dir: Some(work.path().to_path_buf()),
                    stdin_request_tx: None,
                    graceful_shutdown_signal: None,
                    execution_mode: ToolExecutionMode::Direct,
                };
                let a0 = ALLOCS.load(Ordering::Relaxed);
                let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
                COUNTING.with(|c| c.set(true));
                let start = Instant::now();
                let res = registry.execute("apply_patch", input, ctx).await;
                let elapsed = start.elapsed();
                COUNTING.with(|c| c.set(false));
                let out = res?;
                black_box(out.output.len());
                run_us += elapsed.as_micros();
                run_allocs += ALLOCS.load(Ordering::Relaxed) - a0;
                run_bytes += ALLOC_BYTES.load(Ordering::Relaxed) - b0;
            }
            times_us.push(run_us);
            last_allocs = run_allocs;
            last_bytes = run_bytes;
        }
        times_us.sort_unstable();
        println!(
            "BENCH {label}: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
            times_us[0],
            times_us[RUNS / 2],
            times_us[RUNS - 1]
        );
    }
    Ok(())
}
