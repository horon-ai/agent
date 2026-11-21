//! A terminal client for the horon-agent harness.
//!
//! `horon-tui` is a separate package and binary. It talks to the daemon only
//! through `horon-agent-sdk`, so the harness never links a terminal library and
//! a TUI bug cannot take down the server.
//!
//! The crate keeps its logic testable: [`app::App`] turns SDK events and
//! keystrokes into state, and rendering is a plain function over any
//! `ratatui` backend, so the tests draw into a `TestBackend` instead of a
//! terminal. [`run`] is the only entry point that touches the real terminal.

pub mod app;
pub mod commands;
pub mod context;
mod fuzzy;
pub mod gallery;
pub mod help;
pub mod history;
pub mod input;
mod item_lines;
pub mod keys;
mod links;
mod loop_io;
mod markdown;
pub mod overlays;
pub mod paste;
mod path_completion;
pub mod prompts;
pub mod report;
pub mod scroll;
pub mod sdk_bridge;
pub mod settings;
pub mod statusline;
mod suggestions;
mod terminal;
pub mod theme;
mod title;
mod todo_panel;
mod tool_expand;
pub mod tool_line;
pub mod transcript;
mod visual_blocks;
mod wrap;

#[cfg(test)]
#[path = "streaming_tests.rs"]
mod streaming_tests;

#[cfg(test)]
#[path = "tool_line_tests.rs"]
mod tool_line_tests;

#[cfg(test)]
#[path = "tool_expand_tests.rs"]
mod tool_expand_tests;

#[cfg(test)]
#[path = "title_tests.rs"]
mod title_tests;

#[cfg(test)]
#[path = "turn_state_tests.rs"]
mod turn_state_tests;

#[cfg(test)]
#[path = "prompt_queue_tests.rs"]
mod prompt_queue_tests;

#[cfg(test)]
#[path = "input_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "app_settings_tests.rs"]
mod app_settings_tests;

#[cfg(test)]
#[path = "scroll_tests.rs"]
mod scroll_tests;

#[cfg(test)]
#[path = "context_tests.rs"]
mod context_tests;

#[cfg(test)]
#[path = "report_tests.rs"]
mod report_tests;

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod markdown_tests;

#[cfg(test)]
#[path = "help_tests.rs"]
mod help_tests;

#[cfg(test)]
#[path = "fuzzy_tests.rs"]
mod fuzzy_tests;

#[cfg(test)]
#[path = "path_completion_tests.rs"]
mod path_completion_tests;

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::sdk_bridge::{Command, Runtime};
use crate::terminal::{show_failure, window_label, window_state};
use crate::theme::changes::{self, ChangePalette};
use crate::theme::{Theme, ThemeLoader};
// The test modules reach this through `use super::*`; production code draws
// through `item_lines`, which takes the glyph set from the app itself.
#[cfg(test)]
use crate::theme::Symbols;
use crate::transcript::Item;

pub use help::USAGE;

/// Parsed command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    /// Socket to connect to instead of the resolved default.
    pub socket: Option<std::path::PathBuf>,
    /// Existing session to attach to instead of creating one.
    pub session: Option<String>,
    /// Working directory for a session this client creates.
    pub cwd: Option<std::path::PathBuf>,
    /// Extra settings files, applied in the order given.
    pub config: Vec<std::path::PathBuf>,
    /// Gallery scene to preview instead of connecting, if the flag was given.
    ///
    /// `--gallery` alone resolves to [`gallery::FIRST`], so the field carries
    /// the scene either way and `run` has one branch to take.
    pub gallery: Option<String>,
    /// Motion preset for this launch, over every settings file.
    pub motion: Option<settings::MotionMode>,
    /// Spinner interval for this launch, in milliseconds.
    pub spinner_interval: Option<i64>,
    /// Frame limit for this launch, in frames a second.
    pub max_fps: Option<i64>,
    /// Start with the model's reasoning hidden, whatever the setting says.
    pub hide_thinking: bool,
    /// Print the version and exit.
    pub version: bool,
    /// Print the usage and exit.
    pub help: bool,
}

impl Args {
    /// Parse the arguments after the program name.
    pub fn parse<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = String>,
    {
        let mut parsed = Self::default();
        let mut arguments = args.into_iter();
        while let Some(argument) = arguments.next() {
            // The motion flags share one setting family and take both spellings,
            // so they are read and converted together instead of one arm per
            // flag per spelling.
            if settings::motion::LaunchFlag::read(
                &argument,
                &mut arguments,
                &mut parsed.motion,
                &mut parsed.spinner_interval,
                &mut parsed.max_fps,
            )? {
                continue;
            }
            match argument.as_str() {
                "--version" | "-V" => parsed.version = true,
                "--help" | "-h" => parsed.help = true,
                "--gallery" => parsed.gallery = Some(gallery::FIRST.to_string()),
                "--hide-thinking" => parsed.hide_thinking = true,
                "--socket" => match arguments.next() {
                    Some(path) => parsed.socket = Some(path.into()),
                    None => return Err("--socket needs a path".to_string()),
                },
                "--session" => match arguments.next() {
                    Some(session) if !session.is_empty() => parsed.session = Some(session),
                    _ => return Err("--session needs a session id".to_string()),
                },
                "--cwd" => match arguments.next() {
                    Some(path) => parsed.cwd = Some(path.into()),
                    None => return Err("--cwd needs a directory".to_string()),
                },
                "--config" => match arguments.next() {
                    Some(path) if !path.is_empty() => parsed.config.push(path.into()),
                    _ => return Err("--config needs a file".to_string()),
                },
                other => match other.split_once('=') {
                    Some(("--socket" | "--session" | "--cwd" | "--config", "")) => {
                        return Err(format!("{} needs a value", other.trim_end_matches('=')));
                    }
                    Some(("--gallery", "")) => {
                        return Err("--gallery needs a scene name after '='".to_string());
                    }
                    Some(("--gallery", scene)) => parsed.gallery = Some(scene.to_string()),
                    Some(("--socket", path)) => parsed.socket = Some(path.into()),
                    Some(("--session", session)) => parsed.session = Some(session.to_string()),
                    Some(("--cwd", path)) => parsed.cwd = Some(path.into()),
                    Some(("--config", path)) => parsed.config.push(path.into()),
                    _ => return Err(format!("unknown argument: {other}")),
                },
            }
        }
        Ok(parsed)
    }
}

