//! Opt-in Python cells with a process scoped to the owning session.

mod bridge;
mod host;
mod output;
#[cfg(test)]
mod tests;

use super::{Tool, ToolContext, ToolEffects, ToolOutput, WeakRegistry};
use anyhow::{Context, Result};
use async_trait::async_trait;
use horon_agent_tool_types::{ToolRejected, ToolVerdict};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub(super) const NAME: &str = "eval";
pub(super) const APPROVAL_REASON: &str =
    "Python cells run with the agent's OS permissions and require explicit approval";
const MAX_TIMEOUT: u64 = 3600;
const DEFAULT_TIMEOUT: u64 = 30;

pub(super) struct EvalTool {
    registry: WeakRegistry,
}

impl EvalTool {
    pub(super) fn new(registry: WeakRegistry) -> Self {
        Self { registry }
    }
}

pub(super) use bridge::{AbortCommandOnDrop, inside_bridge, owns_subcall};
pub(super) use host::stop_child;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cell {
    code: String,
    timeout: Option<u64>,
    reset: Option<bool>,
    #[serde(rename = "intent")]
    _intent: Option<String>,
    #[serde(rename = "accept_large_output")]
    _accept_large_output: Option<bool>,
}

#[derive(Default)]
struct Slot {
    kernel: Option<(PathBuf, host::Kernel)>,
    stopping: Option<tokio::task::JoinHandle<()>>,
    started: bool,
}

#[derive(Default)]
struct SessionKernel {
    slot: tokio::sync::Mutex<Slot>,
    retired: tokio_util::sync::CancellationToken,
}

// Lookups on the default-off path must not initialize a registry.
static SESSIONS: OnceLock<Mutex<HashMap<String, Arc<SessionKernel>>>> = OnceLock::new();

fn get_session(session_id: &str) -> Arc<SessionKernel> {
    let mut sessions = SESSIONS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A cell usually reuses its session, so look it up before paying for the
    // owned key the map would only need on the first cell.
    if let Some(session) = sessions.get(session_id) {
        return Arc::clone(session);
    }
    let session = Arc::new(SessionKernel::default());
    sessions.insert(session_id.to_owned(), Arc::clone(&session));
    session
}

pub(crate) fn forget_session(session_id: &str) {
    let Some(sessions) = SESSIONS.get() else { return };
    let session = sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_id);
    if let Some(session) = session {
        session.retired.cancel();
        if let Ok(mut slot) = session.slot.try_lock()
            && let Some((_, kernel)) = slot.kernel.take()
        {
            drop(kernel.begin_stop());
        }
    }
}

pub(crate) fn release_idle_kernel(session_id: &str) {
    let Some(sessions) = SESSIONS.get() else { return };
    let session = sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session_id)
        .cloned();
    if let Some(session) = session
        && let Ok(mut slot) = session.slot.try_lock()
        && let Some((_, kernel)) = slot.kernel.take()
    {
        slot.stopping = kernel.begin_stop();
    }
}

pub(super) fn enabled() -> bool {
    crate::config::config().tools.eval
}

pub(super) async fn working_dir(ctx: &ToolContext) -> Result<PathBuf> {
    if inside_bridge(&ctx.tool_call_id) {
        return Err(ToolRejected("Python tool calls cannot start another Python cell.".into()).into());
    }
    let (tiers, policy) = super::toolbox::tiers_for_session(&ctx.session_id);
    let config = crate::config::config();
    if !config.tools.eval
        || tiers.tier(NAME) == crate::config::LoadMode::Off
        || (policy.is_none() && config.tools.selection().disabled_tools.contains(NAME))
    {
        return Err(ToolRejected("Python eval is disabled in this session.".into()).into());
    }
    match ctx.working_dir.as_deref() {
        Some(path) => tokio::fs::canonicalize(path).await,
        None => tokio::fs::canonicalize(std::env::current_dir()?).await,
    }
    .context("resolve the Python eval working directory")
}

