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
pub mod input;
pub mod prompts;
pub mod sdk_bridge;
pub mod settings;
pub mod theme;
pub mod transcript;

use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

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
  /quit          leave (Ctrl-C cancels a running turn, Ctrl-D always leaves)
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
    let working_dir = args.cwd.as_ref().map(|dir| dir.display().to_string());
    match sdk_bridge::connect(
        connect_options(args.socket),
        args.session.as_deref(),
        working_dir.as_deref(),
    ) {
        Ok(runtime) => run_interactive(runtime, &mut reloader),
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
            if let Err(message) = draw(&mut terminal, &mut app) {
                eprintln!("horon-tui: {message}");
            }
            ratatui::restore();
        }
        Err(error) => eprintln!("horon-tui: cannot open a terminal: {error}"),
    }
}

/// Drive the real terminal until the user leaves.
fn run_interactive(runtime: Runtime, reloader: &mut settings::Reloader) -> i32 {
    let mut themes = ThemeLoader::new();
    let mut app = App::new(runtime.description.clone(), runtime.events);
    // The stderr copy of these warnings is wiped by the alternate screen, so
    // the transcript carries them for the person using the interface.
    for warning in reloader.warnings() {
        app.warn(format!("settings: {warning}"));
    }
    for warning in themes.apply(reloader.settings()) {
        app.warn(format!("theme: {warning}"));
    }
    app.theme = themes.theme().clone();
    app.symbols = *themes.symbols();
    install_panic_hook();
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("horon-tui: cannot open a terminal: {error}");
            return 1;
        }
    };
    let code = event_loop(&mut terminal, &mut app, &runtime.commands, reloader, &mut themes);
    ratatui::restore();
    code
}

/// Restore the terminal before the default hook prints a panic message.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        previous(info);
    }));
}

/// The main loop: drain SDK events, redraw, then read at most one keystroke.
fn event_loop<B>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    commands: &std::sync::mpsc::Sender<Command>,
    reloader: &mut settings::Reloader,
    themes: &mut ThemeLoader,
) -> i32
where
    B: Backend,
    B::Error: std::fmt::Display,
{
    loop {
        drain_events(app);
        if !app.running {
            break;
        }
        // The reloader throttles itself to one file check per second; the
        // theme loader shares that cadence for the selected theme file.
        let mut themed = false;
        if let Some(reload) = reloader.tick() {
            for warning in reload.warnings {
                app.warn(format!("settings: {warning}"));
            }
            for warning in themes.apply(&reload.settings) {
                app.warn(format!("theme: {warning}"));
                themed = true;
            }
            themed = true;
        }
        for warning in themes.tick() {
            app.warn(format!("theme: {warning}"));
            themed = true;
        }
        if themed {
            app.theme = themes.theme().clone();
            app.symbols = *themes.symbols();
        }
        if let Err(message) = draw(terminal, app) {
            eprintln!("horon-tui: {message}");
            return 1;
        }
        match crossterm::event::poll(Duration::from_millis(50)) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                app.warn(format!("cannot read the keyboard: {error}"));
                app.running = false;
                app.exit_code = 1;
                continue;
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
            }
            Ok(_) => {}
            Err(error) => {
                app.warn(format!("cannot read the keyboard: {error}"));
                app.running = false;
                app.exit_code = 1;
            }
        }
    }
    app.exit_code
}

/// Move every buffered SDK delivery into the app state.
fn drain_events(app: &mut App) {
    while let Ok(incoming) = app.events.try_recv() {
        match incoming {
            Incoming::Event(event) => app.apply_event(&event),
            Incoming::Notice(message) => app.warn(message),
            Incoming::Closed(message) => app.disconnected(&message),
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
    let theme = app.theme.clone();
    let symbols = app.symbols;
    let prompt_lines = app.prompt_lines();
    let mut constraints = vec![Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)];
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
    frame.render_widget(transcript_widget(app), transcript_area);
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
    frame.render_widget(status_widget(app, &theme, symbols), status_area);
    index += 1;

    let input_area = areas[index];
    let input = Line::from(vec![
        Span::styled(
            format!("{} ", symbols.prompt_marker),
            Style::default().fg(theme.core.accent),
        ),
        Span::styled(
            app.input.text().to_string(),
            Style::default().fg(theme.prompt.prompt_text),
        ),
    ]);
    frame.render_widget(Paragraph::new(input), input_area);
    let cursor_x = input_area
        .x
        .saturating_add(symbols.prompt_marker_width())
        .saturating_add(1)
        .saturating_add(app.input.cursor_column());
    frame.set_cursor_position((cursor_x, input_area.y));
}

/// The bottom line: the attached-to description, the model, and the state,
/// joined by the theme's separator.
fn status_widget(app: &App, theme: &Theme, symbols: Symbols) -> Paragraph<'static> {
    let parts = app.status_parts();
    let last = parts.len().saturating_sub(1);
    let state_style = if app.pending_turn {
        Style::default().fg(theme.core.warning)
    } else {
        Style::default().fg(theme.core.success)
    };
    let mut spans = Vec::new();
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                format!(" {} ", symbols.separator),
                Style::default().fg(theme.core.border),
            ));
        }
        let style = if index == 0 {
            Style::default().fg(theme.status.status_accent)
        } else if index == last {
            state_style
        } else {
            Style::default().fg(theme.status.status_text)
        };
        spans.push(Span::styled(part, style));
    }
    Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.status.status_bg))
}

/// The transcript pane, scrolled to its last lines.
fn transcript_widget(app: &App) -> Paragraph<'static> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for item in app.transcript.items() {
        lines.extend(render_item(item, &app.theme, app.symbols));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Send a message to start. /quit leaves.",
            Style::default().fg(app.theme.core.muted),
        )));
    }
    // Keep the newest lines: the pane has no scrollback yet, and the end of the
    // conversation is the part a user is reading.
    let capacity = app.viewport_height.unwrap_or(24).max(1) as usize;
    if lines.len() > capacity {
        lines.drain(..lines.len() - capacity);
    }
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

fn render_item(item: &Item, theme: &Theme, symbols: Symbols) -> Vec<Line<'static>> {
    match item {
        Item::User(text) => prefixed(
            Span::styled("> ", Style::default().fg(theme.core.accent)),
            text,
            Style::default().fg(theme.transcript.user_text),
        ),
        Item::Assistant(text) => text
            .lines()
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.transcript.assistant_text),
                ))
            })
            .collect(),
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
}
