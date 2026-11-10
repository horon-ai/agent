//! Tests for theme files, colour resolution, glyph presets and live reload.
//!
//! Every test builds its own temporary themes directory, so no test reads the
//! real `$HORON_HOME`, and reload tests call `check_now` instead of sleeping.

use std::path::{Path, PathBuf};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use super::*;
use crate::app::App;
use crate::settings::layers::LayerSet;
use crate::settings::{Settings, Sources};

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
        ascii.prompt_marker,
        ascii.separator,
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
