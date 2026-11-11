//! Tests for the generated settings screen.
//!
//! Each test builds its own `$HORON_HOME` and its own reloader, then drives the
//! screen with the keystrokes a person sends, so a pass means the row was found
//! from the registry, the widget matched its declared kind, and a confirmed
//! value reached both the settings in force and the file on disk.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::settings::{Outcome, SettingChange, SettingsOverlay};
use crate::app::App;
use crate::settings::{self, SettingValue, Settings, Sources};
use crate::theme::ThemeLoader;

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

    /// `$HORON_HOME`, which the theme directory and the global file share.
    fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    fn global(&self) -> PathBuf {
        self.home().join("tui.toml")
    }

    fn themes(&self) -> PathBuf {
        self.home().join("themes")
    }

    fn write(&self, path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create the fixture directory");
        }
        std::fs::write(path, text).expect("write the fixture file");
    }

    fn sources(&self) -> Sources {
        Sources {
            global: self.global(),
            project: None,
            configs: Vec::new(),
        }
    }

    fn reloader(&self) -> settings::Reloader {
        std::fs::create_dir_all(self.home()).expect("create home");
        settings::Reloader::new(self.sources())
    }

    /// An app drawing the settings in force, ready to take `/settings`.
    fn app(&self, reloader: &settings::Reloader) -> (App, ThemeLoader) {
        let (_sender, receiver) = std::sync::mpsc::channel();
        let mut app = App::new("socket".to_string(), receiver);
        app.themes_dir = self.themes();
        let mut themes = ThemeLoader::in_dir(self.themes());
        crate::apply_settings(&mut app, reloader.settings(), &mut themes);
        (app, themes)
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Type one character at a time, the way a terminal reports them.
fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(key(code));
}

/// Open the screen through the command the user types.
fn open(app: &mut App) {
    type_text(app, "/settings");
    press(app, KeyCode::Enter);
}

/// Move the selection onto the row for `id`.
fn select(app: &mut App, id: &str) {
    let overlay = app.overlay.as_mut().expect("the settings screen is open");
    assert!(
        overlay.select_id(id),
        "`{id}` has no row on any tab: {:?}",
        overlay.tabs()
    );
}

fn overlay(app: &App) -> &SettingsOverlay {
    app.overlay.as_ref().expect("the settings screen is open")
}

fn draw_screen(app: &mut App, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    if let Err(message) = crate::draw(&mut terminal, app) {
        panic!("draw failed: {message}");
    }
    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &Buffer) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                text.push_str(cell.symbol());
            }
        }
        text.push('\n');
    }
    text
}

fn has_foreground(buffer: &Buffer, colour: Color) -> bool {
    (0..buffer.area.height)
        .any(|y| (0..buffer.area.width).any(|x| buffer.cell((x, y)).is_some_and(|cell| cell.fg == colour)))
}

/// The light theme's accent, as `crates/tui/themes/light.json` declares it.
const LIGHT_ACCENT: Color = Color::Rgb(0x2e, 0x5b, 0xd6);

#[test]
fn overlays_theme_change_reaches_the_cell_the_settings_and_the_file() {
    let fixture = Fixture::new();
    let mut reloader = fixture.reloader();
    let (mut app, mut themes) = fixture.app(&reloader);
    assert_eq!(app.theme.name, "dark");

    open(&mut app);
    assert!(app.overlay.is_some(), "/settings opens the screen");
    select(&mut app, "theme");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        overlay(&app).choices(),
        Some(&["dark".to_string(), "light".to_string()][..]),
        "the built-in themes are offered"
    );
    press(&mut app, KeyCode::Down);
    assert_eq!(overlay(&app).pick_index(), Some(1));
    press(&mut app, KeyCode::Enter);
    // The screen reports the change; the caller writes and re-applies it.
    crate::flush_setting_change(&mut app, &mut reloader, &mut themes);

    assert_eq!(reloader.settings().text("theme"), Some("light"));
    assert_eq!(app.theme.name, "light", "the app follows the saved value");
    assert!(app.banner.is_none(), "{:?}", app.banner);
    let saved = std::fs::read_to_string(fixture.global()).expect("the global file was written");
    assert!(saved.contains("theme = \"light\""), "{saved}");

    let buffer = draw_screen(&mut app, 80, 24);
    assert!(
        has_foreground(&buffer, LIGHT_ACCENT),
        "the screen is not drawn in the light theme:\n{}",
        buffer_text(&buffer)
    );
}

