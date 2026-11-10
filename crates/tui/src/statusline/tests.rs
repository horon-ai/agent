//! Status-line tests: presets, custom arrays, width pressure and options.

use std::time::Duration;

use chrono::NaiveTime;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::widgets::Paragraph;

use super::{
    Layout, Preset, SEGMENT_IDS, SegmentId, StatusData, TokenTotals, compact, render, shorten_path, status_word,
};
use crate::settings::{Reloader, Settings, Sources};
use crate::theme::{Symbols, Theme};

fn settings_from(text: &str) -> Settings {
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

fn time(hour: u32, minute: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(hour, minute, 0).expect("valid time")
}

fn sample() -> StatusData<'static> {
    StatusData {
        provider: Some("anthropic"),
        model: Some("claude-sonnet-4"),
        session_id: Some("abcdef1234567890"),
        pending_turn: false,
        session_status: Some("idle"),
        prompt: None,
        cwd: Some("/home/u/proj"),
        home: Some("/home/u"),
        tokens: Some(TokenTotals {
            input: 200,
            output: 50,
            cache_read: 0,
            cache_creation: 0,
        }),
        now: Some(time(9, 5)),
    }
}

fn draw_line(line: ratatui::text::Line<'static>, width: u16) -> String {
    let backend = TestBackend::new(width, 1);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(line), frame.area()))
        .expect("draw");
    // `TestBackend`'s Display wraps each row in quotes; the cells are what the
    // client actually draws.
    let view = format!("{}", terminal.backend());
    view.trim_end().trim_matches('"').trim_end().to_string()
}

fn rendered(data: &StatusData<'_>, layout: &Layout, width: u16) -> String {
    render(data, layout, &Theme::default(), Symbols::default(), width).to_string()
}

#[test]
fn statusline_default_preset_orders_left_then_right() {
    let layout = Layout::default();
    assert_eq!(layout.preset, Preset::Default);
    assert_eq!(layout.left, vec![SegmentId::Status, SegmentId::Model, SegmentId::Path]);
    assert_eq!(
        layout.right,
        vec![SegmentId::Tokens, SegmentId::ContextPct, SegmentId::Time]
    );

    let line = rendered(&sample(), &layout, 100);
    assert!(
        line.starts_with("idle · anthropic/claude-sonnet-4 · ~/proj"),
        "left side wrong: {line:?}"
    );
    assert!(line.ends_with("↑200 ↓50 · 09:05"), "right side wrong: {line:?}");
    assert_eq!(line.chars().count(), 100, "line does not fill its width: {line:?}");
}

#[test]
fn statusline_minimal_preset_keeps_model_and_status() {
    let (layout, warnings) = Layout::from_settings(&settings_from("[statusline]\npreset = \"minimal\"\n"));
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(layout.preset, Preset::Minimal);
    assert_eq!(layout.left, vec![SegmentId::Model]);
    assert_eq!(layout.right, vec![SegmentId::Status]);

    let line = rendered(&sample(), &layout, 80);
    assert!(line.starts_with("anthropic/claude-sonnet-4"), "{line:?}");
    assert!(line.ends_with("idle"), "{line:?}");
    assert_eq!(line.chars().count(), 80, "line does not fill its width: {line:?}");
}

#[test]
fn statusline_custom_preset_renders_left_then_right_in_order() {
    let (layout, warnings) = Layout::from_settings(&settings_from(
        "[statusline]\npreset = \"custom\"\nleft = [\"model\", \"path\"]\nright = [\"context_pct\", \"time\"]\ncontext_window = 1000\n",
    ));
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(layout.preset, Preset::Custom);
    assert_eq!(layout.left, vec![SegmentId::Model, SegmentId::Path]);
    assert_eq!(layout.right, vec![SegmentId::ContextPct, SegmentId::Time]);

    let line = render(&sample(), &layout, &Theme::default(), Symbols::default(), 80);
    let text = line.to_string();
    assert!(
        text.starts_with("anthropic/claude-sonnet-4 · ~/proj"),
        "left side wrong: {text:?}"
    );
    assert!(text.ends_with("25% · 09:05"), "right side wrong: {text:?}");

    // The same line through a real backend, which is what the client draws.
    let drawn = draw_line(line, 80);
    assert!(drawn.starts_with("anthropic/claude-sonnet-4 · ~/proj"), "{drawn:?}");
    assert!(drawn.ends_with("25% · 09:05"), "{drawn:?}");
}

