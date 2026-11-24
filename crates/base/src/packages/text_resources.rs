//! Text resources a plugin declares: skills, rules and prompts.
//!
//! A plugin (`plugin.toml`) declares files relative to its own root alongside
//! the one bundled extension. This module turns those declarations into loaded
//! text at the two boundaries the operator can observe:
//!
//! - Install: [`validate_resources`] runs from [`super::validate_package`], so an
//!   escaping path, a file that is not the format its slot requires, an
//!   oversized file or a duplicate public name is refused before any copy is
//!   published.
//! - Generation load: [`PackageTextResources::load`] reads the installed copy
//!   the catalog record names and proves every declared file still matches the
//!   recorded length and SHA-256 before parsing it. A copy whose bytes changed
//!   is rejected for that package only; every other package still loads.
//!
//! Package text stays body text: nothing here expands a project-file citation
//! (`@path`), so a rule, a skill body or a prompt cannot pull a file from
//! outside its own installed copy into the prompt. Rendering rules as a
//! labelled, capped, low-priority block is `prompt::package_source`'s
//! job, and prompts are never rendered automatically: [`PackageTextResources::prompt`]
//! answers a command that asks for one by name.
//!
//! [`PackageTextCache`] keys each package by the content digest the catalog
//! records. A replacement publishes new bytes under a new digest, so only the
//! owning package's entry is rebuilt; a removal drops only that package's entry
//! and every other package keeps the `Arc` it already had.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use anyhow::{Context, Result, ensure};
use horon_agent_extension_types::{PackageResources, PluginManifest};
use sha2::{Digest, Sha256};

use super::command_resources::{PackageCommand, read_commands, validate_command_files};
use super::manifest::{open_package_file, relative_path};
use super::{PackageKind, PackageRecord, PackageScope, PackageState, PackageStore};
use crate::skill::FileStamp;
use crate::skill::frontmatter::{AllowedTools, parse_text};

/// Upper bound on one `SKILL.md` declared by a package.
pub const MAX_SKILL_BYTES: u64 = 1024 * 1024;
/// Upper bound on one rule file declared by a package.
pub const MAX_RULE_BYTES: u64 = 16 * 1024;
/// Upper bound on one prompt file declared by a package.
pub const MAX_PROMPT_BYTES: u64 = 64 * 1024;
/// Upper bound on the rule bytes one package may declare in total.
pub const MAX_PACKAGE_RULE_BYTES: usize = 24 * 1024;
/// Upper bound on a public resource name, the same 64-byte cap tool and command
/// names use.
pub const MAX_PUBLIC_NAME: usize = 64;

const MAX_PLUGIN_MANIFEST_BYTES: u64 = 256 * 1024;

/// One skill a package declares, resolved against the installed copy.
#[derive(Debug)]
pub struct PackageSkill {
    /// ID of the package that declared the skill.
    pub package_id: String,
    /// Public name, `<package id>--<declared name>`.
    pub name: String,
    pub description: String,
    pub allowed_tools: Option<Vec<String>>,
    /// Absolute path of the installed `SKILL.md`.
    pub path: PathBuf,
}

/// One rule file a package declares.
#[derive(Debug)]
pub struct PackageRule {
    pub package_id: String,
    /// Package-relative path as the manifest declared it, used as the source label.
    pub relative_path: String,
    pub text: String,
}

/// One prompt file a package declares.
#[derive(Debug)]
pub struct PackagePrompt {
    /// Public name, `<package id>--<file stem>`.
    pub name: String,
    pub text: String,
}

/// The text resources of one installed package.
#[derive(Debug)]
pub struct PackageText {
    pub skills: Vec<PackageSkill>,
    pub rules: Vec<PackageRule>,
    pub prompts: Vec<PackagePrompt>,
    /// The Markdown commands the plugin declares, sorted by typed name.
    pub commands: Vec<PackageCommand>,
}

impl PackageText {
    /// The prompt whose public name is `name`, for a command that asked for it.
    pub fn prompt(&self, name: &str) -> Option<&str> {
        self.prompts
            .iter()
            .find(|prompt| prompt.name == name)
            .map(|prompt| prompt.text.as_str())
    }

