use super::{StdinInputRequest, Tool, ToolContext, ToolEffects, ToolOutput};
use super::{bash_interceptor, git_guard};
use crate::background::TaskResult;
use crate::bus::{BackgroundTaskProgress, BackgroundTaskProgressKind, BackgroundTaskProgressSource};
use crate::util::truncate_str;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

mod stream;

pub(crate) use stream::BackgroundTail;

/// Context budget for one command's output; the rest is saved as an artifact.
const MAX_OUTPUT_BYTES: usize = 30_000;
const DEFAULT_TIMEOUT_MS: u64 = 120000;
const STDIN_POLL_INTERVAL_MS: u64 = 500;
const STALE_CWD_CAPTURE_SECONDS: u64 = 300;
const STDIN_INITIAL_DELAY_MS: u64 = 300;
/// First and longest wait between exit checks of a detached foreground command.
const DETACHED_POLL_MIN: Duration = Duration::from_millis(2);
const DETACHED_POLL_MAX: Duration = Duration::from_millis(100);
/// Longest accepted `name` for a named background service.
const SERVICE_NAME_MAX_LEN: usize = 48;
/// How much of a service's output file a readiness `log` check inspects.
const READINESS_OUTPUT_TAIL_BYTES: usize = 64 * 1024;
const READINESS_POLL_INTERVAL_MS: u64 = 100;
const READINESS_DEFAULT_TIMEOUT_SECS: f64 = 30.0;
const READINESS_CONNECT_TIMEOUT_MS: u64 = 250;
/// Readiness window bounds, in seconds: a wait shorter than this cannot see a
/// process start, and one longer than this ties up a tool call for an hour.
const READINESS_MIN_TIMEOUT_SECS: f64 = 0.05;
const READINESS_MAX_TIMEOUT_SECS: f64 = 3600.0;
const PROGRESS_MARKER_PREFIX: &str = "HORON_AGENT_PROGRESS ";
const CHECKPOINT_MARKER_PREFIX: &str = "HORON_AGENT_CHECKPOINT ";
const BACKGROUND_PROGRESS_GUIDANCE: &str = "For long-running background commands, prefer scripts or commands that periodically print progress updates. Best format: print lines starting with `HORON_AGENT_PROGRESS ` followed by JSON like {\"percent\":42,\"message\":\"Running\"} or {\"current\":120,\"total\":1000,\"unit\":\"batches\",\"message\":\"Epoch 2/5\",\"eta_seconds\":30}. Supported JSON fields are `percent`, `message`, `current`, `total`, `unit`, `eta_seconds`, and optional `kind`=`indeterminate` or `kind`=`checkpoint`. For milestone-style wakeups, print `HORON_AGENT_CHECKPOINT {\"message\":\"Unit tests passed\"}`. Generic fallback output that can be parsed includes `42%`, `3/10 tests`, `3 of 10 steps`, `1.5/3.0 GiB`, or phase lines like `Compiling ...`, `Downloading ...`, `Running ...`, and `Building ...`. If you are writing the script yourself, add these progress/checkpoint lines explicitly. Put large temporary files, worktrees, and virtual environments under `$HORON_AGENT_SCRATCH_DIR`, not `/tmp`, because `/tmp` may be RAM-backed.";
const BASH_TOOL_DESCRIPTION: &str = "Run a bash command; prefer agentgrep or rg over grep -r; --hidden --no-ignore.";
const WINDOWS_SHELL_TOOL_DESCRIPTION: &str =
    "Run a Windows cmd.exe command (compatibility name `bash`). Use cmd.exe syntax, not Bash.";

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Route ordinary `cargo` invocations (including those inside child scripts)
/// through the repository wrapper. Besides applying the project's build policy,
/// that wrapper appends real action timings to rust-actions.jsonl.
fn wrap_repo_cargo_commands(command: &str, working_dir: Option<&Path>) -> Option<String> {
    let working_dir = working_dir?;
    let repo = crate::build::find_repo_in_ancestors(working_dir)?;
    let wrapper = repo.join("scripts").join("dev").join("cargo.sh");
    if !wrapper.is_file() {
        return None;
    }

    Some(format!(
        r#"export HORON_AGENT_DEV_CARGO_SCRIPT={wrapper}
cargo() {{
  if [[ "${{HORON_AGENT_IN_DEV_CARGO:-0}}" == "1" ]]; then
    command cargo "$@"
  else
    HORON_AGENT_IN_DEV_CARGO=1 "$HORON_AGENT_DEV_CARGO_SCRIPT" "$@"
  fi
}}
export -f cargo
{command}"#,
        wrapper = shell_single_quote(&wrapper.to_string_lossy()),
    ))
}

/// Build a clear timeout message. The `timeout` param is in milliseconds, which
/// agents frequently mistake for seconds (e.g. passing 1000 thinking it means
/// 1000s when it is 1s). Spell out the seconds equivalent and, for suspiciously
/// short timeouts, hint that the unit is milliseconds so the next attempt uses a
/// sane value instead of repeating the same mistake.
fn timeout_message(timeout_ms: u64) -> String {
    let secs = timeout_ms as f64 / 1000.0;
    let mut msg = format!("Command timed out after {}ms ({:.1}s)", timeout_ms, secs);
    if timeout_ms <= 5000 {
        msg.push_str(
            ". Note: the `timeout` parameter is in MILLISECONDS, not seconds. \
             If you meant a longer limit, pass a larger value (e.g. 600000 = 10min) or omit `timeout`.",
        );
    }
    msg
}

/// The longest deadline a caller may request. Anything above it is clamped so
/// a typo in the unit cannot leave a command waiting for an hour.
const MAX_TIMEOUT_MS: u64 = 600000;

/// A resolved `timeout` argument.
struct BashTimeout {
    /// `None` means no deadline at all.
    duration: Option<Duration>,
    /// The requested value when it exceeded [`MAX_TIMEOUT_MS`], so the caller
    /// can tell the model its value was clamped.
    clamped_from: Option<u64>,
}

/// Resolve a foreground `timeout` argument (milliseconds): omitted means the
/// default, `0` disables the deadline, and values above the cap clamp.
fn resolve_bash_timeout(requested: Option<u64>) -> BashTimeout {
    match requested {
        None => BashTimeout {
            duration: Some(Duration::from_millis(DEFAULT_TIMEOUT_MS)),
            clamped_from: None,
        },
        Some(0) => BashTimeout {
            duration: None,
            clamped_from: None,
        },
        Some(ms) if ms > MAX_TIMEOUT_MS => BashTimeout {
            duration: Some(Duration::from_millis(MAX_TIMEOUT_MS)),
            clamped_from: Some(ms),
        },
        Some(ms) => BashTimeout {
            duration: Some(Duration::from_millis(ms)),
            clamped_from: None,
        },
    }
}

/// Resolve a background `timeout` argument. A background task already keeps
/// running on its own, so an omitted value stays "no deadline" as it always
/// had; `0` is an explicit way to say the same thing.
fn resolve_background_timeout(requested: Option<u64>) -> BashTimeout {
    match requested {
        None => BashTimeout {
            duration: None,
            clamped_from: None,
        },
        Some(ms) => resolve_bash_timeout(Some(ms)),
    }
}

/// The sentence appended to output when the requested timeout was clamped.
fn timeout_clamp_note(clamped_from: Option<u64>) -> String {
    match clamped_from {
        Some(requested) => format!(
            "\n\n[Timeout clamped to {MAX_TIMEOUT_MS}ms (requested {requested}ms; allowed range 1-{MAX_TIMEOUT_MS}ms).]"
        ),
        None => String::new(),
    }
}

fn progress_ratio_regex() -> Result<&'static regex::Regex> {
    static REGEX: LazyLock<Result<regex::Regex, regex::Error>> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(?P<current>\d{1,6})\s*/\s*(?P<total>\d{1,6})\b(?:\s*(?P<unit>tests?|steps?|files?|items?|cases?|tasks?|targets?|chunks?|batches?|examples?|crates?|modules?|packages?|workers?))?",
        )
    });
    REGEX
        .as_ref()
        .map_err(|err| anyhow::anyhow!("invalid progress ratio regex: {err}"))
}

fn progress_of_regex() -> Result<&'static regex::Regex> {
    static REGEX: LazyLock<Result<regex::Regex, regex::Error>> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(?P<current>\d{1,6})\s+of\s+(?P<total>\d{1,6})\b(?:\s+(?P<unit>tests?|steps?|files?|items?|cases?|tasks?|targets?|chunks?|batches?|examples?|crates?|modules?|packages?|workers?))?",
        )
    });
    REGEX
        .as_ref()
        .map_err(|err| anyhow::anyhow!("invalid progress-of regex: {err}"))
}

fn progress_byte_ratio_regex() -> Result<&'static regex::Regex> {
    static REGEX: LazyLock<Result<regex::Regex, regex::Error>> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(?P<current>\d+(?:\.\d+)?)\s*/\s*(?P<total>\d+(?:\.\d+)?)\s*(?P<unit>bytes?|[kmgt]i?b)\b",
        )
    });
    REGEX
        .as_ref()
        .map_err(|err| anyhow::anyhow!("invalid progress byte-ratio regex: {err}"))
}

fn progress_percent_regex() -> Result<&'static regex::Regex> {
    static REGEX: LazyLock<Result<regex::Regex, regex::Error>> =
        LazyLock::new(|| regex::Regex::new(r"(?i)\b(?P<percent>100|[1-9]?\d)\s*%"));
    REGEX
        .as_ref()
        .map_err(|err| anyhow::anyhow!("invalid progress percent regex: {err}"))
}

#[derive(Deserialize)]
struct ProgressMarker {
    #[serde(default)]
    percent: Option<f32>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    current: Option<u64>,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    eta_seconds: Option<u64>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    checkpoint: Option<bool>,
}

fn task_id_from_output_path(path: &Path) -> Option<&str> {
    path.file_stem()?.to_str()
}

fn parse_progress_kind(kind: Option<&str>) -> BackgroundTaskProgressKind {
    match kind {
        Some("indeterminate") => BackgroundTaskProgressKind::Indeterminate,
        _ => BackgroundTaskProgressKind::Determinate,
    }
}

