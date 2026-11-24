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
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::settings::{Outcome, SettingChange, SettingsOverlay};
use crate::app::App;
use crate::settings::{self, SettingValue, Settings, Sources};
use crate::theme::{Symbols, Theme, ThemeLoader};

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
        vec![
            "Appearance",
            "Status line",
            "Editor",
            "Transcript",
            super::settings::DAEMON_TAB
        ],
        "tabs follow the order the registry declares, then the daemon's own"
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

/// The settings overlay drawn on its own over a screen painted with
/// `background`, and the panel rectangle it chose.
fn settings_screen(area: Rect, background: Option<&str>) -> (Buffer, Rect) {
    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => panic!("test backend failed: {error}"),
    };
    let theme = Theme::default();
    let symbols = Symbols::default();
    let mut overlay = SettingsOverlay::new(PathBuf::from("/nonexistent/themes"));
    overlay.refresh(&Settings::defaults());
    if let Err(error) = terminal.draw(|frame| {
        let area = frame.area();
        if let Some(text) = background {
            frame.render_widget(Paragraph::new(Line::from(text.to_string())), area);
        }
        super::settings::draw(frame, area, &overlay, &theme, symbols);
    }) {
        panic!("draw failed: {error}");
    }
    (terminal.backend().buffer().clone(), super::settings::panel(area))
}

#[test]
fn the_settings_panel_clears_every_cell_of_the_pane_it_covers() {
    let area = Rect::new(0, 0, 60, 16);
    let (buffer, panel) = settings_screen(area, Some(&"X".repeat(usize::from(area.width))));
    assert!(panel.width > 0 && panel.height > 0, "the panel was not drawn");
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if (panel.x..panel.right()).contains(&x) && (panel.y..panel.bottom()).contains(&y) {
                continue;
            }
            let symbol = buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or("");
            assert_ne!(
                symbol, "X",
                "the cell at ({x}, {y}) beside the panel still shows the conversation"
            );
        }
    }
}

#[test]
fn the_settings_panel_stays_inside_the_pane_at_every_width() {
    for (width, height) in [(100, 30), (80, 24), (40, 24), (20, 8), (13, 6), (12, 5)] {
        let area = Rect::new(0, 0, width, height);
        let (buffer, panel) = settings_screen(area, None);
        assert!(
            panel.right() <= area.right() && panel.bottom() <= area.bottom(),
            "the panel at {width}x{height} leaves the pane: {panel:?}"
        );
        let middle = panel.y + panel.height / 2;
        let symbol = buffer
            .cell((panel.right() - 1, middle))
            .map(|cell| cell.symbol())
            .unwrap_or("");
        assert_eq!(
            symbol, "│",
            "the right border of the panel at {width}x{height} was not drawn"
        );
    }
}

#[test]
fn a_pane_too_small_for_the_settings_panel_says_so() {
    let area = Rect::new(0, 0, 10, 4);
    let (buffer, _panel) = settings_screen(area, Some(&"X".repeat(10)));
    let text = buffer_text(&buffer);
    assert!(text.contains("settings"), "the screen hid itself: {text:?}");
    assert!(!text.contains('X'), "the conversation shows through: {text:?}");
}

