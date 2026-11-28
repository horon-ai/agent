//! Which items changed their rows in place, for the scroll measurement.
//!
//! The measurement keeps every older item's height, so a card that grew after
//! a newer item arrived (a result, a preview, a parallel call finishing, a
//! request restated anywhere in the list) has to be named here, or the counted
//! height comes up short and the view cannot reach the first messages.

use super::Transcript;

impl Transcript {
    /// Note that the item at `index` may have changed its rows.
    pub(super) fn mark_changed(&mut self, index: usize) {
        self.changed_from = Some(self.changed_from.map_or(index, |first| first.min(index)));
    }

    /// The first item that changed in place since the last call, for the scroll
    /// measurement to count again from.
    pub fn take_changed_from(&mut self) -> Option<usize> {
        self.changed_from.take()
    }
}
