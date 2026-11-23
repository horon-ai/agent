//! `/diff` and `/commit`: the git state of the workspace the session works in.
//!
//! `/diff` prints the staged and the unstaged changes as two sections. `/commit`
//! reads the staged set once, previews it by exact path, and commits it only
//! after a second, explicit line. The approval covers one index tree: the
//! preview records the tree git wrote from the index it showed, and the commit
//! is made from that tree with `git commit-tree` and moved into place with a
//! compare-and-swap `git update-ref` against the reviewed branch. A file staged
//! between the preview and the confirmation therefore cannot enter the commit,
//! and a change to the index, `HEAD` or the branch in between refuses it with
//! the command that starts a fresh review. Nothing is ever pushed.
//!
//! The commit is written by plumbing, so repository hooks are **not run**:
//! `pre-commit`, `prepare-commit-msg`, `commit-msg` and `post-commit` would
//! otherwise change or vet what was approved, and `reference-transaction` is
//! the one git would run for the `update-ref` that moves the branch. A
//! repository that configures any of them is refused instead, and the user is
//! told to run an ordinary `git commit` in a terminal after the same review.
//! `user.name`, `user.email` and `commit.gpgsign` are still read from the
//! machine's configuration, so the commit is attributed and signed the way this
//! machine commits.
//!
//! Only this machine's copy of the workspace is read. A session behind an
//! explicit socket or a remote bridge reports a directory that belongs to the
//! other machine, so every entry point refuses it before git is started, on the
//! same evidence `/artifact` uses for the artifact store.
//!
//! git is started with structured arguments: no command line is assembled from a
//! string, no path read out of the index is handed back to git as an argument,
//! and the commit message reaches `git commit-tree` on its standard input.
//! Output is collected in files rather than pipes and read back bounded, so a
//! large diff can neither fill the transcript nor block the writer, and a
//! command that does not finish is stopped instead of holding the interface.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use horon_agent_sdk::process;

mod text;

/// Most bytes of one file listing kept for a section or a review.
///
/// `--name-status -z` is a few dozen bytes per file, so this covers thousands of
/// paths; a listing past it is refused rather than described from a prefix.
const LIST_LIMIT: u64 = 512 * 1024;

/// Most bytes of a patch kept before it is cut to [`PATCH_LINES`].
const PATCH_LIMIT: u64 = 256 * 1024;

/// Most lines of one patch that reach the transcript.
const PATCH_LINES: usize = 240;

/// Most characters one patch line keeps.
const PATCH_LINE_CHARS: usize = 400;

/// Most files one review describes exactly.
///
/// A preview whose paths were cut would not be the set the user approved, and a
/// confirmation for it would be a confirmation of something unseen, so a staged
/// set past this bound is refused and the user commits it from a shell.
const REVIEW_FILES: usize = 256;

/// Most untracked paths named in one `/diff`.
const UNTRACKED_ROWS: usize = 8;

/// Most bytes read from `git ls-files --others`.
const UNTRACKED_LIMIT: u64 = 64 * 1024;

/// Longest commit message this client accepts, in characters.
const MESSAGE_CHARS: usize = 2000;

