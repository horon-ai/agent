//! Tests for theme files, colour resolution, glyph presets and live reload.
//!
//! Every test builds its own temporary themes directory, so no test reads the
//! real `$HORON_HOME`, and reload tests call `check_now` instead of sleeping.

use std::path::{Path, PathBuf};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::text::Line;

use super::*;
use crate::app::App;
use crate::settings::layers::LayerSet;
use crate::settings::{Settings, Sources};
use crate::tool_line::{self, OutputHead};
use crate::transcript::{ToolCall, ToolState};

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temporary directory"),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn themes(&self) -> PathBuf {
        self.root().join("themes")
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.themes().join(name);
        std::fs::create_dir_all(self.themes()).expect("create the themes directory");
        std::fs::write(&path, text).expect("write the theme file");
        path
    }

    fn loader(&self) -> ThemeLoader {
        ThemeLoader::in_dir(self.themes())
    }

    fn settings(&self, text: &str) -> Settings {
        let path = self.root().join("tui.toml");
        std::fs::write(&path, text).expect("write the settings file");
        let sources = Sources {
            global: path,
            project: None,
            configs: Vec::new(),
        };
        let mut layers = LayerSet::new(&sources);
        let mut warnings = Vec::new();
        layers.refresh(&mut warnings);
        let (settings, _) = layers.resolve(|_: &str| None);
        settings
    }
}

/// A complete theme document, with `accent` as the only variable.
fn theme_json(name: &str, accent: &str) -> String {
    format!(
        r##"{{
  "name": "{name}",
  "colors": {{
    "accent": "{accent}",
    "border": "",
    "muted": 240,
    "text": "#010203",
    "success": "#00ff00",
    "warning": "#ffaa00",
    "error": "#ff0000",
    "user_text": "#111111",
    "assistant_text": "#222222",
    "tool_title": "#333333",
    "tool_running": "#444444",
    "tool_done": "#555555",
    "tool_error": "#666666",
    "notice": "#777777",
    "prompt_border": "#888888",
    "prompt_text": "#999999",
    "selected_bg": "#aaaaaa",
    "status_bg": "#bbbbbb",
    "status_text": "#cccccc",
    "status_accent": "#dddddd"
  }}
}}"##
    )
}

fn cell_fg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
    match terminal.backend().buffer().cell((x, y)) {
        Some(cell) => cell.fg,
        None => panic!("no cell at ({x}, {y})"),
    }
}

#[test]
fn theme_builtin_dark_and_light_load() {
    let dark = parse(schema::BUILTIN_DARK, "built-in dark")
        .unwrap_or_else(|error| panic!("built-in dark must parse: {error}"));
    let light = parse(schema::BUILTIN_LIGHT, "built-in light")
        .unwrap_or_else(|error| panic!("built-in light must parse: {error}"));
    assert_eq!(dark.name, "dark");
    assert_eq!(light.name, "light");
    assert_ne!(dark.core.text, light.core.text);
    assert_eq!(dark.symbols, Some(SymbolPreset::Unicode));
    assert_eq!(light.symbols, Some(SymbolPreset::Unicode));
    assert!(builtin_source("dark").is_some());
    assert!(builtin_source("light").is_some());
    assert!(builtin_source("mine").is_none());
}

#[test]
fn theme_user_file_overrides_lookup() {
    let fixture = Fixture::new();
    let path = fixture.write("mine.json", &theme_json("mine", "#123456"));
    let mut loader = fixture.loader();

    let warnings = loader.apply_choice("mine", None);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(loader.name(), "mine");
    assert_eq!(loader.theme().core.accent, Color::Rgb(0x12, 0x34, 0x56));
    assert_eq!(loader.source_path(), Some(path.as_path()));

    // A built-in name still reads the built-in document.
    let warnings = loader.apply_choice("dark", None);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(loader.name(), "dark");
    assert_eq!(loader.source_path(), None);
}