fn summarize_background_command(description: Option<&str>, command: &str) -> String {
    if let Some(description) = description.map(str::trim).filter(|description| !description.is_empty()) {
        return truncate_str(description, 28).to_string();
    }

    let trimmed = command.trim();
    if trimmed.is_empty() {
        return "bash".to_string();
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let start = tokens.iter().position(|token| !token.contains('=')).unwrap_or(0);
    let tokens = &tokens[start..];
    if tokens.is_empty() {
        return truncate_str(trimmed, 28).to_string();
    }

    let label = match tokens {
        ["python" | "python3" | "bash" | "sh" | "node", script, ..] => std::path::Path::new(script)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(script)
            .to_string(),
        ["cargo", subcommand, ..] => format!("cargo {}", subcommand),
        ["npm" | "pnpm" | "yarn", command, script, ..] if *command == "run" => {
            format!("{} {} {}", tokens[0], command, script)
        }
        [first, second, ..] => format!("{} {}", first, second),
        [first] => first.to_string(),
        [] => "bash".to_string(),
    };

    truncate_str(&label, 28).to_string()
}

fn parse_progress_marker_with_checkpoint(line: &str) -> Option<(BackgroundTaskProgress, bool)> {
    let payload = line.trim().strip_prefix(PROGRESS_MARKER_PREFIX)?.trim();
    let marker: ProgressMarker = serde_json::from_str(payload).ok()?;
    let is_checkpoint = marker.checkpoint.unwrap_or(false) || matches!(marker.kind.as_deref(), Some("checkpoint"));
    let kind = if marker.percent.is_some() || matches!((marker.current, marker.total), (_, Some(total)) if total > 0) {
        BackgroundTaskProgressKind::Determinate
    } else {
        parse_progress_kind(marker.kind.as_deref())
    };

    Some((
        BackgroundTaskProgress {
            kind,
            percent: marker.percent,
            message: marker.message,
            current: marker.current,
            total: marker.total,
            unit: marker.unit,
            eta_seconds: marker.eta_seconds,
            updated_at: Utc::now().to_rfc3339(),
            source: BackgroundTaskProgressSource::Reported,
        }
        .normalize(),
        is_checkpoint,
    ))
}

#[cfg(all(test, not(windows)))]
fn parse_progress_marker(line: &str) -> Option<BackgroundTaskProgress> {
    parse_progress_marker_with_checkpoint(line).map(|(progress, _)| progress)
}

fn parse_checkpoint_marker(line: &str) -> Option<BackgroundTaskProgress> {
    let payload = line.trim().strip_prefix(CHECKPOINT_MARKER_PREFIX)?.trim();
    let marker: ProgressMarker = serde_json::from_str(payload).unwrap_or_else(|_| ProgressMarker {
        percent: None,
        message: Some(payload.to_string()),
        current: None,
        total: None,
        unit: None,
        eta_seconds: None,
        kind: Some("checkpoint".to_string()),
        checkpoint: Some(true),
    });

    Some(
        BackgroundTaskProgress {
            kind: BackgroundTaskProgressKind::Indeterminate,
            percent: marker.percent,
            message: marker.message,
            current: marker.current,
            total: marker.total,
            unit: marker.unit,
            eta_seconds: marker.eta_seconds,
            updated_at: Utc::now().to_rfc3339(),
            source: BackgroundTaskProgressSource::Reported,
        }
        .normalize(),
    )
}

fn progress_message_from_line(line: &str, matched_fragment: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(matched_fragment.trim()) {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn progress_from_counts(
    trimmed: &str,
    matched: &str,
    current: u64,
    total: u64,
    unit: Option<String>,
) -> Option<BackgroundTaskProgress> {
    if total < 2 || current > total {
        return None;
    }

    Some(
        BackgroundTaskProgress {
            kind: BackgroundTaskProgressKind::Determinate,
            percent: None,
            message: progress_message_from_line(trimmed, matched),
            current: Some(current),
            total: Some(total),
            unit,
            eta_seconds: None,
            updated_at: Utc::now().to_rfc3339(),
            source: BackgroundTaskProgressSource::ParsedOutput,
        }
        .normalize(),
    )
}

pub(super) fn parse_heuristic_progress(line: &str) -> Result<Option<BackgroundTaskProgress>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Some(captures) = progress_byte_ratio_regex()?.captures(trimmed) {
        let current = captures.name("current").and_then(|m| m.as_str().parse::<f64>().ok());
        let total = captures.name("total").and_then(|m| m.as_str().parse::<f64>().ok());
        if let (Some(current), Some(total), Some(matched)) = (current, total, captures.get(0))
            && total > 0.0
            && current <= total
        {
            return Ok(Some(
                BackgroundTaskProgress {
                    kind: BackgroundTaskProgressKind::Determinate,
                    percent: Some(((current / total) * 100.0) as f32),
                    message: progress_message_from_line(trimmed, matched.as_str()),
                    current: None,
                    total: None,
                    unit: captures.name("unit").map(|unit| unit.as_str().to_ascii_lowercase()),
                    eta_seconds: None,
                    updated_at: Utc::now().to_rfc3339(),
                    source: BackgroundTaskProgressSource::ParsedOutput,
                }
                .normalize(),
            ));
        }
    }

    if let Some(captures) = progress_ratio_regex()?.captures(trimmed) {
        let current = captures.name("current").and_then(|m| m.as_str().parse::<u64>().ok());
        let total = captures.name("total").and_then(|m| m.as_str().parse::<u64>().ok());
        if let (Some(current), Some(total), Some(matched)) = (current, total, captures.get(0)) {
            return Ok(progress_from_counts(
                trimmed,
                matched.as_str(),
                current,
                total,
                captures.name("unit").map(|unit| unit.as_str().to_ascii_lowercase()),
            ));
        }
    }

    if let Some(captures) = progress_of_regex()?.captures(trimmed) {
        let current = captures.name("current").and_then(|m| m.as_str().parse::<u64>().ok());
        let total = captures.name("total").and_then(|m| m.as_str().parse::<u64>().ok());
        if let (Some(current), Some(total), Some(matched)) = (current, total, captures.get(0)) {
            return Ok(progress_from_counts(
                trimmed,
                matched.as_str(),
                current,
                total,
                captures.name("unit").map(|unit| unit.as_str().to_ascii_lowercase()),
            ));
        }
    }

    if let Some(captures) = progress_percent_regex()?.captures(trimmed)
        && let (Some(percent), Some(matched)) = (
            captures.name("percent").and_then(|m| m.as_str().parse::<f32>().ok()),
            captures.get(0),
        )
    {
        return Ok(Some(
            BackgroundTaskProgress {
                kind: BackgroundTaskProgressKind::Determinate,
                percent: Some(percent),
                message: progress_message_from_line(trimmed, matched.as_str()),
                current: None,
                total: None,
                unit: None,
                eta_seconds: None,
                updated_at: Utc::now().to_rfc3339(),
                source: BackgroundTaskProgressSource::ParsedOutput,
            }
            .normalize(),
        ));
    }

    const PHASE_PREFIXES: &[&str] = &[
        "Compiling ",
        "Downloading ",
        "Running ",
        "Building ",
        "Linking ",
        "Resolving ",
        "Fetching ",
        "Installing ",
    ];
    if PHASE_PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix)) {
        return Ok(Some(
            BackgroundTaskProgress {
                kind: BackgroundTaskProgressKind::Indeterminate,
                percent: None,
                message: Some(trimmed.to_string()),
                current: None,
                total: None,
                unit: None,
                eta_seconds: None,
                updated_at: Utc::now().to_rfc3339(),
                source: BackgroundTaskProgressSource::ParsedOutput,
            }
            .normalize(),
        ));
    }

    Ok(None)
}

async fn handle_background_output_line(output_path: &Path, file: &mut tokio::fs::File, raw_line: &str, stderr: bool) {
    match parse_progress_line(raw_line) {
        Ok(Some(update)) => {
            if let Some(task_id) = task_id_from_output_path(output_path) {
                apply_progress_update(task_id, update).await;
            }
            return;
        }
        Ok(None) => {}
        Err(err) => {
            let warning = format!("[horon-agent warning] failed to parse background progress: {err}\n");
            file.write_all(warning.as_bytes()).await.ok();
            file.flush().await.ok();
        }
    }

    let rendered = if stderr {
        format!("[stderr] {}\n", raw_line)
    } else {
        format!("{}\n", raw_line)
    };
    file.write_all(rendered.as_bytes()).await.ok();
    file.flush().await.ok();
}

/// A progress or checkpoint update parsed from one line of command output.
#[derive(Debug)]
pub(super) enum ProgressLineUpdate {
    Progress(BackgroundTaskProgress),
    Checkpoint(BackgroundTaskProgress),
}

/// Parse one output line for any supported progress signal: explicit
/// `HORON_AGENT_CHECKPOINT`/`HORON_AGENT_PROGRESS` markers first, then heuristic patterns
/// (ratios, percentages, byte ratios, phase prefixes).
pub(super) fn parse_progress_line(line: &str) -> Result<Option<ProgressLineUpdate>> {
    if let Some(progress) = parse_checkpoint_marker(line) {
        return Ok(Some(ProgressLineUpdate::Checkpoint(progress)));
    }

    if let Some((progress, is_checkpoint)) = parse_progress_marker_with_checkpoint(line) {
        return Ok(Some(if is_checkpoint {
            ProgressLineUpdate::Checkpoint(progress)
        } else {
            ProgressLineUpdate::Progress(progress)
        }));
    }

    Ok(parse_heuristic_progress(line)?.map(ProgressLineUpdate::Progress))
}

async fn apply_progress_update(task_id: &str, update: ProgressLineUpdate) {
    let manager = crate::background::global();
    let _ = match update {
        ProgressLineUpdate::Progress(progress) => manager.update_progress(task_id, progress).await,
        ProgressLineUpdate::Checkpoint(progress) => manager.update_checkpoint(task_id, progress).await,
    };
}

/// Progress state for a foreground command that may be promoted to a
/// background task if it exceeds the foreground timeout.
///
/// Before promotion there is no task to attach progress to, so only the most
/// recent update is kept. When the command is promoted, `attach_task` flushes
/// that pending update so the task row starts at the real percentage instead
/// of 0%, and later updates stream directly to the background manager.
#[derive(Default)]
struct PromotedCommandProgress {
    task_id: std::sync::OnceLock<String>,
    pending: std::sync::Mutex<Option<ProgressLineUpdate>>,
}

impl PromotedCommandProgress {
    async fn record(&self, update: ProgressLineUpdate) {
        let direct = {
            let mut pending = self.pending.lock().expect("progress mutex poisoned");
            if self.task_id.get().is_none() {
                *pending = Some(update);
                None
            } else {
                Some(update)
            }
        };
        if let Some(update) = direct
            && let Some(task_id) = self.task_id.get()
        {
            apply_progress_update(task_id, update).await;
        }
    }

    async fn attach_task(&self, task_id: &str) {
        let _ = self.task_id.set(task_id.to_string());
        let pending = self.pending.lock().expect("progress mutex poisoned").take();
        if let Some(update) = pending {
            apply_progress_update(task_id, update).await;
        }
    }
}

