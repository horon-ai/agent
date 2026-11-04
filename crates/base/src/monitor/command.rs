use super::{
    ChangeKind, INLINE_OUTPUT_BYTES, MonitorChange, MonitorError, MonitorResult, MonitorSpec, OUTPUT_QUEUE_LINES,
    SPOOL_FILE_BYTES, SPOOL_FILE_COUNT,
};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Where a monitor spools output it cannot keep inline.
pub trait SpoolSink: Send {
    /// Append bytes. `Err(SpoolBlocked)` means the spool is full right now, so
    /// the caller keeps the bytes queued rather than growing its own buffer.
    fn write(&mut self, bytes: &[u8]) -> Result<(), SpoolBlocked>;

    /// Bytes that rotation discarded, so a monitor never pretends it kept
    /// everything.
    fn rotated_bytes(&self) -> u64;
}

/// The spool could not accept more bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpoolBlocked;

/// A spool of [`SPOOL_FILE_COUNT`] files of [`SPOOL_FILE_BYTES`] each.
///
/// Writes append to the current file; a full file rotates to the next and
/// truncates the oldest. Every byte rotation discards is counted in
/// [`FileSpool::rotated_bytes`].
pub struct FileSpool {
    dir: PathBuf,
    current: usize,
    written: u64,
    rotated: u64,
    /// Set when a rotation could not open its next file. The sink then reports
    /// blocked instead of accepting bytes it cannot keep.
    rotation_failed: bool,
}

impl FileSpool {
    pub fn new(dir: PathBuf) -> MonitorResult<Self> {
        std::fs::create_dir_all(&dir)
            .map_err(|error| MonitorError::io(format!("cannot create spool dir {}: {error}", dir.display())))?;
        Ok(Self {
            dir,
            current: 0,
            written: 0,
            rotated: 0,
            rotation_failed: false,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file_path(&self, index: usize) -> PathBuf {
        self.dir.join(format!("spool-{index}.log"))
    }

    fn rotate(&mut self) {
        self.rotated = self.rotated.saturating_add(self.written);
        self.current = (self.current + 1) % SPOOL_FILE_COUNT;
        self.written = 0;
        let path = self.file_path(self.current);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path);
    }
}

impl SpoolSink for FileSpool {
    fn write(&mut self, bytes: &[u8]) -> Result<(), SpoolBlocked> {
        if self.rotation_failed {
            return Err(SpoolBlocked);
        }
        let mut rest = bytes;
        while !rest.is_empty() {
            let room = SPOOL_FILE_BYTES.saturating_sub(self.written);
            if room == 0 {
                self.rotate();
                continue;
            }
            let take = room.min(rest.len() as u64) as usize;
            let path = self.file_path(self.current);
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|_| SpoolBlocked)?;
            use std::io::Write;
            file.write_all(&rest[..take]).map_err(|_| SpoolBlocked)?;
            self.written = self.written.saturating_add(take as u64);
            rest = &rest[take..];
        }
        Ok(())
    }

    fn rotated_bytes(&self) -> u64 {
        self.rotated
    }
}

/// What the bounded buffers hold right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputStats {
    pub inline_bytes: usize,
    pub queued: usize,
    pub spooled_bytes: u64,
    pub rotated_bytes: u64,
    pub dropped_bytes: u64,
    pub overflowed: bool,
}

/// Output kept inside every bound at once.
///
/// * `inline` holds the last [`INLINE_OUTPUT_BYTES`], cut on a UTF-8 boundary so
///   a report never splits a character.
/// * `queue` holds at most [`OUTPUT_QUEUE_LINES`] lines waiting for the spool.
/// * a full queue writes straight to the spool; a spool that refuses the write
///   raises `overflowed` and counts the dropped bytes instead of buffering them.
pub struct BoundedOutput {
    inline: String,
    queue: VecDeque<Vec<u8>>,
    spool: Box<dyn SpoolSink>,
    spooled: u64,
    dropped: u64,
    overflowed: bool,
}