#[test]
fn tab_walks_the_header_forwards_and_shift_tab_back() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    assert_eq!(
        overlay(&app).tab_name(),
        Some("Appearance"),
        "the tabs do not start at the first one"
    );

    press(&mut app, KeyCode::Tab);
    assert_eq!(
        overlay(&app).tab_name(),
        Some("Status line"),
        "tab did not move to the next tab in the header"
    );
    press(&mut app, KeyCode::Tab);
    assert_eq!(overlay(&app).tab_name(), Some("Editor"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(overlay(&app).tab_name(), Some("Transcript"));

    press(&mut app, KeyCode::BackTab);
    assert_eq!(overlay(&app).tab_name(), Some("Editor"), "shift+tab did not walk back");
    press(&mut app, KeyCode::Left);
    assert_eq!(overlay(&app).tab_name(), Some("Status line"));
}

/// The daemon tab's rows are the server's, and the screen never derives one.
fn daemon_tab_listing() -> crate::settings::DaemonSettings {
    crate::settings::DaemonSettings::from(vec![
        horon_agent_sdk::api::EffectiveSetting {
            key: "provider.stream_idle_timeout_secs".into(),
            label: "Stream idle timeout (seconds)".into(),
            value: "240".into(),
            source: horon_agent_sdk::api::SettingSource::File,
            origin: Some("/home/u/config.toml".into()),
        },
        horon_agent_sdk::api::EffectiveSetting {
            key: "provider.anthropic_reasoning_effort".into(),
            label: "Anthropic reasoning effort".into(),
            value: "(unset)".into(),
            source: horon_agent_sdk::api::SettingSource::Default,
            origin: None,
        },
    ])
}

#[test]
fn the_daemon_tab_draws_the_values_and_sources_the_server_reported() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    app.show_daemon_settings(daemon_tab_listing());
    let overlay = app.overlay.as_mut().expect("the settings screen is open");
    assert!(overlay.select_tab(super::settings::DAEMON_TAB));

    let buffer = draw_screen(&mut app, 100, 30);
    let text = buffer_text(&buffer);
    assert!(text.contains("Stream idle timeout (seconds)"), "{text}");
    assert!(text.contains("240"), "{text}");
    assert!(
        text.contains("/home/u/config.toml"),
        "the file is named as the source: {text}"
    );
    assert!(text.contains("daemon default"), "a default row says so: {text}");
}

#[test]
fn a_daemon_value_is_not_editable_and_enter_changes_nothing() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    app.show_daemon_settings(daemon_tab_listing());
    let screen = app.overlay.as_mut().expect("the settings screen is open");
    assert!(screen.select_tab(super::settings::DAEMON_TAB));

    press(&mut app, KeyCode::Enter);
    assert!(!overlay(&app).is_editing(), "a daemon value opens no editor");
    assert_eq!(app.take_setting_change(), None, "and it reports no change to write");
}

#[test]
fn r_on_the_daemon_tab_asks_for_the_values_again_and_keeps_the_screen_open() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    app.show_daemon_settings(daemon_tab_listing());
    let overlay = app.overlay.as_mut().expect("the settings screen is open");
    assert!(overlay.select_tab(super::settings::DAEMON_TAB));

    let action = app.on_key(key(KeyCode::Char('r')));
    assert_eq!(action, Some(crate::app::Action::DaemonSettings));
    assert!(
        app.overlay.is_some(),
        "the screen stays open while the answer is in flight"
    );
}

#[test]
fn a_daemon_row_keeps_a_space_between_a_long_label_and_its_value() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    app.show_daemon_settings(daemon_tab_listing());
    let overlay = app.overlay.as_mut().expect("the settings screen is open");
    assert!(overlay.select_tab(super::settings::DAEMON_TAB));

    let buffer = draw_screen(&mut app, 100, 30);
    let text = buffer_text(&buffer);
    assert!(
        text.contains("Stream idle timeout (seconds) 240"),
        "the label is wider than its column and must not run into the value: {text}"
    );
}

#[test]
fn a_daemon_read_that_failed_says_so_and_does_not_stay_on_reading() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    app.show_daemon_settings(crate::settings::DaemonSettings::Failed(
        "the harness bridge stopped: broken pipe".into(),
    ));
    let overlay = app.overlay.as_mut().expect("the settings screen is open");
    assert!(overlay.select_tab(super::settings::DAEMON_TAB));

    let buffer = draw_screen(&mut app, 100, 30);
    let text = buffer_text(&buffer);
    assert!(
        text.contains("could not be read"),
        "the tab says the read failed: {text}"
    );
    assert!(
        !text.contains("reading the daemon settings"),
        "a finished read must not still say it is reading: {text}"
    );
    assert!(
        text.contains("broken pipe"),
        "the reason is on the footer, so the user can act on it: {text}"
    );
}