/// How long one read-only git command may run.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `git commit` may run: its hooks are the repository's own and may
/// take longer than a read.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Why git was not read, or a review refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The attached session does not run on this machine.
    RemoteSession,
    /// The daemon never named the directory the session works in.
    UnknownWorkspace,
    /// The directory is not inside a work tree.
    NotARepository(String),
    /// git itself failed, or could not be started.
    Git(String),
    /// The index holds nothing to commit.
    NothingStaged,
    /// More files are staged than one review can describe exactly.
    TooManyFiles(usize),
    /// A staged file listing was too large to read completely.
    TooLarge,
    /// A reviewed set is still open, or there is none to confirm.
    ReviewOpen,
    NoReview,
    /// The message is empty or too long.
    Message(String),
    /// `HEAD` is detached, so there is no branch a commit could move.
    Detached,
    /// `HEAD` is a symbolic reference that is not a local branch.
    NotABranch(String),
    /// The repository configures commit hooks this client does not run.
    Hooked(Vec<String>),
    /// What the review described is no longer what the workspace holds.
    Stale {
        reviewed: String,
        moved: &'static str,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RemoteSession => formatter.write_str(
                "the session is behind an explicit socket or a remote bridge; its workspace is not this machine's, so git is not read",
            ),
            Self::UnknownWorkspace => formatter.write_str(
                "the session did not report the directory it works in; there is no workspace here to read",
            ),
            Self::NotARepository(where_) => write!(formatter, "{where_} is not inside a git work tree"),
            Self::Git(detail) => write!(formatter, "git: {detail}"),
            Self::NothingStaged => {
                formatter.write_str("nothing is staged, so there is nothing to review or commit")
            }
            Self::TooManyFiles(count) => write!(
                formatter,
                "{count} files are staged; one review describes at most {REVIEW_FILES} exactly, so commit from a shell"
            ),
            Self::TooLarge => formatter.write_str(
                "the staged file list is too large to read completely, so the set cannot be previewed exactly",
            ),
            Self::ReviewOpen => formatter.write_str(
                "a reviewed staged set is already open; `/commit --yes` commits it and `/commit --cancel` discards it",
            ),
            Self::NoReview => {
                formatter.write_str("no staged set was reviewed; `/commit <message>` previews one first")
            }
            Self::Message(why) => write!(formatter, "{why}"),
            Self::Detached => formatter.write_str(
                "HEAD is detached, so there is no branch to commit to; check out a branch and review again",
            ),
            Self::NotABranch(reference) => write!(
                formatter,
                "HEAD names `{reference}`, which is not a local branch; check out a branch and review again"
            ),
            Self::Hooked(hooks) => write!(
                formatter,
                "this repository configures the hook(s) {}; this client does not run repository hooks, so nothing was committed. Run `/diff staged`, then `git commit` in a terminal to let the hooks run",
                hooks.join(", ")
            ),
            Self::Stale { reviewed, moved } => write!(
                formatter,
                "{moved} changed after review {reviewed}, so nothing was committed; \
                 `/diff staged` reads the current set and `/commit <message>` reviews it again"
            ),
        }
    }
}

/// Which sections `/diff` reads and prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The staged and the unstaged changes.
    Both,
    /// Only the index, which is what `/commit` would include.
    Staged,
    /// Only the working tree, which is what `/commit` leaves alone.
    Unstaged,
}

impl Scope {
    fn staged(self) -> bool {
        matches!(self, Self::Both | Self::Staged)
    }

    fn unstaged(self) -> bool {
        matches!(self, Self::Both | Self::Unstaged)
    }
}

/// One changed path and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Change {
    /// git's status letter: `A`, `M`, `D`, `T` and so on.
    pub(crate) status: char,
    /// The path, as git reported it.
    pub(crate) path: String,
}

impl Change {
    /// The row the transcript draws.
    fn line(&self) -> String {
        format!("{}  {}", self.status, self.path)
    }
}

/// One side of `/diff`: what changed there, and the diff itself, bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
    /// The changed paths, in the order git listed them.
    pub(crate) files: Vec<Change>,
    /// `git diff --shortstat`'s line, or a count when git printed none.
    pub(crate) summary: String,
    /// The patch, already cut to [`PATCH_LINES`] and stripped of escapes.
    pub(crate) patch: String,
    /// Whether the patch was cut.
    pub(crate) truncated: bool,
}

/// What `/diff` found, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diffs {
    /// The work tree's top level.
    pub(crate) root: PathBuf,
    /// The branch, or `None` for a detached `HEAD`.
    pub(crate) branch: Option<String>,
    /// The staged changes, when the scope asked for them.
    pub(crate) staged: Option<Section>,
    /// The working-tree changes, when the scope asked for them.
    pub(crate) unstaged: Option<Section>,
    /// Untracked paths, bounded, which neither diff above shows.
    pub(crate) untracked: Vec<String>,
    /// Whether more untracked paths exist than were read.
    pub(crate) untracked_more: bool,
}

/// The staged set one `/commit` preview showed.
///
/// It is what `/commit --yes` verifies against and what the commit is built
/// from: the tree git wrote from the index the preview showed, the local branch
/// `HEAD` named, the commit it pointed at, and the message the preview printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Review {
    /// The work tree the review describes.
    root: PathBuf,
    /// The branch `HEAD` named, without the `refs/heads/` prefix.
    branch: String,
    /// The full local reference the commit is allowed to move.
    reference: String,
    /// The commit `HEAD` named, or `None` in a repository without one.
    head: Option<String>,
    /// The staged paths exactly as the preview listed them.
    files: Vec<Change>,
    /// `git diff --cached --raw -z`'s bytes: the staged content, fingerprintable.
    index: Vec<u8>,
    /// The tree `git write-tree` wrote from that index: what the approval covers.
    tree: String,
    /// A short, stable id for this set, drawn in the preview and in refusals.
    id: String,
    /// The message the preview printed.
    message: String,
}

