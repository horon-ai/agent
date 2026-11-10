//! Archive reads: `read bundle.tar.gz` lists members, `read bundle.zip:dir/a.txt`
//! returns one member through the normal text path.
//!
//! Nothing is extracted to disk. A tar (optionally gzipped) is streamed with
//! `tar::Archive`, and a zip is read through `zip::ZipArchive`, which seeks in
//! the file rather than copying it. Member text then goes through the same
//! selector and line caps as a file read ([`super::page_text`]).
//!
//! A member path that escapes the archive (`..` or absolute) is refused before
//! the member is looked up, so a crafted archive cannot name a file outside it.
//! The archive itself goes through the path-deny policy that already judged the
//! raw `read` argument.

use super::{ReadInput, page_text};
use crate::tool::read_selector::{Selector, split_member_and_selector};
use crate::tool::{ToolContext, ToolOutput};
use anyhow::{Result, anyhow, bail};
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};

/// Largest member body read into memory. A selector only renders part of it,
/// but the member still has to be read once, so this bounds one call.
const MAX_MEMBER_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArchiveKind {
    Tar,
    TarGz,
    Zip,
}

impl ArchiveKind {
    /// The archive kind of `path`, from its extension and its magic bytes.
    ///
    /// Both are needed: the extension says a selector may follow the path, and
    /// the magic bytes keep a text file called `.zip` on the plain-read path.
    pub(super) fn detect(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        let declared = if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Self::TarGz
        } else if name.ends_with(".tar") {
            Self::Tar
        } else if name.ends_with(".zip") {
            Self::Zip
        } else {
            return None;
        };
        let magic = read_magic(path)?;
        match declared {
            Self::Zip if is_zip(&magic) => Some(Self::Zip),
            Self::Tar if is_tar(&magic) => Some(Self::Tar),
            Self::TarGz if is_gzip(&magic) => Some(Self::TarGz),
            _ => None,
        }
    }
}

fn read_magic(path: &Path) -> Option<Vec<u8>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return None,
    };
    let mut magic = vec![0u8; 512];
    let mut filled = 0;
    while filled < magic.len() {
        match file.read(&mut magic[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    magic.truncate(filled);
    Some(magic)
}

fn is_zip(magic: &[u8]) -> bool {
    magic.starts_with(b"PK\x03\x04") || magic.starts_with(b"PK\x05\x06") || magic.starts_with(b"PK\x07\x08")
}

fn is_gzip(magic: &[u8]) -> bool {
    magic.starts_with(&[0x1f, 0x8b])
}

fn is_tar(magic: &[u8]) -> bool {
    magic.get(257..262).is_some_and(|tag| tag == b"ustar")
}

/// One listed member.
struct Member {
    name: String,
    size: u64,
    is_dir: bool,
}

/// Resolves `raw` to an archive and an optional member, or `None` when the
/// request is not an archive read.
///
/// The whole string wins when it is an existing archive, so `bundle.zip` lists
/// members. Otherwise each colon is tried from the right, and the first prefix
/// that is an existing archive names the archive while the remainder is the
/// member (plus an optional line selector).
/// A resolved archive request: the archive path, its kind, and the optional
/// member with a line selector inside it.
pub(super) type ArchiveRequest = (PathBuf, ArchiveKind, Option<(String, Selector)>);

pub(super) fn resolve(raw: &str, cwd: &Path) -> Result<Option<ArchiveRequest>> {
    let whole = absolute(raw, cwd);
    if let Some(kind) = ArchiveKind::detect(&whole) {
        return Ok(Some((whole, kind, None)));
    }
    for (index, _) in raw.rmatch_indices(':') {
        if raw[index + 1..].contains("://") {
            continue;
        }
        let (prefix, rest) = (&raw[..index], &raw[index + 1..]);
        if prefix.is_empty() || rest.is_empty() {
            continue;
        }
        let candidate = absolute(prefix, cwd);
        let Some(kind) = ArchiveKind::detect(&candidate) else {
            continue;
        };
        let (member, selector) = split_member_and_selector(rest)?;
        if member.is_empty() {
            return Ok(Some((candidate, kind, None)));
        }
        return Ok(Some((candidate, kind, Some((member, selector)))));
    }
    Ok(None)
}

fn absolute(raw: &str, cwd: &Path) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd.join(candidate)
    }
}

/// Reads one archive request end to end.
pub(super) fn read_archive(params: &ReadInput, ctx: &ToolContext, cwd: &Path) -> Result<ToolOutput> {
    let (path, kind, member) =
        resolve(&params.file_path, cwd)?.ok_or_else(|| anyhow!("not an archive read: {}", params.file_path))?;
    // The policy judged the raw argument, which may not equal the archive path
    // once a member selector is stripped off it.
    crate::tool::path_policy::enforce("read", &serde_json::json!({ "file_path": path.to_string_lossy() }), ctx)?;

    let Some((member, selector)) = member else {
        let members = list_members(&path, kind)?;
        return Ok(ToolOutput::new(render_listing(&params.file_path, &members)).with_title(params.file_path.clone()));
    };

    refuse_escaping_member(&member)?;
    let bytes = read_member(&path, kind, &member)?;
    let text = String::from_utf8_lossy(&bytes);
    let page = page_text(params, &selector, &text)?;
    Ok(ToolOutput::new(page).with_title(format!("{}:{member}", params.file_path)))
}

