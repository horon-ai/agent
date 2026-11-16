//! What a dispatch does at its edges: a call that is dropped, a call that runs
//! out of time, and a result too large for the context.
//!
//! The dispatcher's fast path was reworked to do less per call. These cases pin
//! the behaviour that work had to leave alone, through the public dispatch
//! surface: a call that stops early releases everything it held and the next
//! call runs, and a large result is cut at the same size, with the same notice,
//! as before.
//!
//! Every case runs with a private `HORON_HOME`, because a result that is cut is
//! saved under it.

use async_trait::async_trait;
use horon_agent_app_core::tool::inflight::is_tool_in_flight;
use horon_agent_app_core::tool::{Registry, Tool, ToolContext, ToolExecutionMode, ToolOutput, ToolVerdict};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

/// Serializes the cases: each installs process-wide environment variables.
static TEST_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Size above which the dispatcher cuts a result that has no budget of its own
/// (`GENERIC_CONDENSE_BUDGET_BYTES` in `tool/artifacts.rs`). Repeated here on
/// purpose: the test fails if the cut moves.
const CONDENSE_BUDGET_BYTES: usize = 48 * 1024;

/// How long a case waits for the fixture tool to start before it gives up.
const START_DEADLINE: Duration = Duration::from_secs(30);

const HANGING_TOOL: &str = "fixture_hanging";
const QUICK_TOOL: &str = "fixture_quick";
const ROWS_TOOL: &str = "fixture_rows";

/// Sets its flag when dropped, so a test can tell that the future holding it
/// was dropped and not merely left behind.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A tool that reports that it started and then never finishes.
struct HangingTool {
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for HangingTool {
    fn name(&self) -> &str {
        HANGING_TOOL
    }

    fn description(&self) -> &str {
        "Starts and never finishes"
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
        let _flag = DropFlag(Arc::clone(&self.dropped));
        self.started.notify_one();
        std::future::pending::<()>().await;
        Ok(ToolOutput::new("unreachable"))
    }
}

/// A tool with a constant result.
struct QuickTool;

#[async_trait]
impl Tool for QuickTool {
    fn name(&self) -> &str {
        QUICK_TOOL
    }

    fn description(&self) -> &str {
        "Returns at once"
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new("ok"))
    }
}

/// A tool that returns the text it was built with.
struct RowsTool {
    text: String,
}

#[async_trait]
impl Tool for RowsTool {
    fn name(&self) -> &str {
        ROWS_TOOL
    }

    fn description(&self) -> &str {
        "Returns a fixed text"
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new(self.text.clone()))
    }
}

