use anyhow::{Context, Result, bail};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::task::Poll;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, ReadBuf};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use super::bridge::{Bridge, MAX_CALLS, ToolRequest};
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

pub(super) enum Frame {
    Started {
        id: u64,
    },
    Stdout {
        id: u64,
        text: String,
    },
    Stderr {
        id: u64,
        text: String,
    },
    Error {
        id: Option<u64>,
        text: String,
    },
    Done {
        id: u64,
        ok: bool,
    },
    ToolCall {
        id: u64,
        seq: u32,
        name: String,
        args: serde_json::Value,
    },
}

#[derive(Clone, Copy)]
enum FrameKind {
    Started,
    Stdout,
    Stderr,
    Error,
    Done,
    ToolCall,
}

impl<'de> Deserialize<'de> for FrameKind {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct KindVisitor;
        impl Visitor<'_> for KindVisitor {
            type Value = FrameKind;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("eval frame type")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value {
                    "started" => Ok(FrameKind::Started),
                    "stdout" => Ok(FrameKind::Stdout),
                    "stderr" => Ok(FrameKind::Stderr),
                    "error" => Ok(FrameKind::Error),
                    "done" => Ok(FrameKind::Done),
                    "tool_call" => Ok(FrameKind::ToolCall),
                    other => Err(de::Error::unknown_variant(
                        other,
                        &["started", "stdout", "stderr", "error", "done", "tool_call"],
                    )),
                }
            }
        }
        deserializer.deserialize_str(KindVisitor)
    }
}

enum FrameField {
    Type,
    Id,
    Text,
    Ok,
    Seq,
    Name,
    Args,
}

const FRAME_FIELDS: &[&str] = &["type", "id", "text", "ok", "seq", "name", "args"];

impl<'de> Deserialize<'de> for FrameField {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldVisitor;
        impl Visitor<'_> for FieldVisitor {
            type Value = FrameField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("frame field name")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value {
                    "type" => Ok(FrameField::Type),
                    "id" => Ok(FrameField::Id),
                    "text" => Ok(FrameField::Text),
                    "ok" => Ok(FrameField::Ok),
                    "seq" => Ok(FrameField::Seq),
                    "name" => Ok(FrameField::Name),
                    "args" => Ok(FrameField::Args),
                    other => Err(de::Error::unknown_field(other, FRAME_FIELDS)),
                }
            }
        }
        deserializer.deserialize_identifier(FieldVisitor)
    }
}

impl<'de> Deserialize<'de> for Frame {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FrameVisitor;
        impl<'de> Visitor<'de> for FrameVisitor {
            type Value = Frame;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an eval control frame object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut kind: Option<FrameKind> = None;
                let mut id: Option<Option<u64>> = None;
                let mut text: Option<String> = None;
                let mut ok: Option<bool> = None;
                let mut seq: Option<u32> = None;
                let mut name: Option<String> = None;
                let mut args: Option<serde_json::Value> = None;

                while let Some(field) = map.next_key::<FrameField>()? {
                    match field {
                        FrameField::Type => {
                            if kind.is_some() {
                                return Err(de::Error::duplicate_field("type"));
                            }
                            kind = Some(map.next_value()?);
                        }
                        FrameField::Id => {
                            if id.is_some() {
                                return Err(de::Error::duplicate_field("id"));
                            }
                            id = Some(map.next_value()?);
                        }
                        FrameField::Text => {
                            if text.is_some() {
                                return Err(de::Error::duplicate_field("text"));
                            }
                            text = Some(map.next_value()?);
                        }
                        FrameField::Ok => {
                            if ok.is_some() {
                                return Err(de::Error::duplicate_field("ok"));
                            }
                            ok = Some(map.next_value()?);
                        }
                        FrameField::Seq => {
                            if seq.is_some() {
                                return Err(de::Error::duplicate_field("seq"));
                            }
                            seq = Some(map.next_value()?);
                        }
                        FrameField::Name => {
                            if name.is_some() {
                                return Err(de::Error::duplicate_field("name"));
                            }
                            name = Some(map.next_value()?);
                        }
                        FrameField::Args => {
                            if args.is_some() {
                                return Err(de::Error::duplicate_field("args"));
                            }
                            args = Some(map.next_value()?);
                        }
                    }
                }

                let kind = kind.ok_or_else(|| de::Error::missing_field("type"))?;
                let require_id = |id: Option<Option<u64>>| -> std::result::Result<u64, A::Error> {
                    match id {
                        Some(Some(value)) => Ok(value),
                        Some(None) => Err(de::Error::invalid_type(de::Unexpected::Unit, &"u64")),
                        None => Err(de::Error::missing_field("id")),
                    }
                };
                let forbid = |field: &'static str, expected: &'static [&'static str]| -> A::Error {
                    de::Error::unknown_field(field, expected)
                };

