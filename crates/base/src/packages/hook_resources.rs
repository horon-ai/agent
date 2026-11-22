//! The observation hook files a plugin bundles.
//!
//! A plugin names hook files in `[resources] hooks`. Each file is a JSON object
//! that subscribes the plugin's own child to observation events:
//!
//! ```json
//! {"events": ["turn.end"]}
//! ```
//!
//! A package's subscription set is its extension declaration's `events` plus
//! every event a bundled hook file names, and both reach the child through one
//! path: `observer/notify` on the package wire. An event the declaration already
//! covers is refused here, at install time and again when an installed copy is
//! read, because two subscriptions for one event would deliver it twice and
//! leave the package's own bookkeeping ambiguous. Duplicate subscriptions among
//! the hook files themselves are refused the same way.
//!
//! A hook file only declares what the package's binary observes. It cannot
//! allow, deny or modify a call, and the `[hooks] pre_tool` command gate keeps
//! its exit-code contract unchanged.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use horon_agent_extension_types::{ExtensionManifest, ObserverEvent, PackageKind, PluginManifest};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::catalog::PackageRecord;
use super::manifest::{open_package_file, relative_path};

const PLUGIN_FILE: &str = "plugin.toml";
const MAX_PLUGIN_BYTES: u64 = 256 * 1024;
const MAX_HOOK_BYTES: u64 = 16 * 1024;

/// The shape one bundled hook file must have.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookFile {
    events: Vec<ObserverEvent>,
}

/// Validate the hook files a plugin declares inside `root`.
///
/// Called from [`super::validate_package`] before anything is published: a
/// declared path that leaves the package, a file that is not the documented
/// shape, an oversized file, an empty subscription list, and an event already
/// declared in `extension.toml` or named by another hook file all fail the
/// install instead of subscribing the child twice later.
pub fn validate(root: &Path, declared: &[String], extension: &ExtensionManifest) -> Result<()> {
    let mut subscriptions: BTreeSet<ObserverEvent> = extension.events.iter().copied().collect();
    let mut seen_files = BTreeSet::new();
    for path in declared {
        ensure!(seen_files.insert(path.as_str()), "duplicate package hook file: {path}");
        let file: HookFile =
            serde_json::from_str(&read_declared(root, path)?).with_context(|| format!("parse {path}"))?;
        ensure!(!file.events.is_empty(), "hook file names no event: {path}");
        for event in file.events {
            ensure!(
                !extension.events.contains(&event),
                "package event {event:?} is declared in extension.toml and bundled again in {path}"
            );
            ensure!(
                subscriptions.insert(event),
                "duplicate package event: {event:?} in {path}"
            );
        }
    }
    Ok(())
}

/// The events one installed package's child observes.
///
/// The declaration's own `events`, plus every event a bundled hook file names.
/// Each hook file is checked against the length and digest the record published
/// before a byte of it is parsed, so the subscription set comes from the copy
/// the installer recorded. A duplicate between the declaration and a hook file,
/// or among hook files, is refused here as well as at install.
pub fn observation_events(
    root: &Path,
    record: &PackageRecord,
    manifest: &ExtensionManifest,
) -> Result<BTreeSet<ObserverEvent>> {
    let mut events: BTreeSet<ObserverEvent> = manifest.events.iter().copied().collect();
    if record.kind != PackageKind::Plugin {
        return Ok(events);
    }
    for declared in declared_hooks(root)? {
        let file: HookFile = serde_json::from_str(&read_recorded(root, record, &declared)?)
            .with_context(|| format!("parse {declared}"))?;
        for event in file.events {
            ensure!(events.insert(event), "package {} observes {event:?} twice", record.id);
        }
    }
    Ok(events)
}

/// The hook files the copy's own `plugin.toml` names.
///
/// The copy is content-addressed and published by the installer, and every hook
/// file it names is verified against the record before use, so this read only
/// has to prove the file is present and well formed.
fn declared_hooks(root: &Path) -> Result<Vec<String>> {
    if !root.join(PLUGIN_FILE).exists() {
        return Ok(Vec::new());
    }
    let file = open_package_file(root, Path::new(PLUGIN_FILE))?;
    let text = read_capped(file, MAX_PLUGIN_BYTES, Path::new(PLUGIN_FILE))?;
    let plugin: PluginManifest = toml::from_str(&text).with_context(|| format!("parse {PLUGIN_FILE}"))?;
    Ok(plugin.resources.hooks)
}

fn read_declared(root: &Path, declared: &str) -> Result<String> {
    let path = relative_path(declared)?;
    read_capped(open_package_file(root, &path)?, MAX_HOOK_BYTES, &path)
}

fn read_recorded(root: &Path, record: &PackageRecord, declared: &str) -> Result<String> {
    let recorded = record
        .files
        .iter()
        .find(|file| file.path == declared)
        .with_context(|| format!("package {} records no hook file at {declared}", record.id))?;
    let path = relative_path(declared)?;
    let mut bytes = Vec::new();
    open_package_file(root, &path)?
        .take(MAX_HOOK_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {declared}"))?;
    ensure!(
        bytes.len() as u64 <= MAX_HOOK_BYTES,
        "{declared} exceeds {MAX_HOOK_BYTES} bytes"
    );
    ensure!(
        bytes.len() as u64 == recorded.len && hex::encode(Sha256::digest(&bytes)) == recorded.sha256,
        "the installed hook file {declared} of package {} does not match its record",
        record.id
    );
    String::from_utf8(bytes).with_context(|| format!("read {declared}"))
}

fn read_capped(file: std::fs::File, limit: u64, path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    ensure!(bytes.len() as u64 <= limit, "{} exceeds {limit} bytes", path.display());
    String::from_utf8(bytes).with_context(|| format!("read {}", path.display()))
}