/// Connect options for a socket override, keeping every other SDK default.
///
/// `ensure_runtime` stays on: an app that only works once the user has started
/// two daemons by hand is indistinguishable from a broken one.
///
/// `supports_ask_requests` is on because this client draws the questions and
/// answers them: the daemon parks a turn on the `ask` tool only for a
/// connection that says so, and the shipped default of `false` is why those
/// prompts never arrived.
pub fn connect_options(socket: Option<std::path::PathBuf>) -> horon_agent_sdk::ConnectOptions {
    horon_agent_sdk::ConnectOptions {
        socket_path: socket,
        client_name: concat!("horon-tui/", env!("CARGO_PKG_VERSION")).to_string(),
        ensure_runtime: true,
        supports_ask_requests: true,
        ..horon_agent_sdk::ConnectOptions::default()
    }
}

/// Run the client and return the process exit code.
///
/// The arguments are the process arguments including `argv[0]`, which is
/// skipped. Taking them as a parameter instead of reading `std::env::args`
/// directly is what lets a test cover the parse-and-exit paths.
pub fn run<I>(args: I) -> i32
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let arguments = args
        .into_iter()
        .skip(1)
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let args = match Args::parse(arguments) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("horon-tui: {message}");
            eprint!("{USAGE}");
            return 2;
        }
    };
    if args.version {
        println!("horon-tui {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    if args.help {
        print!("{USAGE}");
        return 0;
    }
    // The gallery is a viewer over built-in fixtures, so it runs before
    // settings are read and before a socket is opened: a frame must not depend
    // on a file on the machine, and a broken settings file is not a reason to
    // hide the renderer.
    if let Some(scene) = args.gallery.as_deref() {
        return gallery::run(scene);
    }
    // Settings load before the connection so a broken settings file is
    // reported even when the daemon is unreachable.
    let (sources, note) = settings::Sources::from_environment(args.config.clone());
    if let Some(note) = note {
        eprintln!("horon-tui: {note}");
    }
    // A launch flag outranks every file, so it rides with the reloader and is
    // reapplied after each resolve instead of being written anywhere.
    let overrides = settings::motion::launch_overrides(args.motion, args.spinner_interval, args.max_fps);
    let mut reloader = settings::Reloader::with_overrides(sources, std::time::Duration::from_secs(1), overrides);
    for warning in reloader.warnings() {
        eprintln!("horon-tui: settings: {warning}");
    }
    let mut keys = keys::Loader::new();
    for warning in keys.warnings() {
        eprintln!("horon-tui: keys: {warning}");
    }
    let working_dir = args.cwd.as_ref().map(|dir| dir.display().to_string());
    let launch_dir = path_completion::launch_directory(args.cwd);
    match sdk_bridge::connect(
        connect_options(args.socket),
        args.session.as_deref(),
        working_dir.as_deref(),
    ) {
        Ok(runtime) => run_interactive(runtime, &mut reloader, &mut keys, launch_dir, args.hide_thinking),
        Err(message) => {
            // A failed connection is an ordinary state, not a crash: draw the
            // banner if a terminal is available, always leave the sentence on
            // stderr, and exit non-zero so a script can see the failure.
            show_failure(&message);
            1
        }
    }
}

