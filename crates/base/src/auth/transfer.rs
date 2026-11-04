//! Explicit, one-time transfer of one HoronAgent-owned OpenAI or Claude OAuth account.
//!
//! This is not credential discovery or synchronization. Only the selected/active
//! account in `openai-auth.json` or `auth.json` is read. External tools (including
//! previously trusted tools), keychains, environment credentials, API keys, AWS
//! credentials and general configuration are deliberately outside the supported
//! set. Callers must obtain consent before exporting and use a private transport.
//! No provider loaders are used: those may migrate, harden, refresh or discover
//! credentials. Export never writes to its source.
//!
//! Import refuses *every* existing destination store, including an empty,
//! malformed, symlinked or other-provider-only store. Existing provider writers
//! do not share a lock, so read/merge/rename would silently clobber concurrent
//! updates. A no-replace atomic publication is the only supported operation.
//! Other stores are never opened or modified. This is a copy, not a token move:
//! providers may later invalidate either copy when rotating refresh tokens.
//! OpenAI expiry prefers the stored timestamp, then the token's unverified JWT
//! `exp` claim. This is an offline expiry check, not authentication validation.
//! Opaque tokens without an explicit expiry remain supported.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::str::FromStr;

pub const MAX_TRANSFER_BYTES: usize = 64 * 1024;
const MAX_STORE_BYTES: usize = 1024 * 1024;
const TRANSFER_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferProvider {
    OpenAi,
}

impl TransferProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
        }
    }

    fn store_name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai-auth.json",
        }
    }
}

impl FromStr for TransferProvider {
    type Err = TransferError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "openai" => Ok(Self::OpenAi),
            _ => Err(TransferError::UnsupportedProvider),
        }
    }
}

/// Only static diagnostics. Never attach a parser/I/O error or credential value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferError {
    UnsupportedProvider,
    UnsupportedPlatform,
    Unavailable,
    InvalidStore,
    InvalidPayload,
    UnsupportedVersion,
    ProviderMismatch,
    TooLarge,
    Expired,
    ExistingStore,
    UnsafePath,
    Io,
}

impl TransferError {
    pub fn message(self) -> &'static str {
        match self {
            Self::UnsupportedProvider => "Local login import supports only OpenAI OAuth.",
            Self::UnsupportedPlatform => "Secure local login import is not supported on this platform.",
            Self::Unavailable => {
                "No selected HoronAgent-owned OAuth login is available for transfer. Use /login instead."
            }
            Self::InvalidStore => "The local OAuth store is malformed. Use /login instead.",
            Self::InvalidPayload => "The credential transfer payload is invalid.",
            Self::UnsupportedVersion => "The credential transfer version is not supported.",
            Self::ProviderMismatch => "The credential transfer does not match the selected provider.",
            Self::TooLarge => "The credential transfer or source store exceeds the size limit.",
            Self::Expired => "The selected OAuth login has expired. Use /login instead.",
            Self::ExistingStore => {
                "The selected provider's destination store already exists. It was not changed. Use /login instead."
            }
            Self::UnsafePath => "The credential store path is not a safe regular file or private directory.",
            Self::Io => "The credential store could not be accessed securely.",
        }
    }
}

impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for TransferError {}

/// Secret bytes, intentionally without Debug, Display, Clone or Serialize.
/// Do not log these bytes, put them in argv, or persist a transport copy.
pub struct CredentialTransfer {
    bytes: Vec<u8>,
}

impl CredentialTransfer {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    version: u32,
    provider: TransferProvider,
    credential: T,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiCredential {
    access_token: String,
    refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
}

fn validate_tokens(access: &str, refresh: &str, expires: Option<i64>) -> Result<(), TransferError> {
    if access.trim().is_empty() || refresh.trim().is_empty() {
        return Err(TransferError::InvalidPayload);
    }
    if expires.is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp_millis()) {
        return Err(TransferError::Expired);
    }
    Ok(())
}

fn serialize<T: Serialize>(provider: TransferProvider, credential: T) -> Result<CredentialTransfer, TransferError> {
    let bytes = serde_json::to_vec(&Envelope {
        version: TRANSFER_VERSION,
        provider,
        credential,
    })
    .map_err(|_| TransferError::InvalidPayload)?;
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(TransferError::TooLarge);
    }
    Ok(CredentialTransfer { bytes })
}

