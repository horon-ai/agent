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
mod artifact;
mod clipboard;
pub mod commands;
pub mod context;
mod editor;
mod fuzzy;
pub mod gallery;
mod git_review;
pub mod help;
pub mod history;
pub mod input;
mod item_lines;
pub mod keys;
mod links;
mod loop_io;
mod markdown;
pub mod notes;
pub mod overlays;
mod pane;
pub mod paste;
mod path_completion;
mod progress;
pub mod prompts;
pub mod recording;
pub mod report;
pub mod scroll;
pub mod sdk_bridge;
mod session_pins;
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
mod voice;
mod window;
mod work_notes;
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
#[path = "progress_tests.rs"]
mod progress_tests;

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
#[path = "recording_tests.rs"]
mod recording_tests;

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
#[path = "artifact_tests.rs"]
mod artifact_tests;

#[cfg(test)]
#[path = "fuzzy_tests.rs"]
mod fuzzy_tests;

#[cfg(test)]
#[path = "path_completion_tests.rs"]
mod path_completion_tests;

#[cfg(test)]
#[path = "git_review_tests.rs"]
mod git_review_tests;

#[cfg(test)]
#[path = "voice_tests.rs"]
mod voice_tests;

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::pane::transcript_pane;
use crate::progress::Channels;
use crate::sdk_bridge::{Command, Runtime};
use crate::terminal::show_failure;
use crate::theme::changes::{self, ChangePalette};
use crate::theme::{Theme, ThemeLoader};
// The test modules reach this through `use super::*`; production code draws
// through `item_lines`, which takes the glyph set from the app itself.
#[cfg(test)]
use crate::theme::Symbols;
#[cfg(test)]
use crate::transcript::Item;

pub use help::USAGE;

/// Parsed command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    /// Socket to connect to instead of the resolved default.
    pub socket: Option<std::path::PathBuf>,
    /// Existing session to attach to instead of creating one.
    pub session: Option<String>,
    /// Objective and token ceiling for a new session.
    pub goal: Option<String>,
    pub goal_budget: Option<u64>,
    /// Start the new session in read-only plan mode.
    pub plan: bool,
    /// Working directory for a session this client creates.
    pub cwd: Option<std::path::PathBuf>,
    /// Extra workspace directories for the attached session, in the order given.
    pub add_dirs: Vec<std::path::PathBuf>,
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
    /// Leave the terminal window title alone for this launch.
    pub no_title: bool,
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
                "--no-title" => parsed.no_title = true,
                "--socket" => match arguments.next() {
                    Some(path) => parsed.socket = Some(path.into()),
                    None => return Err("--socket needs a path".to_string()),
                },
                "--session" => match arguments.next() {
                    Some(session) if !session.is_empty() => parsed.session = Some(session),
                    _ => return Err("--session needs a session id".to_string()),
                },
                "--goal" => parsed.goal = Some(parse_goal(arguments.next().as_deref())?),
                "--plan" => parsed.plan = true,
                "--goal-budget" => parsed.goal_budget = Some(parse_goal_budget(arguments.next().as_deref())?),
                "--cwd" => match arguments.next() {
                    Some(path) => parsed.cwd = Some(path.into()),
                    None => return Err("--cwd needs a directory".to_string()),
                },
                "--add-dir" => match arguments.next() {
                    Some(path) if !path.is_empty() => parsed.add_dirs.push(path.into()),
                    _ => return Err("--add-dir needs a directory".to_string()),
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
                    Some(("--goal", title)) => parsed.goal = Some(parse_goal(Some(title))?),
                    Some(("--goal-budget", budget)) => {
                        parsed.goal_budget = Some(parse_goal_budget(Some(budget))?);
                    }
                    Some(("--cwd", path)) => parsed.cwd = Some(path.into()),
                    Some(("--add-dir", "")) => {
                        return Err("--add-dir needs a value".to_string());
                    }
                    Some(("--add-dir", path)) => parsed.add_dirs.push(path.into()),
                    Some(("--config", path)) => parsed.config.push(path.into()),
                    _ => return Err(format!("unknown argument: {other}")),
                },
            }
        }
        if parsed.goal.is_some() != parsed.goal_budget.is_some() {
            return Err("--goal and --goal-budget must be used together".into());
        }
        if parsed.plan && (parsed.session.is_some() || parsed.gallery.is_some()) {
            return Err("--plan can only be used when starting a new session".into());
        }
        if parsed.goal.is_some() && (parsed.session.is_some() || parsed.gallery.is_some()) {
            return Err("--goal can only be used when starting a new session".into());
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

fn parse_goal(value: Option<&str>) -> Result<String, &'static str> {
    match value {
        Some(title) if !title.trim().is_empty() => Ok(title.to_string()),
        _ => Err("--goal needs a nonempty objective"),
    }
}