/// Drive the real terminal until the user leaves.
///
/// `launch_dir` is the directory the session was asked for, used only when the
/// daemon's answer names none, so `--cwd` and the session keep one workspace.
fn run_interactive(
    runtime: Runtime,
    reloader: &mut settings::Reloader,
    keys: &mut keys::Loader,
    launch_dir: Option<std::path::PathBuf>,
    hide_thinking: bool,
) -> i32 {
    let mut themes = ThemeLoader::new();
    let workspace = runtime.workspace.clone();
    // Read before the events travel with the app: the flag decides whether a
    // keystroke may read this machine's disk for the session.
    let local = runtime.local;
    let mut app = App::new(runtime.description.clone(), runtime.events);
    // The stderr copy of these warnings is wiped by the alternate screen, so
    // the transcript carries them for the person using the interface.
    for warning in reloader.warnings() {
        app.warn(format!("settings: {warning}"));
    }
    for warning in keys.warnings() {
        app.warn(format!("keys: {warning}"));
    }
    app.bindings = keys.bindings().clone();
    apply_settings(&mut app, reloader.settings(), &mut themes);
    // The flag is a session choice, applied after the settings because it
    // outranks them: `apply_settings` hands the transcript the setting, and
    // this then overrides what that setting resolved to.
    if hide_thinking {
        app.transcript.set_show_reasoning(false);
        // The notice names the key in force, not the shipped default: a
        // rebound `app.toggle_thinking` has to be the key it tells the user to
        // press.
        let shows = match app.bindings.first_key(keys::ids::APP_TOGGLE_THINKING) {
            Some(key) => format!("{key} shows it for this session"),
            None => "no key is bound to show it; /help lists the keys in force".to_string(),
        };
        app.transcript
            .push_notice(format!("reasoning hidden by --hide-thinking; {shows}"));
    }
    // The session's own directory names the workspace: `--cwd` is what the
    // create was asked for, and a resumed session reports the directory it
    // already has. A daemon that names none still gets the launch directory on
    // screen, but path completion stays off, because where the session works
    // is then only a guess.
    app.workspace_known = workspace.is_some();
    app.cwd = workspace.or_else(|| launch_dir.map(|dir| dir.display().to_string()));
    if app.cwd.is_none() {
        match std::env::current_dir() {
            Ok(directory) => app.cwd = Some(directory.display().to_string()),
            Err(error) => app.warn(format!("cannot read the working directory: {error}")),
        }
    }
    app.workspace_is_local = local;
    app.reload_commands();
    // `try_init` installs the restoring panic hook itself, so a panic inside
    // the alternate screen still leaves the terminal usable.
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            ratatui::restore();
            eprintln!("horon-tui: cannot open a terminal: {error}");
            return 1;
        }
    };
    // The restore hook knows nothing about the paste mode, the mouse mode or the
    // window title, so it is chained behind hooks that undo those: a panic must
    // not leave a mode on, or the window named after a session that is gone, for
    // the next program in this terminal.
    paste::guard_panics();
    scroll::guard_panics();
    title::guard_panics();
    // A terminal that refuses the mode is not a reason to stop: a paste then
    // arrives as ordinary keystrokes, which the editor already handles.
    if let Err(error) = paste::enable() {
        app.warn(format!("paste: {error}"));
    }
    let mut title = title::WindowTitle::stdout();
    // Decided once: the terminal's answer about its own title handling does not
    // change while the client runs, and a terminal that has none is left as it
    // was before `title.state` existed.
    let state_supported = title.state_supported();
    if let Err(error) = title.start(&window_label(&app, state_supported)) {
        app.warn(format!("title: {error}"));
    }
    // Installed once the modes it restores are on: a signal before this point
    // had nothing to undo. A refusal is reported and the client keeps running.
    if let Err(error) = terminal::guard_signals() {
        app.warn(format!("signals: {error}"));
    }
    let (code, failure) = event_loop(
        &mut terminal,
        &mut app,
        &mut title,
        &runtime.commands,
        reloader,
        &mut themes,
        keys,
    );
    // The same sequence the signal path runs: see `terminal::restore`.
    terminal::restore();
    if let Some(message) = failure {
        eprintln!("horon-tui: {message}");
    }
    code
}

/// Apply a resolved settings snapshot to the app and the theme loader.
///
/// Startup, a file change, and a save from the settings screen all end here, so
/// a value reaches the renderer by one path and a warning is reported once.
pub(crate) fn apply_settings(app: &mut App, settings: &settings::Settings, themes: &mut ThemeLoader) {
    for warning in themes.apply(settings) {
        app.warn(format!("theme: {warning}"));
    }
    let (layout, layout_warnings) = statusline::Layout::from_settings(settings);
    app.statusline = layout;
    for warning in layout_warnings {
        app.warn(format!("statusline: {warning}"));
    }
    app.refresh_settings(settings);
    // The palette is applied to the copy the renderer reads, never to the
    // theme file, so clearing the setting restores the selected theme exactly.
    app.theme = changes::apply(themes.theme(), ChangePalette::from_settings(settings));
    app.symbols = *themes.symbols();
    // The status line's own two choices sit on top of the theme and the glyph
    // preset, so they are applied here where every snapshot arrives rather than
    // in the drawing call: a save from the settings screen and a hand edit of
    // `tui.toml` then take effect on the next frame alike.
    statusline::apply_appearance(settings, &mut app.theme, &mut app.symbols);
}

/// Write the change the settings screen confirmed, then re-apply it.
///
/// A rejected save changes nothing in memory: the banner names the file and the
/// error, and the screen keeps showing the value that is still in force.
pub(crate) fn flush_setting_change(app: &mut App, reloader: &mut settings::Reloader, themes: &mut ThemeLoader) {
    let Some(change) = app.take_setting_change() else {
        return;
    };
    match reloader.save_global(change.id, &change.value) {
        Ok(warnings) => {
            for warning in warnings {
                app.warn(format!("settings: {warning}"));
            }
            app.banner = None;
            app.transcript.push_notice(format!(
                "settings: saved {} to {}",
                change.id,
                reloader.sources().global.display()
            ));
            let settings = reloader.settings().clone();
            apply_settings(app, &settings, themes);
        }
        Err(error) => {
            app.banner = Some(format!(
                "settings: cannot save {} to {}: {error}",
                change.id,
                reloader.sources().global.display()
            ));
            let current = reloader.settings().clone();
            app.refresh_settings(&current);
        }
    }
}

