//! The policy an executable package command passes before its child runs.
//!
//! A slash command is not a message: a template expands into text the user
//! could have typed, while an executable command starts a process. The two
//! therefore never share a dispatch. This module is the one place that asks the
//! same questions a tool call asks, in the same order, so a client cannot reach
//! a package child by submitting a command instead of a tool call:
//!
//! 1. plan mode refuses a process effect;
//! 2. a name the session disabled is refused;
//! 3. the approval mode decides whether a user must answer, through the editor
//!    bridge when one is serving the session and through the safety queue
//!    otherwise;
//! 4. the `pre_tool` gate reviews the exact payload the child receives, after
//!    secret placeholders the session knows are restored.
//!
//! The tool allow-list is deliberately not consulted: it names the tools a
//! model may call, and a `/id:command` line is the user's own request. The
//! disabled list is consulted because it is an explicit denial, and the name it
//! carries is the same `ext__<package>__<command>` a package tool of that name
//! would use.

use anyhow::{Result, bail};
use serde_json::Value;

use super::{
    ApprovalPromptRef, Registry, ToolContext, ToolEffects, approval_override, permission_bridge,
    run_gated_pre_tool_hook, sdk, session_tool_policy,
};

/// The registry-shaped name one executable command's policy is decided under.
///
/// It names the same surface a package tool would, so an operator who disabled
/// `ext__<package>__<command>` disables the command too.
pub(crate) fn policy_name(package_id: &str, command: &str) -> String {
    format!("ext__{package_id}__{command}")
}

/// Refuse an executable command this session's policy forbids, and return the
/// payload the `pre_tool` gate reviewed.
///
/// The returned value is what [`crate::extensions::invoke::call_command`] must
/// receive: placeholders are restored before the gate sees them, so the gate
/// and the child review one value instead of two spellings of it.
pub(crate) async fn authorize(session_id: &str, name: &str, input: Value, ctx: &ToolContext) -> Result<Value> {
    if !crate::plan_mode::allows_effects(session_id, ToolEffects::process()) {
        bail!(crate::plan_mode::block_message(name));
    }
    if command_disabled(session_id, name) {
        bail!("The package command '{name}' is disabled for this session.");
    }
    if let Some(refusal) = approval_refusal(session_id, name, &input, ctx).await {
        bail!(refusal);
    }
    let working_dir = ctx.working_dir.as_ref().map(|dir| dir.display().to_string());
    if !crate::hooks::hook_configured("pre_tool") {
        return Ok(crate::secrets::restore_tool_input(input, session_id));
    }
    let gate_session = session_id.to_string();
    let (input, decision) = crate::secrets::restore_then_gate(input, session_id, move |payload| async move {
        run_gated_pre_tool_hook(&gate_session, working_dir.as_deref(), name, &payload).await
    })
    .await;
    if let crate::hooks::GateDecision::Block { reason } = decision {
        bail!("Package command blocked by pre_tool hook: {reason}");
    }
    Ok(input)
}

/// Whether this session's explicit denials name the command.
fn command_disabled(session_id: &str, name: &str) -> bool {
    if let Some(config) = sdk::config(session_id)
        && config.disabled.iter().any(|existing| existing == name)
    {
        return true;
    }
    session_tool_policy(session_id).is_some_and(|policy| policy.disabled_tools.contains(name))
}

/// The approval decision for one command, or the refusal the user must read.
///
/// The editor bridge answers inline when one is serving the session, exactly as
/// it does for a tool call; otherwise the queue opens a request the existing
/// approval surface resolves, and the command is refused until it is answered.
async fn approval_refusal(session_id: &str, name: &str, input: &Value, ctx: &ToolContext) -> Option<String> {
    let mode = approval_override::get(session_id).unwrap_or(crate::config::config().tools.approval_mode);
    let dual_confirm = crate::config::config().tools.dual_confirm_classes.clone();
    let danger = crate::approval::danger_class(name, input, ctx.working_dir.as_deref());
    let evaluation = crate::approval::evaluate_and_audit(
        crate::approval::ApprovalCall {
            tool: name,
            effects: ToolEffects::process(),
            mode,
            dual_confirm: &dual_confirm,
            danger,
            bash_pattern: None,
            paths: &[],
            restricted: None,
        },
        &ctx.tool_call_id,
    );
    let crate::approval::ApprovalEvaluation::RequiresApproval {
        reason,
        is_dual_confirm,
    } = evaluation
    else {
        return None;
    };
    let prompt = ApprovalPromptRef {
        resolved_name: name,
        reason: &reason,
        is_dual_confirm,
        input,
    };
    match permission_bridge::for_session(session_id) {
        Some(bridge) => permission_bridge::await_permission(&bridge, &prompt, ToolEffects::process(), ctx).await,
        None => Registry::approval_refusal(name, &reason, is_dual_confirm, input, ToolEffects::process(), ctx),
    }
}