#[test]
fn overlays_rejected_save_keeps_the_old_value_and_names_the_file() {
    let fixture = Fixture::new();
    // A regular file where the settings directory must be: every write under it
    // fails, which is the failure a read-only home directory produces.
    let blocked = fixture.root().join("blocked");
    fixture.write(&blocked, "not a directory");
    let sources = Sources {
        global: blocked.join("tui.toml"),
        project: None,
        configs: Vec::new(),
    };
    let mut reloader = settings::Reloader::new(sources);
    let (mut app, mut themes) = fixture.app(&reloader);

    open(&mut app);
    select(&mut app, "theme");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    crate::flush_setting_change(&mut app, &mut reloader, &mut themes);

    let banner = app.banner.clone().expect("the failure is reported");
    assert!(banner.contains("theme"), "{banner}");
    assert!(banner.contains("tui.toml"), "{banner}");
    assert_eq!(
        reloader.settings().text("theme"),
        Some("dark"),
        "a rejected save changes nothing in memory"
    );
    assert_eq!(app.theme.name, "dark");
    assert_eq!(
        overlay(&app).value("theme"),
        Some(&SettingValue::Str("dark".to_string())),
        "the row keeps showing the value still in force"
    );
}

#[test]
fn overlays_rows_are_generated_from_the_registry() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    let mut listed: Vec<&'static str> = Vec::new();
    let tabs: Vec<&'static str> = overlay(&app).tabs().to_vec();
    assert_eq!(
        tabs,
        vec!["Appearance", "Status line", "Editor", "Transcript"],
        "tabs follow the order the registry declares"
    );
    for tab in tabs {
        let overlay = app.overlay.as_mut().expect("the settings screen is open");
        assert!(overlay.select_tab(tab));
        for row in overlay.rows() {
            assert_eq!(
                row.spec.ui.tab, tab,
                "`{}` is listed under a tab its declaration does not name",
                row.id
            );
            listed.push(row.id);
        }
    }
    let mut expected: Vec<&'static str> = crate::settings::registry::ids().collect();
    listed.sort_unstable();
    expected.sort_unstable();
    assert_eq!(listed, expected, "every declared setting appears exactly once");
}

#[test]
fn overlays_bool_row_toggles_without_an_editor() {
    let fixture = Fixture::new();
    let mut reloader = fixture.reloader();
    let (mut app, mut themes) = fixture.app(&reloader);
    open(&mut app);
    select(&mut app, "editor.submit_on_enter");

    assert_eq!(app.on_key(key(KeyCode::Enter)), None, "a toggle is not a message");
    assert!(!overlay(&app).is_editing(), "a toggle opens no editor");

    // Write the toggle so the row shows the value now in force, then toggle it
    // back and read the reported change.
    crate::flush_setting_change(&mut app, &mut reloader, &mut themes);
    assert_eq!(
        reloader.settings().boolean("editor.submit_on_enter"),
        Some(false),
        "Enter turned the boolean off and the save kept it"
    );
    assert_eq!(
        overlay(&app).value("editor.submit_on_enter"),
        Some(&SettingValue::Bool(false))
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.take_setting_change(),
        Some(SettingChange {
            id: "editor.submit_on_enter",
            value: SettingValue::Bool(true)
        })
    );
}

#[test]
fn overlays_enum_row_opens_a_pick_list_over_its_declared_values() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    select(&mut app, "symbols");

    press(&mut app, KeyCode::Enter);
    assert!(overlay(&app).is_editing());
    assert_eq!(
        overlay(&app).choices(),
        Some(&["unicode".to_string(), "nerd".to_string(), "ascii".to_string()][..])
    );
    assert_eq!(overlay(&app).pick_index(), Some(0));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(overlay(&app).pick_index(), Some(2));
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.take_setting_change(),
        Some(SettingChange {
            id: "symbols",
            value: SettingValue::Str("ascii".to_string())
        })
    );
}

#[test]
fn overlays_number_row_rejects_text_with_an_inline_error() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    select(&mut app, "statusline.path_max");

    press(&mut app, KeyCode::Enter);
    // The editor starts on the value in force, so clear it first.
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    type_text(&mut app, "wide");
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.take_setting_change(), None, "a rejected edit saves nothing");
    let error = overlay(&app).error().expect("the editor explains the problem");
    assert!(error.contains("whole number"), "{error}");
    assert!(overlay(&app).is_editing());
}

#[test]
fn overlays_array_row_round_trips_through_the_file() {
    let fixture = Fixture::new();
    let mut reloader = fixture.reloader();
    let (mut app, mut themes) = fixture.app(&reloader);
    open(&mut app);
    select(&mut app, "statusline.left");

    press(&mut app, KeyCode::Enter);
    for _ in 0.."model".len() {
        press(&mut app, KeyCode::Backspace);
    }
    type_text(&mut app, "provider, model");
    press(&mut app, KeyCode::Enter);
    crate::flush_setting_change(&mut app, &mut reloader, &mut themes);

    assert_eq!(
        reloader.settings().list("statusline.left"),
        Some(&["provider".to_string(), "model".to_string()][..])
    );
    let saved = std::fs::read_to_string(fixture.global()).expect("the global file was written");
    assert!(saved.contains("provider"), "{saved}");
}