/// The main loop: drain SDK events, redraw, then read at most one keystroke.
///
/// A failure that ends the loop is returned instead of printed: the alternate
/// screen and raw mode are still on here, so a line written to stderr now would
/// be wiped by the restore. The caller restores first and prints afterwards.
fn event_loop<B>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    title: &mut title::WindowTitle,
    commands: &std::sync::mpsc::Sender<Command>,
    reloader: &mut settings::Reloader,
    themes: &mut ThemeLoader,
    keys: &mut keys::Loader,
) -> (i32, Option<String>)
where
    B: Backend,
    B::Error: std::fmt::Display,
{
    let mut failure = None;
    let mut mouse_captured = false;
    // The scheduler draws the first frame and sets the cadence after that.
    let mut frames = loop_io::Frames::new(app.motion, std::time::Instant::now());
    // The minute the status line's clock last showed, so the frame that turns
    // it over is drawn and no other idle frame is.
    let mut clock = statusline::local_minute();
    // Whether the terminal keeps a title worth adding the state to; asked once,
    // because the answer is the terminal's and does not change while it runs.
    let state_supported = title.state_supported();
    // The state the window title already carries: `title.start` wrote it, and
    // the first frame must not write it again.
    let mut shown_state = window_state(app, state_supported);
    loop {
        let mut changed = match loop_io::drain_events(app) {
            loop_io::Redraw::Immediate => true,
            loop_io::Redraw::None => false,
            loop_io::Redraw::Stream => {
                frames.request_stream();
                false
            }
        };
        if !app.running {
            break;
        }
        // A message typed during a turn waits for that turn; this is the one
        // place it is sent, right after the event that ended the turn.
        if let Some(action) = app.take_queued() {
            loop_io::dispatch(app, commands, action);
            changed = true;
        }
        // Titles can change on either a daemon event or local input.
        // A cycle pressed before the model catalog was known asked for it
        // instead; that press walks now that the answer is in.
        if let Some(action) = app.take_model_cycle() {
            loop_io::dispatch(app, commands, action);
            changed = true;
        }
        // Titles can change on either a daemon event or local input.
        let state = window_state(app, state_supported);
        if changed || state != shown_state {
            shown_state = state;
            let label = window_label(app, state_supported);
            if let Err(error) = title.show(&label) {
                app.warn(format!("title: {error}"));
                changed = true;
            }
        }
        // The reloader throttles itself to one file check per second; the
        // theme loader, the key bindings and the command directories share
        // that cadence for their own files.
        if let Some(reload) = reloader.tick() {
            for warning in reload.warnings {
                app.warn(format!("settings: {warning}"));
            }
            apply_settings(app, &reload.settings, themes);
            changed = true;
        }
        let theme_warnings = themes.tick();
        if !theme_warnings.is_empty() {
            for warning in theme_warnings {
                app.warn(format!("theme: {warning}"));
            }
            apply_settings(app, reloader.settings(), themes);
            changed = true;
        }
        if let Some(reload) = keys.tick() {
            for warning in reload.warnings {
                app.warn(format!("keys: {warning}"));
            }
            app.bindings = reload.bindings;
            changed = true;
        }
        // Command directories are only stat-ed here; a file is re-read when its
        // own stamp moved.
        changed |= app.tick_commands();
        if loop_io::sync_mouse_capture(app, &mut mouse_captured) {
            changed = true;
        }
        let minute = statusline::local_minute();
        if minute != clock {
            clock = minute;
            changed = true;
        }
        // The frame this pass owes: one for a screen that changed, one the
        // spinner scheduler says is due, and the first after the loop starts.
        // An idle screen reaches none of those and is left untouched.
        if let Err(message) = loop_io::draw_due(terminal, app, &mut frames, changed) {
            failure = Some(message);
            break;
        }
        match crossterm::event::poll(loop_io::POLL) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                failure = Some(format!("cannot read the keyboard: {error}"));
                app.running = false;
                app.exit_code = 1;
                break;
            }
        }
        // Every input event can change what is on screen: a key edits the
        // draft or moves the selection, a paste inserts, the wheel scrolls and a
        // resize remakes the whole layout.
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Key(key)) => {
                // Windows always reports a key's release as its own event; acting
                // on both would send every message twice.
                if key.is_release() {
                    continue;
                }
                if let Some(action) = app.on_key(key) {
                    loop_io::dispatch(app, commands, action);
                }
                flush_setting_change(app, reloader, themes);
            }
            // One paste is one edit of the draft and never a submission: a
            // pasted newline must not send a message the user is still writing.
            Ok(crossterm::event::Event::Paste(text)) => {
                app.on_paste(&text);
                flush_setting_change(app, reloader, themes);
            }
            // The wheel and a click in the draft are acted on, and only while
            // the setting holds the capture that makes the terminal report
            // them at all.
            Ok(crossterm::event::Event::Mouse(mouse)) => app.on_mouse(mouse),
            Ok(_) => {}
            Err(error) => {
                failure = Some(format!("cannot read the keyboard: {error}"));
                app.running = false;
                app.exit_code = 1;
            }
        }
        // Asked for after the event was acted on, so one keystroke costs one
        // frame rather than one per pass.
        frames.request();
    }
    (if failure.is_some() { 1 } else { app.exit_code }, failure)
}

