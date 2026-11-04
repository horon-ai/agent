//! Foreground `bash` runs handed to the ACP client's terminal.
//!
//! The client owns the process, so the whole exchange is
//! `terminal/create` -> `terminal/wait_for_exit` -> `terminal/output` ->
//! `terminal/release` (spec Terminals). `wait_for_exit` carries the call's own
//! deadline: when it expires the command is killed instead of promoted to a
//! background task, because a terminal that lives in the editor cannot be
//! followed from the daemon.
//!
//! Release is unconditional. Every path that created a terminal goes through
//! [`ReleaseOnDrop`], which sends the release itself when the tool finishes and
//! sends it from a detached task when the turn is cancelled mid-wait and this
//! code's future is dropped.

use crate::tool::client_bridge::ClientBridge;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Deadline for the short control requests (`create`, `output`, `kill`,
/// `release`). The command's deadline is the caller's timeout; this only bounds
/// a client that stops answering a control message.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

/// One foreground call as the client's terminal needs it.
pub(super) struct TerminalRequest<'a> {
    /// The command text, already wrapped by the caller (session cwd capture,
    /// repository cargo wrapper) exactly as the local spawner would run it.
    pub command: String,
    pub cwd: Option<&'a Path>,
    pub env: Vec<(String, String)>,
    pub output_byte_limit: usize,
    /// The call's deadline, measured on `terminal/wait_for_exit`.
    pub timeout: Duration,
    /// The same deadline in milliseconds, for the timeout text.
    pub timeout_ms: u64,
    pub tool_call_id: Option<String>,
}

/// What the client's terminal reported, before the caller formats it.
pub(super) struct TerminalOutcome {
    pub output: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    /// `Some(timeout_ms)` when the command outlived its deadline and was killed.
    pub timed_out_ms: Option<u64>,
}

/// Ask the client to start the command.
///
/// A failure here means the client cannot serve a terminal for this call, and
/// the caller runs the command locally instead; no release is owed, because no
/// terminal exists as far as this side knows. A client that answers only after
/// the deadline below leaves its terminal id with no waiter, so the answer is
/// reported as unknown and dropping that terminal is the client's job: the
/// command may then have run twice, once in the client and once locally.
pub(super) async fn create(bridge: &ClientBridge, request: &TerminalRequest<'_>) -> Result<String> {
    let env: Vec<Value> = request
        .env
        .iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect();
    let mut params = json!({
        "command": "bash",
        "args": ["-c", request.command],
        "env": env,
        "outputByteLimit": request.output_byte_limit,
    });
    if let Some(cwd) = request.cwd {
        // The spec requires an absolute cwd; the local spawner would accept a
        // relative one and resolve it against the daemon's own directory.
        params["cwd"] = json!(absolute(cwd));
    }
    let answer = bridge
        .call("terminal/create", params, request.tool_call_id.clone(), CONTROL_TIMEOUT)
        .await?;
    answer
        .get("terminalId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("the ACP client answered terminal/create without a terminalId: {answer}"))
}

fn absolute(path: &Path) -> std::path::PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Wait for the command, read what it produced, and release the terminal.
pub(super) async fn run(
    bridge: Arc<ClientBridge>,
    terminal_id: String,
    request: &TerminalRequest<'_>,
) -> Result<TerminalOutcome> {
    let release = ReleaseOnDrop::arm(Arc::clone(&bridge), terminal_id.clone());
    let outcome = wait_and_collect(&bridge, &terminal_id, request).await;
    release.release().await;
    outcome
}

async fn wait_and_collect(
    bridge: &ClientBridge,
    terminal_id: &str,
    request: &TerminalRequest<'_>,
) -> Result<TerminalOutcome> {
    // The answer deadline is longer than the command's so that the command's own
    // deadline below is what decides a timeout; a client answering a moment
    // before it still lands in the success branch.
    let waited = tokio::time::timeout(
        request.timeout,
        bridge.call(
            "terminal/wait_for_exit",
            json!({"terminalId": terminal_id}),
            None,
            request.timeout + CONTROL_TIMEOUT,
        ),
    )
    .await;

    match waited {
        Ok(Ok(value)) => {
            let exit = ExitStatus::from_value(&value);
            collect(bridge, terminal_id, Some(exit), None).await
        }
        Ok(Err(error)) => Err(error.context("the ACP client terminal could not report the command's exit")),
        Err(_) => {
            // Outlived the deadline: kill first, then read what it managed to
            // print, so the model sees why it hung instead of only that it did.
            let _ = bridge
                .call(
                    "terminal/kill",
                    json!({"terminalId": terminal_id}),
                    None,
                    CONTROL_TIMEOUT,
                )
                .await;
            match collect(bridge, terminal_id, None, Some(request.timeout_ms)).await {
                Ok(outcome) => Ok(outcome),
                Err(error) => {
                    crate::logging::warn(&format!(
                        "[bash] ACP client terminal output after a kill failed: {error:#}"
                    ));
                    Ok(TerminalOutcome {
                        output: String::new(),
                        truncated: false,
                        exit_code: None,
                        signal: None,
                        timed_out_ms: Some(request.timeout_ms),
                    })
                }
            }
        }
    }
}

async fn collect(
    bridge: &ClientBridge,
    terminal_id: &str,
    exit: Option<ExitStatus>,
    timed_out_ms: Option<u64>,
) -> Result<TerminalOutcome> {
    let answer = bridge
        .call(
            "terminal/output",
            json!({"terminalId": terminal_id}),
            None,
            CONTROL_TIMEOUT,
        )
        .await?;
    let exit = match exit {
        Some(exit) => Some(exit),
        None => answer.get("exitStatus").map(ExitStatus::from_value),
    };
    Ok(TerminalOutcome {
        output: answer
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        truncated: answer.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        exit_code: exit.as_ref().and_then(|exit| exit.code),
        signal: exit.and_then(|exit| exit.signal),
        timed_out_ms,
    })
}

struct ExitStatus {
    code: Option<i32>,
    signal: Option<String>,
}

impl ExitStatus {
    fn from_value(value: &Value) -> Self {
        Self {
            code: value
                .get("exitCode")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok()),
            signal: value.get("signal").and_then(Value::as_str).map(str::to_string),
        }
    }
}

/// Hands the terminal back exactly once: inline on the normal path, from a
/// detached task when the future is dropped by a cancelled turn.
struct ReleaseOnDrop {
    bridge: Arc<ClientBridge>,
    terminal_id: String,
    armed: bool,
}

impl ReleaseOnDrop {
    fn arm(bridge: Arc<ClientBridge>, terminal_id: String) -> Self {
        Self {
            bridge,
            terminal_id,
            armed: true,
        }
    }

    async fn release(mut self) {
        self.armed = false;
        let _ = self
            .bridge
            .call(
                "terminal/release",
                json!({"terminalId": self.terminal_id}),
                None,
                CONTROL_TIMEOUT,
            )
            .await;
    }
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Without a runtime the process is shutting down and the client
        // connection goes with it, so there is nobody left to release to.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let bridge = Arc::clone(&self.bridge);
        let terminal_id = std::mem::take(&mut self.terminal_id);
        handle.spawn(async move {
            let _ = bridge
                .call(
                    "terminal/release",
                    json!({"terminalId": terminal_id}),
                    None,
                    CONTROL_TIMEOUT,
                )
                .await;
        });
    }
}