/// What one commit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Committed {
    /// The commit `HEAD` now names.
    pub(crate) id: String,
    /// The commit's subject, as git stored it.
    pub(crate) subject: String,
    /// The branch that moved, or `None` when `HEAD` was detached.
    pub(crate) branch: Option<String>,
    /// How many paths the commit changed.
    pub(crate) files: usize,
    /// What did not go as reviewed, in the words a warning shows.
    pub(crate) notes: Vec<String>,
}

/// One git command's collected result.
struct Captured {
    /// Whether git exited successfully.
    ok: bool,
    /// The exit code, when git reported one.
    status: Option<i32>,
    /// Standard output, read up to the caller's limit.
    out: Vec<u8>,
    /// Whether standard output was cut at the limit.
    truncated: bool,
    /// Standard error, trimmed to one short sentence.
    error: String,
}

impl Captured {
    /// Standard output as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.out).into_owned()
    }

    /// The sentence a failure or a warning shows.
    fn detail(&self) -> String {
        if !self.error.is_empty() {
            return self.error.clone();
        }
        match self.status {
            Some(code) => format!("exited with status {code}"),
            None => "was ended by a signal".to_string(),
        }
    }
}

/// The git command line shared by every read here.
///
/// `--no-pager` keeps a configured pager from waiting for a terminal that is
/// not there. The four environment names are removed because a `GIT_DIR`,
/// `GIT_WORK_TREE` or `GIT_INDEX_FILE` in the client's environment would point
/// every command at a repository the attached workspace does not name.
fn command_line(directory: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .arg("--no-pager")
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_COMMON_DIR")
        .stdin(Stdio::null());
    command
}

/// Run one git command in `directory` and collect what it printed.
///
/// Standard output and standard error are written to private temporary files
/// rather than pipes: a reader that stops at its limit cannot leave git blocked
/// on a full pipe, and the two streams keep their own order. A command still
/// running at `timeout` has its process group stopped, so a hook that waits for
/// input cannot hold the interface.
fn git_in(
    directory: &Path,
    args: &[&str],
    env: &[(&str, &OsStr)],
    stdin: Option<&[u8]>,
    limit: u64,
    timeout: Duration,
) -> Result<Captured, Refusal> {
    let mut output = Scratch::new("horon-tui-git-out-")?;
    let mut error = Scratch::new("horon-tui-git-err-")?;
    let mut command = command_line(directory);
    command
        .args(args)
        .stdout(Stdio::from(output.handle()?))
        .stderr(Stdio::from(error.handle()?));
    for &(name, value) in env {
        command.env(name, value);
    }
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    process::isolate_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|problem| Refusal::Git(format!("cannot be started: {problem}")))?;
    let pid = child.id();
    let mut group = Group::new(pid);
    if let Some(payload) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        // git may have stopped before reading (a hook that refused the commit,
        // an empty index); its own standard error says why, so a broken pipe
        // here is not the error to report.
        if let Err(problem) = pipe.write_all(payload)
            && problem.kind() != std::io::ErrorKind::BrokenPipe
        {
            drop(process::force_stop_process_group(pid));
            group.disarm();
            drop(child.wait());
            return Err(Refusal::Git(format!("cannot hand git the commit message: {problem}")));
        }
    }
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut poll = Duration::from_micros(250);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(poll.min(remaining));
                poll = (poll * 2).min(Duration::from_millis(10));
            }
            Err(problem) => {
                process::kill_process_group(pid);
                group.disarm();
                drop(child.wait());
                return Err(Refusal::Git(format!("cannot wait for git: {problem}")));
            }
        }
    };
    let ok = match status {
        Some(status) => {
            // git has ended; a child it left behind is still in the group.
            process::kill_process_group(pid);
            group.disarm();
            status.success()
        }
        None => {
            process::terminate_process_group(pid, Duration::from_secs(1));
            drop(child.wait());
            group.disarm();
            false
        }
    };
    let (out, truncated) = output.read(limit)?;
    let (errors, _) = error.read(ERROR_BYTES)?;
    let error = if timed_out {
        format!("did not finish within {}s and was stopped", timeout.as_secs())
    } else {
        first_line(&String::from_utf8_lossy(&errors))
    };
    Ok(Captured {
        ok,
        status: status.and_then(|status| status.code()),
        out,
        truncated,
        error,
    })
}

