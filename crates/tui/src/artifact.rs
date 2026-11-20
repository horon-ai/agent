//! The retained artifacts of the session on screen, opened as a private copy.
//!
//! The store belongs to the harness: `crates/base/src/artifacts.rs` writes one
//! file per spilled tool result under `$HORON_HOME/artifacts/<session>/` as
//! `<id>.<tool>.log`, and the transcript's `artifact://<id>` pointer names it.
//! The client reads that layout and nothing else, and only for its own harness:
//! an explicit socket or a remote bridge can report a session id that coincides
//! with a local one, so that session is refused before the store is touched.
//! Within the store an address naming another session, a URL or a path is
//! refused before a file is looked at, and a link standing in for the store
//! directory or one of its files is refused rather than followed.
//!
//! The client never writes here. Opening an artifact copies it into a private
//! temporary file, which the editor may change; the retained file is only read.

use std::fmt;
use std::path::{Path, PathBuf};

/// The suffix every retained artifact file carries.
///
/// The same suffix the store appends in `crates/base/src/artifacts.rs`; a file
/// without it is not an artifact, whatever its name.
const SUFFIX: &str = ".log";

/// Why an artifact was not selected, or not found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No session is attached, so no store belongs to the screen.
    NoSession,
    /// The attached session does not run on this client's own harness.
    RemoteSession,
    /// The attached session id is not a directory name in the store.
    BadSession(String),
    /// The address is a URL or a path rather than `artifact://<id>`.
    NotAnArtifact(String),
    /// The number in the address is not an id.
    NotAnId(String),
    /// The session's store holds no artifact with that id.
    Missing(u64),
    /// The entry is a link, a directory or another non-file.
    NotAFile(u64),
    /// A store directory is a link, so it is not followed.
    Linked(PathBuf),
    /// A store component exists but is not a directory.
    NotADirectory(PathBuf),
    /// The store could not be listed.
    Store(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSession => formatter.write_str("no session is attached; /artifact needs one"),
            Self::RemoteSession => formatter.write_str(
                "the session is behind an explicit socket or a remote bridge; only a local harness's artifacts open",
            ),
            Self::BadSession(session) => write!(formatter, "`{session}` is not a session id the store names"),
            Self::NotAnArtifact(address) => write!(
                formatter,
                "`{address}` is not an artifact address; use /artifact <id> or /artifact artifact://<id>"
            ),
            Self::NotAnId(text) => write!(formatter, "`{text}` is not an artifact id"),
            Self::Missing(id) => write!(formatter, "artifact {id} is not in this session's store"),
            Self::NotAFile(id) => write!(formatter, "artifact {id} is a link or another kind of file; not opened"),
            Self::Linked(path) => {
                write!(
                    formatter,
                    "`{}` is a link; the artifact store is not followed",
                    path.display()
                )
            }
            Self::NotADirectory(path) => write!(formatter, "`{}` is not a directory", path.display()),
            Self::Store(error) => write!(formatter, "cannot read the artifact store: {error}"),
        }
    }
}

/// An artifact named in a typed line, as the id `artifact://` addresses.
///
/// Two spellings reach the same id: the bare number and the pointer the
/// transcript prints (`artifact://<id>`). A session id is never part of an
/// address: the store directory is the attached session's, so an id that only
/// another session's store holds is not found here, and a line that prefixes
/// one is refused rather than resolved.
pub(crate) fn parse(text: &str) -> Result<u64, Refusal> {
    let address = text.trim();
    if address.is_empty() {
        return Err(Refusal::NotAnArtifact(String::new()));
    }
    if let Some(rest) = address.strip_prefix("artifact://") {
        return number(rest);
    }
    // A scheme is a remote or an external document, not this machine's store.
    if address.contains("://") {
        return Err(Refusal::NotAnArtifact(address.to_string()));
    }
    // A path, absolute or relative, names a file rather than a stored id.
    if address.contains('/') || address.contains('\\') {
        return Err(Refusal::NotAnArtifact(address.to_string()));
    }
    number(address)
}

/// A decimal id, refusing anything else instead of reading a prefix of it.
fn number(text: &str) -> Result<u64, Refusal> {
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|character| character.is_ascii_digit()) {
        return Err(Refusal::NotAnId(text.to_string()));
    }
    text.parse::<u64>().map_err(|_| Refusal::NotAnId(text.to_string()))
}

