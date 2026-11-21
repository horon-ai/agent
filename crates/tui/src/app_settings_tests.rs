//! Settings that change behavior as they are applied rather than on the next
//! drawing call.
//!
//! `transcript.tool_output_lines` is a number, so a settings file can hold a
//! `-1` that is not a row count. A value that cannot be used falls back to the
//! default instead of reading as "hide the preview", and a usable one is
//! applied as written.
//!
//! The status line's own two choices are checked the same way: a settings
//! snapshot the client resolves, applied through the path a settings save and a
//! file reload both take, changes the row the next frame draws. `models.shortlist`
//! is read from the settings on every refresh, because the cycle reads it on the
//! press. `title.state` follows the same path into the app, where the event loop
//! reads it before it writes the window title.

use std::path::Path;
use std::sync::mpsc;

use horon_agent_sdk::api::ApiEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use crate::app::App;
use crate::settings::{Settings, Sources, layers::LayerSet};
use crate::theme::ThemeLoader;
use crate::tool_line::DEFAULT_OUTPUT_LINES;

/// An app with no connection: `refresh_settings` only reads the settings and
/// the transcript.
fn app() -> App {
    let (_sender, receiver) = mpsc::channel();
    App::new("socket · session".to_string(), receiver)
}

/// The settings a global `tui.toml` holding `text` resolves to.
fn settings_with(dir: &Path, text: &str) -> Settings {
    let global = dir.join("tui.toml");
    std::fs::write(&global, text).expect("write the fixture settings file");
    let sources = Sources {
        global,
        project: None,
        configs: Vec::new(),
    };
    let mut warnings = Vec::new();
    let mut layers = LayerSet::new(&sources);
    layers.refresh(&mut warnings);
    let (settings, resolve_warnings) = layers.resolve(|_: &str| None);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(resolve_warnings.is_empty(), "{resolve_warnings:?}");
    settings
}

#[test]
fn a_negative_tool_output_lines_falls_back_to_the_default() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut app = app();
    app.refresh_settings(&settings_with(dir.path(), "[transcript]\ntool_output_lines = -1\n"));
    assert_eq!(app.transcript.tool_output_lines(), DEFAULT_OUTPUT_LINES);
}

#[test]
fn a_countable_tool_output_lines_is_applied() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut app = app();
    app.refresh_settings(&settings_with(dir.path(), "[transcript]\ntool_output_lines = 6\n"));
    assert_eq!(app.transcript.tool_output_lines(), 6);
}

/// A theme loader reading a directory of its own, so a test never looks at the
/// real `$HORON_HOME`.
fn theme_loader(dir: &Path) -> ThemeLoader {
    ThemeLoader::in_dir(dir.join("themes"))
}

/// The bottom line the app draws, as its cells read, and the background each
/// cell was painted with.
fn drawn_status_row(app: &mut App) -> (String, Vec<Color>) {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test backend");
    crate::draw(&mut terminal, app).expect("draw the frame");
    let buffer = terminal.backend().buffer().clone();
    for y in 0..buffer.area.height {
        let mut text = String::new();
        let mut backgrounds = Vec::new();
        for x in 0..buffer.area.width {
            let Some(cell) = buffer.cell((x, y)) else { continue };
            text.push_str(cell.symbol());
            backgrounds.push(cell.bg);
        }
        if text.contains("claude-sonnet-4") {
            return (text, backgrounds);
        }
    }
    panic!("the status line is not on screen");
}

#[test]
fn a_saved_status_line_style_reaches_the_drawn_bottom_line() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut themes = theme_loader(dir.path());
    let mut app = app();
    app.apply_event(&ApiEvent::ModelInfo {
        session_id: "s1".into(),
        provider: Some("anthropic".into()),
        model: Some("claude-sonnet-4".into()),
        reasoning_effort: None,
        auth_method: None,
    });

    // The defaults keep the glyph set's mark and the theme's background.
    crate::apply_settings(&mut app, &settings_with(dir.path(), ""), &mut themes);
    let (text, backgrounds) = drawn_status_row(&mut app);
    let themed = app.theme.status.status_bg;
    assert_ne!(themed, Color::Reset, "the default paints the theme's colour");
    assert!(text.contains("idle · anthropic/claude-sonnet-4"), "{text:?}");
    assert!(
        backgrounds.iter().all(|background| *background == themed),
        "the theme's colour is not on every cell of the row: {backgrounds:?}"
    );

    // A file edit, resolved again, changes both the mark and the background the
    // next frame draws.
    let styled = settings_with(
        dir.path(),
        "[statusline]\nseparator = \"bar\"\nbackground = \"terminal\"\n",
    );
    crate::apply_settings(&mut app, &styled, &mut themes);
    let (text, backgrounds) = drawn_status_row(&mut app);
    assert!(text.contains("idle | anthropic/claude-sonnet-4"), "{text:?}");
    assert!(!text.contains('·'), "the glyph set's mark survived: {text:?}");
    assert!(
        backgrounds.iter().all(|background| *background == Color::Reset),
        "the line is still painted: {backgrounds:?}"
    );
}

#[test]
fn the_model_shortlist_comes_from_the_settings_file() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut app = app();
    app.refresh_settings(&settings_with(
        dir.path(),
        "[models]\nshortlist = [\"alpha\", \"beta\"]\n",
    ));
    app.show_models(vec!["alpha".to_string(), "beta".to_string()], Some("alpha".to_string()));
    app.model = Some("alpha".to_string());

    // The cycle reads the list the file declared, in its order.
    assert_eq!(
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('p'),
            crossterm::event::KeyModifiers::CONTROL,
        )),
        Some(crate::app::Action::Model("beta".to_string()))
    );
}

#[test]
fn a_repeated_shortlist_entry_warns_once_per_file_edit() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut app = app();
    let settings = settings_with(dir.path(), "[models]\nshortlist = [\"alpha\", \"alpha\"]\n");
    app.refresh_settings(&settings);
    let warnings = app
        .transcript
        .items()
        .iter()
        .filter(|item| matches!(item, crate::transcript::Item::Warning(text) if text.contains("twice")))
        .count();
    assert_eq!(warnings, 1, "the repeated entry was not named once");

    // A second refresh of the same file is a save, not an edit: the warning is
    // not repeated for a file that did not change.
    app.refresh_settings(&settings);
    let warnings = app
        .transcript
        .items()
        .iter()
        .filter(|item| matches!(item, crate::transcript::Item::Warning(text) if text.contains("twice")))
        .count();
    assert_eq!(warnings, 1, "the same file warned again");
}

#[test]
fn the_title_state_setting_reaches_the_app() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut app = app();
    app.refresh_settings(&settings_with(dir.path(), ""));
    assert!(!app.title_state, "the title state is on before anything asked for it");

    app.refresh_settings(&settings_with(dir.path(), "[title]\nstate = true\n"));
    assert!(app.title_state, "the setting did not reach the window title");
}