/// Most bytes of standard error kept for the one line a refusal shows.
const ERROR_BYTES: u64 = 4 * 1024;

/// The first non-empty line of a message, bounded.
fn first_line(text: &str) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim();
    line.chars().take(240).collect()
}

/// One private temporary file that is removed when it is dropped.
struct Scratch {
    file: tempfile::NamedTempFile,
}

impl Scratch {
    fn new(prefix: &str) -> Result<Self, Refusal> {
        let file = tempfile::Builder::new()
            .prefix(prefix)
            .tempfile()
            .map_err(|problem| Refusal::Git(format!("cannot create a temporary file: {problem}")))?;
        Ok(Self { file })
    }

    /// A fresh handle for the child to write through.
    fn handle(&self) -> Result<std::fs::File, Refusal> {
        self.file
            .reopen()
            .map_err(|problem| Refusal::Git(format!("cannot open a temporary file: {problem}")))
    }

    /// Read what was written, up to `limit` bytes.
    fn read(&mut self, limit: u64) -> Result<(Vec<u8>, bool), Refusal> {
        let mut handle = self.handle()?;
        handle
            .seek(SeekFrom::Start(0))
            .map_err(|problem| Refusal::Git(format!("cannot read git's output: {problem}")))?;
        let mut bytes = Vec::new();
        (&mut handle)
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|problem| Refusal::Git(format!("cannot read git's output: {problem}")))?;
        let truncated = bytes.len() as u64 > limit;
        if truncated {
            bytes.truncate(limit as usize);
        }
        Ok((bytes, truncated))
    }
}

/// Keeps git's process group reachable so an abandoned child is stopped.
///
/// Rust does not kill a child when its `Child` is dropped, so a panic or an
/// early return between the spawn and the normal stop would otherwise leave the
/// command running.
struct Group {
    pid: u32,
    armed: bool,
}

