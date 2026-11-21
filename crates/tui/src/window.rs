//! The name the terminal window carries for the app's state.
//!
//! The session's own title wins and the working directory names a session the
//! daemon never titled, so a stack of terminals says which one belongs to which
//! session and a fresh window still says where it is. With `title.state` on,
//! the turn state follows the name as a fixed word, so a window seen from
//! another one says whether its session is busy without being switched to.
//!
//! Writing the name is [`crate::title`]'s job; this module only decides what it
//! says, from the state the app already holds.

use crate::app::App;
use crate::title;

/// The name the window carries.
///
/// `state_supported` is the terminal's answer about its own title handling,
/// asked once by the caller: a terminal that keeps no title stack keeps the
/// plain name, exactly as it did before `title.state` existed.
pub(crate) fn label(app: &App, state_supported: bool) -> String {
    let name = title::label(app.session_title.as_deref(), app.cwd.as_deref());
    title::with_state(name, state(app, state_supported))
}

/// The state the title names, or none when the setting is off or the terminal
/// keeps no title worth adding to.
///
/// The frame loop compares this rather than the name: it is one fixed word, so
/// a repeated state writes nothing and the name is built only when it changes.
pub(crate) fn state(app: &App, state_supported: bool) -> Option<title::RunState> {
    if !state_supported || !app.title_state {
        return None;
    }
    Some(title::RunState::of(
        app.prompt.is_some(),
        app.pending_turn,
        app.session_status.as_deref(),
    ))
}
