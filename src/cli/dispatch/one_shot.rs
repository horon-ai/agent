use anyhow::Result;

use super::{Args, commands, provider_init, register_on_demand_server_spawner_for_run};
use crate::agent::Agent;
use crate::cli::args::RunModelOverrides;

pub(super) async fn run(args: &Args) -> Result<()> {
    let Some(super::Command::Run {
        message,
        json: emit_json,
        ndjson: emit_ndjson,
        no_pty,
        no_session,
        model_overrides: overrides,
        max_time,
        wait_async,
    }) = &args.command
    else {
        anyhow::bail!("Expected a run command");
    };
    let (emit_json, emit_ndjson) = (*emit_json, *emit_ndjson);
    let limits = commands::RunLimits {
        max_time: *max_time,
        wait_async: *wait_async,
    };
    register_on_demand_server_spawner_for_run(args.provider.clone(), args.model.clone(), args.provider_profile.clone());
    let provider = provider_init::init_provider_for_run(&args.provider, args.model.as_deref()).await?;
    let registry = crate::tool::Registry::new(provider.clone()).await;
    if *no_pty {
        registry.disable_bash_pty().await;
    }
    if commands::run_command_mcp_enabled() {
        // Register cached tool schemas before the first request. On a cold
        // cache, wait briefly for the first connection to advertise schemas.
        registry.register_mcp_tools(None, None, None).await;
        commands::wait_for_cold_cache_mcp_tools(&registry).await;
    }
    if *no_session && (args.resume.is_some() || args.goal.is_some()) {
        anyhow::bail!(
            "--no-session cannot be combined with --resume, --continue or --goal, which need a stored session"
        );
    }
    let mut agent = if *no_session {
        Agent::for_memory_only_run(provider.clone(), registry)
    } else {
        Agent::for_one_shot_run(provider.clone(), registry)
    }
    .closing_after_run();
    let setup = (|| {
        commands::restore_agent_session_if_requested(&mut agent, args.resume.as_deref())?;
        apply_model_overrides(&mut agent, overrides)?;
        if let (Some(title), Some(budget)) = (&args.goal, args.goal_budget) {
            let goal = crate::goal::start_session_goal(
                agent.session_id(),
                agent.working_dir().map(std::path::Path::new),
                title.clone(),
                budget,
            )?;
            if !emit_json && !emit_ndjson {
                super::output::stderr_info(crate::goal::render_goal_detail(&goal));
            }
        }
        Ok(())
    })();
    if let Err(error) = setup {
        agent.mark_closed();
        return Err(error);
    }
    // User-authored `run` message; later helper turns are synthetic.
    agent.arm_effort_keywords(message);
    commands::run_single_message_with_agent(&mut agent, provider, message, emit_json, emit_ndjson, limits).await
}

fn apply_model_overrides(agent: &mut Agent, overrides: &RunModelOverrides) -> Result<()> {
    if let Some(tier) = overrides.service_tier.as_deref() {
        agent.provider_handle().set_service_tier(tier)?;
    }
    if let Some(effort) = overrides.thinking.as_deref() {
        if !agent.provider_handle().available_efforts().contains(&effort) {
            anyhow::bail!("unsupported --thinking value {effort:?} for the selected model");
        }
        agent.set_reasoning_effort(effort)?;
    }
    Ok(())
}