impl BoundedOutput {
    pub fn new(spool: Box<dyn SpoolSink>) -> Self {
        Self {
            inline: String::new(),
            queue: VecDeque::new(),
            spool,
            spooled: 0,
            dropped: 0,
            overflowed: false,
        }
    }

    pub fn push(&mut self, line: &str) {
        self.push_bytes(line.as_bytes());
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.append_inline(bytes);
        if self.queue.len() < OUTPUT_QUEUE_LINES {
            self.queue.push_back(bytes.to_vec());
            return;
        }
        match self.spool.write(bytes) {
            Ok(()) => self.spooled = self.spooled.saturating_add(bytes.len() as u64),
            Err(SpoolBlocked) => {
                self.dropped = self.dropped.saturating_add(bytes.len() as u64);
                self.overflowed = true;
            }
        }
    }

    /// Record bytes a reader had to drop because the line queue was full.
    pub fn note_dropped(&mut self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.dropped = self.dropped.saturating_add(bytes);
        self.overflowed = true;
    }

    fn append_inline(&mut self, bytes: &[u8]) {
        self.inline.push_str(&String::from_utf8_lossy(bytes));
        if self.inline.len() <= INLINE_OUTPUT_BYTES {
            return;
        }
        let mut cut = self.inline.len() - INLINE_OUTPUT_BYTES;
        while cut < self.inline.len() && !self.inline.is_char_boundary(cut) {
            cut += 1;
        }
        self.inline.drain(..cut);
    }

    /// Move queued lines into the spool until it refuses or the queue empties. A
    /// refused write leaves its line queued, so nothing is lost and nothing grows.
    pub fn drain(&mut self) -> usize {
        let mut moved = 0;
        while let Some(front) = self.queue.front() {
            match self.spool.write(front) {
                Ok(()) => {
                    self.spooled = self.spooled.saturating_add(front.len() as u64);
                    self.queue.pop_front();
                    moved += 1;
                }
                Err(SpoolBlocked) => {
                    self.overflowed = true;
                    break;
                }
            }
        }
        moved
    }

    pub fn inline(&self) -> &str {
        &self.inline
    }

    pub fn stats(&self) -> OutputStats {
        OutputStats {
            inline_bytes: self.inline.len(),
            queued: self.queue.len(),
            spooled_bytes: self.spooled,
            rotated_bytes: self.spool.rotated_bytes(),
            dropped_bytes: self.dropped,
            overflowed: self.overflowed,
        }
    }
}

/// The permission, working-directory, and shell policy a command monitor obeys.
///
/// The `bash` tool owns this policy and a monitor must not be a way around it, so
/// every command monitor call goes through the same implementation instead of
/// spawning a shell of its own.
pub trait CommandPolicy: Send + Sync {
    /// Refuse a command this session may not run. Returning text is a refusal.
    fn check(&self, command: &str, cwd: Option<&Path>, session_id: &str) -> Result<(), String>;

    fn working_dir(&self, requested: Option<&Path>, session_id: &str) -> Option<PathBuf>;
}

/// The policy for a caller with no session gate: allow, and use the directory it
/// asked for.
pub struct PermissiveCommandPolicy;

impl CommandPolicy for PermissiveCommandPolicy {
    fn check(&self, _command: &str, _cwd: Option<&Path>, _session_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn working_dir(&self, requested: Option<&Path>, _session_id: &str) -> Option<PathBuf> {
        requested.map(Path::to_path_buf)
    }
}

/// Which stream a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandStream {
    Stdout,
    Stderr,
}

impl CommandStream {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// One line the child wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandLine {
    pub stream: CommandStream,
    pub text: String,
}

/// The bounded hand-off between a reader thread and the monitor.
///
/// Reader threads push here instead of straight into the output buffer, so the
/// queue is the only unbounded-growth risk and it is capped at
/// [`OUTPUT_QUEUE_LINES`]: a full queue drops the line and counts its bytes.
#[derive(Default)]
struct LineQueue {
    lines: Mutex<VecDeque<CommandLine>>,
    dropped: AtomicU64,
}

