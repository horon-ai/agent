//! Tests for layer precedence, merging, reload and save.
//!
//! Every test builds its own temporary tree and passes an explicit environment
//! map, so no test reads the real `$HORON_HOME` or the process environment and
//! the suite stays deterministic when tests run in parallel. Reload tests call
//! `check_now` instead of sleeping.

use std::path::{Path, PathBuf};

use super::*;
use crate::settings::layers::{FileLayerKind, LayerSet};

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

    fn global(&self) -> PathBuf {
        self.root().join("home").join("tui.toml")
    }

    fn project(&self) -> PathBuf {
        self.root().join("work").join(".horon").join("tui.toml")
    }

    fn config(&self, name: &str) -> PathBuf {
        self.root().join(name)
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
            project: Some(self.project()),
            configs: Vec::new(),
        }
    }

    fn resolve(&self, sources: &Sources, env: &[(&str, &str)]) -> (Settings, Vec<String>) {
        let mut warnings = Vec::new();
        let mut layers = LayerSet::new(sources);
        layers.refresh(&mut warnings);
        let environment = env
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect::<std::collections::BTreeMap<String, String>>();
        let (settings, resolve_warnings) = layers.resolve(|name: &str| environment.get(name).cloned());
        warnings.extend(resolve_warnings);
        (settings, warnings)
    }
}

#[test]
fn settings_layers_apply_in_precedence_order() {
    let fixture = Fixture::new();
    fixture.write(&fixture.global(), "theme = \"light\"\n");
    fixture.write(&fixture.project(), "theme = \"dark\"\n");
    let config = fixture.config("extra.toml");
    fixture.write(&config, "theme = \"light\"\n");

    let sources = Sources {
        global: fixture.global(),
        project: Some(fixture.project()),
        configs: vec![config.clone()],
    };
    let (settings, warnings) = fixture.resolve(&sources, &[]);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(settings.text("theme"), Some("light"));
    assert_eq!(
        settings.provenance("theme").map(Provenance::label),
        Some(format!("config {}", config.display()))
    );

    let sources = Sources {
        global: fixture.global(),
        project: Some(fixture.project()),
        configs: Vec::new(),
    };
    let (settings, warnings) = fixture.resolve(&sources, &[]);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(settings.text("theme"), Some("dark"));
    assert_eq!(
        settings.provenance("theme").map(Provenance::label),
        Some(format!("project {}", fixture.project().display()))
    );

    let (settings, warnings) = fixture.resolve(&sources, &[("HORON_TUI_THEME", "light")]);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(settings.text("theme"), Some("light"));
    assert_eq!(
        settings.provenance("theme").map(Provenance::label).as_deref(),
        Some("env HORON_TUI_THEME")
    );

    assert_eq!(settings.text("symbols"), Some("unicode"));
    assert_eq!(settings.provenance("symbols"), Some(&Provenance::Default));
}

#[test]
fn settings_tables_deep_merge_and_arrays_replace() {
    let fixture = Fixture::new();
    fixture.write(
        &fixture.global(),
        "[statusline]\npreset = \"minimal\"\nleft = [\"model\", \"provider\"]\n",
    );
    fixture.write(
        &fixture.project(),
        "[statusline]\nleft = [\"session\"]\nright = [\"cost\"]\n",
    );
    let (settings, warnings) = fixture.resolve(&fixture.sources(), &[]);
    assert!(warnings.is_empty(), "{warnings:?}");

    // The sibling key from the global file survives the project's table.
    assert_eq!(settings.text("statusline.preset"), Some("minimal"));
    // An array is replaced whole, not appended to.
    assert_eq!(settings.list("statusline.left"), Some(&["session".to_string()][..]));
    assert_eq!(settings.list("statusline.right"), Some(&["cost".to_string()][..]));
    assert_eq!(
        settings.provenance("statusline.left"),
        Some(&Provenance::File {
            layer: FileLayerKind::Project,
            path: fixture.project(),
        })
    );
    assert_eq!(
        settings.provenance("statusline.preset"),
        Some(&Provenance::File {
            layer: FileLayerKind::Global,
            path: fixture.global(),
        })
    );
}

