//! Bearer credentials for the harness API's opt-in TCP listener.
//!
//! One owner-only JSON file under `$HORON_HOME/harness-api/` holds every
//! record. The plaintext token is printed once, at issue or rotate time, and
//! only its `sha256:<hex>` digest is stored, so reading the file never yields a
//! usable token. The bridge re-reads the file for every connection, which is
//! what makes revoke and rotate take effect without a restart; every read is
//! fail-closed, so a file that became a symlink, changed owner or lost its
//! owner-only mode refuses all authentication instead of trusting it.
//!
//! Hosted processes get per-user credentials for free: the path is under
//! `HORON_HOME`, and a per-user container's `HORON_HOME` is that tenant's
//! volume (`packages/docs-site/reference/architecture/hosting.mdx`).

use horon_agent_base::storage::horon_agent_dir;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Directory under `HORON_HOME` that holds the credential store.
pub const CREDENTIALS_DIRECTORY: &str = "harness-api";
/// File name of the credential store.
pub const CREDENTIALS_FILE: &str = "credentials.json";
/// Label used when `api-credentials issue` is given no `--label`.
pub const DEFAULT_LABEL: &str = "default";

const CREDENTIALS_VERSION: u64 = 1;
const TOKEN_BYTES: usize = 32;
const ID_BYTES: usize = 6;
/// Longest accepted credential label, in characters.
pub const MAX_LABEL_CHARS: usize = 64;

/// One issued credential. The record never contains the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRecord {
    /// Stable identifier, also what `rotate` and `revoke` take.
    pub id: String,
    /// Operator-chosen label, unique enough to select a credential by hand.
    pub label: String,
    /// `sha256:<hex>` of the token bytes.
    pub token_hash: String,
    /// RFC 3339 issue time.
    pub created_at: String,
    /// RFC 3339 time of the last rotation, absent until one happens.
    #[serde(default)]
    pub rotated_at: Option<String>,
}

/// What a connection proved, for the bridge's audit line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedCredential {
    /// Record id that matched.
    pub id: String,
    /// Record label that matched.
    pub label: String,
}

/// A freshly issued or rotated credential. `token` is the only copy of the
/// plaintext; the caller prints it exactly once.
#[derive(Debug, Clone)]
pub struct IssuedCredential {
    /// The stored record.
    pub record: CredentialRecord,
    /// The plaintext bearer token, 64 hex characters.
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CredentialFile {
    #[serde(default = "credential_file_version")]
    version: u64,
    #[serde(default)]
    credentials: Vec<CredentialRecord>,
}

const fn credential_file_version() -> u64 {
    CREDENTIALS_VERSION
}

/// The credential store at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStore {
    path: PathBuf,
}

impl CredentialStore {
    /// A store at an explicit path; tests and alternate homes use this.
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// The store for this process's `HORON_HOME`.
    pub fn default_store() -> anyhow::Result<Self> {
        let home = horon_agent_dir()
            .map_err(|error| anyhow::anyhow!("cannot resolve the horon-agent home for API credentials: {error}"))?;
        Ok(Self::at(home.join(CREDENTIALS_DIRECTORY).join(CREDENTIALS_FILE)))
    }

    /// Path of the JSON file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every record, oldest first. Never includes a token.
    pub fn records(&self) -> Result<Vec<CredentialRecord>, CredentialError> {
        let Some(bytes) = read_credential_bytes(&self.path)? else {
            return Ok(Vec::new());
        };
        let file: CredentialFile = serde_json::from_slice(&bytes).map_err(|error| CredentialError::Store {
            path: self.path.clone(),
            reason: format!("it is not valid credential JSON: {error}"),
        })?;
        if file.version != CREDENTIALS_VERSION {
            return Err(CredentialError::Store {
                path: self.path.clone(),
                reason: format!(
                    "it declares version {}, but this build writes version {CREDENTIALS_VERSION}",
                    file.version
                ),
            });
        }
        Ok(file.credentials)
    }