/// Tail a detached background task's output file and translate progress lines
/// into background-manager progress updates.
///
/// Detached commands write directly to their output file, so nothing in-process
/// sees their output as it streams. This follower polls the file while the task
/// is `Running`, parsing complete lines from where it left off. It performs one
/// final drain after the task leaves `Running` and then exits.
fn spawn_detached_progress_follower(task_id: String, output_file: std::path::PathBuf) {
    tokio::spawn(async move {
        let manager = crate::background::global();
        let mut pos: u64 = 0;
        let mut partial: Vec<u8> = Vec::new();
        loop {
            let running = manager
                .status(&task_id)
                .await
                .map(|status| status.status == crate::bus::BackgroundTaskStatus::Running)
                .unwrap_or(false);

            if let Ok(mut file) = tokio::fs::File::open(&output_file).await {
                use tokio::io::AsyncSeekExt;
                if file.seek(std::io::SeekFrom::Start(pos)).await.is_ok() {
                    let mut chunk = Vec::new();
                    if file.read_to_end(&mut chunk).await.is_ok() && !chunk.is_empty() {
                        pos += chunk.len() as u64;
                        partial.extend_from_slice(&chunk);
                        while let Some(newline) = partial.iter().position(|byte| *byte == b'\n') {
                            let line: Vec<u8> = partial.drain(..=newline).collect();
                            let line = String::from_utf8_lossy(&line);
                            if let Ok(Some(update)) = parse_progress_line(line.trim_end()) {
                                apply_progress_update(&task_id, update).await;
                            }
                        }
                    }
                }
            }

            if !running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

/// The scratch directory a child shell receives, for the approval gate to
/// assess the same path.
///
/// A platform whose shells are not given a scratch directory reports `None`, so
/// the gate assesses exactly what the child sees.
pub(crate) fn tool_scratch_dir_for_risk() -> Option<std::path::PathBuf> {
    if !horon_agent_platform::term::passes_scratch_dir_to_shell() {
        return None;
    }
    tool_scratch_dir()
}

fn tool_scratch_dir() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("HORON_AGENT_SCRATCH_DIR")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| crate::storage::horon_agent_dir().ok().map(|dir| dir.join("scratch")))?;
    crate::storage::ensure_dir(&dir).ok()?;
    Some(dir)
}

/// Pager variables that become `cat`, so no command stops at a pager prompt.
const PAGERS: [&str; 11] = [
    "PAGER",
    "GIT_PAGER",
    "MANPAGER",
    "SYSTEMD_PAGER",
    "BAT_PAGER",
    "DELTA_PAGER",
    "GH_PAGER",
    "GLAB_PAGER",
    "PSQL_PAGER",
    "MYSQL_PAGER",
    "HOMEBREW_PAGER",
];

/// Editor variables that become `true`, so an editor invocation returns at once.
const EDITORS: [&str; 4] = ["GIT_EDITOR", "VISUAL", "EDITOR", "GIT_SEQUENCE_EDITOR"];

/// Package-manager and CLI switches that auto-confirm or silence prompts.
const PROMPT_FREE_TOOLS: [(&str, &str); 18] = [
    ("npm_config_yes", "true"),
    ("npm_config_update_notifier", "false"),
    ("npm_config_fund", "false"),
    ("npm_config_audit", "false"),
    ("npm_config_progress", "false"),
    ("PNPM_DISABLE_SELF_UPDATE_CHECK", "true"),
    ("PNPM_UPDATE_NOTIFIER", "false"),
    ("YARN_ENABLE_TELEMETRY", "0"),
    ("YARN_ENABLE_PROGRESS_BARS", "0"),
    ("CARGO_TERM_PROGRESS_WHEN", "never"),
    ("DEBIAN_FRONTEND", "noninteractive"),
    ("PIP_NO_INPUT", "1"),
    ("PIP_DISABLE_PIP_VERSION_CHECK", "1"),
    ("TF_INPUT", "0"),
    ("TF_IN_AUTOMATION", "1"),
    ("GH_PROMPT_DISABLED", "1"),
    ("COMPOSER_NO_INTERACTION", "1"),
    ("CLOUDSDK_CORE_DISABLE_PROMPTS", "1"),
];

/// Console code-page and locale fixes where the platform shell needs them.
///
/// Resolved once per process: on Windows the locale probe walks the whole
/// process environment, and nothing in this process sets `LANG` or `LC_*`
/// after start, so the answer cannot change between tool calls.
static PLATFORM_SHELL_ENV: LazyLock<Vec<(&'static str, String)>> =
    LazyLock::new(horon_agent_platform::term::shell_env_overrides);

/// Variables every child command sees so nothing can stop and wait for a
/// terminal answer that will never come: pagers and editors become no-ops,
/// Git and SSH cannot prompt, and package managers auto-confirm.
///
/// Order matters only in that later entries win; a caller that sets
/// `HORON_AGENT_SESSION_ID`/`HORON_AGENT_CWD_FILE` after these still keeps the
/// last word. Every value except `SSH_ASKPASS` is borrowed from a table that
/// lives for the process, so a tool call no longer rebuilds it.
fn non_interactive_env(pty: bool) -> Vec<(&'static str, Cow<'static, str>)> {
    let platform: &'static [(&'static str, String)] = &PLATFORM_SHELL_ENV;
    let mut env = Vec::with_capacity(PAGERS.len() + EDITORS.len() + PROMPT_FREE_TOOLS.len() + platform.len() + 11);
    env.extend(PAGERS.iter().map(|&name| (name, Cow::Borrowed("cat"))));
    env.push(("AWS_PAGER", Cow::Borrowed("")));
    env.push(("LESS", Cow::Borrowed("FRX")));
    env.push(("TERM", Cow::Borrowed(if pty { "xterm-256color" } else { "dumb" })));
    if !pty {
        env.push(("NO_COLOR", Cow::Borrowed("1")));
    }
    env.push(("PYTHONUNBUFFERED", Cow::Borrowed("1")));
    env.extend(EDITORS.iter().map(|&name| (name, Cow::Borrowed("true"))));
    env.push(("GIT_TERMINAL_PROMPT", Cow::Borrowed("0")));
    env.push(("GCM_INTERACTIVE", Cow::Borrowed("never")));
    env.push(("SSH_ASKPASS", Cow::Owned(absolute_false_command().to_string())));
    if !pty && !ci_suppressed() {
        env.push(("CI", Cow::Borrowed("true")));
    }
    env.push(("AGENT", Cow::Borrowed("1")));
    env.extend(
        PROMPT_FREE_TOOLS
            .iter()
            .map(|&(name, value)| (name, Cow::Borrowed(value))),
    );
    env.extend(
        platform
            .iter()
            .map(|(name, value)| (*name, Cow::Borrowed(value.as_str()))),
    );
    env
}

/// The last `PATH` value `SSH_ASKPASS` was resolved against, with its result.
type FalseCommandCache = Mutex<Option<(Option<OsString>, Arc<str>)>>;

static FALSE_COMMAND: FalseCommandCache = Mutex::new(None);

/// The absolute path of the `false` program, so `SSH_ASKPASS` fails closed
/// instead of falling back to a GUI prompt.
///
/// The `PATH` walk runs once per distinct `PATH` value rather than once per
/// tool call; on Windows nothing provides `false`, so every call used to probe
/// every `PATH` directory.
fn absolute_false_command() -> Arc<str> {
    cached_false_command(&FALSE_COMMAND, std::env::var_os("PATH"))
}

fn cached_false_command(cache: &FalseCommandCache, path: Option<OsString>) -> Arc<str> {
    let mut cached = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((seen, value)) = cached.as_ref()
        && *seen == path
    {
        return Arc::clone(value);
    }
    let value: Arc<str> = resolve_false_command(path.as_deref()).into();
    *cached = Some((path, Arc::clone(&value)));
    value
}

/// Walk `path` for a `false` program. A bare `false` is the documented
/// fallback when nothing on `PATH` provides it.
fn resolve_false_command(path: Option<&OsStr>) -> String {
    let Some(path) = path else {
        return "false".to_string();
    };
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join("false");
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "false".to_string()
}

/// `HORON_AGENT_BASH_NO_CI=1` lets a caller keep a `CI` value of its own (for a
/// command that behaves differently under CI) while the rest of the
/// non-interactive environment stays on.
fn ci_suppressed() -> bool {
    std::env::var("HORON_AGENT_BASH_NO_CI")
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

fn non_interactive_env_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = *BASH_NON_INTERACTIVE_ENV_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
    {
        return forced;
    }
    crate::config::config().tools.bash_non_interactive_env
}

/// Force the switch for one test. The switch is process-global config, so a
/// test that depends on it must not read whatever config the machine has.
#[cfg(test)]
static BASH_NON_INTERACTIVE_ENV_TEST_OVERRIDE: Mutex<Option<bool>> = Mutex::new(None);

#[cfg(all(test, not(windows)))]
fn set_bash_non_interactive_env_test_override(value: Option<bool>) {
    *BASH_NON_INTERACTIVE_ENV_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
}

/// Whether bash output is passed through the opt-in output minimizer. The
/// switch is off by default because it rewrites what the model reads.
fn bash_minimizer_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = *BASH_MINIMIZER_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
    {
        return forced;
    }
    crate::config::config().tools.bash_minimizer
}

#[cfg(test)]
static BASH_MINIMIZER_TEST_OVERRIDE: Mutex<Option<bool>> = Mutex::new(None);

#[cfg(all(test, not(windows)))]
fn set_bash_minimizer_test_override(value: Option<bool>) {
    *BASH_MINIMIZER_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
}

/// Build the shell command for `cmd_str` and apply the non-interactive
/// environment to it.
///
/// The shell itself (`bash -c`, `cmd.exe /D /S /C`, and the quoting each needs)
/// comes from the platform adapter; the scratch directory is passed through the
/// same call so a child's temporary files land there.
fn build_shell_command(cmd_str: &str, pty: bool) -> TokioCommand {
    // Only a POSIX shell reads the temporary-directory variables, so the
    // scratch directory is created and passed only where the platform uses it.
    let scratch = if horon_agent_platform::term::passes_scratch_dir_to_shell() {
        tool_scratch_dir()
    } else {
        None
    };
    let mut cmd = TokioCommand::from(horon_agent_platform::process::shell_command(
        cmd_str,
        scratch.as_deref(),
    ));
    // Callers add their own variables after this, so those keep the last word.
    if non_interactive_env_enabled() {
        for (name, value) in non_interactive_env(pty) {
            cmd.env(name, &*value);
        }
    }
    cmd
}

fn configure_background_command_stdio(command: &mut TokioCommand) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

/// Build the detached wrapper that runs `command` outside this session.
///
/// `None` means this platform has no local shell that can record the command's
/// final working directory, so the caller must not take the detached path.
fn build_detached_shell_wrapper(command: &str, cwd_capture: Option<&Path>) -> Option<StdCommand> {
    let mut cmd = horon_agent_platform::term::detached_shell_command(command, cwd_capture)?;
    if non_interactive_env_enabled() {
        for (name, value) in non_interactive_env(false) {
            cmd.env(name, &*value);
        }
    }
    if let Some(dir) = tool_scratch_dir() {
        cmd.env("TMPDIR", &dir).env("HORON_AGENT_SCRATCH_DIR", dir);
    }
    Some(cmd)
}

static BASH_SESSION_CWDS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();

fn bash_session_cwds() -> &'static Mutex<HashMap<String, PathBuf>> {
    BASH_SESSION_CWDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember_bash_session_cwd(session_id: &str, dir: PathBuf) {
    let mut cwds = bash_session_cwds()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cwds.insert(session_id.to_string(), dir);
}

fn bash_session_cwd(session_id: &str) -> Option<PathBuf> {
    bash_session_cwds()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .cloned()
}

fn bash_session_cwd_enabled() -> bool {
    // The state-capture suffix is POSIX shell syntax, so the memory stays off
    // where the platform's shell is not POSIX.
    if !horon_agent_platform::term::shell_is_posix() {
        return false;
    }
    #[cfg(test)]
    if let Some(forced) = *BASH_SESSION_CWD_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
    {
        return forced;
    }
    crate::config::config().tools.bash_session_cwd
}

/// The switch is process-global config, so tests that need it off force it here
/// instead of rewriting the config file every other test in this binary reads.
#[cfg(test)]
static BASH_SESSION_CWD_TEST_OVERRIDE: Mutex<Option<bool>> = Mutex::new(None);

#[cfg(all(test, not(windows)))]
fn set_bash_session_cwd_test_override(value: Option<bool>) {
    *BASH_SESSION_CWD_TEST_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
}

#[cfg(all(test, not(windows)))]
fn forget_bash_session_cwd(session_id: &str) {
    bash_session_cwds()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
}

/// Reject an explicit `cwd` that cannot be entered before anything is spawned.
///
/// A missing directory otherwise surfaces as an opaque spawn error, or worse,
/// as a command that quietly ran somewhere else.
fn validate_bash_cwd(explicit: Option<&str>) -> Result<()> {
    let Some(dir) = explicit.map(str::trim).filter(|dir| !dir.is_empty()) else {
        return Ok(());
    };
    match std::fs::metadata(dir) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(anyhow::anyhow!("Working directory is not a directory: {dir}")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(anyhow::anyhow!("Working directory does not exist: {dir}"))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotADirectory => {
            Err(anyhow::anyhow!("Working directory is not a directory: {dir}"))
        }
        Err(err) => Err(anyhow::anyhow!("Working directory is not accessible: {dir} ({err})")),
    }
}

/// Start directory for one call: an explicit `cwd` argument wins, then the
/// directory the session's previous call ended in, then the session working
/// directory. A remembered path that no longer exists falls back to the session
/// directory instead of failing to spawn.
fn resolve_bash_start_dir(explicit: Option<&str>, ctx: &ToolContext, session_cwd: bool) -> Option<PathBuf> {
    if let Some(dir) = explicit.map(str::trim).filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if session_cwd
        && let Some(dir) = bash_session_cwd(&ctx.session_id)
        && dir.is_dir()
    {
        return Some(dir);
    }
    ctx.working_dir.clone()
}

/// The per-call temp file the child shell writes its final directory into.
struct BashCwdCapture {
    path: PathBuf,
}

