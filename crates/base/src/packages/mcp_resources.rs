//! The MCP servers one plugin declares as resources.
//!
//! A plugin names its MCP definition files in `[resources] mcp`. Each file is a
//! JSON object holding an `mcpServers` map - the shape every MCP client already
//! reads - and every entry is one of two forms:
//!
//! ```json
//! {"mcpServers": {
//!   "echo": {"type": "stdio", "mode": "mcp"},
//!   "clock": {"type": "remote", "server": "corp-clock"}
//! }}
//! ```
//!
//! A `stdio` entry runs the package's own installed executable with `mode` (and
//! any extra `args`) after the installed root. The host starts that child
//! through the shared package supervision, so it is digest-checked, pinned and
//! stopped inside the package's own process scope like every other package
//! process. A `remote` entry names no endpoint of its own: it refers to a server
//! the user already configured, and the user's connection settings are what the
//! session registers. A package therefore never adds a URL, a credential or a
//! login the user did not provide.
//!
//! The host registers every one of them as `plugin_<id>_<name>`, so a package
//! can never take a name a user source already uses. Definitions are validated
//! when the package installs and read again against the recorded digests when a
//! generation's servers are resolved, so the bytes that run come from the copy
//! the catalog pinned.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::catalog::StoredFile;
use super::manifest::open_package_file;
use super::store::{self, Layout};
use super::{PackageKind, PackageRecord, PackageState, PackageStore, PluginManifest};

/// Largest definition file this host reads.
const MAX_DEFINITION_BYTES: u64 = 64 * 1024;
/// Largest `plugin.toml` this host re-reads to find the declared files.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
/// Longest name this host registers for one package server.
const MAX_PUBLIC_NAME: usize = 64;
/// Longest local name a definition file may use.
const MAX_LOCAL_NAME: usize = 32;
/// Longest MCP mode token a definition may declare.
const MAX_MODE_LEN: usize = 64;
/// How many arguments one stdio entry may add after its mode.
const MAX_ARGS: usize = 16;
/// Longest single argument a definition may declare.
const MAX_ARG_LEN: usize = 128;
/// Longest server reference a remote entry may name.
const MAX_REFERENCE_LEN: usize = 128;
/// Longest reply timeout a definition may ask for.
const MAX_TIMEOUT_SECS: u64 = 3600;
/// The plugin manifest of a copy.
const PLUGIN_FILE: &str = "plugin.toml";

/// One MCP server a package declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageMcpServer {
    /// Package that declared it.
    pub package_id: String,
    /// Name this host registers, `plugin_<id>_<name>`.
    pub public_name: String,
    pub kind: PackageMcpKind,
}

/// How a declared server is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageMcpKind {
    /// The package's own executable, started in its declared MCP mode.
    Stdio {
        /// Absolute path of the installed executable.
        executable: PathBuf,
        /// The mode argument the host passes first.
        mode: String,
        /// Arguments after the mode, in declaration order.
        args: Vec<String>,
        /// Per-request reply timeout the definition asked for.
        timeout_secs: Option<u64>,
    },
    /// A connection config the user already provides, by server name.
    Remote {
        /// Name of the user's own MCP server.
        server: String,
    },
}

impl PackageStore {
    /// The MCP servers one installed record declares.
    ///
    /// The definitions are read from the published copy the record names, and
    /// every file is checked against the length and digest the catalog recorded
    /// before a byte of it is parsed. A record that is not installed, that is not
    /// a plugin, or that declares no MCP resource yields no servers.
    pub fn mcp_servers(&self, record: &PackageRecord) -> Result<Vec<PackageMcpServer>> {
        if record.state != PackageState::Installed || !matches!(record.kind, PackageKind::Plugin) {
            return Ok(Vec::new());
        }
        let (Some(digest), Some(executable)) = (record.digest.as_deref(), record.executable.as_deref()) else {
            return Ok(Vec::new());
        };
        let copy = store::digest_dir(&Layout::new(self.root()), digest)?;
        let declared = declared_files(&copy)?;
        if declared.is_empty() {
            return Ok(Vec::new());
        }
        let executable = copy.join(executable);
        let mut servers: Vec<PackageMcpServer> = Vec::new();
        let mut names = BTreeSet::new();
        for path in &declared {
            let recorded = record
                .files
                .iter()
                .find(|file| file.path.as_str() == path.as_str())
                .with_context(|| format!("package {} does not record MCP definitions {path}", record.id))?;
            for (name, kind) in read_verified_definitions(&copy, recorded, &record.id)? {
                let public_name = public_name(&record.id, &name)?;
                ensure!(
                    names.insert(public_name.clone()),
                    "package {} declares MCP server {name} twice",
                    record.id
                );
                let kind = match kind {
                    Declared::Stdio {
                        mode,
                        args,
                        timeout_secs,
                    } => PackageMcpKind::Stdio {
                        executable: executable.clone(),
                        mode,
                        args,
                        timeout_secs,
                    },
                    Declared::Remote { server } => PackageMcpKind::Remote { server },
                };
                servers.push(PackageMcpServer {
                    package_id: record.id.clone(),
                    public_name,
                    kind,
                });
            }
        }
        Ok(servers)
    }
}

