//! Allocation cost of one tool dispatch, reported to the structural gate.
//!
//! `Registry::execute` is the path every tool call takes: name resolution,
//! policy, argument repair, the approval gate, secret restore, the two
//! `TOOL_LIFECYCLE` log lines and the crash ring. CPU time on that path is a
//! wall-clock number that moves with the runner, so the gate counts what it
//! can count exactly instead: how many heap allocations one dispatch makes and
//! how many bytes it asks for. A serialization, a deep copy of the input or a
//! rebuilt log field shows up in both numbers on every machine.
//!
//! The counter is a `#[global_allocator]`, which is why this is its own
//! integration target: the allocator is per binary, and neither the agent
//! binary nor the shared lib-test binary may carry it. Only the thread that
//! runs the dispatch is counted, so the logger's cleanup thread and the
//! watchdog add no noise.
//!
//! The fixture tool does nothing, on purpose. Its input has the shape of a
//! file write (`file_path` plus `content`), so the path policy and the
//! per-key size fields run as they do for a real call, and everything counted
//! is dispatch overhead rather than the tool's own work.

use async_trait::async_trait;
use horon_agent_app_core::tool::{Registry, Tool, ToolContext, ToolExecutionMode, ToolOutput};
use serde_json::{Value, json};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Set only around a measured dispatch, on the thread that runs it.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

/// The system allocator, with a count of the requests the measuring thread
/// makes. A `realloc` counts as one request for the new size.
struct Counting;

impl Counting {
    fn record(bytes: usize) {
        // `try_with` instead of `with`: a thread that is being torn down may
        // still allocate after its thread-locals are gone.
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    }
}

// SAFETY: every method forwards to `System` with the arguments it was given;
// the bookkeeping touches only atomics and a `Cell<bool>` and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(new_size);
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const TOOL_NAME: &str = "fixture_dispatch";
const TOOL_OUTPUT: &str = "ok";

/// Dispatches that run before the measured ones. The crash ring holds 64
/// operations and the argument-repair cache, the logger and the lazy statics
/// all fill on first use; past this count every dispatch does the same work.
const WARMUP_DISPATCHES: usize = 96;
/// Measured dispatches per input size.
const MEASURED_DISPATCHES: usize = 200;
/// Size of the `content` argument of the large input.
const LARGE_CONTENT_BYTES: usize = 10 * 1024;

/// A tool with a fixed schema and a constant result.
struct FixtureTool;

#[async_trait]
impl Tool for FixtureTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        "Accepts a file path and its content, and does nothing with them"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "File the call names" },
                "content": { "type": "string", "description": "Text the call carries" }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new(TOOL_OUTPUT))
    }
}

/// Private `HORON_HOME`, runtime directory and `HOME` for the test, restored
/// on drop. The log file the dispatch writes lands under this home.
struct IsolatedEnv {
    home: tempfile::TempDir,
    _runtime: tempfile::TempDir,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl IsolatedEnv {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("horon-tool-dispatch-home-")
            .tempdir()
            .expect("a private home");
        let runtime = tempfile::Builder::new()
            .prefix("horon-tool-dispatch-runtime-")
            .tempdir()
            .expect("a private runtime directory");
        let previous = ["HORON_HOME", "HORON_AGENT_RUNTIME_DIR", "HOME"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        horon_agent_app_core::env::set_var("HORON_HOME", home.path());
        horon_agent_app_core::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
        horon_agent_app_core::env::set_var("HOME", home.path());
        horon_agent_app_core::config::invalidate_config_cache();
        Self {
            home,
            _runtime: runtime,
            previous,
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => horon_agent_app_core::env::set_var(key, value),
                None => horon_agent_app_core::env::remove_var(key),
            }
        }
        horon_agent_app_core::config::invalidate_config_cache();
    }
}

