//! The layers and how they combine.
//!
//! A layer is one TOML file (or the registry defaults, or the environment).
//! Resolution walks the layers from lowest to highest and writes each declared
//! key into one map, so the map records the winning value and its source. A key
//! a file does not declare keeps the value a lower layer gave it; a key with the
//! wrong shape is reported and skipped for that reason alone.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use toml::{Table, Value};
use toml_edit::{Array, Decor, Document, Entry, InlineTable, Item, Table as EditTable, Value as EditValue};

use super::registry::{self, SettingKind, SettingSpec};

/// A resolved value, in the shape its declaration promises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    Bool(bool),
    Str(String),
    Number(i64),
    List(Vec<String>),
}

impl SettingValue {
    /// How a warning names a value's shape.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Bool(_) => "a boolean",
            Self::Str(_) => "a string",
            Self::Number(_) => "a number",
            Self::List(_) => "an array of strings",
        }
    }
}

/// Which file layer a value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLayerKind {
    /// `$HORON_HOME/tui.toml`.
    Global,
    /// `<cwd>/.horon/tui.toml`.
    Project,
    /// A file named by `--config`.
    Config,
}

impl FileLayerKind {
    fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Config => "config",
        }
    }
}

/// Where a resolved value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// The registry default, because no layer set the key.
    Default,
    /// A settings file, named so a screen can show its path.
    File { layer: FileLayerKind, path: PathBuf },
    /// An environment variable.
    Env { name: &'static str },
}

impl Provenance {
    /// One line naming the layer, for warnings and the settings overlay.
    pub fn label(&self) -> String {
        match self {
            Self::Default => "default".to_string(),
            Self::File { layer, path } => format!("{} {}", layer.label(), path.display()),
            Self::Env { name } => format!("env {name}"),
        }
    }
}

impl fmt::Display for Provenance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label())
    }
}

/// One resolved key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedValue {
    /// The value in force.
    pub value: SettingValue,
    /// The layer that supplied it.
    pub provenance: Provenance,
}

/// Every declared key with its value and source after all layers applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    entries: BTreeMap<&'static str, ResolvedValue>,
}

impl Settings {
    /// The registry defaults alone.
    pub fn defaults() -> Self {
        let mut entries = BTreeMap::new();
        for setting in registry::REGISTRY {
            entries.insert(
                setting.id,
                ResolvedValue {
                    value: setting.default_value(),
                    provenance: Provenance::Default,
                },
            );
        }
        Self { entries }
    }

    /// The resolved entry for `id`.
    pub fn get(&self, id: &str) -> Option<&ResolvedValue> {
        self.entries.get(id)
    }

    /// The origin of `id`'s value.
    pub fn provenance(&self, id: &str) -> Option<&Provenance> {
        self.entries.get(id).map(|entry| &entry.provenance)
    }

    /// A boolean key.
    pub fn boolean(&self, id: &str) -> Option<bool> {
        match self.get(id).map(|entry| &entry.value) {
            Some(SettingValue::Bool(value)) => Some(*value),
            Some(SettingValue::Str(_) | SettingValue::Number(_) | SettingValue::List(_)) | None => None,
        }
    }

    /// A string key.
    pub fn text(&self, id: &str) -> Option<&str> {
        match self.get(id).map(|entry| &entry.value) {
            Some(SettingValue::Str(value)) => Some(value),
            Some(SettingValue::Bool(_) | SettingValue::Number(_) | SettingValue::List(_)) | None => None,
        }
    }

    /// A number key.
    pub fn number(&self, id: &str) -> Option<i64> {
        match self.get(id).map(|entry| &entry.value) {
            Some(SettingValue::Number(value)) => Some(*value),
            Some(SettingValue::Bool(_) | SettingValue::Str(_) | SettingValue::List(_)) | None => None,
        }
    }

    /// An array key.
    pub fn list(&self, id: &str) -> Option<&[String]> {
        match self.get(id).map(|entry| &entry.value) {
            Some(SettingValue::List(value)) => Some(value),
            Some(SettingValue::Bool(_) | SettingValue::Str(_) | SettingValue::Number(_)) | None => None,
        }
    }

    /// Every key in registry order, for a settings overlay.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &ResolvedValue)> {
        registry::REGISTRY
            .iter()
            .filter_map(|setting| self.entries.get(setting.id).map(|entry| (setting.id, entry)))
    }
}

/// The files a [`Settings`] value is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources {
    /// `$HORON_HOME/tui.toml`.
    pub global: PathBuf,
    /// `<cwd>/.horon/tui.toml`, absent when the working directory is unknown.
    pub project: Option<PathBuf>,
    /// `--config` files, in the order the user gave them.
    pub configs: Vec<PathBuf>,
}

