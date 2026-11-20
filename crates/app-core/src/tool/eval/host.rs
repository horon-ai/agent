use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::task::Poll;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, ReadBuf};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use super::output::Capture;

const RUNNER: &str = include_str!("runner.py");
const MAX_FRAME: usize = 256 * 1024;
const ENV_KEYS: &[&str] = &[
    "PATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SYSTEMROOT",
    "WINDIR",
    "TEMP",
    "TMP",
    "TMPDIR",
];

#[derive(Serialize)]
struct Request<'a> {
    id: u64,
    code: &'a str,
}

pub(super) fn code_fits_frame(code: &str) -> bool {
    // Reserve the full decimal width of a u64 cell ID and the JSON envelope.
    let budget = MAX_FRAME - b"{\"id\":18446744073709551615,\"code\":\"\"}\n".len();
    let mut encoded = 0;
    for byte in code.bytes() {
        encoded += match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
            0..=31 => 6,
            _ => 1,
        };
        if encoded > budget {
            return false;
        }
    }
    true
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame {
    Started { id: u64 },
    Stdout { id: u64, text: String },
    Stderr { id: u64, text: String },
    Error { id: Option<u64>, text: String },
    Done { id: u64, ok: bool },
}

pub(super) struct Kernel {
    child: Option<Child>,
    stdin: ChildStdin,
    wire: BufReader<ChildStdout>,
    diagnostics: Option<ChildStderr>,
    frame: Vec<u8>,
    next_id: u64,
}

impl Kernel {
    pub(super) async fn spawn(cwd: &Path) -> Result<Self> {
        let python = find_python(cwd).await?;
        let mut command = Command::new(python);
        command
            .args(["-I", "-u", "-c", RUNNER])
            .current_dir(cwd)
            .env_clear()
            .envs(
                ENV_KEYS
                    .iter()
                    .filter_map(|key| std::env::var_os(key).map(|value| (key, value))),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        horon_agent_platform::process::isolate_process_group(command.as_std_mut());
        horon_agent_platform::process::hide_console_window(command.as_std_mut());
        let mut child = command
            .spawn()
            .context("start Python eval (requires Python 3.10 or later)")?;
        let stdin = child.stdin.take().context("take Python eval stdin")?;
        let stdout = child.stdout.take().context("take Python eval control output")?;
        let diagnostics = child.stderr.take();
        Ok(Self {
            child: Some(child),
            stdin,
            wire: BufReader::new(stdout),
            diagnostics,
            frame: Vec::new(),
            next_id: 0,
        })
    }

    pub(super) async fn execute(&mut self, code: &str, output: &mut Capture) -> Result<bool> {
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        serde_json::to_writer(&mut self.frame, &Request { id, code })?;
        if self.frame.len() >= MAX_FRAME {
            bail!("Python eval cell exceeds the 256 KiB encoded request limit");
        }
        self.frame.push(b'\n');
        self.stdin
            .write_all(&self.frame)
            .await
            .context("send Python eval cell")?;
        self.frame.clear();

        let mut started = false;
        let mut native = [0; 4096];
        loop {
            let frame = tokio::select! {
                frame = next_frame(&mut self.wire, &mut self.frame) => frame,
                bytes = diagnostics_read(&mut self.diagnostics, &mut native) => {
                    output.push_native(&native[..bytes?]);
                    continue;
                }
            };
            let frame = match frame {
                Ok(frame) => frame,
                Err(error) => {
                    self.drain_diagnostics(output, &mut native).await;
                    return Err(error);
                }
            };
            match frame {
                Frame::Started { id: received } if !started && received == id => started = true,
                Frame::Stdout { id: received, text }
                | Frame::Stderr { id: received, text }
                | Frame::Error {
                    id: Some(received),
                    text,
                } if started && received == id => {
                    output.push(&text);
                }
                Frame::Error { id: None, text } if !started => bail!("Python eval runner rejected the request: {text}"),
                Frame::Done { id: received, ok } if started && received == id => {
                    self.drain_diagnostics(output, &mut native).await;
                    return Ok(ok);
                }
                _ => bail!("Python eval runner returned an unexpected frame"),
            }
        }
    }

    async fn drain_diagnostics(&mut self, output: &mut Capture, buffer: &mut [u8]) {
        let Some(stderr) = &mut self.diagnostics else { return };
        // Drain bytes already in the pipe without waiting on a background writer.
        for _ in 0..16 {
            let ready = std::future::poll_fn(|cx| {
                let mut read = ReadBuf::new(buffer);
                let received = match Pin::new(&mut *stderr).poll_read(cx, &mut read) {
                    Poll::Ready(Ok(())) => read.filled().len(),
                    _ => 0,
                };
                Poll::Ready(received)
            })
            .await;
            if ready == 0 {
                break;
            }
            output.push_native(&buffer[..ready]);
        }
    }

    pub(super) fn begin_stop(mut self) -> Option<tokio::task::JoinHandle<()>> {
        let child = self.child.take()?;
        // EOF could let the leader exit before Windows walks its children.
        let stop = move || {
            stop_child(child);
            drop(self);
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            Some(runtime.spawn_blocking(stop))
        } else {
            stop();
            None
        }
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        let Some(child) = self.child.take() else { return };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn_blocking(move || stop_child(child)));
        } else {
            stop_child(child);
        }
    }
}

fn stop_child(mut child: Child) {
    if let Some(pid) = child.id()
        && horon_agent_platform::process::force_stop_process_group(pid).is_err()
    {
        let _result = child.start_kill();
    }
    for _ in 0..40 {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

async fn find_python(cwd: &Path) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH is missing; install Python 3.10 or later for eval")?;
    let names = [
        format!("python3{}", std::env::consts::EXE_SUFFIX),
        format!("python{}", std::env::consts::EXE_SUFFIX),
    ];
    for dir in std::env::split_paths(&path).filter(|dir| dir.is_absolute()) {
        let Ok(canonical_dir) = tokio::fs::canonicalize(&dir).await else {
            continue;
        };
        if canonical_dir.starts_with(cwd) {
            continue;
        }
        for name in &names {
            let candidate = dir.join(name);
            if let Ok(target) = tokio::fs::canonicalize(&candidate).await
                && !target.starts_with(cwd)
                && horon_agent_platform::fs::is_executable(&target)
            {
                return Ok(candidate);
            }
        }
    }
    bail!("Python eval requires Python 3.10 or later on an absolute PATH outside the working directory")
}

async fn next_frame(reader: &mut BufReader<ChildStdout>, frame: &mut Vec<u8>) -> Result<Frame> {
    loop {
        let chunk = reader.fill_buf().await.context("read Python eval control output")?;
        if chunk.is_empty() {
            bail!("Python eval runner exited before completing the cell");
        }
        let end = chunk.iter().position(|&byte| byte == b'\n');
        let len = end.map_or(chunk.len(), |position| position + 1);
        if frame.len() + len > MAX_FRAME {
            bail!("Python eval runner returned a frame larger than 256 KiB");
        }
        frame.extend_from_slice(&chunk[..len]);
        reader.consume(len);
        if end.is_some() {
            let decoded = serde_json::from_slice(frame).context("invalid JSON frame from Python eval");
            frame.clear();
            return decoded;
        }
    }
}

async fn diagnostics_read(stderr: &mut Option<ChildStderr>, buffer: &mut [u8]) -> std::io::Result<usize> {
    if let Some(reader) = stderr {
        let bytes = reader.read(buffer).await?;
        if bytes != 0 {
            return Ok(bytes);
        }
        *stderr = None;
    }
    std::future::pending().await
}
