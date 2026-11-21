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
use crate::clipboard::{self, Prepared};
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
                if let ApiEvent::SessionRenamed {
                    session_id,
                    display_title,
                    ..
                } = *event
                {
                    if app.session_id.as_deref() == Some(session_id.as_str()) {
                        app.session_title = Some(display_title);
                        app.transcript.push_notice("session title updated".to_owned());
                    } else {
                        changed = previously_changed;
                    }
                    continue;
                }
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
            Incoming::Todos {
                session_id,
                items,
                revision,
            } => {
                changed = previously_changed;
                if app.session_id.as_deref().is_none_or(|current| current == session_id) {
                    if app.set_todos(&session_id, items, revision) {
                        changed = Redraw::Immediate;
                    }
                }
            }
            Incoming::TodoEdit {
                session_id,
                applied,
                refusal,
                items,
                revision,
                id,
                status,
            } => {
                if app.todo_edit_reply(&session_id, applied, refusal, items, revision, &id, &status) {
                    changed = Redraw::Immediate;
                }
            }
            Incoming::Models { models, current } => app.show_models(models, current),
            Incoming::Usage(snapshot) => app.usage(snapshot),
            Incoming::RuntimeInfo {
                provider,
                model,
                effort,
                level,
            } => app.runtime_info(provider, model, effort, level),
            Incoming::DaemonSettings(settings) => app.show_daemon_settings(settings),
            Incoming::Sessions(rows) => app.show_sessions(rows),
            Incoming::SessionsChecked { gone, unverified } => app.sessions_checked(&gone, unverified),
            Incoming::Skills(rows) => app.show_skills(rows),
            // The answer names the text it settles, so a steer the client no
            // longer holds is dropped rather than restored into another turn.
            Incoming::Steered { text } => app.steered(&text),
            Incoming::SteerFailed { text, message } => app.steer_failed(&text, &message),
            Incoming::Attached { session, working_dir } => {
                // Read before the attach so the notice names what happened: the
                // same answer covers a resumed session and one just created.
                let started = app.take_pending_new_session();
                app.attach_session(*session, started);
                // The directory travels with the answer: the session's own
                // folder becomes the completion root, and an answer that names
                // none turns path completion off rather than keeping the
                // folder the client was launched in.
                app.apply_session_directory(working_dir.as_deref());
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
        Action::TodoStatus(request) => Command::SetTodoStatus {
            session_id: request.session_id,
            expected_revision: request.revision,
            id: request.id,
            status: request.status,
        },
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
        Action::Rename(title) => Command::Rename(title),
        Action::CheckSessions(sessions) => Command::CheckSessions(sessions),
        // The user opened the list, so a failure is theirs to see and the
        // request announces itself.
        Action::Skills => Command::Skills { announce: true },
        // The frame loop answers these two: the editor needs the terminal that
        // loop is drawing on, so the action never reaches the bridge thread.
        Action::EditDraft | Action::OpenArtifact(_) => return,
        Action::DaemonSettings => Command::DaemonSettings,
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

/// Hand the draft to the user's editor, then take the draft back.
///
/// The program is resolved before the screen is given up: a client with no
/// `VISUAL` or `EDITOR` has nothing to hand the terminal to, and must not blink
/// it to say so. A missing program, a failed exit, a file that cannot be read,
/// a path the editor swapped for a link or text that is not UTF-8 all leave the
/// draft on screen exactly as it was, because the alternative is losing what the
/// user typed to a program that never saved it.
///
/// Nothing is sent when the text comes back: the draft is where an edit ends,
/// and the key that sends it is still the user's to press.
///
/// Returns the sentence that ends the session when the screen did not come back.
/// A terminal that refused raw mode or the alternate screen cannot be drawn on
/// or read from, so the loop stops rather than painting escapes over the shell;
/// a refused paste or title is a warning and the client keeps running.
pub(crate) fn hand_over_draft<B>(
    terminal: &mut ratatui::Terminal<B>,
    app: &mut App,
    title: &mut crate::title::WindowTitle,
    mouse_captured: &mut bool,
) -> Option<String>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
{
    let program = match crate::editor::Program::from_environment() {
        Ok(program) => program,
        Err(problem) => {
            app.warn(format!("editor: {problem}"));
            return None;
        }
    };
    let draft = app.input.text();
    let handed = with_terminal(terminal, app, title, mouse_captured, || {
        crate::editor::edit(&draft, &program)
    });
    let outcome = match handed {
        Ok(outcome) => outcome,
        // The edit is not applied, because there is no frame left to show it
        // in; the caller restores what it can and prints this sentence on the
        // shell's screen.
        Err(error) => return Some(format!("cannot take the terminal back after the editor: {error}")),
    };
    match outcome {
        Ok(text) => {
            app.history.reset();
            app.input.set_text(&text);
            app.sync_slash();
        }
        Err(problem) => app.warn(format!("editor: {problem}")),
    }
    None
}

/// Open a retained artifact as a private copy in the user's editor.
///
/// The path arrived resolved, so a missing artifact or an address outside this
/// session never reaches this point and the screen is never given up for one.
/// The program is resolved before the hand-over for the same reason the draft
/// editor does it: a client with no `VISUAL` or `EDITOR` has nothing to hand the
/// terminal to and must not blink it to say so.
///
/// Nothing is read back: an edit lands in the copy, which is removed with the
/// editor's directory, and the retained artifact is only ever read. Returns the
/// sentence that ends the session when the screen did not come back, exactly as
/// [`hand_over_draft`] does.
pub(crate) fn hand_over_artifact<B>(
    terminal: &mut ratatui::Terminal<B>,
    app: &mut App,
    title: &mut crate::title::WindowTitle,
    mouse_captured: &mut bool,
    path: &std::path::Path,
) -> Option<String>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
{
    let program = match crate::editor::Program::from_environment() {
        Ok(program) => program,
        Err(problem) => {
            app.warn(format!("editor: {problem}"));
            return None;
        }
    };
    let source = path.to_path_buf();
    let handed = with_terminal(terminal, app, title, mouse_captured, || {
        crate::editor::open_copy(&source, &program)
    });
    let outcome = match handed {
        Ok(outcome) => outcome,
        Err(error) => return Some(format!("cannot take the terminal back after the editor: {error}")),
    };
    match outcome {
        Ok(()) => {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            app.transcript.push_notice(format!(
                "artifact: {name} opened as a private copy; the retained file is unchanged"
            ));
        }
        Err(problem) => app.warn(format!("editor: {problem}")),
    }
    None
}

/// Give the terminal to the editor for one run, then take it back.
///
/// `run` is the editor itself. Its result is handed back only when the screen
/// came back: without a screen there is no frame to apply a change in and no key
/// to read, so the caller ends the session with the returned sentence instead.
/// A refused paste or title is a warning, exactly as at startup, and the client
/// keeps running.
///
/// The take-back failure is the [`std::io::Result`] `take_back` produced, passed
/// on unchanged; the caller writes the sentence because only it knows what to do
/// with the edit that was already made.
fn with_terminal<B, T>(
    terminal: &mut ratatui::Terminal<B>,
    app: &mut App,
    title: &mut crate::title::WindowTitle,
    mouse_captured: &mut bool,
    run: impl FnOnce() -> T,
) -> std::io::Result<T>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
{
    crate::terminal::hand_over();
    let outcome = run();
    let resumed = crate::terminal::take_back(terminal);
    // The hand-over released the wheel; the frame loop captures it again at the
    // top of the next pass when the setting asks for it.
    *mouse_captured = false;
    // Stop before writing any more terminal state if restoration failed.
    resumed?;
    // A refusal is a warning, not a reason to stop, exactly as at startup: the
    // paste then arrives as ordinary keystrokes the editor already handles.
    if let Err(error) = crate::paste::enable() {
        app.warn(format!("paste: {error}"));
    }
    if title.enabled() {
        title.forget_label();
        let state_supported = title.state_supported();
        if let Err(error) = title.show(&crate::window::label(app, state_supported)) {
            app.warn(format!("title: {error}"));
        }
    }
    Ok(outcome)
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

/// Write the copy the app queued, if any, and say what became of it.
///
/// The terminal is the only clipboard this client has, so the terminal is asked
/// first: one that cannot take the sequence gets a sentence naming why, and the
/// text stays on screen for the terminal's own selection instead of a copy that
/// silently never arrives. A copy that does leave is reported with what it
/// carried, because the cap and the control-character filter can both change it.
pub(crate) fn flush_copy(app: &mut App, out: &mut impl std::io::Write, env: impl Fn(&str) -> Option<String>) -> bool {
    let Some(request) = app.take_copy() else {
        return false;
    };
    if let Err(refusal) = clipboard::support(&env) {
        app.warn(format!(
            "copy: {}; {} stays on screen to select with the terminal",
            refusal.reason(),
            request.label
        ));
        return true;
    }
    let prepared = clipboard::prepare(&request.text);
    let Some(sequence) = clipboard::sequence(&prepared) else {
        app.warn(format!("copy: {} holds nothing to copy", request.label));
        return true;
    };
    match clipboard::write(out, &sequence) {
        Ok(()) => app.transcript.push_notice(copy_notice(&request.label, &prepared)),
        Err(error) => app.warn(format!("copy: the terminal did not take the sequence: {error}")),
    }
    true
}

/// The line the transcript keeps after a copy left the client.
fn copy_notice(label: &str, prepared: &Prepared) -> String {
    let characters = prepared.text.chars().count();
    let mut line = format!("asked the terminal to copy {label} ({characters} characters");
    if prepared.truncated {
        line.push_str(&format!(", cut from {} bytes", prepared.total));
    }
    if prepared.dropped > 0 {
        line.push_str(&format!(", {} control characters left out", prepared.dropped));
    }
    line.push(')');
    line
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
