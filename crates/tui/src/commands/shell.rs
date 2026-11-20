//! The local shell behind `!command` and `!!command`.
//!
//! The command is typed by the operator into the composer and is run by this
//! client, in the client's working directory. It is never taken from a
//! transcript item, a tool result or a model message, and neither the command
//! nor its output is sent to the harness: the harness sees the session exactly
//! as it was before the line was submitted. `!` keeps the output in the
//! transcript; `!!` is the excluded form, so its command and output are
//! dropped instead of recorded, and only the exit status remains.
//!
//! The header is recorded and drawn before the command starts. The command
//! itself runs on a worker thread so the interface keeps drawing and reading
//! keys while it runs, and the result is collected by the event loop on a later
//! frame. One command runs at a time: a second submission while one is pending
//! or running is refused rather than queued. A command still running when the
//! client leaves is stopped by the terminal restore path.
//!
//! Process control belongs to the platform adapter. This module builds the
//! shell with `process::shell_command`, places it in its own group with
//! `process::isolate_process_group`, and stops the tree with the platform's
//! terminate helpers, so there is no operating-system branch here.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use horon_agent_sdk::process;

use crate::app::App;

/// How long one local command may run before its process tree is stopped.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Most bytes of output kept for the transcript.
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

/// Most lines of output kept for the transcript.
const MAX_OUTPUT_LINES: usize = 200;

/// Most characters one output line keeps before it is cut.
const MAX_LINE_CHARS: usize = 400;

/// Most bytes read from the command's output file, so a command that writes
/// without end cannot fill memory before the caps above are applied.
const MAX_READ_BYTES: u64 = 64 * 1024;

/// The process group of the command in flight, for the exit path.
///
/// Zero means nothing is running. The terminal's restore path stops this group
/// so a client that leaves does not leave a shell behind.
static ACTIVE_PID: AtomicU32 = AtomicU32::new(0);

/// The number in the next output file's name.
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

/// The one command that may be pending or running, and its result.
struct State {
    /// A header was drawn, and the worker may not start until the next frame.
    pending: Option<Launch>,
    /// A worker owns the active command.
    busy: bool,
    /// Results the event loop has not collected.
    done: VecDeque<Done>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            pending: None,
            busy: false,
            done: VecDeque::new(),
        }
    }
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

/// One command waiting for the frame that shows its header to be drawn.
struct Launch {
    command: String,
    cwd: PathBuf,
    excluded: bool,
}

/// One command that ended, waiting for the event loop to draw it.
struct Done {
    excluded: bool,
    result: Result<Run, ShellError>,
}

/// Why a local command could not run or its output could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellError(String);

impl ShellError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ShellError {}

/// Releases the one-command slot however the worker ends.
struct Worker;

impl Drop for Worker {
    fn drop(&mut self) {
        let mut state = state();
        state.busy = false;
    }
}

/// What one command did, in the words the transcript shows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    /// `exit 0`, `exit 2`, `killed by signal 15 (SIGTERM)`, or the timeout line.
    pub status: String,
    /// Whether the command exited with status zero.
    pub ok: bool,
    /// The bounded, cleaned output, or nothing when the command printed nothing.
    pub output: String,
}

/// The lock, with a poisoned guard treated as the state it was holding.
fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Start the pending command, after the frame that shows its header.
pub(crate) fn start_pending() {
    let launch = {
        let mut state = state();
        if state.busy {
            return;
        }
        let Some(launch) = state.pending.take() else {
            return;
        };
        state.busy = true;
        launch
    };
    let excluded = launch.excluded;
    let started = std::thread::Builder::new()
        .name("horon-tui-shell".to_string())
        .spawn(move || {
            // A panic in the worker must not leave the one-command slot held:
            // the guard releases it on the way out, and the next submission is
            // accepted.
            let _worker = Worker;
            let result = run(&launch.command, &launch.cwd);
            let mut state = state();
            state.busy = false;
            state.done.push_back(Done { excluded, result });
        });
    if let Err(error) = started {
        let mut state = state();
        state.busy = false;
        state.done.push_back(Done {
            excluded,
            result: Err(ShellError::new(format!("cannot start the local shell worker: {error}"))),
        });
    }
}

/// Collect finished commands into the transcript, reporting whether any came.
pub(crate) fn drain(app: &mut App) -> bool {
    let done: Vec<Done> = {
        let mut state = state();
        state.done.drain(..).collect()
    };
    if done.is_empty() {
        return false;
    }
    for done in done {
        app.finish_local_shell(done.excluded, done.result);
    }
    true
}

/// Stop the command in flight, for the terminal's exit and signal path.
pub(crate) fn kill_active_group() {
    let pid = ACTIVE_PID.swap(0, Ordering::SeqCst);
    if pid != 0 {
        // Best effort: the command may already have ended, and the caller is
        // about to end the client anyway.
        match process::force_stop_process_group(pid) {
            Ok(()) => {}
            Err(_) => {}
        }
    }
}