    /// Whether at least one credential exists.
    pub fn has_any(&self) -> Result<bool, CredentialError> {
        Ok(!self.records()?.is_empty())
    }

    /// Refuse with the command that fixes it when the store is empty.
    pub fn require_credential(&self) -> Result<(), CredentialError> {
        if self.has_any()? {
            return Ok(());
        }
        Err(CredentialError::Refused {
            path: self.path.clone(),
            reason: "no API credential exists there; issue one with `horon-agent api-credentials issue`".to_string(),
        })
    }

    /// Issue one credential and return its plaintext token once.
    pub fn issue(&self, label: Option<&str>) -> Result<IssuedCredential, CredentialError> {
        let token = generate_token();
        let record = CredentialRecord {
            id: generate_id(),
            label: normalise_label(label)?,
            token_hash: hash_token(&token),
            created_at: horon_agent_time::Timestamp::now().to_rfc3339(),
            rotated_at: None,
        };
        let mut records = self.records()?;
        records.push(record.clone());
        self.save(&records)?;
        Ok(IssuedCredential { record, token })
    }

    /// Replace the token of the credential named by id or label.
    pub fn rotate(&self, selector: &str) -> Result<IssuedCredential, CredentialError> {
        let mut records = self.records()?;
        let index = find_index(&records, selector)?;
        let token = generate_token();
        let record = &mut records[index];
        record.token_hash = hash_token(&token);
        record.rotated_at = Some(horon_agent_time::Timestamp::now().to_rfc3339());
        let record = record.clone();
        self.save(&records)?;
        Ok(IssuedCredential { record, token })
    }

    /// Delete the credential named by id or label and return it.
    pub fn revoke(&self, selector: &str) -> Result<CredentialRecord, CredentialError> {
        let mut records = self.records()?;
        let index = find_index(&records, selector)?;
        let removed = records.remove(index);
        self.save(&records)?;
        Ok(removed)
    }

    /// Verify a presented token against every record, without an early exit.
    pub fn authenticate(&self, token: &str) -> Result<Option<AuthenticatedCredential>, CredentialError> {
        let presented = match token_digest(token) {
            Some(digest) => digest,
            // A token that is not in this build's shape cannot match any record,
            // so it is rejected before the file is even read.
            None => return Ok(None),
        };
        let records = self.records()?;
        let mut matched: Option<AuthenticatedCredential> = None;
        for record in &records {
            let Some(stored) = stored_digest(&record.token_hash) else {
                // A malformed record cannot authenticate anything; keep scanning
                // so a bad entry never short-circuits a later match.
                continue;
            };
            if constant_time_eq(&stored, &presented) {
                matched = Some(AuthenticatedCredential {
                    id: record.id.clone(),
                    label: record.label.clone(),
                });
            }
        }
        Ok(matched)
    }

    fn save(&self, records: &[CredentialRecord]) -> Result<(), CredentialError> {
        let file = CredentialFile {
            version: CREDENTIALS_VERSION,
            credentials: records.to_vec(),
        };
        let mut text = serde_json::to_string_pretty(&file).map_err(|error| CredentialError::Store {
            path: self.path.clone(),
            reason: format!("cannot serialize the credential store: {error}"),
        })?;
        text.push('\n');
        atomic_owner_write(&self.path, text.as_bytes())
    }
}

fn find_index(records: &[CredentialRecord], selector: &str) -> Result<usize, CredentialError> {
    if let Some(index) = records.iter().position(|record| record.id == selector) {
        return Ok(index);
    }
    let matches: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.label == selector)
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(CredentialError::Unknown {
            selector: selector.to_string(),
        }),
        many => Err(CredentialError::Ambiguous {
            selector: selector.to_string(),
            matches: many.iter().map(|index| records[*index].id.clone()).collect(),
        }),
    }
}