impl BashCwdCapture {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        // One directory read every so often is enough to keep the temp dir from
        // collecting leftovers without paying for it on every call.
        if sequence.is_multiple_of(64) {
            prune_stale_cwd_captures();
        }
        let path = std::env::temp_dir().join(format!("horon-agent-cwd-{}-{}.txt", std::process::id(), sequence));
        Self { path }
    }

    fn finish(&self, session_id: &str) {
        let Ok(reported) = std::fs::read_to_string(&self.path) else {
            return;
        };
        let reported = PathBuf::from(reported.trim());
        if reported.is_dir() {
            remember_bash_session_cwd(session_id, reported);
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// A run that is handed to the background manager writes its directory after
/// this call has already dropped its file, so those files cannot be deleted
/// eagerly. Sweep the ones from earlier calls here so a long-lived process does
/// not accumulate them; the age bound keeps a concurrent call's file intact.
fn prune_stale_cwd_captures() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(STALE_CWD_CAPTURE_SECONDS))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("horon-agent-cwd-") || !name.ends_with(".txt") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map(|modified| modified < cutoff)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl Drop for BashCwdCapture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The suffix starts on its own line so a trailing comment, `&`, or heredoc in
/// the caller's command cannot swallow it, and it re-raises that command's own
/// status rather than its own.
fn with_cwd_capture(command: &str) -> String {
    format!("{command}\n__st=$?; pwd > \"$HORON_AGENT_CWD_FILE\" 2>/dev/null; exit $__st")
}

fn client_terminal_env(session_id: &str, capture: Option<&BashCwdCapture>) -> Vec<(String, String)> {
    let mut env = vec![("HORON_AGENT_SESSION_ID".to_string(), session_id.to_string())];
    if let Some(capture) = capture {
        env.push((
            "HORON_AGENT_CWD_FILE".to_string(),
            capture.path().to_string_lossy().to_string(),
        ));
    }
    env
}

fn format_client_terminal_output(session_id: &str, outcome: client_terminal::TerminalOutcome) -> String {
    let client_terminal::TerminalOutcome {
        output,
        truncated,
        exit_code,
        signal,
        timed_out_ms,
    } = outcome;
    // The client caps its buffer by dropping the oldest output, so its note
    // belongs in front of what is left; the local path only ever loses the tail.
    let body = if truncated {
        format!("... (output truncated)\n{output}")
    } else {
        output
    };
    // A killed command has no exit code to report and was not promoted to a
    // background task, because its terminal lives in the client.
    if let Some(timeout_ms) = timed_out_ms {
        let notice = format!("Command timed out after {timeout_ms} ms and was killed (client terminal)");
        return if body.trim().is_empty() {
            notice
        } else {
            format!("{notice}\n\n{body}")
        };
    }
    let mut formatted = format_command_output(session_id, "client terminal", body, exit_code);
    if let Some(signal) = signal {
        formatted.push_str(&format!("\n\nSignal: {signal}"));
    }
    formatted
}

/// How a foreground command exited, for the ACP v2 terminal view
/// (`ToolExit`, sent by the turn loop).
fn exit_status_metadata(status: &std::process::ExitStatus) -> serde_json::Value {
    let signal = horon_agent_platform::signal::exit_signal(status).map(horon_agent_platform::signal::signal_name);
    json!({ "exit_status": { "exit_code": status.code(), "signal": signal } })
}

/// Render a finished command's output for the transcript.
///
/// The head, the error/warning lines of the omitted middle, and the tail stay
/// inline, and the full text is saved as a session artifact so nothing is lost:
/// an over-budget result is cut by [`super::artifacts::condense_for_context`],
/// and an opt-in minimizer replaces progress chatter with a summary that points
/// at the raw artifact.
fn format_command_output(session_id: &str, command: &str, output: String, exit_code: Option<i32>) -> String {
    let mut output = if bash_minimizer_enabled() {
        match super::minimizer::minimize(command, &output, exit_code) {
            Some(minimized) => {
                let original_len = output.len();
                let mut rendered = minimized;
                match crate::artifacts::save(session_id, "bash-original", &output) {
                    Ok(id) => rendered.push_str(&format!(
                        "\n[minimized from {original_len} bytes; raw output: artifact://{id}]"
                    )),
                    Err(error) => rendered.push_str(&format!(
                        "\n[Full output was not saved (artifact write failed: {error})]"
                    )),
                }
                rendered
            }
            None => super::artifacts::condense_for_context(session_id, "bash", output, MAX_OUTPUT_BYTES),
        }
    } else {
        super::artifacts::condense_for_context(session_id, "bash", output, MAX_OUTPUT_BYTES)
    };

    if let Some(code) = exit_code.filter(|code| *code != 0) {
        output.push_str(&format!("\n\nExit code: {}", code));
    }

    if output.trim().is_empty() {
        "Command completed successfully (no output)".to_string()
    } else {
        output
    }
}

#[cfg(test)]
mod utf8_truncation_tests {
    use std::time::Duration;

    use super::build_shell_command;
    use super::format_command_output;

    #[test]
    fn format_command_output_keeps_head_and_tail_of_long_output() {
        // The minimizer test flips a process-wide override under this lock.
        let _guard = crate::storage::lock_test_env();
        let input = format!("{}é", "a".repeat(29_999));
        let output = format_command_output("test-session", "cargo build", input, None);
        assert!(output.starts_with(&"a".repeat(12_000)), "the head keeps its 40% budget");
        assert!(
            !output.starts_with(&"a".repeat(12_001)),
            "the head must not exceed its budget"
        );
        assert!(output.contains("omitted from `bash` output"), "the cut must be marked");
        assert!(output.contains('é'), "the tail must survive the cut");
        assert!(output.contains("artifact://"), "the full output is saved: {output}");
        assert!(output.len() <= 30_000, "output stayed within the budget");
    }

    #[test]
    fn resolved_timeout_zero_disables_the_deadline() {
        let zero = super::resolve_bash_timeout(Some(0));
        assert!(zero.duration.is_none());
        assert!(super::timeout_clamp_note(zero.clamped_from).is_empty());

        let omitted = super::resolve_bash_timeout(None);
        assert_eq!(omitted.duration, Some(Duration::from_millis(super::DEFAULT_TIMEOUT_MS)));

        let clamped = super::resolve_bash_timeout(Some(900_000));
        assert_eq!(clamped.duration, Some(Duration::from_millis(600_000)));
        assert_eq!(
            super::timeout_clamp_note(clamped.clamped_from),
            "\n\n[Timeout clamped to 600000ms (requested 900000ms; allowed range 1-600000ms).]"
        );

        let background = super::resolve_background_timeout(None);
        assert!(background.duration.is_none());
        let background_zero = super::resolve_background_timeout(Some(0));
        assert!(background_zero.duration.is_none());
    }

    #[test]
    fn non_interactive_env_disables_pagers_and_prompts() {
        let env = super::non_interactive_env(false);
        let find = |name: &str| env.iter().find(|(key, _)| *key == name).map(|(_, value)| &**value);
        assert_eq!(find("PAGER"), Some("cat"));
        assert_eq!(find("HOMEBREW_PAGER"), Some("cat"));
        assert_eq!(find("AWS_PAGER"), Some(""));
        assert_eq!(find("LESS"), Some("FRX"));
        assert_eq!(find("TERM"), Some("dumb"));
        assert_eq!(find("NO_COLOR"), Some("1"));
        assert_eq!(find("GIT_EDITOR"), Some("true"));
        assert_eq!(find("EDITOR"), Some("true"));
        assert_eq!(find("GIT_TERMINAL_PROMPT"), Some("0"));
        assert_eq!(find("AGENT"), Some("1"));
        assert_eq!(find("npm_config_yes"), Some("true"));
        let askpass = find("SSH_ASKPASS").expect("SSH_ASKPASS is set");
        assert!(askpass == "false" || std::path::Path::new(askpass).is_absolute());

        // A terminal keeps colors, its own TERM, and no forced CI marker.
        let env = super::non_interactive_env(true);
        let find = |name: &str| env.iter().find(|(key, _)| *key == name).map(|(_, value)| &**value);
        assert_eq!(find("TERM"), Some("xterm-256color"));
        assert_eq!(find("NO_COLOR"), None);
        assert_eq!(find("CI"), None);
    }

    #[test]
    fn false_command_walks_path_once_per_distinct_value() {
        let with_false = tempfile::tempdir().expect("temp dir");
        let without_false = tempfile::tempdir().expect("temp dir");
        let program = with_false.path().join("false");
        std::fs::write(&program, b"").expect("write false");
        let cache = super::FalseCommandCache::new(None);

        // No PATH falls back to a bare `false`, exactly as before.
        assert_eq!(&*super::cached_false_command(&cache, None), "false");

        // A PATH without `false` keeps the bare fallback, and the second call
        // with the same value is served from the cache, not a new walk.
        let empty_path = std::env::join_paths([without_false.path()]).expect("join");
        let first = super::cached_false_command(&cache, Some(empty_path.clone()));
        let second = super::cached_false_command(&cache, Some(empty_path.clone()));
        assert_eq!(&*first, "false");
        assert!(std::sync::Arc::ptr_eq(&first, &second));

        // A changed PATH is resolved again and finds the new program.
        let found_path = std::env::join_paths([without_false.path(), with_false.path()]).expect("join");
        let found = super::cached_false_command(&cache, Some(found_path));
        assert_eq!(&*found, program.to_string_lossy());
        assert_eq!(super::resolve_false_command(Some(&empty_path)), "false");
    }

    #[test]
    fn non_interactive_env_keeps_the_same_child_environment() {
        // The table moved to constants; the child must still receive every
        // entry, in the same order, with the platform fixes last.
        let env = super::non_interactive_env(false);
        let names: Vec<&str> = env.iter().map(|(name, _)| *name).collect();
        let platform: Vec<&str> = horon_agent_platform::term::shell_env_overrides()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let mut expected = vec![
            "PAGER",
            "GIT_PAGER",
            "MANPAGER",
            "SYSTEMD_PAGER",
            "BAT_PAGER",
            "DELTA_PAGER",
            "GH_PAGER",
            "GLAB_PAGER",
            "PSQL_PAGER",
            "MYSQL_PAGER",
            "HOMEBREW_PAGER",
            "AWS_PAGER",
            "LESS",
            "TERM",
            "NO_COLOR",
            "PYTHONUNBUFFERED",
            "GIT_EDITOR",
            "VISUAL",
            "EDITOR",
            "GIT_SEQUENCE_EDITOR",
            "GIT_TERMINAL_PROMPT",
            "GCM_INTERACTIVE",
            "SSH_ASKPASS",
        ];
        if !super::ci_suppressed() {
            expected.push("CI");
        }
        expected.push("AGENT");
        expected.extend(super::PROMPT_FREE_TOOLS.iter().map(|(name, _)| *name));
        expected.extend(platform);
        assert_eq!(names, expected);
        assert_eq!(super::PROMPT_FREE_TOOLS[0], ("npm_config_yes", "true"));
        assert_eq!(super::PROMPT_FREE_TOOLS[17], ("CLOUDSDK_CORE_DISABLE_PROMPTS", "1"));
    }

    #[test]
    fn format_command_output_appends_the_exit_code_after_the_cut() {
        let output = format_command_output(
            "test-session",
            "cargo build",
            format!("{}\nlast-line", "x".repeat(40_000)),
            Some(3),
        );
        assert!(output.ends_with("Exit code: 3"));
        assert!(output.contains("last-line"));
    }

    #[tokio::test]
    async fn build_shell_command_uses_cmd_and_executes_command() {
        // Only the `cmd.exe` path can run this; a POSIX shell has no `echo`
        // built-in difference to check here.
        if horon_agent_platform::term::shell_is_posix() {
            return;
        }
        let output = build_shell_command("echo hello-from-cmd", false)
            .output()
            .await
            .expect("run cmd command");
        assert!(output.status.success(), "cmd command should succeed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.to_ascii_lowercase().contains("hello-from-cmd"),
            "unexpected stdout: {}",
            stdout
        );

        let probe_path = std::env::temp_dir().join(format!("horon-agent-cmd-quoting-probe-{}.cmd", std::process::id()));
        std::fs::write(
            &probe_path,
            concat!(
                "@echo off\r\n",
                "if \"%~1\"==\"text with spaces\" if \"%~2\"==\"\" (\r\n",
                "  echo quoted-argument-ok\r\n",
                "  exit /b 0\r\n",
                ")\r\n",
                "echo first=[%~1] second=[%~2]\r\n",
                "exit /b 1\r\n",
            ),
        )
        .expect("write cmd quoting probe");

        let quoted_command = format!("call \"{}\" \"text with spaces\"", probe_path.display());
        let quoted_output = build_shell_command(&quoted_command, false)
            .output()
            .await
            .expect("run cmd quoting probe");
        let _ = std::fs::remove_file(&probe_path);
        let quoted_stdout = String::from_utf8_lossy(&quoted_output.stdout);
        let quoted_stderr = String::from_utf8_lossy(&quoted_output.stderr);
        assert!(
            quoted_output.status.success(),
            "quoted argument should remain one child-process argument; stdout={quoted_stdout:?} stderr={quoted_stderr:?}"
        );
        assert!(
            quoted_stdout.contains("quoted-argument-ok"),
            "unexpected quoted-command stdout: {quoted_stdout}"
        );
    }

    #[test]
    fn build_shell_command_uses_disk_backed_scratch_directory() {
        // Only a platform that hands its shells a scratch directory can check
        // this; elsewhere the variables are deliberately absent.
        if !horon_agent_platform::term::passes_scratch_dir_to_shell() {
            return;
        }
        // Keep HORON_HOME stable until the final directory assertion. Running
        // synchronously avoids holding the environment MutexGuard across await.
        let _env_lock = crate::storage::lock_test_env();
        let expected = super::tool_scratch_dir().expect("horon-agent scratch directory");
        let output = build_shell_command("printf '%s\\n%s\\n' \"$TMPDIR\" \"$HORON_AGENT_SCRATCH_DIR\"", false)
            .as_std_mut()
            .output()
            .expect("run bash command");
        assert!(output.status.success(), "bash command should succeed");
        let stdout = String::from_utf8(output.stdout).expect("utf-8 scratch paths");
        let paths = stdout.lines().collect::<Vec<_>>();
        let expected = expected.to_string_lossy().into_owned();
        assert_eq!(paths, vec![expected.as_str(), expected.as_str()]);
        assert!(std::path::Path::new(&expected).is_dir());
    }
}

pub struct BashTool;

impl BashTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct BashInput {
    command: String,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    run_in_background: Option<bool>,
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_bool_or_default::<_, true>"
    )]
    notify: bool,
    #[serde(default, deserialize_with = "deserialize_bool_or_default::<_, false>")]
    wake: bool,
    /// For background runs: wake the agent after this many seconds with no
    /// new output and no progress events. Resets on activity.
    #[serde(default)]
    stall_wake_seconds: Option<u64>,
    /// Set only when re-issuing a call the gate refused (#604).
    #[serde(default)]
    justification: Option<String>,
    /// Terminal allocation override: "off", "auto", or "always". Omitted
    /// means the `[tools] bash_pty` session default.
    #[serde(default)]
    pty: Option<crate::config::BashPtyMode>,
    /// Start directory for this call only. Overrides both the directory the
    /// session's previous call ended in and the session working directory.
    #[serde(default)]
    cwd: Option<String>,
    /// Named long-running service. The call always runs in the background,
    /// and `bg` plus later calls can address the task by this name.
    #[serde(default)]
    name: Option<String>,
    /// Readiness checks for a named service. Ignored without `name`.
    #[serde(default)]
    ready: Option<ReadySpec>,
}