/// Run one command in `cwd` and collect what it printed.
///
/// `Err` is a command that never started or whose output could not be read;
/// a command that started and failed comes back as an `Ok` with its status.
fn run(command: &str, cwd: &Path) -> Result<Run, ShellError> {
    if command.trim().is_empty() {
        return Err(ShellError::new("the command is empty"));
    }
    if !cwd.is_dir() {
        return Err(ShellError::new(format!("{} is not a working directory", cwd.display())));
    }
    let (_scratch, output_file) = Scratch::new()?;
    let error_file = output_file
        .try_clone()
        .map_err(|error| ShellError::new(format!("cannot open the output file: {error}")))?;
    let stdout = Stdio::from(
        output_file
            .try_clone()
            .map_err(|error| ShellError::new(format!("cannot open the output file: {error}")))?,
    );
    let stderr = Stdio::from(error_file);
    let mut command = process::shell_command(command, None);
    command
        .current_dir(cwd)
        // A command that waits for input would otherwise hold the worker until
        // the timeout with nothing on screen saying why.
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    // Its own group, so the timeout and the client's exit can end the children
    // too. The platform adapter decides what that means on this build.
    process::isolate_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| ShellError::new(format!("cannot start a shell in {}: {error}", cwd.display())))?;
    let pid = child.id();
    let mut active = Active::new(pid);
    let deadline = Instant::now() + TIMEOUT;
    let exited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => break None,
            Err(error) => {
                process::kill_process_group(pid);
                active.disarm();
                drop(child.wait());
                return Err(ShellError::new(format!("cannot wait for the command: {error}")));
            }
        }
    };
    let ok = exited.as_ref().is_some_and(|status| status.success());
    let status = match exited {
        Some(status) => {
            // The shell has ended; a background child it left is still in the
            // group, and this is what takes it down on a platform that has
            // groups.
            process::kill_process_group(pid);
            active.disarm();
            describe(Some(status), false)
        }
        None => {
            process::terminate_process_group(pid, Duration::from_secs(1));
            drop(child.wait());
            active.disarm();
            describe(None, true)
        }
    };
    let output = read_output(output_file)?;
    Ok(Run { ok, status, output })
}

/// Keep the command's group for the exit path, and stop it if this run unwinds.
///
/// Rust does not kill a child when its `Child` is dropped, so a panic between
/// the spawn and the normal stop would otherwise leave the command running.
struct Active {
    pid: u32,
    armed: bool,
}

impl Active {
    fn new(pid: u32) -> Self {
        ACTIVE_PID.store(pid, Ordering::SeqCst);
        Self { pid, armed: true }
    }

    /// Clear the recorded group after the command and its children are gone.
    fn disarm(&mut self) {
        if ACTIVE_PID.load(Ordering::SeqCst) == self.pid {
            ACTIVE_PID.store(0, Ordering::SeqCst);
        }
        self.armed = false;
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        if self.armed {
            // The command is being abandoned: a panic, an early return or a
            // write that failed. The platform takes the tree down.
            match process::force_stop_process_group(self.pid) {
                Ok(()) => {}
                Err(_) => {}
            }
        }
        if ACTIVE_PID.load(Ordering::SeqCst) == self.pid {
            ACTIVE_PID.store(0, Ordering::SeqCst);
        }
    }
}

/// The output file, removed when the run is over.
///
/// A named file rather than a pipe: one file receives both streams in the
/// order the shell wrote them, and a reader that stops at the cap cannot leave
/// a writer blocked on a full pipe. The name is created with `create_new`, so a
/// link planted in a shared temporary directory cannot redirect the output.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<(Self, File), ShellError> {
        let directory = std::env::temp_dir();
        for _ in 0..16 {
            let name = format!(
                "horon-tui-shell-{}-{}.log",
                std::process::id(),
                NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
            );
            let path = directory.join(name);
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            match options.open(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(ShellError::new(format!("cannot create a temporary file: {error}"))),
            }
        }
        Err(ShellError::new(
            "cannot create a temporary file: every name tried was taken",
        ))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // The file handle is closed before this runs, because `read_output`
        // consumes it; Windows cannot remove an open file. A file already gone
        // is the only failure, and there is nothing left to report it to.
        drop(std::fs::remove_file(&self.path));
    }
}

/// What the transcript calls the command's end.
fn describe(status: Option<std::process::ExitStatus>, timed_out: bool) -> String {
    if timed_out {
        return format!("stopped after {}s: the process tree was stopped", TIMEOUT.as_secs());
    }
    let Some(status) = status else {
        return "finished without an exit status".to_string();
    };
    if let Some(signal) = process::exit_signal(&status) {
        return format!("killed by signal {signal} ({})", process::signal_name(signal));
    }
    match status.code() {
        Some(0) => "exit 0".to_string(),
        Some(code) => format!("exit {code}"),
        None => "finished without an exit status".to_string(),
    }
}

