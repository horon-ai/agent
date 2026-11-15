#![cfg_attr(test, allow(clippy::await_holding_lock, clippy::items_after_test_module))]

use super::debug_ambient::maybe_handle_ambient_command;
use super::debug_command_exec::{DebugInterruptContext, execute_debug_command, resolve_debug_session};
use super::debug_events::{maybe_handle_event_query_command, maybe_handle_event_subscription_command};
use super::debug_help::{debug_help_text, parse_namespaced_command, swarm_debug_help_text};
use super::debug_jobs::{DebugJob, maybe_handle_job_command};
use super::debug_server_state::maybe_handle_server_state_command;
use super::debug_session_admin::maybe_handle_session_admin_command;
use super::debug_swarm_read::maybe_handle_swarm_read_command;
use super::debug_swarm_write::{DebugSwarmWriteContext, maybe_handle_swarm_write_command};
use super::{
    FileTouchService, PlanProposalStore, ServerIdentity, SwarmEvent, SwarmMember, VersionedPlan, debug_control_allowed,
};
use crate::agent::Agent;
use crate::ambient_runner::AmbientRunnerHandle;
use crate::protocol::{Request, ServerEvent, decode_request, encode_event};
use crate::provider::Provider;
use crate::transport::Stream;
use anyhow::Result;
use horon_agent_agent_runtime::InterruptSignal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