fn context(call: &str, working_dir: &Path) -> ToolContext {
    ToolContext {
        session_id: "tool-dispatch-perf".to_string(),
        message_id: "tool-dispatch-perf-message".to_string(),
        tool_call_id: call.to_string(),
        working_dir: Some(working_dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn small_input() -> Value {
    json!({ "file_path": "notes/summary.md", "content": "one short line\n" })
}

fn large_input() -> Value {
    let line = "The dispatcher must not copy or re-serialize this text more than once.\n";
    let mut content = String::with_capacity(LARGE_CONTENT_BYTES + line.len());
    while content.len() < LARGE_CONTENT_BYTES {
        content.push_str(line);
    }
    content.truncate(LARGE_CONTENT_BYTES);
    json!({ "file_path": "notes/report.md", "content": content })
}

/// Allocation requests and requested bytes of the measured dispatches.
struct Cost {
    allocations: u64,
    bytes: u64,
}

/// Run `dispatches` calls and count what the dispatcher allocates.
///
/// The input and the context are built outside the counted window, because
/// the caller owns them; the window covers `Registry::execute` only.
async fn dispatch(
    registry: &Registry,
    label: &str,
    dispatches: usize,
    working_dir: &Path,
    input: impl Fn() -> Value,
) -> Cost {
    let allocations_before = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes_before = ALLOCATED_BYTES.load(Ordering::Relaxed);
    for index in 0..dispatches {
        let input = input();
        let ctx = context(&format!("{label}-{index}"), working_dir);
        COUNTING.with(|counting| counting.set(true));
        let result = registry.execute(TOOL_NAME, input, ctx).await;
        COUNTING.with(|counting| counting.set(false));
        let output = result.unwrap_or_else(|error| panic!("{label} dispatch {index} failed: {error:#}"));
        assert_eq!(
            output.output, TOOL_OUTPUT,
            "{label} dispatch {index} returned another result"
        );
    }
    Cost {
        allocations: ALLOCATIONS.load(Ordering::Relaxed) - allocations_before,
        bytes: ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_before,
    }
}

/// Reports what one dispatch allocates for a small and a 10 KB input.
///
/// The bounds live in `scripts/perf/structural_budget.json`; this test only
/// proves the measurement is real. Every dispatch must succeed, and each one
/// must leave its `start` and `done` lifecycle lines in the log file: a
/// logger that was never initialised would make the path cheaper without
/// anything having been improved.
#[tokio::test]
async fn one_dispatch_allocates_a_bounded_amount() {
    let env = IsolatedEnv::new();
    horon_agent_app_core::logging::init();

    let registry = Registry::empty();
    registry.register(TOOL_NAME.to_string(), Arc::new(FixtureTool)).await;

    dispatch(&registry, "warmup-small", WARMUP_DISPATCHES, env.home(), small_input).await;
    dispatch(&registry, "warmup-large", WARMUP_DISPATCHES, env.home(), large_input).await;

    let small = dispatch(&registry, "small", MEASURED_DISPATCHES, env.home(), small_input).await;
    let large = dispatch(&registry, "large", MEASURED_DISPATCHES, env.home(), large_input).await;

    let log_path = horon_agent_app_core::logging::log_path().expect("the logger resolved a log file");
    let contents =
        std::fs::read_to_string(&log_path).unwrap_or_else(|error| panic!("read {}: {error}", log_path.display()));
    for phase in ["start", "done"] {
        let lines = contents
            .lines()
            .filter(|line| {
                line.contains("TOOL_LIFECYCLE")
                    && line.contains(&format!("phase={phase}"))
                    && (line.contains("tool_call_id=small-") || line.contains("tool_call_id=large-"))
            })
            .count();
        assert_eq!(
            lines,
            MEASURED_DISPATCHES * 2,
            "every measured dispatch writes one `{phase}` lifecycle line to {}",
            log_path.display()
        );
    }

    assert!(
        small.allocations > 0,
        "the allocation counter recorded nothing for the small input"
    );
    assert!(
        large.allocations > 0,
        "the allocation counter recorded nothing for the large input"
    );

    println!(
        "STRUCTURAL_PERF section=tool_dispatch source=app-core dispatches={MEASURED_DISPATCHES} small_allocs={} small_alloc_bytes={} large_allocs={} large_alloc_bytes={}",
        small.allocations, small.bytes, large.allocations, large.bytes
    );
}
