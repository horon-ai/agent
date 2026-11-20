//! Editing the draft in the user's own editor.
//!
//! `VISUAL`, then `EDITOR`, names the program. The draft goes into a private
//! temporary file, the program runs on that file, and what it saved comes back
//! as the draft: nothing is sent, and a program that is missing or exits
//! non-zero leaves the draft exactly as it was.
//!
//! The value is split by `shlex`, which is POSIX word syntax and nothing else: a
//! quoted path stays one word, `$HOME` stays seven characters, and no value can
//! run a second command. `shlex` also reads a `#` at the start of a word as the
//! start of a comment, as a shell would; a program or path with a `#` in it is
//! written quoted.
//!
//! The file is `tempfile`'s own contract: an unpredictable name, mode 0600, and
//! removal on drop. What the editor left is read back only when it is still a
//! regular file and no larger than [`MAX_DRAFT_BYTES`]: a link, a pipe or a
//! directory put in its place is refused. The platform crate's no-follow open is
//! not reachable from this client, so the entry is inspected before it is opened
//! and the opened handle's own metadata decides afterwards; the editor has
//! finished by then, so the only writer that could race that check is another
//! process of the same user.
//!
//! The screen is not this module's business: the caller hands the terminal over
//! before [`edit`] and takes it back after.
//!
//! [`open_copy`] hands the program a private copy of a retained artifact and
//! reads nothing back: the source is only read, so an edit lands in the copy
//! that is removed with it.

use std::fmt;
use std::io::{Read, Write};
use std::path::Path;

/// The most an edited draft may grow to before it is refused.
///
/// A draft is a message for a model's context. The editor is handed a path, and
/// a save that wrote a build log or a copied directory into it would otherwise
/// become the next message.
const MAX_DRAFT_MIB: u64 = 4;

/// The same limit in bytes.
const MAX_DRAFT_BYTES: u64 = MAX_DRAFT_MIB * 1024 * 1024;

/// The most an artifact copy may read from the store before it is refused.
///
/// A retained artifact is at most 16 MiB, because the store keeps a head and a
/// tail of 8 MiB each, so this is a ceiling on the copy rather than on the
/// artifact. It is checked before the bytes are read and again on the count the
/// read saw.
const MAX_COPY_MIB: u64 = 32;

/// The copy limit in bytes.
const MAX_COPY_BYTES: u64 = MAX_COPY_MIB * 1024 * 1024;

/// The program that edits a draft, already split into words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Program {
    /// The executable to run.
    program: String,
    /// The words after it, as the value wrote them.
    arguments: Vec<String>,
}

impl Program {
    /// The program named by `VISUAL`, falling back to `EDITOR`.
    ///
    /// An empty or blank value is treated as unset: a shell that exports
    /// `EDITOR=` with nothing after it has not chosen an editor, and starting a
    /// blank program would fail after the screen was already handed over.
    pub(crate) fn resolve(visual: Option<&str>, editor: Option<&str>) -> Result<Self, Problem> {
        let value = [visual, editor]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|value| !value.is_empty())
            .ok_or(Problem::Missing)?;
        let words = shlex::split(value).ok_or(Problem::Unusable("unclosed quote".into()))?;
        let mut words = words.into_iter();
        let Some(program) = words.next() else {
            return Err(Problem::Missing);
        };
        Ok(Self {
            program,
            arguments: words.collect(),
        })
    }

    /// The program this session's environment names.
    pub(crate) fn from_environment() -> Result<Self, Problem> {
        let visual = std::env::var("VISUAL").ok();
        let editor = std::env::var("EDITOR").ok();
        Self::resolve(visual.as_deref(), editor.as_deref())
    }
}