#[test]
fn theme_user_file_colour_reaches_a_cell() {
    let fixture = Fixture::new();
    fixture.write("mine.json", &theme_json("mine", "#123456"));
    let mut loader = fixture.loader();
    assert!(loader.apply_choice("mine", None).is_empty());

    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut app = App::new("socket".to_string(), receiver);
    app.theme = loader.theme().clone();
    app.symbols = *loader.symbols();
    let mut terminal =
        Terminal::new(TestBackend::new(80, 24)).unwrap_or_else(|error| panic!("test backend failed: {error}"));
    crate::draw(&mut terminal, &mut app).unwrap_or_else(|message| panic!("draw failed: {message}"));
    // The prompt marker is drawn in the accent colour.
    assert_eq!(cell_fg(&terminal, 0, 23), Color::Rgb(0x12, 0x34, 0x56));
}

#[test]
fn theme_missing_token_keeps_the_previous_theme() {
    let fixture = Fixture::new();
    let broken = theme_json("mine", "#123456").replace("\"accent\": \"#123456\",", "");
    fixture.write("mine.json", &broken);
    let mut loader = fixture.loader();
    let before = loader.theme().core.accent;

    let warnings = loader.apply_choice("mine", None);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("core"), "{warnings:?}");
    assert!(warnings[0].contains("accent"), "{warnings:?}");
    assert!(warnings[0].contains("keeping `dark`"), "{warnings:?}");
    assert_eq!(loader.name(), "dark");
    assert_eq!(loader.theme().core.accent, before);
}

#[test]
fn theme_colour_values_resolve_hex_index_var_and_default() {
    let text = r##"{
      "name": "mixed",
      "vars": { "ink": "#0a0b0c", "dim": "" },
      "colors": {
        "accent": "ink",
        "border": 240,
        "muted": "dim",
        "text": "#FfEeDd",
        "success": "", "warning": 0, "error": "ink",
        "user_text": "", "assistant_text": "", "tool_title": "",
        "tool_running": "", "tool_done": "", "tool_error": "", "notice": "",
        "prompt_border": "", "prompt_text": "", "selected_bg": "",
        "status_bg": "", "status_text": "", "status_accent": ""
      }
    }"##;
    let theme = parse(text, "mixed.json").unwrap_or_else(|error| panic!("theme must parse: {error}"));
    assert_eq!(theme.core.accent, Color::Rgb(0x0a, 0x0b, 0x0c));
    assert_eq!(theme.core.border, Color::Indexed(240));
    assert_eq!(theme.core.muted, Color::Reset);
    assert_eq!(theme.core.text, Color::Rgb(0xff, 0xee, 0xdd));
    assert_eq!(theme.core.success, Color::Reset);
    assert_eq!(theme.core.warning, Color::Indexed(0));
}

#[test]
fn theme_bad_hex_and_index_are_rejected() {
    let bad_hex = theme_json("bad", "#12345");
    let error = parse(&bad_hex, "bad.json")
        .err()
        .unwrap_or_else(|| panic!("a short hex colour must be rejected"));
    assert!(error.contains("#12345"), "{error}");

    let bad_index = theme_json("bad", "0").replace("\"accent\": \"0\",", "\"accent\": 300,");
    let error = parse(&bad_index, "bad.json")
        .err()
        .unwrap_or_else(|| panic!("an index above 255 must be rejected"));
    assert!(error.contains("0-255"), "{error}");

    let unknown_var = theme_json("bad", "not_a_var");
    let error = parse(&unknown_var, "bad.json")
        .err()
        .unwrap_or_else(|| panic!("an undeclared var must be rejected"));
    assert!(error.contains("not_a_var"), "{error}");
    assert!(error.contains("declared var"), "{error}");
}

#[test]
fn theme_unknown_symbol_preset_is_rejected() {
    let text = theme_json("bad", "#123456").replace("  }\n}", "  },\n  \"symbols\": { \"preset\": \"emoji\" }\n}");
    let error = parse(&text, "bad.json")
        .err()
        .unwrap_or_else(|| panic!("an unknown preset must be rejected"));
    assert!(error.contains("symbols"), "{error}");
    assert!(error.contains("emoji"), "{error}");
}

