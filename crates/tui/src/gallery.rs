//! A developer-only preview of the renderer, drawn from fixed fixtures.
//!
//! The client has rendering tests, but no way to look at the shapes a terminal
//! actually shows: a warning banner, a cut tool line, a parked approval, a
//! message full of double-width characters. `horon-tui --gallery` opens the
//! production renderer on a set of built-in scenes and switches between them
//! with a key, so a layout change can be judged by eye as well as by assertion.
//!
//! The preview is deterministic on purpose. Each scene is rebuilt from an empty
//! [`App`] with every field a fixture sets written down here, the status line is
//! given a custom layout without the clock, and the transcript carries no
//! elapsed times, so the same scene at the same width draws the same frame on
//! every run. That is what makes a frame worth comparing, and it is the only
//! reason the timer segments are left out.
//!
//! Nothing here reaches a daemon: no socket is opened, no session is read and
//! no settings file is loaded. A scene is a plain `App` in the state the
//! renderer expects, which is why the viewer can run on a machine that has
//! never started the harness.

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use crate::USAGE;
use crate::app::App;
use crate::prompts::Prompt;
use crate::settings::Settings;
use crate::statusline::{Layout, Preset, SegmentId, TokenTotals};

/// The scene shown when `--gallery` names none.
pub const FIRST: &str = "conversation";

/// One built-in scene: a name for the command line and the fixture behind it.
pub struct Scene {
    /// The name `--gallery=<name>` accepts.
    pub name: &'static str,
    /// Writes the scene into an app that is otherwise empty.
    fill: fn(&mut App),
}

/// The scenes in the order the gallery walks them.
pub const SCENES: &[Scene] = &[
    Scene {
        name: "conversation",
        fill: conversation,
    },
    Scene {
        name: "tools",
        fill: tools,
    },
    Scene {
        name: "error",
        fill: error,
    },
    Scene {
        name: "prompt",
        fill: prompt,
    },
    Scene {
        name: "unicode",
        fill: unicode,
    },
];

/// The scene names, in gallery order, for an error that names what is on offer.
pub fn names() -> String {
    SCENES.iter().map(|scene| scene.name).collect::<Vec<_>>().join(", ")
}

/// Build the fixture behind `name`, or `None` for an unknown scene.
///
/// The app carries no event source and no settings file: every value the
/// renderer reads is set here, so the frame depends on the scene and the width
/// alone.
pub fn fixture(name: &str) -> Option<App> {
    let index = SCENES.iter().position(|scene| scene.name == name)?;
    let scene = SCENES.get(index)?;
    // The bridge's receiver is kept in the app but never polled: the viewer has
    // no thread to talk to, and dropping the sender keeps it that way.
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("gallery".to_string(), receiver);
    app.settings = Settings::defaults();
    app.transcript.set_show_reasoning(true);
    app.transcript.set_tool_output_lines(8);
    // Fixed rather than the machine's: the path segment shortens against the
    // home directory, and two machines would otherwise draw different rows.
    app.home = Some("/Users/dev".to_string());
    app.cwd = Some("/Users/dev/projects/horon-agent".to_string());
    app.provider = Some("deepseek".to_string());
    app.model = Some("deepseek-chat".to_string());
    app.reasoning_effort = Some("high".to_string());
    app.session_id = Some("01J9GALLERYFIXTURE".to_string());
    app.session_status = Some("idle".to_string());
    app.tokens = Some(TokenTotals {
        input: 18_432,
        output: 1_210,
        cache_read: 12_800,
        cache_creation: 0,
    });
    // The default preset ends with the clock, which no two runs share. The same
    // segments without it keep the line whole and the frame comparable.
    app.statusline = Layout {
        preset: Preset::Custom,
        left: vec![SegmentId::Status, SegmentId::Model, SegmentId::Path],
        right: vec![SegmentId::Tokens, SegmentId::ContextPct, SegmentId::Pending],
        path_max: 32,
        context_window: 200_000,
    };
    app.transcript
        .push_notice(format!("gallery · {} · {}/{}", scene.name, index + 1, SCENES.len()));
    (scene.fill)(&mut app);
    Some(app)
}

