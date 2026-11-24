//! Tar export and import of one user's memory wiki.
//!
//! The archive is deliberately plain: `wiki/` holds the page files byte for
//! byte, `metadata.jsonl` holds one line per page without its body, and
//! `manifest.json` holds the format version, the store revision, the encryption
//! field and a digest per file. No key, token or signed URL is in it; delivering
//! the tar to the user is the control plane's job, and this module only produces
//! and consumes the file.
//!
//! # Export
//!
//! [`export`] takes the store's writer lock for the whole read, so the tar is
//! one consistent revision instead of a mix of two. It writes to a temporary
//! file beside the caller's path and renames it, so an interrupted export never
//! leaves a truncated tar where an importer would find it.
//!
//! # Import
//!
//! [`import`] unpacks into a **new, empty** store; a store root that already
//! holds anything is refused, because merging two wikis would silently resolve
//! two pages with one id. It then checks the archive structurally and
//! cryptographically before writing a single page:
//!
//! * a path that is absolute, escapes with `..`, or is a symlink, hardlink,
//!   device or fifo is refused;
//! * the entry count and the expanded size are bounded;
//! * `manifest.json` must exist exactly once and `validate` must pass;
//! * every page file's size and SHA-256 must match its manifest row, and the
//!   manifest must list exactly the page files the archive carries;
//! * every page file must decode as a page and have a metadata line naming the
//!   same id and path.
//!
//! After writing, the index is rebuilt from the markdown — the index is derived
//! data, so nothing from the archive's index is trusted — and the imported page
//! count must equal the manifest's.

use super::crypto;
use super::key::{self, KeyMaterial, KeyProvider};
use super::manifest::{
    ENCRYPTION_AES_256_GCM, ENCRYPTION_NONE, FORMAT_VERSION, FileDigest, MANIFEST_FILE, MAX_ENTRIES, MAX_PAGE_BYTES,
    MAX_TOTAL_BYTES, METADATA_FILE, Manifest, PageMetadata, WIKI_PREFIX, is_page_path, parse_metadata, render_metadata,
    sha256_hex,
};
use super::{VolumeLayout, write_private_file};
use crate::memory::store::{self, MemoryWiki};
use anyhow::{Context, Result, anyhow, bail};
use horon_agent_memory_types::TrustLevel;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// What one export produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportReport {
    /// The tar the caller asked for.
    pub path: PathBuf,
    /// Pages written into it.
    pub pages: usize,
    /// Bytes of page files, before tar framing.
    pub page_bytes: u64,
    /// Size of the tar itself.
    pub archive_bytes: u64,
    /// The store revision the export read.
    pub store_revision: u64,
}

/// What one import produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportReport {
    /// Pages installed.
    pub pages: usize,
    /// Bytes of page files installed.
    pub page_bytes: u64,
    /// The store revision after the index rebuild.
    pub store_revision: u64,
}

/// Export the memory wiki of `layout` as a plaintext tar at `destination`.
pub fn export(layout: &VolumeLayout, destination: &Path) -> Result<ExportReport> {
    export_inner(layout, destination, None)
}

/// Export the memory wiki of `layout` as an AES-256-GCM encrypted tar using
/// key material for `key_id` fetched from `provider`.
pub fn export_with_key(
    layout: &VolumeLayout,
    destination: &Path,
    provider: &dyn KeyProvider,
    key_id: &str,
) -> Result<ExportReport> {
    let key_id = key_id.trim();
    if key_id.is_empty() {
        bail!("key_id cannot be empty");
    }
    if let Some(record) = key::read_key(layout)?
        && record.is_revoked()
    {
        bail!(
            "cannot export volume: user data key `{}` has been revoked",
            record.key_id
        );
    }
    let material = provider
        .key_material(key_id)?
        .ok_or_else(|| anyhow!("key provider returned no key material for `{key_id}`"))?;
    export_inner(layout, destination, Some((key_id.to_string(), material)))
}

