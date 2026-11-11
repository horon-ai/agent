//! Telling a client what a turn's request lost to provider schema constraints.
//!
//! The receipts reach the stored assistant message in the same append that
//! stores it (`Session::take_degradations`), so the record carries them at
//! rest without a second write. This only forwards them to an attached client.
//!
//! Nothing here can fail a turn. A message without receipts sends nothing, so
//! the ordinary path is one lookup of the message just appended.

use crate::protocol::ServerEvent;

/// Emit one additive event for the receipts stored on `message_id`, when the
/// message has any.
pub(crate) fn announce(
    session: &crate::session::Session,
    event_tx: &tokio::sync::mpsc::UnboundedSender<ServerEvent>,
    message_id: &str,
) {
    let Some(receipts) = session.degradations_of(message_id) else {
        return;
    };
    // A closed channel means nobody is watching the stream; the stored record
    // already carries the loss, so the send result has no further use.
    drop(event_tx.send(ServerEvent::DegradationReceipts {
        id: message_id.to_string(),
        receipts,
    }));
}