/// Why a draft did not come back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Problem {
    /// Neither `VISUAL` nor `EDITOR` names a program.
    Missing,
    /// The value cannot be read as a command line.
    Unusable(String),
    /// The temporary file could not be prepared or written.
    TempFile(String),
    /// The program could not be started.
    Start(String),
    /// The program ran and failed; the file it may have edited is not read.
    Exit(String),
    /// The saved draft could not be read back.
    Read(String),
    /// The editor left a link, a pipe, a directory or another non-file.
    Replaced,
    /// The saved draft is past the size limit.
    TooLarge,
    /// The saved draft is not UTF-8, so the draft on screen is kept.
    NotText,
    /// The retained artifact could not be inspected or read.
    Source(String),
    /// The retained artifact is a link, a directory or another non-file.
    SourceReplaced,
    /// The retained artifact is past the copy limit.
    SourceTooLarge,
    /// The editor ran on an artifact copy and failed; the copy is discarded.
    SourceExit(String),
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("no editor: set VISUAL or EDITOR"),
            Self::Unusable(reason) => write!(formatter, "cannot read the editor command: {reason}"),
            Self::TempFile(error) => write!(formatter, "cannot prepare a private draft: {error}"),
            Self::Start(error) => write!(formatter, "cannot start the editor: {error}"),
            Self::Exit(status) => write!(formatter, "the editor {status}; the draft is unchanged"),
            Self::Read(error) => write!(formatter, "cannot read the draft back: {error}"),
            Self::Replaced => {
                formatter.write_str("the editor left a link or another kind of file; the draft is unchanged")
            }
            Self::TooLarge => write!(
                formatter,
                "the editor saved more than {MAX_DRAFT_MIB} MiB; the draft is unchanged"
            ),
            Self::NotText => formatter.write_str("the editor saved text that is not UTF-8; the draft is unchanged"),
            Self::Source(error) => write!(formatter, "cannot read the artifact: {error}"),
            Self::SourceReplaced => formatter.write_str("the artifact is a link or another kind of file; not opened"),
            Self::SourceTooLarge => write!(formatter, "the artifact is larger than {MAX_COPY_MIB} MiB; not opened"),
            Self::SourceExit(status) => write!(formatter, "the editor {status}; the artifact copy was discarded"),
        }
    }
}

/// Run `program` over `draft` and return the text it saved.
///
/// The file is removed on every path out, including the failing ones. A failed
/// exit is not read back: editing text a program half wrote is how a draft gets
/// replaced by a file that was never saved.
pub(crate) fn edit(draft: &str, program: &Program) -> Result<String, Problem> {
    let file = prepare(draft)?;
    let outcome = std::process::Command::new(&program.program)
        .args(&program.arguments)
        .arg(file.path())
        .status();
    let status = match outcome {
        Ok(status) => status,
        Err(error) => return Err(Problem::Start(format!("`{}`: {error}", program.program))),
    };
    if !status.success() {
        return Err(Problem::Exit(describe(status)));
    }
    read_saved(file.path())
}

/// Write `draft` into the temporary file the editor is given.
///
/// `tempfile` owns the name, the mode and the removal: the name carries enough
/// randomness that it cannot be guessed, the file is created with `create_new`
/// as mode 0600, and dropping the handle removes whatever is at that path.
fn prepare(draft: &str) -> Result<tempfile::NamedTempFile, Problem> {
    let mut file = tempfile::Builder::new()
        .prefix("horon-draft-")
        .suffix(".md")
        .tempfile()
        .map_err(|error| Problem::TempFile(error.to_string()))?;
    file.write_all(draft.as_bytes())
        .map_err(|error| Problem::TempFile(error.to_string()))?;
    Ok(file)
}

/// Run `program` over a private copy of the artifact at `source`.
///
/// The store's file is only read: the editor is never handed that path, and
/// what it saves is not read back, so an edit lands in the copy and nowhere the
/// transcript can see. The copy is `tempfile`'s own 0600 file and is removed
/// when the program exits, including a failing one. A source that is a link, a
/// pipe or a directory, or one past the copy limit, is refused before a byte is
/// copied.
pub(crate) fn open_copy(source: &Path, program: &Program) -> Result<(), Problem> {
    let copy = prepare_copy(source)?;
    let outcome = std::process::Command::new(&program.program)
        .args(&program.arguments)
        .arg(copy.path())
        .status();
    match outcome {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(Problem::SourceExit(describe(status))),
        Err(error) => Err(Problem::Start(format!("`{}`: {error}", program.program))),
    }
}