impl Sources {
    /// The standard locations for this process.
    ///
    /// The returned sentence explains why the project layer is missing, and is
    /// empty in the normal case. A missing working directory is not fatal: the
    /// global and `--config` layers still apply.
    pub fn from_environment(configs: Vec<PathBuf>) -> (Self, Option<String>) {
        let (project, note) = match std::env::current_dir() {
            Ok(directory) => (Some(directory.join(".horon").join("tui.toml")), None),
            Err(error) => (
                None,
                Some(format!(
                    "cannot read the working directory: {error}; project settings are skipped"
                )),
            ),
        };
        (
            Self {
                global: horon_agent_sdk::user_horon_agent_home().join("tui.toml"),
                project,
                configs,
            },
            note,
        )
    }

    /// Every file layer, lowest first.
    pub fn layers(&self) -> Vec<(FileLayerKind, PathBuf)> {
        let mut layers = vec![(FileLayerKind::Global, self.global.clone())];
        if let Some(project) = &self.project {
            layers.push((FileLayerKind::Project, project.clone()));
        }
        for config in &self.configs {
            layers.push((FileLayerKind::Config, config.clone()));
        }
        layers
    }
}

/// Size and modification time of a settings file.
///
/// Missing files have no stamp, so creating, deleting or rewriting a file all
/// change the stamp list and trigger a re-resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    /// File length in bytes.
    pub len: u64,
    /// Modification time, when the platform reports one.
    pub modified: Option<SystemTime>,
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => return None,
        };
        let mut modified = None;
        if let Ok(time) = metadata.modified() {
            modified = Some(time);
        }
        Some(Self {
            len: metadata.len(),
            modified,
        })
    }
}

/// One file layer with the last table that parsed.
struct FileLayer {
    kind: FileLayerKind,
    path: PathBuf,
    table: Table,
}

impl FileLayer {
    /// Re-read the file. A parse or read failure keeps the previous table and
    /// records one warning; a missing file empties the layer.
    fn refresh(&mut self, warnings: &mut Vec<String>) {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => match toml::from_str::<Table>(&text) {
                Ok(table) => self.table = table,
                Err(error) => warnings.push(format!(
                    "cannot parse {}: {error}; keeping the last values read from it",
                    self.path.display()
                )),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.table = Table::new(),
            Err(error) => warnings.push(format!(
                "cannot read {}: {error}; keeping the last values read from it",
                self.path.display()
            )),
        }
    }
}

/// The file layers of one [`Sources`], in application order.
pub struct LayerSet {
    layers: Vec<FileLayer>,
}

impl LayerSet {
    /// Layers with no file read yet.
    pub fn new(sources: &Sources) -> Self {
        let layers = sources
            .layers()
            .into_iter()
            .map(|(kind, path)| FileLayer {
                kind,
                path,
                table: Table::new(),
            })
            .collect();
        Self { layers }
    }

    /// Read every file, returning the warnings for files that failed.
    pub fn refresh(&mut self, warnings: &mut Vec<String>) {
        for layer in &mut self.layers {
            layer.refresh(warnings);
        }
    }

    /// Current file stamps, one entry per layer in the same order.
    pub fn stamps(&self) -> Vec<Option<FileStamp>> {
        self.layers.iter().map(|layer| FileStamp::of(&layer.path)).collect()
    }

    /// Combine the defaults, the cached file tables and the environment.
    ///
    /// `env` is a parameter instead of a direct `std::env` read so a test can
    /// supply an exact environment without touching the process's.
    pub fn resolve(&self, env: impl Fn(&str) -> Option<String>) -> (Settings, Vec<String>) {
        let mut settings = Settings::defaults();
        let mut warnings = Vec::new();
        for layer in &self.layers {
            let provenance = Provenance::File {
                layer: layer.kind,
                path: layer.path.clone(),
            };
            apply_table(&mut settings.entries, &layer.table, &provenance, &mut warnings);
        }
        for setting in registry::REGISTRY {
            if let Some(raw) = env(setting.env) {
                apply_env(&mut settings.entries, setting, &raw, &mut warnings);
            }
        }
        (settings, warnings)
    }
}

