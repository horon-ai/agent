use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, symlink_metadata};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use horon_agent_extension_types::{
    EXTENSION_PROTOCOL, ExtensionManifest, MANIFEST_VERSION, PackageKind, PackageResources, PluginManifest, TARGETS,
};
use horon_agent_platform::fs::{is_executable, is_path_link, native_package_target, package_open_options};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::catalog::PackageRecord;
use super::native_target::verify_native_target;

const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const EXTENSION_FILE: &str = "extension.toml";
const PLUGIN_FILE: &str = "plugin.toml";

/// The declared paths are relative to the package root and have passed file checks.
/// Installation must validate the staged copy again before publishing it.
#[derive(Debug)]
pub struct ValidatedPackage {
    pub kind: PackageKind,
    pub extension: ExtensionManifest,
    pub display_name: String,
    pub resources: PackageResources,
    pub executable: PathBuf,
    pub files: BTreeSet<PathBuf>,
}

/// Validate a package directory and select the executable for the running host.
/// This function only reads files and never starts a child.
pub fn validate_package(root: &Path) -> Result<ValidatedPackage> {
    let root_meta = symlink_metadata(root).with_context(|| format!("open {}", root.display()))?;
    ensure!(
        root_meta.is_dir() && !is_path_link(&root_meta),
        "package root must be a directory without links"
    );
    let has_extension = exists(&root.join(EXTENSION_FILE))?;
    let has_plugin = exists(&root.join(PLUGIN_FILE))?;
    let mut files = BTreeSet::new();
    let (kind, extension_path, plugin) = match (has_extension, has_plugin) {
        (true, false) => (PackageKind::Extension, PathBuf::from(EXTENSION_FILE), None),
        (false, true) => {
            let plugin: PluginManifest = read_manifest(root, Path::new(PLUGIN_FILE))?;
            check_identity(plugin.schema_version, &plugin.id, &plugin.version, &plugin.name)?;
            let extension_path = relative_path(&plugin.extension)?;
            ensure!(
                extension_path.file_name().and_then(|name| name.to_str()) == Some(EXTENSION_FILE),
                "plugin must declare a bundled extension.toml"
            );
            ensure!(
                plugin.resources.paths().next().is_some(),
                "plugin must declare at least one resource"
            );
            files.insert(PathBuf::from(PLUGIN_FILE));
            (PackageKind::Plugin, extension_path, Some(plugin))
        }
        _ => bail!("package root must contain exactly one of extension.toml and plugin.toml"),
    };

    let extension: ExtensionManifest = read_manifest(root, &extension_path)?;
    check_identity(
        extension.schema_version,
        &extension.id,
        &extension.version,
        &extension.name,
    )?;
    ensure!(
        extension.protocol == EXTENSION_PROTOCOL,
        "unsupported extension protocol: {}",
        extension.protocol
    );
    if let Some(plugin) = &plugin {
        ensure!(
            plugin.id == extension.id && plugin.version == extension.version,
            "plugin and extension IDs and versions must match"
        );
    }
    let (display_name, resources) = match plugin {
        Some(plugin) => (plugin.name, plugin.resources),
        None => (extension.name.clone(), PackageResources::default()),
    };
    check_public_names(&extension, &resources)?;
    super::text_resources::validate_resources(root, &resources)?;
    super::hook_resources::validate(root, &resources.hooks, &extension)?;
    files.insert(extension_path.clone());
    let directory = extension_path.parent().context("extension manifest needs a parent")?;
    let host = native_package_target().context("this host does not support native packages")?;
    ensure!(
        extension.targets.contains_key(host),
        "package has no executable for {host}"
    );
    let mut selected = None;
    for (target, declared) in &extension.targets {
        ensure!(
            TARGETS.contains(&target.as_str()),
            "unsupported executable target: {target}"
        );
        let path = directory.join(relative_path(declared)?);
        let file = open_package_file(root, &path)?;
        verify_native_target(file, target, &path)
            .with_context(|| format!("validate {} for {target}", path.display()))?;
        if target == host {
            ensure!(
                is_executable(&root.join(&path)),
                "package executable is not runnable: {}",
                path.display()
            );
            selected = Some(path.clone());
        }
        ensure!(files.insert(path), "duplicate package file: {declared}");
    }
    for declared in resources.paths() {
        let path = relative_path(declared)?;
        open_package_file(root, &path)?;
        ensure!(files.insert(path), "duplicate package file: {declared}");
    }
    super::mcp_resources::validate(root, &resources.mcp, &extension.id)?;
    let mut spellings = BTreeMap::new();
    for path in &files {
        for prefix in path.ancestors().take_while(|part| !part.as_os_str().is_empty()) {
            let folded = prefix.to_string_lossy().to_ascii_lowercase();
            let spelling = spellings.entry(folded).or_insert(prefix);
            ensure!(
                *spelling == prefix,
                "case-insensitive package path collision: {}",
                prefix.display()
            );
        }
    }
    Ok(ValidatedPackage {
        kind,
        extension,
        display_name,
        resources,
        executable: selected.context("validated package has no host executable")?,
        files,
    })
}