/// Render one frame.
///
/// A pending repaint request is answered first: `clear` erases the screen and
/// resets the frame Ratatui keeps, so the draw below writes every cell instead
/// of only the rows that differ. That is what recovers a screen something else
/// has overwritten, and it is the whole of what the repaint key does.
pub fn draw<B>(terminal: &mut Terminal<B>, app: &mut App) -> Result<(), B::Error>
where
    B: Backend,
{
    if app.take_repaint_request() {
        terminal.clear()?;
    }
    terminal.draw(|frame| render(frame, app)).map(|_| ())
}

/// Rows the conversation keeps when a tall draft asks for more.
const MIN_TRANSCRIPT_ROWS: u16 = 3;

/// Rows the input pane grows to before the draft scrolls inside it.
const MAX_INPUT_ROWS: u16 = 8;

fn render(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    // The popup and the local suggestion are read from the draft on screen,
    // before anything borrows the app to draw.
    app.sync_slash();
    app.sync_suggestions();
    let ghost = app.suggestion_text();
    let theme: &Theme = &app.theme;
    let symbols = app.symbols;
    let prompt_lines = app.prompt_lines();
    // A prompt line can be wider than the pane (a long option label, a whole
    // command), and a clipped line hides the choice it names. Wrapping here is
    // what makes the pane taller for it instead of cutting it off.
    let prompt_rows: Vec<Line<'static>> = prompt_lines
        .iter()
        .enumerate()
        .flat_map(|(index, text)| {
            // The first line is the prompt itself; a background sets it apart
            // from the rest of the pane.
            let style = if index == 0 {
                Style::default()
                    .fg(theme.prompt.prompt_border)
                    .bg(theme.prompt.selected_bg)
            } else {
                Style::default().fg(theme.prompt.prompt_text)
            };
            crate::wrap::wrap_line(&Line::from(text.clone()), usize::from(frame.area().width))
                .into_iter()
                .map(move |row| row.style(style))
        })
        .collect();
    // The popup lists the same commands the hint names, with their
    // descriptions, so the hint is only the fallback for a popup `esc` closed.
    let popup = app.slash.is_open().then(|| app.slash.len());
    let hint = if popup.is_some() { None } else { app.completion_hint() };
    // The input pane grows with the draft, but never past a cap and never at
    // the transcript's expense: three rows of conversation stay readable on a
    // short terminal, and a taller draft scrolls inside its own pane.
    let marker_width = symbols.prompt_marker_width();
    let text_width = frame.area().width.saturating_sub(marker_width).saturating_sub(1);
    // The popup takes its rows from the transcript, so it is held to what is
    // left once the conversation keeps its minimum: on a terminal too short for
    // both, the conversation wins and the popup shows fewer commands.
    let fixed = u16::from(app.banner.is_some())
        .saturating_add(u16::try_from(prompt_rows.len()).unwrap_or(u16::MAX))
        .saturating_add(u16::from(hint.is_some()))
        .saturating_add(MIN_TRANSCRIPT_ROWS)
        .saturating_add(1);
    let popup_rows = match popup {
        Some(len) => (u16::try_from(len.min(crate::help::POPUP_ROWS)).unwrap_or(1))
            .min(frame.area().height.saturating_sub(fixed)),
        None => 0,
    };
    let todo_rows = app.todos.height(
        frame
            .area()
            .height
            .saturating_sub(fixed.saturating_add(popup_rows).saturating_add(1)),
    );
    let reserved = u16::from(app.banner.is_some())
        .saturating_add(u16::try_from(prompt_rows.len()).unwrap_or(u16::MAX))
        .saturating_add(u16::from(hint.is_some()))
        .saturating_add(popup_rows)
        .saturating_add(todo_rows)
        .saturating_add(1);
    let input_rows = frame
        .area()
        .height
        .saturating_sub(reserved.saturating_add(MIN_TRANSCRIPT_ROWS))
        .clamp(1, MAX_INPUT_ROWS);
    let view = app.input.view(text_width, input_rows);
    let mut constraints = vec![Constraint::Min(MIN_TRANSCRIPT_ROWS), Constraint::Length(1)];
    if popup_rows > 0 {
        constraints.push(Constraint::Length(popup_rows));
    }
    constraints.push(Constraint::Length(u16::try_from(view.rows.len()).unwrap_or(1)));
    if hint.is_some() {
        constraints.push(Constraint::Length(1));
    }
    if !prompt_rows.is_empty() {
        constraints.insert(1, Constraint::Length(prompt_rows.len() as u16));
    }
    if todo_rows > 0 {
        constraints.insert(1, Constraint::Length(todo_rows));
    }
    if app.banner.is_some() {
        constraints.insert(0, Constraint::Length(1));
    }
    let areas = Layout::vertical(constraints).split(frame.area());
    let mut index = 0;
    if app.banner.is_some() {
        if let Some(banner) = app.banner.clone() {
            let style = Style::default().fg(theme.core.error).add_modifier(Modifier::REVERSED);
            frame.render_widget(
                Paragraph::new(Line::from(format!("! {banner}"))).style(style),
                areas[index],
            );
        }
        index += 1;
    }

    let transcript_area: Rect = areas[index];
    app.viewport_height = Some(transcript_area.height);
    let pane = transcript_pane(app, transcript_area.width, transcript_area.height);
    // The pane measured the scrollback through a mutable borrow; the rest of the
    // frame reads the theme again from the app it was borrowed from.
    let theme: &Theme = &app.theme;
    let rows_height = transcript_area
        .height
        .saturating_sub(u16::from(pane.indicator.is_some()));
    let rows_area = Rect {
        height: rows_height,
        ..transcript_area
    };
    frame.render_widget(Paragraph::new(pane.rows), rows_area);
    if let Some(indicator) = pane.indicator {
        let indicator_area = Rect {
            y: transcript_area.y.saturating_add(rows_height),
            height: transcript_area.height.saturating_sub(rows_height),
            ..transcript_area
        };
        frame.render_widget(Paragraph::new(indicator), indicator_area);
    }
    index += 1;

    if todo_rows > 0 {
        let lines: Vec<_> = app.todos.lines(todo_rows, theme).collect();
        frame.render_widget(Paragraph::new(lines), areas[index]);
        index += 1;
    }

    if !prompt_rows.is_empty() {
        frame.render_widget(Paragraph::new(prompt_rows), areas[index]);
        index += 1;
    }

    let status_area = areas[index];
    frame.render_widget(status_widget(app, status_area.width), status_area);
    index += 1;

    // Above the input and below the status line: the commands are read next to
    // the draft they complete, and the decision line keeps its own row.
    if popup_rows > 0 {
        let popup_area = areas[index];
        let (first, count) = crate::help::window(app.slash.len(), app.slash.selected(), usize::from(popup_rows));
        let lines = crate::help::popup_lines(
            app.slash.matches(),
            app.slash.selected(),
            first,
            count,
            usize::from(popup_area.width),
            crate::help::Look {
                needle: app.slash.needle().unwrap_or(""),
                symbols,
            },
            theme,
        );
        frame.render_widget(Paragraph::new(lines), popup_area);
        index += 1;
    }

    let input_area = areas[index];
    // The mouse click is read against the same cells the frame used, so the
    // pane's position and the character each row starts at are recorded once
    // the layout is final. An edit drops them; the next frame records them
    // again.
    app.input.note_view(
        crate::input::Placement {
            x: input_area.x,
            y: input_area.y,
            lead: marker_width.saturating_add(1),
        },
        &view,
    );
    // One row per visible line of the draft. Only the first carries the prompt
    // marker; the rest are padded to the same column so the text lines up under
    // itself instead of under the marker.
    let marker = format!("{} ", symbols.prompt_marker);
    let padding = " ".repeat(usize::from(marker_width) + 1);
    // The ghost suffix is drawn after the caret on the row the caret is on.
    // The offer was checked against the exact draft before this frame, so what
    // is drawn here is a suffix of the text already on screen.
    let rows: Vec<Line<'static>> = view
        .rows
        .iter()
        .enumerate()
        .map(|(row, text)| {
            let lead = if row == 0 { marker.clone() } else { padding.clone() };
            let mut spans = vec![
                Span::styled(lead, Style::default().fg(theme.core.accent)),
                Span::styled(text.clone(), Style::default().fg(theme.prompt.prompt_text)),
            ];
            if row == view.cursor_row
                && let Some(ghost) = ghost.as_deref()
            {
                let room = text_width.saturating_sub(crate::input::text_cells(text));
                let tail = crate::input::clip_cells(ghost, room);
                if !tail.is_empty() {
                    spans.push(Span::styled(tail, Style::default().fg(theme.core.muted)));
                }
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), input_area);
    if let Some(hint) = hint {
        index += 1;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(theme.core.muted)))),
            areas[index],
        );
    }
    // The settings screen covers the conversation rather than the input, so
    // closing it reveals the transcript unchanged and the message being typed
    // stays visible under it; a picker covers the same area and sits above it.
    if let Some(picker) = app.picker.as_mut() {
        overlays::picker::draw(frame, transcript_area, picker, theme, symbols);
        return;
    }
    match app.overlay.as_ref() {
        Some(overlay) => {
            if let Some((x, y)) = overlays::settings::draw(frame, transcript_area, overlay, theme, symbols) {
                frame.set_cursor_position((x, y));
            }
        }
        None => {
            let mut cursor_x = input_area
                .x
                .saturating_add(marker_width)
                .saturating_add(1)
                .saturating_add(view.cursor_cell);
            if input_area.width > 0 {
                // The caret never paints past the pane, and the terminal cannot
                // place it at the cell after the last column.
                cursor_x = cursor_x.min(input_area.right().saturating_sub(1));
            }
            let cursor_y = input_area
                .y
                .saturating_add(u16::try_from(view.cursor_row).unwrap_or(0))
                .min(input_area.bottom().saturating_sub(1));
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

/// The bottom line: the segments the status-line layout names, joined by the
/// separator in force, on the theme's background unless `statusline.background`
/// leaves the terminal's own showing.
fn status_widget(app: &App, width: u16) -> Paragraph<'static> {
    let mut data = app.status_data();
    data.now = Some(statusline::local_now());
    let line = statusline::render(&data, &app.statusline, &app.theme, app.symbols, width);
    Paragraph::new(line).style(Style::default().bg(app.theme.status.status_bg))
}

/// The transcript pane: the rows on screen, and the follow-bottom indicator.
///
/// Lines are wrapped here rather than by the widget: a logical line that wraps
/// over the pane width occupies more than one row, so trimming logical lines
/// first would push the newest rows off the bottom of the pane.
///
/// Items are walked from the newest one so the work is bounded by the pane: a
/// turn in flight redraws as its text arrives, and wrapping the whole
/// conversation on every one of those frames would cost more as the session
/// grows.
///
/// A message that is still streaming is drawn first as the newest entry, so
/// text appears while it arrives instead of all at once when the turn ends.
/// It is not also in `items`, and `finish_stream` moves it there exactly once.
///
/// A view held above the end is a different walk: the content is measured once,
/// the rows below it are skipped by whole blocks, and only the block holding the
/// bottom edge of the view is wrapped again. That is what keeps a frame's cost at
/// the visible rows plus the scroll offset rather than the whole conversation.
fn transcript_pane(app: &mut App, width: u16, height: u16) -> Pane {
    // Noticed before anything is drawn: a delta that lands between two frames is
    // what the indicator calls new output, whether or not it added a row.
    let mark = scroll::Mark::new(
        app.transcript.items().len(),
        app.transcript.open_text().map_or(0, str::len),
        app.transcript.open_reasoning().map_or(0, str::len),
    );
    app.scroll.observe(mark);
    let view = usize::from(height.max(1));
    // The indicator spends the pane's bottom row while it is drawn, so the
    // content is measured against the rows it may actually fill.
    let held_before = app.scroll.offset() > 0;
    let measure_rows = if held_before {
        view.saturating_sub(1).max(1)
    } else {
        view
    };
    if held_before {
        measure_transcript(app, width, measure_rows);
    } else {
        // A frame at the end measures nothing, so it must say so: the next held
        // frame takes its height baseline fresh rather than trusting the last
        // scroll's.
        app.scroll.following_frame();
    }
    let offset = app.scroll.offset();
    let indicator = offset > 0 && view > 1;
    let capacity = if indicator { view - 1 } else { view };
    let rows = if offset > 0 {
        held_rows(app, width, offset, capacity)
    } else {
        follow_rows(app, width, capacity)
    };
    let indicator = if offset > 0 && view > 1 {
        Some(indicator_line(app, offset))
    } else {
        None
    };
    Pane { rows, indicator }
}

/// The rows the pane shows, and the line that says it is not at the end.
struct Pane {
    rows: Vec<Line<'static>>,
    indicator: Option<Line<'static>>,
}

/// One block of the conversation, in the order the pane draws them.
enum Block<'a> {
    Item(&'a Item),
    Reasoning(&'a str),
    Answer(&'a str),
}

/// The conversation as the pane draws it, oldest first.
///
/// Reasoning being assembled sits above the answer it precedes, the same order
/// the model produced it in, and both sit below everything already committed.
fn blocks(app: &App) -> Vec<Block<'_>> {
    let mut blocks: Vec<Block<'_>> = app.transcript.items().iter().map(Block::Item).collect();
    if let Some(text) = app.transcript.open_reasoning() {
        blocks.push(Block::Reasoning(text));
    }
    if let Some(text) = app.transcript.open_text() {
        blocks.push(Block::Answer(text));
    }
    blocks
}

/// One block's lines, ready to be asked for the rows the pane shows.
///
/// The wrapped height is what the scrollback measures, so a block means the same
/// thing here and in `measure_transcript`. `first` is the oldest block, which has
/// nothing above it to be separated from.
///
/// A block is not wrapped here: the pane asks its source for the rows it draws,
/// and a tool call drawing its whole output answers with only the rows near the
/// end instead of a row for every line it kept.
fn block_source<'a>(
    block: &Block<'_>,
    look: &item_lines::Look<'_>,
    expand: Option<&tool_expand::Frame<'a>>,
    first: bool,
) -> wrap::Source<'a> {
    let mut source = match block {
        Block::Item(item) => item_lines::source(item, look, expand),
        Block::Reasoning(text) => wrap::Source::built(item_lines::reasoning(text, look)),
        Block::Answer(text) => wrap::Source::built(item_lines::answer(text, look)),
    };
    // A turn starts one blank row below the previous one, so a new message is
    // not read as more of the answer above it. It is a line of the block, so a
    // window that starts at the block's first row still counts it.
    if !first && matches!(block, Block::Item(Item::User(_))) {
        source.prepend(wrap::Indented::plain(Line::default()));
    }
    source
}