/// Copy the artifact at `source` into the temporary file the editor is given.
///
/// The store's file is inspected before it is opened and the opened handle's
/// metadata again after, exactly as [`read_saved`] checks a saved draft, so a
/// link or a directory in its place is refused. The size limit is checked on the
/// metadata and again on the count the copy actually moved, because the artifact
/// can grow between the two.
fn prepare_copy(source: &Path) -> Result<tempfile::NamedTempFile, Problem> {
    let entry = std::fs::symlink_metadata(source).map_err(|error| Problem::Source(error.to_string()))?;
    if !entry.file_type().is_file() {
        return Err(Problem::SourceReplaced);
    }
    let file = std::fs::File::open(source).map_err(|error| Problem::Source(error.to_string()))?;
    let metadata = file.metadata().map_err(|error| Problem::Source(error.to_string()))?;
    if !metadata.is_file() {
        return Err(Problem::SourceReplaced);
    }
    if metadata.len() > MAX_COPY_BYTES {
        return Err(Problem::SourceTooLarge);
    }
    let mut copy = tempfile::Builder::new()
        .prefix("horon-artifact-")
        .suffix(".log")
        .tempfile()
        .map_err(|error| Problem::TempFile(error.to_string()))?;
    let mut reader = file.take(MAX_COPY_BYTES + 1);
    let copied = std::io::copy(&mut reader, copy.as_file_mut()).map_err(|error| Problem::Source(error.to_string()))?;
    // The artifact can grow between the metadata and the read, so the count that
    // decides is the one the copy actually saw.
    if copied > MAX_COPY_BYTES {
        return Err(Problem::SourceTooLarge);
    }
    Ok(copy)
}

/// How a finished process failed, for the line the transcript keeps.
fn describe(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exited with status {code}"),
        None => "was ended by a signal".to_string(),
    }
}

/// Read what the editor saved, refusing anything but a regular file.
fn read_saved(path: &Path) -> Result<String, Problem> {
    // The entry is inspected before it is opened: opening a link follows it,
    // and opening a pipe waits for a writer that is not coming.
    let entry = std::fs::symlink_metadata(path).map_err(|error| Problem::Read(error.to_string()))?;
    if !entry.file_type().is_file() {
        return Err(Problem::Replaced);
    }
    let file = std::fs::File::open(path).map_err(|error| Problem::Read(error.to_string()))?;
    // The handle is what the read comes from, so its own metadata decides: a
    // swap between the check above and the open cannot change this answer.
    let metadata = file.metadata().map_err(|error| Problem::Read(error.to_string()))?;
    if !metadata.is_file() {
        return Err(Problem::Replaced);
    }
    if metadata.len() > MAX_DRAFT_BYTES {
        return Err(Problem::TooLarge);
    }
    let mut saved = Vec::new();
    let mut reader = file.take(MAX_DRAFT_BYTES + 1);
    reader
        .read_to_end(&mut saved)
        .map_err(|error| Problem::Read(error.to_string()))?;
    // The file can grow between the metadata and the read, so the count that
    // decides is the one the read actually saw.
    if saved.len() as u64 > MAX_DRAFT_BYTES {
        return Err(Problem::TooLarge);
    }
    String::from_utf8(saved).map_err(|_| Problem::NotText)
}

#[cfg(test)]
mod tests {
    use super::{Problem, Program, prepare, read_saved};

    fn program(program: &str, arguments: &[&str]) -> Program {
        Program {
            program: program.to_string(),
            arguments: arguments.iter().map(|word| word.to_string()).collect(),
        }
    }

    #[test]
    fn visual_is_preferred_and_editor_is_the_fallback() {
        let chosen = Program::resolve(Some("vi"), Some("nano")).expect("a program");
        assert_eq!(chosen, program("vi", &[]));
        let fallback = Program::resolve(None, Some("nano")).expect("a program");
        assert_eq!(fallback, program("nano", &[]));
        // An exported-but-empty value is not a choice; the next one is.
        let skipped = Program::resolve(Some("  "), Some("nano")).expect("a program");
        assert_eq!(skipped, program("nano", &[]));
    }

    #[test]
    fn no_editor_is_reported_before_anything_runs() {
        assert_eq!(Program::resolve(None, None), Err(Problem::Missing));
        assert_eq!(Program::resolve(Some(""), Some("   ")), Err(Problem::Missing));
    }