/// Read the command's output file, bounded for the terminal.
fn read_output(mut file: File) -> Result<String, ShellError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| ShellError::new(format!("cannot read the command output: {error}")))?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_READ_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ShellError::new(format!("cannot read the command output: {error}")))?;
    let cut_by_read = bytes.len() as u64 > MAX_READ_BYTES;
    if cut_by_read {
        bytes.truncate(MAX_READ_BYTES as usize);
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut bounded = bounded(&text);
    if cut_by_read && !bounded.ends_with("… output truncated") {
        if !bounded.is_empty() {
            bounded.push('\n');
        }
        bounded.push_str("… output truncated");
    }
    Ok(bounded)
}

/// Bound the output to the display caps.
///
/// The result ends with `… output truncated` when anything was cut.
fn bounded(raw: &str) -> String {
    let mut kept = String::new();
    let mut lines = 0usize;
    let mut cut = false;
    for line in raw.lines() {
        if lines == MAX_OUTPUT_LINES {
            cut = true;
            break;
        }
        let mut piece: String = line
            .chars()
            .take(MAX_LINE_CHARS)
            .filter(|character| *character == '\t' || !character.is_control())
            .collect();
        if line.chars().count() > MAX_LINE_CHARS {
            piece.push('…');
            cut = true;
        }
        let separator = usize::from(lines > 0);
        if kept.len() + separator + piece.len() > MAX_OUTPUT_BYTES {
            let room = MAX_OUTPUT_BYTES.saturating_sub(kept.len() + separator);
            if room > 0 {
                if separator == 1 {
                    kept.push('\n');
                }
                let cut_at = floor_char_boundary(&piece, room);
                kept.push_str(&piece[..cut_at]);
            }
            cut = true;
            break;
        }
        if separator == 1 {
            kept.push('\n');
        }
        kept.push_str(&piece);
        lines += 1;
    }
    if cut {
        if !kept.is_empty() {
            kept.push('\n');
        }
        kept.push_str("… output truncated");
    }
    kept
}

/// The largest boundary in `text` no further than `limit` bytes in.
fn floor_char_boundary(text: &str, limit: usize) -> usize {
    if limit >= text.len() {
        return text.len();
    }
    let mut boundary = limit;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

impl App {
    /// Answer `!command` or `!!command` from the line the operator submitted.
    ///
    /// The header is recorded before the command runs, and the worker starts
    /// only after the frame that draws it, so the line the client leaves always
    /// shows where the command runs and that it was local before any output can
    /// arrive. A command that cannot start is a warning and nothing else.
    pub(crate) fn run_local_shell(&mut self, command: &str, excluded: bool) {
        let command = command.trim();
        if command.is_empty() {
            self.warn(super::shell_usage());
            return;
        }
        {
            let state = state();
            if state.busy || state.pending.is_some() || !state.done.is_empty() {
                drop(state);
                self.warn("local shell: a command is already running; wait for it to finish");
                return;
            }
        }
        let Some(cwd) = local_directory(self.cwd.as_deref()) else {
            let named = self.cwd.clone().unwrap_or_else(|| "the working directory".to_string());
            self.warn(format!("local shell: {named} is not a working directory; nothing ran"));
            return;
        };
        let location = cwd.display().to_string();
        if excluded {
            self.transcript.push_live_notice(format!(
                "local shell (excluded) · {location} · runs in this terminal, not the agent session"
            ));
        } else {
            let line = command.replace('\n', " ⏎ ");
            self.transcript.push_live_notice(format!(
                "local shell · {location} · runs in this terminal, not the agent session\n$ {line}"
            ));
        }
        let mut state = state();
        state.pending = Some(Launch {
            command: command.to_string(),
            cwd,
            excluded,
        });
    }

    /// Draw one finished local command.
    fn finish_local_shell(&mut self, excluded: bool, result: Result<Run, ShellError>) {
        match result {
            Ok(done) if excluded => {
                let line = format!("local shell: {}; the command and its output were not kept", done.status);
                if done.ok {
                    self.transcript.push_live_notice(line);
                } else {
                    self.warn(line);
                }
            }
            Ok(done) => {
                if !done.output.is_empty() {
                    self.transcript
                        .push_live_notice(format!("local shell output:\n{}", done.output));
                }
                let line = format!("local shell: {}", done.status);
                if done.ok {
                    self.transcript.push_live_notice(line);
                } else {
                    self.warn(line);
                }
            }
            Err(message) => self.warn(format!("local shell: {message}")),
        }
        self.scroll.to_bottom();
    }
}

/// The directory a local command runs in, resolved against the process.
fn local_directory(configured: Option<&str>) -> Option<PathBuf> {
    let directory = match configured {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => match std::env::current_dir() {
            Ok(directory) => directory,
            Err(_) => return None,
        },
    };
    directory.is_dir().then_some(directory)
}