/// Count the conversation's rows so a held view knows how far it may move.
///
/// The measurement is kept between frames: only items the last one never saw are
/// wrapped, so a view held during a long turn does not re-wrap the conversation.
/// The block still streaming is counted every frame, because it is the one block
/// whose height changes while the view is held.
fn measure_transcript(app: &mut App, width: u16, view: usize) {
    let items = app.transcript.items().len();
    let App {
        scroll,
        transcript,
        theme,
        symbols,
        settings,
        bindings,
        ..
    } = app;
    let look = item_lines::Look {
        theme,
        symbols: *symbols,
        width,
        markdown: item_lines::markdown_enabled(settings),
        visual_blocks: item_lines::visual_blocks_enabled(settings),
    };
    let open = [
        open_rows(transcript.open_reasoning(), &look, true),
        open_rows(transcript.open_text(), &look, false),
    ];
    // An expanded call is one row taller per row of its whole output, so the
    // height measured here has to know about the expansion: a view held above
    // the end counts this height, and a stale one would leave the offset short.
    let expand = tool_expand::Frame::new(transcript.expand_state(), bindings);
    scroll.measure(width, items, open, view, |index| match transcript.items().get(index) {
        // Counted, not built: the rows of a block that stays off screen are not
        // made to measure it.
        Some(item) => block_source(&Block::Item(item), &look, Some(&expand), index == 0).height(width),
        None => 0,
    });
}

