//! Address resolution and visibility for the IRC bus, scoped to one daemon's
//! live swarm members.

use super::super::SwarmMember;
use super::super::swarm::swarm_is_self_or_ancestor;
use futures::future::BoxFuture;
use horon_agent_irc_core::{Directory, Endpoint};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Resolves IRC addresses against the daemon's live swarm member map.
pub(super) struct SwarmDirectory {
    members: Arc<RwLock<HashMap<String, SwarmMember>>>,
}

impl SwarmDirectory {
    pub(super) fn new(members: Arc<RwLock<HashMap<String, SwarmMember>>>) -> Self {
        Self { members }
    }
}

fn endpoint(member: &SwarmMember) -> Endpoint {
    Endpoint {
        id: Arc::from(member.session_id.as_str()),
        handle: member.friendly_name.as_deref().map(Arc::from),
        parent: member.report_back_to_session_id.as_deref().map(Arc::from),
        status: Arc::from(member.status.as_str()),
    }
}

fn terminal(status: &str) -> bool {
    matches!(status, "stopped" | "failed" | "completed")
}

/// Whether two members may message each other: they share a swarm id, or one
/// of them was spawned with a report-back owner of the other.
fn visible(from: &SwarmMember, to: &SwarmMember) -> bool {
    let same_swarm = matches!(
        (from.swarm_id.as_deref(), to.swarm_id.as_deref()),
        (Some(left), Some(right)) if left == right
    );
    same_swarm
        || from.report_back_to_session_id.as_deref() == Some(to.session_id.as_str())
        || to.report_back_to_session_id.as_deref() == Some(from.session_id.as_str())
}

impl Directory for SwarmDirectory {
    fn resolve<'a>(&'a self, sender: &'a str, target: &'a str) -> BoxFuture<'a, Result<Endpoint, String>> {
        Box::pin(async move {
            let members = self.members.read().await;
            let from = members
                .get(sender)
                .ok_or_else(|| format!("Unknown agent \"{sender}\" - use irc action=list"))?;
            let to = match members.get(target) {
                Some(member) => member,
                None => {
                    let mut candidates = members
                        .values()
                        .filter(|member| member.friendly_name.as_deref() == Some(target));
                    let first = candidates
                        .next()
                        .ok_or_else(|| format!("Unknown agent \"{target}\" - use irc action=list"))?;
                    if candidates.next().is_some() {
                        return Err(format!("handle \"{target}\" is ambiguous; use the session id"));
                    }
                    first
                }
            };
            if !visible(from, to) {
                return Err(format!("agent \"{target}\" is not visible to this session"));
            }
            Ok(endpoint(to))
        })
    }

    fn broadcast_targets<'a>(&'a self, sender: &'a str) -> BoxFuture<'a, Result<Vec<Endpoint>, String>> {
        Box::pin(async move {
            let members = self.members.read().await;
            let from = members
                .get(sender)
                .ok_or_else(|| format!("Unknown agent \"{sender}\" - use irc action=list"))?;
            if from.report_back_to_session_id.is_some() {
                return Err("only a root session can broadcast".to_string());
            }
            let mut targets: Vec<Endpoint> = members
                .values()
                .filter(|member| {
                    member.session_id != sender
                        && !terminal(&member.status)
                        && swarm_is_self_or_ancestor(&members, sender, &member.session_id)
                })
                .map(endpoint)
                .collect();
            targets.sort_by(|left, right| left.id.cmp(&right.id));
            Ok(targets)
        })
    }

    fn roster<'a>(&'a self, sender: &'a str) -> BoxFuture<'a, Vec<Endpoint>> {
        Box::pin(async move {
            let members = self.members.read().await;
            let Some(from) = members.get(sender) else {
                return Vec::new();
            };
            let mut peers: Vec<Endpoint> = members
                .values()
                .filter(|member| member.session_id != sender && visible(from, member))
                .map(endpoint)
                .collect();
            peers.sort_by(|left, right| left.id.cmp(&right.id));
            peers
        })
    }
}