/// Readiness checks for a named service.
#[derive(Debug, Clone, Default, Deserialize)]
struct ReadySpec {
    /// Regex matched against the last 64 KiB of the task's output.
    #[serde(default)]
    log: Option<String>,
    /// TCP port that must accept a connection.
    #[serde(default)]
    port: Option<u16>,
    /// Host for the port check (default `127.0.0.1`).
    #[serde(default)]
    host: Option<String>,
    /// How long to wait for readiness (default 30, clamped 0.05-3600).
    #[serde(default)]
    timeout_secs: Option<f64>,
}

fn validate_service_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let valid = name.len() <= SERVICE_NAME_MAX_LEN
        && chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    anyhow::ensure!(
        valid,
        "Service name must be 1-48 letters, numbers, dots, underscores, or hyphens"
    );
    Ok(())
}

/// Compile the readiness pattern, requiring at least one check to wait on.
fn compile_ready_log(spec: &ReadySpec) -> Result<Option<regex::Regex>> {
    anyhow::ensure!(
        spec.log.is_some() || spec.port.is_some(),
        "ready needs a log pattern or a port"
    );
    match spec.log.as_deref() {
        Some(pattern) => Ok(Some(
            regex::Regex::new(pattern).map_err(|err| anyhow::anyhow!("Invalid readiness regex: {err}"))?,
        )),
        None => Ok(None),
    }
}

fn background_state_label(status: &crate::bus::BackgroundTaskStatus) -> &'static str {
    match status {
        crate::bus::BackgroundTaskStatus::Running => "running",
        crate::bus::BackgroundTaskStatus::Completed => "completed",
        crate::bus::BackgroundTaskStatus::Failed => "failed",
        crate::bus::BackgroundTaskStatus::Superseded => "superseded",
    }
}

/// The persisted status of one task: its label, pid, and whether it still runs.
async fn task_state(
    manager: &crate::background::BackgroundTaskManager,
    task_id: &str,
) -> (Option<&'static str>, Option<u32>, bool) {
    match manager.status(task_id).await {
        Some(status) => (
            Some(background_state_label(&status.status)),
            status.pid,
            status.status == crate::bus::BackgroundTaskStatus::Running,
        ),
        // The status file is written before the spawn returns, so a missing one
        // means a concurrent cleanup, not a dead task.
        None => (None, None, true),
    }
}

/// What a readiness wait learned about a named service.
struct ServiceState {
    label: &'static str,
    pid: Option<u32>,
    ready: bool,
    timed_out: bool,
    timeout_secs: f64,
    output_tail: Option<String>,
}

fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(count);
    lines[start..].join("\n")
}

/// The last [`READINESS_OUTPUT_TAIL_BYTES`] of a service's output file.
async fn output_tail(path: &Path) -> String {
    use tokio::io::AsyncSeekExt as _;

    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return String::new();
    };
    let len = file.metadata().await.map(|meta| meta.len()).unwrap_or(0);
    if len > READINESS_OUTPUT_TAIL_BYTES as u64 {
        let _ = file
            .seek(std::io::SeekFrom::Start(len - READINESS_OUTPUT_TAIL_BYTES as u64))
            .await;
    }
    let mut buffer = Vec::new();
    if file.read_to_end(&mut buffer).await.is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

async fn port_accepts(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(READINESS_CONNECT_TIMEOUT_MS),
            tokio::net::TcpStream::connect((host, port))
        )
        .await,
        Ok(Ok(_))
    )
}

/// Wait until every configured readiness check passes, the task exits, or the
/// window closes.
async fn await_service_ready(
    manager: &crate::background::BackgroundTaskManager,
    task_id: &str,
    output_file: &Path,
    spec: &ReadySpec,
    log: Option<&regex::Regex>,
) -> ServiceState {
    let timeout_secs = spec
        .timeout_secs
        .unwrap_or(READINESS_DEFAULT_TIMEOUT_SECS)
        .clamp(READINESS_MIN_TIMEOUT_SECS, READINESS_MAX_TIMEOUT_SECS);
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(timeout_secs);
    let host = spec.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
    loop {
        let (label, pid, running) = task_state(manager, task_id).await;
        let log_ready = match log {
            Some(pattern) => pattern.is_match(&output_tail(output_file).await),
            None => true,
        };
        let port_ready = match spec.port {
            Some(port) => port_accepts(&host, port).await,
            None => true,
        };
        if log_ready && port_ready {
            return ServiceState {
                label: label.unwrap_or("running"),
                pid,
                ready: true,
                timed_out: false,
                timeout_secs,
                output_tail: None,
            };
        }
        if !running {
            let tail = last_lines(&output_tail(output_file).await, 40);
            return ServiceState {
                label: label.unwrap_or("running"),
                pid,
                ready: false,
                timed_out: false,
                timeout_secs,
                output_tail: (!tail.is_empty()).then_some(tail),
            };
        }
        if tokio::time::Instant::now() >= deadline {
            return ServiceState {
                label: label.unwrap_or("running"),
                pid,
                ready: false,
                timed_out: true,
                timeout_secs,
                output_tail: None,
            };
        }
        tokio::time::sleep(Duration::from_millis(READINESS_POLL_INTERVAL_MS)).await;
    }
}

/// A snapshot for a service without readiness checks, allowing up to a second
/// for the child's pid to be recorded.
async fn snapshot_service_state(
    manager: &crate::background::BackgroundTaskManager,
    task_id: &str,
    output_file: &Path,
) -> ServiceState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let (label, pid, running) = task_state(manager, task_id).await;
        if pid.is_some() || !running || tokio::time::Instant::now() >= deadline {
            let tail = last_lines(&output_tail(output_file).await, 40);
            return ServiceState {
                label: label.unwrap_or("running"),
                pid,
                ready: false,
                timed_out: false,
                timeout_secs: 0.0,
                output_tail: (!tail.is_empty()).then_some(tail),
            };
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A running task in this session that already uses `name`.
///
/// A persisted `Running` status is not enough on its own: a task whose owning
/// process died (an earlier run, a reload) leaves its status file behind, and
/// the orphan reconciler deliberately skips a non-detached task that recorded a
/// child pid. Liveness of the owner is the fact that decides.
async fn running_service(
    manager: &crate::background::BackgroundTaskManager,
    session_id: &str,
    name: &str,
) -> Option<String> {
    manager
        .list()
        .await
        .into_iter()
        .find(|task| {
            task.session_id == session_id
                && task.status == crate::bus::BackgroundTaskStatus::Running
                && task.display_name.as_deref() == Some(name)
                && manager.task_owner_is_live(task)
        })
        .map(|task| task.task_id)
}

fn append_notes(output: &mut ToolOutput, notes: &[String]) {
    for note in notes {
        output.output.push_str("\n\n");
        output.output.push_str(note);
    }
}

fn default_true() -> bool {
    true
}

// OpenAI strict schemas represent omitted optional arguments as explicit null.
// Serde's `default` only handles missing keys, so accept null separately while
// retaining the same defaults and rejecting non-boolean values.
fn deserialize_bool_or_default<'de, D, const DEFAULT: bool>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(DEFAULT))
}

#[path = "bash_pty.rs"]
mod bash_pty;
#[path = "client_terminal.rs"]
mod client_terminal;
#[path = "bash_destructive_gate.rs"]
mod destructive_gate;
use bash_pty::{BoxedReader, BoxedWriter};
use destructive_gate::destructive_command_refusal;