#[test]
fn settings_env_override_files_and_parse_shapes() {
    let fixture = Fixture::new();
    fixture.write(&fixture.global(), "statusline.left = [\"model\"]\n");
    let (settings, warnings) = fixture.resolve(
        &fixture.sources(),
        &[
            ("HORON_TUI_STATUSLINE_LEFT", "model, cost"),
            ("HORON_TUI_EDITOR_SUBMIT_ON_ENTER", "off"),
            ("HORON_TUI_SYMBOLS", "emoji"),
        ],
    );
    assert_eq!(
        settings.list("statusline.left"),
        Some(&["model".to_string(), "cost".to_string()][..])
    );
    assert_eq!(settings.boolean("editor.submit_on_enter"), Some(false));
    assert_eq!(
        settings.provenance("statusline.left").map(Provenance::label).as_deref(),
        Some("env HORON_TUI_STATUSLINE_LEFT")
    );

    // An enum value outside the registry is refused, and the file value stays.
    assert_eq!(settings.text("symbols"), Some("unicode"));
    assert_eq!(settings.provenance("symbols"), Some(&Provenance::Default));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("HORON_TUI_SYMBOLS"), "{warnings:?}");
    assert!(warnings[0].contains("emoji"), "{warnings:?}");
}

#[test]
fn settings_unknown_key_warns_and_is_ignored() {
    let fixture = Fixture::new();
    fixture.write(
        &fixture.global(),
        "theme = \"light\"\nbogus = 1\n\n[statusline]\nbogus_group = \"x\"\n",
    );
    let (settings, warnings) = fixture.resolve(&fixture.sources(), &[]);
    assert_eq!(settings.text("theme"), Some("light"));
    assert_eq!(settings.get("bogus"), None);
    assert_eq!(settings.get("statusline.bogus_group"), None);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings.iter().all(|warning| warning.contains("bogus")), "{warnings:?}");
    assert!(
        warnings
            .iter()
            .all(|warning| warning.contains(fixture.global().display().to_string().as_str())),
        "{warnings:?}"
    );
}

#[test]
fn settings_type_mismatch_warns_and_keeps_lower_value() {
    let fixture = Fixture::new();
    fixture.write(&fixture.global(), "[editor]\nsubmit_on_enter = \"yes\"\n");
    let (settings, warnings) = fixture.resolve(&fixture.sources(), &[]);
    assert_eq!(settings.boolean("editor.submit_on_enter"), Some(true));
    assert_eq!(
        settings.provenance("editor.submit_on_enter"),
        Some(&Provenance::Default)
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("editor.submit_on_enter"), "{warnings:?}");
    assert!(warnings[0].contains("a boolean"), "{warnings:?}");
}

#[test]
fn settings_broken_layer_keeps_last_good_value() {
    let fixture = Fixture::new();
    fixture.write(&fixture.global(), "theme = \"light\"\n");
    let mut reloader = Reloader::new(fixture.sources());
    assert!(reloader.warnings().is_empty(), "{:?}", reloader.warnings());
    assert_eq!(reloader.settings().text("theme"), Some("light"));

    // A file that stops parsing keeps its last good table and reports once.
    fixture.write(&fixture.global(), "theme = [\"light\"\n");
    let reload = reloader.check_now().expect("the file changed");
    assert_eq!(reload.warnings.len(), 1, "{:?}", reload.warnings);
    assert!(reload.warnings[0].contains("cannot parse"), "{:?}", reload.warnings);
    assert_eq!(reloader.settings().text("theme"), Some("light"));
    assert_eq!(
        reloader.settings().provenance("theme").map(Provenance::label),
        Some(format!("global {}", fixture.global().display()))
    );

    // Another layer keeps updating while this one is broken.
    fixture.write(&fixture.project(), "symbols = \"ascii\"\n");
    let reload = reloader.check_now().expect("the project file appeared");
    assert_eq!(reload.warnings.len(), 1, "{:?}", reload.warnings);
    assert_eq!(reloader.settings().text("symbols"), Some("ascii"));
    assert_eq!(reloader.settings().text("theme"), Some("light"));

    // Fixing the file takes effect again and clears the warning.
    fixture.write(&fixture.global(), "theme = \"dark\"\n");
    let reload = reloader.check_now().expect("the file changed again");
    assert!(reload.warnings.is_empty(), "{:?}", reload.warnings);
    assert_eq!(reloader.settings().text("theme"), Some("dark"));
}

