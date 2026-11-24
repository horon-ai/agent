//! The declaration an installed package carries.
//!
//! An installed copy holds the extension declaration (`extension.toml`) at its
//! root, or a plugin declaration (`plugin.toml`) that names the one bundled
//! `extension.toml`. The digest the host compares against its installation
//! record is the SHA-256 of the raw `extension.toml` bytes, written as lowercase
//! hex, so a standalone extension and a plugin that bundles identical bytes
//! report the same digest.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use horon_agent_extension_types::wire::{
    CapabilitySet, HandshakeError, HostExpectation, InitializeParams, InitializeResult, ProtocolRange, ProtocolVersion,
    verify_offer,
};
use horon_agent_extension_types::{
    EXTENSION_PROTOCOL, ExtensionManifest, MANIFEST_VERSION, PackageKind, PluginManifest,
};
use sha2::{Digest, Sha256};

const EXTENSION_FILE: &str = "extension.toml";
const PLUGIN_FILE: &str = "plugin.toml";

/// Largest declaration file this SDK reads. The installer applies the same cap,
/// so a declaration that installs can always be read back by the child.
const MAX_DECLARATION_BYTES: u64 = 256 * 1024;

/// Protocol range this SDK speaks. Both ends of one wire implement the same
/// range, so a package never picks a protocol version of its own.
pub const PROTOCOL_RANGE: ProtocolRange = ProtocolRange::exactly(ProtocolVersion::new(1, 0));

/// One installed package's declaration, read from the files the host installed.
#[derive(Debug)]
pub struct Declaration {
    manifest: ExtensionManifest,
    kind: PackageKind,
    digest: String,
    path: PathBuf,
}

impl Declaration {
    /// Read the declaration under `root`.
    ///
    /// A root holding `plugin.toml` is a plugin and hashes the bundled
    /// `extension.toml` that `plugin.toml` names; a root holding `extension.toml`
    /// is a standalone extension and hashes that file.
    pub fn load(root: impl AsRef<Path>) -> Result<Self, LoadError> {
        let root = root.as_ref();
        let plugin = root.join(PLUGIN_FILE);
        let standalone = root.join(EXTENSION_FILE);
        match (plugin.is_file(), standalone.is_file()) {
            (true, false) => Self::load_plugin(root, &plugin),
            (false, true) => Self::read(&standalone, PackageKind::Extension),
            (true, true) => Err(LoadError::Layout {
                reason: "the package root holds both plugin.toml and extension.toml",
            }),
            (false, false) => Err(LoadError::Layout {
                reason: "the package root holds neither plugin.toml nor extension.toml",
            }),
        }
    }

    fn load_plugin(root: &Path, plugin: &Path) -> Result<Self, LoadError> {
        let plugin: PluginManifest = read_manifest(plugin)?;
        if plugin.schema_version != MANIFEST_VERSION {
            return Err(LoadError::Schema {
                declared: plugin.schema_version,
            });
        }
        check_identity(&plugin.id, &plugin.name)?;
        let bundled = relative(&plugin.extension).ok_or(LoadError::Layout {
            reason: "plugin.toml must name a relative extension.toml",
        })?;
        let declaration = Self::read(&root.join(bundled), PackageKind::Plugin)?;
        if plugin.id != declaration.manifest.id || plugin.version != declaration.manifest.version {
            return Err(LoadError::Layout {
                reason: "plugin.toml and its bundled extension.toml name different packages",
            });
        }
        Ok(declaration)
    }

    fn read(path: &Path, kind: PackageKind) -> Result<Self, LoadError> {
        let bytes = read_bytes(path)?;
        let manifest: ExtensionManifest = parse(&bytes, path)?;
        if manifest.schema_version != MANIFEST_VERSION {
            return Err(LoadError::Schema {
                declared: manifest.schema_version,
            });
        }
        if manifest.protocol != EXTENSION_PROTOCOL {
            let _ = ProtocolVersion::parse(&manifest.protocol);
            return Err(LoadError::Protocol {
                declared: manifest.protocol.clone(),
            });
        }
        check_identity(&manifest.id, &manifest.name)?;
        Ok(Self {
            manifest,
            kind,
            digest: sha256_hex(&bytes),
            path: path.to_path_buf(),
        })
    }

    /// The declaration file that was read and hashed.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this copy is a standalone extension or a plugin's bundled one.
    pub fn kind(&self) -> PackageKind {
        self.kind
    }

    /// The parsed declaration.
    pub fn manifest(&self) -> &ExtensionManifest {
        &self.manifest
    }