    /// The command whose declared name is `command`.
    pub fn command(&self, command: &str) -> Option<&PackageCommand> {
        self.commands.iter().find(|entry| entry.command == command)
    }
}

/// The text resources of every package one generation lists.
///
/// Built by [`PackageTextResources::load`] for the records a session's snapshot
/// captured, so a resource appears from the turn that captured its package and
/// disappears from the turn that no longer lists it.
#[derive(Debug, Default)]
pub struct PackageTextResources {
    packages: BTreeMap<String, Arc<PackageText>>,
    rejections: BTreeMap<String, String>,
}

impl PackageTextResources {
    /// Load the resources of `records` through the process-wide cache.
    ///
    /// `workspace` is the session's canonical workspace: a globally scoped
    /// package always loads, a project-scoped one loads only when its recorded
    /// root is that workspace. A snapshot already applies this filter; applying
    /// it again keeps a direct caller from loading another project's rules.
    pub fn load<'a>(
        store: &PackageStore,
        workspace: Option<&Path>,
        records: impl IntoIterator<Item = &'a PackageRecord>,
    ) -> Arc<Self> {
        PackageTextCache::shared().load(store, workspace, records)
    }

    pub fn package(&self, package_id: &str) -> Option<Arc<PackageText>> {
        self.packages.get(package_id).cloned()
    }

    pub fn skills(&self) -> impl Iterator<Item = &PackageSkill> {
        self.packages.values().flat_map(|package| package.skills.iter())
    }

    pub fn rules(&self) -> impl Iterator<Item = &PackageRule> {
        self.packages.values().flat_map(|package| package.rules.iter())
    }

    /// Every Markdown command the listed packages declare.
    ///
    /// The order is the loader's: packages by ID, commands by typed name. A
    /// caller that lists them sorts by `name` itself when it needs one order
    /// across every package.
    pub fn commands(&self) -> impl Iterator<Item = &PackageCommand> {
        self.packages.values().flat_map(|package| package.commands.iter())
    }

    /// The command whose declared name is `command` in `package_id`.
    pub fn command(&self, package_id: &str, command: &str) -> Option<&PackageCommand> {
        self.packages.get(package_id)?.command(command)
    }

    /// The prompt whose public name is `name`, from any listed package.
    ///
    /// Nothing calls this while building a session prompt: a prompt is returned
    /// only to a command that names it.
    pub fn prompt(&self, name: &str) -> Option<&str> {
        self.packages.values().find_map(|package| package.prompt(name))
    }

    /// Why `package_id` contributed no resources, when it was listed but rejected.
    pub fn rejection(&self, package_id: &str) -> Option<&str> {
        self.rejections.get(package_id).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }
}

/// One package's validated resources.
///
/// Only a successful load is cached. A rejection is reported for the load that
/// met it and retried next time, so a copy restored or a permission fixed later
/// is picked up instead of staying refused for the process's lifetime.
struct CachedPackage {
    digest: String,
    /// Every file this entry read, with the identity it read it under. A change
    /// sends the next load back through the recorded-hash check.
    files: Vec<(PathBuf, FileStamp)>,
    /// `None` for a plugin that declares no skill, rule or prompt.
    text: Option<Arc<PackageText>>,
}

impl CachedPackage {
    fn is_fresh(&self) -> bool {
        self.files
            .iter()
            .all(|(path, stamp)| FileStamp::capture(path).is_ok_and(|current| current == *stamp))
    }
}

/// What one load of one package produced.
struct LoadedPackage {
    digest: String,
    files: Vec<(PathBuf, FileStamp)>,
    text: Option<Arc<PackageText>>,
    rejection: Option<String>,
}

impl LoadedPackage {
    fn rejected(digest: String, reason: String) -> Self {
        Self {
            digest,
            files: Vec::new(),
            text: None,
            rejection: Some(reason),
        }
    }
}

/// The per-package resource cache of the runtime.
///
/// One entry per package ID. Loading holds the map's lock across the read so two
/// sessions cannot load the same package twice; the reads themselves only happen
/// on a miss or when an installed file's identity moved.
#[derive(Default)]
pub struct PackageTextCache {
    entries: Mutex<BTreeMap<String, CachedPackage>>,
}