#[test]
fn opening_settings_on_a_closed_connection_says_the_read_cannot_happen() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    app.disconnected = true;

    assert_eq!(app.open_settings(), None, "a closed connection has nothing to ask");
    assert_eq!(
        app.daemon_settings,
        crate::settings::DaemonSettings::Failed("the connection is closed".into()),
        "the tab must not wait for a request that never left"
    );
}

#[test]
fn a_daemon_that_cannot_report_its_settings_still_opens_the_screen() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    app.show_daemon_settings(crate::settings::DaemonSettings::Unavailable(
        "the daemon does not report daemon settings".into(),
    ));
    let overlay = app.overlay.as_mut().expect("the settings screen is open");
    assert!(overlay.select_tab(super::settings::DAEMON_TAB));

    let buffer = draw_screen(&mut app, 100, 30);
    let text = buffer_text(&buffer);
    assert!(
        text.contains("does not report daemon settings"),
        "the tab explains the gap: {text}"
    );
    // The registry tabs are untouched, which is what keeps the screen usable.
    let overlay = app.overlay.as_mut().expect("the settings screen is open");
    assert!(
        overlay.select_tab("Editor"),
        "the local tabs still work: {:?}",
        overlay.tabs()
    );
    assert!(
        !overlay.rows().is_empty(),
        "the editor tab still lists its rows while the daemon tab reports nothing"
    );
}

/// Type the search key and a query, the way a person narrows the list.
fn search(app: &mut App, query: &str) {
    press(app, KeyCode::Char('/'));
    type_text(app, query);
}

/// The ids of the rows the screen lists, in the order it draws them.
fn listed(app: &App) -> Vec<&'static str> {
    overlay(app).rows().iter().map(|row| row.id).collect()
}

#[test]
fn overlays_search_narrows_across_tabs_and_selects_the_match() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    assert_eq!(overlay(&app).tab_name(), Some("Appearance"));

    // `newline` is in no label or key: only the description of
    // `editor.submit_on_enter`, on a tab that is not the open one.
    search(&mut app, "newline");
    assert_eq!(listed(&app), vec!["editor.submit_on_enter"]);
    assert_eq!(
        overlay(&app).selected().map(|row| row.id),
        Some("editor.submit_on_enter"),
        "the only match is not the selected row"
    );
    assert_eq!(
        overlay(&app).tab_name(),
        Some("Editor"),
        "the header did not follow the row the search landed on"
    );

    let text = buffer_text(&draw_screen(&mut app, 80, 24));
    assert!(text.contains("Enter sends"), "the match was not drawn: {text}");
    assert!(text.contains("Editor · Keys"), "{text}");
    assert!(!text.contains("Symbol set"), "{text}");
}

#[test]
fn overlays_search_finds_a_setting_by_its_key_on_another_tab() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    search(&mut app, "path_max");
    assert_eq!(listed(&app), vec!["statusline.path_max"]);
    assert_eq!(overlay(&app).tab_name(), Some("Status line"));
}

#[test]
fn overlays_escape_clears_the_search_before_closing() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    select(&mut app, "statusline.path_max");
    assert_eq!(overlay(&app).tab_name(), Some("Status line"));

    search(&mut app, "wheel");
    assert_eq!(listed(&app), vec!["transcript.mouse_scroll"]);

    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_some());
    assert!(listed(&app).contains(&"transcript.show_reasoning"));
    assert_eq!(
        overlay(&app).tab_name(),
        Some("Transcript"),
        "clearing the filter lost the tab it had settled on"
    );

    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none(), "the second `esc` did not close the screen");
}

#[test]
fn overlays_escape_clears_a_kept_filter_before_closing() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    search(&mut app, "wheel");
    press(&mut app, KeyCode::Enter);
    assert!(app.overlay.is_some());
    assert_eq!(listed(&app), vec!["transcript.mouse_scroll"]);

    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_some(), "the filter stayed on the first `esc`");
    assert!(listed(&app).contains(&"transcript.show_reasoning"));
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none());
}

