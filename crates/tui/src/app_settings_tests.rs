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

/// The glyph the status line draws for a turn in flight, read from the segment
/// itself rather than from the app's private helper.
fn spinner(app: &App) -> Option<&'static str> {
    app.status_data().spinner
}

#[test]
fn motion_full_animates_a_running_turn_and_reduced_freezes_it() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut app = app();
    app.refresh_settings(&settings_with(dir.path(), "[motion]\nspinner_interval = 40\n"));
    app.pending_turn = true;

    app.motion_frame = 0;
    let first = spinner(&app).expect("a full preset draws a spinner");
    app.motion_frame = 1;
    let second = spinner(&app).expect("a full preset draws a spinner");
    assert_ne!(first, second, "the spinner did not advance between frames");

    app.refresh_settings(&settings_with(dir.path(), "[motion]\npreset = \"reduced\"\n"));
    app.motion_frame = 0;
    let frozen = spinner(&app).expect("a reduced preset keeps one frame");
    app.motion_frame = 3;
    assert_eq!(spinner(&app), Some(frozen), "a reduced preset advanced the spinner");
    assert_eq!(frozen, app.symbols.spinner[0]);
}

#[test]
fn motion_static_draws_no_spinner_and_no_idle_frame() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut app = app();
    app.refresh_settings(&settings_with(dir.path(), "[motion]\npreset = \"static\"\n"));
    app.pending_turn = true;
    assert_eq!(spinner(&app), None, "a static preset drew a spinner");

    // A prompt and a lost connection already say more than a turning mark, so
    // even a full preset stops drawing one for them.
    app.refresh_settings(&settings_with(dir.path(), "[motion]\npreset = \"full\"\n"));
    assert!(spinner(&app).is_some());
    app.disconnected = true;
    assert_eq!(spinner(&app), None, "a lost connection kept the spinner");
}

#[test]
fn a_refused_motion_number_is_reported_and_the_default_cadence_stays() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut app = app();
    app.refresh_settings(&settings_with(dir.path(), "[motion]\nmax_fps = 0\n"));
    let warnings: Vec<&str> = app
        .transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            crate::transcript::Item::Warning(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("motion.max_fps"), "{warnings:?}");
    assert_eq!(app.motion, crate::settings::Motion::default());
}