/// Render one scene into a headless terminal of the given size.
///
/// Tests compare frames through the same `draw` entry point the viewer uses, so
/// a frame a test pins is the frame a person sees at that width. It is test-only
/// because the viewer draws into a real terminal instead.
#[cfg(test)]
pub fn frame(name: &str, width: u16, height: u16) -> std::io::Result<String> {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let Some(mut app) = fixture(name) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unknown gallery scene: {name}"),
        ));
    };
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).map_err(std::io::Error::other)?;
    crate::draw(&mut terminal, &mut app).map_err(std::io::Error::other)?;
    Ok(format!("{}", terminal.backend()))
}

/// The window title for one scene.
fn scene_label(name: &str) -> String {
    format!("gallery: {name}")
}

/// Show the gallery in the terminal and wait for a key to leave.
///
/// The exit is the client's own: [`crate::terminal::restore`] puts back every
/// mode the alternate screen and raw mode took, so a viewer that ends here
/// leaves the shell the way `/quit` does.
pub fn run(start: &str) -> i32 {
    let Some(mut index) = SCENES.iter().position(|scene| scene.name == start) else {
        eprintln!("horon-tui: unknown gallery scene: {start}");
        eprintln!("horon-tui: scenes: {}", names());
        eprint!("{USAGE}");
        return 2;
    };
    let mut app = match fixture(start) {
        Some(app) => app,
        None => return 2,
    };
    println!(
        "horon-tui gallery: {} scenes; n/p or the arrow keys switch, q leaves.",
        SCENES.len()
    );
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            ratatui::restore();
            eprintln!("horon-tui: cannot open a terminal: {error}");
            return 1;
        }
    };
    // The frame says nothing about which fixture it is, so the window title
    // does. `terminal::restore` pops this entry on the way out, the same pair
    // the interactive client pushes and pops.
    let mut title = crate::title::WindowTitle::stdout();
    if let Err(error) = title.start(&scene_label(SCENES[index].name)) {
        app.warn(format!("title: {error}"));
    }
    let mut code = 0;
    // A frame is drawn when something shows differently, as the client itself
    // does: an idle viewer shares no clock with the screen, so redrawing on
    // every poll would repaint an unchanged frame five times a second.
    let mut redraw = true;
    loop {
        if redraw {
            if let Err(message) = crate::draw(&mut terminal, &mut app) {
                eprintln!("horon-tui: {message}");
                code = 1;
                break;
            }
            redraw = false;
        }
        match crossterm::event::poll(std::time::Duration::from_millis(200)) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                eprintln!("horon-tui: cannot read the keyboard: {error}");
                code = 1;
                break;
            }
        }
        let event = match crossterm::event::read() {
            Ok(event) => event,
            Err(error) => {
                eprintln!("horon-tui: cannot read the keyboard: {error}");
                code = 1;
                break;
            }
        };
        let key = match event {
            crossterm::event::Event::Key(key) => key,
            // The next frame is laid out at the new size; a paste and a mouse
            // report have nothing to act on here.
            crossterm::event::Event::Resize(_, _) => {
                redraw = true;
                continue;
            }
            _ => continue,
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let step = match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyCode::Char('n') | KeyCode::Right | KeyCode::Tab | KeyCode::Enter => 1,
            KeyCode::Char('p') | KeyCode::Left | KeyCode::BackTab => -1,
            _ => 0,
        };
        if step == 0 {
            continue;
        }
        let count = SCENES.len();
        index = if step > 0 {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        // The index was taken modulo the scene count, so it names a scene.
        let scene = &SCENES[index];
        match fixture(scene.name) {
            Some(next) => {
                app = next;
                if let Err(error) = title.show(&scene_label(scene.name)) {
                    app.warn(format!("title: {error}"));
                }
            }
            None => {
                // Unreachable while every scene has a fixture; a viewer that
                // cannot build the next scene leaves rather than freeze on a
                // stale frame.
                code = 1;
                break;
            }
        }
        redraw = true;
    }
    crate::terminal::restore();
    code
}

