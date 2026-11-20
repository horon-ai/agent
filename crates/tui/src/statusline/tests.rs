//! Status-line tests: presets, custom arrays, width pressure and options.

use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::widgets::Paragraph;
use time::Time;
use unicode_width::UnicodeWidthStr;

use super::{
    Layout, Preset, SEGMENT_IDS, SegmentId, StatusData, TokenTotals, UsageTotals, apply_appearance, compact, render,
    shorten_path, status_word,
};
use crate::settings::{Reloader, Settings, Sources};
use crate::theme::{SymbolPreset, Symbols, Theme};

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

fn time(hour: u32, minute: u32) -> Time {
    Time::from_hms(hour as u8, minute as u8, 0).expect("valid time")
}

fn sample() -> StatusData<'static> {
    StatusData {
        provider: Some("anthropic"),
        model: Some("claude-sonnet-4"),
        effort: None,
        session_id: Some("abcdef1234567890"),
        pending_turn: false,
        disconnected: false,
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
        // Unknown to the runtime in the sample, so every test that expects a
        // percentage supplies the capacity itself.
        capacity: None,
        usage: None,
        cost: None,
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
    assert_eq!(text, "…/deep");
    assert_eq!(shorten_path("/home/u/projects/deep", Some("/home/u"), 8), "…/deep");
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

/// The theme and the glyphs a settings file resolves to.
fn appearance(text: &str) -> (Theme, Symbols) {
    let mut theme = Theme::default();
    let mut symbols = Symbols::default();
    apply_appearance(&settings_from(text), &mut theme, &mut symbols);
    (theme, symbols)
}

#[test]
fn a_chosen_separator_joins_every_part_of_the_line() {
    let (theme, symbols) = appearance("[statusline]\nseparator = \"bar\"\n");
    let line = render(&sample(), &Layout::default(), &theme, symbols, 100).to_string();
    assert!(
        line.starts_with("idle | anthropic/claude-sonnet-4 | ~/proj"),
        "left side wrong: {line:?}"
    );
    assert!(line.ends_with("↑200 ↓50 | 09:05"), "right side wrong: {line:?}");
    assert!(!line.contains('·'), "the glyph set's mark survived: {line:?}");

    // The model and its effort are parts of the same line, so the chosen mark
    // joins them too.
    let mut data = sample();
    data.effort = Some("high");
    let line = render(&data, &Layout::default(), &theme, symbols, 100).to_string();
    assert!(line.contains("anthropic/claude-sonnet-4 | high"), "{line:?}");

    // `preset` keeps whatever the glyph set draws, whichever set it is.
    let mut ascii = SymbolPreset::Ascii.symbols();
    apply_appearance(
        &settings_from("[statusline]\nseparator = \"preset\"\n"),
        &mut Theme::default(),
        &mut ascii,
    );
    assert_eq!(ascii.separator, "|");
}

#[test]
fn a_chosen_separator_keeps_the_narrow_priority_order() {
    let (theme, symbols) = appearance("[statusline]\nseparator = \"bar\"\n");
    let layout = Layout {
        context_window: 1000,
        ..Layout::default()
    };
    let narrow = render(&sample(), &layout, &theme, symbols, 20).to_string();
    assert!(narrow.contains("anthropic/claude-sonnet-4"), "{narrow:?}");
    assert!(!narrow.contains("09:05"), "the clock should go first: {narrow:?}");
    assert!(!narrow.contains('↑'), "the tokens should go second: {narrow:?}");
    assert!(
        !narrow.contains('|'),
        "a separator was left with nothing beside it: {narrow:?}"
    );
}

#[test]
fn the_background_setting_chooses_the_theme_or_the_terminal() {
    let theme = Theme::default();
    let (themed, _) = appearance("[statusline]\nbackground = \"theme\"\n");
    assert_eq!(themed.status.status_bg, theme.status.status_bg);

    let (unset, _) = appearance("");
    assert_eq!(
        unset.status.status_bg, theme.status.status_bg,
        "the default is the theme's background"
    );

    let (terminal, _) = appearance("[statusline]\nbackground = \"terminal\"\n");
    assert_eq!(terminal.status.status_bg, Color::Reset);
    assert_eq!(
        terminal.status.status_text, theme.status.status_text,
        "the setting changed more than the background"
    );
}

#[test]
fn a_lost_connection_is_named_in_the_state_segment() {
    let layout = Layout {
        left: vec![SegmentId::Status],
        right: Vec::new(),
        ..Layout::default()
    };
    let mut data = StatusData {
        pending_turn: true,
        ..StatusData::default()
    };
    assert_eq!(rendered(&data, &layout, 80), "running");

    data.disconnected = true;
    assert_eq!(rendered(&data, &layout, 80), "disconnected");
}

#[test]
fn the_path_cut_keeps_whole_segments_after_the_marker() {
    let path = "/srv/alpha/beta/gamma";
    // Twelve cells hold the marker and the last two names exactly.
    assert_eq!(shorten_path(path, None, 12), "…/beta/gamma");
    // One cell less drops the parent rather than half of it.
    assert_eq!(shorten_path(path, None, 11), "…/gamma");
    // The home form cuts the same way: the marker stands for the directories
    // left out, never for half a name.
    assert_eq!(shorten_path("/home/u/projects/deep", Some("/home/u"), 14), "…/deep");
}

#[test]
fn a_name_longer_than_the_limit_is_cut_inside_itself() {
    let path = "/srv/a/verylongsegmentname";
    let cut = shorten_path(path, None, 10);
    assert!(cut.starts_with("…/"), "the marker was dropped: {cut:?}");
    assert!(cut.ends_with("tname"), "the leaf's tail was lost: {cut:?}");
    assert_eq!(UnicodeWidthStr::width(cut.as_str()), 10, "{cut:?}");

    // Wide characters are counted as the cells they draw and never split.
    let cjk = shorten_path("/日本語/ディレクトリ", None, 8);
    assert_eq!(cjk, "…/クトリ");
    assert_eq!(UnicodeWidthStr::width(cjk.as_str()), 8, "{cjk:?}");
    let emoji = shorten_path("/a/😀😀😀😀", None, 5);
    assert_eq!(emoji, "…/😀");
    assert!(UnicodeWidthStr::width(emoji.as_str()) <= 5, "{emoji:?}");
}

#[test]
fn the_path_cut_never_passes_the_limit_and_never_panics() {
    let paths = [
        "/",
        "~",
        "/home/u",
        "/home/u/projects/deep",
        "/srv/verylongsegmentnamethatgoesonandon",
        "/日本語/ディレクトリ/さらに深い",
        "/a/😀😀😀😀",
        "relative/name",
        "namewithoutseparator",
    ];
    for path in paths {
        for max in 0..60 {
            let cut = shorten_path(path, Some("/home/u"), max);
            if max == 0 {
                // A zero limit keeps the whole path, home substitution and all.
                assert_eq!(
                    cut,
                    shorten_path(path, Some("/home/u"), usize::MAX),
                    "a zero limit must keep the path whole"
                );
                continue;
            }
            let width = UnicodeWidthStr::width(cut.as_str());
            assert!(width <= max, "`{path}` at {max} drew {width} cells: {cut:?}");
            assert!(!cut.is_empty(), "`{path}` at {max} drew nothing");
        }
    }
    // The degenerate limits still say something.
    assert_eq!(shorten_path("/a/b", None, 1), "…");
    assert_eq!(shorten_path("/a/b", None, 2), "…/");
    assert_eq!(shorten_path("/a/b", None, 4), "/a/b");
    assert_eq!(shorten_path("/a/b", None, 3), "…/b");
    assert_eq!(shorten_path("/", None, 1), "/");
}

#[test]
fn a_lone_name_carries_no_separator_marker() {
    assert_eq!(shorten_path("namewithoutseparator", None, 6), "…rator");
    assert_eq!(shorten_path("~/alpha", None, 6), "…/lpha");
}

#[test]
fn the_context_percentage_follows_the_runtime_window_until_the_setting_overrides_it() {
    let data = StatusData {
        capacity: Some(200_000),
        ..sample()
    };
    let layout = Layout {
        left: Vec::new(),
        right: vec![SegmentId::ContextPct],
        ..Layout::default()
    };
    // 250 tokens of 200 000 rounded down is zero percent, and a zero percent
    // somebody measured is still a reading.
    assert_eq!(rendered(&data, &layout, 40), "0%");

    // The manual override wins over the report: a user who knows the route
    // better than this daemon's catalog does gets the number they typed.
    let override_ = Layout {
        context_window: 1_000,
        ..layout.clone()
    };
    assert_eq!(rendered(&data, &override_, 40), "25%");

    // Neither known draws nothing: an unknown window is not a zero percent.
    let unknown = StatusData {
        capacity: None,
        ..sample()
    };
    assert_eq!(rendered(&unknown, &layout, 40), "");
}

#[test]
fn cumulative_usage_shows_only_the_metrics_the_runtime_recorded() {
    let layout = Layout {
        left: Vec::new(),
        right: vec![SegmentId::Usage],
        ..Layout::default()
    };
    let with_cache = StatusData {
        usage: Some(UsageTotals {
            input: Some(12_345),
            output: Some(678),
            cache_read: Some(4_000),
            cache_creation: None,
        }),
        ..sample()
    };
    assert_eq!(rendered(&with_cache, &layout, 40), "Σ↑12.3k ↓678 ⚡4.0k");

    // A provider that never reported cache accounting gets no cache column,
    // and not a zero one.
    let no_cache = StatusData {
        usage: Some(UsageTotals {
            input: Some(2_000),
            output: None,
            cache_read: None,
            cache_creation: None,
        }),
        ..sample()
    };
    assert_eq!(rendered(&no_cache, &layout, 40), "Σ↑2.0k");

    // Nothing reported draws nothing at all.
    assert_eq!(rendered(&sample(), &layout, 40), "");
}

#[test]
fn a_spend_estimate_is_drawn_only_when_one_was_published() {
    let layout = Layout {
        left: Vec::new(),
        right: vec![SegmentId::Cost],
        ..Layout::default()
    };
    let priced = StatusData {
        cost: Some(1.5),
        ..sample()
    };
    assert_eq!(rendered(&priced, &layout, 40), "$1.50");

    // This build carries no price table, so every report leaves the estimate
    // absent; the segment then hides rather than claiming a free session.
    assert_eq!(rendered(&sample(), &layout, 40), "");
    let nonsense = StatusData {
        cost: Some(f64::NAN),
        ..sample()
    };
    assert_eq!(rendered(&nonsense, &layout, 40), "");
}

#[test]
fn opt_in_segments_are_dropped_before_the_preset_segments_under_width_pressure() {
    let layout = Layout {
        preset: Preset::Custom,
        left: vec![SegmentId::Model],
        right: vec![SegmentId::Time, SegmentId::Usage, SegmentId::Cost],
        ..Layout::default()
    };
    let data = StatusData {
        usage: Some(UsageTotals {
            input: Some(9_000),
            output: Some(1_000),
            cache_read: None,
            cache_creation: None,
        }),
        cost: Some(0.25),
        ..sample()
    };
    let wide = rendered(&data, &layout, 120);
    assert!(wide.contains("Σ↑9.0k"), "the usage vanished: {wide:?}");
    assert!(wide.contains("$0.25"), "the estimate vanished: {wide:?}");

    // The clock goes first, then the estimate, then the usage: the preset's
    // own segments outlive both opt-in ones, which is what makes them opt-in.
    let narrow = rendered(&data, &layout, 20);
    assert!(narrow.starts_with("anthropic/claude-sonnet-4"), "{narrow:?}");
    assert!(!narrow.contains("09:05"), "the clock should go first: {narrow:?}");
    assert!(!narrow.contains('$'), "the estimate should go second: {narrow:?}");
}
