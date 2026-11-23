use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::process::Command;

use super::ToolContext;

pub(super) async fn load(start_dir: Option<&Path>, ctx: &ToolContext) -> Result<Vec<(String, String)>> {
    if !crate::config::config().tools.bash_direnv || !horon_agent_platform::term::shell_is_posix() {
        return Ok(Vec::new());
    }
    let current_dir = std::env::current_dir().context("direnv could not resolve the current directory")?;
    let start_dir = start_dir.unwrap_or(&current_dir);
    let workspace = ctx.working_dir.as_deref().unwrap_or(&current_dir);
    load_for_workspace(start_dir, workspace).await
}

async fn load_for_workspace(start_dir: &Path, workspace: &Path) -> Result<Vec<(String, String)>> {
    let workspace = tokio::fs::canonicalize(workspace)
        .await
        .context("direnv could not resolve the workspace")?;
    let start_dir = tokio::fs::canonicalize(start_dir)
        .await
        .context("direnv could not resolve the shell directory")?;
    // Skip automatic loading for calls that deliberately leave the session workspace.
    if !start_dir.starts_with(&workspace) {
        return Ok(Vec::new());
    }
    let status = direnv_json(&["status", "--json"], &start_dir).await?;
    let rc = status
        .pointer("/state/foundRC")
        .context("direnv status did not report an environment file")?;
    if rc.is_null() {
        return Ok(Vec::new());
    }
    let rc_path = rc
        .get("path")
        .and_then(Value::as_str)
        .context("direnv status omitted the environment file path")?;
    let rc_path = tokio::fs::canonicalize(rc_path)
        .await
        .context("direnv could not resolve the environment file")?;
    if !rc_path.starts_with(&workspace) {
        bail!("direnv environment file is outside the session workspace");
    }
    if !crate::workspace_trust::allows_shell_hooks(&workspace) {
        bail!(
            "direnv needs Horon workspace trust. Restart with --trust-workspace for this workspace, or use HORON_AGENT_WORKSPACE_TRUST=trust for the process"
        );
    }
    if rc.get("allowed").and_then(Value::as_u64) != Some(0) {
        bail!("direnv blocked the environment file. Review it and run `direnv allow` in its directory");
    }
    let exported = direnv_json(&["export", "json"], &start_dir).await?;
    let values = match exported {
        Value::Object(values) => values,
        _ => bail!("direnv did not return an environment object"),
    };
    let mut child_env = Vec::with_capacity(values.len());
    for (key, value) in values {
        if key.starts_with("DIRENV_")
            || key.starts_with("HORON_")
            || matches!(key.as_str(), "BASH_ENV" | "ENV")
            || std::env::var_os(key.as_str()).is_some()
        {
            continue;
        }
        // The value is moved out of the parsed document rather than copied.
        if let Value::String(value) = value {
            child_env.push((key, value));
        }
    }
    Ok(child_env)
}

async fn direnv_json(args: &[&str], directory: &Path) -> Result<Value> {
    let mut command = Command::new("direnv");
    command
        .args(args)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // Ignore an inherited direnv diff so the child reads the live approval of this workspace.
    for name in [
        "DIRENV_DIFF",
        "DIRENV_DIR",
        "DIRENV_FILE",
        "DIRENV_WATCHES",
        "DIRENV_REQUIRED",
    ] {
        command.env_remove(name);
    }
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .context("direnv timed out while preparing the shell environment")?
        .context("could not run system direnv; install direnv on PATH or disable [tools] bash_direnv")?;
    if !output.status.success() {
        bail!(
            "direnv failed to prepare the shell environment. Run `direnv status` in the workspace and review its approval"
        );
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| anyhow::anyhow!("direnv returned invalid JSON; install direnv 2.38 or later for status --json"))
}

#[cfg(test)]
mod tests;
