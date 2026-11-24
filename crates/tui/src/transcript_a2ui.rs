//! Transcript mutation helpers for read-only A2UI fallback cards.

use serde_json::Value;

use super::{Item, Transcript, next_items_generation};
use crate::a2ui::A2uiCard;

impl Transcript {
    /// Append a parsed read-only A2UI card from stored conversation history.
    pub(crate) fn push_a2ui_card(&mut self, card: A2uiCard) {
        self.finish_stream();
        self.push_item(Item::A2ui(Box::new(card)), None);
    }

    /// Insert or update an in-place live A2UI fallback card for `tool_call_id`.
    /// Returns `true` when an existing card was updated in place.
    pub(crate) fn apply_a2ui_envelope(
        &mut self,
        tool_call_id: &str,
        sequence: u32,
        envelope: Value,
        fallback_text: Option<String>,
    ) -> bool {
        if !tool_call_id.is_empty()
            && let Some(Item::A2ui(card)) = self
                .items
                .iter_mut()
                .rev()
                .find(|item| matches!(item, Item::A2ui(card) if card.call_id == tool_call_id))
        {
            card.apply_envelope(sequence, envelope, fallback_text);
            self.items_generation = next_items_generation();
            return true;
        }
        self.finish_stream();
        let card = A2uiCard::from_envelope(tool_call_id, sequence, envelope, fallback_text);
        self.push_item(Item::A2ui(Box::new(card)), None);
        false
    }
}