fn export_inner(
    layout: &VolumeLayout,
    destination: &Path,
    encryption: Option<(String, KeyMaterial)>,
) -> Result<ExportReport> {
    let store_root = layout.store_root();
    if !store_root.is_dir() {
        return Err(anyhow!(
            "the memory store {} does not exist; there is nothing to export",
            store_root.display()
        ));
    }
    // One writer at a time, and no writer between the scan and the last byte:
    // the tar must describe exactly one revision.
    let lock = crate::memory::write_lock(&store_root)?;
    let wiki = MemoryWiki::open(&store_root)
        .with_context(|| format!("cannot open the memory store {}", store_root.display()))?;
    let revision = wiki.revision()?;
    let rows = wiki.list(&store::ListFilter {
        include_inactive: true,
        ..store::ListFilter::default()
    })?;
    let wiki_root = wiki.wiki_root();
    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(rows.len());
    let mut lines: Vec<PageMetadata> = Vec::with_capacity(rows.len());
    for row in &rows {
        let id = horon_agent_memory_types::page::PageId::parse(&row.id)
            .with_context(|| format!("the index holds the unstorable page id `{}`", row.id))?;
        let Some((page, bytes)) = store::read_page_and_bytes(&wiki_root, id.file_stem().as_str(), &row.path)? else {
            continue;
        };
        let path = format!("{WIKI_PREFIX}{}", row.path);
        lines.push(metadata_for(&page, &path));
        files.push((path, bytes));
    }
    drop(lock);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    lines.sort_by(|left, right| left.path.cmp(&right.path));

    let page_bytes: u64 = files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    let metadata_plain = render_metadata(&lines)?;

    let (encryption_mode, key_id, tar_files, metadata_bytes) = match encryption {
        None => (ENCRYPTION_NONE.to_string(), None, files, metadata_plain),
        Some((key_id, material)) => {
            let mut to_seal = Vec::with_capacity(files.len() + 1);
            to_seal.push((METADATA_FILE.to_string(), metadata_plain));
            to_seal.extend(files);
            let mut sealed = crypto::seal_entries(material, FORMAT_VERSION, revision, to_seal)?;
            let metadata_sealed = sealed.remove(0).1;
            (
                ENCRYPTION_AES_256_GCM.to_string(),
                Some(key_id),
                sealed,
                metadata_sealed,
            )
        }
    };

    let stored_bytes: u64 = tar_files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    let manifest = Manifest {
        version: FORMAT_VERSION,
        exported_at: super::now_rfc3339(),
        store_revision: revision,
        encryption: encryption_mode,
        key_id,
        pages: tar_files.len(),
        files: tar_files
            .iter()
            .map(|(path, bytes)| FileDigest {
                path: path.clone(),
                bytes: bytes.len() as u64,
                sha256: sha256_hex(bytes),
            })
            .collect(),
        total_bytes: stored_bytes,
    };
    manifest.validate()?;
    let manifest_bytes = manifest.to_json()?;

    let archive = build_tar(&tar_files, &metadata_bytes, &manifest_bytes)?;
    let archive_bytes = archive.len() as u64;
    write_private_file(destination, &archive)?;
    crate::logging::info(&format!(
        "memory volume exported: {} page(s), {archive_bytes} bytes, revision {revision}",
        tar_files.len()
    ));
    Ok(ExportReport {
        path: destination.to_path_buf(),
        pages: tar_files.len(),
        page_bytes,
        archive_bytes,
        store_revision: revision,
    })
}

/// Import a plaintext tar at `source` into the new, empty store of `layout`.
pub fn import(layout: &VolumeLayout, source: &Path) -> Result<ImportReport> {
    import_inner(layout, source, None)
}

/// Import a plaintext or AES-256-GCM encrypted tar at `source` into the new,
/// empty store of `layout`, fetching decryption keys via `provider`.
pub fn import_with_key(layout: &VolumeLayout, source: &Path, provider: &dyn KeyProvider) -> Result<ImportReport> {
    import_inner(layout, source, Some(provider))
}

