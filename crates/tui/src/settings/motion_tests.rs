//! Motion presets, the bounded numbers behind them, and the frame scheduler.
//!
//! The tests read a settings snapshot from a temporary file, and the one that
//! needs a real gap between frames waits for the shortest cadence the settings
//! allow rather than for the default.

use std::time::Duration;

use super::{
    DEFAULT_MAX_FPS, DEFAULT_SPINNER_INTERVAL, Motion, MotionMode, launch_overrides, parse_max_fps, parse_mode,
    parse_spinner_interval,
};
use crate::loop_io::Frames;
use crate::settings::{Reloader, Settings, Sources};
use crate::theme::{SymbolPreset, Symbols};

/// The settings a `tui.toml` holding `text` resolves to.
fn settings(text: &str) -> Settings {
    let directory = tempfile::tempdir().expect("temp dir");
    let global = directory.path().join("tui.toml");
    std::fs::write(&global, text).expect("write settings");
    let sources = Sources {
        global,
        project: None,
        configs: Vec::new(),
    };
    Reloader::with_interval(sources, Duration::from_secs(3600))
        .settings()
        .clone()
}

#[test]
fn motion_presets_parse_and_the_default_animates_every_spinner_frame() {
    for name in ["full", "reduced", "static"] {
        let mode = MotionMode::parse(name).unwrap_or_else(|| panic!("missing preset {name}"));
        assert_eq!(mode.as_str(), name);
    }
    assert_eq!(MotionMode::parse("off"), None);
    assert!(parse_mode("slow").is_err());

    let motion = Motion::default();
    assert_eq!(motion.mode, MotionMode::Full);
    assert_eq!(motion.frame_interval(), Some(Duration::from_millis(50)));
    let symbols = Symbols::default();
    let drawn: Vec<_> = (0..symbols.spinner.len())
        .map(|frame| motion.glyph(symbols, frame))
        .collect();
    assert_eq!(
        drawn,
        symbols.spinner.iter().map(|frame| Some(*frame)).collect::<Vec<_>>()
    );
    // A terminal set to ASCII spins with ASCII marks rather than bailing out of
    // the animation or drawing glyphs it cannot show.
    assert!(
        motion
            .glyph(SymbolPreset::Ascii.symbols(), 1)
            .is_some_and(|mark| mark.is_ascii())
    );
}

#[test]
fn a_reduced_preset_freezes_the_spinner_and_static_draws_none() {
    let (reduced, warnings) = Motion::from_settings(&settings("[motion]\npreset = \"reduced\"\n"));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(reduced.mode, MotionMode::Reduced);
    assert_eq!(
        reduced.frame_interval(),
        None,
        "a reduced preset draws no frame of its own"
    );
    let symbols = Symbols::default();
    assert_eq!(reduced.glyph(symbols, 0), symbols.spinner.first().copied());
    assert_eq!(
        reduced.glyph(symbols, 3),
        symbols.spinner.first().copied(),
        "the frozen frame advanced"
    );

    let (frozen, warnings) = Motion::from_settings(&settings("[motion]\npreset = \"static\"\n"));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(frozen.mode, MotionMode::Static);
    assert_eq!(frozen.frame_interval(), None);
    assert_eq!(
        frozen.glyph(Symbols::default(), 0),
        None,
        "a static preset drew a spinner"
    );
}

#[test]
fn an_out_of_range_timing_is_refused_with_a_warning_naming_the_default() {
    let (motion, warnings) = Motion::from_settings(&settings("[motion]\nspinner_interval = 1\nmax_fps = 5000\n"));
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].contains("motion.spinner_interval") && warnings[0].contains(&DEFAULT_SPINNER_INTERVAL.to_string()),
        "{warnings:?}"
    );
    assert!(
        warnings[1].contains("motion.max_fps") && warnings[1].contains(&DEFAULT_MAX_FPS.to_string()),
        "{warnings:?}"
    );
    // The refusals leave the default cadence in force rather than a clamped one.
    assert_eq!(motion.frame_interval(), Motion::default().frame_interval());
}

