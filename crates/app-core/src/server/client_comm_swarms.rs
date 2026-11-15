//! `comm_list_swarms` / `comm_set_swarm_label` request handlers.

use super::{
    ClientConnectionInfo, FileTouchService, SessionAgents, SwarmEvent, SwarmEventType, SwarmMember, record_swarm_event,
};
use crate::protocol::{AgentInfo, ServerEvent};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, mpsc};

pub(super) async fn handle_comm_list_swarms(
    id: u64,
    req_session_id: String,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
) {
    let swarms =
        super::swarm_labels::list_swarms(&req_session_id, swarm_members, swarms_by_id, Some(swarm_coordinators)).await;
    let _ = client_event_tx.send(ServerEvent::CommSwarms { id, swarms });
}

#[expect(
    clippy::too_many_arguments,
    reason = "label changes are recorded in swarm event history and answered with the directory"
)]
pub(super) async fn handle_comm_set_swarm_label(
    id: u64,
    req_session_id: String,
    label: String,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
) {
    let (swarm_id, friendly_name) = {
        let members = swarm_members.read().await;
        match members.get(&req_session_id) {
            Some(member) => (member.swarm_id.clone(), member.friendly_name.clone()),
            None => (None, None),
        }
    };
    let Some(swarm_id) = swarm_id else {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: "Not in a swarm, so there is no swarm to label.".to_string(),
            retry_after_secs: None,
        });
        return;
    };
    let known: HashSet<String> = swarms_by_id.read().await.keys().cloned().collect();
    match super::swarm_labels::set_swarm_label(&swarm_id, &label, &known) {
        Ok(applied) => {
            record_swarm_event(
                event_history,
                event_counter,
                swarm_event_tx,
                req_session_id.clone(),
                friendly_name,
                Some(swarm_id.clone()),
                SwarmEventType::Notification {
                    notification_type: "swarm_label".to_string(),
                    message: match applied {
                        Some(label) => format!("Swarm labeled '{label}'"),
                        None => "Swarm label cleared".to_string(),
                    },
                },
            )
            .await;
            handle_comm_list_swarms(
                id,
                req_session_id,
                client_event_tx,
                swarm_members,
                swarms_by_id,
                swarm_coordinators,
            )
            .await;
        }
        Err(err) => {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: err.to_string(),
                retry_after_secs: None,
            });
        }
    }
}

/// `comm_list`: every member of the requester's swarm with live runtime
/// extras (activity, provider, token and todo counters).
async fn swarm_id_for_session(
    session_id: &str,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) -> Option<String> {
    let members = swarm_members.read().await;
    members.get(session_id).and_then(|m| m.swarm_id.clone())
}

#[expect(
    clippy::too_many_arguments,
    reason = "member listing joins swarm state, file touches, agents, and connections"
)]
pub(super) async fn handle_comm_list(
    id: u64,
    req_session_id: String,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    file_touch: &FileTouchService,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
) {
    let swarm_id = swarm_id_for_session(&req_session_id, swarm_members).await;

    if let Some(swarm_id) = swarm_id {
        let swarm_session_ids: Vec<String> = {
            let swarms = swarms_by_id.read().await;
            swarms
                .get(&swarm_id)
                .map(|sessions| sessions.iter().cloned().collect())
                .unwrap_or_default()
        };

        // Snapshot the static member fields first, releasing the members lock
        // before gathering per-session runtime extras (which briefly lock
        // individual agents and read the connection map).
        struct MemberStatic {
            session_id: String,
            friendly_name: Option<String>,
            files: Vec<String>,
            status: String,
            detail: Option<String>,
            task_label: Option<String>,
            role: String,
            is_headless: bool,
            report_back_to_session_id: Option<String>,
            latest_completion_report: Option<String>,
            live_attachments: usize,
            status_age_secs: u64,
        }

        let statics: Vec<MemberStatic> = {
            let members = swarm_members.read().await;
            let touches = file_touch.reverse_snapshot().await;
            swarm_session_ids
                .iter()
                .filter_map(|sid| {
                    members.get(sid).map(|member| {
                        let mut files: Vec<String> = touches
                            .get(sid)
                            .into_iter()
                            .flat_map(|paths| paths.iter())
                            .map(|path| path.display().to_string())
                            .collect();
                        files.sort();
                        MemberStatic {
                            session_id: sid.clone(),
                            friendly_name: member.friendly_name.clone(),
                            files,
                            status: member.status.clone(),
                            detail: member.detail.clone(),
                            task_label: member.task_label.clone(),
                            role: member.role.clone(),
                            is_headless: member.is_headless,
                            report_back_to_session_id: member.report_back_to_session_id.clone(),
                            latest_completion_report: member.latest_completion_report.clone(),
                            live_attachments: member.event_txs.len(),
                            status_age_secs: member.last_status_change.elapsed().as_secs(),
                        }
                    })
                })
                .collect()
        };

        let mut member_list: Vec<AgentInfo> = Vec::with_capacity(statics.len());
        for m in statics {
            let extras = super::comm_sync::member_runtime_extras(
                &m.session_id,
                m.status == "running",
                sessions,
                client_connections,
            )
            .await;

            let stalled_secs = super::swarm_watchdog::stalled_for_secs(&m.session_id);
            member_list.push(AgentInfo {
                session_id: m.session_id,
                friendly_name: m.friendly_name,
                files_touched: m.files,
                status: Some(m.status),
                detail: m.detail,
                task_label: m.task_label,
                role: Some(m.role),
                is_headless: Some(m.is_headless),
                report_back_to_session_id: m.report_back_to_session_id,
                latest_completion_report: m.latest_completion_report,
                live_attachments: Some(m.live_attachments),
                status_age_secs: Some(m.status_age_secs),
                last_activity_age_secs: extras.last_activity_age_secs,
                stalled_secs,
                activity: extras.activity,
                provider_name: extras.provider_name,
                provider_model: extras.provider_model,
                provider_effort: extras.provider_effort,
                turn_count: extras.turn_count,
                recent_total_tokens: extras.recent_total_tokens,
                recent_output_tokens: extras.recent_output_tokens,
                recent_window_secs: extras.recent_window_secs,
                cumulative_total_tokens: extras.cumulative_total_tokens,
                todos_completed: extras.todos_completed,
                todos_total: extras.todos_total,
            });
        }

        let _ = client_event_tx.send(ServerEvent::CommMembers {
            id,
            members: member_list,
        });
    } else {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: "Not in a swarm. Use a git repository to enable swarm features.".to_string(),
            retry_after_secs: None,
        });
    }
}
