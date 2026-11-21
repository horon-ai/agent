//! What the frame loop hands over between frames, and when the next frame is due.
//!
//! Deliveries from the bridge become app state, a keystroke's action goes out to
//! the bridge thread, and the terminal's mouse capture follows the setting that
//! asks for it. None of it draws, which is what keeps it out of the renderer and
//! testable on its own; the one exception is the frame the loop has already
//! decided is due, handed straight back to the client's `draw`.
//!
//! The last piece is [`Frames`]: the one thing that decides whether a frame is
//! due, so an idle screen stays untouched and a streaming one animates at the
//! cadence the motion settings allow.

use std::time::{Duration, Instant};

use horon_agent_sdk::api::ApiEvent;
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::{Action, App};
use crate::scroll;
use crate::sdk_bridge::{Command, Incoming};

/// How long the loop waits for a keystroke before looking at the screen again.
///
/// This is a bound on input latency, not a frame rate: a key waits for it, so it
/// stays short whatever the motion settings say.
pub(crate) const POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Redraw {
    None,
    Stream,
    Immediate,
}

/// Apply buffered events, keeping text-only updates on the stream cadence.
pub(crate) fn drain_events(app: &mut App) -> Redraw {
    let mut changed = Redraw::None;
    loop {
        let delivery = match app.events.try_recv() {
            Ok(delivery) => delivery,
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // The first reason wins: a sentence about an unreachable
                // daemon is more use than "the bridge stopped", and the banner
                // is what the user reads, so it must not change under them.
                app.disconnect("the harness bridge stopped");
                changed = Redraw::Immediate;
                break;
            }
        };
        let previously_changed = changed;
        changed = Redraw::Immediate;
        match delivery {
            Incoming::Event(event) => {
                if matches!(&*event, ApiEvent::TextDelta { .. } | ApiEvent::ReasoningDelta { .. })
                    && previously_changed != Redraw::Immediate
                {
                    changed = Redraw::Stream;
                }
                app.apply_event(&event);
            }
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
                        changed = Redraw::Immediate;
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

/// Draw the one frame this pass owes, if any.
///
/// A settings file, the settings screen and a launch flag all end in one
/// applied snapshot, so the cadence is adopted here and a changed preset takes
/// effect without waiting for the next event. A frame is due when something
/// changed, when the scheduler says the spinner is due, or on its first pass;
/// an idle screen reaches none of those and is left untouched.
pub(crate) fn draw_due<B>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    frames: &mut Frames,
    changed: bool,
) -> Result<(), String>
where
    B: Backend,
    B::Error: std::fmt::Display,
{
    frames.retune(app.motion);
    if changed {
        frames.request();
    }
    let Some(frame) = frames.due(app.animating(), Instant::now()) else {
        return Ok(());
    };
    app.motion_frame = frame;
    crate::draw(terminal, app).map_err(|error| error.to_string())
}

/// Schedule automatic frames without delaying input or advancing the spinner
/// on every redraw. The loop supplies one timestamp per scheduling pass.
pub(crate) struct Frames {
    motion: crate::settings::Motion,
    pending: bool,
    stream: bool,
    last_draw: Instant,
    last_spinner: Option<Instant>,
    frame: usize,
}

impl Frames {
    pub fn new(motion: crate::settings::Motion, now: Instant) -> Self {
        Self {
            motion,
            pending: true,
            stream: false,
            last_draw: now,
            last_spinner: None,
            frame: 0,
        }
    }

    /// Apply a new cadence immediately and restart the indicator.
    pub fn retune(&mut self, motion: crate::settings::Motion) {
        if self.motion != motion {
            self.motion = motion;
            self.last_spinner = None;
            self.frame = 0;
            self.request();
        }
    }

    pub fn request(&mut self) {
        self.pending = true;
    }

    pub fn request_stream(&mut self) {
        self.stream = true;
    }

    pub fn due(&mut self, animate: bool, now: Instant) -> Option<usize> {
        let mut spinner_due = false;
        if let Some(interval) = self.motion.frame_interval().filter(|_| animate) {
            let last = *self.last_spinner.get_or_insert(now);
            spinner_due = now.duration_since(last) >= interval;
        } else {
            self.last_spinner = None;
            self.frame = 0;
        }

        let automatic =
            (self.stream || spinner_due) && now.duration_since(self.last_draw) >= self.motion.stream_interval();
        if !self.pending && !automatic {
            return None;
        }
        if spinner_due {
            self.last_spinner = Some(now);
            self.frame = self.frame.wrapping_add(1);
        }
        self.pending = false;
        self.stream = false;
        self.last_draw = now;
        Some(self.frame)
    }
}