/// The destructive-command gate, exposed so a monitored command cannot skip it.
///
/// A command monitor runs the same policy the `bash` tool applies, so a command
/// refused here would be refused there too; without this a monitor would be a
/// way around the gate.
pub(super) fn monitored_command_refusal(command: &str, working_dir: Option<std::path::PathBuf>) -> Option<String> {
    destructive_command_refusal(command, None, working_dir)
}
#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        if !horon_agent_platform::term::shell_is_posix() {
            WINDOWS_SHELL_TOOL_DESCRIPTION
        } else {
            BASH_TOOL_DESCRIPTION
        }
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::write().union(ToolEffects::process())
    }

    fn parameters_schema(&self) -> Value {
        destructive_gate::bash_parameters_schema()
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let mut params: BashInput = serde_json::from_value(input)?;
        // A named service is a background task by definition; `run_in_background`
        // only adds the same mode for an unnamed command.
        let run_in_background = params.run_in_background.unwrap_or(false) || params.name.is_some();
        if let Some(name) = params.name.as_deref() {
            validate_service_name(name)?;
        }

        // An explicit start directory is checked before anything is spawned, so
        // a typo cannot look like a command that ran somewhere unexpected.
        validate_bash_cwd(params.cwd.as_deref())?;

        let mut notes: Vec<String> = Vec::new();
        if params.ready.is_some() && params.name.is_none() {
            notes.push("Ignored ready: it only applies together with name.".to_string());
        }

        // Interceptor: name the dedicated tool for an idiom that has one. It
        // runs before the destructive gate because a blocked call should say
        // which tool replaces it, not lecture about blast radius.
        let interceptor_mode = crate::config::config().tools.bash_interceptor;
        let mut interceptor_note = None;
        if interceptor_mode != crate::config::BashInterceptorMode::Off
            && let Some(interception) = bash_interceptor::intercept(&params.command, params.name.is_some())
        {
            if interceptor_mode == crate::config::BashInterceptorMode::Block {
                return Err(anyhow::anyhow!(
                    "Blocked: {}\n\nOriginal command: {}",
                    interception.message,
                    params.command
                ));
            }
            interceptor_note = Some(interception.message);
        }

        // Shared-checkout guard: a history rewrite in the main checkout can
        // discard another session's uncommitted work. The rev-parse probes are
        // synchronous, so they run off the async worker.
        if crate::config::config().tools.git_guard {
            let working_dir = ctx.working_dir.clone().unwrap_or_else(|| PathBuf::from("."));
            let command = params.command.clone();
            if let Ok(Some(refusal)) =
                tokio::task::spawn_blocking(move || git_guard::review(&command, &working_dir)).await
            {
                return Err(anyhow::anyhow!(refusal));
            }
        }

        // Destructive-command gate (#604), before background dispatch.
        if let Some(refusal) = destructive_command_refusal(
            &params.command,
            params.justification.as_deref(),
            ctx.working_dir.clone(),
        ) {
            return Err(anyhow::anyhow!(refusal));
        }

        // The wrapper is a POSIX shell function; a `cmd.exe` child gets none.
        if horon_agent_platform::term::shell_is_posix()
            && let Some(wrapped) = wrap_repo_cargo_commands(&params.command, ctx.working_dir.as_deref())
        {
            params.command = wrapped;
        }

        if run_in_background {
            let mut output = self.execute_background(params, ctx).await?;
            if let Some(note) = interceptor_note {
                output.output = format!("[Note: {note}]\n{}", output.output);
            }
            append_notes(&mut output, &notes);
            return Ok(output);
        }

        // Foreground execution with stdin detection
        let hint = file_edit_hint(&params.command);
        let mut output = self.execute_foreground(&params, &ctx).await?;
        if let Some(note) = interceptor_note {
            output.output = format!("[Note: {note}]\n{}", output.output);
        }
        if let Some(hint) = hint {
            output.output.push_str("\n\n");
            output.output.push_str(hint);
        }
        append_notes(&mut output, &notes);
        Ok(output)
    }
}

const FILE_EDIT_HINT: &str = "Note: this command edits files in place. Next time use `edit` \
(exact replacements, all-or-nothing, pass `edits` for several), `replace` (regex or multi-file, \
with expected_count), or `apply_patch` (multi-file patches). They fail loudly on a missed match \
instead of silently writing nothing, and show a reviewable diff.";

/// Detect shell commands that rewrite source files in place, where a missed
/// match silently does nothing. The command still runs, the agent is nudged.
fn file_edit_hint(command: &str) -> Option<&'static str> {
    let compact: String = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let sed_in_place = compact.split(['|', ';', '&']).any(|segment| {
        let mut words = segment.split_whitespace();
        words.next() == Some("sed")
            && words.any(|word| {
                word == "--in-place"
                    || word.starts_with("--in-place=")
                    || (word.starts_with('-') && !word.starts_with("--") && word[1..].contains('i'))
            })
    });
    let perl_in_place = compact
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0] == "perl" && pair[1].starts_with('-') && pair[1].contains('i'));
    let script_rewrite = (compact.contains("python") || compact.contains("node "))
        && (compact.contains(".replace(") || compact.contains("re.sub("))
        && (compact.contains(",'w')")
            || compact.contains(",\"w\")")
            || compact.contains(", 'w')")
            || compact.contains(", \"w\")")
            || compact.contains("write_text(")
            || compact.contains("writeFileSync("));
    (sed_in_place || perl_in_place || script_rewrite).then_some(FILE_EDIT_HINT)
}

#[cfg(test)]
mod file_edit_hint_tests {
    use super::file_edit_hint;

    #[test]
    fn flags_in_place_edits() {
        for command in [
            "sed -i 's/a/b/' src/main.rs",
            "cd x && sed -Ei 's/a/b/g' f.rs",
            "sed --in-place=.bak 's/a/b/' f",
            "perl -pi -e 's/a/b/' f.rs",
            "python3 - <<'EOF'\ns=open(p).read()\ns=s.replace('a','b')\nopen(p,'w').write(s)\nEOF",
            "python3 -c \"import pathlib;p=pathlib.Path('f');p.write_text(p.read_text().replace('a','b'))\"",
        ] {
            assert!(file_edit_hint(command).is_some(), "{command}");
        }
    }

    #[test]
    fn ignores_read_only_commands() {
        for command in [
            "sed -n '1,20p' f.rs",
            "sed 's/a/b/' f.rs",
            "grep -i foo f.rs",
            "cargo test -p horon-agent-app-core",
            "python3 -c \"print('a'.replace('a','b'))\"",
            "git diff --ignore-space-change",
        ] {
            assert!(file_edit_hint(command).is_none(), "{command}");
        }
    }
}

impl BashTool {
    async fn execute_foreground(&self, params: &BashInput, ctx: &ToolContext) -> Result<ToolOutput> {
        let pty_mode = params.pty.unwrap_or_else(|| crate::config::config().tools.bash_pty);
        let wants_terminal = match pty_mode {
            crate::config::BashPtyMode::Always => true,
            crate::config::BashPtyMode::Off => false,
            crate::config::BashPtyMode::Auto => bash_pty::command_wants_terminal(&params.command),
        };

        // A terminal's master descriptor lives in this process, so a command that
        // is handed to the detached reload-persistable path would lose it.
        if !wants_terminal && self.supports_reload_persistence(ctx) {
            return self.execute_reload_persistable_foreground(params, ctx).await;
        }

        let timeout = resolve_bash_timeout(params.timeout);

        let has_stdin_channel = ctx.stdin_request_tx.is_some();

        let session_cwd = bash_session_cwd_enabled();
        let start_dir = resolve_bash_start_dir(params.cwd.as_deref(), ctx, session_cwd);
        let cwd_capture = session_cwd.then(BashCwdCapture::new);
        let command_text = if cwd_capture.is_some() {
            with_cwd_capture(&params.command)
        } else {
            params.command.clone()
        };

        // An ACP turn runs this command in the client's terminal when the client
        // advertises one. A PTY call must stay local because the terminal
        // descriptor would live in this process, so only a plain foreground call
        // reaches here, and a client terminal is always given a deadline.
        if let Some(timeout_duration) = timeout.duration
            && !wants_terminal
            && let Some(bridge) = super::client_bridge::for_session(&ctx.session_id)
            && bridge.caps().terminal
        {
            let request = client_terminal::TerminalRequest {
                command: command_text.clone(),
                cwd: start_dir.as_deref(),
                env: client_terminal_env(&ctx.session_id, cwd_capture.as_ref()),
                output_byte_limit: MAX_OUTPUT_BYTES,
                timeout: timeout_duration,
                timeout_ms: timeout_duration.as_millis() as u64,
                tool_call_id: Some(ctx.tool_call_id.clone()),
            };
            match client_terminal::create(&bridge, &request).await {
                Ok(terminal_id) => {
                    let outcome = client_terminal::run(Arc::clone(&bridge), terminal_id, &request).await;
                    // The client ran the wrapped command, so its end directory is
                    // known now; a capture the shell never wrote leaves the session
                    // directory where it was.
                    if let Some(capture) = cwd_capture.as_ref() {
                        capture.finish(&ctx.session_id);
                    }
                    let title = params.intent.clone().unwrap_or_else(|| params.command.clone());
                    return Ok(
                        ToolOutput::new(format_client_terminal_output(&ctx.session_id, outcome?)).with_title(title),
                    );
                }
                Err(error) => crate::logging::warn(&format!(
                    "[bash] ACP client terminal could not be created, running locally: {error:#}"
                )),
            }
        }

        // The plan decides whether a terminal is allocated; build the command
        // with that answer so it can pick a matching TERM/NO_COLOR set.
        let plan = bash_pty::plan(pty_mode, &params.command)?;
        let has_terminal = matches!(&plan, bash_pty::PtyPlan::Terminal(_));
        let mut command = build_shell_command(&command_text, has_terminal);
        command
            .env("HORON_AGENT_SESSION_ID", &ctx.session_id)
            .kill_on_drop(true);
        if let Some(capture) = cwd_capture.as_ref() {
            command.env("HORON_AGENT_CWD_FILE", capture.path());
        }

        // A terminal already starts the child in its own session; every other
        // child becomes a process-group leader so signals reach its whole tree.
        if !has_terminal {
            horon_agent_platform::process::isolate_process_group(command.as_std_mut());
        }

        let (pty_handle, pipe_note) = match plan {
            bash_pty::PtyPlan::Pipe { note } => {
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
                if has_stdin_channel {
                    command.stdin(Stdio::piped());
                }
                (None, note)
            }
            bash_pty::PtyPlan::Terminal(mut handle) => {
                handle.configure(&mut command)?;
                (Some(handle), None)
            }
        };

        if let Some(ref dir) = start_dir {
            command.current_dir(dir);
        }
        let mut child = command.spawn()?;
        // The terminal descriptors were moved into `command` for the child; drop
        // the parent's copies now so the master end reports end-of-file once the
        // child exits instead of waiting on a slave the parent still holds.
        drop(command);

        let child_pid = child.id().unwrap_or(0);
        let stdin_handle = match pty_handle.as_ref() {
            Some(handle) if has_stdin_channel => Some(Box::new(handle.writer()?) as BoxedWriter),
            Some(_) => None,
            None => child.stdin.take().map(|stdin| Box::new(stdin) as BoxedWriter),
        };
        let (stdout_handle, stderr_handle) = match pty_handle.as_ref() {
            Some(handle) => (Some(Box::new(handle.reader()?) as BoxedReader), None),
            None => (
                child.stdout.take().map(|stdout| Box::new(stdout) as BoxedReader),
                child.stderr.take().map(|stderr| Box::new(stderr) as BoxedReader),
            ),
        };
        drop(pty_handle);

        // Owned copies so the work can outlive this call if it is promoted to a
        // background task on timeout.
        let title = params.intent.clone().unwrap_or_else(|| params.command.clone());
        let stdin_tx = ctx.stdin_request_tx.clone();
        let tool_call_id = ctx.tool_call_id.clone();
        let title_for_work = title.clone();
        // The output formatter needs the owning session (for the artifact
        // store) and the command text (for the minimizer), and the work task
        // outlives this borrow scope.
        let output_session_id = ctx.session_id.clone();
        let output_command = params.command.clone();
        // Track progress parsed from output so a timeout promotion starts the
        // background task at the real percentage instead of 0%.
        let promoted_progress = std::sync::Arc::new(PromotedCommandProgress::default());
        let stdout_progress = std::sync::Arc::clone(&promoted_progress);
        let stderr_progress = std::sync::Arc::clone(&promoted_progress);
        // Asked once, here: a session no v2 connection owns gets `None`, which
        // collects no bytes and arms no timer for the whole run.
        let v2 = crate::tool::acp_session::owns_v2(&ctx.session_id);
        let stdout_stream = stream::OutputStream::for_call(&ctx.session_id, &ctx.tool_call_id, v2);
        let stderr_stream = stream::OutputStream::for_call(&ctx.session_id, &ctx.tool_call_id, v2);

        // Run the command (read stdout/stderr, service stdin, wait for exit) in a
        // dedicated task so that, if it exceeds the foreground timeout, we can hand
        // the still-running task off to the background manager instead of killing it.
        let mut work_handle: tokio::task::JoinHandle<Result<ToolOutput>> = tokio::spawn(async move {
            // If this future is dropped before the child exits — a cancelled
            // call, a panicking sibling task — stop the whole process tree
            // instead of leaving the shell's children running. A terminal child
            // leads its own session already.
            let mut process_group_guard = if has_terminal {
                horon_agent_platform::process::ProcessGroupGuard::new(None)
            } else {
                horon_agent_platform::process::ProcessGroupGuard::new(Some(child_pid))
            };
            let stdout_task = tokio::spawn(stream::collect_output(stdout_handle, stdout_progress, stdout_stream));

            let stderr_task = tokio::spawn(stream::collect_output(stderr_handle, stderr_progress, stderr_stream));

            let stdin_task = if has_stdin_channel {
                Some(tokio::spawn(async move {
                    if let (Some(mut stdin_pipe), Some(stdin_tx)) = (stdin_handle, stdin_tx) {
                        tokio::time::sleep(Duration::from_millis(STDIN_INITIAL_DELAY_MS)).await;

                        let mut request_counter = 0u32;
                        loop {
                            // The adapter picks the strongest check the
                            // platform offers, including the descendant walk
                            // that finds a wrapper chain reading stdin.
                            let state = horon_agent_platform::term::process_tree_stdin_state(child_pid);

                            if state == horon_agent_platform::term::StdinState::Reading {
                                request_counter += 1;
                                let request_id = format!("stdin-{}-{}", tool_call_id, request_counter);
                                let (response_tx, response_rx) = tokio::sync::oneshot::channel();

                                let request = StdinInputRequest {
                                    request_id,
                                    prompt: String::new(),
                                    is_password: false,
                                    response_tx,
                                };

                                if stdin_tx.send(request).is_err() {
                                    break;
                                }

                                match response_rx.await {
                                    Ok(input) => {
                                        let line = if input.ends_with('\n') {
                                            input
                                        } else {
                                            format!("{}\n", input)
                                        };
                                        if stdin_pipe.write_all(line.as_bytes()).await.is_err() {
                                            break;
                                        }
                                        if stdin_pipe.flush().await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }

                                tokio::time::sleep(Duration::from_millis(100)).await;
                            } else {
                                tokio::time::sleep(Duration::from_millis(STDIN_POLL_INTERVAL_MS)).await;
                            }
                        }
                    }
                }))
            } else {
                drop(stdin_handle);
                None
            };

            let status = child.wait().await?;
            process_group_guard.disarm();

            if let Some(task) = stdin_task {
                task.abort();
            }

            let stdout = stdout_task.await.unwrap_or_default();
            let stderr = stderr_task.await.unwrap_or_default();

            let mut output = String::new();
            if !stdout.is_empty() {
                output.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&stderr);
            }
            let output = format_command_output(&output_session_id, &output_command, output, status.code());
            let output = match pipe_note {
                Some(note) => format!("{output}\n{note}"),
                None => output,
            };
            Ok(ToolOutput::new(output)
                .with_title(title_for_work)
                .with_metadata(exit_status_metadata(&status)))
        });

        // `None` from this expression means the deadline passed while the
        // command kept running: promote it rather than kill it. A `timeout: 0`
        // call has no deadline at all and simply awaits completion.
        let join_result = match timeout.duration {
            Some(duration) => tokio::time::timeout(duration, &mut work_handle).await.ok(),
            None => Some((&mut work_handle).await),
        };
        match join_result {
            Some(Ok(Ok(mut output))) => {
                // The call returned here, so its end directory is known. A
                // promoted command is not: it keeps running past this return,
                // so only this branch may move the session directory.
                if let Some(capture) = cwd_capture.as_ref() {
                    capture.finish(&ctx.session_id);
                }
                output.output.push_str(&timeout_clamp_note(timeout.clamped_from));
                Ok(output)
            }
            Some(Ok(Err(e))) => Err(anyhow::anyhow!("Command failed: {}", e)),
            Some(Err(join_err)) => Err(anyhow::anyhow!("Command task panicked: {}", join_err)),
            None => {
                // Timed out, but the command is still running. Instead of killing
                // it, promote it to a background task so it keeps running, renders
                // as a background-task card, and the agent is told where to find it.
                let display_name = summarize_background_command(params.intent.as_deref(), &params.command);
                let finalized = crate::background::global().subscribe_finalized();
                let info = crate::background::global()
                    .adopt_with_options_for_tool_call(
                        "bash",
                        Some(display_name.clone()),
                        &ctx.session_id,
                        params.notify,
                        params.wake,
                        &ctx.tool_call_id,
                        Some(&params.command),
                        start_dir.as_deref(),
                        work_handle,
                    )
                    .await;
                crate::server::background_work::report_started(
                    &ctx.session_id,
                    &ctx.tool_call_id,
                    &info.task_id,
                    "bash",
                    Some(&params.command),
                    start_dir.as_deref(),
                    finalized,
                    None,
                );
                // Route progress parsed from the still-running command's output
                // to the new background task, including any update seen before
                // promotion, so the task row shows real progress from the start.
                promoted_progress.attach_task(&info.task_id).await;
                let timeout_ms = timeout.duration.unwrap_or_default().as_millis() as u64;

                let output = format!(
                    "Command exceeded the foreground timeout after {:.1}s and is continuing in background (not killed).\n\n\
                     Task ID: {}\n\
                     Name: {}\n\
                     Output file: {}\n\
                     Status file: {}\n\n\
                     The command is still running; do not rerun it unless you intentionally want a second copy.\n\
                     Use `bg` with action=\"wait\" and task_id=\"{}\" to wait for completion or the next progress checkpoint.\n\
                     Use `bg` with action=\"output\" and task_id=\"{}\" to inspect output.\n\
                     If you expected it to finish quickly and it did not, the `timeout` parameter is in MILLISECONDS; pass a larger value or omit it.{}{}",
                    timeout_ms as f64 / 1000.0,
                    info.task_id,
                    display_name,
                    info.output_file.display(),
                    info.status_file.display(),
                    info.task_id,
                    info.task_id,
                    pipe_note.map(|note| format!("\n\n{note}")).unwrap_or_default(),
                    timeout_clamp_note(timeout.clamped_from),
                );

                Ok(ToolOutput::new(output).with_title(title).with_metadata(json!({
                    "background": true,
                    "task_id": info.task_id,
                    "display_name": display_name,
                    "output_file": info.output_file.to_string_lossy(),
                    "status_file": info.status_file.to_string_lossy(),
                    "timeout_promoted": true,
                    "foreground_timeout_ms": timeout_ms,
                })))
            }
        }
    }