fn parse_goal_budget(value: Option<&str>) -> Result<u64, &'static str> {
    let message = "--goal-budget needs a positive token count";
    let Some(raw) = value else {
        return Err(message);
    };
    let budget = raw.parse::<u64>().map_err(|_| message)?;
    if budget == 0 {
        return Err(message);
    }
    Ok(budget)
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
        return if args.no_title {
            gallery::run_with_title(scene, false)
        } else {
            gallery::run(scene)
        };
    }
    // Settings load before the connection so a broken settings file is
    // reported even when the daemon is unreachable.
    let (sources, note) = settings::Sources::from_environment(args.config.clone());
    if let Some(note) = note {
        eprintln!("horon-tui: {note}");
    }
    // A launch flag outranks every file, so it rides with the reloader and is
    // reapplied after each resolve instead of being written anywhere.
    let mut overrides = settings::motion::launch_overrides(args.motion, args.spinner_interval, args.max_fps);
    if args.no_title {
        overrides.push(("title.enabled", settings::SettingValue::Bool(false)));
    }
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
        Ok(runtime) => {
            // The session's extra roots are sent before the goal so a goal's
            // first turn already sees them in its session context.
            for directory in &args.add_dirs {
                if let Err(error) = runtime.commands.send(Command::Runtime {
                    command: horon_agent_sdk::RuntimeCommand::Dirs,
                    arguments: vec!["add".to_string(), directory.display().to_string()],
                }) {
                    show_failure(&format!(
                        "could not add workspace directory {}: {error}",
                        directory.display()
                    ));
                    return 1;
                }
            }
            if let Some((title, budget)) = args.goal.zip(args.goal_budget)
                && let Err(error) = runtime.commands.send(Command::Runtime {
                    command: horon_agent_sdk::RuntimeCommand::Goal,
                    arguments: vec![budget.to_string(), title],
                })
            {
                show_failure(&format!("could not queue the startup goal: {error}"));
                return 1;
            }
            if args.plan
                && let Err(error) = runtime.commands.send(Command::Runtime {
                    command: horon_agent_sdk::RuntimeCommand::Plan,
                    arguments: vec!["on".into()],
                })
            {
                show_failure(&format!("could not queue plan mode: {error}"));
                return 1;
            }
            run_interactive(runtime, &mut reloader, &mut keys, launch_dir, args.hide_thinking)
        }
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
    // The endpoint names the list of pins this client reads and writes: two
    // daemons can issue the same session id, so the file keeps them apart.
    let pin_scope = runtime.scope.clone();
    // Read before the events travel with the app: the flag decides whether a
    // keystroke may read this machine's disk for the session.
    let local = runtime.local;
    let mut app = App::new(runtime.description.clone(), runtime.events);
    // Notes live in a file this client owns, read here so a replay's notes are
    // on screen with the conversation they describe and never on a path a test
    // or an embedded `App` writes to.
    app.notes = notes::NoteStore::owned();
    app.load_notes();
    app.pins.store = session_pins::PinStore::owned(pin_scope);
    app.load_pins();
    // The alternate screen hides stderr, so also show warnings in the transcript.
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
    if let Some(profile) = std::env::var_os("HORON_PROFILE").filter(|name| !name.is_empty()) {
        app.transcript
            .push_notice(format!("profile: {}", profile.to_string_lossy()));
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
    progress::guard_panics();
    // A terminal that refuses the mode is not a reason to stop: a paste then
    // arrives as ordinary keystrokes, which the editor already handles.
    if let Err(error) = paste::enable() {
        app.warn(format!("paste: {error}"));
    }
    let title_enabled = reloader.settings().boolean("title.enabled").unwrap_or(true);
    let mut title = title::WindowTitle::stdout(title_enabled);
    // Decided once: the terminal's answer about its own title handling does not
    // change while the client runs, and a terminal that has none is left as it
    // was before `title.state` existed.
    let state_supported = title.state_supported();
    if state_supported {
        title::guard_panics();
    }
    if title.enabled()
        && let Err(error) = title.start(&window::label(&app, state_supported))
    {
        app.warn(format!("title: {error}"));
    }
    // Writes nothing until a different state reaches the terminal.
    let mut signals = progress::Signals::stdout();
    // Installed once the modes it restores are on: a signal before this point
    // had nothing to undo. A refusal is reported and the client keeps running.
    if let Err(error) = terminal::guard_signals(state_supported) {
        app.warn(format!("signals: {error}"));
    }
    let (code, failure) = event_loop(
        &mut terminal,
        &mut app,
        Channels {
            title: &mut title,
            signals: &mut signals,
        },
        &runtime.commands,
        reloader,
        &mut themes,
        keys,
    );
    // The tab indicator and the status record come down before the alternate screen, from the
    // writer that knows what it put on the wire; the panic and signal paths go through
    // `terminal::restore`, which has no writer left.
    let signals_error = signals.clear().err();
    // The same sequence the signal path runs: see `terminal::restore`.
    terminal::restore(state_supported);
    if let Some(message) = failure {
        eprintln!("horon-tui: {message}");
    }
    if let Some(error) = signals_error {
        eprintln!("horon-tui: terminal signals: {error}");
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
    // Apply the palette to a copy so reloading can restore the theme exactly.
    app.theme = changes::apply(themes.theme(), ChangePalette::from_settings(settings));
    app.symbols = *themes.symbols();
    let mut status_symbols = app.symbols;
    statusline::apply_appearance(settings, &mut app.theme, &mut status_symbols);
    app.status_separator = Some(status_symbols.separator);
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
    channels: Channels<'_>,
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
    let mut frames = loop_io::Frames::new(app.motion, std::time::Instant::now());
    // Redraw the idle clock only when its minute changes.
    let mut clock = statusline::local_minute();
    // Whether the terminal keeps a title worth adding the state to; asked once,
    // because the answer is the terminal's and does not change while it runs.
    let state_supported = channels.title.state_supported();
    // The state the window title already carries: `title.start` wrote it, and
    // the first frame must not write it again.
    let mut shown_state = window::state(app, state_supported);
    loop {
        let mut changed = match loop_io::drain_events(app) {
            loop_io::Redraw::Immediate => true,
            loop_io::Redraw::None => false,
            loop_io::Redraw::Stream => {
                frames.request_stream();
                false
            }
        };
        // A local command runs on a worker thread; its result joins the same
        // redraw decision as an event from the bridge.
        changed |= crate::commands::shell::drain(app);
        changed |= app.tick_voice();
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
        // Only changed signals reach the terminal.
        let signal = app.terminal_signal();
        if let Err(error) = channels.signals.sync(signal, app.terminal_signals) {
            app.warn(format!("terminal signals: {error}"));
            changed = true;
        }
        // Titles can change on either a daemon event or local input.
        let state = window::state(app, state_supported);
        if channels.title.enabled() && (changed || state != shown_state) {
            shown_state = state;
            let label = window::label(app, state_supported);
            if let Err(error) = channels.title.show(&label) {
                app.warn(format!("title: {error}"));
                changed = true;
            }
        }
        // Reloaders check for changes once per second.
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
        // Reread command files only when their stamps change.
        changed |= app.tick_commands();
        changed |= app.tick_replay();
        if loop_io::sync_mouse_capture(app, &mut mouse_captured) {
            changed = true;
        }
        // The one place a copy reaches standard output: the app decided what it
        // carries, and the terminal is asked here, after the frame is measured and before it is drawn.
        changed |= loop_io::flush_copy(app, &mut std::io::stdout().lock(), clipboard::environment());
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
        // The header has been drawn, so the command may start: after the frame,
        // not inside the key handler, so the target shows before any output.
        crate::commands::shell::start_pending();
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
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Key(key)) => {
                // Windows key releases must not submit another message.
                if key.is_release() {
                    continue;
                }
                if let Some(action) = app.on_key(key) {
                    // Editor actions borrow the terminal owned by this loop.
                    let error = match action {
                        crate::app::Action::EditDraft => {
                            loop_io::hand_over_draft(terminal, app, channels.title, &mut mouse_captured)
                        }
                        crate::app::Action::OpenArtifact(path) => {
                            loop_io::hand_over_artifact(terminal, app, channels.title, &mut mouse_captured, &path)
                        }
                        action => {
                            loop_io::dispatch(app, commands, action);
                            None
                        }
                    };
                    if let Some(message) = error {
                        failure = Some(message);
                        app.running = false;
                        app.exit_code = 1;
                        break;
                    }
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
    let hint = if popup.is_some() {
        None
    } else {
        app.voice_hint().or_else(|| app.completion_hint())
    };
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
    let ghost = app.ghost_suffix();
    let rows: Vec<Line<'_>> = view
        .rows
        .iter()
        .enumerate()
        .map(|(row, text)| {
            let lead = if row == 0 { marker.as_str() } else { padding.as_str() };
            let mut spans = vec![
                Span::styled(lead, Style::default().fg(theme.core.accent)),
                Span::styled(text.as_str(), Style::default().fg(theme.prompt.prompt_text)),
            ];
            if row == view.cursor_row
                && let Some(ghost) = ghost
            {
                let room = text_width.saturating_sub(crate::input::text_cells(text));
                let tail = crate::input::clip_cells_str(ghost, room);
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
    let mut symbols = app.symbols;
    symbols.separator = app.status_separator.unwrap_or(symbols.separator);
    let line = statusline::render(&data, &app.statusline, &app.theme, symbols, width);
    Paragraph::new(line).style(Style::default().bg(app.theme.status.status_bg))
}

#[cfg(test)]
#[path = "picker_app_tests.rs"]
mod picker_app_tests;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