    #[test]
    fn words_keep_their_shape_without_a_shell() {
        let chosen = Program::resolve(Some("code --wait"), None).expect("a program");
        assert_eq!(chosen, program("code", &["--wait"]));
        let quoted = Program::resolve(Some(r#"edit -n "$HOME/a b.md""#), None).expect("a program");
        assert_eq!(quoted, program("edit", &["-n", "$HOME/a b.md"]));
        let broken = Program::resolve(Some("sh -c \"echo"), None);
        assert!(matches!(broken, Err(Problem::Unusable(_))));
    }

    #[test]
    fn the_draft_reaches_the_file_and_leaves_with_it() {
        let draft = "line one\nline two\n";
        let path = {
            let file = prepare(draft).expect("a draft file");
            let path = file.path().to_path_buf();
            assert_eq!(std::fs::read_to_string(&path).expect("the draft"), draft);
            path
        };
        assert!(!path.exists(), "the draft outlived its edit");
    }

    #[test]
    fn a_saved_draft_is_read_back_whole() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("saved.md");
        std::fs::write(&path, "line one\nline two\n").expect("the saved draft");
        assert_eq!(read_saved(&path).expect("the draft"), "line one\nline two\n");
    }

    #[test]
    fn a_directory_is_not_a_saved_draft() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(read_saved(directory.path()), Err(Problem::Replaced));
    }

    #[test]
    fn a_save_past_the_size_limit_is_refused() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("huge.md");
        // Text, so only the limit can refuse this one.
        let text = vec![b'x'; super::MAX_DRAFT_BYTES as usize + 1];
        std::fs::write(&path, text).expect("the oversized draft");
        assert_eq!(read_saved(&path), Err(Problem::TooLarge));
    }

    #[test]
    fn bytes_that_are_not_text_keep_the_draft() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("saved.md");
        std::fs::write(&path, [0xFFu8]).expect("the saved bytes");
        assert_eq!(read_saved(&path), Err(Problem::NotText));
    }

    #[test]
    fn an_artifact_is_copied_whole_and_the_source_is_left_alone() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let source = directory.path().join("12.bash.log");
        std::fs::write(&source, "original output").expect("the artifact");
        let copy = super::prepare_copy(&source).expect("the copy");
        assert_ne!(
            copy.path(),
            source.as_path(),
            "the editor would have been handed the retained file"
        );
        assert_eq!(
            std::fs::read_to_string(copy.path()).expect("the copy is readable"),
            "original output",
            "the copy does not hold the artifact"
        );
        let copy_path = copy.path().to_path_buf();
        drop(copy);
        assert!(!copy_path.exists(), "the artifact copy outlived the open");
        assert_eq!(
            std::fs::read_to_string(&source).expect("the artifact is still there"),
            "original output",
            "the retained artifact was written to"
        );
    }

    #[test]
    fn a_directory_in_place_of_an_artifact_is_not_copied() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        // A link is refused by the same regular-file check; creating one is the
        // platform's business and is covered by the PTY receipt.
        assert_eq!(super::prepare_copy(directory.path()), Err(Problem::SourceReplaced));
    }

    #[test]
    fn an_artifact_past_the_copy_limit_is_refused_before_it_is_read() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let source = directory.path().join("12.bash.log");
        let file = std::fs::File::create(&source).expect("an artifact");
        // Sparse, so the limit is tested without writing 32 MiB.
        file.set_len(super::MAX_COPY_BYTES + 1).expect("a large artifact");
        drop(file);
        assert_eq!(super::prepare_copy(&source), Err(Problem::SourceTooLarge));
    }

    #[test]
    fn an_editor_that_cannot_start_is_reported_and_the_source_is_kept() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let source = directory.path().join("12.bash.log");
        std::fs::write(&source, "original output").expect("the artifact");
        // A program no platform has: the failure is the start, not the exit, so
        // nothing has to exist to run it.
        let editor = program("horon-tui-test-no-such-editor", &[]);
        match super::open_copy(&source, &editor) {
            Err(Problem::Start(message)) => {
                assert!(message.contains("horon-tui-test-no-such-editor"), "message: {message}");
            }
            other => panic!("expected an editor that could not be started, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&source).expect("the artifact is still there"),
            "original output",
            "a failed editor reached the retained artifact"
        );
    }
}