/// Report every definition file the plugin declares that this host can serve.
///
/// Installation runs this before anything is published. A definition this host
/// cannot read is a warning, not an install error: the package still installs,
/// and the servers that do validate are the ones it serves. Reading a
/// generation's servers runs the same checks again, so a hand-edited copy is
/// refused instead of half-registered.
pub(super) fn validate(root: &Path, declared: &[String], package_id: &str) -> Result<()> {
    for path in declared {
        match read_definitions(root, Path::new(path), package_id) {
            Ok(definitions) => {
                for (name, _) in definitions {
                    public_name(package_id, &name)?;
                }
            }
            Err(error) => crate::logging::warn(&format!(
                "MCP: package {package_id} declares no usable MCP server: {error:#}"
            )),
        }
    }
    Ok(())
}

/// The name this host registers for one declared server.
fn public_name(package_id: &str, name: &str) -> Result<String> {
    let public = format!("plugin_{package_id}_{name}");
    ensure!(
        public.len() <= MAX_PUBLIC_NAME,
        "MCP server name '{public}' exceeds {MAX_PUBLIC_NAME} bytes"
    );
    Ok(public)
}

/// One definition file as its author writes it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefinitionFile<'a> {
    #[serde(borrow, rename = "mcpServers", alias = "servers")]
    servers: std::collections::BTreeMap<std::borrow::Cow<'a, str>, &'a serde_json::value::RawValue>,
}

/// One entry of a definition file, before its type decides what is required.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefinitionEntry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// A validated entry, without the executable a record supplies.
enum Declared {
    Stdio {
        mode: String,
        args: Vec<String>,
        timeout_secs: Option<u64>,
    },
    Remote {
        server: String,
    },
}

/// Read one declared definition file into `(local name, kind)` pairs.
fn parse_definitions(text: &str, path: &Path, package_id: &str) -> Result<Vec<(String, Declared)>> {
    let parsed: DefinitionFile<'_> = serde_json::from_str(text)
        .with_context(|| format!("parse the MCP definitions {} of package {package_id}", path.display()))?;
    let mut definitions = Vec::with_capacity(parsed.servers.len());
    for (name, raw) in parsed.servers {
        ensure!(
            valid_name(&name, MAX_LOCAL_NAME),
            "package {package_id} declares an invalid MCP server name: {name}"
        );
        let entry: DefinitionEntry = serde_json::from_str(raw.get())
            .with_context(|| format!("parse MCP server '{name}' of package {package_id}"))?;
        definitions.push((name.into_owned(), declared_kind(entry, package_id)?));
    }
    Ok(definitions)
}

fn read_definitions(root: &Path, path: &Path, package_id: &str) -> Result<Vec<(String, Declared)>> {
    let file =
        open_package_file(root, path).with_context(|| format!("open the MCP definitions of package {package_id}"))?;
    let text = read_capped(file, MAX_DEFINITION_BYTES, path)?;
    parse_definitions(&text, path, package_id)
}

fn read_verified_definitions(root: &Path, recorded: &StoredFile, package_id: &str) -> Result<Vec<(String, Declared)>> {
    let path = Path::new(&recorded.path);
    let mut file = open_package_file(root, path)
        .with_context(|| format!("open the installed MCP definitions {}", recorded.path))?;
    let bytes = super::text_resources::read_capped_prefix(&mut file, MAX_DEFINITION_BYTES, &recorded.path)?;
    let mut len = u64::try_from(bytes.len()).context("the MCP definition chunk length overflowed")?;
    let hex_buf = if len <= MAX_DEFINITION_BYTES {
        super::text_resources::sha256_hex_buf(&bytes)
    } else {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let mut buffer = [0u8; 8 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .with_context(|| format!("read the installed MCP definitions {}", recorded.path))?;
            if read == 0 {
                break;
            }
            len += u64::try_from(read).context("the MCP definition chunk length overflowed")?;
            hasher.update(&buffer[..read]);
        }
        super::text_resources::format_sha256_hex(hasher.finalize().into())
    };
    let sha256 = std::str::from_utf8(&hex_buf).unwrap_or("");
    ensure!(
        len == recorded.len && sha256 == recorded.sha256,
        "the installed MCP definitions {} are {len} bytes with sha256 {sha256}, recorded {} bytes with {}",
        recorded.path,
        recorded.len,
        recorded.sha256,
    );
    ensure!(
        len <= MAX_DEFINITION_BYTES,
        "{} exceeds {MAX_DEFINITION_BYTES} bytes",
        recorded.path
    );
    let text = std::str::from_utf8(&bytes).with_context(|| format!("{} is not UTF-8", recorded.path))?;
    parse_definitions(text, path, package_id)
}

