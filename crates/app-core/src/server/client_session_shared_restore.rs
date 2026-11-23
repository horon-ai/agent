use super::super::rename_plan_participant;
use super::{
    ClientConnectionInfo, SessionInterruptQueues, SwarmEvent, SwarmMember, SwarmMemberRename, SwarmState,
    broadcast_swarm_status, persist_swarm_state_for, register_background_tool_signal, register_session_interrupt_queue,
    remove_plan_participant, unregister_session_event_sender, update_member_status,
};
use crate::agent::Agent;
use crate::protocol::ServerEvent;
use crate::provider::Provider;
use crate::server::VersionedPlan;
use crate::tool::Registry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

/// Resources updated when detaching one connection from a still-shared session
/// onto a cold-restored session without touching the previous session's state.
pub(super) struct SharedSourcePublish<'a> {
    pub old_session_id: &'a str,
    pub session_id: &'a str,
    pub client_connection_id: &'a str,
    pub agent: &'a Arc<Mutex<Agent>>,
    pub shutdown_signals: &'a Arc<RwLock<HashMap<String, horon_agent_agent_runtime::InterruptSignal>>>,
    pub soft_interrupt_queues: &'a SessionInterruptQueues,
    pub swarm_members: &'a Arc<RwLock<HashMap<String, SwarmMember>>>,
    pub swarms_by_id: &'a Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub swarm_plans: &'a Arc<RwLock<HashMap<String, VersionedPlan>>>,
    pub swarm_coordinators: &'a Arc<RwLock<HashMap<String, String>>>,
    pub client_event_tx: &'a mpsc::UnboundedSender<ServerEvent>,
    pub event_history: &'a Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    pub event_counter: &'a Arc<std::sync::atomic::AtomicU64>,
    pub swarm_event_tx: &'a broadcast::Sender<SwarmEvent>,
}

/// Whether another live connection or unattended headless member still uses `old_session_id`.
pub(super) async fn source_is_shared(
    old_session_id: &str,
    target_session_id: &str,
    client_connection_id: &str,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) -> bool {
    if old_session_id == target_session_id {
        return false;
    }
    let another_client = client_connections
        .read()
        .await
        .values()
        .any(|info| info.client_id != client_connection_id && info.session_id == old_session_id);
    if another_client {
        return true;
    }
    swarm_members
        .read()
        .await
        .get(old_session_id)
        .is_some_and(|member| member.is_headless)
}

/// Build an isolated provisional `Agent` so cold restore never mutates a shared live session.
pub(super) fn fresh_restore_agent(
    provider: &Arc<dyn Provider>,
    registry: &Registry,
    working_dir_override: Option<&str>,
) -> Arc<Mutex<Agent>> {
    let mut restored =
        Agent::new_provisional_with_initial_working_dir(provider.fork(), registry.clone(), working_dir_override);
    restored.set_memory_enabled(crate::config::config().features.memory);
    restored.set_persistent_runtime(true);
    Arc::new(Mutex::new(restored))
}