#[derive(Clone, Debug)]
pub(super) struct ClientConnectionInfo {
    pub(super) client_id: String,
    pub(super) session_id: String,
    pub(super) client_instance_id: Option<String>,
    pub(super) connected_at: Instant,
    pub(super) last_seen: Instant,
    pub(super) is_processing: bool,
    pub(super) current_tool_name: Option<String>,
    /// Terminal-identifying env vars captured from this client (tmux/zellij/
    /// kitty/DISPLAY/...). Used to route spawn/focus hooks to the client's
    /// terminal instead of the long-lived server's stale startup env (#405).
    pub(super) terminal_env: Vec<(String, String)>,
    pub(super) disconnect_tx: mpsc::UnboundedSender<()>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "debug client wiring fans out across sessions, swarms, files, jobs, and transport state"
)]
pub(super) async fn handle_debug_client(
    stream: Stream,
    sessions: Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>,
    is_processing: Arc<RwLock<bool>>,
    session_id: Arc<RwLock<String>>,
    provider: Arc<dyn Provider>,
    client_connections: Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    plan_proposals: PlanProposalStore,
    swarm_plans: Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: Arc<RwLock<HashMap<String, String>>>,
    file_touch: FileTouchService,
    debug_jobs: Arc<RwLock<HashMap<String, DebugJob>>>,
    event_history: Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: broadcast::Sender<SwarmEvent>,
    server_identity: ServerIdentity,
    server_start_time: std::time::Instant,
    ambient_runner: Option<AmbientRunnerHandle>,
    mcp_pool: Option<Arc<crate::mcp::SharedMcpPool>>,
    shutdown_signals: Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: super::SessionInterruptQueues,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }

        let request = match decode_request(&line) {
            Ok(r) => r,
            Err(e) => {
                let event = ServerEvent::Error {
                    id: 0,
                    message: format!("Invalid request: {}", e),
                    retry_after_secs: None,
                };
                let json = encode_event(&event);
                writer.write_all(json.as_bytes()).await?;
                continue;
            }
        };

        match request {
            Request::Ping { id } => {
                let event =
                    super::session_connections::connection_presence_pong(id, &client_connections, &swarm_members).await;
                let json = encode_event(&event);
                writer.write_all(json.as_bytes()).await?;
            }

            Request::GetState { id } => {
                let current_session_id = session_id.read().await.clone();
                let sessions = sessions.read().await;
                let message_count = sessions.len();

                let event = ServerEvent::State {
                    id,
                    session_id: current_session_id,
                    message_count,
                    is_processing: *is_processing.read().await,
                    pending_soft_interrupts: None,
                };
                let json = encode_event(&event);
                writer.write_all(json.as_bytes()).await?;
            }

            Request::DebugCommand {
                id,
                command,
                session_id: requested_session,
            } => {
                if !debug_control_allowed() {
                    let event = ServerEvent::Error {
                        id,
                        message: "Debug control is disabled. Set HORON_AGENT_DEBUG_CONTROL=1, enable display.debug_socket, or start the shared server from a self-dev session.".to_string(),
                        retry_after_secs: None,
                    };
                    let json = encode_event(&event);
                    writer.write_all(json.as_bytes()).await?;
                    continue;
                }

                // Parse the namespaced command. The retired `client` and
                // `tester` namespaces are refused; every other command runs
                // against the server.
                let cmd = match parse_namespaced_command(&command) {
                    Ok(cmd) => cmd,
                    Err(message) => {
                        let event = ServerEvent::Error {
                            id,
                            message,
                            retry_after_secs: None,
                        };
                        let json = encode_event(&event);
                        writer.write_all(json.as_bytes()).await?;
                        continue;
                    }
                };

                let result = {
                    // Server commands
                    if let Some(output) = maybe_handle_job_command(cmd, &debug_jobs).await? {
                        Ok(output)
                    } else if let Some(output) = maybe_handle_session_admin_command(
                        cmd,
                        &sessions,
                        &session_id,
                        &provider,
                        &swarm_members,
                        &swarms_by_id,
                        &swarm_coordinators,
                        &swarm_plans,
                        &event_history,
                        &event_counter,
                        &swarm_event_tx,
                        &soft_interrupt_queues,
                        mcp_pool.clone(),
                    )
                    .await?
                    {
                        Ok(output)
                    } else if let Some(output) = maybe_handle_server_state_command(
                        cmd,
                        &sessions,
                        &client_connections,
                        &swarm_members,
                        &server_identity,
                        server_start_time,
                        &swarms_by_id,
                        &swarm_plans,
                        &swarm_coordinators,
                        &file_touch,
                        &debug_jobs,
                        &event_history,
                        &shutdown_signals,
                        &soft_interrupt_queues,
                    )
                    .await?
                    {
                        Ok(output)
                    } else if let Some(output) = maybe_handle_swarm_read_command(
                        cmd,
                        &sessions,
                        &swarm_members,
                        &swarms_by_id,
                        &plan_proposals,
                        &swarm_plans,
                        &swarm_coordinators,
                        &file_touch,
                        &server_identity,
                    )
                    .await?
                    {
                        Ok(output)
                    } else if let Some(output) = maybe_handle_swarm_write_command(
                        cmd,
                        &DebugSwarmWriteContext {
                            session_id: &session_id,
                            swarm_members: &swarm_members,
                            swarms_by_id: &swarms_by_id,
                            plan_proposals: &plan_proposals,
                            swarm_plans: &swarm_plans,
                            swarm_coordinators: &swarm_coordinators,
                        },
                    )
                    .await?
                    {
                        Ok(output)
                    } else if let Some(output) = maybe_handle_ambient_command(cmd, &ambient_runner, &provider).await? {
                        Ok(output)
                    } else if maybe_handle_event_subscription_command(id, cmd, &swarm_event_tx, &mut writer).await? {
                        return Ok(());
                    } else if let Some(output) = maybe_handle_event_query_command(cmd, &event_history).await {
                        Ok(output)
                    } else if cmd == "swarm:help" {
                        Ok(swarm_debug_help_text())
                    } else if cmd == "help" {
                        Ok(debug_help_text())
                    } else {
                        match resolve_debug_session(&sessions, &session_id, requested_session).await {
                            Ok((_session, agent)) => {
                                execute_debug_command(
                                    agent,
                                    cmd,
                                    Arc::clone(&debug_jobs),
                                    Some(&server_identity),
                                    Some(DebugInterruptContext {
                                        session_id: _session,
                                        shutdown_signals: Arc::clone(&shutdown_signals),
                                        soft_interrupt_queues: Arc::clone(&soft_interrupt_queues),
                                    }),
                                )
                                .await
                            }
                            Err(e) => Err(e),
                        }
                    }
                };

                let (ok, output) = match result {
                    Ok(output) => (true, output),
                    Err(e) => (false, e.to_string()),
                };
                let event = ServerEvent::DebugResponse { id, ok, output };
                let json = encode_event(&event);
                writer.write_all(json.as_bytes()).await?;
            }

            _ => {
                // Debug socket only allows ping, state, and debug_command
                let event = ServerEvent::Error {
                    id: request.id(),
                    message: "Debug socket only allows ping, state, and debug_command".to_string(),
                    retry_after_secs: None,
                };
                let json = encode_event(&event);
                writer.write_all(json.as_bytes()).await?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "debug_tests.rs"]
mod debug_tests;
