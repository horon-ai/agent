//! Disambiguated key reports: on while the interface runs, off on every way out.
//!
//! A terminal in its default mode sends Shift-Enter as the same byte as Enter,
//! so the `shift+enter` newline binding never fires and the draft is sent
//! instead. Terminals that speak the kitty keyboard protocol (kitty, Ghostty,
//! WezTerm, foot, iTerm2 with the protocol on, recent Alacritty) report a
//! modified Enter as its own sequence once asked to disambiguate, which is the
//! only flag the client pushes: it changes nothing about unmodified keys, and
//! it asks for no release events.
//!
//! The terminal is asked first whether it speaks the protocol, and only then
//! is the flag pushed. A terminal that does not answer keeps its default reports,
//! where `alt+enter` and `ctrl+j` remain the newline keys. Like bracketed
//! paste, the pushed flag outlives the process, so it is popped on the normal
//! exit, on the error path, from the panic hook and from the signal path.

use std::io::Write;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;

/// The terminal's answer to the support query, asked once per process on the
/// first [`enable`]: the terminal does not change under the client, and asking
/// again after the external editor returns would cost another round trip. A
/// terminal that cannot answer is treated as one that does not speak the
/// protocol, which leaves it in the mode it was in.
static SUPPORTED: LazyLock<bool> =
    LazyLock::new(|| crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false));

/// Whether the client pushed a flag set the terminal still holds.
static PUSHED: AtomicBool = AtomicBool::new(false);

/// Ask a terminal that speaks the kitty keyboard protocol to report modified
/// keys unambiguously.
///
/// Runs on the loop's own thread in raw mode while nothing else reads the
/// input, because the first call's support query reads the terminal's answer
/// from that same input.
pub fn enable() -> std::io::Result<()> {
    if PUSHED.load(Ordering::Acquire) || !*SUPPORTED {
        return Ok(());
    }
    let mut output = std::io::stdout();
    execute!(
        output,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .and_then(|()| output.flush())?;
    PUSHED.store(true, Ordering::Release);
    Ok(())
}

/// Pop the flag set [`enable`] pushed, and say whether the terminal took the
/// sequence; a client that pushed nothing has nothing to pop.
///
/// Best effort by design, like the paste mode: it runs on the failure, panic
/// and signal paths, where the terminal may already be gone.
pub fn disable() -> bool {
    if !PUSHED.swap(false, Ordering::AcqRel) {
        return true;
    }
    let mut output = std::io::stdout();
    let written = execute!(output, PopKeyboardEnhancementFlags).is_ok();
    written && output.flush().is_ok()
}

/// Pop the flag before a panic unwinds, then let the installed hook run.
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