/// Detach this connection from a shared source session and register the restored target cleanly.
pub(super) async fn publish_shared_source_restore(ctx: SharedSourcePublish<'_>) {
    unregister_session_event_sender(ctx.swarm_members, ctx.old_session_id, ctx.client_connection_id).await;

    let (old_swarm_enabled, target_has_member) = {
        let members = ctx.swarm_members.read().await;
        (
            members.get(ctx.old_session_id).map(|member| member.swarm_enabled),
            members.contains_key(ctx.session_id),
        )
    };

    let (shutdown_signal, bg_signal, soft_queue) = {
        let guard = ctx.agent.lock().await;
        (
            guard.graceful_shutdown_signal(),
            guard.background_tool_signal(),
            guard.soft_interrupt_queue(),
        )
    };
    {
        let mut signals = ctx.shutdown_signals.write().await;
        signals.insert(ctx.session_id.to_string(), shutdown_signal);
    }
    register_background_tool_signal(ctx.session_id, bg_signal);
    register_session_interrupt_queue(ctx.soft_interrupt_queues, ctx.session_id, soft_queue).await;

    if let Some(swarm_enabled) = old_swarm_enabled
        && !target_has_member
    {
        super::ensure_client_swarm_member(
            ctx.session_id,
            ctx.client_connection_id,
            &None,
            ctx.client_event_tx,
            ctx.agent,
            swarm_enabled,
            ctx.swarm_members,
            ctx.swarms_by_id,
            ctx.event_history,
            ctx.event_counter,
            ctx.swarm_event_tx,
        )
        .await;
    }

    update_member_status(
        ctx.session_id,
        "ready",
        None,
        ctx.swarm_members,
        ctx.swarms_by_id,
        Some(ctx.event_history),
        Some(ctx.event_counter),
        Some(ctx.swarm_event_tx),
    )
    .await;

    let target_swarm_id = {
        let members = ctx.swarm_members.read().await;
        members.get(ctx.session_id).and_then(|member| member.swarm_id.clone())
    };
    if let Some(swarm_id) = target_swarm_id {
        let swarm_state = SwarmState {
            members: Arc::clone(ctx.swarm_members),
            swarms_by_id: Arc::clone(ctx.swarms_by_id),
            plans: Arc::clone(ctx.swarm_plans),
            coordinators: Arc::clone(ctx.swarm_coordinators),
        };
        persist_swarm_state_for(&swarm_id, &swarm_state).await;
    }
}

/// Update coordinators, plans, and swarm snapshots after moving `old_session_id` onto `session_id`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_moved_source_swarm(
    old_session_id: &str,
    session_id: &str,
    rename: SwarmMemberRename,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
) {
    let abandoned_swarm_id = rename
        .old_swarm_id
        .clone()
        .filter(|old| rename.new_swarm_id.as_ref() != Some(old));
    {
        let mut coordinators = swarm_coordinators.write().await;
        // The temporary connection's own swarm is abandoned on resume.
        // Renaming its coordinator entry would make the resumed session
        // coordinate a swarm it never joined (issue #1650).
        if let Some(abandoned) = abandoned_swarm_id.as_ref()
            && coordinators.get(abandoned).map(String::as_str) == Some(old_session_id)
        {
            coordinators.remove(abandoned);
        }
        for coordinator in coordinators.values_mut() {
            if *coordinator == old_session_id {
                *coordinator = session_id.to_string();
            }
        }
    }
    if let Some(healed) = rename.healed_swarm_id.as_ref() {
        remove_plan_participant(healed, session_id, swarm_plans).await;
        let mut coordinators = swarm_coordinators.write().await;
        if coordinators.get(healed).map(String::as_str) == Some(session_id) {
            coordinators.remove(healed);
        }
    }
    if let Some(abandoned) = abandoned_swarm_id.as_ref() {
        remove_plan_participant(abandoned, old_session_id, swarm_plans).await;
    }
    for stale_swarm_id in abandoned_swarm_id
        .iter()
        .chain(rename.healed_swarm_id.iter())
        .filter(|stale| rename.new_swarm_id.as_ref() != Some(*stale))
    {
        let swarm_state = SwarmState {
            members: Arc::clone(swarm_members),
            swarms_by_id: Arc::clone(swarms_by_id),
            plans: Arc::clone(swarm_plans),
            coordinators: Arc::clone(swarm_coordinators),
        };
        persist_swarm_state_for(stale_swarm_id, &swarm_state).await;
        broadcast_swarm_status(stale_swarm_id, swarm_members, swarms_by_id).await;
    }
    update_member_status(
        session_id,
        "ready",
        None,
        swarm_members,
        swarms_by_id,
        Some(event_history),
        Some(event_counter),
        Some(swarm_event_tx),
    )
    .await;
    let target_swarm_id = {
        let members = swarm_members.read().await;
        members.get(session_id).and_then(|member| member.swarm_id.clone())
    };
    if let Some(swarm_id) = target_swarm_id {
        rename_plan_participant(&swarm_id, old_session_id, session_id, swarm_plans).await;
        let swarm_state = SwarmState {
            members: Arc::clone(swarm_members),
            swarms_by_id: Arc::clone(swarms_by_id),
            plans: Arc::clone(swarm_plans),
            coordinators: Arc::clone(swarm_coordinators),
        };
        persist_swarm_state_for(&swarm_id, &swarm_state).await;
    }
}