#[test]
fn theme_symbol_presets_follow_precedence() {
    let fixture = Fixture::new();
    let text = theme_json("mine", "#123456").replace("  }\n}", "  },\n  \"symbols\": { \"preset\": \"nerd\" }\n}");
    fixture.write("mine.json", &text);
    let mut loader = fixture.loader();

    // The theme's own preset wins over the registry default.
    let settings = fixture.settings("theme = \"mine\"\n");
    assert_eq!(
        settings.provenance("symbols"),
        Some(&crate::settings::Provenance::Default)
    );
    assert!(loader.apply(&settings).is_empty());
    assert_eq!(*loader.symbols(), SymbolPreset::Nerd.symbols());

    // A setting from a real layer wins over the theme.
    let settings = fixture.settings("theme = \"mine\"\nsymbols = \"ascii\"\n");
    assert!(loader.apply(&settings).is_empty());
    assert_eq!(*loader.symbols(), SymbolPreset::Ascii.symbols());

    // An explicit choice wins over both.
    assert!(loader.apply_choice("mine", Some(SymbolPreset::Unicode)).is_empty());
    assert_eq!(*loader.symbols(), SymbolPreset::Unicode.symbols());
}

#[test]
fn theme_symbol_presets_are_distinct_and_ascii_is_plain() {
    let ascii = SymbolPreset::Ascii.symbols();
    for mark in [
        ascii.bullet,
        ascii.tool_running,
        ascii.tool_done,
        ascii.tool_error,
        ascii.tool_lead,
        ascii.prompt_marker,
        ascii.more_below,
        ascii.separator,
        ascii.quote,
        ascii.rule,
        ascii.bar_full,
        ascii.bar_empty,
        ascii.spinner[0],
        ascii.spinner[1],
    ] {
        assert!(mark.is_ascii(), "{mark:?} is not ASCII");
    }
    let unicode = SymbolPreset::Unicode.symbols();
    assert_ne!(unicode.bullet, ascii.bullet);
    assert_ne!(unicode.separator, ascii.separator);
    assert_eq!(SymbolPreset::parse("nerd"), Some(SymbolPreset::Nerd));
    assert_eq!(SymbolPreset::parse("emoji"), None);
    assert_eq!(SymbolPreset::Nerd.as_str(), "nerd");
    assert_eq!(
        Symbols::default(),
        SymbolPreset::Unicode.symbols(),
        "the default glyphs are the Unicode preset"
    );
}

#[test]
fn theme_builtin_name_collision_warns_and_builtin_wins() {
    let fixture = Fixture::new();
    let path = fixture.write("dark.json", &theme_json("dark", "#abcdef"));
    let mut loader = fixture.loader();
    let builtin = parse(schema::BUILTIN_DARK, "built-in dark")
        .unwrap_or_else(|error| panic!("built-in dark must parse: {error}"));

    let warnings = loader.apply_choice("dark", None);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("ignored"), "{warnings:?}");
    let path_text = path.display().to_string();
    assert!(warnings[0].contains(&path_text), "{warnings:?}");
    assert_eq!(loader.theme().core.accent, builtin.core.accent);
}

#[test]
fn theme_missing_file_warns_then_loads_when_created() {
    let fixture = Fixture::new();
    let mut loader = fixture.loader();
    let warnings = loader.apply_choice("mine", None);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("not applied"), "{warnings:?}");
    assert!(warnings[0].contains("keeping `dark`"), "{warnings:?}");

    fixture.write("mine.json", &theme_json("mine", "#0f0f0f"));
    let warnings = loader.check_now();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(loader.name(), "mine");
    assert_eq!(loader.theme().core.accent, Color::Rgb(0x0f, 0x0f, 0x0f));
}