fn import_inner(layout: &VolumeLayout, source: &Path, provider: Option<&dyn KeyProvider>) -> Result<ImportReport> {
    let store_root = layout.store_root();
    ensure_empty_target(&store_root)?;
    let file = std::fs::File::open(source).with_context(|| format!("cannot open the archive {}", source.display()))?;
    let mut archive = tar::Archive::new(std::io::BufReader::new(file));
    let mut entries: HashMap<String, Vec<u8>> = HashMap::new();
    let mut total_bytes = 0u64;
    let mut count = 0usize;
    for entry in archive.entries().context("cannot read the archive's entries")? {
        let mut entry = entry.context("cannot read an archive entry")?;
        count += 1;
        if count > MAX_ENTRIES + 3 {
            bail!("the archive holds more than {MAX_ENTRIES} entries");
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            bail!(
                "the archive entry `{}` is not a regular file ({kind:?}); only page files and the two metadata files are accepted",
                entry
                    .path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| "<unreadable>".into())
            );
        }
        let path = entry
            .path()
            .context("an archive entry has no readable path")?
            .to_str()
            .ok_or_else(|| anyhow!("an archive entry path is not UTF-8"))?
            .to_string();
        if !is_accepted_path(&path) {
            bail!("the archive entry `{path}` is not a page file or one of the two metadata files");
        }
        let size = entry.header().size().context("an archive entry has no size")?;
        if size > MAX_PAGE_BYTES {
            bail!("the archive entry `{path}` is {size} bytes, over the {MAX_PAGE_BYTES}-byte cap");
        }
        total_bytes = total_bytes
            .checked_add(size)
            .ok_or_else(|| anyhow!("the archive's size overflows"))?;
        if total_bytes > MAX_TOTAL_BYTES {
            bail!("the archive expands to more than {MAX_TOTAL_BYTES} bytes");
        }
        let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("cannot read the archive entry `{path}`"))?;
        if bytes.len() as u64 != size {
            bail!(
                "the archive entry `{path}` declares {size} bytes and holds {}",
                bytes.len()
            );
        }
        if entries.contains_key(&path) {
            bail!("the archive holds the entry `{path}` twice");
        }
        entries.insert(path, bytes);
    }

    let manifest_bytes = entries
        .remove(MANIFEST_FILE)
        .ok_or_else(|| anyhow!("the archive holds no {MANIFEST_FILE}; it is not a memory export"))?;
    let metadata_raw = entries
        .remove(METADATA_FILE)
        .ok_or_else(|| anyhow!("the archive holds no {METADATA_FILE}; it is not a memory export"))?;
    let manifest = Manifest::from_json(&manifest_bytes)?;

    let manifest_by_path: HashMap<&str, &FileDigest> =
        manifest.files.iter().map(|file| (file.path.as_str(), file)).collect();

    // Every remaining archive entry must be listed in the manifest with matching
    // byte count and SHA-256 digest before any decryption or disk write.
    let mut verified_raw: Vec<(String, Vec<u8>)> = Vec::with_capacity(entries.len());
    for (path, bytes) in entries {
        let digest = manifest_by_path
            .get(path.as_str())
            .copied()
            .ok_or_else(|| anyhow!("the archive carries `{path}`, which the manifest does not list"))?;
        if bytes.len() as u64 != digest.bytes {
            bail!(
                "`{path}` is {} bytes but the manifest says {}",
                bytes.len(),
                digest.bytes
            );
        }
        let actual = sha256_hex(&bytes);
        if actual != digest.sha256 {
            bail!("`{path}` does not match its manifest digest");
        }
        verified_raw.push((path, bytes));
    }
    if verified_raw.len() != manifest.pages {
        bail!(
            "the manifest lists {} page(s) but the archive carries {}",
            manifest.pages,
            verified_raw.len()
        );
    }

    let (metadata_bytes, plaintext_pages) = match manifest.encryption.as_str() {
        ENCRYPTION_NONE => (metadata_raw, verified_raw),
        ENCRYPTION_AES_256_GCM => {
            let key_id = manifest
                .key_id
                .as_deref()
                .ok_or_else(|| anyhow!("this archive is encrypted with `{ENCRYPTION_AES_256_GCM}` without a key_id"))?;
            let Some(provider) = provider else {
                bail!(
                    "this archive is encrypted with `{ENCRYPTION_AES_256_GCM}` (key `{key_id}`); supply a key provider to import it"
                );
            };
            let material = provider
                .key_material(key_id)?
                .ok_or_else(|| anyhow!("key `{key_id}` is unavailable or revoked; cannot decrypt archive"))?;
            let opened = crypto::open_entries(
                material,
                manifest.version,
                manifest.store_revision,
                METADATA_FILE,
                &metadata_raw,
                verified_raw,
            )?;
            (opened.metadata, opened.pages)
        }
        other => bail!("unsupported archive encryption `{other}`"),
    };

    let metadata = parse_metadata(&metadata_bytes)?;
    if metadata.len() != manifest.pages {
        bail!(
            "the metadata file holds {} line(s) for {} page(s)",
            metadata.len(),
            manifest.pages
        );
    }
    let metadata_by_path: HashMap<&str, &PageMetadata> =
        metadata.iter().map(|line| (line.path.as_str(), line)).collect();

    let mut verified: Vec<(String, Vec<u8>, &str)> = Vec::with_capacity(plaintext_pages.len());
    for (path, bytes) in plaintext_pages {
        let line = metadata_by_path
            .get(path.as_str())
            .copied()
            .ok_or_else(|| anyhow!("the archive carries `{path}` with no metadata line"))?;
        verified.push((path, bytes, line.id.as_str()));
    }

    // Write the page files, then rebuild the index from the markdown. Nothing
    // from the archive's derived data is imported.
    verified.sort_by(|left, right| left.0.cmp(&right.0));
    let wiki_root = store_root.join("wiki");
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();
    let mut installed_bytes = 0u64;
    for (path, bytes, expected_id) in &verified {
        let Some(relative) = path.strip_prefix(WIKI_PREFIX) else {
            bail!("`{path}` is not below `{WIKI_PREFIX}`");
        };
        let text = std::str::from_utf8(bytes).map_err(|_| anyhow!("`{path}` is not UTF-8"))?;
        let stem = Path::new(relative)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow!("`{path}` has no UTF-8 file stem"))?;
        let page = store::decode_page(text, stem)?;
        if *expected_id != page.id.as_str() {
            bail!(
                "`{path}` decodes as page `{}` but its metadata line names `{expected_id}`",
                page.id
            );
        }
        let target = store::safe_join(&wiki_root, relative)?;
        if let Some(parent) = target.parent()
            && created_dirs.insert(parent.to_path_buf())
        {
            create_private_dir(parent)?;
        }
        write_private_file(&target, bytes)?;
        installed_bytes += bytes.len() as u64;
    }

    let mut wiki = MemoryWiki::open(&store_root)
        .with_context(|| format!("cannot build the index for {}", store_root.display()))?;
    if wiki.last_report().indexed != verified.len() || !wiki.last_report().skipped.is_empty() {
        wiki.reindex()
            .context("cannot rebuild the index from the imported pages")?;
    }
    let counts = wiki.stats()?;
    let expected = i64::try_from(verified.len()).unwrap_or(i64::MAX);
    if counts.pages != expected {
        bail!(
            "the imported store holds {} page(s) after the rebuild but the archive declares {expected}",
            counts.pages
        );
    }
    let revision = wiki.revision()?;
    crate::logging::info(&format!(
        "memory volume imported: {} page(s), {} bytes, revision {revision}",
        verified.len(),
        installed_bytes
    ));
    Ok(ImportReport {
        pages: verified.len(),
        page_bytes: installed_bytes,
        store_revision: revision,
    })
}