impl Group {
    fn new(pid: u32) -> Self {
        Self { pid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.armed {
            // Best effort: the command may already have ended, and the caller
            // is on its way out with nothing left to report to.
            drop(process::force_stop_process_group(self.pid));
        }
    }
}

/// The work tree the attached directory belongs to.
struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Find the work tree `directory` is in.
    fn open(directory: &Path) -> Result<Self, Refusal> {
        if !directory.is_dir() {
            return Err(Refusal::NotARepository(directory.display().to_string()));
        }
        let captured = git_in(
            directory,
            &["rev-parse", "--show-toplevel"],
            &[],
            None,
            LIST_LIMIT,
            READ_TIMEOUT,
        )?;
        if !captured.ok {
            // A directory outside any repository is the ordinary answer; a git
            // that refused for another reason (a safe-directory rule, a broken
            // object store) is reported in its own words rather than as "not a
            // repository", which would send the user looking for the wrong fix.
            let detail = captured.detail();
            if detail.contains("not a git repository") {
                return Err(Refusal::NotARepository(directory.display().to_string()));
            }
            return Err(Refusal::Git(detail));
        }
        let root = captured.text().trim().to_string();
        if root.is_empty() {
            return Err(Refusal::NotARepository(directory.display().to_string()));
        }
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    /// A read that must not refresh the index: git takes no optional lock and
    /// therefore writes nothing on a machine where the index is read-only.
    fn read(&self, args: &[&str], limit: u64) -> Result<Captured, Refusal> {
        git_in(
            &self.root,
            args,
            &[("GIT_OPTIONAL_LOCKS", OsStr::new("0"))],
            None,
            limit,
            READ_TIMEOUT,
        )
    }

    /// The branch `HEAD` names, or `None` when it is detached.
    fn branch(&self) -> Result<Option<String>, Refusal> {
        let captured = self.read(&["symbolic-ref", "--short", "-q", "HEAD"], LIST_LIMIT)?;
        if captured.ok {
            let name = captured.text().trim().to_string();
            return Ok((!name.is_empty()).then_some(name));
        }
        if captured.status == Some(1) {
            return Ok(None);
        }
        Err(Refusal::Git(captured.detail()))
    }

    /// The commit `HEAD` names, or `None` in a repository without a commit.
    fn head(&self) -> Result<Option<String>, Refusal> {
        let captured = self.read(&["rev-parse", "--verify", "--quiet", "HEAD"], LIST_LIMIT)?;
        if captured.ok {
            let id = captured.text().trim().to_string();
            return Ok((!id.is_empty()).then_some(id));
        }
        if captured.status == Some(1) {
            return Ok(None);
        }
        Err(Refusal::Git(captured.detail()))
    }

    /// The changed paths of the index or of the working tree.
    fn changes(&self, cached: bool) -> Result<Vec<Change>, Refusal> {
        let mut args = vec!["diff"];
        if cached {
            args.push("--cached");
        }
        args.extend(["--name-status", "-z", "--no-renames", "--no-ext-diff"]);
        let captured = self.read(&args, LIST_LIMIT)?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        if captured.truncated {
            return Err(Refusal::TooLarge);
        }
        Ok(parse_status(&captured.out, false))
    }

    /// The index entries as bytes, which is what a review is verified against.
    ///
    /// `--raw` carries each entry's blob id, so these bytes change whenever the
    /// staged content does, and comparing them is comparing the staged set
    /// itself rather than a list of names.
    fn index_entries(&self) -> Result<Vec<u8>, Refusal> {
        let captured = self.read(
            &[
                "diff",
                "--cached",
                "--raw",
                "-z",
                "--no-renames",
                "--no-abbrev",
                "--no-ext-diff",
            ],
            LIST_LIMIT,
        )?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        if captured.truncated {
            return Err(Refusal::TooLarge);
        }
        Ok(captured.out)
    }

    /// One side of `/diff`.
    fn section(&self, cached: bool) -> Result<Section, Refusal> {
        let files = self.changes(cached)?;
        if files.is_empty() {
            return Ok(Section {
                files,
                summary: "0 file(s)".to_string(),
                patch: String::new(),
                truncated: false,
            });
        }
        let mut patch_args = vec!["diff"];
        let mut summary_args = vec!["diff"];
        if cached {
            patch_args.push("--cached");
            summary_args.push("--cached");
        }
        patch_args.extend([
            "--patch",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
        ]);
        summary_args.extend(["--shortstat", "--no-renames"]);
        let captured = self.read(&patch_args, PATCH_LIMIT)?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        let (patch, truncated) = bounded_patch(&captured.text(), captured.truncated);
        let summary = self.read(&summary_args, LIST_LIMIT)?;
        let summary = if summary.ok {
            first_line(&summary.text())
        } else {
            String::new()
        };
        let summary = if summary.is_empty() {
            format!("{} file(s)", files.len())
        } else {
            summary
        };
        Ok(Section {
            files,
            summary,
            patch,
            truncated,
        })
    }

    /// Untracked paths, which neither diff shows.
    ///
    /// They are named so an empty unstaged section is not read as "the work
    /// tree is clean" when a new file simply was never added.
    fn untracked(&self) -> Result<(Vec<String>, bool), Refusal> {
        let captured = self.read(&["ls-files", "--others", "--exclude-standard", "-z"], UNTRACKED_LIMIT)?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        let paths: Vec<String> = captured
            .out
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
            .map(|field| String::from_utf8_lossy(field).into_owned())
            .collect();
        let more = captured.truncated || paths.len() > UNTRACKED_ROWS;
        Ok((paths.into_iter().take(UNTRACKED_ROWS).collect(), more))
    }

    /// The paths one commit changed.
    fn commit_changes(&self, id: &str) -> Result<Vec<Change>, Refusal> {
        let captured = self.read(
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-status",
                "-r",
                "-z",
                "--no-renames",
                "--root",
                id,
            ],
            LIST_LIMIT,
        )?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        if captured.truncated {
            return Err(Refusal::TooLarge);
        }
        Ok(parse_status(&captured.out, false))
    }

    /// The tree the index names, which is exactly what a commit would store.
    ///
    /// `write-tree` fails on an index with unmerged paths, so a conflicted
    /// merge is refused here instead of becoming a commit with two sides.
    fn write_tree(&self) -> Result<String, Refusal> {
        let captured = self.read(&["write-tree"], LIST_LIMIT)?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        let tree = captured.text().trim().to_string();
        if tree.is_empty() {
            return Err(Refusal::Git("git wrote no tree".to_string()));
        }
        Ok(tree)
    }

    /// The full reference `HEAD` names, or `None` when it is detached.
    fn head_ref(&self) -> Result<Option<String>, Refusal> {
        let captured = self.read(&["symbolic-ref", "-q", "HEAD"], LIST_LIMIT)?;
        if captured.ok {
            let name = captured.text().trim().to_string();
            return Ok((!name.is_empty()).then_some(name));
        }
        if captured.status == Some(1) {
            return Ok(None);
        }
        Err(Refusal::Git(captured.detail()))
    }

