//! Settings that change behavior as they are applied rather than on the next
//! drawing call.
//!
//! `transcript.tool_output_lines` is a number, so a settings file can hold a
//! `-1` that is not a row count. A value that cannot be used falls back to the
//! default instead of reading as "hide the preview", and a usable one is
//! applied as written. `title.state` follows the same path into the app, where
//! the event loop reads it before it writes the window title.

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
fn the_title_state_setting_reaches_the_app() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut app = app();
    app.refresh_settings(&settings_with(dir.path(), ""));
    assert!(!app.title_state, "the title state is on before anything asked for it");

    app.refresh_settings(&settings_with(dir.path(), "[title]\nstate = true\n"));
    assert!(app.title_state, "the setting did not reach the window title");
}
