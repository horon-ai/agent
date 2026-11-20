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
pub mod help;
pub mod history;
pub mod input;
mod item_lines;
pub mod keys;
mod loop_io;
mod markdown;
pub mod overlays;
pub mod paste;
pub mod prompts;
pub mod report;
pub mod scroll;
pub mod sdk_bridge;
pub mod settings;
pub mod statusline;
mod terminal;
pub mod theme;
mod title;
mod todo_panel;
mod tool_expand;
pub mod tool_line;
pub mod transcript;
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

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::sdk_bridge::{Command, Runtime};
use crate::theme::{Theme, ThemeLoader};
// The test modules reach this through `use super::*`; production code draws
// through `item_lines`, which takes the glyph set from the app itself.
#[cfg(test)]
use crate::theme::Symbols;
use crate::transcript::Item;

/// Program usage, printed by `--help` and after an argument error.
pub const USAGE: &str = "\
horon-tui - terminal client for the horon-agent harness

Usage: horon-tui [--socket <path>] [--session <id>] [--cwd <dir>]
                 [--config <file>]...
                 [--motion <full|reduced|static>]
                 [--spinner-interval <ms>] [--max-fps <n>]

Options:
  --socket <path>           Harness API socket to connect to (default: the
                            resolved harness socket for this user)
  --session <id>            Attach to an existing session instead of creating one
  --cwd <dir>               Working directory for a newly created session
  --config <file>           Extra settings file, read after the global and
                            project files and before the environment
                            (repeatable)
  --motion <mode>           full, reduced or static; overrides the `motion`
                            setting for this launch
  --spinner-interval <ms>   Milliseconds one spinner frame stays, 40 to 2000;
                            overrides `motion.spinner_interval`
  --max-fps <n>             Most frames a second a streaming turn draws, 1 to
                            240; overrides `motion.max_fps`
  -V, --version             Print the version and exit
  -h, --help                Print this help and exit

Keys:
  Enter          send the typed message
  Alt-Enter      start a new line (Ctrl-J does the same; most terminals
                 send Shift-Enter as a plain Enter, which sends)
  Tab            complete the slash command being typed
  Esc            send the queued messages into the running turn, or
                 interrupt it when nothing is queued
  Ctrl-C         cancel a running turn, or leave when nothing is running
  Ctrl-D         leave
  Ctrl-U, Ctrl-K delete from the start or the end of the line to the cursor
  Ctrl-W         delete the word before the cursor
  Ctrl-R         search the drafts sent before this one
  PgUp, PgDn     scroll the transcript one page
  Shift-Up/Down  scroll the transcript one row
  Ctrl-End       follow the end of the conversation again
  Ctrl-O         show or hide the whole output of the newest cut tool call
  Shift-Tab      step to the next reasoning effort
  Alt-P          pick the model this session uses
  Bindings are remapped in $HORON_HOME/keybindings.toml, and /help lists
  the keys in force.

Commands:
  /help          list the commands (built-in, harness, command files),
                 the skills and the keys in force; /help <command>
                 describes one of them
  /clear         clear the transcript pane
  /effort        set the reasoning effort, or pick one from the ladder
  /exit          leave, the same as /quit
  /keys          list the keys in force and what each does
  /model [name]  pick a model from a list, or switch to the one named
  /new           start a new session in this directory
  /quit          leave
  /resume [id]   replay a stored session, or pick one from the list
  /settings      open the settings screen, generated from the registry
  /skills        list the skills the harness reports, and put one in the
                 draft
  /switch [id]   move to another stored session, or pick one from the list
  //text         send the literal message /text