    /// A boolean from the machine's git configuration, `false` when unset.
    fn config_bool(&self, key: &str) -> Result<bool, Refusal> {
        let captured = self.read(&["config", "--bool", "--get", key], LIST_LIMIT)?;
        if captured.ok {
            return Ok(captured.text().trim() == "true");
        }
        // Exit 1 is git's answer for a key nobody set, which is not an error.
        if captured.status == Some(1) {
            return Ok(false);
        }
        Err(Refusal::Git(captured.detail()))
    }

    /// The hooks this repository configures around a commit, by name.
    ///
    /// `reference-transaction` is included because it is the one hook this
    /// client's `git update-ref` would run by itself: a repository that
    /// audits or vetoes ref updates must not be moved by a client that claims
    /// not to run hooks. A hook that exists at all is reported: git runs a hook
    /// file without regards to its mode on Windows, and a hooks directory
    /// holding a hand-written `pre-commit` is far more often active than a
    /// leftover. The `.sample` files a fresh repository ships do not match
    /// these names.
    fn commit_hooks(&self) -> Result<Vec<String>, Refusal> {
        let captured = self.read(&["rev-parse", "--git-path", "hooks"], LIST_LIMIT)?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        let path = PathBuf::from(captured.text().trim());
        if path.as_os_str().is_empty() {
            return Err(Refusal::Git("git names no hooks directory".to_string()));
        }
        let directory = if path.is_absolute() { path } else { self.root.join(path) };
        let mut found = Vec::new();
        for name in [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
            "reference-transaction",
        ] {
            match std::fs::symlink_metadata(directory.join(name)) {
                Ok(entry) if !entry.is_dir() => found.push(name.to_string()),
                Ok(_) => {}
                Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
                Err(problem) => {
                    return Err(Refusal::Git(format!(
                        "cannot read the hooks directory at {}: {problem}",
                        directory.display()
                    )));
                }
            }
        }
        Ok(found)
    }

    /// Write one commit object from `tree`, on `parent`, without running hooks.
    ///
    /// The identity is the machine's (`user.name`, `user.email`), and the
    /// signature, when `commit.gpgsign` asks for one, is made by the same
    /// configuration an ordinary commit uses.
    fn commit_tree(&self, tree: &str, parent: Option<&str>, message: &[u8], sign: bool) -> Result<String, Refusal> {
        let mut args = vec!["commit-tree", tree];
        if let Some(parent) = parent {
            args.push("-p");
            args.push(parent);
        }
        if sign {
            args.push("-S");
        }
        let captured = git_in(&self.root, &args, &[], Some(message), LIST_LIMIT, COMMIT_TIMEOUT)?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        let id = captured.text().trim().to_string();
        if id.is_empty() {
            return Err(Refusal::Git("git wrote no commit".to_string()));
        }
        Ok(id)
    }

    /// Move `reference` to `commit` only if it still names `expected`.
    ///
    /// The transaction holds exactly one instruction: the compare-and-swap on
    /// the approved local branch. A `verify HEAD` here would be wrong as well as
    /// redundant — git dereferences `HEAD` to that same branch, and a
    /// transaction may not name one reference twice. The current `HEAD` is read
    /// separately right before this call, and only the approved reference is
    /// ever touched: a checkout in the last instant can leave the branch moved
    /// with `HEAD` elsewhere, which the caller reports rather than hides.
    ///
    /// A reference that did not exist yet is expected at the all-zero object
    /// name, which git accepts only while the reference is still missing.
    fn update_ref(&self, reference: &str, commit: &str, expected: &str) -> Result<(), Refusal> {
        if reference
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(Refusal::Git(format!(
                "`{reference}` holds a character git cannot take on a transaction line"
            )));
        }
        let transaction = format!("update {reference} {commit} {expected}\n");
        let captured = git_in(
            &self.root,
            &["update-ref", "--stdin", "-m", "horon-tui: commit from reviewed staging"],
            &[],
            Some(transaction.as_bytes()),
            LIST_LIMIT,
            READ_TIMEOUT,
        )?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        Ok(())
    }

    /// The short subject of the commit `id` names.
    fn subject(&self, id: &str) -> Result<String, Refusal> {
        let captured = self.read(&["log", "-1", "--format=%s", id], LIST_LIMIT)?;
        if !captured.ok {
            return Err(Refusal::Git(captured.detail()));
        }
        Ok(captured.text().trim().to_string())
    }
}

