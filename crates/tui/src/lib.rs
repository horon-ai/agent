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
pub mod input;
pub mod keys;
pub mod overlays;
pub mod prompts;
pub mod sdk_bridge;
pub mod settings;
pub mod statusline;
pub mod theme;
pub mod transcript;
mod wrap;

#[cfg(test)]
#[path = "streaming_tests.rs"]
mod streaming_tests;

use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{Action, App};
use crate::sdk_bridge::{Command, Incoming, Runtime};
use crate::theme::{Symbols, Theme, ThemeLoader};
use crate::transcript::{Item, ToolState};

/// Program usage, printed by `--help` and after an argument error.
pub const USAGE: &str = "\
horon-tui - terminal client for the horon-agent harness

Usage: horon-tui [--socket <path>] [--session <id>] [--cwd <dir>]
                 [--config <file>]...

Options:
  --socket <path>  Harness API socket to connect to (default: the resolved
                   harness socket for this user)
  --session <id>   Attach to an existing session instead of creating one
  --cwd <dir>      Working directory for a newly created session
  --config <file>  Extra settings file, read after the global and project
                   files and before the environment (repeatable)
  -V, --version    Print the version and exit
  -h, --help       Print this help and exit

Keys:
  Enter          send the typed message
  Tab            complete the slash command being typed
  Ctrl-C         cancel a running turn, or leave when nothing is running
  Ctrl-D         leave
  Bindings are remapped in $HORON_HOME/keybindings.toml.

Commands:
  /help          list the commands, including the harness ones and the
                 markdown files under .horon/commands and $HORON_HOME/commands
  /clear         clear the transcript pane
  /model [name]  list the models, or switch to the one named
  /settings      open the settings screen, generated from the registry
  /quit          leave
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
                other => match other.split_once('=') {
                    Some(("--socket" | "--session" | "--cwd" | "--config", "")) => {
                        return Err(format!("{} needs a value", other.trim_end_matches('=')));
                    }
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
pub fn connect_options(socket: Option<std::path::PathBuf>) -> horon_agent_sdk::ConnectOptions {
    horon_agent_sdk::ConnectOptions {
        socket_path: socket,
        client_name: concat!("horon-tui/", env!("CARGO_PKG_VERSION")).to_string(),
        ensure_runtime: true,
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
    let mut reloader = settings::Reloader::new(sources);
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
            ratatui::restore();
            if let Some(message) = failure {
                eprintln!("horon-tui: {message}");
            }
        }
        Err(error) => {
            ratatui::restore();
            eprintln!("horon-tui: cannot open a terminal: {error}");
        }
    }
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
    let (code, failure) = event_loop(&mut terminal, &mut app, &runtime.commands, reloader, &mut themes, keys);
    ratatui::restore();
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
    loop {
        drain_events(app);
        if !app.running {
            break;
        }
        // The reloader throttles itself to one file check per second; the
        // theme loader, the key bindings and the command directories share
        // that cadence for their own files.
        if let Some(reload) = reloader.tick() {
            for warning in reload.warnings {
                app.warn(format!("settings: {warning}"));
            }
            apply_settings(app, &reload.settings, themes);
        }
        let theme_warnings = themes.tick();
        if !theme_warnings.is_empty() {
            for warning in theme_warnings {
                app.warn(format!("theme: {warning}"));
            }
            apply_settings(app, reloader.settings(), themes);
        }
        if let Some(reload) = keys.tick() {
            for warning in reload.warnings {
                app.warn(format!("keys: {warning}"));
            }
            app.bindings = reload.bindings;
        }
        // Command directories are only stat-ed here; a file is re-read when its
        // own stamp moved.
        app.tick_commands();
        if let Err(message) = draw(terminal, app) {
            failure = Some(message);
            break;
        }
        match crossterm::event::poll(Duration::from_millis(50)) {
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
                // Windows always reports a key's release as its own event; acting
                // on both would send every message twice.
                if key.is_release() {
                    continue;
                }
                if let Some(action) = app.on_key(key) {
                    dispatch(app, commands, action);
                }
                flush_setting_change(app, reloader, themes);
            }
            Ok(_) => {}
            Err(error) => {
                failure = Some(format!("cannot read the keyboard: {error}"));
                app.running = false;
                app.exit_code = 1;
            }
        }
    }
    (if failure.is_some() { 1 } else { app.exit_code }, failure)
}