/// Write one key into the global file, leaving every other key as it is.
///
/// The file is re-read immediately before the change, so a key edited by hand
/// (or by another window) since the client started survives; comments and the
/// file's own layout survive too, because the edit applies to the parsed
/// document instead of re-serializing a copy of its values. A file that no
/// longer parses is left untouched: overwriting it would lose the user's work.
pub fn save_global(sources: &Sources, id: &str, value: &SettingValue) -> Result<(), String> {
    let setting = registry::spec(id).ok_or_else(|| format!("unknown setting `{id}`"))?;
    if !setting.accepts(value) {
        return Err(format!(
            "{} must be {}, not {}",
            setting.id,
            setting.kind.label(),
            value.kind_label()
        ));
    }
    let path = &sources.global;
    let mut document = match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<Document>()
            .map_err(|error| format!("cannot parse {}: {error}; refusing to overwrite it", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Document::new(),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    set_path(document.as_table_mut(), setting.id, to_edit(value), path)?;
    write_atomic(path, &document.to_string())
}

/// `id` as nested tables: `statusline.left` becomes `[statusline] left`.
fn set_path(table: &mut EditTable, id: &str, value: EditValue, path: &Path) -> Result<(), String> {
    set_table_path(table, &mut id.split('.').peekable(), value).map_err(|not_table| {
        format!(
            "`{}` in {} is not a table; refusing to change `{id}`",
            not_table.key,
            path.display()
        )
    })
}

/// A key the walk reached that holds a value, so it cannot hold the key below.
///
/// The dotted key is the whole payload: the caller turns it into the user-facing
/// sentence, so the walkers need neither the setting id nor the file path.
struct NotATable {
    key: String,
}

/// One level of the walk, with the remaining parts still to place.
fn set_table_path(
    table: &mut EditTable,
    parts: &mut std::iter::Peekable<std::str::Split<'_, char>>,
    value: EditValue,
) -> Result<(), NotATable> {
    let part = match parts.next() {
        Some(part) => part,
        None => return Ok(()),
    };
    if parts.peek().is_none() {
        insert_leaf(table, part, value);
        return Ok(());
    }
    let item = table.entry(part).or_insert(Item::Table(EditTable::new()));
    match item {
        Item::Table(next) => set_table_path(next, parts, value),
        Item::Value(EditValue::InlineTable(next)) => set_inline_path(next, parts, value, part),
        Item::None | Item::Value(_) | Item::ArrayOfTables(_) => Err(NotATable { key: part.to_string() }),
    }
}

/// Set a leaf while keeping the comment written above the key.
///
/// `Table::insert` would replace the key and with it that comment, so the value
/// goes through the existing entry when there is one; only its decoration (the
/// whitespace and comment around the value) is carried over.
fn insert_leaf(table: &mut EditTable, part: &str, value: EditValue) {
    match table.entry(part) {
        Entry::Occupied(mut entry) => {
            let decor = match entry.get() {
                Item::Value(old) => old.decor().clone(),
                Item::None | Item::Table(_) | Item::ArrayOfTables(_) => Decor::default(),
            };
            let mut value = value;
            *value.decor_mut() = decor;
            entry.insert(Item::Value(value));
        }
        Entry::Vacant(entry) => {
            entry.insert(Item::Value(value));
        }
    }
}

/// The same walk inside an inline table, whose entries are values, not items.
fn set_inline_path(
    table: &mut InlineTable,
    parts: &mut std::iter::Peekable<std::str::Split<'_, char>>,
    value: EditValue,
    part: &str,
) -> Result<(), NotATable> {
    let next = match parts.next() {
        Some(next) => next,
        None => return Ok(()),
    };
    if parts.peek().is_none() {
        // `InlineTable::insert` hands back the replaced value; the caller asked
        // for the new one, so the old one goes no further.
        drop(table.insert(next, value));
        return Ok(());
    }
    let entry = table.entry(next).or_insert(EditValue::InlineTable(InlineTable::new()));
    match entry {
        EditValue::InlineTable(nested) => set_inline_path(nested, parts, value, part),
        _ => Err(NotATable {
            key: format!("{part}.{next}"),
        }),
    }
}

fn to_edit(value: &SettingValue) -> EditValue {
    match value {
        SettingValue::Bool(value) => EditValue::from(*value),
        SettingValue::Str(value) => EditValue::from(value.clone()),
        SettingValue::Number(value) => EditValue::from(*value),
        SettingValue::List(items) => {
            let mut array = Array::new();
            for item in items {
                array.push(item.as_str());
            }
            EditValue::Array(array)
        }
    }
}

/// Write through a temporary file in the same directory and rename it into
/// place, so a reader never sees half a file.
///
/// The bytes reach the disk before the rename, so a crash cannot leave the new
/// name pointing at an empty file, and a failed write removes its temporary
/// file instead of leaving it behind.
pub(super) fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) | None => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent).map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tui.toml".to_string());
    let temp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    if let Err(error) = write_temp(&temp, text) {
        return Err(format!("cannot write {}: {error}{}", temp.display(), removed(&temp)));
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        return Err(format!("cannot replace {}: {error}{}", path.display(), removed(&temp)));
    }
    Ok(())
}