fn render_listing(requested: &str, members: &[Member]) -> String {
    let mut out = format!("Archive: {requested}\n{} members:\n", members.len());
    for member in members {
        if member.is_dir {
            out.push_str(&format!("- {}/\n", member.name));
        } else {
            out.push_str(&format!("- {} ({} bytes)\n", member.name, member.size));
        }
    }
    if members.is_empty() {
        out.push_str("(empty archive)\n");
    }
    out
}

fn list_members(path: &Path, kind: ArchiveKind) -> Result<Vec<Member>> {
    let mut members = Vec::new();
    match kind {
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            let file = std::fs::File::open(path)
                .map_err(|error| anyhow!("cannot open archive {}: {error}", path.display()))?;
            let reader = BufReader::new(file);
            let mut archive: tar::Archive<Box<dyn Read>> = match kind {
                ArchiveKind::TarGz => tar::Archive::new(Box::new(flate2::read::GzDecoder::new(reader))),
                _ => tar::Archive::new(Box::new(reader)),
            };
            for entry in archive.entries()? {
                let entry = entry?;
                let name = entry.path()?.to_string_lossy().to_string();
                members.push(Member {
                    name,
                    size: entry.size(),
                    is_dir: entry.header().entry_type().is_dir(),
                });
            }
        }
        ArchiveKind::Zip => {
            let file = std::fs::File::open(path)
                .map_err(|error| anyhow!("cannot open archive {}: {error}", path.display()))?;
            let mut archive = zip::ZipArchive::new(BufReader::new(file))
                .map_err(|error| anyhow!("cannot read zip {}: {error}", path.display()))?;
            for index in 0..archive.len() {
                let entry = archive.by_index_raw(index)?;
                let name = entry
                    .name()
                    .map_err(|error| anyhow!("cannot read a zip member name in {}: {error}", path.display()))?
                    .to_string();
                members.push(Member {
                    name,
                    size: entry.size(),
                    is_dir: entry.is_dir(),
                });
            }
        }
    }
    Ok(members)
}

fn read_member(path: &Path, kind: ArchiveKind, member: &str) -> Result<Vec<u8>> {
    match kind {
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            let file = std::fs::File::open(path)
                .map_err(|error| anyhow!("cannot open archive {}: {error}", path.display()))?;
            let reader = BufReader::new(file);
            let mut archive: tar::Archive<Box<dyn Read>> = match kind {
                ArchiveKind::TarGz => tar::Archive::new(Box::new(flate2::read::GzDecoder::new(reader))),
                _ => tar::Archive::new(Box::new(reader)),
            };
            for entry in archive.entries()? {
                let entry = entry?;
                let name = entry.path()?.to_string_lossy().to_string();
                let normalized = name.trim_start_matches("./");
                if normalized != member {
                    continue;
                }
                if !entry.header().entry_type().is_file() {
                    bail!("{member} is not a regular file in {}", path.display());
                }
                return read_capped(&mut entry, path, member);
            }
            Err(anyhow!("no member named {member} in {}", path.display()))
        }
        ArchiveKind::Zip => {
            let file = std::fs::File::open(path)
                .map_err(|error| anyhow!("cannot open archive {}: {error}", path.display()))?;
            let mut archive = zip::ZipArchive::new(BufReader::new(file))
                .map_err(|error| anyhow!("cannot read zip {}: {error}", path.display()))?;
            let entry = archive
                .by_name(member)
                .map_err(|_| anyhow!("no member named {member} in {}", path.display()))?;
            if entry.is_dir() {
                bail!("{member} is a directory in {}", path.display());
            }
            read_capped(entry, path, member)
        }
    }
}

fn read_capped(reader: impl Read, path: &Path, member: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_MEMBER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| anyhow!("cannot read {member} from {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_MEMBER_BYTES {
        bail!(
            "{member} in {} is larger than the {} byte member budget; read it with a line selector or an extraction tool",
            path.display(),
            MAX_MEMBER_BYTES
        );
    }
    Ok(bytes)
}

/// Refuses a member name that is absolute or climbs out of the archive.
fn refuse_escaping_member(member: &str) -> Result<()> {
    let path = Path::new(member);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        bail!("archive member path is refused because it escapes the archive: {member}");
    }
    Ok(())
}

/// Reads a member body from bytes already in memory. Test-only helper kept
/// beside the production readers so a fixture needs no temporary file.
#[cfg(test)]
fn member_from_bytes(bytes: &[u8], kind: ArchiveKind, member: &str) -> Result<Vec<u8>> {
    use std::io::Cursor;
    match kind {
        ArchiveKind::Zip => {
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes.to_vec()))?;
            let entry = archive
                .by_name(member)
                .map_err(|_| anyhow!("no member named {member}"))?;
            let mut body = Vec::new();
            entry.take(MAX_MEMBER_BYTES + 1).read_to_end(&mut body)?;
            Ok(body)
        }
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            let reader: Box<dyn Read> = match kind {
                ArchiveKind::TarGz => Box::new(flate2::read::GzDecoder::new(Cursor::new(bytes.to_vec()))),
                _ => Box::new(Cursor::new(bytes.to_vec())),
            };
            let mut archive = tar::Archive::new(reader);
            for entry in archive.entries()? {
                let mut entry = entry?;
                if entry.path()?.to_string_lossy().trim_start_matches("./") == member {
                    let mut body = Vec::new();
                    entry.take(MAX_MEMBER_BYTES + 1).read_to_end(&mut body)?;
                    return Ok(body);
                }
            }
            Err(anyhow!("no member named {member}"))
        }
    }
}

#[cfg(test)]
#[path = "read_archive_tests.rs"]
mod tests;