impl LineQueue {
    fn push(&self, line: CommandLine) {
        let mut lines = self.lines.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if lines.len() >= OUTPUT_QUEUE_LINES {
            self.dropped.fetch_add(line.text.len() as u64, Ordering::Relaxed);
            return;
        }
        lines.push_back(line);
    }

    fn drain(&self) -> Vec<CommandLine> {
        let mut lines = self.lines.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        lines.drain(..).collect()
    }

    fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, Ordering::Relaxed)
    }
}

/// A command that runs at most once per explicit start.
///
/// There is no supervisor loop and no ping/pong: the child runs to exit, its
/// lines are observed, and the monitor reports the exit. Only
/// [`CommandWatcher::start`] launches it, and the monitor layer calls that only
/// from an explicit `rearm`. The child and its reader threads are plain OS
/// processes and threads, so observing it never needs an async reactor.
pub struct CommandWatcher {
    command: String,
    cwd: Option<PathBuf>,
    queue: Arc<LineQueue>,
    readers: Vec<std::thread::JoinHandle<()>>,
    output: BoundedOutput,
    child: Option<Child>,
    pid: Option<u32>,
    exit: Option<i32>,
}

impl CommandWatcher {
    pub fn prepare(spec: &MonitorSpec, spool: Box<dyn SpoolSink>) -> MonitorResult<Self> {
        let command = spec
            .command
            .clone()
            .ok_or_else(|| MonitorError::invalid("a command monitor needs a command"))?;
        Ok(Self {
            command,
            cwd: spec.cwd.as_ref().map(PathBuf::from),
            queue: Arc::new(LineQueue::default()),
            readers: Vec::new(),
            output: BoundedOutput::new(spool),
            child: None,
            pid: None,
            exit: None,
        })
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit
    }

    pub fn output(&self) -> &BoundedOutput {
        &self.output
    }

    /// Start the child. Refuses while one still runs, so a doubled `rearm` cannot
    /// fork a second process.
    pub fn start(&mut self, session_id: &str, policy: &dyn CommandPolicy) -> MonitorResult<u32> {
        if self.child.is_some() {
            return Err(MonitorError::invalid(
                "this command monitor is still running; cancel it or wait for the exit",
            ));
        }
        policy
            .check(&self.command, self.cwd.as_deref(), session_id)
            .map_err(MonitorError::invalid)?;
        let start_dir = policy.working_dir(self.cwd.as_deref(), session_id);
        let mut command = shell_command(&self.command);
        command
            .env("HORON_AGENT_SESSION_ID", session_id)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = start_dir {
            command.current_dir(dir);
        }
        let mut child = command
            .spawn()
            .map_err(|error| MonitorError::io(format!("cannot start the watched command: {error}")))?;
        let pid = child.id();

        if let Some(stdout) = child.stdout.take() {
            self.readers
                .push(spawn_reader(stdout, CommandStream::Stdout, Arc::clone(&self.queue)));
        }
        if let Some(stderr) = child.stderr.take() {
            self.readers
                .push(spawn_reader(stderr, CommandStream::Stderr, Arc::clone(&self.queue)));
        }
        self.child = Some(child);
        self.pid = Some(pid);
        self.exit = None;
        Ok(pid)
    }

    /// Take everything the child produced since the last poll.
    pub fn poll(&mut self) -> Vec<MonitorChange> {
        let mut changes = Vec::new();
        let fresh = self.queue.drain();
        self.output.note_dropped(self.queue.take_dropped());
        if !fresh.is_empty() {
            for line in &fresh {
                self.output.push(&format!("[{}] {}\n", line.stream.as_str(), line.text));
            }
            changes.push(
                MonitorChange::new(ChangeKind::Output, summarize_lines(&fresh)).with_detail(serde_json::json!({
                    "lines": fresh.len(),
                    "output": self.output.inline(),
                    "stats": stats_value(&self.output.stats()),
                })),
            );
        }
        self.output.drain();

        if let Some(status) = self.child.as_mut().and_then(|child| child.try_wait().ok().flatten()) {
            self.finish(status, &mut changes);
        }
        changes
    }