/// Read the two sides of `/diff` for the directory the session works in.
pub(crate) fn diffs(directory: &Path, scope: Scope) -> Result<Diffs, Refusal> {
    let repo = Repo::open(directory)?;
    let branch = repo.branch()?;
    let staged = if scope.staged() {
        Some(repo.section(true)?)
    } else {
        None
    };
    let (unstaged, untracked, untracked_more) = if scope.unstaged() {
        let section = repo.section(false)?;
        let (paths, more) = repo.untracked()?;
        (Some(section), paths, more)
    } else {
        (None, Vec::new(), false)
    };
    Ok(Diffs {
        root: repo.root,
        branch,
        staged,
        unstaged,
        untracked,
        untracked_more,
    })
}

/// Read the staged set, its tree and the message into a review, without
/// committing.
///
/// The tree and the index entries read here are the identity `/commit --yes`
/// verifies: the approval covers exactly this tree, so a staged file added or
/// rewritten after the preview refuses the commit instead of joining it.
pub(crate) fn review(directory: &Path, message: &str) -> Result<Review, Refusal> {
    let message = message.trim();
    if message.is_empty() {
        return Err(Refusal::Message(
            "a commit needs a message; `/commit <message>` reviews the staged set with it".to_string(),
        ));
    }
    if message.chars().count() > MESSAGE_CHARS {
        return Err(Refusal::Message(format!(
            "the message is longer than {MESSAGE_CHARS} characters; shorten it and review again"
        )));
    }
    if message.contains('\0') {
        return Err(Refusal::Message(
            "the message holds a NUL byte, which git cannot store".to_string(),
        ));
    }
    let repo = Repo::open(directory)?;
    // The client writes the commit with plumbing, so a repository that expects
    // its hooks to run is refused before anything is read or shown.
    let hooks = repo.commit_hooks()?;
    if !hooks.is_empty() {
        return Err(Refusal::Hooked(hooks));
    }
    let Some(reference) = repo.head_ref()? else {
        return Err(Refusal::Detached);
    };
    let Some(branch) = reference.strip_prefix("refs/heads/").map(str::to_string) else {
        return Err(Refusal::NotABranch(reference.clone()));
    };
    let head = repo.head()?;
    let index = repo.index_entries()?;
    let files = parse_status(&index, true);
    if files.is_empty() {
        return Err(Refusal::NothingStaged);
    }
    if files.len() > REVIEW_FILES {
        return Err(Refusal::TooManyFiles(files.len()));
    }
    let tree = repo.write_tree()?;
    let id = fingerprint(&index, head.as_deref(), Some(branch.as_str()));
    Ok(Review {
        root: repo.root,
        branch,
        reference,
        head,
        files,
        index,
        tree,
        id,
        message: message.to_string(),
    })
}

