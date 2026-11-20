//! `transcript.tool_output_lines` is a number, so a settings file can hold a
//! `-1` that is not a row count. A value that cannot be used falls back to the
//! default instead of reading as "hide the preview", and a usable one is
//! applied as written.

use std::path::Path;
use std::sync::mpsc;

use crate::app::App;
use crate::settings::{Settings, Sources, layers::LayerSet};
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