    /// A turn a client is waiting on reports into that client's tool result, so
    /// it runs in this process; the detached path is for unattended turns, whose
    /// command outlives the reload that ends them. ACP sessions are checked
    /// separately because a v2 connection registers no session event sender. A
    /// platform that cannot replace its image has no reload to survive, and
    /// neither has a process that never listens for one: a one-shot `run` exits
    /// when its turn ends, so its command stays a child it can wait on and stop.
    fn supports_reload_persistence(&self, ctx: &ToolContext) -> bool {
        horon_agent_platform::process::supports_process_replacement()
            && crate::server::reload::process_reloads_in_place()
            && matches!(ctx.execution_mode, crate::tool::ToolExecutionMode::AgentTurn)
            && ctx.stdin_request_tx.is_none()
            && !crate::server::session_has_attached_client(&ctx.session_id)
            && !crate::tool::acp_session::is_acp(&ctx.session_id)
            && ctx.graceful_shutdown_signal.is_some()
    }

    async fn execute_reload_persistable_foreground(&self, params: &BashInput, ctx: &ToolContext) -> Result<ToolOutput> {
        let timeout = resolve_bash_timeout(params.timeout);
        let started_at = Utc::now().to_rfc3339();
        let started = Instant::now();
        let manager = crate::background::global();
        let info = manager.reserve_task_info();
        let display_name = summarize_background_command(params.intent.as_deref(), &params.command);

        let session_cwd = bash_session_cwd_enabled();
        let start_dir = resolve_bash_start_dir(params.cwd.as_deref(), ctx, session_cwd);
        // The state file is only read when the child is seen to exit here. A run
        // that is handed to the background manager never updates the session
        // directory, because its end time is unknowable.
        let cwd_capture = session_cwd.then(BashCwdCapture::new);

        let Some(mut cmd) =
            build_detached_shell_wrapper(&params.command, cwd_capture.as_ref().map(BashCwdCapture::path))
        else {
            anyhow::bail!("this platform has no shell that can record a detached command's working directory");
        };
        cmd.env("HORON_AGENT_SESSION_ID", &ctx.session_id);
        let stdout = OpenOptions::new().create(true).append(true).open(&info.output_file)?;
        let stderr = stdout.try_clone()?;
        cmd.stdin(Stdio::null()).stdout(stdout).stderr(stderr);
        if let Some(ref dir) = start_dir {
            cmd.current_dir(dir);
        }

        let mut child = horon_agent_platform::spawn_detached(&mut cmd)?;
        let pid = child.id();
        let shutdown_signal = ctx.graceful_shutdown_signal.clone();
        // The detached child is checked by polling, because its exit has to be
        // seen without holding a handle a reload would lose. Most commands end
        // within milliseconds, so the interval starts short and doubles up to
        // the old fixed 100 ms instead of making every quick command wait it out.
        let mut poll_interval = DETACHED_POLL_MIN;

        loop {
            if let Some(status) = child.try_wait()? {
                let output = tokio::fs::read_to_string(&info.output_file).await.unwrap_or_default();
                let _ = tokio::fs::remove_file(&info.output_file).await;
                let _ = tokio::fs::remove_file(&info.status_file).await;
                if let Some(capture) = cwd_capture.as_ref() {
                    capture.finish(&ctx.session_id);
                }
                let mut output = format_command_output(&ctx.session_id, &params.command, output, status.code());
                output.push_str(&timeout_clamp_note(timeout.clamped_from));
                return Ok(ToolOutput::new(output)
                    .with_title(params.intent.clone().unwrap_or_else(|| params.command.clone()))
                    .with_metadata(exit_status_metadata(&status)));
            }

            if timeout
                .duration
                .map(|duration| started.elapsed() >= duration)
                .unwrap_or(false)
            {
                let elapsed = started.elapsed();
                let timeout_ms = timeout.duration.unwrap_or_default().as_millis() as u64;
                manager
                    .register_detached_task(
                        &info,
                        "bash",
                        Some(display_name.clone()),
                        &ctx.session_id,
                        pid,
                        &started_at,
                        params.notify,
                        params.wake,
                    )
                    .await;
                // Detached commands write straight to the output file, so no
                // in-process reader sees their output. Follow the file to keep
                // the task's progress bar live.
                spawn_detached_progress_follower(info.task_id.clone(), info.output_file.clone());

                let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
                let output = format!(
                    "Command exceeded the foreground timeout after {:.1}s and is continuing in background.\n\n\
                     Task ID: {}\n\
                     Name: {}\n\
                     Foreground time used: {:.1}s\n\
                     Output file: {}\n\
                     Status file: {}\n\n\
                     The command is still running; do not rerun it unless you intentionally want a second copy.\n\
                     Use `bg` with action=\"wait\" and task_id=\"{}\" to wait for completion or the next progress checkpoint.\n\
                     Use `bg` with action=\"output\" and task_id=\"{}\" to inspect output.{}",
                    timeout_ms as f64 / 1000.0,
                    info.task_id,
                    display_name,
                    elapsed.as_secs_f64(),
                    info.output_file.display(),
                    info.status_file.display(),
                    info.task_id,
                    info.task_id,
                    timeout_clamp_note(timeout.clamped_from),
                );
                return Ok(ToolOutput::new(output)
                    .with_title(params.intent.clone().unwrap_or_else(|| params.command.clone()))
                    .with_metadata(json!({
                        "background": true,
                        "task_id": info.task_id,
                        "output_file": info.output_file.to_string_lossy(),
                        "status_file": info.status_file.to_string_lossy(),
                        "timeout_promoted": true,
                        "foreground_timeout_ms": timeout_ms,
                        "foreground_elapsed_ms": elapsed_ms,
                        "pid": pid,
                    })));
            }

            if shutdown_signal.as_ref().map(|signal| signal.is_set()).unwrap_or(false) {
                manager
                    .register_detached_task(
                        &info,
                        "bash",
                        Some(display_name.clone()),
                        &ctx.session_id,
                        pid,
                        &started_at,
                        params.notify,
                        params.wake,
                    )
                    .await;
                spawn_detached_progress_follower(info.task_id.clone(), info.output_file.clone());
                let output = format!(
                    "Command continued in background due to reload.\n\nTask ID: {}\nOutput file: {}\nStatus file: {}\n\nUse `bg` with action=\"wait\" and task_id=\"{}\" after reload to wait for completion or the next progress checkpoint.",
                    info.task_id,
                    info.output_file.display(),
                    info.status_file.display(),
                    info.task_id,
                );
                return Ok(ToolOutput::new(output)
                    .with_title(params.intent.clone().unwrap_or_else(|| params.command.clone()))
                    .with_metadata(json!({
                        "background": true,
                        "task_id": info.task_id,
                        "output_file": info.output_file.to_string_lossy(),
                        "status_file": info.status_file.to_string_lossy(),
                        "reload_persisted": true,
                        "pid": pid,
                    })));
            }

            tokio::time::sleep(poll_interval).await;
            poll_interval = (poll_interval * 2).min(DETACHED_POLL_MAX);
        }
    }

