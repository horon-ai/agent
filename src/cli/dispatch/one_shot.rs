use anyhow::Result;

use super::{Args, commands, provider_init, register_on_demand_server_spawner_for_run};
use crate::agent::Agent;
use crate::cli::args::RunModelOverrides;

pub(super) async fn run(
    args: &Args,
    message: &str,
    emit_json: bool,
    emit_ndjson: bool,
    no_pty: bool,
    overrides: &RunModelOverrides,
) -> Result<()> {
    register_on_demand_server_spawner_for_run(args.provider.clone(), args.model.clone(), args.provider_profile.clone());
    let provider = provider_init::init_provider_for_run(&args.provider, args.model.as_deref()).await?;
    let registry = crate::tool::Registry::new(provider.clone()).await;
    if no_pty {
        registry.disable_bash_pty().await;
    }
    if commands::run_command_mcp_enabled() {
        // Register cached tool schemas before the first request. On a cold
        // cache, wait briefly for the first connection to advertise schemas.
        registry.register_mcp_tools(None, None, None).await;
        commands::wait_for_cold_cache_mcp_tools(&registry).await;
    }
    let mut agent = Agent::for_one_shot_run(provider.clone(), registry).closing_after_run();
    let setup = (|| {
        commands::restore_agent_session_if_requested(&mut agent, args.resume.as_deref())?;
        apply_model_overrides(&mut agent, overrides)
    })();
    if let Err(error) = setup {
        agent.mark_closed();
        return Err(error);
    }
    // User-authored `run` message; later helper turns are synthetic.
    agent.arm_effort_keywords(message);
    commands::run_single_message_with_agent(&mut agent, provider, message, emit_json, emit_ndjson).await
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