    /// Block until the child exits, then report everything it produced.
    ///
    /// This is the barrier a caller uses when it wants the terminal state now,
    /// instead of sampling ticks and hoping the child finished.
    pub fn wait(&mut self) -> Vec<MonitorChange> {
        let mut changes = Vec::new();
        if let Some(mut child) = self.child.take() {
            let status = child.wait();
            for reader in self.readers.drain(..) {
                let _ = reader.join();
            }
            match status {
                Ok(status) => self.finish_with_child(status, &mut changes),
                Err(error) => {
                    self.exit = None;
                    changes.push(MonitorChange::new(
                        ChangeKind::Exited,
                        format!("command could not be waited on: {error}"),
                    ));
                }
            }
        }
        changes
    }

    fn finish(&mut self, status: std::process::ExitStatus, changes: &mut Vec<MonitorChange>) {
        self.child = None;
        self.finish_with_child(status, changes);
    }

    fn finish_with_child(&mut self, status: std::process::ExitStatus, changes: &mut Vec<MonitorChange>) {
        let fresh = self.queue.drain();
        self.output.note_dropped(self.queue.take_dropped());
        if !fresh.is_empty() {
            for line in &fresh {
                self.output.push(&format!("[{}] {}\n", line.stream.as_str(), line.text));
            }
            changes.push(
                MonitorChange::new(ChangeKind::Output, summarize_lines(&fresh)).with_detail(serde_json::json!({
                    "lines": fresh.len(),
                    "output": self.output.inline(),
                    "stats": stats_value(&self.output.stats()),
                })),
            );
        }
        self.output.drain();
        self.exit = status.code();
        changes.push(
            MonitorChange::new(
                ChangeKind::Exited,
                format!("command exited with status {}", describe_exit(&status)),
            )
            .with_detail(serde_json::json!({
                "exit_code": status.code(),
                "success": status.success(),
                "output": self.output.inline(),
                "stats": stats_value(&self.output.stats()),
            })),
        );
    }

    /// Kill the child's whole process group, the way the `bash` tool cancels.
    pub fn cancel(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = crate::platform::signal_detached_process_group(child.id(), libc::SIGKILL);
            let _ = child.kill();
            let _ = child.wait();
        }
        self.readers.clear();
    }
}

impl Drop for CommandWatcher {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn describe_exit(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => code.to_string(),
        None => "a signal".to_string(),
    }
}

fn summarize_lines(lines: &[CommandLine]) -> String {
    let last = lines.last().map(|line| line.text.trim_end()).unwrap_or_default();
    let last = if last.chars().count() > 160 {
        let cut: String = last.chars().take(160).collect();
        format!("{cut}…")
    } else {
        last.to_string()
    };
    if lines.len() == 1 {
        format!("command wrote 1 new line: {last}")
    } else {
        format!("command wrote {} new lines, last: {last}", lines.len())
    }
}

fn stats_value(stats: &OutputStats) -> serde_json::Value {
    serde_json::json!({
        "inline_bytes": stats.inline_bytes,
        "queued": stats.queued,
        "spooled_bytes": stats.spooled_bytes,
        "rotated_bytes": stats.rotated_bytes,
        "dropped_bytes": stats.dropped_bytes,
        "overflowed": stats.overflowed,
    })
}

fn spawn_reader<R>(stream: R, which: CommandStream, queue: Arc<LineQueue>) -> std::thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {
                    while line.last().is_some_and(|byte| *byte == b'\n' || *byte == b'\r') {
                        line.pop();
                    }
                    queue.push(CommandLine {
                        stream: which,
                        text: String::from_utf8_lossy(&line).to_string(),
                    });
                }
                Err(_) => break,
            }
        }
    })
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(command);
    if let Some(dir) = scratch_dir() {
        cmd.env("TMPDIR", &dir).env("HORON_AGENT_SCRATCH_DIR", dir);
    }
    cmd
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/D", "/S", "/C"]).arg(command);
    cmd
}

fn scratch_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("HORON_AGENT_SCRATCH_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::storage::horon_agent_dir().ok().map(|dir| dir.join("scratch")))?;
    crate::storage::ensure_dir(&dir).ok()?;
    Some(dir)
}