#[test]
fn theme_file_edit_reloads_and_a_broken_edit_keeps_the_last_theme() {
    let fixture = Fixture::new();
    fixture.write("mine.json", &theme_json("mine", "#101010"));
    let mut loader = fixture.loader();
    assert!(loader.apply_choice("mine", None).is_empty());
    assert_eq!(loader.theme().core.accent, Color::Rgb(0x10, 0x10, 0x10));

    fixture.write("mine.json", &format!("{}\n", theme_json("mine", "#202020")));
    let warnings = loader.check_now();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(loader.theme().core.accent, Color::Rgb(0x20, 0x20, 0x20));

    fixture.write("mine.json", "{ not json");
    let warnings = loader.check_now();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("not applied"), "{warnings:?}");
    assert_eq!(loader.theme().core.accent, Color::Rgb(0x20, 0x20, 0x20));
}

#[test]
fn theme_default_settings_select_dark() {
    let fixture = Fixture::new();
    let mut loader = fixture.loader();
    assert!(loader.apply(&Settings::defaults()).is_empty());
    assert_eq!(loader.name(), "dark");
    assert_eq!(loader.theme().symbols, Some(SymbolPreset::Unicode));
}

/// WCAG relative luminance, measured here rather than trusted from the palette
/// constants: the test is what makes those numbers true.
fn luminance(color: Color) -> f64 {
    let Color::Rgb(red, green, blue) = color else {
        panic!("{color:?} is not an RGB colour");
    };
    let channel = |value: u8| {
        let value = f64::from(value) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
}

/// Contrast of two colours as a ratio, the way WCAG states it.
fn contrast(first: Color, second: Color) -> f64 {
    let (one, other) = (luminance(first), luminance(second));
    let (high, low) = if one >= other { (one, other) } else { (other, one) };
    (high + 0.05) / (low + 0.05)
}

/// A colour's OKLCH lightness, chroma and hue in degrees.
///
/// Hue separation is measured in OKLCH because that is the space in which two
/// hues of equal distance look equally different, which is what a
/// colour-vision-friendly pair has to be checked against.
fn oklch(color: Color) -> (f64, f64, f64) {
    let Color::Rgb(red, green, blue) = color else {
        panic!("{color:?} is not an RGB colour");
    };
    let channel = |value: u8| {
        let value = f64::from(value) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let linear = |value: f64| value.cbrt();
    let (red, green, blue) = (channel(red), channel(green), channel(blue));
    let long = linear(0.412_221_470_8 * red + 0.536_332_536_3 * green + 0.051_445_992_9 * blue);
    let medium = linear(0.211_903_498_2 * red + 0.680_699_545_1 * green + 0.107_396_956_6 * blue);
    let short = linear(0.088_302_461_9 * red + 0.281_718_837_6 * green + 0.629_978_700_5 * blue);
    let lightness = 0.210_454_255_3 * long + 0.793_617_785_0 * medium - 0.004_072_046_8 * short;
    let a = 1.977_998_495_1 * long - 2.428_592_205_0 * medium + 0.450_593_709_9 * short;
    let b = 0.025_904_037_1 * long + 0.782_771_766_2 * medium - 0.808_675_766_0 * short;
    (lightness, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0))
}

/// The smaller angle between two colours' hues, in degrees.
fn hue_gap(first: Color, second: Color) -> f64 {
    let gap = (oklch(first).2 - oklch(second).2).abs();
    gap.min(360.0 - gap)
}

fn builtin(name: &str) -> Theme {
    let text = builtin_source(name).expect("a built-in theme");
    parse(text, &format!("built-in theme `{name}`")).expect("the built-in theme parses")
}

/// A finished tool call whose output is exactly `rows`, with `hidden` rows the
/// preview does not keep.
fn call_with_output(rows: &[&str], hidden: usize) -> ToolCall {
    ToolCall {
        call_id: "c1".to_string(),
        name: "edit".to_string(),
        state: ToolState::Done,
        input: r#"{"path":"src/main.rs"}"#.to_string(),
        duration_ms: Some(12),
        output: Some(OutputHead {
            rows: rows.iter().map(|row| (*row).to_string()).collect(),
            total_rows: rows.len() + hidden,
        }),
    }
}

/// A tool call whose output is a unified diff, as `edit` reports one.
fn diff_call() -> ToolCall {
    // Two more rows than the preview keeps, so the count row is drawn too: it
    // starts with a sign and must not read as an added line.
    call_with_output(
        &[
            "--- a/src/main.rs",
            "+++ b/src/main.rs",
            "@@ -1,2 +1,3 @@",
            "-old",
            "+new",
            " unchanged",
        ],
        2,
    )
}

fn drawn_row(line: &Line<'_>) -> String {
    line.spans.iter().map(|span| span.content.as_ref()).collect()
}

fn row_color(line: &Line<'_>) -> Option<Color> {
    line.spans.first().and_then(|span| span.style.fg)
}

#[test]
fn theme_changes_setting_selects_the_palette() {
    let fixture = Fixture::new();
    // No layer sets the key: the theme keeps its own pair.
    assert_eq!(
        ChangePalette::from_settings(&Settings::defaults()),
        ChangePalette::Theme
    );

    let settings = fixture.settings("appearance.changes = \"accessible\"\n");
    assert_eq!(ChangePalette::from_settings(&settings), ChangePalette::Accessible);

    // A word outside the declared set is dropped by the loader, so the theme's
    // own pair stays in force rather than an unreadable value repainting it.
    let settings = fixture.settings("appearance.changes = \"rainbow\"\n");
    assert_eq!(ChangePalette::from_settings(&settings), ChangePalette::Theme);
}

#[test]
fn theme_changes_palette_off_restores_the_selected_theme() {
    for name in ["dark", "light"] {
        let theme = builtin(name);
        // Turning the setting on repaints the copy, and the copy carries the
        // palette that drew it.
        let drawn = changes::apply(&theme, ChangePalette::Accessible);
        assert_ne!(drawn.core.success, theme.core.success, "{name}");
        assert_eq!(drawn.changes, ChangePalette::Accessible, "{name}");

        // Turning it off again reads the theme the loader holds, which the
        // palette never touched, so every token is the file's own again.
        let restored = changes::apply(&theme, ChangePalette::Theme);
        assert_eq!(restored, theme, "{name}: the restored theme is not the file's");
        assert_eq!(restored.changes, ChangePalette::Theme, "{name}");
    }
}

#[test]
fn theme_changes_palette_assumes_a_dark_background_when_the_text_is_the_terminal_own() {
    // A theme may leave its text to the terminal, which says nothing about the
    // background. The bright pair is the one that still reads on the dark
    // background `Theme::default` assumes.
    let mut theme = builtin("dark");
    theme.core.text = Color::Reset;
    let drawn = changes::apply(&theme, ChangePalette::Accessible);
    let dark = changes::apply(&builtin("dark"), ChangePalette::Accessible);
    assert_eq!(drawn.core.success, dark.core.success);
    assert_eq!(drawn.core.error, dark.core.error);
}

#[test]
fn theme_changes_palette_is_readable_on_both_builtin_appearances() {
    for name in ["dark", "light"] {
        let theme = builtin(name);
        let drawn = changes::apply(&theme, ChangePalette::Accessible);
        let (added, removed) = (drawn.core.success, drawn.core.error);

        // The pair is not the theme's own, or the setting would do nothing.
        assert_ne!(added, theme.core.success, "{name}");
        assert_ne!(removed, theme.core.error, "{name}");
        assert_ne!(added, removed, "{name}: both changes would read as one colour");

        // Hues far enough apart to survive the common colour-vision
        // differences, and far enough from the amber the client already uses
        // for a warning that the two never read as one colour.
        let pair = hue_gap(added, removed);
        assert!(pair >= 120.0, "{name}: the pair is {pair:.1} degrees apart");
        for (change, color) in [("added", added), ("removed", removed)] {
            let away = hue_gap(color, drawn.core.warning);
            assert!(
                away >= 30.0,
                "{name}: {change} is {away:.1} degrees from the warning colour"
            );
        }

        // Measured against the surfaces the theme itself paints, which is the
        // only background the client knows on a terminal that draws its own.
        for (surface, color) in [
            ("status_bg", theme.status.status_bg),
            ("selected_bg", theme.prompt.selected_bg),
        ] {
            for (change, ink) in [("added", added), ("removed", removed)] {
                let ratio = contrast(ink, color);
                assert!(
                    ratio >= 4.5,
                    "{name}: {change} on {surface} measures {ratio:.2}:1, under 4.5:1"
                );
            }
        }
    }
}

#[test]
fn theme_changes_palette_marks_diff_rows_by_text_not_only_colour() {
    let call = diff_call();
    let symbols = SymbolPreset::Unicode.symbols();
    for name in ["dark", "light"] {
        let theme = builtin(name);
        let drawn = changes::apply(&theme, ChangePalette::Accessible);
        let lines = tool_line::lines(&call, &drawn, symbols, 80);
        let rows: Vec<String> = lines.iter().map(drawn_row).collect();

        // The signs the tool wrote are still on the rows: that is the marker a
        // reader who cannot tell the two colours apart follows.
        let removed = lines
            .iter()
            .find(|line| drawn_row(line).ends_with("-old"))
            .unwrap_or_else(|| panic!("{name}: no removed row in {rows:?}"));
        let added = lines
            .iter()
            .find(|line| drawn_row(line).ends_with("+new"))
            .unwrap_or_else(|| panic!("{name}: no added row in {rows:?}"));
        assert_eq!(row_color(removed), Some(drawn.core.error), "{name}: {rows:?}");
        assert_eq!(row_color(added), Some(drawn.core.success), "{name}: {rows:?}");

        // A file header and a hunk header describe the file rather than change
        // it, and the count row the client adds itself starts with a sign of
        // its own, so all three keep the output's ordinary colour.
        for row in ["--- a/src/main.rs", "+++ b/src/main.rs", "@@ -1,2 +1,3 @@", "+2 lines"] {
            let line = lines
                .iter()
                .find(|line| drawn_row(line).ends_with(row))
                .unwrap_or_else(|| panic!("{name}: no `{row}` row in {rows:?}"));
            assert_eq!(
                row_color(line),
                Some(drawn.transcript.notice),
                "{name}: `{row}` was drawn as a change"
            );
        }
    }
}

#[test]
fn theme_changes_palette_leaves_output_that_is_not_a_diff_alone() {
    // `ls -l` prints an ordinary file with a leading `-` and a tool's own prose
    // may hold a Markdown bullet. Neither is a change: a leading sign means one
    // only inside a diff, and this block carries no diff marker.
    let call = call_with_output(
        &[
            "total 8",
            "-rw-r--r--  1 u  staff  12 main.rs",
            "- a bullet in the tool's prose",
            "+ plain text",
        ],
        0,
    );
    let symbols = SymbolPreset::Unicode.symbols();
    for name in ["dark", "light"] {
        let theme = changes::apply(&builtin(name), ChangePalette::Accessible);
        let lines = tool_line::lines(&call, &theme, symbols, 80);
        for line in lines.iter().skip(1) {
            assert_eq!(
                row_color(line),
                Some(theme.transcript.notice),
                "{name}: `{}` was read as a change outside a diff",
                drawn_row(line)
            );
        }
    }
}

#[test]
fn theme_changes_palette_off_leaves_every_output_row_its_own_colour() {
    let call = diff_call();
    let symbols = SymbolPreset::Unicode.symbols();
    let theme = builtin("dark");
    let lines = tool_line::lines(&call, &theme, symbols, 80);
    for line in lines.iter().skip(1) {
        assert_eq!(
            row_color(line),
            Some(theme.transcript.notice),
            "`{}` left the output's own colour with the palette off",
            drawn_row(line)
        );
    }
    // And the signs are there the same way, because they never came from colour.
    assert!(lines.iter().any(|line| drawn_row(line).ends_with("-old")));
    assert!(lines.iter().any(|line| drawn_row(line).ends_with("+new")));
}