#[test]
fn the_spinner_interval_is_the_frame_cadence_until_the_limit_is_slower() {
    let (slow, warnings) = Motion::from_settings(&settings("[motion]\nspinner_interval = 400\n"));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(slow.frame_interval(), Some(Duration::from_millis(400)));

    // Two frames a second is slower than a 50 ms spinner, so the limit wins and
    // the spinner keeps turning four times a second instead of twenty.
    let (limited, warnings) = Motion::from_settings(&settings("[motion]\nmax_fps = 4\n"));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(limited.frame_interval(), Some(Duration::from_millis(250)));
}

#[test]
fn launch_flags_outrank_a_file_and_survive_a_reload() {
    let directory = tempfile::tempdir().expect("temp dir");
    let global = directory.path().join("tui.toml");
    std::fs::write(&global, "[motion]\npreset = \"full\"\n").expect("write settings");
    let sources = Sources {
        global: global.clone(),
        project: None,
        configs: Vec::new(),
    };
    let overrides = launch_overrides(Some(MotionMode::Static), Some(120), Some(10));
    let mut reloader = Reloader::with_overrides(sources, Duration::from_secs(3600), overrides);
    assert_eq!(reloader.settings().text("motion.preset"), Some("static"));
    assert_eq!(
        reloader
            .settings()
            .provenance("motion.preset")
            .map(|origin| origin.label())
            .as_deref(),
        Some("command line")
    );

    // A hand edit to the file still reloads, and the flag still wins over it.
    std::fs::write(&global, "[motion]\npreset = \"reduced\"\n").expect("rewrite settings");
    assert!(reloader.check_now().is_some(), "the changed file did not reload");
    assert_eq!(
        reloader.settings().text("motion.preset"),
        Some("static"),
        "a reload dropped the flag"
    );
    assert_eq!(reloader.settings().number("motion.spinner_interval"), Some(120));
    assert_eq!(reloader.settings().number("motion.max_fps"), Some(10));
}

#[test]
fn flag_values_are_bounded_and_reported() {
    assert_eq!(parse_spinner_interval("120"), Ok(120));
    assert!(parse_spinner_interval("39").is_err());
    assert!(parse_spinner_interval("2001").is_err());
    assert!(parse_spinner_interval("soon").is_err());
    assert_eq!(parse_max_fps("60"), Ok(60));
    assert!(parse_max_fps("0").is_err());
    assert!(parse_max_fps("241").is_err());
}

#[test]
fn an_idle_screen_is_never_due_and_a_request_draws_at_once() {
    let mut frames = Frames::new(Motion::default());
    assert_eq!(frames.due(false), Some(0), "the first pass must draw");
    assert_eq!(frames.due(false), None, "an idle app was redrawn with no event");
    assert_eq!(frames.due(false), None);

    frames.request();
    assert_eq!(frames.due(false), Some(1), "a requested frame waited for the cadence");
    assert_eq!(frames.due(false), None);
}

#[test]
fn a_turn_in_flight_draws_on_its_interval_and_advances_the_spinner() {
    // The fastest cadence the settings allow, so the wait stays short.
    let (motion, warnings) = Motion::from_settings(&settings("[motion]\nspinner_interval = 40\nmax_fps = 240\n"));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(motion.frame_interval(), Some(Duration::from_millis(40)));
    let mut frames = Frames::new(motion);
    assert_eq!(frames.due(true), Some(0));
    // The same instant is not due again: the cadence has a floor.
    assert_eq!(frames.due(true), None);
    std::thread::sleep(Duration::from_millis(45));
    assert_eq!(frames.due(true), Some(1), "a busy turn stopped animating");
}

#[test]
fn a_static_preset_asks_for_no_frame_of_its_own() {
    let (motion, _) = Motion::from_settings(&settings("[motion]\npreset = \"static\"\n"));
    let mut frames = Frames::new(motion);
    assert_eq!(frames.due(true), Some(0), "the first pass draws the screen once");
    assert_eq!(frames.due(true), None, "a static preset drew a frame with no event");
    frames.request();
    assert_eq!(
        frames.due(true),
        Some(1),
        "a static preset suppressed the frame an event asked for"
    );
}
