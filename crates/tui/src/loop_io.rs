//! What the frame loop hands over between frames, and when the next frame is due.
//!
//! Deliveries from the bridge become app state, a keystroke's action goes out to
//! the bridge thread, and the terminal's mouse capture follows the setting that
//! asks for it. None of it draws, which is what keeps it out of the renderer and
//! testable on its own.
//!
//! The last piece is [`Frames`]: the one thing that decides whether a frame is
//! due, so an idle screen stays untouched and a streaming one animates at the
//! cadence the motion settings allow.

use std::time::{Duration, Instant};

use crate::app::{Action, App};
use crate::scroll;
use crate::sdk_bridge::{Command, Incoming};

/// How long the loop waits for a keystroke before looking at the screen again.
///
/// This is a bound on input latency, not a frame rate: a key waits for it, so it
/// stays short whatever the motion settings say.
pub(crate) const POLL: Duration = Duration::from_millis(50);

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
        let previously_changed = changed;
        changed = true;
        match delivery {
            Incoming::Event(event) => app.apply_event(&event),
            Incoming::Notice(message) => app.warn(message),
            Incoming::SendFailed { text, message, lost } => app.send_failed(&text, &message, lost),
            Incoming::Info(message) => app.transcript.push_notice(message),
            Incoming::Context(report) => app.transcript.push_context(*report),
            Incoming::Report(report) => app.transcript.push_report(*report),
            Incoming::Todos { session_id, items } => {
                changed = previously_changed;
                if app.session_id.as_deref().is_none_or(|current| current == session_id) {
                    let panel = crate::todo_panel::TodoPanel::from_items(&items);
                    if app.todos != panel {
                        app.todos = panel;
                        changed = true;
                    }
                }
            }
            Incoming::Models { models, current } => app.show_models(models, current),
            Incoming::RuntimeInfo {
                provider,
                model,
                effort,
                level,
            } => app.runtime_info(provider, model, effort, level),
            Incoming::Sessions(rows) => app.show_sessions(rows),
            Incoming::Skills(rows) => app.show_skills(rows),
            // The answer names the text it settles, so a steer the client no
            // longer holds is dropped rather than restored into another turn.
            Incoming::Steered { text } => app.steered(&text),
            Incoming::SteerFailed { text, message } => app.steer_failed(&text, &message),
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
        Action::Steer(text) => Command::Steer(text),
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

/// When a frame is due, and which animation frame it draws.
///
/// Nothing here draws. The loop asks one question per pass — [`Frames::due`] —
/// and the answer covers the whole screen: a turn in flight is the only state
/// that animates, so it is the only one that can be due without an event.
/// An idle app is never due, which is what keeps a quiet screen from being
/// redrawn at all.
pub(crate) struct Frames {
    /// The interval between frames while a turn runs, or `None` for a preset
    /// that adds no frame of its own.
    interval: Option<Duration>,
    /// A frame asked for by something other than the cadence: the first frame,
    /// a resized terminal, a reloaded settings file.
    pending: bool,
    /// When the last frame was drawn.
    last: Instant,
    /// Which animation frame the next drawn frame uses.
    frame: usize,
}

impl Frames {
    /// A scheduler whose first pass draws.
    pub fn new(motion: crate::settings::Motion) -> Self {
        Self {
            interval: motion.frame_interval(),
            pending: true,
            last: Instant::now(),
            frame: 0,
        }
    }

    /// Adopt a new motion preset without losing the frame already due.
    ///
    /// A changed cadence counts as a reason to draw once, so the spinner stops
    /// and restarts where the new preset says rather than at the next event.
    pub fn retune(&mut self, motion: crate::settings::Motion) {
        let interval = motion.frame_interval();
        if interval != self.interval {
            self.interval = interval;
            self.pending = true;
        }
    }

    /// Ask for a frame at the next pass, whatever the cadence says.
    pub fn request(&mut self) {
        self.pending = true;
    }

    /// The pass count taken when a frame is drawn, so the caller knows which
    /// spinner frame it used.
    ///
    /// `animate` is whether a turn is in flight: `false` makes every pass idle,
    /// and an idle pass never draws without an event, whatever the interval.
    pub fn due(&mut self, animate: bool) -> Option<usize> {
        let due = if self.pending {
            true
        } else if animate {
            match self.interval {
                Some(interval) => self.last.elapsed() >= interval,
                None => false,
            }
        } else {
            false
        };
        if !due {
            return None;
        }
        self.pending = false;
        self.last = Instant::now();
        let frame = self.frame;
        self.frame = self.frame.wrapping_add(1);
        Some(frame)
    }
}