/// Classify one entry and check what its type requires.
fn declared_kind(entry: DefinitionEntry, package_id: &str) -> Result<Declared> {
    match entry.kind.as_str() {
        "stdio" => {
            let mode = entry
                .mode
                .with_context(|| format!("package {package_id}: a stdio MCP server must declare a mode"))?;
            ensure!(valid_mode(&mode), "package {package_id}: invalid MCP mode {mode:?}");
            ensure!(
                entry.server.is_none(),
                "package {package_id}: a stdio MCP server must not name a server"
            );
            ensure!(
                entry.args.len() <= MAX_ARGS,
                "package {package_id}: a stdio MCP server takes at most {MAX_ARGS} arguments"
            );
            for argument in &entry.args {
                ensure!(
                    valid_argument(argument),
                    "package {package_id}: invalid MCP argument {argument:?}"
                );
            }
            if let Some(timeout) = entry.timeout_secs {
                ensure!(
                    (1..=MAX_TIMEOUT_SECS).contains(&timeout),
                    "package {package_id}: an MCP timeout must be 1 to {MAX_TIMEOUT_SECS} seconds"
                );
            }
            Ok(Declared::Stdio {
                mode,
                args: entry.args,
                timeout_secs: entry.timeout_secs,
            })
        }
        "remote" => {
            let server = entry
                .server
                .with_context(|| format!("package {package_id}: a remote MCP server must name a configured server"))?;
            ensure!(
                valid_reference(&server),
                "package {package_id}: invalid MCP server reference {server:?}"
            );
            ensure!(
                entry.mode.is_none() && entry.args.is_empty() && entry.timeout_secs.is_none(),
                "package {package_id}: a remote MCP server takes its settings from the user's connection"
            );
            Ok(Declared::Remote { server })
        }
        other => bail!("package {package_id}: unknown MCP server type {other:?}"),
    }
}

/// The definition files the plugin's `plugin.toml` declares.
fn declared_files(root: &Path) -> Result<Vec<String>> {
    let path = Path::new(PLUGIN_FILE);
    if !root.join(path).is_file() {
        return Ok(Vec::new());
    }
    let file = open_package_file(root, path).context("open the installed plugin.toml")?;
    let text = read_capped(file, MAX_MANIFEST_BYTES, path)?;
    let plugin: PluginManifest = toml::from_str(&text).context("parse the installed plugin.toml")?;
    Ok(plugin.resources.mcp)
}

/// Read `file` up to `limit` bytes as UTF-8 text.
fn read_capped(file: std::fs::File, limit: u64, path: &Path) -> Result<String> {
    let label = path.display().to_string();
    let bytes = super::text_resources::read_capped_file(file, limit, &label)?;
    String::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8"))
}

/// A local server name: lowercase ASCII letters, digits, `-` and `_`.
fn valid_name(name: &str, limit: usize) -> bool {
    name.len() <= limit && super::text_resources::is_resource_name(name)
}

/// A mode token: one argv element without whitespace or control bytes.
fn valid_mode(mode: &str) -> bool {
    !mode.is_empty()
        && mode.len() <= MAX_MODE_LEN
        && mode
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._:".contains(&byte))
}

/// One extra argv element: non-empty, bounded, and free of control bytes.
fn valid_argument(argument: &str) -> bool {
    !argument.is_empty() && argument.len() <= MAX_ARG_LEN && !argument.bytes().any(|byte| byte.is_ascii_control())
}

/// The name of a user server a remote entry refers to.
fn valid_reference(server: &str) -> bool {
    !server.is_empty() && server.len() <= MAX_REFERENCE_LEN && !server.bytes().any(|byte| byte.is_ascii_control())
}