/// The file artifact `id` names in the store rooted at `root`.
///
/// The store is `<root>/<session>/`, and an artifact is a regular file whose
/// name starts with `<id>.` and ends with `.log`. A link or a directory that
/// happens to carry that name is refused rather than followed: the copy the
/// editor gets must be bytes this store holds. A session that is not this
/// client's own harness (`local`) is refused before the store is touched.
pub(crate) fn resolve(root: &Path, session_id: &str, id: u64, local: bool) -> Result<PathBuf, Refusal> {
    own_harness(local)?;
    let directory = session_dir(root, session_id)?;
    let entries = std::fs::read_dir(&directory).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Refusal::Missing(id),
        _ => Refusal::Store(error.to_string()),
    })?;
    let prefix = format!("{id}.");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) || !name.ends_with(SUFFIX) {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            return Err(Refusal::NotAFile(id));
        }
        return Ok(path);
    }
    Err(Refusal::Missing(id))
}

/// One artifact in a session's store, as the listing draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stored {
    /// The id `artifact://` addresses.
    pub id: u64,
    /// The tool name the store kept in the file name.
    pub tool: String,
    /// Size on disk now.
    pub bytes: u64,
}

/// Every artifact in `session_id`'s store, lowest id first.
///
/// A store that does not exist yet is an empty list: no artifact has been
/// spilled for this session, which is a fact rather than a failure. Entries
/// that are not regular files are skipped, because [`resolve`] would refuse
/// them anyway. As in [`resolve`], a session that is not this client's own
/// harness reads no store.
pub(crate) fn entries(root: &Path, session_id: &str, local: bool) -> Result<Vec<Stored>, Refusal> {
    own_harness(local)?;
    let directory = session_dir(root, session_id)?;
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Refusal::Store(error.to_string())),
    };
    let mut stored = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some((id, tool)) = parse_name(&name) else {
            continue;
        };
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        stored.push(Stored {
            id,
            tool: tool.to_string(),
            bytes: metadata.len(),
        });
    }
    stored.sort_by_key(|entry| entry.id);
    Ok(stored)
}

/// The id and tool a store file name carries, or `None` for a name the store
/// did not write.
fn parse_name(name: &str) -> Option<(u64, &str)> {
    let stem = name.strip_suffix(SUFFIX)?;
    let (id, tool) = stem.split_once('.')?;
    if id.is_empty() || !id.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    // Digits can still be an id the store never wrote if they overflow; that is
    // a name to skip, not a failure to report.
    match id.parse::<u64>() {
        Ok(id) => Some((id, tool)),
        Err(_) => None,
    }
}

/// Refuse every read for a session that is not this client's own harness.
///
/// The connection says where the session runs, and an id alone cannot: an
/// explicit `--socket` or a `HORON_AGENT_API_SOCKET` override may reach an SSH
/// forward or a remote bridge whose session id coincides with a local one. The
/// check lives here, at the only two doors into the store, rather than in the
/// command that calls them.
fn own_harness(local: bool) -> Result<(), Refusal> {
    if local { Ok(()) } else { Err(Refusal::RemoteSession) }
}

/// `<root>/<session>`, with the session id held to the alphabet the store uses.
///
/// The id lands in a path, so anything else could name a parent directory or a
/// file outside the store. The rule is the store's own
/// (`crates/base/src/artifacts.rs`), repeated here because the client is not
/// allowed an engine dependency for one check.
///
/// Both directories above the file are checked too: a link at either can point
/// the read at a directory this store never wrote, and a session id that looks
/// local is not evidence that the bytes behind it are.
fn session_dir(root: &Path, session_id: &str) -> Result<PathBuf, Refusal> {
    let valid = !session_id.is_empty()
        && session_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-');
    if !valid {
        return Err(Refusal::BadSession(session_id.to_string()));
    }
    real_directory(root)?;
    let directory = root.join(session_id);
    real_directory(&directory)?;
    Ok(directory)
}

/// Refuse a store component that a link or a file stands in for.
///
/// An absent path is not a failure: nothing has been spilled for the session
/// yet. A path that exists must be a real directory, so the bytes read below it
/// are the ones this store wrote rather than wherever a link points.
fn real_directory(path: &Path) -> Result<(), Refusal> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Refusal::Linked(path.to_path_buf())),
        Ok(metadata) if !metadata.is_dir() => Err(Refusal::NotADirectory(path.to_path_buf())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Refusal::Store(error.to_string())),
    }
}
