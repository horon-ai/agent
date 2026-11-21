//! `/pin` and `/unpin`: the sessions the reader keeps at the top of the list.
//!
//! The state is a client-local file of session ids, one list per endpoint, and
//! the rows of the last session-list answer. Keeping the rows here lets a pin
//! taken from the open list be reordered without asking the harness again, and
//! a pin taken by command find the label the list would show.
//!
//! Nothing here is sent to the harness. A pin that cannot be written is not
//! taken: the notice and the order both wait for the file, so the screen never
//! claims a pin the next launch would not read.

use super::{Action, App};
use crate::sdk_bridge::SessionRow;
use crate::session_pins::PinStore;

/// The client's pinned sessions and the rows the last list was built from.
pub(crate) struct SessionPins {
    /// The file-backed store, scoped to the endpoint the client connected to.
    pub(crate) store: PinStore,
    /// The rows of the last session-list answer, newest first.
    ///
    /// Held so a pin change can rebuild the list's order and labels without a
    /// second request; a later answer replaces them whole.
    pub(crate) rows: Vec<SessionRow>,
}

impl Default for SessionPins {
    fn default() -> Self {
        Self {
            store: PinStore::memory(),
            rows: Vec::new(),
        }
    }
}

impl App {
    /// Read the pins file once, reporting anything the reader should know.
    pub(crate) fn load_pins(&mut self) {
        for warning in self.pins.store.ensure_loaded() {
            self.warn(format!("pins: {warning}"));
        }
    }

    /// `/pin` and `/unpin`: the session named, or the one attached.
    pub(crate) fn run_pin(&mut self, arguments: &str, pinned: bool) -> Option<Action> {
        self.load_pins();
        let argument = arguments.trim();
        let id = if argument.is_empty() {
            match self.session_id.clone() {
                Some(id) => id,
                None => {
                    self.warn(format!(
                        "pins: no session is attached; name one with /{} <id>",
                        if pinned { "pin" } else { "unpin" }
                    ));
                    return None;
                }
            }
        } else {
            argument.to_string()
        };
        self.set_session_pin(&id, pinned);
        None
    }

    /// Pin or unpin the selected row of the open session list.
    pub(crate) fn toggle_session_pin(&mut self, session: &str) {
        self.load_pins();
        let pinned = !self.pins.store.is_pinned(session);
        self.set_session_pin(session, pinned);
    }

    /// Store one pin and reorder the open list when the file took it.
    fn set_session_pin(&mut self, session: &str, pinned: bool) {
        let label = self.session_label(session);
        match self.pins.store.set_pinned(session, pinned) {
            Ok(true) => {
                self.transcript.push_notice(if pinned {
                    format!("pinned {label}")
                } else {
                    format!("unpinned {label}")
                });
                // The list moves under the reader; the row they acted on stays
                // selected even when the filter hides it, so a second press
                // undoes the first.
                self.refresh_session_picker(session);
            }
            Ok(false) => self.transcript.push_notice(if pinned {
                format!("{label} is already pinned")
            } else {
                format!("{label} is not pinned")
            }),
            // Nothing on screen changes: no notice, no order, no marker. The
            // warning names the file problem, which is the part the reader can
            // act on.
            Err(fault) => self.warn(format!("pins: {fault}; the list and the pin are unchanged")),
        }
    }

    /// What the session list calls this session, or its id when it has no row.
    fn session_label(&self, session: &str) -> String {
        self.pins
            .rows
            .iter()
            .find(|row| row.id == session)
            .map_or_else(|| session.to_string(), |row| row.label.clone())
    }
}

#[cfg(test)]
#[path = "pins_tests.rs"]
mod tests;
