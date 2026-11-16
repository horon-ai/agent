//! Bracketed paste: on while the interface runs, off on every way out.
//!
//! A terminal asked for this mode wraps a paste in escape sequences, so the
//! client receives one [`crossterm::event::Event::Paste`] instead of a burst of
//! keystrokes. Without it a pasted line break is indistinguishable from the
//! Enter the user pressed, and a three-line paste is sent as three messages.
//!
//! The mode outlives the process: a client that dies with it on leaves the
//! shell pasting escape codes into the next program. It is therefore turned off
//! on the normal exit, on the error path, and from the panic hook — `try_init`
//! installs a hook that restores the screen but knows nothing about this mode.

use std::io::Write;

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;

/// Ask the terminal to report a paste as one event.
///
/// A terminal that does not support the mode answers nothing, which is not a
/// reason to refuse to start: the editor still works, and a paste arrives as
/// ordinary keystrokes.
pub fn enable() -> Result<(), String> {
    let mut output = std::io::stdout();
    execute!(output, EnableBracketedPaste)
        .and_then(|()| output.flush())
        .map_err(|error| error.to_string())
}

/// Turn the mode off, and say whether the terminal took the sequence.
///
/// Best effort by design: it runs on the failure and panic paths, where the
/// terminal may already be gone, and a second error there would replace the one
/// the caller is trying to report. The answer is there for a caller that has
/// somewhere to put it; the exit path has nothing left to report to.
pub fn disable() -> bool {
    let mut output = std::io::stdout();
    let written = execute!(output, DisableBracketedPaste).is_ok();
    written && output.flush().is_ok()
}

/// Turn the mode off before a panic unwinds, then let the installed hook run.
///
/// Installed after `try_init`, so the hook it chains is the one that restores
/// the alternate screen.
pub fn guard_panics() {
    let restoring = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        disable();
        restoring(info);
    }));
}