/// Wrapped rows of one open buffer, as the pane would draw it.
fn open_rows(text: Option<&str>, look: &item_lines::Look<'_>, reasoning: bool) -> usize {
    match text {
        Some(text) if reasoning => wrap::Source::built(item_lines::reasoning(text, look)).height(look.width),
        Some(text) => wrap::Source::built(item_lines::answer(text, look)).height(look.width),
        None => 0,
    }
}

/// The newest rows, following the end of the conversation.
fn follow_rows(app: &App, width: u16, capacity: usize) -> Vec<Line<'static>> {
    let look = item_lines::Look::new(app, width);
    let expand = tool_expand::Frame::new(app.transcript.expand_state(), &app.bindings);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let blocks = blocks(app);
    for (index, block) in blocks.iter().enumerate().rev() {
        if rows.len() >= capacity {
            break;
        }
        // Only the rows this block contributes to the pane are built, and they
        // are built from the end: a block taller than the pane costs the pane's
        // rows, not its own.
        let source = block_source(block, &look, Some(&expand), index == 0);
        let wanted = capacity - rows.len();
        let window = source.rows_from_end(width, wrap::FromEnd::new(0, wanted));
        rows.extend(window.into_iter().rev());
    }
    rows.reverse();
    placeholder(app, rows, width)
}