#[test]
fn overlays_escape_closes_without_saving() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    select(&mut app, "editor.submit_on_enter");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        overlay(&app).value("editor.submit_on_enter"),
        Some(&SettingValue::Bool(true)),
        "the row still shows the value in force"
    );

    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none());
    assert!(app.banner.is_none());
    assert!(!fixture.global().exists(), "nothing was written");
    assert_eq!(reloader.settings().boolean("editor.submit_on_enter"), Some(true));
}

#[test]
fn overlays_escape_leaves_an_editor_before_closing() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    select(&mut app, "statusline.path_max");
    press(&mut app, KeyCode::Enter);
    assert!(overlay(&app).is_editing());

    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_some(), "the screen is still open");
    assert!(!overlay(&app).is_editing());
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none());
}

#[test]
fn overlays_keys_do_not_reach_the_message_behind_it() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    for character in "hello".chars() {
        assert_eq!(app.on_key(key(KeyCode::Char(character))), None);
    }
    assert_eq!(app.input.text(), "", "typing went to the screen");
    assert_eq!(app.on_key(key(KeyCode::Enter)), None, "no message was sent");
    assert!(!app.pending_turn);
    assert_eq!(
        app.transcript.items().len(),
        0,
        "the command left no message in the conversation"
    );
}

#[test]
fn overlays_a_higher_layer_keeps_its_value_and_its_source_after_a_save() {
    let fixture = Fixture::new();
    let project = fixture.root().join("work").join(".horon").join("tui.toml");
    fixture.write(&project, "theme = \"light\"\n");
    let sources = Sources {
        global: fixture.global(),
        project: Some(project.clone()),
        configs: Vec::new(),
    };
    let mut reloader = settings::Reloader::new(sources);
    let (mut app, mut themes) = fixture.app(&reloader);
    open(&mut app);
    assert_eq!(
        overlay(&app).provenance("theme").map(|origin| origin.label()),
        Some(format!("project {}", project.display())),
        "the screen says which layer supplied the value"
    );

    // Saving the global file cannot beat the project layer: choose `dark`,
    // which only the global file would carry.
    select(&mut app, "theme");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Up);
    assert_eq!(overlay(&app).pick_index(), Some(0));
    press(&mut app, KeyCode::Enter);
    crate::flush_setting_change(&mut app, &mut reloader, &mut themes);

    assert_eq!(reloader.settings().text("theme"), Some("light"));
    let saved = std::fs::read_to_string(fixture.global()).expect("the global file was written");
    assert!(saved.contains("theme = \"dark\""), "{saved}");
    assert_eq!(
        overlay(&app).provenance("theme").map(|origin| origin.label()),
        Some(format!("project {}", project.display()))
    );
    assert_eq!(
        overlay(&app).value("theme"),
        Some(&SettingValue::Str("light".to_string()))
    );
}

#[test]
fn overlays_draws_at_sixty_by_sixteen() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    let buffer = draw_screen(&mut app, 60, 16);
    let text = buffer_text(&buffer);
    assert!(text.contains("Settings"), "{text}");
    assert!(text.contains("Appearance"), "{text}");
    assert!(text.contains("Theme"), "{text}");
    assert!(text.contains("Status line"), "{text}");
}

#[test]
fn overlays_draws_without_panicking_when_the_terminal_is_tiny() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    for (width, height) in [(1, 1), (5, 3), (11, 4), (12, 5), (20, 6)] {
        let buffer = draw_screen(&mut app, width, height);
        assert_eq!(buffer.area.width, width);
        assert_eq!(buffer.area.height, height);
    }
}

#[test]
fn overlays_theme_choices_are_the_built_ins_then_user_files() {
    let fixture = Fixture::new();
    fixture.write(&fixture.themes().join("mine.json"), "{}");
    fixture.write(&fixture.themes().join("notes.txt"), "");
    let mut overlay = SettingsOverlay::new(fixture.themes());
    overlay.refresh(&Settings::defaults());
    overlay.select_id("theme");
    assert_eq!(overlay.on_key(key(KeyCode::Enter)), Outcome::Handled);
    assert_eq!(
        overlay.choices(),
        Some(&["dark".to_string(), "light".to_string(), "mine".to_string()][..]),
        "only `*.json` stems join the two built-ins"
    );
}

#[test]
fn overlays_pick_list_arrows_move_and_confirm() {
    let fixture = Fixture::new();
    let mut overlay = SettingsOverlay::new(fixture.themes());
    overlay.refresh(&Settings::defaults());
    overlay.select_id("symbols");
    assert_eq!(overlay.on_key(key(KeyCode::Enter)), Outcome::Handled);
    assert_eq!(overlay.pick_index(), Some(0));
    assert_eq!(overlay.on_key(key(KeyCode::Right)), Outcome::Handled);
    assert_eq!(overlay.pick_index(), Some(1));
    assert_eq!(
        overlay.on_key(key(KeyCode::Enter)),
        Outcome::Change(SettingChange {
            id: "symbols",
            value: SettingValue::Str("nerd".to_string())
        })
    );
    assert!(!overlay.is_editing());
}