";

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
    /// Motion preset for this launch, over every settings file.
    pub motion: Option<settings::MotionMode>,
    /// Spinner interval for this launch, in milliseconds.
    pub spinner_interval: Option<i64>,
    /// Frame limit for this launch, in frames a second.
    pub max_fps: Option<i64>,
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
            match argument.as_str() {
                "--version" | "-V" => parsed.version = true,
                "--help" | "-h" => parsed.help = true,
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
                "--motion" => match arguments.next() {
                    Some(value) => parsed.motion = Some(settings::motion::parse_mode(&value).map_err(String::from)?),
                    None => return Err("--motion needs full, reduced or static".to_string()),
                },
                "--spinner-interval" => match arguments.next() {
                    Some(value) => {
                        parsed.spinner_interval =
                            Some(settings::motion::parse_spinner_interval(&value).map_err(String::from)?);
                    }
                    None => return Err("--spinner-interval needs milliseconds".to_string()),
                },
                "--max-fps" => match arguments.next() {
                    Some(value) => {
                        parsed.max_fps = Some(settings::motion::parse_max_fps(&value).map_err(String::from)?)
                    }
                    None => return Err("--max-fps needs frames a second".to_string()),
                },
                other => match other.split_once('=') {
                    Some(("--socket" | "--session" | "--cwd" | "--config", "")) => {
                        return Err(format!("{} needs a value", other.trim_end_matches('=')));
                    }
                    Some(("--socket", path)) => parsed.socket = Some(path.into()),
                    Some(("--session", session)) => parsed.session = Some(session.to_string()),
                    Some(("--cwd", path)) => parsed.cwd = Some(path.into()),
                    Some(("--config", path)) => parsed.config.push(path.into()),
                    Some(("--motion", value)) => {
                        parsed.motion = Some(settings::motion::parse_mode(value).map_err(String::from)?);
                    }
                    Some(("--spinner-interval", value)) => {
                        parsed.spinner_interval =
                            Some(settings::motion::parse_spinner_interval(value).map_err(String::from)?);
                    }
                    Some(("--max-fps", value)) => {
                        parsed.max_fps = Some(settings::motion::parse_max_fps(value).map_err(String::from)?);
                    }
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
    match sdk_bridge::connect(
        connect_options(args.socket),
        args.session.as_deref(),
        working_dir.as_deref(),
    ) {
        Ok(runtime) => run_interactive(runtime, &mut reloader, &mut keys),
        Err(message) => {
            // A failed connection is an ordinary state, not a crash: draw the
            // banner if a terminal is available, always leave the sentence on
            // stderr, and exit non-zero so a script can see the failure.
            show_failure(&message);
            1
        }
    }
}

/// Draw the connection-failure banner once, when there is a terminal to draw it
/// on, then restore it.
fn show_failure(message: &str) {
    eprintln!("horon-tui: {message}");
    let mut app = App::failure(message);
    match ratatui::try_init() {
        Ok(mut terminal) => {
            let failure = draw(&mut terminal, &mut app).err();
            paste::disable();
            ratatui::restore();
            if let Some(message) = failure {
                eprintln!("horon-tui: {message}");
            }
        }
        Err(error) => {
            paste::disable();
            ratatui::restore();
            eprintln!("horon-tui: cannot open a terminal: {error}");
        }
    }
}

/// The name the window carries for the app's state.
///
/// The session's own title wins and the working directory names a session the
/// daemon never titled, so a stack of terminals says which one belongs to which
/// session and a fresh window still says where it is.
fn window_label(app: &App) -> String {
    title::label(app.session_title.as_deref(), app.cwd.as_deref())
}

/// Drive the real terminal until the user leaves.
fn run_interactive(runtime: Runtime, reloader: &mut settings::Reloader, keys: &mut keys::Loader) -> i32 {
    let mut themes = ThemeLoader::new();
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
    match std::env::current_dir() {
        Ok(directory) => app.cwd = Some(directory.display().to_string()),
        Err(error) => app.warn(format!("cannot read the working directory: {error}")),
    }
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
    if let Err(error) = title.start(&window_label(&app)) {
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
    app.theme = themes.theme().clone();
    app.symbols = *themes.symbols();
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
    // Whether the terminal is currently reporting the mouse, which follows the
    // `transcript.mouse_scroll` setting rather than being decided once.
    let mut mouse_captured = false;
    // The screen is redrawn when something on it changed, not twenty times a
    // second: a draw walks the visible conversation, and an idle client has
    // nothing new to show. A turn in flight is the one exception, and its
    // cadence is the motion settings'; the scheduler's first pass draws.
    let mut frames = loop_io::Frames::new(app.motion);
    // The minute the status line's clock last showed, so the frame that turns
    // it over is drawn and no other idle frame is.
    let mut clock = statusline::local_minute();
    loop {
        let mut changed = loop_io::drain_events(app);
        if !app.running {
            break;
        }
        // A message typed during a turn waits for that turn; this is the one
        // place it is sent, right after the event that ended the turn.
        if let Some(action) = app.take_queued() {
            loop_io::dispatch(app, commands, action);
            changed = true;
        }
        // The session and the working directory can change while the client
        // runs; the window is renamed only when the label on it is not the one
        // the app now names.
        if changed {
            let label = window_label(app);
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
        // A settings file, the settings screen and a flag all end in one
        // applied snapshot; adopting its cadence here is what makes a changed
        // preset take effect without waiting for the next event.
        frames.retune(app.motion);
        // One frame is drawn when something changed, when the scheduler says
        // the spinner is due, or when its first pass has not drawn yet. An idle
        // screen reaches none of those and is left untouched, which is the
        // behaviour the client had before the animation existed.
        if changed {
            frames.request();
        }
        if let Some(frame) = frames.due(app.animating()) {
            app.motion_frame = frame;
            if let Err(message) = draw(terminal, app) {
                failure = Some(message);
                break;
            }
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
            // Only the wheel is acted on, and only while the setting holds the
            // capture that makes the terminal report it at all.
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
pub fn draw<B>(terminal: &mut Terminal<B>, app: &mut App) -> Result<(), String>
where
    B: Backend,
    B::Error: std::fmt::Display,
{
    terminal
        .draw(|frame| render(frame, app))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Rows the conversation keeps when a tall draft asks for more.
const MIN_TRANSCRIPT_ROWS: u16 = 3;

/// Rows the input pane grows to before the draft scrolls inside it.
const MAX_INPUT_ROWS: u16 = 8;

fn render(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    // The popup is read from the draft on screen, before anything borrows the
    // app to draw.
    app.sync_slash();
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
    // One row per visible line of the draft. Only the first carries the prompt
    // marker; the rest are padded to the same column so the text lines up under
    // itself instead of under the marker.
    let marker = format!("{} ", symbols.prompt_marker);
    let padding = " ".repeat(usize::from(marker_width) + 1);
    let rows: Vec<Line<'static>> = view
        .rows
        .iter()
        .enumerate()
        .map(|(row, text)| {
            let lead = if row == 0 { marker.clone() } else { padding.clone() };
            Line::from(vec![
                Span::styled(lead, Style::default().fg(theme.core.accent)),
                Span::styled(text.clone(), Style::default().fg(theme.prompt.prompt_text)),
            ])
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
/// theme's separator with the right-hand group aligned to the edge.
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
    if !rows.is_empty() {
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