impl PackageTextCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide cache the runtime and the prompt path share.
    pub fn shared() -> &'static Self {
        static SHARED: LazyLock<PackageTextCache> = LazyLock::new(PackageTextCache::new);
        &SHARED
    }

    /// Load the resources of `records`, reusing entries whose digest and file
    /// identities did not move.
    pub fn load<'a>(
        &self,
        store: &PackageStore,
        workspace: Option<&Path>,
        records: impl IntoIterator<Item = &'a PackageRecord>,
    ) -> Arc<PackageTextResources> {
        let mut canonical_ws: Option<Option<PathBuf>> = None;
        let mut entries = self.lock();
        let mut listed = (!entries.is_empty()).then(BTreeSet::new);
        let mut packages = BTreeMap::new();
        let mut rejections = BTreeMap::new();
        for record in records {
            if let Some(set) = listed.as_mut() {
                set.insert(record.id.as_str());
            }
            if record.state != PackageState::Installed || record.kind != PackageKind::Plugin {
                entries.remove(&record.id);
                continue;
            }
            if matches!(record.scope, PackageScope::Project { .. }) {
                let resolved = canonical_ws
                    .get_or_insert_with(|| canonical_workspace(workspace))
                    .as_deref();
                if !in_workspace(&record.scope, resolved) {
                    entries.remove(&record.id);
                    continue;
                }
            }
            // The cache is read and released before the map is touched again:
            // holding an entry across an insert would borrow the map twice.
            if let Some(cached) = entries.get(&record.id)
                && Some(cached.digest.as_str()) == record.digest.as_deref()
                && cached.is_fresh()
            {
                if let Some(text) = cached.text.as_ref() {
                    packages.insert(record.id.clone(), Arc::clone(text));
                }
                continue;
            }
            let loaded = load_package(store, record);
            if let Some(reason) = loaded.rejection {
                entries.remove(&record.id);
                rejections.insert(record.id.clone(), reason);
                continue;
            }
            if let Some(text) = loaded.text.as_ref() {
                packages.insert(record.id.clone(), Arc::clone(text));
            }
            entries.insert(
                record.id.clone(),
                CachedPackage {
                    digest: loaded.digest,
                    files: loaded.files,
                    text: loaded.text,
                },
            );
        }
        // Only packages this generation did not list at all are dropped here: a
        // removal leaves every other package's entry exactly as it was.
        if let Some(set) = listed {
            entries.retain(|id, _| set.contains(id.as_str()));
        }
        Arc::new(PackageTextResources { packages, rejections })
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, CachedPackage>> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Read, hash-check and parse everything one record declares.
///
/// A rejection is reported, never fatal: the record stays installed and every
/// other package keeps its resources.
fn load_package(store: &PackageStore, record: &PackageRecord) -> LoadedPackage {
    let Some(digest) = record.digest.clone() else {
        return LoadedPackage::rejected(String::new(), format!("package {} names no published copy", record.id));
    };
    match load_installed(store, record, &digest) {
        Ok(loaded) => loaded,
        Err(error) => {
            let reason = format!("{error:#}");
            crate::logging::warn(&format!(
                "Package '{}' text resources are not loaded: {reason}",
                record.id
            ));
            LoadedPackage::rejected(digest, reason)
        }
    }
}

fn load_installed(store: &PackageStore, record: &PackageRecord, digest: &str) -> Result<LoadedPackage> {
    ensure!(super::store::is_digest(digest), "invalid stored copy digest: {digest}");
    let root = store.store_dir().join(digest);
    ensure!(root.is_dir(), "stored copy {digest} is missing");
    let resources = read_plugin_resources(&root, record)?;
    let mut files = Vec::new();
    let mut skills = Vec::new();
    let mut rules = Vec::new();
    let mut prompts = Vec::new();
    let mut names = BTreeSet::new();
    for declared in &resources.skills {
        let file = read_verified(&root, record, declared, MAX_SKILL_BYTES)?;
        let skill = parse_skill(&file, &record.id)?;
        ensure!(
            names.insert(skill.name.clone()),
            "duplicate package skill name: {}",
            skill.name
        );
        files.push((file.path.clone(), stamp(&file.path)?));
        skills.push(skill);
    }
    let mut rule_bytes = 0usize;
    for declared in &resources.rules {
        let file = read_verified(&root, record, declared, MAX_RULE_BYTES)?;
        ensure!(!file.text.trim().is_empty(), "package rule is empty: {declared}");
        rule_bytes += file.text.len();
        files.push((file.path.clone(), stamp(&file.path)?));
        rules.push(PackageRule {
            package_id: record.id.clone(),
            relative_path: file.relative,
            text: file.text,
        });
    }
    ensure!(
        rule_bytes <= MAX_PACKAGE_RULE_BYTES,
        "package rules exceed {MAX_PACKAGE_RULE_BYTES} bytes"
    );
    let mut prompt_names = BTreeSet::new();
    for declared in &resources.prompts {
        let file = read_verified(&root, record, declared, MAX_PROMPT_BYTES)?;
        ensure!(!file.text.trim().is_empty(), "package prompt is empty: {declared}");
        let name = public_prompt_name(&record.id, declared)?;
        ensure!(
            prompt_names.insert(name.clone()),
            "duplicate package prompt name: {name}"
        );
        files.push((file.path.clone(), stamp(&file.path)?));
        prompts.push(PackagePrompt { name, text: file.text });
    }
    let (mut commands, mut command_files) = read_commands(&root, record, &resources.commands)?;
    files.append(&mut command_files);
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    rules.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    prompts.sort_by(|left, right| left.name.cmp(&right.name));
    commands.sort_by(|left, right| left.name.cmp(&right.name));
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files.dedup_by(|left, right| left.0 == right.0);
    if skills.is_empty() && rules.is_empty() && prompts.is_empty() && commands.is_empty() {
        // A plugin may declare only MCP servers; it then holds no text
        // resource and must not make an otherwise empty set non-empty.
        return Ok(LoadedPackage {
            digest: digest.to_owned(),
            files,
            text: None,
            rejection: None,
        });
    }
    Ok(LoadedPackage {
        digest: digest.to_owned(),
        files,
        text: Some(Arc::new(PackageText {
            skills,
            rules,
            prompts,
            commands,
        })),
        rejection: None,
    })
}

/// The bundled `plugin.toml` of the installed copy, checked against the record.
fn read_plugin_resources(root: &Path, record: &PackageRecord) -> Result<PackageResources> {
    let path = Path::new("plugin.toml");
    let mut file = open_package_file(root, path)?;
    let bytes = read_capped_prefix(
        &mut file,
        MAX_PLUGIN_MANIFEST_BYTES,
        &format!("plugin.toml of package {}", record.id),
    )?;
    ensure!(
        bytes.len() as u64 <= MAX_PLUGIN_MANIFEST_BYTES,
        "plugin.toml of package {} is too large",
        record.id
    );
    let text = String::from_utf8(bytes).with_context(|| format!("read plugin.toml of package {}", record.id))?;
    // The declarations are only trustworthy while the manifest bytes are the
    // ones the catalog recorded, so the manifest is hash-checked like every
    // resource it names.
    let stored = record
        .files
        .iter()
        .find(|file| file.path == "plugin.toml")
        .with_context(|| format!("plugin.toml is not a recorded file of package {}", record.id))?;
    ensure!(
        text.len() as u64 == stored.len,
        "plugin.toml of package {} no longer matches the recorded length",
        record.id
    );
    let hex_buf = sha256_hex_buf(text.as_bytes());
    ensure!(
        hex_buf.as_slice() == stored.sha256.as_bytes(),
        "plugin.toml of package {} no longer matches the recorded SHA-256",
        record.id
    );
    let manifest: PluginManifest = toml::from_str(&text).context("parse the installed plugin.toml")?;
    ensure!(
        manifest.id == record.id,
        "the installed plugin.toml declares ID {}, not {}",
        manifest.id,
        record.id
    );
    ensure!(
        manifest.version == record.version,
        "the installed plugin.toml declares version {}, not {}",
        manifest.version,
        record.version
    );
    Ok(manifest.resources)
}

/// Lowercase hex encoding of a SHA-256 digest in a stack buffer.
pub(super) fn format_sha256_hex(digest: [u8; 32]) -> [u8; 64] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex_buf = [0u8; 64];
    for (i, &byte) in digest.iter().enumerate() {
        hex_buf[i * 2] = HEX[(byte >> 4) as usize];
        hex_buf[i * 2 + 1] = HEX[(byte & 0x0f) as usize];
    }
    hex_buf
}