/// Move every buffered SDK delivery into the app state.
///
/// A closed bridge is a real end of the session, not an empty queue: the
/// channel only reports it once, so treating it as "nothing yet" would leave
/// the interface redrawing a dead connection forever.
fn drain_events(app: &mut App) {
    loop {
        match app.events.try_recv() {
            Ok(Incoming::Event(event)) => app.apply_event(&event),
            Ok(Incoming::Notice(message)) => app.warn(message),
            Ok(Incoming::Info(message)) => app.transcript.push_notice(message),
            Ok(Incoming::Closed(message)) => app.disconnected(&message),
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // A more specific banner (an unreachable daemon, say) wins over
                // the generic one, and a session that already ended keeps its
                // own reason.
                if app.running {
                    app.disconnected("the harness bridge stopped");
                }
                break;
            }
        }
    }
}

/// Send one user action to the bridge thread.
fn dispatch(app: &mut App, commands: &std::sync::mpsc::Sender<Command>, action: Action) {
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
        Action::Quit => {
            app.running = false;
            return;
        }
    };
    if let Err(error) = commands.send(command) {
        app.warn(format!("the harness bridge stopped: {error}"));
    }
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

fn render(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    let theme: &Theme = &app.theme;
    let symbols = app.symbols;
    let prompt_lines = app.prompt_lines();
    let hint = app.completion_hint();
    let mut constraints = vec![Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)];
    if hint.is_some() {
        constraints.push(Constraint::Length(1));
    }
    if !prompt_lines.is_empty() {
        constraints.insert(1, Constraint::Length(prompt_lines.len() as u16));
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
    frame.render_widget(transcript_widget(app, transcript_area.width), transcript_area);
    index += 1;

    if !prompt_lines.is_empty() {
        let lines: Vec<Line<'_>> = prompt_lines
            .into_iter()
            .enumerate()
            .map(|(line, text)| {
                // The first line is the prompt itself; a background sets it
                // apart from the rest of the pane.
                let style = if line == 0 {
                    Style::default()
                        .fg(theme.prompt.prompt_border)
                        .bg(theme.prompt.selected_bg)
                } else {
                    Style::default().fg(theme.prompt.prompt_text)
                };
                Line::from(Span::styled(text, style))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), areas[index]);
        index += 1;
    }

    let status_area = areas[index];
    frame.render_widget(status_widget(app, status_area.width), status_area);
    index += 1;

    let input_area = areas[index];
    // The pane is one line high, so the line scrolls under the prompt marker
    // instead of growing: a long message keeps the caret on screen.
    let marker_width = symbols.prompt_marker_width();
    let text_width = input_area.width.saturating_sub(marker_width).saturating_sub(1);
    let (visible, cursor_cell) = app.input.window(text_width);
    let input = Line::from(vec![
        Span::styled(
            format!("{} ", symbols.prompt_marker),
            Style::default().fg(theme.core.accent),
        ),
        Span::styled(visible, Style::default().fg(theme.prompt.prompt_text)),
    ]);
    frame.render_widget(Paragraph::new(input), input_area);
    if let Some(hint) = hint {
        index += 1;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(theme.core.muted)))),
            areas[index],
        );
    }
    // The settings screen covers the conversation rather than the input, so
    // closing it reveals the transcript unchanged and the message being typed
    // stays visible under it.
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
                .saturating_add(cursor_cell);
            if input_area.width > 0 {
                // The caret never paints past the pane, and the terminal cannot
                // place it at the cell after the last column.
                cursor_x = cursor_x.min(input_area.right().saturating_sub(1));
            }
            frame.set_cursor_position((cursor_x, input_area.y));
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

