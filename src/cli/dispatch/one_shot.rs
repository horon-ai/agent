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
        warn_cost,
        max_cost,
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
    if let (Some(warn), Some(max)) = (warn_cost, max_cost)
        && warn > max
    {
        anyhow::bail!("--warn-cost ({warn}) must not exceed --max-cost ({max})");
    }
    if warn_cost.is_some() || max_cost.is_some() {
        crate::spend_cap::install(crate::spend_cap::Limits {
            warn_usd: *warn_cost,
            max_usd: *max_cost,
        });
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
    let result =
        commands::run_single_message_with_agent(&mut agent, provider, message, emit_json, emit_ndjson, limits).await;
    if let Some(summary) = crate::spend_cap::summary() {
        super::output::stderr_info(format!(
            "Estimated spend about ${:.4} over {} priced request(s); {} request(s) had no known price and are not counted{}.",
            summary.spent_usd,
            summary.priced_requests,
            summary.unpriced_requests,
            if summary.stopped {
                "; the spend cap stopped further requests"
            } else {
                ""
            }
        ));
    }
    result
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
    if let Some(choice) = overrides.advisor.as_deref() {
        agent.set_advisor_enabled(choice == "on");
    }
    Ok(())
}
