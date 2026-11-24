//! Authenticated encryption (`AES-256-GCM`) for memory volume exports.
//!
//! Each sealed archive entry carries a short magic header (`HME1`), a fresh
//! 96-bit random nonce and the GCM ciphertext+tag. Associated data binds the
//! format version, store revision, `key_id` and tar entry path so ciphertext
//! cannot be swapped across entries, revisions or keys.

use super::key::{KeyMaterial, KeyProvider};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use std::fmt;
use std::path::{Path, PathBuf};

/// Versioned header prefix for sealed export archive entries.
pub const ENTRY_MAGIC: &[u8; 4] = b"HME1";

/// Required AES-256 key length in bytes.
pub const KEY_LEN: usize = 32;

const AAD_DOMAIN: &[u8] = b"horon-memory-export-aead-v1";

/// In-memory [`KeyProvider`] backed by one fixed key id and zeroized bytes.
pub struct StaticKeyProvider {
    key_id: String,
    bytes: Vec<u8>,
}

impl StaticKeyProvider {
    /// Construct a provider for `key_id` and raw key `bytes`.
    pub fn new(key_id: impl Into<String>, bytes: Vec<u8>) -> Result<Self> {
        let key_id = key_id.into();
        if key_id.trim().is_empty() {
            bail!("key_id cannot be empty");
        }
        if bytes.len() != KEY_LEN {
            bail!("AES-256-GCM requires {KEY_LEN} key bytes, got {} byte(s)", bytes.len());
        }
        Ok(Self { key_id, bytes })
    }
}

impl Drop for StaticKeyProvider {
    fn drop(&mut self) {
        self.bytes.fill(0);
        std::hint::black_box(&mut self.bytes);
        self.bytes.clear();
    }
}

impl fmt::Debug for StaticKeyProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StaticKeyProvider")
            .field("key_id", &self.key_id)
            .field("bytes", &format_args!("<redacted {} byte(s)>", self.bytes.len()))
            .finish()
    }
}

impl KeyProvider for StaticKeyProvider {
    fn key_material(&self, key_id: &str) -> Result<Option<KeyMaterial>> {
        if key_id == self.key_id {
            Ok(Some(KeyMaterial::new(key_id, self.bytes.clone())))
        } else {
            Ok(None)
        }
    }
}

/// [`KeyProvider`] resolving 32-byte key material from an environment variable
/// or file when an encrypted operation asks for `key_id`.
#[derive(Debug, Clone)]
pub struct SourceKeyProvider {
    env_var: Option<String>,
    file: Option<PathBuf>,
}

impl SourceKeyProvider {
    /// Construct a provider that reads key bytes from `env_var` or `file`.
    pub fn new(env_var: Option<String>, file: Option<PathBuf>) -> Self {
        Self { env_var, file }
    }
}

impl KeyProvider for SourceKeyProvider {
    fn key_material(&self, key_id: &str) -> Result<Option<KeyMaterial>> {
        let material = resolve_key_material(key_id, self.env_var.as_deref(), self.file.as_deref())?;
        Ok(Some(material))
    }
}

/// Read 32-byte key material from `env_var` first, then `file`.
pub fn resolve_key_material(key_id: &str, env_var: Option<&str>, file: Option<&Path>) -> Result<KeyMaterial> {
    if key_id.trim().is_empty() {
        bail!("key_id cannot be empty");
    }
    if let Some(variable) = env_var {
        let value = std::env::var(variable)
            .with_context(|| format!("cannot read encryption key environment variable `{variable}`"))?;
        let decoded = parse_key_material_text(&value)
            .with_context(|| format!("invalid AES-256-GCM key in environment variable `{variable}`"))?;
        return Ok(KeyMaterial::new(key_id, decoded));
    }
    if let Some(path) = file {
        let raw = std::fs::read(path).with_context(|| format!("cannot read encryption key file {}", path.display()))?;
        let decoded = parse_key_material_bytes(&raw)
            .with_context(|| format!("invalid AES-256-GCM key in file {}", path.display()))?;
        return Ok(KeyMaterial::new(key_id, decoded));
    }
    bail!("no encryption key source: pass --key-env or --key-file")
}

/// Parse 32 raw bytes or UTF-8 hex/base64 key text into a 32-byte vector.
pub fn parse_key_material_bytes(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() == KEY_LEN {
        return Ok(raw.to_vec());
    }
    let text = std::str::from_utf8(raw).map_err(|_| {
        anyhow!(
            "AES-256-GCM key must be {KEY_LEN} raw bytes, 64 hex characters or base64 encoding {KEY_LEN} bytes (got {} bytes)",
            raw.len()
        )
    })?;
    parse_key_material_text(text)
}

/// Parse trimmed hex (64 chars) or base64 (standard/URL-safe) into 32 bytes.
pub fn parse_key_material_text(raw: &str) -> Result<Vec<u8>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("encryption key input is empty");
    }
    if trimmed.len() == KEY_LEN * 2 && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let bytes = hex::decode(trimmed).map_err(|_| anyhow!("invalid hex encryption key"))?;
        if bytes.len() == KEY_LEN {
            return Ok(bytes);
        }
    }
    for engine in [
        &base64::engine::general_purpose::STANDARD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        &base64::engine::general_purpose::URL_SAFE,
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(bytes) = engine.decode(trimmed)
            && bytes.len() == KEY_LEN
        {
            return Ok(bytes);
        }
    }
    bail!("AES-256-GCM key must decode to {KEY_LEN} bytes (64 hex chars, base64, or {KEY_LEN} raw bytes)")
}