/// Commit exactly the tree `review` described, or refuse.
///
/// The commit is one `git commit-tree` from the approved tree and the approved
/// parent, then one compare-and-swap `git update-ref` on the approved local
/// branch. No hook runs and no other reference moves: a branch that changed
/// after the review, a `HEAD` that moved, or an index whose tree differs all
/// fail before the commit object is made or before the reference is touched.
pub(crate) fn commit(review: &Review) -> Result<Committed, Refusal> {
    let repo = Repo::open(&review.root)?;
    if repo.root != review.root {
        return Err(stale(review, "the work tree"));
    }
    // A hook that appeared after the review must not be skipped silently.
    let hooks = repo.commit_hooks()?;
    if !hooks.is_empty() {
        return Err(Refusal::Hooked(hooks));
    }
    if repo.head_ref()?.as_deref() != Some(review.reference.as_str()) {
        return Err(stale(review, "the branch"));
    }
    if repo.head()? != review.head {
        return Err(stale(review, "HEAD"));
    }
    if repo.index_entries()? != review.index {
        return Err(stale(review, "the staged index"));
    }
    if repo.write_tree()? != review.tree {
        // The index entries matched but the tree they name did not, which is
        // the tree/entry mismatch a `git add` that rewrote stat data can leave.
        return Err(stale(review, "the staged index"));
    }
    let sign = repo.config_bool("commit.gpgsign")?;
    let written = repo.commit_tree(&review.tree, review.head.as_deref(), review.message.as_bytes(), sign)?;
    // The transaction pins the approved branch only: `HEAD` would name that
    // same reference after dereferencing, and a transaction cannot name one
    // reference twice. So HEAD's symbolic target and commit are read once more
    // here, immediately before the swap, and the post-check below reports a
    // HEAD that moved in the last instant instead of hiding it.
    if repo.head_ref()?.as_deref() != Some(review.reference.as_str()) || repo.head()? != review.head {
        return Err(stale(review, "HEAD"));
    }
    let expected = review.head.clone().unwrap_or_else(|| ZERO_OID.to_string());
    repo.update_ref(&review.reference, &written, &expected)?;
    let subject = repo.subject(&written)?;
    let changes = repo.commit_changes(&written)?;
    let mut notes = Vec::new();
    if repo.head()?.as_deref() != Some(written.as_str()) {
        // The reference moved, but HEAD followed it only if it still names the
        // branch; a checkout in the last instant leaves the branch ahead of a
        // working tree that is somewhere else.
        notes.push(format!(
            "`{}` now names {} but HEAD points elsewhere; check that branch out to see the commit",
            review.reference,
            short_id(&written)
        ));
    }
    let reviewed: BTreeSet<&str> = review.files.iter().map(|change| change.path.as_str()).collect();
    let landed: BTreeSet<&str> = changes.iter().map(|change| change.path.as_str()).collect();
    for path in landed.difference(&reviewed) {
        notes.push(format!(
            "`{path}` is in the commit but was not in the review; check the branch"
        ));
    }
    for path in reviewed.difference(&landed) {
        notes.push(format!("`{path}` was reviewed but is not part of the commit"));
    }
    Ok(Committed {
        id: written,
        subject,
        branch: Some(review.branch.clone()),
        files: changes.len(),
        notes,
    })
}

/// The refusal a reviewed set earns when it no longer describes the workspace.
fn stale(review: &Review, moved: &'static str) -> Refusal {
    Refusal::Stale {
        reviewed: review.id.clone(),
        moved,
    }
}

/// The object name a reference that does not exist yet is expected at.
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Read `--name-status -z` or `--raw -z --no-renames` output into changes.
///
/// The NUL-separated pairs are status (or raw metadata ending with status) and
/// path, in that order, with no quoting to undo: a path may hold a space, a
/// newline or a leading dash and still be exactly the bytes git wrote.
fn parse_status(bytes: &[u8], raw: bool) -> Vec<Change> {
    let mut fields = bytes.split(|byte| *byte == 0);
    let mut changes = Vec::new();
    while let Some(meta) = fields.next() {
        if meta.is_empty() {
            continue;
        }
        let Some(path) = fields.next() else {
            break;
        };
        let status_bytes = if raw {
            meta.iter()
                .rposition(|byte| *byte == b' ')
                .and_then(|space| meta.get(space + 1..))
                .unwrap_or(b"?")
        } else {
            meta
        };
        let status = String::from_utf8_lossy(status_bytes);
        changes.push(Change {
            status: status.chars().next().unwrap_or('?'),
            path: String::from_utf8_lossy(path).into_owned(),
        });
    }
    changes
}

/// Cut a patch to the lines and columns the transcript draws.
fn bounded_patch(text: &str, cut_by_read: bool) -> (String, bool) {
    let mut kept = String::new();
    let mut cut = cut_by_read;
    for (lines, line) in text.lines().enumerate() {
        if lines == PATCH_LINES {
            cut = true;
            break;
        }
        let piece: String = line
            .chars()
            .take(PATCH_LINE_CHARS)
            .filter(|character| *character == '\t' || !character.is_control())
            .collect();
        if line.chars().count() > PATCH_LINE_CHARS {
            cut = true;
        }
        if !kept.is_empty() {
            kept.push('\n');
        }
        kept.push_str(&piece);
    }
    if cut {
        if !kept.is_empty() {
            kept.push('\n');
        }
        kept.push_str("… diff truncated");
    }
    (kept, cut)
}

/// A short, stable id over the staged entries, `HEAD` and the branch.
///
/// FNV-1a: the id only has to tell one reviewed set from another in one
/// workspace, and it has to be the same in the preview and in the refusal
/// without a hash crate on the dependency list.
fn fingerprint(index: &[u8], head: Option<&str>, branch: Option<&str>) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    feed(index);
    feed(head.unwrap_or("").as_bytes());
    feed(branch.unwrap_or("").as_bytes());
    format!("{:012x}", hash & 0x0000_ffff_ffff_ffff)
}

/// The first twelve characters of an object id, which is how a commit is named.
fn short_id(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}
