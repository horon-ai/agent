//! Carrying a turn's degradation receipts on the message the request produced.
//!
//! A provider narrows the tool catalog while it builds a request and leaves the
//! receipts in [`horon_agent_provider_core::degradation`], keyed by session. The
//! assistant message appended for that request takes them as it is built, so
//! the receipts reach the record in the same append that stores the message:
//! no stored message is rewritten and no extra write happens.

use crate::message::Role;
use horon_agent_session_types::DegradationReceipt;

impl super::Session {
    /// The receipts this session's last request left, for an assistant message
    /// about to be appended; `None` for every other role and for a request that
    /// degraded nothing, so those records keep their previous bytes.
    pub(super) fn take_degradations(&self, role: &Role) -> Option<Vec<DegradationReceipt>> {
        if *role != Role::Assistant {
            return None;
        }
        let receipts = horon_agent_provider_core::degradation::take_pending(&self.id);
        (!receipts.is_empty()).then_some(receipts)
    }

    /// The receipts stored on `message_id`, for the turn loop to forward to an
    /// attached client. Searches from the newest message, which is where the
    /// turn loop's just-appended message is.
    pub fn degradations_of(&self, message_id: &str) -> Option<Vec<DegradationReceipt>> {
        self.messages
            .iter()
            .rev()
            .find(|message| message.id == message_id)
            .and_then(|message| message.degradations.clone())
    }
}