#[async_trait]
impl Tool for EvalTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Run a Python cell after explicit approval. State is kept in the session until reset, timeout or kernel shutdown. Python runs with the agent's OS permissions."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["code"],
            "additionalProperties": false,
            "properties": {
                "code": {"type": "string", "description": "Python code for one cell."},
                "timeout": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT, "default": DEFAULT_TIMEOUT},
                "reset": {"type": "boolean", "default": false, "description": "Discard the current Python kernel before running this cell."}
            }
        })
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::process()
            .union(ToolEffects::write())
            .union(ToolEffects::network())
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        if !enabled() {
            return Err(ToolRejected("Python eval is disabled. Set [tools] eval = true to opt in.".into()).into());
        }
        let input: Cell = serde_json::from_value(input).context("invalid Python eval arguments")?;
        if !host::code_fits_frame(&input.code) {
            return Err(ToolRejected("Python eval cell exceeds the 256 KiB encoded request limit".into()).into());
        }
        let timeout = input.timeout.unwrap_or(DEFAULT_TIMEOUT);
        if !(1..=MAX_TIMEOUT).contains(&timeout) {
            return Err(ToolRejected("Python eval timeout must be 1 to 3600 seconds.".into()).into());
        }
        let cwd = ctx
            .working_dir
            .as_deref()
            .context("Python eval needs a validated working directory")?;
        let session = get_session(&ctx.session_id);
        let bridge = bridge::Bridge {
            registry: self
                .registry
                .upgrade()
                .context("Python tool registry is no longer available")?,
            context: &ctx,
        };
        let mut capture = output::Capture::default();
        let mut restarted = false;
        let mut status = String::new();
        let active = run_cell(&session, cwd, input, &bridge, &mut capture, &mut restarted);
        let cancelled = async {
            match ctx.graceful_shutdown_signal.as_ref() {
                Some(signal) => signal.notified().await,
                None => std::future::pending().await,
            }
        };
        let verdict = tokio::select! {
            biased;
            _ = session.retired.cancelled() => ToolVerdict::Interrupted,
            _ = cancelled => ToolVerdict::Interrupted,
            _ = tokio::time::sleep(Duration::from_secs(timeout)) => {
                status = format!("\nPython eval timed out after {timeout}s.\n");
                ToolVerdict::Interrupted
            }
            result = active => match result {
                Ok(true) => ToolVerdict::Ok,
                Ok(false) => ToolVerdict::Failed,
                Err(error) => {
                    status = format!("\n{error:#}\n");
                    ToolVerdict::Failed
                }
            }
        };
        let notice = if restarted {
            "[Python kernel restarted; previous state is unavailable.]\n"
        } else {
            ""
        };
        Ok(capture.finish(&ctx.session_id, notice, &status, verdict))
    }
}

async fn run_cell(
    session: &SessionKernel,
    cwd: &Path,
    cell: Cell,
    bridge: &bridge::Bridge<'_>,
    capture: &mut output::Capture,
    restarted: &mut bool,
) -> Result<bool> {
    let mut slot = session.slot.lock().await;
    wait_for_stop(&mut slot).await?;
    let mut current = slot.kernel.take();
    if current
        .as_ref()
        .is_some_and(|(working_dir, _)| cell.reset == Some(true) || working_dir != cwd)
        && let Some((_, kernel)) = current.take()
    {
        slot.stopping = kernel.begin_stop();
        wait_for_stop(&mut slot).await?;
    }
    let current = match current {
        Some(kernel) => kernel,
        None => {
            *restarted = slot.started;
            (cwd.to_path_buf(), host::Kernel::spawn(cwd).await?)
        }
    };
    slot.started = true;
    let mut active = ActiveCell {
        slot,
        kernel: Some(current),
    };
    let (_, kernel) = active.kernel.as_mut().context("lost Python eval kernel")?;
    let result = kernel.execute(&cell.code, bridge, capture).await;
    if result.is_ok() {
        active.slot.kernel = active.kernel.take();
    }
    result
}

async fn wait_for_stop(slot: &mut Slot) -> Result<()> {
    if let Some(stopping) = &mut slot.stopping {
        let result = stopping.await;
        slot.stopping = None;
        result.context("wait for previous Python kernel to stop")?;
    }
    Ok(())
}

struct ActiveCell<'a> {
    slot: tokio::sync::MutexGuard<'a, Slot>,
    kernel: Option<(PathBuf, host::Kernel)>,
}

impl Drop for ActiveCell<'_> {
    fn drop(&mut self) {
        if let Some((_, kernel)) = self.kernel.take() {
            self.slot.stopping = kernel.begin_stop();
        }
    }
}