#[test]
fn settings_reload_applies_a_modified_file() {
    let fixture = Fixture::new();
    fixture.write(&fixture.global(), "theme = \"dark\"\n");
    let mut reloader = Reloader::with_interval(fixture.sources(), std::time::Duration::from_secs(1));
    // Constructing the reloader just read the files; the interval has not
    // elapsed, so a tick does no work.
    assert!(reloader.tick().is_none());
    assert_eq!(reloader.settings().text("theme"), Some("dark"));

    fixture.write(&fixture.global(), "theme = \"light\"\n");
    let reload = reloader.check_now().expect("the mtime changed");
    assert!(reload.warnings.is_empty(), "{:?}", reload.warnings);
    assert_eq!(reload.settings.text("theme"), Some("light"));
    assert_eq!(reloader.settings().text("theme"), Some("light"));
    assert!(reloader.check_now().is_none(), "nothing changed since the reload");
}

#[test]
fn settings_save_global_preserves_other_keys_and_external_edits() {
    let fixture = Fixture::new();
    fixture.write(
        &fixture.global(),
        "theme = \"light\"\nsymbols = \"ascii\"\n\n[statusline]\npreset = \"custom\"\n",
    );
    let sources = fixture.sources();

    save_global(&sources, "theme", &SettingValue::Str("dark".to_string())).expect("save the global theme");
    let table: toml::Table = toml::from_str(&std::fs::read_to_string(fixture.global()).expect("read the saved file"))
        .expect("the saved file parses");
    assert_eq!(table.get("theme").and_then(|value| value.as_str()), Some("dark"));
    assert_eq!(table.get("symbols").and_then(|value| value.as_str()), Some("ascii"));
    assert_eq!(
        table
            .get("statusline")
            .and_then(|value| value.get("preset"))
            .and_then(|value| value.as_str()),
        Some("custom")
    );

    // An edit made outside the client between two saves survives the next one.
    fixture.write(
        &fixture.global(),
        "theme = \"dark\"\nsymbols = \"ascii\"\n\n[statusline]\npreset = \"custom\"\nright = [\"cost\"]\n",
    );
    save_global(&sources, "statusline.preset", &SettingValue::Str("minimal".to_string())).expect("save the nested key");
    let table: toml::Table = toml::from_str(&std::fs::read_to_string(fixture.global()).expect("read the saved file"))
        .expect("the saved file parses");
    assert_eq!(
        table
            .get("statusline")
            .and_then(|value| value.get("preset"))
            .and_then(|value| value.as_str()),
        Some("minimal")
    );
    assert_eq!(
        table
            .get("statusline")
            .and_then(|value| value.get("right"))
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(table.get("theme").and_then(|value| value.as_str()), Some("dark"));
}

#[test]
fn settings_save_creates_a_missing_file_and_nested_table() {
    let fixture = Fixture::new();
    let sources = fixture.sources();
    save_global(
        &sources,
        "statusline.left",
        &SettingValue::List(vec!["model".to_string(), "cost".to_string()]),
    )
    .expect("create the global file");
    let reloader = Reloader::new(sources);
    assert!(reloader.warnings().is_empty(), "{:?}", reloader.warnings());
    assert_eq!(
        reloader.settings().list("statusline.left"),
        Some(&["model".to_string(), "cost".to_string()][..])
    );
}

#[test]
fn settings_save_refuses_unknown_keys_and_bad_types() {
    let fixture = Fixture::new();
    let sources = fixture.sources();
    assert!(save_global(&sources, "not_a_key", &SettingValue::Bool(true)).is_err());
    // `theme` names a file, so any string is accepted; only the type is checked.
    assert!(save_global(&sources, "theme", &SettingValue::Bool(true)).is_err());
    assert!(save_global(&sources, "symbols", &SettingValue::Bool(true)).is_err());
    assert!(!fixture.global().exists(), "a refused save must not create the file");
}

#[test]
fn settings_save_refuses_to_overwrite_a_broken_file() {
    let fixture = Fixture::new();
    fixture.write(&fixture.global(), "theme = [\n");
    let sources = fixture.sources();
    assert!(save_global(&sources, "theme", &SettingValue::Str("dark".to_string())).is_err());
    assert_eq!(
        std::fs::read_to_string(fixture.global()).expect("the file is untouched"),
        "theme = [\n"
    );
}

#[test]
fn settings_defaults_cover_the_registry() {
    let settings = Settings::defaults();
    for setting in REGISTRY {
        let entry = settings.get(setting.id).expect("every declared key has a default");
        assert_eq!(entry.provenance, Provenance::Default, "{}", setting.id);
        assert!(
            setting.accepts(&entry.value),
            "{} default has the declared shape",
            setting.id
        );
    }
    assert_eq!(settings.iter().count(), REGISTRY.len());
}