/// Seal every `(entry_path, plaintext)` pair with one `KeyMaterial` handle.
pub fn seal_entries(
    material: KeyMaterial,
    format_version: u32,
    store_revision: u64,
    entries: Vec<(String, Vec<u8>)>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let key_id = material.key_id().to_string();
    material.consume(|key_bytes| {
        let key = aead_key(key_bytes)?;
        let rng = SystemRandom::new();
        let mut sealed = Vec::with_capacity(entries.len());
        for (entry_path, plaintext) in entries {
            let mut nonce_bytes = [0u8; NONCE_LEN];
            rng.fill(&mut nonce_bytes)
                .map_err(|_| anyhow!("failed generating random nonce for `{entry_path}`"))?;
            let nonce = Nonce::assume_unique_for_key(nonce_bytes);
            let aad_bytes = build_aad(format_version, store_revision, &key_id, &entry_path);

            let mut buffer =
                Vec::with_capacity(ENTRY_MAGIC.len() + NONCE_LEN + plaintext.len() + AES_256_GCM.tag_len());
            buffer.extend_from_slice(ENTRY_MAGIC);
            buffer.extend_from_slice(&nonce_bytes);
            let payload_start = buffer.len();
            buffer.extend_from_slice(&plaintext);

            let mut payload = buffer.split_off(payload_start);
            key.seal_in_place_append_tag(nonce, Aad::from(&aad_bytes), &mut payload)
                .map_err(|_| anyhow!("failed encrypting archive entry `{entry_path}`"))?;
            buffer.extend_from_slice(&payload);
            sealed.push((entry_path, buffer));
        }
        Ok(sealed)
    })
}

/// Open `metadata_sealed` and all `pages_sealed` entries with `material`.
pub fn open_entries(
    material: KeyMaterial,
    format_version: u32,
    store_revision: u64,
    metadata_path: &str,
    metadata_sealed: &[u8],
    pages_sealed: Vec<(String, Vec<u8>)>,
) -> Result<(Vec<u8>, Vec<(String, Vec<u8>)>)> {
    let key_id = material.key_id().to_string();
    material.consume(|key_bytes| {
        let key = aead_key(key_bytes)?;
        let metadata = open_with_key(
            &key,
            format_version,
            store_revision,
            &key_id,
            metadata_path,
            metadata_sealed,
        )?;
        let mut pages = Vec::with_capacity(pages_sealed.len());
        for (entry_path, ciphertext) in pages_sealed {
            let plaintext = open_with_key(&key, format_version, store_revision, &key_id, &entry_path, &ciphertext)?;
            pages.push((entry_path, plaintext));
        }
        Ok((metadata, pages))
    })
}

fn aead_key(key_bytes: &[u8]) -> Result<LessSafeKey> {
    if key_bytes.len() != KEY_LEN {
        bail!(
            "AES-256-GCM requires {KEY_LEN} key bytes, got {} byte(s)",
            key_bytes.len()
        );
    }
    let unbound = UnboundKey::new(&AES_256_GCM, key_bytes).map_err(|_| anyhow!("invalid AES-256-GCM key material"))?;
    Ok(LessSafeKey::new(unbound))
}

fn open_with_key(
    key: &LessSafeKey,
    format_version: u32,
    store_revision: u64,
    key_id: &str,
    entry_path: &str,
    sealed: &[u8],
) -> Result<Vec<u8>> {
    let min_len = ENTRY_MAGIC.len() + NONCE_LEN + AES_256_GCM.tag_len();
    if sealed.len() < min_len {
        bail!("encrypted entry `{entry_path}` is truncated");
    }
    if &sealed[..ENTRY_MAGIC.len()] != ENTRY_MAGIC {
        bail!("encrypted entry `{entry_path}` has an unexpected header");
    }
    let nonce_start = ENTRY_MAGIC.len();
    let cipher_start = nonce_start + NONCE_LEN;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    nonce_bytes.copy_from_slice(&sealed[nonce_start..cipher_start]);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let aad_bytes = build_aad(format_version, store_revision, key_id, entry_path);

    let mut buf = sealed[cipher_start..].to_vec();
    let plaintext = key
        .open_in_place(nonce, Aad::from(&aad_bytes), &mut buf)
        .map_err(|_| anyhow!("cannot decrypt archive entry `{entry_path}`: authentication failed"))?;
    Ok(plaintext.to_vec())
}

fn build_aad(format_version: u32, store_revision: u64, key_id: &str, entry_path: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_DOMAIN.len() + 4 + 8 + 8 + key_id.len() + entry_path.len());
    aad.extend_from_slice(AAD_DOMAIN);
    aad.extend_from_slice(&format_version.to_le_bytes());
    aad.extend_from_slice(&store_revision.to_le_bytes());
    aad.extend_from_slice(&(key_id.len() as u32).to_le_bytes());
    aad.extend_from_slice(key_id.as_bytes());
    aad.extend_from_slice(&(entry_path.len() as u32).to_le_bytes());
    aad.extend_from_slice(entry_path.as_bytes());
    aad
}