/// Create the temporary file and get its bytes onto the disk.
fn write_temp(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// Remove a temporary file after a failure, reporting a second failure in the
/// same sentence instead of hiding it.
fn removed(path: &Path) -> String {
    match std::fs::remove_file(path) {
        Ok(()) => String::new(),
        Err(error) => format!("; cannot remove {}: {error}", path.display()),
    }
}

fn apply_table(
    entries: &mut BTreeMap<&'static str, ResolvedValue>,
    table: &Table,
    provenance: &Provenance,
    warnings: &mut Vec<String>,
) {
    let mut leaves = Vec::new();
    flatten(table, "", &mut leaves);
    for (path, value) in leaves {
        apply_leaf(entries, &path, value, provenance, warnings);
    }
}

/// Flatten nested tables to dot paths. Scalars and arrays stay whole, which is
/// what makes tables merge and arrays replace between layers.
fn flatten<'a>(table: &'a Table, prefix: &str, out: &mut Vec<(String, &'a Value)>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Table(nested) => flatten(nested, &path, out),
            _ => out.push((path, value)),
        }
    }
}

fn apply_leaf(
    entries: &mut BTreeMap<&'static str, ResolvedValue>,
    path: &str,
    value: &Value,
    provenance: &Provenance,
    warnings: &mut Vec<String>,
) {
    let Some(setting) = registry::spec(path) else {
        warnings.push(format!("unknown setting `{path}` in {provenance}; ignored"));
        return;
    };
    match convert(setting, value) {
        Ok(value) => {
            entries.insert(
                setting.id,
                ResolvedValue {
                    value,
                    provenance: provenance.clone(),
                },
            );
        }
        Err(reason) => warnings.push(format!("{reason} in {provenance}; keeping the lower value")),
    }
}

fn convert(setting: &SettingSpec, value: &Value) -> Result<SettingValue, String> {
    let mismatch = || {
        format!(
            "{} expects {}, found {}",
            setting.id,
            setting.kind.label(),
            toml_type(value)
        )
    };
    match (setting.kind, value) {
        (SettingKind::Bool, Value::Boolean(value)) => Ok(SettingValue::Bool(*value)),
        (SettingKind::Text, Value::String(value)) => Ok(SettingValue::Str(value.clone())),
        (SettingKind::Number, Value::Integer(value)) => Ok(SettingValue::Number(*value)),
        (SettingKind::Enum(values), Value::String(value)) => {
            if values.contains(&value.as_str()) {
                Ok(SettingValue::Str(value.clone()))
            } else {
                Err(mismatch())
            }
        }
        (SettingKind::Array, Value::Array(items)) => {
            let mut strings = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(item) => strings.push(item.clone()),
                    _ => return Err(mismatch()),
                }
            }
            Ok(SettingValue::List(strings))
        }
        _ => Err(mismatch()),
    }
}

fn toml_type(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "a string",
        Value::Integer(_) => "a number",
        Value::Float(_) => "a float",
        Value::Boolean(_) => "a boolean",
        Value::Datetime(_) => "a datetime",
        Value::Array(_) => "an array",
        Value::Table(_) => "a table",
    }
}

fn apply_env(
    entries: &mut BTreeMap<&'static str, ResolvedValue>,
    setting: &SettingSpec,
    raw: &str,
    warnings: &mut Vec<String>,
) {
    match parse_env(setting, raw) {
        Ok(value) => {
            entries.insert(
                setting.id,
                ResolvedValue {
                    value,
                    provenance: Provenance::Env { name: setting.env },
                },
            );
        }
        Err(reason) => warnings.push(format!("{reason} (from {}); keeping the lower value", setting.env)),
    }
}

fn parse_env(setting: &SettingSpec, raw: &str) -> Result<SettingValue, String> {
    match setting.kind {
        SettingKind::Bool => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(SettingValue::Bool(true)),
            "0" | "false" | "no" | "off" => Ok(SettingValue::Bool(false)),
            other => Err(format!("{} expects a boolean, found `{other}`", setting.id)),
        },
        SettingKind::Text => Ok(SettingValue::Str(raw.to_string())),
        SettingKind::Enum(values) => {
            let value = raw.trim();
            if values.contains(&value) {
                Ok(SettingValue::Str(value.to_string()))
            } else {
                Err(format!(
                    "{} expects one of {}, found `{value}`",
                    setting.id,
                    values.join(", ")
                ))
            }
        }
        SettingKind::Number => raw
            .trim()
            .parse::<i64>()
            .map(SettingValue::Number)
            .map_err(|_| format!("{} expects a number, found `{raw}`", setting.id)),
        SettingKind::Array => Ok(SettingValue::List(
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect(),
        )),
    }
}