/// Private `HORON_HOME`, runtime directory and `HOME` for one case, restored
/// on drop.
struct IsolatedEnv {
    home: tempfile::TempDir,
    _runtime: tempfile::TempDir,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl IsolatedEnv {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("horon-tool-boundary-home-")
            .tempdir()
            .expect("a private home");
        let runtime = tempfile::Builder::new()
            .prefix("horon-tool-boundary-runtime-")
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

fn context(session_id: &str, working_dir: &Path) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: format!("{session_id}-message"),
        tool_call_id: format!("{session_id}-call"),
        working_dir: Some(working_dir.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// Registry holding the hanging tool and the quick tool, with the hanging
/// tool's two signals.
async fn hanging_registry() -> (Registry, Arc<Notify>, Arc<AtomicBool>) {
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let registry = Registry::empty();
    registry
        .register(
            HANGING_TOOL.to_string(),
            Arc::new(HangingTool {
                started: Arc::clone(&started),
                dropped: Arc::clone(&dropped),
            }),
        )
        .await;
    registry.register(QUICK_TOOL.to_string(), Arc::new(QuickTool)).await;
    (registry, started, dropped)
}

/// Text of `rows` numbered lines, none of which looks like an error line, so
/// the cut carries no "matching lines" block.
fn rows(rows: usize) -> String {
    let mut text = String::with_capacity(rows * 11);
    for row in 0..rows {
        text.push_str(&format!("row {row:06}\n"));
    }
    text
}

async fn rows_registry(text: &str) -> Registry {
    let registry = Registry::empty();
    registry
        .register(ROWS_TOOL.to_string(), Arc::new(RowsTool { text: text.to_string() }))
        .await;
    registry
}

/// The next call on the same registry runs to its result: nothing the stopped
/// call held (the in-flight mark, the hook lock) is still held.
async fn assert_next_call_runs(registry: &Registry, session_id: &str, working_dir: &Path) {
    let output = tokio::time::timeout(
        START_DEADLINE,
        registry.execute(QUICK_TOOL, json!({}), context(session_id, working_dir)),
    )
    .await
    .expect("the next call is not blocked by the stopped one")
    .expect("the next call succeeds");
    assert_eq!(output.output, "ok");
}

/// Aborting the task that runs a dispatch drops the tool's future and clears
/// the in-flight mark, and the registry serves the next call.
#[tokio::test]
async fn a_cancelled_dispatch_releases_the_call() {
    let _env_guard = TEST_ENV.lock().await;
    let env = IsolatedEnv::new();
    let (registry, started, dropped) = hanging_registry().await;
    let ctx = context("boundary-cancel", env.home());
    let call_id = ctx.tool_call_id.clone();

    let running = tokio::spawn({
        let registry = registry.clone();
        async move { registry.execute(HANGING_TOOL, json!({}), ctx).await }
    });
    tokio::time::timeout(START_DEADLINE, started.notified())
        .await
        .expect("the tool started");
    assert!(is_tool_in_flight(&call_id), "a running call is marked in flight");
    assert!(!dropped.load(Ordering::SeqCst), "the tool is still running");

    running.abort();
    let joined = running.await;
    assert!(
        joined.as_ref().is_err_and(tokio::task::JoinError::is_cancelled),
        "the aborted dispatch ends as cancelled, got {joined:?}",
    );

    assert!(dropped.load(Ordering::SeqCst), "cancelling drops the tool's future");
    assert!(!is_tool_in_flight(&call_id), "a cancelled call is no longer in flight");
    assert_next_call_runs(&registry, "boundary-cancel-next", env.home()).await;
}

/// A dispatch that outlives its caller's deadline is dropped by that deadline:
/// the tool's future is dropped, the mark is cleared, the next call runs.
#[tokio::test]
async fn a_dispatch_that_times_out_releases_the_call() {
    let _env_guard = TEST_ENV.lock().await;
    let env = IsolatedEnv::new();
    let (registry, started, dropped) = hanging_registry().await;
    let ctx = context("boundary-timeout", env.home());
    let call_id = ctx.tool_call_id.clone();

    {
        let call = registry.execute(HANGING_TOOL, json!({}), ctx);
        tokio::pin!(call);
        tokio::select! {
            result = &mut call => panic!("the hanging tool returned: {result:?}"),
            started = tokio::time::timeout(START_DEADLINE, started.notified()) => {
                started.expect("the tool started");
            }
        }
        assert!(is_tool_in_flight(&call_id), "a running call is marked in flight");

        let timed_out = tokio::time::timeout(Duration::from_millis(50), &mut call).await;
        assert!(timed_out.is_err(), "the deadline passes before the tool returns");
    }

    assert!(dropped.load(Ordering::SeqCst), "the deadline drops the tool's future");
    assert!(!is_tool_in_flight(&call_id), "a timed-out call is no longer in flight");
    assert_next_call_runs(&registry, "boundary-timeout-next", env.home()).await;
}

/// A result of exactly the budget is returned as the tool produced it.
#[tokio::test]
async fn a_result_at_the_budget_is_returned_whole() {
    let _env_guard = TEST_ENV.lock().await;
    let env = IsolatedEnv::new();
    let mut text = rows(CONDENSE_BUDGET_BYTES / 11 + 1);
    text.truncate(CONDENSE_BUDGET_BYTES);
    assert_eq!(text.len(), CONDENSE_BUDGET_BYTES);
    let registry = rows_registry(&text).await;

    let output = registry
        .execute(ROWS_TOOL, json!({}), context("boundary-at-budget", env.home()))
        .await
        .expect("the call succeeds");

    assert_eq!(output.output, text, "a result at the budget is not cut");
    assert_eq!(output.verdict, None, "a whole result carries no cut verdict");
}

/// A result over the budget is cut to head, notice and tail, the full text is
/// saved, and the verdict says the text is a cut.
#[tokio::test]
async fn a_result_over_the_budget_is_cut_with_a_pointer() {
    let _env_guard = TEST_ENV.lock().await;
    let env = IsolatedEnv::new();
    let text = rows(20_000);
    assert!(text.len() > 4 * CONDENSE_BUDGET_BYTES);
    let registry = rows_registry(&text).await;

    let output = registry
        .execute(ROWS_TOOL, json!({}), context("boundary-over-budget", env.home()))
        .await
        .expect("the call succeeds");

    assert_eq!(output.verdict, Some(ToolVerdict::Truncated));
    assert!(
        output.output.len() <= CONDENSE_BUDGET_BYTES,
        "the cut fits the budget, got {} bytes",
        output.output.len(),
    );
    assert!(
        output.output.starts_with("row 000000\nrow 000001\n"),
        "the head is kept"
    );
    assert!(output.output.ends_with("row 019999\n"), "the tail is kept");

    let notice = output
        .output
        .lines()
        .find(|line| line.starts_with("[horon: "))
        .expect("the cut names what it left out");
    assert!(
        notice.contains(&format!("lines omitted from `{ROWS_TOOL}` output. Full output (")),
        "unexpected notice: {notice}",
    );
    assert!(
        notice.contains(&format!(
            "({} bytes, 20000 lines): read file_path=\"artifact://",
            text.len()
        )),
        "the notice points at the saved full text: {notice}",
    );
    assert!(
        notice.ends_with("with start_line/end_line.]"),
        "unexpected notice: {notice}"
    );
    assert_eq!(
        output.output.lines().filter(|line| line.starts_with('[')).count(),
        1,
        "rows without error words add no matching-lines block",
    );
}

/// `accept_large_output` keeps a result over the budget whole: the caller asked
/// for the full text, and the dispatcher's cut steps aside.
#[tokio::test]
async fn an_accepted_large_result_is_returned_whole() {
    let _env_guard = TEST_ENV.lock().await;
    let env = IsolatedEnv::new();
    let text = rows(CONDENSE_BUDGET_BYTES / 11 + 64);
    assert!(text.len() > CONDENSE_BUDGET_BYTES);
    let registry = rows_registry(&text).await;

    for accepted in [json!(true), json!("true")] {
        let output = registry
            .execute(
                ROWS_TOOL,
                json!({ "accept_large_output": accepted }),
                context("boundary-accepted", env.home()),
            )
            .await
            .expect("the call succeeds");

        assert_eq!(output.output, text, "an accepted result is not cut");
        assert_eq!(output.verdict, None, "an accepted result carries no cut verdict");
    }
}
