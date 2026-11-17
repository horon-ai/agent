//! What the frame loop hands over between frames.
//!
//! Deliveries from the bridge become app state, a keystroke's action goes out to
//! the bridge thread, and the terminal's mouse capture follows the setting that
//! asks for it. None of it draws, which is what keeps it out of the renderer and
//! testable on its own.

use crate::app::{Action, App};
use crate::scroll;
use crate::sdk_bridge::{Command, Incoming};

/// Move every buffered SDK delivery into the app state, reporting whether
/// anything arrived.
///
/// The report is what drives the redraw: an empty queue leaves the screen as it
/// is. A closed bridge is a real end of the session, not an empty queue: the
/// channel only reports it once, so treating it as "nothing yet" would leave the
/// interface showing a dead connection forever.
pub(crate) fn drain_events(app: &mut App) -> bool {
    let mut changed = false;
    loop {
        let delivery = match app.events.try_recv() {
            Ok(delivery) => delivery,
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // The first reason wins: a sentence about an unreachable
                // daemon is more use than "the bridge stopped", and the banner
                // is what the user reads, so it must not change under them.
                app.disconnect("the harness bridge stopped");
                changed = true;
                break;
            }
        };
        changed = true;
        match delivery {
            Incoming::Event(event) => app.apply_event(&event),
            Incoming::Notice(message) => app.warn(message),
            Incoming::SendFailed { text, message, lost } => app.send_failed(&text, &message, lost),
            Incoming::Info(message) => app.transcript.push_notice(message),
            Incoming::Models { models, current } => app.show_models(models, current),
            Incoming::RuntimeInfo {
                provider,
                model,
                effort,
                level,
            } => app.runtime_info(provider, model, effort, level),
            Incoming::Sessions(rows) => app.show_sessions(rows),
            Incoming::Skills(rows) => app.show_skills(rows),
            Incoming::Attached(session) => {
                // Read before the attach so the notice names what happened: the
                // same answer covers a resumed session and one just created.
                let started = app.take_pending_new_session();
                app.attach_session(*session, started);
            }
            Incoming::RequestFailed(message) => app.request_failed(&message),
            Incoming::Closed(message) => app.disconnect(&message),
        }
    }
    changed
}

/// Send one user action to the bridge thread.
pub(crate) fn dispatch(app: &mut App, commands: &std::sync::mpsc::Sender<Command>, action: Action) {
    let command = match action {
        Action::Send(text) => Command::Text(text),
        Action::Cancel => Command::Cancel,
        Action::Approval {
            request_id,
            approved,
            always,
        } => Command::Approval {
            request_id,
            approved,
            always,
        },
        Action::Ask { ask_id, answers } => Command::Ask { ask_id, answers },
        Action::Runtime { command, arguments } => Command::Runtime { command, arguments },
        Action::Model(name) => Command::Model(name),
        Action::Effort(level) => Command::Effort(level),
        Action::Models => Command::Models,
        // The directory is the one the client is working in, which is what
        // makes the list the sessions of this project rather than of the
        // daemon's whole history.
        Action::Sessions => Command::Sessions {
            working_dir: app.cwd.clone(),
        },
        // The new session starts where the client is working, so the list of
        // sessions it appears in is the one `/switch` shows.
        Action::NewSession => Command::NewSession {
            working_dir: app.cwd.clone(),
        },
        Action::Attach(session_id) => Command::Attach(session_id),
        // The user opened the list, so a failure is theirs to see and the
        // request announces itself.
        Action::Skills => Command::Skills { announce: true },
        Action::Quit => {
            app.running = false;
            return;
        }
    };
    if let Err(error) = commands.send(command) {
        // A request that never left the client has the same effect as one the
        // harness refused: whatever was waiting for its answer has to stop
        // waiting, and a list left on screen would swallow every key.
        app.request_failed(&format!("the harness bridge stopped: {error}"));
    }
}

/// Keep the terminal's mouse capture in step with `transcript.mouse_scroll`.
///
/// Capture takes the wheel and the terminal's own text selection together, which
/// is why it is a setting and why it is released the moment the setting goes off.
/// A terminal that refuses the sequence is not a reason to stop: the wheel then
/// does nothing and the pane still scrolls from the keyboard, so the refusal is
/// reported once and the setting is turned back off.
pub(crate) fn sync_mouse_capture(app: &mut App, captured: &mut bool) -> bool {
    if *captured == app.mouse_scroll {
        return false;
    }
    if app.mouse_scroll {
        match scroll::enable_mouse() {
            Ok(()) => *captured = true,
            Err(error) => {
                app.mouse_scroll = false;
                app.warn(format!("mouse: {error}"));
            }
        }
        return true;
    }
    if !scroll::disable_mouse() {
        app.warn("mouse: the terminal did not take the release sequence");
    }
    *captured = false;
    true
}
