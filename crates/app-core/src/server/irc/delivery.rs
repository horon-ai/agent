//! Handing an IRC message to a live session: wake an idle one, inject into a
//! busy one, and relay a wake turn's answer back to the sender.

use super::super::session_forget::{PendingRelays, register_pending_relays};
use super::super::{
    SessionAgents, SessionInterruptQueues, SwarmEvent, SwarmMember, fanout_session_event,
    live_turn::LiveTurnSwarmContext, live_turn::run_live_turn_if_idle_with_completion,
    queue_soft_interrupt_for_session,
};
use super::relay;
use crate::protocol::ServerEvent;
use futures::future::BoxFuture;
use horon_agent_agent_runtime::SoftInterruptSource;
use horon_agent_irc_core::{AgentId, Deliver, DeliverResult, Endpoint, IrcMessage, format_envelope};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{RwLock, broadcast};

/// Delivers IRC messages into this daemon's live sessions.
pub(super) struct SessionDeliver {
    members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    sessions: SessionAgents,
    queues: SessionInterruptQueues,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    event_history: Arc<RwLock<VecDeque<SwarmEvent>>>,
    event_counter: Arc<AtomicU64>,
    event_tx: broadcast::Sender<SwarmEvent>,
    /// Wake turns whose answer still has to be relayed, per woken session.
    pending_relays: PendingRelays,
}

impl SessionDeliver {
    pub(super) fn new(
        members: Arc<RwLock<HashMap<String, SwarmMember>>>,
        sessions: SessionAgents,
        queues: SessionInterruptQueues,
        swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
        event_history: Arc<RwLock<VecDeque<SwarmEvent>>>,
        event_counter: Arc<AtomicU64>,
        event_tx: broadcast::Sender<SwarmEvent>,
    ) -> Self {
        let pending_relays = Arc::new(StdMutex::new(HashMap::new()));
        register_pending_relays(&pending_relays);
        Self {
            members,
            sessions,
            queues,
            swarms_by_id,
            event_history,
            event_counter,
            event_tx,
            pending_relays,
        }
    }

    /// The counters shared with session removal; a test hook for observing the
    /// relay mark a wake leaves behind.
    #[cfg(test)]
    pub(super) fn pending_relays(&self) -> PendingRelays {
        Arc::clone(&self.pending_relays)
    }

    fn swarm_context(&self) -> LiveTurnSwarmContext {
        LiveTurnSwarmContext::new(
            &self.members,
            &self.swarms_by_id,
            &self.event_history,
            &self.event_counter,
            &self.event_tx,
        )
    }

    async fn has_session(&self, agent: &str) -> bool {
        self.sessions.read().await.contains_key(agent)
    }
}

fn lock_pending(pending: &StdMutex<HashMap<AgentId, usize>>) -> std::sync::MutexGuard<'_, HashMap<AgentId, usize>> {
    match pending.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl Deliver for SessionDeliver {
    fn deliver<'a>(
        &'a self,
        target: &'a Endpoint,
        message: &'a IrcMessage,
        from_parent: bool,
    ) -> BoxFuture<'a, DeliverResult> {
        Box::pin(async move {
            // The envelope's `handle` attribute names the sender, so resolve the
            // sender's friendly name from its member record. Copy it out before
            // awaiting anything else.
            let sender_handle = {
                let members = self.members.read().await;
                members
                    .get(message.from.as_ref())
                    .and_then(|member| member.friendly_name.clone())
            };
            let envelope = format_envelope(message, sender_handle.as_deref());
            let reminder = format!(
                "IRC message from {}. Reply with the irc tool: action=send, to=\"{}\", reply_to={}. No one replies on your behalf.",
                message.from, message.from, message.id
            );
            if crate::config::config().server.wake_mode == crate::config::WakeMode::External {
                if self.has_session(target.id.as_ref()).await {
                    fanout_session_event(
                        &self.members,
                        target.id.as_ref(),
                        ServerEvent::WakeRequested {
                            session_id: target.id.to_string(),
                            reason: "irc_delivery".to_string(),
                            notification: envelope,
                        },
                    )
                    .await;
                    return DeliverResult::Woken;
                }
            } else {
                let receiver = run_live_turn_if_idle_with_completion(
                    target.id.as_ref(),
                    &envelope,
                    Some(reminder),
                    &self.sessions,
                    self.swarm_context(),
                )
                .await;
                if let Some(receiver) = receiver {
                    if !message.broadcast && !message.progress && !message.wake_relay {
                        lock_pending(&self.pending_relays)
                            .entry(Arc::clone(&target.id))
                            .and_modify(|count| *count += 1)
                            .or_insert(1);
                        relay::spawn(receiver, message.clone(), Arc::clone(&self.pending_relays));
                    }
                    return DeliverResult::Woken;
                }
            }
            if !self.has_session(target.id.as_ref()).await {
                return DeliverResult::Unavailable(format!("agent \"{}\" has no live session", target.id));
            }
            let urgent = from_parent && !message.broadcast && !message.progress;
            let queued = queue_soft_interrupt_for_session(
                target.id.as_ref(),
                envelope,
                urgent,
                SoftInterruptSource::System,
                &self.queues,
                &self.sessions,
            )
            .await;
            if queued {
                DeliverResult::Injected
            } else {
                DeliverResult::Errored("soft interrupt queue rejected the message".to_string())
            }
        })
    }

    fn has_pending_input<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let queue = {
                let queues = self.queues.read().await;
                queues.get(agent).cloned()
            };
            let Some(queue) = queue else {
                return false;
            };
            match queue.lock() {
                Ok(pending) => !pending.is_empty(),
                Err(poisoned) => !poisoned.into_inner().is_empty(),
            }
        })
    }

    fn can_respond<'a>(&'a self, agent: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            {
                let members = self.members.read().await;
                match members.get(agent) {
                    Some(member) if matches!(member.status.as_str(), "running" | "spawned") => return true,
                    Some(_) => {}
                    None => return false,
                }
            }
            if self.has_pending_input(agent).await {
                return true;
            }
            lock_pending(&self.pending_relays)
                .get(agent)
                .is_some_and(|count| *count > 0)
        })
    }
}
