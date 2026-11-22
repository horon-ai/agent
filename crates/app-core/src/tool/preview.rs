//! Model-free preview of the read-only built-ins.
//!
//! This is not a dispatcher: exactly three tools can be named, each runs once
//! through its own `execute` with its own path checks, and no session, provider
//! or approval flow is involved. The output is redacted and capped.

use super::{Tool, ToolContext, ToolExecutionMode};
use anyhow::{Result, bail};
use serde_json::Value;
use std::path::PathBuf;

/// Largest preview returned, in bytes.
pub const PREVIEW_MAX_BYTES: usize = 32 * 1024;

#[derive(Debug, serde::Serialize)]
pub struct Preview {
    pub tool: &'static str,
    /// For `websearch`, the configured engine order. Never an endpoint or credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    pub output: String,
    pub truncated: bool,
}

pub async fn run(tool: &str, input: Value, working_dir: PathBuf) -> Result<Preview> {
    let (name, backend, result): (&'static str, Option<String>, _) = match tool {
        "read" => (
            "read",
            None,
            run_tool(&super::read::ReadTool::new(), input, working_dir).await,
        ),
        "agentgrep" => (
            "agentgrep",
            None,
            run_tool(&super::agentgrep::AgentGrepTool::new(), input, working_dir).await,
        ),
        "websearch" => {
            let config = &crate::config::config().websearch;
            let order: Vec<&str> = std::iter::once(config.engine)
                .chain(config.fallback_engines.iter().copied())
                .map(|engine| engine.as_str())
                .collect();
            (
                "websearch",
                Some(order.join(" > ")),
                run_tool(&super::websearch::WebSearchTool::new(), input, working_dir).await,
            )
        }
        other => bail!("preview supports only read, agentgrep and websearch, not {other:?}"),
    };
    let text = result?;
    let redacted = crate::secrets::obfuscate(&text, &mut crate::secrets::SecretVault::new(), &[]);
    let kept = crate::util::truncate_str(&redacted, PREVIEW_MAX_BYTES);
    Ok(Preview {
        tool: name,
        backend,
        truncated: kept.len() < redacted.len(),
        output: kept.to_owned(),
    })
}

async fn run_tool(tool: &dyn Tool, input: Value, working_dir: PathBuf) -> Result<String> {
    let ctx = ToolContext {
        session_id: "tool-preview".to_owned(),
        message_id: "tool-preview".to_owned(),
        tool_call_id: "tool-preview".to_owned(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    };
    Ok(tool.execute(input, ctx).await?.output)
}