fn normalise_label(label: Option<&str>) -> Result<String, CredentialError> {
    let raw = label.unwrap_or(DEFAULT_LABEL).trim();
    let error = |reason: &str| CredentialError::Store {
        path: PathBuf::from(CREDENTIALS_DIRECTORY).join(CREDENTIALS_FILE),
        reason: format!("the credential label '{raw}' {reason}"),
    };
    if raw.is_empty() {
        return Err(error("is empty; give --label a name"));
    }
    if raw.chars().count() > MAX_LABEL_CHARS {
        return Err(error(&format!("is longer than {MAX_LABEL_CHARS} characters")));
    }
    if raw.chars().any(char::is_control) {
        return Err(error("contains a control character"));
    }
    Ok(raw.to_string())
}

/// A fresh 32-byte token as 64 lowercase hex characters.
fn generate_token() -> String {
    hex::encode(rand::random::<[u8; TOKEN_BYTES]>())
}

/// A fresh identifier that reveals nothing about the token.
fn generate_id() -> String {
    format!("cred-{}", hex::encode(rand::random::<[u8; ID_BYTES]>()))
}

fn hash_token(token: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(token.as_bytes())))
}

fn token_digest(token: &str) -> Option<[u8; 32]> {
    if !horon_agent_base::gateway::is_valid_hex_token(token) {
        return None;
    }
    let digest = Sha256::digest(token.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    Some(bytes)
}

fn stored_digest(token_hash: &str) -> Option<[u8; 32]> {
    let hex_digest = token_hash.strip_prefix("sha256:")?;
    if hex_digest.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    match hex::decode_to_slice(hex_digest, &mut bytes) {
        Ok(()) => Some(bytes),
        Err(_) => None,
    }
}

/// Compare two digests without exiting at the first differing byte.
///
/// Both sides are always 32 bytes, so only the length check can return early
/// and it reveals nothing about the secret. Every byte difference is OR-ed in
/// and inspected once, which is why no `subtle` dependency is needed for a
/// fixed-length digest.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left_byte, right_byte) in left.iter().zip(right) {
        difference |= left_byte ^ right_byte;
    }
    std::hint::black_box(difference) == 0
}

fn store_error(path: &Path, action: &str, error: impl std::fmt::Display) -> CredentialError {
    CredentialError::Store {
        path: path.to_path_buf(),
        reason: format!("cannot {action} it: {error}"),
    }
}

fn refused(path: &Path, reason: impl std::fmt::Display) -> CredentialError {
    CredentialError::Refused {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

fn verify_owner_only(metadata: &std::fs::Metadata, path: &Path) -> Result<(), CredentialError> {
    horon_agent_platform::fs::verify_private_file_metadata(metadata)
        .map_err(|error| refused(path, error.to_string()))?;
    if let Some(bits) = horon_agent_platform::fs::mode_bits(metadata)
        && bits & 0o077 != 0
    {
        return Err(refused(
            path,
            format!("its mode {:04o} is not owner-only (expected 0600)", bits & 0o777),
        ));
    }
    Ok(())
}

fn verify_private_directory(directory: &Path) -> Result<(), CredentialError> {
    let metadata = match std::fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        // A directory that does not exist holds no credentials; the write path
        // creates it owner-only.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(store_error(directory, "inspect", error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(refused(directory, "it must be a real directory, not a symlink or file"));
    }
    horon_agent_platform::fs::verify_private_dir_metadata(&metadata)
        .map_err(|error| refused(directory, error.to_string()))?;
    if let Some(bits) = horon_agent_platform::fs::mode_bits(&metadata)
        && bits & 0o077 != 0
    {
        return Err(refused(
            directory,
            format!("its mode {:04o} is not owner-only (expected 0700)", bits & 0o777),
        ));
    }
    Ok(())
}

fn ensure_private_directory(directory: &Path) -> Result<(), CredentialError> {
    std::fs::create_dir_all(directory).map_err(|error| store_error(directory, "create", error))?;
    let metadata = std::fs::symlink_metadata(directory).map_err(|error| store_error(directory, "inspect", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(refused(directory, "it must be a real directory, not a symlink or file"));
    }
    horon_agent_platform::fs::verify_private_dir_metadata(&metadata)
        .map_err(|error| refused(directory, error.to_string()))?;
    horon_agent_platform::fs::set_dir_permissions_owner_only(directory)
        .map_err(|error| store_error(directory, "narrow to owner-only", error))?;
    verify_private_directory(directory)
}

fn validate_owner_file_if_exists(path: &Path) -> Result<(), CredentialError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(store_error(path, "inspect", error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(refused(path, "it must be a regular file, not a symlink or directory"));
    }
    verify_owner_only(&metadata, path)
}

fn read_credential_bytes(path: &Path) -> Result<Option<Vec<u8>>, CredentialError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(refused(path, "it must be a regular file, not a symlink or directory"));
            }
            verify_owner_only(&metadata, path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(directory) = path.parent() {
                verify_private_directory(directory)?;
            }
            return Ok(None);
        }
        Err(error) => return Err(store_error(path, "inspect", error)),
    }
    if let Some(directory) = path.parent() {
        verify_private_directory(directory)?;
    }
    std::fs::read(path)
        .map(Some)
        .map_err(|error| store_error(path, "read", error))
}

