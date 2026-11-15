//! Pending plan proposals, one per proposing session and swarm.
//!
//! A proposal is not shared context: it is coordinator-addressed control state
//! with a single consumer (approve or reject), it never rides the shared-context
//! fan-out, and it must never appear in `swarm read` listings or in the
//! `swarm:context` debug surface. Keeping it in its own store makes the
//! distinction structural instead of a reserved key prefix.

use crate::plan::PlanItem;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// One pending proposal: the items the proposer wants merged, plus who sent it.
#[derive(Clone, Debug)]
pub struct PendingPlanProposal {
    pub proposer_session: String,
    pub items: Vec<PlanItem>,
    pub from_name: Option<String>,
    pub created_at: Instant,
}

/// Swarm id -> proposer session id -> pending proposal. A session has at most
/// one outstanding proposal per swarm; re-proposing replaces it.
pub type PlanProposalStore = Arc<RwLock<HashMap<String, HashMap<String, PendingPlanProposal>>>>;

/// Insert or replace the proposal `proposer_session` has pending in `swarm_id`.
/// `created_at` is preserved across a re-proposal, as the shared-context entry
/// it replaces did.
pub async fn store_plan_proposal(
    store: &PlanProposalStore,
    swarm_id: &str,
    proposer_session: &str,
    items: Vec<PlanItem>,
    from_name: Option<String>,
) {
    let mut proposals = store.write().await;
    let swarm_proposals = proposals.entry(swarm_id.to_string()).or_default();
    let created_at = swarm_proposals
        .get(proposer_session)
        .map(|proposal| proposal.created_at)
        .unwrap_or_else(Instant::now);
    swarm_proposals.insert(
        proposer_session.to_string(),
        PendingPlanProposal {
            proposer_session: proposer_session.to_string(),
            items,
            from_name,
            created_at,
        },
    );
}

/// Peek at a pending proposal without consuming it.
pub async fn peek_plan_proposal(
    store: &PlanProposalStore,
    swarm_id: &str,
    proposer_session: &str,
) -> Option<PendingPlanProposal> {
    let proposals = store.read().await;
    proposals
        .get(swarm_id)
        .and_then(|swarm_proposals| swarm_proposals.get(proposer_session))
        .cloned()
}

/// Remove the pending proposal so exactly one approve or reject consumes it.
/// Returns the removed proposal, or `None` when nothing was pending.
pub async fn take_plan_proposal(
    store: &PlanProposalStore,
    swarm_id: &str,
    proposer_session: &str,
) -> Option<PendingPlanProposal> {
    let mut proposals = store.write().await;
    let swarm_proposals = proposals.get_mut(swarm_id)?;
    let taken = swarm_proposals.remove(proposer_session);
    if swarm_proposals.is_empty() {
        proposals.remove(swarm_id);
    }
    taken
}

/// Snapshot every pending proposal for one swarm, ordered by proposer session id
/// so listings stay deterministic.
pub async fn pending_plan_proposals_for_swarm(store: &PlanProposalStore, swarm_id: &str) -> Vec<PendingPlanProposal> {
    let proposals = store.read().await;
    let mut pending: Vec<PendingPlanProposal> = match proposals.get(swarm_id) {
        Some(swarm_proposals) => swarm_proposals.values().cloned().collect(),
        None => Vec::new(),
    };
    pending.sort_by(|left, right| left.proposer_session.cmp(&right.proposer_session));
    pending
}

/// Snapshot every pending proposal in every swarm, ordered by swarm id then
/// proposer session id so listings stay deterministic.
pub async fn all_pending_plan_proposals(store: &PlanProposalStore) -> Vec<(String, PendingPlanProposal)> {
    let proposals = store.read().await;
    let mut out: Vec<(String, PendingPlanProposal)> = proposals
        .iter()
        .flat_map(|(swarm_id, swarm_proposals)| {
            swarm_proposals
                .values()
                .map(move |proposal| (swarm_id.clone(), proposal.clone()))
        })
        .collect();
    out.sort_by(|(left_swarm, left), (right_swarm, right)| {
        left_swarm
            .cmp(right_swarm)
            .then_with(|| left.proposer_session.cmp(&right.proposer_session))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> PlanItem {
        PlanItem {
            content: format!("task {id}"),
            status: "pending".to_string(),
            priority: "medium".to_string(),
            id: id.to_string(),
            subsystem: None,
            file_scope: Vec::new(),
            blocked_by: Vec::new(),
            assigned_to: None,
        }
    }

    fn store() -> PlanProposalStore {
        Arc::new(RwLock::new(HashMap::new()))
    }

    #[tokio::test]
    async fn take_consumes_exactly_once() {
        let store = store();
        store_plan_proposal(&store, "swarm-a", "session-1", vec![item("x")], None).await;

        let taken = take_plan_proposal(&store, "swarm-a", "session-1").await;
        assert_eq!(taken.map(|proposal| proposal.items.len()), Some(1));
        assert!(
            take_plan_proposal(&store, "swarm-a", "session-1").await.is_none(),
            "a proposal must not be approvable twice"
        );
        assert!(
            all_pending_plan_proposals(&store).await.is_empty(),
            "an empty swarm entry must not be left behind"
        );
    }

    #[tokio::test]
    async fn peek_does_not_consume() {
        let store = store();
        store_plan_proposal(&store, "swarm-a", "session-1", vec![item("x")], None).await;

        assert!(peek_plan_proposal(&store, "swarm-a", "session-1").await.is_some());
        assert!(peek_plan_proposal(&store, "swarm-a", "session-1").await.is_some());
        assert_eq!(pending_plan_proposals_for_swarm(&store, "swarm-a").await.len(), 1);
    }

    #[tokio::test]
    async fn reproposal_replaces_and_keeps_created_at() {
        let store = store();
        store_plan_proposal(&store, "swarm-a", "session-1", vec![item("x")], None).await;
        let first = peek_plan_proposal(&store, "swarm-a", "session-1").await.unwrap();

        store_plan_proposal(&store, "swarm-a", "session-1", vec![item("y"), item("z")], None).await;
        let second = peek_plan_proposal(&store, "swarm-a", "session-1").await.unwrap();

        assert_eq!(second.items.len(), 2);
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(pending_plan_proposals_for_swarm(&store, "swarm-a").await.len(), 1);
    }

    #[tokio::test]
    async fn proposals_are_scoped_per_swarm_and_ordered() {
        let store = store();
        store_plan_proposal(&store, "swarm-b", "session-2", vec![item("b")], None).await;
        store_plan_proposal(&store, "swarm-a", "session-9", vec![item("9")], None).await;
        store_plan_proposal(&store, "swarm-a", "session-3", vec![item("3")], None).await;

        assert!(peek_plan_proposal(&store, "swarm-a", "session-2").await.is_none());
        assert!(take_plan_proposal(&store, "swarm-b", "session-3").await.is_none());

        let order: Vec<(String, String)> = all_pending_plan_proposals(&store)
            .await
            .into_iter()
            .map(|(swarm_id, proposal)| (swarm_id, proposal.proposer_session))
            .collect();
        assert_eq!(
            order,
            vec![
                ("swarm-a".to_string(), "session-3".to_string()),
                ("swarm-a".to_string(), "session-9".to_string()),
                ("swarm-b".to_string(), "session-2".to_string()),
            ]
        );
    }
}