#[test]
fn overlays_search_with_no_match_says_so() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    search(&mut app, "zzzz");
    assert!(listed(&app).is_empty(), "a query with no hit keeps rows");
    assert!(!overlay(&app).is_editing(), "a search is not a value editor");
    let text = buffer_text(&draw_screen(&mut app, 80, 24));
    assert!(text.contains("nothing matches"), "{text}");

    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_some());
    assert!(listed(&app).contains(&"theme"));
}

#[test]
fn overlays_search_writes_no_settings_and_enter_changes_no_value() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    search(&mut app, "context");
    assert_eq!(app.input.text(), "", "the query leaked into the draft");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.take_setting_change(), None, "keeping a filter reported a change");
    assert!(app.overlay.is_some());
    assert_eq!(listed(&app), vec!["statusline.context_window"]);
    assert!(!fixture.global().exists(), "searching wrote a settings file");
}

#[test]
fn overlays_search_keeps_a_hangul_query_whole_and_backspaces_one_character() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    search(&mut app, "한글");
    assert!(listed(&app).is_empty());
    let text = buffer_text(&draw_screen(&mut app, 80, 24));
    assert!(
        text.replace(' ', "").contains("nothingmatches\"한글\""),
        "a multi-byte query was not kept whole: {text}"
    );

    press(&mut app, KeyCode::Backspace);
    let text = buffer_text(&draw_screen(&mut app, 80, 24));
    assert!(
        text.replace(' ', "").contains("nothingmatches\"한\""),
        "backspace did not cut exactly one character: {text}"
    );
}

#[test]
fn overlays_typing_on_the_row_list_does_not_start_a_search() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);

    type_text(&mut app, "wheel");
    assert!(listed(&app).contains(&"theme"), "plain typing opened a search");
    assert_eq!(app.input.text(), "", "a stray character reached the draft");
    assert!(app.overlay.is_some());
}

#[test]
fn overlays_search_from_the_daemon_tab_lands_on_a_registry_row() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    app.show_daemon_settings(daemon_tab_listing());
    let screen = app.overlay.as_mut().expect("the settings screen is open");
    assert!(screen.select_tab(super::settings::DAEMON_TAB));
    assert!(overlay(&app).rows().is_empty(), "the daemon tab lists no registry row");

    search(&mut app, "path_max");
    assert_eq!(listed(&app), vec!["statusline.path_max"]);
    assert_eq!(overlay(&app).tab_name(), Some("Status line"));
}

#[test]
fn the_daemon_tab_keeps_its_read_only_screen_after_a_search_is_cleared() {
    let fixture = Fixture::new();
    let reloader = fixture.reloader();
    let (mut app, _themes) = fixture.app(&reloader);
    open(&mut app);
    app.show_daemon_settings(daemon_tab_listing());
    let overlay = app.overlay.as_mut().expect("the settings screen is open");
    assert!(overlay.select_tab(super::settings::DAEMON_TAB));

    // A search started from the daemon tab lists registry rows only, and `esc`
    // clears it without taking the screen with it.
    search(&mut app, "path_max");
    assert_eq!(listed(&app), vec!["statusline.path_max"]);
    press(&mut app, KeyCode::Enter);
    assert_eq!(listed(&app), vec!["statusline.path_max"], "enter dropped the filter");
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_some());
    assert!(
        listed(&app).contains(&"statusline.path_max"),
        "esc did not clear the filter"
    );

    // The daemon tab is read-only again once the filter is gone.
    let overlay = app.overlay.as_mut().expect("the settings screen is open");
    assert!(overlay.select_tab(super::settings::DAEMON_TAB));
    let buffer = draw_screen(&mut app, 100, 30);
    let text = buffer_text(&buffer);
    assert!(text.contains("Stream idle timeout (seconds)"), "{text}");
    assert!(text.contains("/home/u/config.toml"), "{text}");

    // `r` asks for the values again once no filter is in force.
    let action = app.on_key(key(KeyCode::Char('r')));
    assert_eq!(action, Some(crate::app::Action::DaemonSettings));
}
