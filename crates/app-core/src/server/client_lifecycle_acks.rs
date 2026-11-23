//! Approval and failover acknowledgement handlers extracted from `client_lifecycle`.
use super::*;

fn send_ack_event(client_event_tx: &mpsc::UnboundedSender<ServerEvent>, event: ServerEvent) {
    if client_event_tx.send(event).is_err() {
        crate::logging::warn("Failed to deliver ack event to disconnected client");
    }
}

pub(super) fn handle_approval_ack(id: u64, request_id: String, client_event_tx: &mpsc::UnboundedSender<ServerEvent>) {
    let cleared = crate::ambient::BlockedApprovalStore::open_default()
        .and_then(|store| store.update_with(&request_id, |record| record.pending_event = None));
    match cleared {
        Ok(record_found) => {
            crate::logging::event_info(
                "server_approval_ack",
                vec![
                    ("id", id.to_string()),
                    ("request_id", request_id),
                    ("record_found", record_found.to_string()),
                ],
            );
            send_ack_event(client_event_tx, ServerEvent::Ack { id });
        }
        Err(error) => {
            crate::logging::warn(&format!(
                "approval ack for request {request_id} could not be applied: {error}"
            ));
            send_ack_event(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message: format!("approval ack could not be applied: {error}"),
                    retry_after_secs: None,
                },
            );
        }
    }
}

pub(super) fn handle_failover_ack(
    id: u64,
    session_id: &str,
    chain_id: String,
    from: String,
    to: String,
    attempt: u32,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let cleared = super::state::try_clear_acked_failover_event(session_id, &chain_id, &from, &to, attempt);
    match cleared {
        Ok(removed) => {
            crate::logging::event_info(
                "server_failover_ack",
                vec![
                    ("id", id.to_string()),
                    ("session_id", session_id.to_string()),
                    ("chain_id", chain_id),
                    ("from", from),
                    ("to", to),
                    ("attempt", attempt.to_string()),
                    ("removed", removed.to_string()),
                ],
            );
            send_ack_event(client_event_tx, ServerEvent::Ack { id });
        }
        Err(error) => {
            crate::logging::warn(&format!(
                "failover ack for chain {chain_id} attempt {attempt} could not be applied: {error}"
            ));
            send_ack_event(
                client_event_tx,
                ServerEvent::Error {
                    id,
                    message: format!("failover ack could not be applied: {error}"),
                    retry_after_secs: None,
                },
            );
        }
    }
}