    /// Lowercase hex SHA-256 of the declaration file's raw bytes.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Capabilities the declaration grants: one per non-empty declaration list.
    pub fn capabilities(&self) -> CapabilitySet {
        self.manifest.capabilities()
    }

    /// Check the host's `initialize` offer against this record and answer it.
    ///
    /// The offer must name this package's id and kind, and its protocol range
    /// must intersect [`PROTOCOL_RANGE`]. The answer reports this package's
    /// range, the capabilities both sides offered and the digest the host
    /// compares with its installation record.
    pub fn accept(&self, params: &InitializeParams) -> Result<InitializeResult, HandshakeError> {
        let capabilities = self.capabilities();
        let expectation = HostExpectation {
            protocol: PROTOCOL_RANGE,
            capabilities: &capabilities,
            declared: &capabilities,
            kind: self.kind,
            package_id: &self.manifest.id,
            declaration_digest: &self.digest,
        };
        verify_offer(&expectation, params)?;
        if params.protocol.intersection(&PROTOCOL_RANGE).is_none() {
            return Err(HandshakeError::ProtocolIncompatible {
                host: params.protocol,
                extension: PROTOCOL_RANGE,
            });
        }
        Ok(InitializeResult {
            protocol: PROTOCOL_RANGE,
            capabilities: capabilities.intersection(&params.capabilities),
            declaration_digest: self.digest.clone(),
        })
    }
}

/// Why an installed declaration could not be loaded or served.
#[derive(Debug)]
pub enum LoadError {
    /// The declaration file could not be read.
    Io { path: PathBuf, source: std::io::Error },
    /// The package root does not lay out one package this SDK can serve.
    Layout { reason: &'static str },
    /// The file is larger than the installer's declaration cap.
    TooLarge { limit: u64 },
    /// The file is not UTF-8, or is not the declaration schema it claims.
    Manifest { path: PathBuf, message: String },
    /// The declaration names another manifest schema version.
    Schema { declared: u32 },
    /// The declaration names a protocol this SDK does not speak.
    Protocol { declared: String },
    /// The declaration's identity is not a valid package identity.
    Identity { reason: &'static str },
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "read {}: {source}", path.display()),
            Self::Layout { reason } => write!(formatter, "{reason}"),
            Self::TooLarge { limit } => write!(formatter, "the declaration exceeds {limit} bytes"),
            Self::Manifest { path, message } => write!(formatter, "parse {}: {message}", path.display()),
            Self::Schema { declared } => write!(formatter, "unsupported declaration schema version {declared}"),
            Self::Protocol { declared } => {
                write!(
                    formatter,
                    "the declaration speaks protocol {declared}, which this SDK does not"
                )
            }
            Self::Identity { reason } => write!(formatter, "{reason}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, LoadError> {
    let io = |source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    if len > MAX_DECLARATION_BYTES {
        return Err(LoadError::TooLarge {
            limit: MAX_DECLARATION_BYTES,
        });
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(MAX_DECLARATION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_DECLARATION_BYTES {
        return Err(LoadError::TooLarge {
            limit: MAX_DECLARATION_BYTES,
        });
    }
    Ok(bytes)
}

fn read_manifest<T: for<'de> serde::Deserialize<'de>>(path: &Path) -> Result<T, LoadError> {
    let bytes = read_bytes(path)?;
    parse(&bytes, path)
}

fn parse<T: for<'de> serde::Deserialize<'de>>(bytes: &[u8], path: &Path) -> Result<T, LoadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| LoadError::Manifest {
        path: path.to_path_buf(),
        message: "the declaration is not UTF-8".to_owned(),
    })?;
    toml::from_str(text).map_err(|error| LoadError::Manifest {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn check_identity(id: &str, name: &str) -> Result<(), LoadError> {
    let valid_id = (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !valid_id {
        return Err(LoadError::Identity {
            reason: "a package id is 1 to 32 lowercase ASCII letters, digits or hyphens",
        });
    }
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(LoadError::Identity {
            reason: "a package display name is not empty and holds no control character",
        });
    }
    Ok(())
}

/// One portable relative path whose last component is `extension.toml`.
fn relative(raw: &str) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    for component in raw.split('/') {
        let portable = !component.is_empty()
            && component.is_ascii()
            && !component.starts_with(' ')
            && !component.ends_with(['.', ' '])
            && !component
                .bytes()
                .any(|byte| byte.is_ascii_control() || b"\\:<>\"|?*".contains(&byte));
        if !portable || component == "." || component == ".." {
            return None;
        }
        path.push(component);
    }
    let file_name = path.file_name()?.to_str()?;
    if file_name != EXTENSION_FILE {
        return None;
    }
    Some(path)
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter().copied() {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}