/// A question, a published thought and a markdown answer.
fn conversation(app: &mut App) {
    app.transcript
        .push_user("Why does the transcript pane wrap before it trims?");
    app.transcript
        .reasoning_delta("The pane keeps the newest rows, so it has to know how many rows a block takes first.");
    app.transcript.reasoning_done();
    app.transcript.push_assistant(
        "## Wrapping is measurement\n\
         \n\
         The pane walks the conversation from the newest item and wraps each\n\
         block against the pane width: one logical line can occupy more rows\n\
         than the pane has.\n\
         \n\
         - a held view skips whole blocks by their measured height\n\
         - the block at the edge is wrapped again for the rows it shows\n\
         \n\
         ```rust\n\
         let source = block_source(block, &look, None, index == 0);\n\
         ```\n",
    );
}

/// Finished, failed and running calls beside a message waiting for the turn.
fn tools(app: &mut App) {
    app.pending_turn = true;
    app.session_status = Some("generating".to_string());
    app.transcript.tool_done(
        "call-read",
        "read",
        Some("fn label(prefix: &str, name: &str) -> String {\n    format!(\"{prefix}: {name}\")\n}\n"),
        None,
    );
    app.transcript.tool_done(
        "call-bash",
        "bash",
        Some("error[E0308]: mismatched types\n --> crates/tui/src/lib.rs:871:9\n  |\n  = note: expected `usize`, found `u16`\n"),
        Some("exit status 101"),
    );
    app.transcript.tool_start("call-grep", "grep");
    app.transcript.tool_input(
        "call-grep",
        "{\"pattern\":\"block_source\",\"path\":\"crates/tui/src\"}",
    );
    app.transcript.push_queued("then run the workspace suite");
}

/// A connection banner over a transcript that stays readable.
fn error(app: &mut App) {
    app.disconnected = true;
    app.banner = Some("cannot reach the harness at /run/user/501/horon/agent.sock: connection refused".to_string());
    app.transcript.tool_done(
        "call-tests",
        "bash",
        Some("test result: FAILED. 128 passed; 1 failed\n"),
        Some("1 test failed"),
    );
    app.transcript
        .push_warning("settings: unknown key `tui.nope` in /Users/dev/.horon-agent/tui.toml");
    app.transcript
        .push_warning("keys: \"transcript.page_up\" is bound to PgUp twice");
    app.transcript
        .push_notice("the conversation stays readable while nothing can be sent; Ctrl-C leaves");
}

/// A parked approval with one more behind it.
fn prompt(app: &mut App) {
    app.prompt = Some(Prompt::approval(
        "req-7".to_string(),
        "bash: cargo publish --dry-run".to_string(),
    ));
    app.waiting_prompts.push_back(Prompt::approval(
        "req-8".to_string(),
        "bash: rm -rf target/debug".to_string(),
    ));
    app.transcript
        .push_user("check what publishing would upload, without uploading");
    app.transcript.push_queued("and then clean the debug profile");
}

/// Double-width text, so a pane that forgets cell widths shows it.
fn unicode(app: &mut App) {
    app.transcript
        .push_user("좁은 터미널에서도 한글·漢字·かな·🙂가 깨지지 않아야 합니다");
    app.transcript.push_assistant(
        "줄바꿈은 글자 수가 아니라 셀 너비로 계산합니다.\n\
         \n\
         全角文字는 두 칸을 차지하고, 이모지 🙂도 두 칸입니다.\n\
         \n\
         긴 경로 하나: /Users/dev/프로젝트/호론-에이전트/crates/tui/src/transcript.rs\n",
    );
    app.transcript
        .push_notice("한 줄 안에서 漢字와 ASCII가 섞여도 줄이 넘치지 않아야 합니다");
}
