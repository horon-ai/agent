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
    }
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
mod tests {
    use super::*;
    use crate::app::Action;
    use crate::transcript::ToolState;
    use horon_agent_sdk::api::ApiEvent;
    use ratatui::backend::TestBackend;

    fn app_with(socket: Option<std::path::PathBuf>) -> App {
        let (_sender, receiver) = std::sync::mpsc::channel();
        match socket {
            Some(path) => App::new(path.display().to_string(), receiver),
            None => App::new("default socket".to_string(), receiver),
        }
    }

    fn draw_to_string(app: &mut App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => panic!("test backend failed: {error}"),
        };
        if let Err(message) = draw(&mut terminal, app) {
            panic!("draw failed: {message}");
        }
        format!("{}", terminal.backend())
    }

    fn text_event(text: &str) -> ApiEvent {
        ApiEvent::TextDelta {
            session_id: "s1".into(),
            text: text.into(),
            message_id: Some("m1".into()),
        }
    }

    /// The transcript only draws finished items, so a test that wants text on
    /// screen closes the stream the way the harness does.
    fn turn_done() -> ApiEvent {
        ApiEvent::TurnDone {
            session_id: "s1".into(),
            pending_soft_interrupts: None,
        }
    }

    #[test]
    fn args_parse_socket_and_flags() {
        let parsed = Args::parse(["--socket".to_string(), "/tmp/a.sock".to_string()]);
        assert_eq!(
            parsed.as_ref().map(|args| args.socket.clone()),
            Ok(Some("/tmp/a.sock".into()))
        );
        let parsed = Args::parse(["--socket=/tmp/b.sock".to_string(), "--version".to_string()]);
        assert!(parsed.map(|args| args.version).unwrap_or(false));
        assert!(Args::parse(["--nope".to_string()]).is_err());
        assert!(Args::parse(["--socket".to_string()]).is_err());
    }

    #[test]
    fn args_parse_session_and_cwd() {
        let parsed = Args::parse([
            "--session".to_string(),
            "sess-42".to_string(),
            "--cwd=/tmp/work".to_string(),
        ]);
        match parsed {
            Ok(args) => {
                assert_eq!(args.session.as_deref(), Some("sess-42"));
                assert_eq!(args.cwd, Some("/tmp/work".into()));
            }
            Err(message) => panic!("expected a parse: {message}"),
        }
        assert!(Args::parse(["--session".to_string()]).is_err());
        assert!(Args::parse(["--cwd".to_string()]).is_err());
    }

    #[test]
    fn args_parse_repeated_config_files() {
        let parsed = Args::parse([
            "--config".to_string(),
            "/tmp/a.toml".to_string(),
            "--config=/tmp/b.toml".to_string(),
        ]);
        match parsed {
            Ok(args) => assert_eq!(
                args.config,
                vec![
                    std::path::PathBuf::from("/tmp/a.toml"),
                    std::path::PathBuf::from("/tmp/b.toml")
                ]
            ),
            Err(message) => panic!("expected a parse: {message}"),
        }
        assert!(Args::parse(["--config".to_string()]).is_err());
        assert!(Args::parse(["--config=".to_string()]).is_err());
    }

    #[test]
    fn help_and_version_exit_without_connecting() {
        // Both paths return before any connection is attempted, which is what
        // makes them testable in CI where no daemon is running.
        let code = run([
            std::ffi::OsString::from("horon-tui"),
            std::ffi::OsString::from("--help"),
        ]);
        assert_eq!(code, 0);
        let code = run([
            std::ffi::OsString::from("horon-tui"),
            std::ffi::OsString::from("--version"),
        ]);
        assert_eq!(code, 0);
        let code = run([
            std::ffi::OsString::from("horon-tui"),
            std::ffi::OsString::from("--bogus"),
        ]);
        assert_eq!(code, 2);
    }

    #[test]
    fn streams_text_and_finishes_turn() {
        let mut app = app_with(None);
        app.apply_event(&text_event("Hello"));
        app.apply_event(&text_event(", "));
        app.apply_event(&text_event("world"));
        app.apply_event(&ApiEvent::ToolStart {
            session_id: "s1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
        });
        app.apply_event(&ApiEvent::ToolDone {
            session_id: "s1".into(),
            call_id: "c1".into(),
            name: "bash".into(),
            output: "ok".into(),
            error: None,
        });
        app.apply_event(&ApiEvent::TurnDone {
            session_id: "s1".into(),
            pending_soft_interrupts: None,
        });

        let screen = draw_to_string(&mut app);
        assert!(
            screen.contains("Hello, world"),
            "assistant text missing from:\n{screen}"
        );
        assert!(screen.contains("* bash"), "tool line missing from:\n{screen}");
        assert!(!app.pending_turn);
    }

    #[test]
    fn approval_pending_shows_prompt_and_resolves() {
        let mut app = app_with(None);
        app.apply_event(&ApiEvent::ApprovalPending {
            session_id: "s1".into(),
            request_id: "r1".into(),
            action: "bash: rm -rf build".into(),
            expires_at: None,
        });
        let screen = draw_to_string(&mut app);
        assert!(screen.contains("approval needed"), "prompt not drawn:\n{screen}");
        assert!(screen.contains("rm -rf build"), "action missing:\n{screen}");

        let action = app.on_key(key(crossterm::event::KeyCode::Char('y')));
        assert!(
            matches!(action, Some(Action::Approval { approved: true, .. })),
            "y did not answer the prompt: {action:?}"
        );

        app.apply_event(&ApiEvent::ApprovalResolved {
            session_id: "s1".into(),
            request_id: "r1".into(),
            approved: true,
            via: "inline".into(),
            resolved_by: "tool".into(),
        });
        let screen = draw_to_string(&mut app);
        assert!(!screen.contains("approval needed"), "prompt still drawn:\n{screen}");
        assert!(!screen.contains("rm -rf build"), "stale action still drawn:\n{screen}");
    }

    #[test]
    fn daemon_unreachable_shows_error() {
        // A path with no listener is the real failure shape: connect fails
        // without a daemon, and the app must render the sentence instead of
        // panicking, with exit code 1.
        let missing = std::env::temp_dir().join("horon-tui-test-no-such-socket.sock");
        let options = horon_agent_sdk::ConnectOptions {
            socket_path: Some(missing),
            ensure_runtime: false,
            request_timeout: Some(Duration::from_millis(200)),
            ..horon_agent_sdk::ConnectOptions::default()
        };
        let failure = sdk_bridge::connect(options, None, None);
        assert!(failure.is_err(), "connecting to a missing socket must fail");

        let mut app = match failure {
            Ok(_) => panic!("expected a connection failure"),
            Err(message) => App::failure(&message),
        };
        let screen = draw_to_string(&mut app);
        assert!(screen.contains("harness"), "error banner missing:\n{screen}");
        assert_eq!(app.exit_code, 1);
    }

    #[test]
    fn turn_done_ends_the_turn_and_replace_retracts() {
        let mut app = app_with(None);
        app.apply_event(&text_event("draft"));
        app.apply_event(&ApiEvent::TextReplace {
            session_id: "s1".into(),
            text: String::new(),
            message_id: Some("m1".into()),
        });
        app.apply_event(&ApiEvent::TurnDone {
            session_id: "s1".into(),
            pending_soft_interrupts: None,
        });
        let screen = draw_to_string(&mut app);
        assert!(!screen.contains("draft"), "retracted text still drawn:\n{screen}");
    }

    #[test]
    fn tool_failure_is_visible() {
        let mut app = app_with(None);
        app.apply_event(&ApiEvent::ToolStart {
            session_id: "s1".into(),
            call_id: "c9".into(),
            name: "write".into(),
        });
        app.apply_event(&ApiEvent::ToolDone {
            session_id: "s1".into(),
            call_id: "c9".into(),
            name: "write".into(),
            output: String::new(),
            error: Some("permission denied".into()),
        });
        assert_eq!(
            app.transcript.items().last(),
            Some(&Item::Tool {
                name: "write".into(),
                state: ToolState::Failed("permission denied".into())
            })
        );
    }

    #[test]
    fn model_info_reaches_the_status_line() {
        let mut app = app_with(None);
        app.apply_event(&ApiEvent::ModelInfo {
            session_id: "s1".into(),
            provider: Some("anthropic".into()),
            model: Some("claude-sonnet-4".into()),
            reasoning_effort: None,
            auth_method: None,
        });
        let status = app.status_line();
        assert!(status.contains("claude-sonnet-4"), "status was {status}");
        assert!(status.contains("anthropic"), "status was {status}");
    }

    #[test]
    fn statusline_custom_preset_draws_left_then_right() {
        let mut app = app_with(None);
        app.apply_event(&ApiEvent::ModelInfo {
            session_id: "s1".into(),
            provider: Some("anthropic".into()),
            model: Some("claude-sonnet-4".into()),
            reasoning_effort: None,
            auth_method: None,
        });
        app.apply_event(&ApiEvent::TokenUsage {
            session_id: "s1".into(),
            input: 200,
            output: 50,
            cache_read_input: None,
            cache_creation_input: None,
        });
        app.cwd = Some("/home/u/proj".to_string());
        app.home = Some("/home/u".to_string());
        app.statusline = statusline::Layout {
            preset: statusline::Preset::Custom,
            left: vec![statusline::SegmentId::Model, statusline::SegmentId::Path],
            right: vec![statusline::SegmentId::ContextPct, statusline::SegmentId::Time],
            path_max: 40,
            context_window: 1000,
        };

        let screen = draw_to_string(&mut app);
        // `TestBackend`'s Display wraps each row in quotes.
        let row = screen
            .lines()
            .nth(22)
            .unwrap_or_default()
            .trim_matches('"')
            .trim_end()
            .to_string();
        assert!(
            row.starts_with("anthropic/claude-sonnet-4 · ~/proj"),
            "left side wrong: {row:?}\n{screen}"
        );
        assert!(row.contains("25% · "), "context percentage missing: {row:?}");
        let time = row.rsplit(" · ").next().unwrap_or_default();
        assert!(time.len() == 5 && time.as_bytes()[2] == b':', "clock missing: {row:?}");
    }

    #[test]
    fn submit_sends_and_quit_leaves() {
        let mut app = app_with(None);
        for character in "hi".chars() {
            app.on_key(key(crossterm::event::KeyCode::Char(character)));
        }
        assert_eq!(
            app.on_key(key(crossterm::event::KeyCode::Enter)),
            Some(Action::Send("hi".into()))
        );
        assert!(app.pending_turn);
        assert_eq!(app.transcript.items().last(), Some(&Item::User("hi".into())));
        assert!(app.input.text().is_empty());

        for character in "/quit".chars() {
            app.on_key(key(crossterm::event::KeyCode::Char(character)));
        }
        assert_eq!(app.on_key(key(crossterm::event::KeyCode::Enter)), Some(Action::Quit));
    }

    #[test]
    fn ask_prompt_collects_one_answer_per_question() {
        let mut app = app_with(None);
        let questions = vec![
            horon_agent_tool_types::AskQuestion {
                id: Some("scope".into()),
                question: "Which scope?".into(),
                header: None,
                options: vec![
                    horon_agent_tool_types::AskOption {
                        label: "one file".into(),
                        description: None,
                    },
                    horon_agent_tool_types::AskOption {
                        label: "whole repo".into(),
                        description: None,
                    },
                ],
                recommended: Some(0),
                multi: false,
            },
            horon_agent_tool_types::AskQuestion {
                id: Some("style".into()),
                question: "Which style?".into(),
                header: None,
                options: vec![horon_agent_tool_types::AskOption {
                    label: "minimal".into(),
                    description: None,
                }],
                recommended: None,
                multi: false,
            },
        ];
        app.apply_event(&ApiEvent::AskRequested {
            session_id: "s1".into(),
            ask_id: "a1".into(),
            questions,
        });
        let screen = draw_to_string(&mut app);
        assert!(screen.contains("Which scope?"), "question missing:\n{screen}");

        assert_eq!(app.on_key(key(crossterm::event::KeyCode::Char('2'))), None);
        let action = app.on_key(key(crossterm::event::KeyCode::Char('1')));
        match action {
            Some(Action::Ask { ask_id, answers }) => {
                assert_eq!(ask_id, "a1");
                assert_eq!(answers.len(), 2);
                assert_eq!(answers[0].question_id, "scope");
                assert_eq!(answers[0].selected, vec!["whole repo".to_string()]);
                assert_eq!(answers[1].selected, vec!["minimal".to_string()]);
            }
            other => panic!("unexpected action {other:?}"),
        }
    }

    #[test]
    fn ctrl_c_cancels_a_turn_and_ctrl_d_leaves() {
        let mut app = app_with(None);
        app.pending_turn = true;
        let cancel = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        assert_eq!(app.on_key(cancel), Some(Action::Cancel));

        let quit = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        assert_eq!(app.on_key(quit), Some(Action::Quit));
    }

    fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn draw_at(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => panic!("test backend failed: {error}"),
        };
        if let Err(message) = draw(&mut terminal, app) {
            panic!("draw failed: {message}");
        }
        format!("{}", terminal.backend())
    }

    #[test]
    fn a_long_line_keeps_its_tail_visible_in_a_narrow_pane() {
        // One logical line far wider than the pane: the newest text is at its
        // end, so the pane must show the end, not the beginning.
        let mut app = app_with(None);
        let text = format!("{}TAILMARK", "x".repeat(480));
        app.apply_event(&text_event(&text));
        app.apply_event(&turn_done());

        let screen = draw_at(&mut app, 40, 12);
        assert!(
            screen.contains("TAILMARK"),
            "the end of a wrapped line was pushed out of the pane:\n{screen}"
        );
    }

    #[test]
    fn the_newest_message_wins_over_the_oldest_after_wrapping() {
        let mut app = app_with(None);
        app.apply_event(&ApiEvent::TextDelta {
            session_id: "s1".into(),
            text: format!("OLDEST {}", "a".repeat(300)),
            message_id: Some("m-old".into()),
        });
        app.apply_event(&ApiEvent::TextDelta {
            session_id: "s1".into(),
            text: format!("NEWEST {}", "b".repeat(300)),
            message_id: Some("m-new".into()),
        });
        app.apply_event(&turn_done());

        let screen = draw_at(&mut app, 40, 12);
        assert!(screen.contains("NEWEST"), "newest text missing:\n{screen}");
        assert!(!screen.contains("OLDEST"), "the pane kept the oldest rows:\n{screen}");
    }

    #[test]
    fn a_long_input_scrolls_instead_of_leaving_the_pane() {
        let mut app = app_with(None);
        for character in "q".repeat(120).chars() {
            app.on_key(key(crossterm::event::KeyCode::Char(character)));
        }
        let backend = TestBackend::new(40, 12);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => panic!("test backend failed: {error}"),
        };
        if let Err(message) = draw(&mut terminal, &mut app) {
            panic!("draw failed: {message}");
        }
        let position = terminal.backend().cursor_position();
        let (column, row) = (position.x, position.y);
        assert!(
            column < 40,
            "the caret was placed outside the input area: column {column}"
        );
        assert!(column > 0, "the caret collapsed to the left edge: column {column}");
        // The bottom line, under the status line and the transcript pane.
        assert_eq!(row, 11, "the caret left the input row");
    }

    #[test]
    fn a_closed_bridge_ends_the_session_instead_of_spinning() {
        // `app_with(None)` drops the sender, so the channel reports
        // `Disconnected` exactly as it does when the bridge thread exits.
        let mut app = app_with(None);
        assert!(app.running);
        drain_events(&mut app);
        assert!(!app.running, "a closed bridge must stop the loop");
        assert_eq!(app.exit_code, 1);
        match app.banner.as_deref() {
            Some(banner) => assert!(
                banner.contains("bridge"),
                "the banner did not explain the closed bridge: {banner}"
            ),
            None => panic!("no banner for the closed bridge"),
        }
    }
}