#[test]
fn statusline_unknown_segment_warns_and_skips_only_itself() {
    let (layout, warnings) = Layout::from_settings(&settings_from(
        "[statusline]\npreset = \"custom\"\nleft = [\"model\", \"foo\", \"path\"]\nright = [\"time\"]\n",
    ));
    assert_eq!(warnings.len(), 1, "expected one warning: {warnings:?}");
    assert!(
        warnings[0].contains("\"foo\""),
        "warning does not name the segment: {warnings:?}"
    );
    assert!(
        warnings[0].contains("statusline.left"),
        "warning does not name the setting: {warnings:?}"
    );
    assert_eq!(layout.left, vec![SegmentId::Model, SegmentId::Path]);

    let text = rendered(&sample(), &layout, 80);
    assert!(text.contains("anthropic/claude-sonnet-4"), "{text:?}");
    assert!(text.contains("~/proj"), "{text:?}");
    assert!(text.ends_with("09:05"), "{text:?}");
}

#[test]
fn statusline_narrow_width_drops_lowest_priority_first() {
    let layout = Layout {
        context_window: 1000,
        ..Layout::default()
    };
    let full = rendered(&sample(), &layout, 200);
    assert!(full.contains("09:05"), "full line lost the clock: {full:?}");

    // 20 columns cannot hold status, model, path, tokens, percent and clock.
    let narrow = rendered(&sample(), &layout, 20);
    assert!(narrow.contains("anthropic/claude-sonnet-4"), "{narrow:?}");
    assert!(!narrow.contains("09:05"), "clock should go first: {narrow:?}");
    assert!(!narrow.contains('↑'), "tokens should go second: {narrow:?}");
}

#[test]
fn statusline_narrow_width_keeps_only_the_highest_priority() {
    let layout = Layout::default();
    let text = rendered(&sample(), &layout, 8);
    assert_eq!(text, "anthropic/claude-sonnet-4");
}

#[test]
fn statusline_zero_width_draws_nothing() {
    let layout = Layout::default();
    assert_eq!(rendered(&sample(), &layout, 0), "");
}

#[test]
fn statusline_empty_segments_are_hidden() {
    let data = StatusData {
        session_status: Some("idle"),
        ..StatusData::default()
    };
    let layout = Layout::default();
    let text = rendered(&data, &layout, 80);
    assert_eq!(text, "idle");
    assert!(!text.contains('·'), "empty segments left a separator: {text:?}");
}

#[test]
fn statusline_path_option_shortens_the_directory() {
    let (layout, warnings) = Layout::from_settings(&settings_from("[statusline]\npath_max = 8\n"));
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(layout.path_max, 8);

    let data = StatusData {
        cwd: Some("/home/u/projects/deep"),
        home: Some("/home/u"),
        ..StatusData::default()
    };
    let only_path = Layout {
        left: vec![SegmentId::Path],
        right: Vec::new(),
        ..layout
    };
    let text = rendered(&data, &only_path, 80);
    assert_eq!(text, "…ts/deep");
    assert_eq!(shorten_path("/home/u/projects/deep", Some("/home/u"), 8), "…ts/deep");
    assert_eq!(shorten_path("/home/u", Some("/home/u"), 40), "~");
    assert_eq!(shorten_path("/other/dir", Some("/home/u"), 0), "/other/dir");
}

#[test]
fn statusline_segment_helpers_format_counts_and_state() {
    assert_eq!(compact(999), "999");
    assert_eq!(compact(1_000), "1.0k");
    assert_eq!(compact(12_345), "12.3k");
    assert_eq!(compact(2_500_000), "2.5m");

    let mut data = sample();
    data.pending_turn = true;
    assert_eq!(status_word(&data), "running");
    data.pending_turn = false;
    data.session_status = Some("generating");
    assert_eq!(status_word(&data), "running");
    data.prompt = Some("approval needed");
    assert_eq!(status_word(&data), "requires_action");

    let layout = Layout {
        right: vec![
            SegmentId::Tokens,
            SegmentId::ContextPct,
            SegmentId::Time,
            SegmentId::Pending,
        ],
        context_window: 1000,
        ..Layout::default()
    };
    let text = rendered(&data, &layout, 120);
    assert!(text.starts_with("requires_action"), "{text:?}");
    assert!(text.ends_with("approval needed"), "{text:?}");
}

#[test]
fn statusline_session_segment_shortens_ids() {
    let layout = Layout {
        left: vec![SegmentId::Session],
        right: Vec::new(),
        ..Layout::default()
    };
    let text = rendered(&sample(), &layout, 80);
    assert_eq!(text, "abcdef12");

    let mut data = sample();
    data.session_id = Some("s1");
    assert_eq!(rendered(&data, &layout, 80), "s1");
}

#[test]
fn statusline_catalogue_round_trips() {
    for id in SEGMENT_IDS {
        let segment = SegmentId::parse(id).unwrap_or_else(|| panic!("missing segment {id}"));
        assert_eq!(segment.as_str(), *id);
        assert!(segment.priority() > 0, "no priority for {id}");
    }
    assert!(SegmentId::parse("foo").is_none());
    for name in ["default", "minimal", "custom"] {
        let preset = Preset::parse(name).unwrap_or_else(|| panic!("missing preset {name}"));
        assert_eq!(preset.as_str(), name);
    }
    assert!(Preset::parse("bogus").is_none());
}
