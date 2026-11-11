//! Remappable keys: action ids, key names, and the file that rebinds them.
//!
//! Every keystroke the interface acts on belongs to one action id here, and
//! every action has a default key that reproduces the behaviour the interface
//! shipped with. `$HORON_HOME/keybindings.toml` replaces an action's keys one
//! entry at a time, `[]` turns an action off, and [`load::Loader`] re-reads the
//! file on the settings cadence so an edit takes effect without a restart.
//!
//! Plain character insertion and the digits that select a question option are
//! not actions: a text field that could lose the letter `j` to a binding is not
//! a text field. Everything else — submitting, cancelling, leaving, editing,
//! and answering a prompt — goes through [`Bindings::resolve`].
//!
//! A key name is normalised before it is compared, so `Shift+Ctrl+Return`,
//! `ctrl+enter` and the terminal's own `KeyEvent` for Ctrl-Enter are one key.
//! See [`key`] for the spelling and [`bindings`] for the conflict rules.

use std::sync::LazyLock;

pub mod bindings;
pub mod key;
pub mod load;

pub use bindings::{ACTIONS, ActionSpec, Bindings, Scope, ids};
pub use key::{Code, Key};
pub use load::{Loader, Reload};

/// The default map, built once.
///
/// A prompt answers keystrokes on its own so its tests can drive it without an
/// [`App`](crate::app::App); this is the map it uses when no caller supplied a
/// loaded one.
pub(crate) fn default_bindings() -> &'static Bindings {
    static DEFAULT: LazyLock<Bindings> = LazyLock::new(Bindings::default);
    &DEFAULT
}

#[cfg(test)]
mod tests;