/// Read the declaration an installed copy carries and prove it is the recorded one.
///
/// The declaration is `extension.toml` at the copy root, or the bundled
/// `extension.toml` a `plugin.toml` names. The returned string is the lowercase
/// hex SHA-256 of that file's raw bytes, which is the digest a wire handshake
/// compares, and it is checked against the copy's own record before the caller
/// reads anything from the manifest: an installed copy whose declaration no
/// longer matches the bytes the installer published is refused here.
///
/// The executable and the resources are not checked; installation validated
/// them, and a caller that needs only the declarations must not pay for a
/// whole-package verification on every turn.
pub fn read_recorded_declaration(root: &Path, record: &PackageRecord) -> Result<(ExtensionManifest, String)> {
    let root_meta = symlink_metadata(root).with_context(|| format!("open {}", root.display()))?;
    ensure!(
        root_meta.is_dir() && !is_path_link(&root_meta),
        "package root must be a directory without links"
    );
    let has_extension = exists(&root.join(EXTENSION_FILE))?;
    let has_plugin = exists(&root.join(PLUGIN_FILE))?;
    let (path, slash_path) = match (has_extension, has_plugin) {
        (true, false) => (PathBuf::from(EXTENSION_FILE), EXTENSION_FILE.to_owned()),
        (false, true) => {
            let plugin: PluginManifest = read_manifest(root, Path::new(PLUGIN_FILE))?;
            (relative_path(&plugin.extension)?, plugin.extension)
        }
        _ => bail!("the installed copy holds neither one extension.toml nor one plugin.toml"),
    };
    let recorded = record
        .files
        .iter()
        .find(|file| file.path == slash_path)
        .with_context(|| format!("package {} records no declaration at {slash_path}", record.id))?;
    let file = open_package_file(root, &path)?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "manifest exceeds {MAX_MANIFEST_BYTES} bytes: {}",
        path.display()
    );
    let digest = hex::encode(Sha256::digest(&bytes));
    ensure!(
        digest == recorded.sha256 && bytes.len() as u64 == recorded.len,
        "the installed declaration of package {} does not match its record",
        record.id
    );
    let text = std::str::from_utf8(&bytes).with_context(|| format!("read {}", path.display()))?;
    let manifest: ExtensionManifest = toml::from_str(text).with_context(|| format!("parse {}", path.display()))?;
    Ok((manifest, digest))
}