/// The rows a view held `offset` rows above the end shows.
///
/// Blocks below the view are skipped by their measured height, so the rows the
/// user scrolled past are not wrapped again. Only the block that holds the bottom
/// edge of the view is wrapped, and only for the rows above that edge.
fn held_rows(app: &App, width: u16, offset: usize, capacity: usize) -> Vec<Line<'static>> {
    let look = item_lines::Look::new(app, width);
    let expand = tool_expand::Frame::new(app.transcript.expand_state(), &app.bindings);
    let blocks = blocks(app);
    let measured = block_heights(app);
    // A measurement taken before the block list changed says nothing about this
    // one, so it is dropped rather than read against the wrong block.
    let heights: &[usize] = if measured.len() == blocks.len() { &measured } else { &[] };
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut below = offset;
    for (index, block) in blocks.iter().enumerate().rev() {
        match heights.get(index) {
            // Wholly below the view: its height is known, so it is skipped
            // without wrapping a single line of it.
            Some(&height) if below >= height && height > 0 => {
                below -= height;
                continue;
            }
            _ => {}
        }
        if rows.len() >= capacity {
            break;
        }
        let source = block_source(block, &look, Some(&expand), index == 0);
        let wanted = capacity - rows.len();
        // The rows below the view's bottom edge are counted and dropped; what
        // is built is the window above them that the pane fills with.
        let window = source.rows_from_end(width, wrap::FromEnd::new(below, wanted));
        rows.extend(window.into_iter().rev());
        below = 0;
    }
    rows.reverse();
    rows
}

/// The measured height of every block, in the order the pane draws them.
///
/// The committed items come from the scrollback's tally; the two open buffers
/// are blocks only while they are being streamed, so their slots are added only
/// then, and the list stays aligned with [`blocks`].
fn block_heights(app: &App) -> Vec<usize> {
    let mut heights = match app.scroll.heights() {
        Some(items) => items.to_vec(),
        None => Vec::new(),
    };
    let open = app.scroll.open_heights();
    if app.transcript.open_reasoning().is_some() {
        heights.push(open[0]);
    }
    if app.transcript.open_text().is_some() {
        heights.push(open[1]);
    }
    heights
}

/// The line a held view keeps in the pane's bottom row.
fn indicator_line(app: &App, below: usize) -> Line<'static> {
    let counted = if below == 1 {
        "1 row".to_string()
    } else {
        format!("{below} rows")
    };
    let text = if app.scroll.has_fresh_output() {
        format!("{} {counted} below · new output", app.symbols.more_below)
    } else {
        format!("{} {counted} below", app.symbols.more_below)
    };
    Line::from(Span::styled(text, Style::default().fg(app.theme.core.muted)))
}

/// What an empty transcript says instead of nothing.
fn placeholder(app: &App, rows: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    // A conversation whose only rows are hidden tool calls still has content:
    // asking the reader to send a message would misread what is on screen.
    if !rows.is_empty() || !app.transcript.items().is_empty() {
        return rows;
    }
    wrap::wrap_line(
        &Line::from(Span::styled(
            "Send a message to start. /help lists the commands; /quit leaves.",
            Style::default().fg(app.theme.core.muted),
        )),
        usize::from(width),
    )
}

#[cfg(test)]
#[path = "picker_app_tests.rs"]
mod picker_app_tests;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