fn read_store(path: &Path) -> Result<Vec<u8>, TransferError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            TransferError::Unavailable
        } else {
            TransferError::Io
        }
    })?;
    let metadata = file.metadata().map_err(|_| TransferError::Io)?;
    if !metadata.is_file() {
        return Err(TransferError::UnsafePath);
    }
    if metadata.len() > MAX_STORE_BYTES as u64 {
        return Err(TransferError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take((MAX_STORE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| TransferError::Io)?;
    if bytes.len() > MAX_STORE_BYTES {
        return Err(TransferError::TooLarge);
    }
    Ok(bytes)
}

/// Read-only availability, without refresh, migration or external discovery.
/// A missing/expired login is unavailable. Malformed stores remain errors.
pub fn available_local(provider: TransferProvider) -> Result<bool, TransferError> {
    match export_local(provider) {
        Ok(_) => Ok(true),
        Err(TransferError::Unavailable | TransferError::Expired) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Read-only availability in an explicitly isolated HoronAgent data directory.
pub fn available_at(home: &Path, provider: TransferProvider) -> Result<bool, TransferError> {
    match export_at(home, provider) {
        Ok(_) => Ok(true),
        Err(TransferError::Unavailable | TransferError::Expired) => Ok(false),
        Err(error) => Err(error),
    }
}

fn validate_labels<'a>(labels: impl Iterator<Item = &'a str>) -> Result<(), TransferError> {
    let mut seen = std::collections::HashSet::new();
    for label in labels {
        if label.trim().is_empty() || !seen.insert(label) {
            return Err(TransferError::InvalidStore);
        }
    }
    Ok(())
}

/// Export exactly the runtime-selected, stored-active, or first account, in that
/// order. An invalid explicit/active selection is refused, never substituted.
pub fn export_local(provider: TransferProvider) -> Result<CredentialTransfer, TransferError> {
    let home = crate::storage::horon_agent_dir().map_err(|_| TransferError::Io)?;
    let selected = super::account_store::runtime_active_override(provider.as_str());
    export_account_at(&home, provider, selected.as_deref())
}

/// Isolated-store equivalent of export, without process-global account overrides.
pub fn export_at(home: &Path, provider: TransferProvider) -> Result<CredentialTransfer, TransferError> {
    export_account_at(home, provider, None)
}

pub fn import_at(home: &Path, provider: TransferProvider, bytes: &[u8]) -> Result<(), TransferError> {
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(TransferError::TooLarge);
    }
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(TransferError::InvalidPayload);
    }
    let envelope: Envelope<Box<serde_json::value::RawValue>> =
        serde_json::from_slice(bytes).map_err(|_| TransferError::InvalidPayload)?;
    if envelope.version != TRANSFER_VERSION {
        return Err(TransferError::UnsupportedVersion);
    }
    if envelope.provider != provider {
        return Err(TransferError::ProviderMismatch);
    }
    if !envelope.credential.get().starts_with('{') {
        return Err(TransferError::InvalidPayload);
    }
    let store = match provider {
        TransferProvider::OpenAi => {
            let mut credential: OpenAiCredential =
                serde_json::from_str(envelope.credential.get()).map_err(|_| TransferError::InvalidPayload)?;
            credential.expires_at = credential
                .expires_at
                .or_else(|| super::codex::expires_at_from_access_token(&credential.access_token));
            validate_tokens(
                &credential.access_token,
                &credential.refresh_token,
                credential.expires_at,
            )?;
            serde_json::to_vec(&super::codex::HoronAgentOpenAiAuthFile {
                openai_accounts: vec![super::codex::OpenAiAccount {
                    label: "openai-otter".into(),
                    access_token: credential.access_token,
                    refresh_token: credential.refresh_token,
                    id_token: credential.id_token,
                    account_id: credential.account_id,
                    expires_at: credential.expires_at,
                    email: None,
                }],
                active_openai_account: Some("openai-otter".into()),
            })
        }
    }
    .map_err(|_| TransferError::InvalidPayload)?;
    secure_publish(home, provider.store_name(), &store)
}

/// Read a specifically selected account, or the stored-active/first account.
/// `home` is a HoronAgent data directory, not a user home or external tool directory.
pub fn export_account_at(
    home: &Path,
    provider: TransferProvider,
    selected: Option<&str>,
) -> Result<CredentialTransfer, TransferError> {
    let bytes = read_store(&home.join(provider.store_name()))?;
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(TransferError::InvalidStore);
    }
    match provider {
        TransferProvider::OpenAi => {
            let store: super::codex::HoronAgentOpenAiAuthFile =
                serde_json::from_slice(&bytes).map_err(|_| TransferError::InvalidStore)?;
            validate_labels(store.openai_accounts.iter().map(|account| account.label.as_str()))?;
            let label = selected.or(store.active_openai_account.as_deref());
            let account = match label {
                Some(label) => store.openai_accounts.iter().find(|account| account.label == label),
                None => store.openai_accounts.first(),
            }
            .ok_or(TransferError::Unavailable)?;
            let expires_at = account
                .expires_at
                .or_else(|| super::codex::expires_at_from_access_token(&account.access_token));
            validate_tokens(&account.access_token, &account.refresh_token, expires_at)?;
            serialize(
                provider,
                OpenAiCredential {
                    access_token: account.access_token.clone(),
                    refresh_token: account.refresh_token.clone(),
                    id_token: account.id_token.clone(),
                    account_id: account.account_id.clone(),
                    expires_at,
                },
            )
        }
    }
}