pub(super) fn sha256_hex_buf(bytes: &[u8]) -> [u8; 64] {
    format_sha256_hex(Sha256::digest(bytes).into())
}

pub(super) fn read_capped_prefix(file: &mut std::fs::File, limit: u64, label: &str) -> Result<Vec<u8>> {
    let cap = match file.metadata() {
        Ok(meta) => usize::try_from(meta.len().min(limit)).unwrap_or(0).saturating_add(1),
        Err(_) => 0,
    };
    let mut bytes = Vec::with_capacity(cap);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {label}"))?;
    Ok(bytes)
}

pub(super) fn read_capped_file(mut file: std::fs::File, limit: u64, label: &str) -> Result<Vec<u8>> {
    let bytes = read_capped_prefix(&mut file, limit, label)?;
    ensure!(bytes.len() as u64 <= limit, "{label} exceeds {limit} bytes");
    Ok(bytes)
}

/// One resource file read from inside a package root.
pub(super) struct ResourceFile {
    pub(super) relative: String,
    pub(super) path: PathBuf,
    pub(super) text: String,
}

pub(super) fn read_verified(root: &Path, record: &PackageRecord, declared: &str, cap: u64) -> Result<ResourceFile> {
    let stored = record
        .files
        .iter()
        .find(|file| file.path == declared)
        .with_context(|| format!("{} is not a recorded file of package {}", declared, record.id))?;
    let (relative, path, bytes) = read_bytes(root, declared, cap)?;
    ensure!(
        bytes.len() as u64 == stored.len,
        "package file {declared} no longer matches the recorded length"
    );
    let hex_buf = sha256_hex_buf(&bytes);
    ensure!(
        hex_buf.as_slice() == stored.sha256.as_bytes(),
        "package file {declared} no longer matches the recorded SHA-256"
    );
    let text = String::from_utf8(bytes).with_context(|| format!("package file {declared} is not UTF-8"))?;
    Ok(ResourceFile { relative, path, text })
}