fn exists(path: &Path) -> Result<bool> {
    match symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn read_manifest<T: DeserializeOwned>(root: &Path, path: &Path) -> Result<T> {
    let file = open_package_file(root, path)?;
    let mut text = String::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("read {}", path.display()))?;
    ensure!(
        text.len() as u64 <= MAX_MANIFEST_BYTES,
        "manifest exceeds {MAX_MANIFEST_BYTES} bytes: {}",
        path.display()
    );
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

pub(super) fn open_package_file(root: &Path, path: &Path) -> Result<File> {
    let mut named = root.to_path_buf();
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        named.push(component);
        let metadata = symlink_metadata(&named).with_context(|| format!("inspect {}", named.display()))?;
        ensure!(
            !is_path_link(&metadata),
            "links are not allowed in packages: {}",
            named.display()
        );
        let right_kind = if components.peek().is_some() {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        ensure!(right_kind, "invalid package file or parent: {}", named.display());
    }
    let file = package_open_options()
        .open(&named)
        .with_context(|| format!("open {}", named.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && !is_path_link(&metadata),
        "package path is not a regular file: {}",
        named.display()
    );
    horon_agent_perf_counters::record_package_area_read();
    Ok(file)
}

pub(super) fn relative_path(raw: &str) -> Result<PathBuf> {
    ensure!(
        raw.split('/').all(portable_component),
        "invalid package-relative path: {raw}"
    );
    Ok(raw.split('/').collect())
}

fn portable_component(name: &str) -> bool {
    // ASCII paths have the same case-folding and spelling on supported hosts.
    // Package display names and resource content can still be Unicode.
    if name.is_empty()
        || !name.is_ascii()
        || name.ends_with(['.', ' '])
        || name.starts_with(' ')
        || name
            .bytes()
            .any(|byte| byte.is_ascii_control() || b"\\:<>\"|?*".contains(&byte))
    {
        return false;
    }
    let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
    let stem = stem.to_ascii_uppercase();
    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        && !["COM", "LPT"].into_iter().any(|prefix| {
            stem.strip_prefix(prefix)
                .is_some_and(|suffix| suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_digit())
        })
}

/// The package ID grammar, shared with the names accepted in the inbox.
///
/// An inbox entry is installed under its directory name, so the name has to be
/// a valid ID before the entry is read at all.
pub(super) fn is_package_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn check_identity(schema: u32, id: &str, version: &str, name: &str) -> Result<()> {
    ensure!(
        schema == MANIFEST_VERSION,
        "unsupported package schema version: {schema}"
    );
    ensure!(is_package_id(id), "invalid package ID: {id}");
    ensure!(valid_version(version), "invalid SemVer package version: {version}");
    ensure!(
        !name.trim().is_empty() && !name.chars().any(char::is_control),
        "invalid package display name"
    );
    Ok(())
}

// Validate rather than interpret versions. Equality here is exact text equality,
// including prerelease and build metadata; dependency resolution is out of scope.
fn valid_version(version: &str) -> bool {
    fn identifier(id: &str) -> bool {
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }
    fn number(id: &str) -> bool {
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && (id == "0" || !id.starts_with('0'))
    }
    let release = if let Some((release, build)) = version.split_once('+') {
        if !build.split('.').all(identifier) {
            return false;
        }
        release
    } else {
        version
    };
    let core = if let Some((core, pre)) = release.split_once('-') {
        if !pre
            .split('.')
            .all(|id| identifier(id) && (!id.bytes().all(|b| b.is_ascii_digit()) || number(id)))
        {
            return false;
        }
        core
    } else {
        release
    };
    core.split('.').count() == 3 && core.split('.').all(number)
}

fn check_public_names(extension: &ExtensionManifest, resources: &PackageResources) -> Result<()> {
    fn insert<'a>(names: &mut BTreeSet<&'a str>, name: &'a str, limit: usize) -> Result<()> {
        ensure!(
            !name.is_empty()
                && name.len() <= limit
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b)),
            "invalid public package name: {name}"
        );
        ensure!(names.insert(name), "duplicate public package name: {name}");
        Ok(())
    }
    let mut names = BTreeSet::new();
    let name_limit = 64 - "ext____".len() - extension.id.len();
    for tool in &extension.tools {
        insert(&mut names, &tool.name, name_limit)?;
        ensure!(
            !tool.description.trim().is_empty() && tool.input_schema.is_object(),
            "invalid tool declaration: {}",
            tool.name
        );
    }
    names.clear();
    for command in &extension.commands {
        insert(&mut names, &command.name, name_limit)?;
        ensure!(
            !command.description.trim().is_empty(),
            "missing command description: {}",
            command.name
        );
    }
    for command in &resources.commands {
        let stem = Path::new(command)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("command resource needs a name")?;
        insert(&mut names, stem, name_limit)?;
    }
    let mut events = BTreeSet::new();
    for event in &extension.events {
        ensure!(events.insert(*event), "duplicate package event: {event:?}");
    }
    Ok(())
}