/// The transcript pane, scrolled to its last rows.
///
/// Lines are wrapped here rather than by the widget: the pane keeps the newest
/// rows, and a logical line that wraps over the pane width occupies more than
/// one row. Trimming logical lines first would push the newest rows off the
/// bottom of the pane.
///
/// Items are walked from the newest one so the work is bounded by the pane: an
/// idle frame rebuilds at least twenty times a second, and wrapping the whole
/// conversation each time would cost more as the session grows.
///
/// A message that is still streaming is drawn first as the newest entry, so
/// text appears while it arrives instead of all at once when the turn ends.
/// It is not also in `items`, and `finish_stream` moves it there exactly once.
fn transcript_widget(app: &App, width: u16) -> Paragraph<'static> {
    let capacity = usize::from(app.viewport_height.unwrap_or(24).max(1));
    let mut rows: Vec<Line<'static>> = Vec::new();
    // Reasoning being assembled is drawn above the answer it precedes, the same
    // order the model produced it in.
    if let Some(text) = app.transcript.open_reasoning() {
        rows.extend(wrapped_rows(reasoning_lines(text, &app.theme), width));
    }
    if let Some(text) = app.transcript.open_text() {
        rows.extend(wrapped_rows(assistant_lines(text, &app.theme), width));
    }
    if rows.len() < capacity {
        for item in app.transcript.items().iter().rev() {
            let item_rows = wrapped_rows(render_item(item, &app.theme, app.symbols), width);
            rows.splice(0..0, item_rows);
            if rows.len() >= capacity {
                break;
            }
        }
    }
    if rows.is_empty() {
        rows.extend(wrap::wrap_line(
            &Line::from(Span::styled(
                "Send a message to start. /help lists the commands; /quit leaves.",
                Style::default().fg(app.theme.core.muted),
            )),
            usize::from(width),
        ));
    }
    // Keep the newest rows: the pane has no scrollback yet, and the end of the
    // conversation is the part a user is reading.
    if rows.len() > capacity {
        rows.drain(..rows.len() - capacity);
    }
    Paragraph::new(rows)
}

/// Wrap a rendered item's lines to the pane width, keeping their styles.
fn wrapped_rows(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    let mut rows: Vec<Line<'static>> = Vec::new();
    for line in lines {
        rows.extend(wrap::wrap_line(&line, usize::from(width)));
    }
    rows
}

/// The rows of one assistant message, committed or still streaming.
fn assistant_lines(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    text.lines()
        .map(|line| {
            Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme.transcript.assistant_text),
            ))
        })
        .collect()
}

fn render_item(item: &Item, theme: &Theme, symbols: Symbols) -> Vec<Line<'static>> {
    match item {
        Item::User(text) => prefixed(
            Span::styled("> ", Style::default().fg(theme.core.accent)),
            text,
            Style::default().fg(theme.transcript.user_text),
        ),
        Item::Assistant(text) => assistant_lines(text, theme),
        Item::Tool { name, state } => {
            let (glyph, color) = match state {
                ToolState::Running => (symbols.tool_running, theme.transcript.tool_running),
                ToolState::Done => (symbols.tool_done, theme.transcript.tool_done),
                ToolState::Failed(_) => (symbols.tool_error, theme.transcript.tool_error),
            };
            vec![Line::from(vec![
                Span::raw("* "),
                Span::styled(name.clone(), Style::default().fg(theme.transcript.tool_title)),
                Span::styled(format!(" {glyph} {}", state.label()), Style::default().fg(color)),
            ])]
        }
        Item::Notice(text) => prefixed(
            Span::styled(
                format!("{} ", symbols.bullet),
                Style::default().fg(theme.transcript.notice),
            ),
            text,
            Style::default().fg(theme.transcript.notice),
        ),
        Item::Warning(text) => prefixed(Span::raw("! "), text, Style::default().fg(theme.core.error)),
        // Reasoning is drawn dimmed and indented, and the theme gains no token:
        // a new required token would reject every theme file already written,
        // and the notice color is already the dimmest text the client has.
        Item::Reasoning(text) => reasoning_lines(text, theme),
    }
}

fn reasoning_lines(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    text.lines()
        .map(|line| {
            Line::from(Span::styled(
                format!("  {line}"),
                Style::default()
                    .fg(theme.transcript.notice)
                    .add_modifier(Modifier::ITALIC),
            ))
        })
        .collect()
}

fn prefixed(prefix: Span<'static>, text: &str, style: Style) -> Vec<Line<'static>> {
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                Line::from(vec![prefix.clone(), Span::styled(line.to_string(), style)])
            } else {
                Line::from(Span::styled(format!("  {line}"), style))
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