/// Write owner-only through a temporary file and one atomic rename.
fn atomic_owner_write(path: &Path, bytes: &[u8]) -> Result<(), CredentialError> {
    let directory = path
        .parent()
        .ok_or_else(|| refused(path, "it has no parent directory"))?;
    ensure_private_directory(directory)?;
    validate_owner_file_if_exists(path)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| store_error(path, "stamp", error))?
        .as_nanos();
    let temporary = path.with_extension(format!("tmp-{}-{nanos}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    horon_agent_platform::fs::request_owner_only_mode(&mut options);
    let mut file = options
        .open(&temporary)
        .map_err(|error| store_error(&temporary, "create a temporary file", error))?;
    let written = std::io::Write::write_all(&mut file, bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        discard_temporary(&temporary, path)?;
        return Err(store_error(path, "write", error));
    }
    if let Err(error) = horon_agent_platform::fs::atomic_replace(&temporary, path) {
        discard_temporary(&temporary, path)?;
        return Err(store_error(path, "replace atomically", error));
    }
    horon_agent_platform::fs::set_file_permissions_owner_only(path)
        .map_err(|error| store_error(path, "narrow to owner-only", error))?;
    horon_agent_platform::fs::sync_parent_directory(path)
        .map_err(|error| store_error(path, "sync the directory entry", error))?;
    Ok(())
}

fn discard_temporary(temporary: &Path, path: &Path) -> Result<(), CredentialError> {
    match std::fs::remove_file(temporary) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CredentialError::Store {
            path: path.to_path_buf(),
            reason: format!(
                "the temporary file {} could not be removed: {error}",
                temporary.display()
            ),
        }),
    }
}

/// Why a credential operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    /// The store itself could not be read, parsed or written.
    Store { path: PathBuf, reason: String },
    /// The store exists but is not something this build will trust.
    Refused { path: PathBuf, reason: String },
    /// No record carries the selector as id or label.
    Unknown { selector: String },
    /// More than one record carries the selector as label.
    Ambiguous { selector: String, matches: Vec<String> },
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store { path, reason } => write!(formatter, "credential store {}: {reason}", path.display()),
            Self::Refused { path, reason } => write!(
                formatter,
                "refusing credential store {}: {reason}; fix the file's ownership and mode (0600, in a 0700 directory) or remove it",
                path.display()
            ),
            Self::Unknown { selector } => write!(
                formatter,
                "no harness API credential matches '{selector}'; `horon-agent api-credentials list` shows every id and label"
            ),
            Self::Ambiguous { selector, matches } => write!(
                formatter,
                "the label '{selector}' matches {} credentials ({}); name one by id instead",
                matches.len(),
                matches.join(", ")
            ),
        }
    }
}

impl std::error::Error for CredentialError {}

#[cfg(test)]
#[path = "credential_tests.rs"]
mod tests;