    /// Execute a command in the background
    async fn execute_background(&self, params: BashInput, ctx: ToolContext) -> Result<ToolOutput> {
        let command = params.command.clone();
        let description = params.intent.clone();
        let service = params
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        if let Some(name) = service.as_deref() {
            validate_service_name(name)?;
        }
        let readiness_log = match (service.as_deref(), params.ready.as_ref()) {
            (Some(_), Some(spec)) => compile_ready_log(spec)?,
            _ => None,
        };
        let display_name = service
            .clone()
            .unwrap_or_else(|| summarize_background_command(description.as_deref(), &command));
        let session_cwd = bash_session_cwd_enabled();
        let start_dir = resolve_bash_start_dir(params.cwd.as_deref(), &ctx, session_cwd);
        let session_id_env = ctx.session_id.clone();
        let pty_mode = params.pty.unwrap_or_else(|| crate::config::config().tools.bash_pty);
        // A service runs until cancelled: its `timeout` would kill the very
        // thing the caller asked to keep alive, so readiness owns the wait.
        let timeout = if service.is_some() {
            BashTimeout {
                duration: None,
                clamped_from: None,
            }
        } else {
            resolve_background_timeout(params.timeout)
        };
        let timeout_duration = timeout.duration;

        let manager = crate::background::global();
        if let Some(name) = service.as_deref()
            && let Some(existing) = running_service(manager, &ctx.session_id, name).await
        {
            return Err(anyhow::anyhow!(
                "Service '{name}' is already running as task {existing}; cancel it first with bg action=cancel."
            ));
        }

        let wake = params.wake;
        let notify = params.notify || wake;
        // Subscribe before the task exists: the manager publishes a task's
        // terminal transition once, and one that exits immediately must still
        // be reported.
        let finalized = manager.subscribe_finalized();
        let start_cwd = start_dir.clone();
        let call_command = command.clone();
        let info = manager
            .spawn_with_notify_for_tool_call(
                "bash",
                Some(display_name.clone()),
                &ctx.session_id,
                notify,
                wake,
                &ctx.tool_call_id,
                Some(params.command.as_str()),
                start_cwd.as_deref(),
                move |output_path| async move {
                    let plan = bash_pty::plan(pty_mode, &command)?;
                    let has_terminal = matches!(&plan, bash_pty::PtyPlan::Terminal(_));
                    let mut cmd = build_shell_command(&command, has_terminal);
                    cmd.env("HORON_AGENT_SESSION_ID", &session_id_env);
                    cmd.kill_on_drop(true);
                    configure_background_command_stdio(&mut cmd);
                    let mut pipe_note = None;
                    let pty_handle = match plan {
                        bash_pty::PtyPlan::Pipe { note } => {
                            pipe_note = note;
                            // A terminal already starts the child in its own
                            // session; every other child becomes a process-group
                            // leader so group signals reach its whole tree.
                            horon_agent_platform::process::isolate_process_group(cmd.as_std_mut());
                            None
                        }
                        bash_pty::PtyPlan::Terminal(mut handle) => {
                            handle.configure(&mut cmd)?;
                            Some(handle)
                        }
                    };
                    if let Some(ref dir) = start_dir {
                        cmd.current_dir(dir);
                    }
                    let mut child = cmd
                        .spawn()
                        .map_err(|e| anyhow::anyhow!("Failed to spawn command: {}", e))?;
                    drop(cmd);
                    let mut process_group_guard = horon_agent_platform::process::ProcessGroupGuard::new(child.id());
                    // Record the pid so `bg cancel` can signal the group before
                    // it drops this future; without it a cancelled command's
                    // children keep running orphaned.
                    if let Some(pid) = child.id()
                        && let Some(task_id) = task_id_from_output_path(&output_path)
                    {
                        let _ = crate::background::global().record_task_pid(task_id, pid).await;
                    }

                    // Stream output to file
                    let mut file = tokio::fs::File::create(&output_path)
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to create output file: {}", e))?;
                    if let Some(note) = pipe_note {
                        file.write_all(format!("{note}\n").as_bytes()).await.ok();
                        file.flush().await.ok();
                    }

                    // Read stdout and stderr truly concurrently using select!
                    // Sequential reads can deadlock if the unread pipe fills up.
                    let (stdout, stderr) = match pty_handle {
                        Some(handle) => (Some(Box::new(handle.reader()?) as BoxedReader), None),
                        None => (
                            child.stdout.take().map(|stdout| Box::new(stdout) as BoxedReader),
                            child.stderr.take().map(|stderr| Box::new(stderr) as BoxedReader),
                        ),
                    };

                    let mut stdout_lines = stdout.map(|s| BufReader::new(s).lines());
                    let mut stderr_lines = stderr.map(|s| BufReader::new(s).lines());
                    let mut stdout_done = stdout_lines.is_none();
                    let mut stderr_done = stderr_lines.is_none();
                    let timeout_sleep = timeout_duration.map(tokio::time::sleep);
                    tokio::pin!(timeout_sleep);
                    let mut timed_out = false;

                    while !stdout_done || !stderr_done {
                        tokio::select! {
                                _ = async {
                                    match timeout_sleep.as_mut().as_pin_mut() {
                                        Some(sleep) => sleep.await,
                                        None => std::future::pending().await,
                                    }
                                }, if timeout_duration.is_some() => {
                                    timed_out = true;
                                    if let Some(pid) = child.id() {
                                        // Give the group a second to run its
                                        // TERM handler, then take it down.
                                        let _ = tokio::task::spawn_blocking(move || {
                                            horon_agent_platform::terminate_process_group(pid, Duration::from_secs(1))
                                        })
                                        .await;
                                    } else {
                                        let _ = child.start_kill();
                                    }
                                    break;
                                }
                            line = async {
                                match stdout_lines.as_mut() {
                                    Some(r) => r.next_line().await,
                                    None => std::future::pending().await,
                                }
                            }, if !stdout_done => {
                                match line {
                                    Ok(Some(line)) => {
                                        handle_background_output_line(&output_path, &mut file, &line, false).await;
                                    }
                                    _ => { stdout_done = true; }
                                }
                            }
                            line = async {
                                match stderr_lines.as_mut() {
                                    Some(r) => r.next_line().await,
                                    None => std::future::pending().await,
                                }
                            }, if !stderr_done => {
                                match line {
                                    Ok(Some(line)) => {
                                        handle_background_output_line(&output_path, &mut file, &line, true).await;
                                    }
                                    _ => { stderr_done = true; }
                                }
                            }
                        }
                    }

                    if timed_out {
                        let _ = child.wait().await;
                        process_group_guard.disarm();
                        let msg = timeout_message(timeout.duration.unwrap_or_default().as_millis() as u64);
                        let timeout_line = format!("\n--- {} ---\n", msg);
                        file.write_all(timeout_line.as_bytes()).await.ok();
                        return Ok(TaskResult::failed(Some(124), msg));
                    }

                    let status = child.wait().await?;
                    process_group_guard.disarm();
                    let exit_code = status.code();

                    // Write final status line
                    let status_line = format!(
                        "\n--- Command finished with exit code: {} ---\n",
                        exit_code.unwrap_or(-1)
                    );
                    file.write_all(status_line.as_bytes()).await.ok();

                    if status.success() {
                        Ok(TaskResult::completed(exit_code))
                    } else {
                        Ok(TaskResult::failed(
                            exit_code,
                            format!("Command exited with code {}", exit_code.unwrap_or(-1)),
                        ))
                    }
                },
            )
            .await?;
        crate::server::background_work::report_started(
            &ctx.session_id,
            &ctx.tool_call_id,
            &info.task_id,
            "bash",
            Some(call_command.as_str()),
            start_cwd.as_deref(),
            finalized,
            Some(stream::BackgroundTail::new(
                &ctx.session_id,
                &ctx.tool_call_id,
                info.output_file.clone(),
            )),
        );

        let service_state = match (service.as_deref(), params.ready.as_ref()) {
            (Some(_), Some(spec)) => {
                Some(await_service_ready(manager, &info.task_id, &info.output_file, spec, readiness_log.as_ref()).await)
            }
            (Some(_), None) => Some(snapshot_service_state(manager, &info.task_id, &info.output_file).await),
            _ => None,
        };

        let notify_msg = if wake {
            "The agent will be woken when the task completes."
        } else if notify {
            "You will be notified when the task completes."
        } else {
            "Notifications disabled. Use `bg` tool to check status."
        };

        let stall_msg = match params.stall_wake_seconds {
            Some(requested) => {
                match crate::background::global()
                    .arm_stall_watchdog(&info.task_id, requested)
                    .await
                {
                    Some(effective) => format!(
                        "Stall watchdog armed: you will be woken after {}s with no output or progress (resets on activity).\n",
                        effective
                    ),
                    None => String::new(),
                }
            }
            None => String::new(),
        };

        let mut output = format!(
            "Command started in background.\n\n\
             Task ID: {}\n\
             Name: {}\n\
             Output file: {}\n\
             Status file: {}\n\n\
             {}\n\
             {}To wait for completion/checkpoints: use the `bg` tool with action=\"wait\" and task_id=\"{}\"\n\
             To check progress immediately: use the `bg` tool with action=\"status\" and task_id=\"{}\"\n\
             To see output: use the `read` tool on the output file, or `bg` with action=\"output\"\n\n\
             {}",
            info.task_id,
            display_name,
            info.output_file.display(),
            info.status_file.display(),
            notify_msg,
            stall_msg,
            info.task_id,
            info.task_id,
            BACKGROUND_PROGRESS_GUIDANCE,
        );
        if let Some(name) = service.as_deref()
            && let Some(state) = service_state.as_ref()
        {
            let pid = state.pid.map(|pid| pid.to_string()).unwrap_or_else(|| "?".to_string());
            let ready = if state.ready { " ready" } else { "" };
            output = format!("{name}: {} pid={pid}{ready}\n\n{output}", state.label);
        }
        if let Some(state) = service_state.as_ref() {
            if state.timed_out {
                output.push_str(&format!(
                    "\n\nNOT ready — readiness timed out after {}s; process state: {}.",
                    state.timeout_secs, state.label
                ));
            }
            if let Some(tail) = state.output_tail.as_deref() {
                output.push_str("\n\nOutput tail:\n");
                output.push_str(tail);
            }
        }
        if service.is_some() && params.timeout.is_some() {
            output.push_str("\n\nIgnored timeout: services run until cancelled; readiness follows ready.timeout_secs.");
        }
        let output = format!("{output}{}", timeout_clamp_note(timeout.clamped_from));

        Ok(ToolOutput::new(output)
            .with_title(description.unwrap_or_else(|| format!("Background: {}", params.command)))
            .with_metadata(json!({
                "background": true,
                "task_id": info.task_id,
                "display_name": display_name,
                "output_file": info.output_file.to_string_lossy(),
                "status_file": info.status_file.to_string_lossy(),
            })))
    }
}

#[cfg(all(test, not(windows)))]
#[path = "bash_tests.rs"]
mod tests;