                match kind {
                    FrameKind::Started => {
                        const EXPECTED: &[&str] = &["type", "id"];
                        if text.is_some() {
                            return Err(forbid("text", EXPECTED));
                        }
                        if ok.is_some() {
                            return Err(forbid("ok", EXPECTED));
                        }
                        if seq.is_some() {
                            return Err(forbid("seq", EXPECTED));
                        }
                        if name.is_some() {
                            return Err(forbid("name", EXPECTED));
                        }
                        if args.is_some() {
                            return Err(forbid("args", EXPECTED));
                        }
                        Ok(Frame::Started { id: require_id(id)? })
                    }
                    FrameKind::Stdout | FrameKind::Stderr => {
                        const EXPECTED: &[&str] = &["type", "id", "text"];
                        if ok.is_some() {
                            return Err(forbid("ok", EXPECTED));
                        }
                        if seq.is_some() {
                            return Err(forbid("seq", EXPECTED));
                        }
                        if name.is_some() {
                            return Err(forbid("name", EXPECTED));
                        }
                        if args.is_some() {
                            return Err(forbid("args", EXPECTED));
                        }
                        let id = require_id(id)?;
                        let text = text.ok_or_else(|| de::Error::missing_field("text"))?;
                        Ok(if matches!(kind, FrameKind::Stdout) {
                            Frame::Stdout { id, text }
                        } else {
                            Frame::Stderr { id, text }
                        })
                    }
                    FrameKind::Error => {
                        const EXPECTED: &[&str] = &["type", "id", "text"];
                        if ok.is_some() {
                            return Err(forbid("ok", EXPECTED));
                        }
                        if seq.is_some() {
                            return Err(forbid("seq", EXPECTED));
                        }
                        if name.is_some() {
                            return Err(forbid("name", EXPECTED));
                        }
                        if args.is_some() {
                            return Err(forbid("args", EXPECTED));
                        }
                        let text = text.ok_or_else(|| de::Error::missing_field("text"))?;
                        Ok(Frame::Error { id: id.flatten(), text })
                    }
                    FrameKind::Done => {
                        const EXPECTED: &[&str] = &["type", "id", "ok"];
                        if text.is_some() {
                            return Err(forbid("text", EXPECTED));
                        }
                        if seq.is_some() {
                            return Err(forbid("seq", EXPECTED));
                        }
                        if name.is_some() {
                            return Err(forbid("name", EXPECTED));
                        }
                        if args.is_some() {
                            return Err(forbid("args", EXPECTED));
                        }
                        Ok(Frame::Done {
                            id: require_id(id)?,
                            ok: ok.ok_or_else(|| de::Error::missing_field("ok"))?,
                        })
                    }
                    FrameKind::ToolCall => {
                        const EXPECTED: &[&str] = &["type", "id", "seq", "name", "args"];
                        if text.is_some() {
                            return Err(forbid("text", EXPECTED));
                        }
                        if ok.is_some() {
                            return Err(forbid("ok", EXPECTED));
                        }
                        Ok(Frame::ToolCall {
                            id: require_id(id)?,
                            seq: seq.ok_or_else(|| de::Error::missing_field("seq"))?,
                            name: name.ok_or_else(|| de::Error::missing_field("name"))?,
                            args: args.ok_or_else(|| de::Error::missing_field("args"))?,
                        })
                    }
                }
            }
        }
        deserializer.deserialize_map(FrameVisitor)
    }
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

    pub(super) async fn execute(&mut self, code: &str, bridge: &Bridge<'_>, output: &mut Capture) -> Result<bool> {
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
        let mut calls = 0;
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
                Frame::Error { id: None, text } => bail!("Python eval runner stopped: {text}"),
                Frame::ToolCall {
                    id: received,
                    seq,
                    name,
                    args,
                } if started && received == id => {
                    if seq != calls + 1 || seq > MAX_CALLS {
                        bail!("Python eval returned an invalid tool call sequence");
                    }
                    calls = seq;
                    let reply = bridge.dispatch(id, seq, ToolRequest { name, args }).await;
                    serde_json::to_writer(&mut self.frame, &reply)?;
                    if self.frame.len() >= MAX_FRAME {
                        bail!("Python tool response exceeds the 256 KiB frame limit");
                    }
                    self.frame.push(b'\n');
                    self.stdin
                        .write_all(&self.frame)
                        .await
                        .context("send Python tool response")?;
                    self.frame.clear();
                }
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

pub(in crate::tool) fn stop_child(mut child: Child) {
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
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || find_python_sync(&cwd, &path))
        .await
        .context("search PATH for Python interpreter")?
}

fn find_python_sync(cwd: &Path, path: &std::ffi::OsStr) -> Result<PathBuf> {
    let suffix = std::env::consts::EXE_SUFFIX;
    let (py3, py): (Cow<'static, str>, Cow<'static, str>) = if suffix.is_empty() {
        (Cow::Borrowed("python3"), Cow::Borrowed("python"))
    } else {
        (
            Cow::Owned(format!("python3{suffix}")),
            Cow::Owned(format!("python{suffix}")),
        )
    };
    let names = [py3.as_ref(), py.as_ref()];
    for dir in std::env::split_paths(path).filter(|dir| dir.is_absolute()) {
        if !std::fs::metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        let Ok(canonical_dir) = std::fs::canonicalize(&dir) else {
            continue;
        };
        if canonical_dir.starts_with(cwd) {
            continue;
        }
        for name in names {
            let candidate = dir.join(name);
            if std::fs::symlink_metadata(&candidate).is_err() {
                continue;
            }
            if let Ok(target) = std::fs::canonicalize(&candidate)
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
