//! The tar's manifest and its per-page metadata lines.
//!
//! Two artefacts describe an export without carrying it twice.
//!
//! * `manifest.json` is the archive's own table of contents: the version of the
//!   format, when it was written, the store revision, the encryption field, and
//!   a `{path, bytes, sha256}` row for every page file. Import verifies every
//!   row before it writes a page, so a truncated or edited archive is refused
//!   instead of installed.
//! * `metadata.jsonl` is one JSON object per page: identity, kind, category,
//!   project, tags, entities, dates, counts and trust. It deliberately omits the
//!   body, because the body is already the page file and duplicating it would
//!   put memory text in an artefact that a catalog, a search index or a
//!   support-ticket attachment can carry. A reader that needs the body reads the
//!   page file the line points at.
//!
//! Neither file carries a key, a token or a signed URL. The encryption field
//! says which algorithm (if any) the tar was sealed with, so an importer can
//! refuse a tar it cannot open instead of installing ciphertext as markdown.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// File name of the manifest inside the tar.
pub const MANIFEST_FILE: &str = "manifest.json";
/// File name of the metadata lines inside the tar.
pub const METADATA_FILE: &str = "metadata.jsonl";
/// Directory prefix the page files carry inside the tar.
pub const WIKI_PREFIX: &str = "wiki/";

/// The export format version this build writes and accepts.
pub const FORMAT_VERSION: u32 = 1;

/// Longest single page file accepted from an archive.
pub const MAX_PAGE_BYTES: u64 = 2 * 1024 * 1024 + 64 * 1024;
/// Most entries an archive may hold, including directories and the two
/// metadata files.
pub const MAX_ENTRIES: usize = 100_000;
/// Most bytes an archive may expand to.
pub const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The archive's table of contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Format version, so a future shape is refused rather than misread.
    pub version: u32,
    /// When the export ran, RFC 3339.
    pub exported_at: String,
    /// The store revision the export read.
    pub store_revision: u64,
    /// `none` when the tar is plain markdown. Any other value names an
    /// algorithm this build does not implement; see the module notes.
    pub encryption: String,
    /// Page count.
    pub pages: usize,
    /// Per-file digests, one per page file.
    pub files: Vec<FileDigest>,
    /// Sum of every page file's bytes.
    pub total_bytes: u64,
}

/// One page file's digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    /// Path inside the tar, always below [`WIKI_PREFIX`].
    pub path: String,
    /// Size in bytes.
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the file's bytes.
    pub sha256: String,
}

/// One metadata line: everything about a page except its body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageMetadata {
    /// Page id.
    pub id: String,
    /// Path inside the tar.
    pub path: String,
    /// Page kind text.
    pub kind: String,
    /// Memory category text.
    pub category: String,
    /// Project key, when the page is project-scoped.
    pub project: Option<String>,
    /// Page title.
    pub title: String,
    /// Tags.
    pub tags: Vec<String>,
    /// Entities.
    pub entities: Vec<String>,
    /// First write, RFC 3339.
    pub created_at: String,
    /// Last write, RFC 3339.
    pub updated_at: String,
    /// Retrieval count.
    pub access_count: u32,
    /// Trust level text.
    pub trust: String,
    /// Consolidation strength.
    pub strength: u32,
    /// Whether the page is current.
    pub active: bool,
    /// Id of the page that replaced this one.
    pub superseded_by: Option<String>,
    /// First valid date.
    pub valid_from: Option<String>,
    /// Last valid date.
    pub valid_to: Option<String>,
    /// Confidence in `0.0..=1.0`.
    pub confidence: f32,
    /// Where the page came from.
    pub source: Option<String>,
    /// Body size, so a reader can size its buffer without the body itself.
    pub body_bytes: u64,
}

/// The encryption field of a plaintext tar.
pub const ENCRYPTION_NONE: &str = "none";

/// The lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

impl Manifest {
    /// Check every structural invariant an importer relies on.
    pub fn validate(&self) -> Result<()> {
        if self.version != FORMAT_VERSION {
            return Err(anyhow!(
                "this archive is format version {} and this build reads {FORMAT_VERSION}",
                self.version
            ));
        }
        if self.encryption != ENCRYPTION_NONE {
            return Err(anyhow!(
                "this archive is encrypted with `{}`; this build implements no export encryption",
                self.encryption
            ));
        }
        if self.files.len() != self.pages {
            return Err(anyhow!(
                "the manifest lists {} file(s) for {} page(s)",
                self.files.len(),
                self.pages
            ));
        }
        if self.files.len() > MAX_ENTRIES {
            return Err(anyhow!("the manifest lists more than {MAX_ENTRIES} files"));
        }
        let mut total = 0u64;
        for file in &self.files {
            if !is_page_path(&file.path) {
                return Err(anyhow!(
                    "the manifest entry `{}` is not a page path below `{WIKI_PREFIX}`",
                    file.path
                ));
            }
            if file.bytes > MAX_PAGE_BYTES {
                return Err(anyhow!(
                    "the manifest entry `{}` is {} bytes, over the {MAX_PAGE_BYTES}-byte cap",
                    file.path,
                    file.bytes
                ));
            }
            if file.sha256.len() != 64 || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(anyhow!("the manifest entry `{}` has no SHA-256 digest", file.path));
            }
            total = total
                .checked_add(file.bytes)
                .ok_or_else(|| anyhow!("the manifest's byte total overflows"))?;
        }
        if total != self.total_bytes {
            return Err(anyhow!(
                "the manifest says {} bytes but its files sum to {total}",
                self.total_bytes
            ));
        }
        if total > MAX_TOTAL_BYTES {
            return Err(anyhow!(
                "the manifest's {total} bytes exceed the {MAX_TOTAL_BYTES}-byte cap"
            ));
        }
        Ok(())
    }

    /// Find one file's digest by its tar path.
    pub fn digest_for(&self, path: &str) -> Option<&FileDigest> {
        self.files.iter().find(|file| file.path == path)
    }

    /// Encode the manifest as the JSON the tar carries.
    pub fn to_json(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).context("cannot encode the export manifest")
    }

    /// Parse a manifest from the bytes the tar carried.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let manifest: Manifest =
            serde_json::from_slice(bytes).context("the export manifest is not a valid manifest")?;
        manifest.validate()?;
        Ok(manifest)
    }
}

/// Render the metadata lines, one JSON object per page and a trailing newline.
pub fn render_metadata(lines: &[PageMetadata]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for line in lines {
        let encoded = serde_json::to_vec(line).context("cannot encode a metadata line")?;
        out.extend_from_slice(&encoded);
        out.push(b'\n');
    }
    Ok(out)
}

/// Parse the metadata file, refusing a malformed line by number.
pub fn parse_metadata(bytes: &[u8]) -> Result<Vec<PageMetadata>> {
    let text = std::str::from_utf8(bytes).map_err(|_| anyhow!("the metadata file is not UTF-8"))?;
    let mut lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: PageMetadata = serde_json::from_str(line)
            .with_context(|| format!("metadata line {} is not a valid page record", index + 1))?;
        lines.push(parsed);
    }
    Ok(lines)
}

/// Whether `path` is a page file path an archive may carry: below the wiki
/// prefix, `/`-separated, no `.` or `..` component, ending in `.md`.
pub fn is_page_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(WIKI_PREFIX) else {
        return false;
    };
    if rest.is_empty() || !rest.ends_with(".md") {
        return false;
    }
    rest.split('/')
        .all(|component| !component.is_empty() && component != "." && component != ".." && !component.contains('\\'))
}
