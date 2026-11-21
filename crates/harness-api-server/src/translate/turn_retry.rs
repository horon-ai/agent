//! A `send_message` that continues a failed run.
//!
//! An ordinary message is acknowledged by the daemon when it is received, and its
//! turn is reported through the event stream: `message_accepted`, then deltas,
//! then `turn_done`. A continuation promises something narrower and later — that
//! a new run exists for the run it named — so the daemon holds its ack until it
//! has decided, and that ack is the request's whole answer:
//!
//! * a started continuation acks once its new run exists, which is `ok` routed to
//!   the request id through `pending_simple`;
//! * a refused one (stale, in flight, ineligible) answers with an error on the
//!   same id, before anything was stored or run.
//!
//! It must not take `pending_message_id`: that slot is what turns the ack into a
//! `message_accepted` stream event, and what makes the following `done` a turn
//! boundary. A continuation takes neither, because the caller was already
//! answered by the decision itself.

use serde_json::Value;

use super::*;

/// Whether `request` asks to continue a run rather than start unrelated work.
pub(super) fn continues_run(request: &Value) -> bool {
    request["expected_run_id"].as_str().is_some_and(|value| !value.is_empty())
}

/// Record what will answer this `send_message`.
pub(super) fn track_send(state: &mut BridgeState, request: &Value, id: u64, api_id: u64, no_reply: bool) {
    if no_reply {
        state.pending_no_reply_message_id = Some((id, api_id));
    } else if continues_run(request) {
        // Awaiting the daemon's decision, which arrives as its ack (`ok`) or as
        // an error on this same legacy id.
        state.pending_simple.push((id, api_id, SimpleKind::ContinueRun));
    } else {
        state.pending_message_id = Some(id);
    }
}

/// Carry the named run to the daemon, leaving the field off when none was named.
pub(super) fn forward_run_id(message: &mut Value, request: &Value) {
    if continues_run(request) {
        message["expected_run_id"] = request["expected_run_id"].clone();
    }
}