/// Read one declared file from `root`, refusing an escaping or absolute path.
fn read_bytes(root: &Path, declared: &str, cap: u64) -> Result<(String, PathBuf, Vec<u8>)> {
    let relative = relative_path(declared)?;
    let file = open_package_file(root, &relative)?;
    let bytes = read_capped_file(file, cap, declared)?;
    Ok((declared.to_owned(), root.join(&relative), bytes))
}

/// [`read_bytes`] decoded as UTF-8.
fn read_text(root: &Path, declared: &str, cap: u64) -> Result<(String, PathBuf, String)> {
    let (relative, path, bytes) = read_bytes(root, declared, cap)?;
    let text = String::from_utf8(bytes).with_context(|| format!("package file {declared} is not UTF-8"))?;
    Ok((relative, path, text))
}

fn stamp(path: &Path) -> Result<FileStamp> {
    FileStamp::capture(path).with_context(|| format!("inspect {}", path.display()))
}

fn parse_skill(file: &ResourceFile, package_id: &str) -> Result<PackageSkill> {
    let parsed = parse_text(&file.text).with_context(|| format!("parse {}", file.relative))?;
    let name = parsed.frontmatter.name.trim().to_owned();
    ensure!(is_resource_name(&name), "invalid package skill name: {name}");
    let public = format!("{package_id}--{name}");
    ensure!(
        public.len() <= MAX_PUBLIC_NAME,
        "package skill name is too long: {public}"
    );
    let description = parsed.frontmatter.description;
    ensure!(
        !description.trim().is_empty(),
        "package skill has no description: {}",
        file.relative
    );
    Ok(PackageSkill {
        package_id: package_id.to_owned(),
        name: public,
        description,
        allowed_tools: parsed.frontmatter.allowed_tools.map(AllowedTools::into_tools),
        path: file.path.clone(),
    })
}

