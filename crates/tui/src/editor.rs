//! Editing the draft in the user's own editor.
//!
//! `VISUAL`, then `EDITOR`, names the program. The draft goes into a 0600 file
//! inside a private temporary directory, the program runs on that file, and what
//! it saved comes back as the draft: nothing is sent, and a program that is
//! missing or exits non-zero leaves the draft exactly as it was.
//!
//! The value is split by `shlex`, which is POSIX word syntax and nothing else: a
//! quoted path stays one word, `$HOME` stays seven characters, and no value can
//! run a second command. `shlex` also reads a `#` at the start of a word as the
//! start of a comment, as a shell would; a program or path with a `#` in it is
//! written quoted.
//!
//! What the editor left is read back only when it is the regular file this user
//! wrote: a link, a pipe or a directory put in its place is refused, and so is a
//! file past the size limit. The screen is not this module's business: the
//! caller hands the terminal over before [`edit`] and takes it back after.

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
    /// The private directory or the draft file in it could not be prepared.
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
        }
    }
}

/// Run `program` over `draft` and return the text it saved.
///
/// The file is private to this user, holds the draft before the program starts,
/// and is removed with its directory on every path out, including the failing
/// ones. A failed exit is not read back: editing text a program half wrote is
/// how a draft gets replaced by a file that was never saved.
pub(crate) fn edit(draft: &str, program: &Program) -> Result<String, Problem> {
    let file = Draft::create(draft)?;
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
    let file = open_saved(path)?;
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

/// Open the saved draft without following a link.
///
/// `O_NOFOLLOW` is the Unix half: the open fails when the path is a link, so a
/// link that appeared after the check in [`read_saved`] is still refused.
/// Windows has no equivalent flag, where that check before the open is the whole
/// guard.
fn open_saved(path: &Path) -> Result<std::fs::File, Problem> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(file) => Ok(file),
        Err(error) => Err(open_problem(error)),
    }
}

/// Why the open in [`open_saved`] failed.
fn open_problem(error: std::io::Error) -> Problem {
    #[cfg(unix)]
    {
        if error.raw_os_error() == Some(libc::ELOOP) {
            return Problem::Replaced;
        }
    }
    Problem::Read(error.to_string())
}

/// The file the editor is given, and the private directory it lives in.
///
/// `tempfile` creates the directory readable only by this user and the file with
/// mode 0600, so the name cannot be guessed into an existing file and no other
/// account can open the draft. Both are removed when the edit ends, file first;
/// the removal is by path, so a file an editor renamed into place is cleaned up
/// with the one it replaced.
struct Draft {
    file: tempfile::NamedTempFile,
    _directory: tempfile::TempDir,
}

impl Draft {
    /// Write `text` into a new private file.
    fn create(text: &str) -> Result<Self, Problem> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("horon-draft-");
        // The file inside is 0600 either way; a directory only this user may
        // enter also keeps its name, and the draft, out of sight.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let directory = builder
            .tempdir()
            .map_err(|error| Problem::TempFile(error.to_string()))?;
        let mut file = tempfile::Builder::new()
            .prefix("draft-")
            .suffix(".md")
            .tempfile_in(directory.path())
            .map_err(|error| Problem::TempFile(error.to_string()))?;
        file.write_all(text.as_bytes())
            .map_err(|error| Problem::TempFile(error.to_string()))?;
        Ok(Self {
            file,
            _directory: directory,
        })
    }

    /// The file the editor is given.
    fn path(&self) -> &Path {
        self.file.path()
    }
}

#[cfg(test)]
mod tests {
    use super::{Problem, Program};

    fn program(program: &str, arguments: &[&str]) -> Program {
        Program {
            program: program.to_string(),
            arguments: arguments.iter().map(|word| word.to_string()).collect(),
        }
    }

    /// An executable script the editor tests run as the editor.
    #[cfg(unix)]
    fn script(directory: &std::path::Path, body: &str) -> Program {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("editor.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("a script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("exec bit");
        program(&path.display().to_string(), &[])
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

    #[cfg(unix)]
    #[test]
    fn a_draft_file_is_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt;
        let draft = super::Draft::create("secret").expect("a draft");
        let metadata = std::fs::metadata(draft.path()).expect("the file");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_draft_file_lives_only_as_long_as_the_edit() {
        let path = {
            let draft = super::Draft::create("kept").expect("a draft");
            let path = draft.path().to_path_buf();
            assert!(path.exists());
            path
        };
        assert!(!path.exists(), "the draft outlived its edit");
    }

    #[cfg(unix)]
    #[test]
    fn a_test_editor_round_trips_a_multiline_draft() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        // Reads what the client wrote and adds a line: the saved text proves the
        // editor was handed the draft, not only the file name.
        let editor = script(directory.path(), "printf 'line three\\n' >> \"$1\"");
        let saved = super::edit("line one\nline two\n", &editor).expect("the saved draft");
        assert_eq!(saved, "line one\nline two\nline three\n");
    }

    #[cfg(unix)]
    #[test]
    fn bytes_that_are_not_text_keep_the_draft() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let editor = script(directory.path(), "printf '\\377' > \"$1\"");
        assert_eq!(super::edit("typed", &editor), Err(Problem::NotText));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_the_editor_left_is_not_read() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let target = directory.path().join("target.md");
        std::fs::write(&target, "not the draft").expect("a target file");
        let editor = script(
            directory.path(),
            &format!("rm -f \"$1\"; ln -s '{}' \"$1\"", target.display()),
        );
        assert_eq!(super::edit("typed", &editor), Err(Problem::Replaced));
    }

    #[cfg(unix)]
    #[test]
    fn a_pipe_the_editor_left_is_not_opened() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let editor = script(directory.path(), "rm -f \"$1\"; mkfifo \"$1\"");
        assert_eq!(super::edit("typed", &editor), Err(Problem::Replaced));
    }

    #[cfg(unix)]
    #[test]
    fn a_save_past_the_size_limit_is_refused() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        // Zero bytes are valid UTF-8, so only the limit can refuse this one.
        let editor = script(
            directory.path(),
            "dd if=/dev/zero of=\"$1\" bs=1048576 count=5 2>/dev/null",
        );
        assert_eq!(super::edit("typed", &editor), Err(Problem::TooLarge));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_editor_yields_no_text_and_leaves_nothing_behind() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let recorded = directory.path().join("seen");
        let editor = script(
            directory.path(),
            &format!("printf '%s' \"$1\" > '{}'; exit 3", recorded.display()),
        );
        let outcome = super::edit("typed", &editor);
        match outcome {
            Err(Problem::Exit(status)) => assert!(status.contains('3'), "status: {status}"),
            other => panic!("expected a failed exit, got {other:?}"),
        }
        let seen = std::fs::read_to_string(&recorded).expect("the path the editor saw");
        assert!(
            !std::path::Path::new(&seen).exists(),
            "the draft file survived the failure"
        );
    }
}