/// Refuse a store root that already holds anything, so an import can never
/// merge two wikis.
fn ensure_empty_target(store_root: &Path) -> Result<()> {
    match std::fs::read_dir(store_root) {
        Ok(mut entries) => {
            if let Some(entry) = entries.next() {
                let entry = entry.context("cannot list the existing store")?;
                bail!(
                    "the import target {} is not empty (it holds `{}`); import into a new home",
                    store_root.display(),
                    entry.file_name().to_string_lossy()
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow!(error).context(format!("cannot inspect {}", store_root.display()))),
    }
}

/// Whether an archive entry path is one this importer accepts.
fn is_accepted_path(path: &str) -> bool {
    path == MANIFEST_FILE || path == METADATA_FILE || is_page_path(path)
}

/// Build the tar in memory.
fn build_tar(files: &[(String, Vec<u8>)], metadata: &[u8], manifest: &[u8]) -> Result<Vec<u8>> {
    let estimated_bytes = files
        .iter()
        .map(|(_, bytes)| 512 + ((bytes.len() + 511) & !511))
        .sum::<usize>()
        + 512
        + ((metadata.len() + 511) & !511)
        + 512
        + ((manifest.len() + 511) & !511)
        + 1024;
    let mut builder = tar::Builder::new(Vec::with_capacity(estimated_bytes));
    builder.mode(tar::HeaderMode::Deterministic);
    for (path, bytes) in files {
        append_bytes(&mut builder, path, bytes)?;
    }
    append_bytes(&mut builder, METADATA_FILE, metadata)?;
    append_bytes(&mut builder, MANIFEST_FILE, manifest)?;
    let archive = builder.into_inner().context("cannot finish the export tar")?;
    Ok(archive)
}

/// Append one owner-only regular file entry.
fn append_bytes(builder: &mut tar::Builder<Vec<u8>>, path: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, bytes)
        .with_context(|| format!("cannot add `{path}` to the export tar"))?;
    Ok(())
}

/// Create a directory owner-only, and tighten an existing one.
fn create_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("cannot create the directory {}", path.display()))?;
    horon_agent_platform::fs::set_dir_permissions_owner_only(path)
        .with_context(|| format!("cannot restrict the directory {}", path.display()))
}

/// Everything about `page` that a metadata line carries, with the body left
/// out and only its size reported.
fn metadata_for(page: &horon_agent_memory_types::MemoryPage, path: &str) -> PageMetadata {
    PageMetadata {
        id: page.id.as_str().to_string(),
        path: path.to_string(),
        kind: page.kind.as_str().to_string(),
        category: page.category.to_string(),
        project: page.project.clone(),
        title: page.title.clone(),
        tags: page.tags.clone(),
        entities: page.entities.clone(),
        created_at: page.created_at.to_rfc3339(),
        updated_at: page.updated_at.to_rfc3339(),
        access_count: page.access_count,
        trust: match page.trust {
            TrustLevel::High => "high",
            TrustLevel::Medium => "medium",
            TrustLevel::Low => "low",
        }
        .to_string(),
        strength: page.strength,
        active: page.active,
        superseded_by: page.superseded_by.clone(),
        valid_from: page.valid_from.clone(),
        valid_to: page.valid_to.clone(),
        confidence: page.confidence,
        source: page.source.clone(),
        body_bytes: page.body.len() as u64,
    }
}