pub(super) fn resource_stem<'a>(declared: &'a str, kind: &str) -> Result<&'a str> {
    let stem = Path::new(declared)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .with_context(|| format!("{kind} resource needs a name: {declared}"))?;
    ensure!(is_resource_name(stem), "invalid package {kind} name: {declared}");
    Ok(stem)
}

/// `<package id>--<file stem>` for a prompt file.
fn public_prompt_name(package_id: &str, declared: &str) -> Result<String> {
    let stem = resource_stem(declared, "prompt")?;
    let name = format!("{package_id}--{stem}");
    ensure!(name.len() <= MAX_PUBLIC_NAME, "package prompt name is too long: {name}");
    Ok(name)
}

pub(super) fn is_resource_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_')
}

/// The session workspace a load filters project scopes against.
///
/// A workspace that cannot be resolved selects only global packages, which is
/// the same choice a package snapshot makes for a session without a directory.
fn canonical_workspace(workspace: Option<&Path>) -> Option<PathBuf> {
    let workspace = workspace?;
    match workspace.canonicalize() {
        Ok(canonical) => Some(canonical),
        Err(error) => {
            crate::logging::debug(&format!(
                "Package text ignores the workspace {}: {error}",
                workspace.display()
            ));
            None
        }
    }
}

fn in_workspace(scope: &PackageScope, workspace: Option<&Path>) -> bool {
    match scope {
        PackageScope::Global => true,
        PackageScope::Project { .. } => scope
            .project_root()
            .is_some_and(|root| workspace.is_some_and(|workspace| workspace == root)),
    }
}

/// Validate the resources a plugin declares inside `root`.
///
/// Called from [`super::validate_package`] before anything is published, and
/// again on the staged copy by the store: a declared path that leaves the
/// package, a file that is not the format its slot requires, an oversized file
/// and a duplicate public name all fail the install instead of loading later.
pub fn validate_resources(root: &Path, resources: &PackageResources) -> Result<()> {
    let mut skill_names = BTreeSet::new();
    for declared in &resources.skills {
        let (_, _, text) = read_text(root, declared, MAX_SKILL_BYTES)?;
        // The package ID is not known here, so this boundary checks the suffix
        // grammar, the worst-case public length and the file format; the exact
        // `<id>--<name>` length is checked when the installed record loads.
        let parsed = parse_text(&text).with_context(|| format!("parse {declared}"))?;
        let name = parsed.frontmatter.name.trim();
        ensure!(is_resource_name(name), "invalid package skill name in {declared}");
        ensure!(
            name.len() + 2 <= MAX_PUBLIC_NAME - 32,
            "package skill name is too long: {declared}"
        );
        ensure!(
            !parsed.frontmatter.description.trim().is_empty(),
            "package skill has no description: {declared}"
        );
        ensure!(
            skill_names.insert(name.to_owned()),
            "duplicate package skill name: {declared}"
        );
    }
    let mut rule_bytes = 0usize;
    for declared in &resources.rules {
        let (_, _, text) = read_text(root, declared, MAX_RULE_BYTES)?;
        ensure!(!text.trim().is_empty(), "package rule is empty: {declared}");
        rule_bytes += text.len();
    }
    ensure!(
        rule_bytes <= MAX_PACKAGE_RULE_BYTES,
        "package rules exceed {MAX_PACKAGE_RULE_BYTES} bytes"
    );
    let mut prompt_names = BTreeSet::new();
    for declared in &resources.prompts {
        let (_, _, text) = read_text(root, declared, MAX_PROMPT_BYTES)?;
        ensure!(!text.trim().is_empty(), "package prompt is empty: {declared}");
        let stem = resource_stem(declared, "prompt")?;
        ensure!(
            prompt_names.insert(stem.to_owned()),
            "duplicate package prompt name: {declared}"
        );
    }
    validate_command_files(root, &resources.commands)?;
    Ok(())
}