#[cfg(not(unix))]
fn secure_publish(_: &Path, _: &str, _: &[u8]) -> Result<(), TransferError> {
    // Do not pretend Unix permission bits enforce a private Windows ACL.
    Err(TransferError::UnsupportedPlatform)
}

#[cfg(unix)]
fn secure_publish(home: &Path, name: &str, bytes: &[u8]) -> Result<(), TransferError> {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;

    // Walk directory descriptors, never following symlinks, including ancestors.
    // Publication remains anchored even if a parent path is renamed concurrently.
    let start = if home.is_absolute() { c"/" } else { c"." };
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::open(start.as_ptr(), flags) };
    if fd < 0 {
        return Err(TransferError::Io);
    }
    let mut directory = unsafe { File::from_raw_fd(fd) };
    let mut saw_component = false;
    for component in home.components() {
        let component = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(component) => component,
            _ => return Err(TransferError::UnsafePath),
        };
        saw_component = true;
        let component = CString::new(component.as_bytes()).map_err(|_| TransferError::UnsafePath)?;
        let mut fd = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            let created = unsafe { libc::mkdirat(directory.as_raw_fd(), component.as_ptr(), 0o700) };
            if created < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
                return Err(TransferError::Io);
            }
            fd = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err(TransferError::UnsafePath);
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    if !saw_component {
        return Err(TransferError::UnsafePath);
    }
    let metadata = directory.metadata().map_err(|_| TransferError::Io)?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(TransferError::UnsafePath);
    }
    let target = CString::new(name).map_err(|_| TransferError::UnsafePath)?;
    // lstat-style check rejects dangling symlinks, directories and malformed files
    // without reading any existing credentials. linkat below is the race guard.
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let exists = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            target.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if exists == 0 {
        return Err(TransferError::ExistingStore);
    }
    if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
        return Err(TransferError::Io);
    }
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
        return Err(TransferError::Io);
    }

    struct Staged<'a> {
        directory: &'a File,
        name: CString,
    }
    impl Drop for Staged<'_> {
        fn drop(&mut self) {
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0);
            }
        }
    }
    let temporary = CString::new(format!(".oauth-transfer-{}", uuid::Uuid::new_v4())).map_err(|_| TransferError::Io)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(TransferError::Io);
    }
    let staged = Staged {
        directory: &directory,
        name: temporary,
    };
    let mut file = unsafe { File::from_raw_fd(fd) };
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(TransferError::Io);
    }
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| TransferError::Io)?;
    // Hard-link creation is atomic and cannot replace an existing destination.
    let result = unsafe {
        libc::linkat(
            directory.as_raw_fd(),
            staged.name.as_ptr(),
            directory.as_raw_fd(),
            target.as_ptr(),
            0,
        )
    };
    if result != 0 {
        return Err(
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
                TransferError::ExistingStore
            } else {
                TransferError::Io
            },
        );
    }
    drop(staged);
    // Publication has already committed. A directory sync failure must not imply
    // the credential was not installed, encouraging a retry or exposing a token.
    let _ = directory.sync_all();
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;
    use std::sync::{Arc, Barrier};

    const FUTURE: i64 = 4_102_444_800_000;

    /// A temporary directory whose `path()` is the canonical spelling.
    ///
    /// On macOS `TempDir::path()` is `/var/folders/...`, and `/var` is a symlink
    /// to `private/var`. `secure_publish` deliberately refuses to follow a
    /// symlinked path component at any depth, so a fixture that hands the
    /// product the `/var` spelling tests the refusal instead of the transfer.
    struct CanonTemp {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
    }

    impl CanonTemp {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn tempdir() -> CanonTemp {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        CanonTemp { _dir: dir, path }
    }

    fn fixture(provider: TransferProvider, token: &str) -> Vec<u8> {
        let credential = match provider {
            TransferProvider::OpenAi => json!({
                "access_token": token, "refresh_token": "synthetic-refresh",
                "expires_at": FUTURE, "account_id": "synthetic-account"
            }),
        };
        serde_json::to_vec(&json!({"version": 1, "provider": provider, "credential": credential})).unwrap()
    }

    #[test]
    fn openai_jwt_expiry_fallback_is_checked_and_persisted_without_source_mutation() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

        let jwt = |seconds: i64| {
            let claims = serde_json::to_vec(&json!({"exp": seconds})).unwrap();
            format!("synthetic.{}.unsigned", URL_SAFE_NO_PAD.encode(claims))
        };
        let provider = TransferProvider::OpenAi;
        for (token, explicit, expected) in [
            (jwt(1), None, Some(1_000)),
            (jwt(FUTURE / 1_000), None, Some(FUTURE)),
            (jwt(1), Some(FUTURE), Some(FUTURE)),
            ("synthetic-opaque-token".to_string(), None, None),
        ] {
            let local = tempdir();
            let remote = tempdir();
            let source = serde_json::to_vec(&json!({
                "openai_accounts": [{
                    "label": "active", "access_token": token,
                    "refresh_token": "synthetic-refresh", "expires_at": explicit
                }], "active_openai_account": "active"
            }))
            .unwrap();
            let path = local.path().join("openai-auth.json");
            fs::write(&path, &source).unwrap();
            let incoming = serde_json::to_vec(&json!({
                "version": 1, "provider": "openai", "credential": {
                    "access_token": token, "refresh_token": "synthetic-refresh",
                    "expires_at": explicit
                }
            }))
            .unwrap();
            if expected == Some(1_000) {
                assert!(matches!(export_at(local.path(), provider), Err(TransferError::Expired)));
                assert_eq!(
                    import_at(remote.path(), provider, &incoming),
                    Err(TransferError::Expired)
                );
                assert_eq!(fs::read_dir(remote.path()).unwrap().count(), 0);
            } else {
                let exported = export_at(local.path(), provider).unwrap();
                let value: Value = serde_json::from_slice(exported.as_bytes()).unwrap();
                assert_eq!(value["credential"]["expires_at"].as_i64(), expected);
                // Exercise incoming envelopes without the export-side normalization.
                import_at(remote.path(), provider, &incoming).unwrap();
                let installed: Value =
                    serde_json::from_slice(&fs::read(remote.path().join("openai-auth.json")).unwrap()).unwrap();
                assert_eq!(installed["openai_accounts"][0]["expires_at"].as_i64(), expected);
                let reexported = export_at(remote.path(), provider).unwrap();
                assert!(exported.as_bytes() == reexported.as_bytes());
            }
            assert!(fs::read(path).unwrap() == source);
        }
    }

    #[test]
    fn racing_imports_have_exactly_one_winner_and_no_partial_or_leftover_files() {
        let remote = tempdir();
        let home = Arc::new(remote.path().join("horon-agent"));
        let barrier = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let home = home.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let payload = fixture(TransferProvider::OpenAi, &format!("synthetic-winner-{index}"));
                    barrier.wait();
                    import_at(&home, TransferProvider::OpenAi, &payload)
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| **result == Err(TransferError::ExistingStore))
                .count(),
            7
        );
        let installed = export_at(&home, TransferProvider::OpenAi).unwrap();
        assert!(
            std::str::from_utf8(installed.as_bytes())
                .unwrap()
                .contains("synthetic-winner-")
        );
        assert_eq!(fs::read_dir(home.as_path()).unwrap().count(), 1);
    }
}
