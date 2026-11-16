//! Logging infrastructure for horon-agent
//!
//! Logs to ~/.horon/logs/ with automatic rotation
//!
//! Supports thread-local context for server, session, provider, and model info.

mod event_buffer;
pub mod watchdog;

use chrono::{Datelike, Local, Timelike};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);
static CLEANUP_STARTED: AtomicBool = AtomicBool::new(false);
static TASK_LOG_CONTEXTS: OnceLock<Mutex<HashMap<tokio::task::Id, TaskLogContext>>> = OnceLock::new();
static RATE_LIMITS: OnceLock<Mutex<HashMap<String, RateLimitState>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Debug => "DEBUG",
        }
    }

    fn is_enabled(self) -> bool {
        !matches!(self, Self::Debug) || std::env::var("HORON_AGENT_TRACE").is_ok()
    }
}

#[derive(Clone, Copy, Debug)]
struct RateLimitState {
    last_emit: Instant,
    suppressed: u64,
}

/// Thread-local logging context
#[derive(Default, Clone)]
pub struct LogContext {
    pub server: Option<String>,
    pub session: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
}

thread_local! {
    static LOG_CONTEXT: RefCell<LogContext> = RefCell::new(LogContext::default());
}

/// Update just the session in the current context
pub fn set_session(session: &str) {
    if with_task_context_mut(|ctx| {
        ctx.session = Some(session.to_string());
    }) {
        return;
    }

    LOG_CONTEXT.with(|c| {
        c.borrow_mut().session = Some(session.to_string());
    });
}

/// Update just the server in the current context
pub fn set_server(server: &str) {
    if with_task_context_mut(|ctx| {
        ctx.server = Some(server.to_string());
    }) {
        return;
    }

    LOG_CONTEXT.with(|c| {
        c.borrow_mut().server = Some(server.to_string());
    });
}

/// Update provider and model in the current context
pub fn set_provider_info(provider: &str, model: &str) {
    if with_task_context_mut(|ctx| {
        ctx.provider = Some(provider.to_string());
        ctx.model = Some(model.to_string());
    }) {
        return;
    }

    LOG_CONTEXT.with(|c| {
        let mut ctx = c.borrow_mut();
        ctx.provider = Some(provider.to_string());
        ctx.model = Some(model.to_string());
    });
}

/// Append the current context as a line prefix: the task's context when the
/// running task has one, the thread's otherwise.
fn push_context_prefix(line: &mut String) {
    if let Some(task_id) = tokio::task::try_id()
        && let Some(store) = TASK_LOG_CONTEXTS.get()
        && let Ok(mut contexts) = store.lock()
        && let Some(entry) = contexts.get_mut(&task_id)
    {
        entry.last_used = Instant::now();
        push_context_prefix_for(line, &entry.context);
        return;
    }

    LOG_CONTEXT.with(|c| push_context_prefix_for(line, &c.borrow()));
}

fn with_task_context_mut(update: impl FnOnce(&mut LogContext)) -> bool {
    let Some(task_id) = tokio::task::try_id() else {
        return false;
    };

    let store = TASK_LOG_CONTEXTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut contexts) = store.lock() {
        if contexts.len() >= MAX_TASK_LOG_CONTEXTS && !contexts.contains_key(&task_id) {
            evict_stale_task_contexts(&mut contexts);
        }
        let entry = contexts.entry(task_id).or_insert_with(|| TaskLogContext {
            context: LogContext::default(),
            last_used: Instant::now(),
        });
        entry.last_used = Instant::now();
        update(&mut entry.context);
        true
    } else {
        false
    }
}

/// Per-task log contexts are keyed by tokio task id. Task ids are never
/// reused and nothing observes task exit, so every finished turn task used to
/// leave its context behind for the server's lifetime. Bound the map and drop
/// the least recently used half when it fills.
const MAX_TASK_LOG_CONTEXTS: usize = 1024;

struct TaskLogContext {
    context: LogContext,
    last_used: Instant,
}

fn evict_stale_task_contexts<K>(contexts: &mut HashMap<K, TaskLogContext>) {
    let mut ages: Vec<Instant> = contexts.values().map(|entry| entry.last_used).collect();
    ages.sort_unstable();
    let cutoff = ages[ages.len() / 2];
    contexts.retain(|_, entry| entry.last_used > cutoff);
}

fn task_context_snapshot() -> Option<LogContext> {
    let task_id = tokio::task::try_id()?;
    let store = TASK_LOG_CONTEXTS.get()?;
    let mut contexts = store.lock().ok()?;
    let entry = contexts.get_mut(&task_id)?;
    entry.last_used = Instant::now();
    Some(entry.context.clone())
}

/// Snapshot the current logging context for diagnostics that need stable,
/// session-scoped in-memory keys in addition to the rendered log prefix.
pub fn current_context_snapshot() -> LogContext {
    task_context_snapshot().unwrap_or_else(|| LOG_CONTEXT.with(|c| c.borrow().clone()))
}

fn push_context_prefix_for(line: &mut String, ctx: &LogContext) {
    let start = line.len();
    let mut push_part = |label: &str, value: &str| {
        line.push(if line.len() == start { '[' } else { '|' });
        line.push_str(label);
        line.push_str(value);
    };

    if let Some(ref server) = ctx.server {
        push_part("srv:", server);
    }
    if let Some(ref session) = ctx.session {
        // Truncate session name if too long
        let short = if session.len() > 20 { &session[..20] } else { session };
        push_part("ses:", short);
    }
    if let Some(ref provider) = ctx.provider {
        push_part("prv:", provider);
    }
    if let Some(ref model) = ctx.model {
        // Just use first part of model name
        let short = model.split('-').next().unwrap_or(model);
        push_part("mod:", short);
    }

    if line.len() != start {
        line.push_str("] ");
    }
}

/// Append `value` in decimal, zero-padded to `width` digits.
fn push_padded(line: &mut String, value: u32, width: u32) {
    let mut divisor = 10_u32.pow(width - 1);
    let mut rest = value % (divisor * 10);
    while divisor > 0 {
        line.push(char::from_digit(rest / divisor, 10).unwrap_or('0'));
        rest %= divisor;
        divisor /= 10;
    }
}

const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// Append `now` as [`TIMESTAMP_FORMAT`] would print it.
///
/// Written by hand because `DateTime::format` renders the offset name into a
/// `String` and the formatter builds another one for its output, on every line.
fn push_timestamp(line: &mut String, now: &chrono::NaiveDateTime) {
    let year = match u32::try_from(now.year()) {
        Ok(year) if year <= 9999 => year,
        _ => {
            line.push_str(&now.format(TIMESTAMP_FORMAT).to_string());
            return;
        }
    };
    push_padded(line, year, 4);
    line.push('-');
    push_padded(line, now.month(), 2);
    line.push('-');
    push_padded(line, now.day(), 2);
    line.push(' ');
    push_padded(line, now.hour(), 2);
    line.push(':');
    push_padded(line, now.minute(), 2);
    line.push(':');
    // A leap second is carried in the nanoseconds.
    push_padded(line, now.second() + now.nanosecond() / 1_000_000_000, 2);
    line.push('.');
    push_padded(line, now.nanosecond() / 1_000_000 % 1000, 3);
}

pub struct Logger {
    file: File,
    /// The line being written, kept between writes.
    line: String,
}

fn log_dir() -> Option<PathBuf> {
    horon_agent_storage::logs_dir().ok()
}

impl Logger {
    fn new() -> Option<Self> {
        let log_dir = log_dir()?;
        horon_agent_storage::ensure_dir(&log_dir).ok()?;

        // Use date-based log file
        let date = Local::now().format("%Y-%m-%d");
        let path = log_dir.join(format!("horon-agent-{}.log", date));

        let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;

        Some(Self {
            file,
            line: String::new(),
        })
    }

    fn write(&mut self, level: &str, message: &str) {
        let mut line = std::mem::take(&mut self.line);
        line.clear();
        line.push('[');
        push_timestamp(&mut line, &Local::now().naive_local());
        line.push_str("] [");
        line.push_str(level);
        line.push_str("] ");
        push_context_prefix(&mut line);
        line.push_str(message);
        line.push('\n');
        let written = self.file.write_all(line.as_bytes());
        if line.capacity() <= event_buffer::MAX_KEPT_BYTES {
            self.line = line;
        }
        if let Err(err) = written {
            eprintln!("horon-agent logger write failed: {err}");
            return;
        }
        if let Err(err) = self.file.flush() {
            eprintln!("horon-agent logger flush failed: {err}");
        }
    }
}

/// Route platform-adapter reports into this crate's log file.
///
/// The adapters sit below this crate (which depends on the application
/// foundation, and the foundation depends on the adapters), so they cannot call
/// it directly. They report through `horon_agent_platform_api::log` instead,
/// and this sink puts those messages in the same file at the same levels.
fn platform_log_sink(level: horon_agent_platform_api::log::PlatformLogLevel, message: &str) {
    use horon_agent_platform_api::log::PlatformLogLevel;

    match level {
        PlatformLogLevel::Error => error(message),
        PlatformLogLevel::Warn => warn(message),
        PlatformLogLevel::Info => info(message),
        PlatformLogLevel::Debug => debug(message),
    }
}

/// Initialize the logger (call once at startup)
pub fn init() {
    let mut guard = match LOGGER.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.is_none() {
        *guard = Logger::new();
    }
    drop(guard);

    horon_agent_platform_api::log::set_log_sink(platform_log_sink);

    // Prune stale daily log files once per process, off the startup path so
    // disk I/O never blocks launch. cleanup_old_logs is scoped to our own
    // `horon-agent-*.log` files, so it is safe to run unconditionally.
    // Detect silent hangs in long sessions: without this a freeze leaves the
    // log simply stopping, with no phase or thread-state evidence.
    watchdog::start();

    if !CLEANUP_STARTED.swap(true, Ordering::SeqCst) {
        std::thread::Builder::new()
            .name("horon-agent-log-cleanup".to_string())
            .spawn(cleanup_old_logs)
            .ok();
    }
}

/// Log an info message
#[expect(
    clippy::collapsible_if,
    reason = "Logger lock + optional logger branching is intentionally straightforward"
)]
pub fn info(message: &str) {
    if let Ok(mut guard) = LOGGER.lock() {
        if let Some(logger) = guard.as_mut() {
            logger.write("INFO", message);
        }
    }
}

/// Log an error message
#[expect(
    clippy::collapsible_if,
    reason = "Logger lock + optional logger branching is intentionally straightforward"
)]
pub fn error(message: &str) {
    if let Ok(mut guard) = LOGGER.lock() {
        if let Some(logger) = guard.as_mut() {
            logger.write("ERROR", message);
        }
    }
}

/// Log a warning message
#[expect(
    clippy::collapsible_if,
    reason = "Logger lock + optional logger branching is intentionally straightforward"
)]
pub fn warn(message: &str) {
    if let Ok(mut guard) = LOGGER.lock() {
        if let Some(logger) = guard.as_mut() {
            logger.write("WARN", message);
        }
    }
}

/// Truncate a value for inclusion in a log line, appending an ellipsis marker
/// with the original length when cut. Char-boundary safe. Use this for
/// payload-bearing values (event debug dumps, request bodies) so a routine
/// warning cannot flood the log with a multi-kilobyte line.
pub fn truncate_for_log(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let truncated: String = value.chars().take(max_chars).collect();
    format!("{}… [{} chars total]", truncated, value.chars().count())
}

/// Log a debug message (only if HORON_AGENT_TRACE is set)
#[expect(
    clippy::collapsible_if,
    reason = "Debug logging keeps env gating and logger access explicit"
)]
pub fn debug(message: &str) {
    if std::env::var("HORON_AGENT_TRACE").is_ok() {
        if let Ok(mut guard) = LOGGER.lock() {
            if let Some(logger) = guard.as_mut() {
                logger.write("DEBUG", message);
            }
        }
    }
}

pub fn event_info<K, V, I>(event_name: &str, fields: I)
where
    K: AsRef<str>,
    V: Display,
    I: IntoIterator<Item = (K, V)>,
{
    event(LogLevel::Info, event_name, fields);
}

pub fn event_warn<K, V, I>(event_name: &str, fields: I)
where
    K: AsRef<str>,
    V: Display,
    I: IntoIterator<Item = (K, V)>,
{
    event(LogLevel::Warn, event_name, fields);
}

pub fn event_error<K, V, I>(event_name: &str, fields: I)
where
    K: AsRef<str>,
    V: Display,
    I: IntoIterator<Item = (K, V)>,
{
    event(LogLevel::Error, event_name, fields);
}

pub fn event_debug<K, V, I>(event_name: &str, fields: I)
where
    K: AsRef<str>,
    V: Display,
    I: IntoIterator<Item = (K, V)>,
{
    event(LogLevel::Debug, event_name, fields);
}

pub fn event<K, V, I>(level: LogLevel, event: &str, fields: I)
where
    K: AsRef<str>,
    V: Display,
    I: IntoIterator<Item = (K, V)>,
{
    if !level.is_enabled() {
        return;
    }
    write_event(level, event, fields);
}

pub fn event_rate_limited<K, V, I>(level: LogLevel, rate_key: &str, min_interval: Duration, event: &str, fields: I)
where
    K: AsRef<str>,
    V: Display,
    I: IntoIterator<Item = (K, V)>,
{
    if !level.is_enabled() {
        return;
    }

    let now = Instant::now();
    let store = RATE_LIMITS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut suppressed = 0;
    let should_emit = match store.lock() {
        Ok(mut guard) => match guard.get_mut(rate_key) {
            Some(state) if now.duration_since(state.last_emit) < min_interval => {
                state.suppressed = state.suppressed.saturating_add(1);
                false
            }
            Some(state) => {
                suppressed = state.suppressed;
                state.suppressed = 0;
                state.last_emit = now;
                true
            }
            None => {
                guard.insert(
                    rate_key.to_string(),
                    RateLimitState {
                        last_emit: now,
                        suppressed: 0,
                    },
                );
                true
            }
        },
        Err(_) => true,
    };

    if !should_emit {
        return;
    }

    let mut fields: Vec<(String, String)> = fields
        .into_iter()
        .map(|(key, value)| (key.as_ref().to_string(), value.to_string()))
        .collect();
    if suppressed > 0 {
        fields.push(("suppressed".to_string(), suppressed.to_string()));
    }
    write_event(level, event, fields);
}

fn write_level(level: LogLevel, message: &str) {
    if let Ok(mut guard) = LOGGER.lock()
        && let Some(logger) = guard.as_mut()
    {
        logger.write(level.as_str(), message);
    }
}

/// Assemble one structured line in this thread's kept buffers and write it.
fn write_event<K, V, I>(level: LogLevel, event: &str, fields: I)
where
    K: AsRef<str>,
    V: Display,
    I: IntoIterator<Item = (K, V)>,
{
    event_buffer::with_event_buffers(|buffers| {
        buffers.compose(structured_json_enabled(), event, fields);
        write_level(level, buffers.message());
    });
}

#[cfg(test)]
fn format_structured_event<K, V, I>(event: &str, fields: I) -> String
where
    K: AsRef<str>,
    V: Display,
    I: IntoIterator<Item = (K, V)>,
{
    format_structured_event_as(structured_json_enabled(), event, fields)
}

/// The line [`write_event`] would write, as JSON or as `key=value` text.
#[cfg(test)]
fn format_structured_event_as<K, V, I>(json: bool, event: &str, fields: I) -> String
where
    K: AsRef<str>,
    V: Display,
    I: IntoIterator<Item = (K, V)>,
{
    event_buffer::with_event_buffers(|buffers| {
        buffers.compose(json, event, fields);
        buffers.take_message()
    })
}

fn structured_json_enabled() -> bool {
    matches!(
        std::env::var("HORON_AGENT_LOG_JSON").as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn sanitize_log_key(key: &str) -> Cow<'_, str> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.';
    if !key.is_empty() && key.len() <= MAX_LOG_KEY_BYTES && key.chars().all(allowed) {
        return Cow::Borrowed(key);
    }
    let sanitized: String = key.chars().map(|c| if allowed(c) { c } else { '_' }).collect();
    Cow::Owned(if sanitized.is_empty() {
        "field".to_string()
    } else {
        truncate(&sanitized, MAX_LOG_KEY_BYTES)
    })
}

/// Append a value, JSON-quoted when it is empty or holds whitespace, `=` or a
/// quote.
fn push_log_field_value(line: &mut String, value: &str) {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '=' || c == '"' || c == '\'')
    {
        line.push_str(&serde_json::to_string(value).unwrap_or_else(|_| "\"<unserializable>\"".to_string()));
    } else {
        line.push_str(value);
    }
}

/// Log a structured auth event with conservative redaction.
///
/// Callers should pass only non-secret metadata. This function still redacts any
/// field whose key looks credential-like so accidental tokens/keys do not land in
/// logs.
pub fn auth_event(event: &str, provider: &str, fields: &[(&str, &str)]) {
    let mut parts = vec![
        format!("event={}", sanitize_log_value(event)),
        format!("provider={}", sanitize_log_value(provider)),
    ];
    for (key, value) in fields {
        parts.push(format!("{}={}", sanitize_log_value(key), redact_auth_field(key, value)));
    }
    let msg = format!("AUTH {}", parts.join(" "));
    if let Ok(mut guard) = LOGGER.lock()
        && let Some(logger) = guard.as_mut()
    {
        logger.write("AUTH", &msg);
    }
}

/// Log a tool call
#[expect(
    clippy::collapsible_if,
    reason = "Logger lock + optional logger branching is intentionally straightforward"
)]
pub fn tool_call(name: &str, input: &str, output: &str) {
    let msg = format!(
        "TOOL[{}] input={} output={}",
        name,
        truncate(input, 200),
        truncate(output, 500)
    );
    if let Ok(mut guard) = LOGGER.lock() {
        if let Some(logger) = guard.as_mut() {
            logger.write("TOOL", &msg);
        }
    }
}

/// Log a crash/panic for auto-debug
#[expect(
    clippy::collapsible_if,
    reason = "Logger lock + optional logger branching is intentionally straightforward"
)]
pub fn crash(error: &str, context: &str) {
    let msg = format!("CRASH: {} | Context: {}", error, context);
    if let Ok(mut guard) = LOGGER.lock() {
        if let Some(logger) = guard.as_mut() {
            logger.write("CRASH", &msg);
        }
    }
}

/// Get the session ID from the current logging context (thread-local or task-local).
pub fn current_session() -> Option<String> {
    if let Some(ctx) = task_context_snapshot() {
        return ctx.session;
    }
    LOG_CONTEXT.with(|c| c.borrow().session.clone())
}

/// Get path to today's log file
pub fn log_path() -> Option<PathBuf> {
    let log_dir = log_dir()?;
    let date = Local::now().format("%Y-%m-%d");
    Some(log_dir.join(format!("horon-agent-{}.log", date)))
}

/// Remove daily `horon-agent-*.log` / `horon-agent-desktop-*.log` files older than 7 days.
///
/// Scoped deliberately to the date-stamped log files this logger produces. The
/// log directory also holds non-log data (e.g. `memory/`, `memory-events-*.jsonl`)
/// that must NOT be touched here, so we never blanket-delete by mtime.
pub fn cleanup_old_logs() {
    if let Some(log_dir) = log_dir() {
        cleanup_old_logs_in(&log_dir, Local::now());
    }
}

/// Core of [`cleanup_old_logs`], parameterized on the directory and "now" so it
/// can be unit-tested without touching the real log directory or process env.
fn cleanup_old_logs_in(log_dir: &std::path::Path, now: chrono::DateTime<Local>) {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };
    let cutoff = now - chrono::Duration::days(7);
    for entry in entries.flatten() {
        // Only consider our own date-stamped log files.
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_horon_agent_log =
            (name.starts_with("horon-agent-") || name.starts_with("horon-agent-desktop-")) && name.ends_with(".log");
        if !is_horon_agent_log {
            continue;
        }

        if let Ok(metadata) = entry.metadata()
            && metadata.is_file()
            && let Ok(modified) = metadata.modified()
        {
            let modified: chrono::DateTime<Local> = modified.into();
            if modified < cutoff
                && let Err(err) = fs::remove_file(entry.path())
            {
                eprintln!("horon-agent logger cleanup failed: {err}");
            }
        }
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", horon_agent_core::util::truncate_str(s, max_len))
    } else {
        s.to_string()
    }
}

/// Whether a field name looks credential-like, compared without regard to
/// ASCII case.
fn is_credential_key(key: &str) -> bool {
    const MARKERS: [&str; 10] = [
        "token",
        "secret",
        "key",
        "credential",
        "callback",
        "authorization",
        "auth_code",
        "oauth_code",
        "code_verifier",
        "code_challenge",
    ];
    let key = key.as_bytes();
    MARKERS.iter().any(|marker| {
        key.windows(marker.len())
            .any(|window| window.eq_ignore_ascii_case(marker.as_bytes()))
    })
}

fn redact_auth_field(key: &str, value: &str) -> String {
    if is_credential_key(key) {
        return "<redacted>".to_string();
    }
    sanitize_log_value(value)
}

const MAX_LOG_KEY_BYTES: usize = 80;
const MAX_LOG_VALUE_BYTES: usize = 160;

fn sanitize_log_value(value: &str) -> String {
    let value = value.replace(['\n', '\r', '\t'], " ");
    let value = redact_url_queries(&value);
    truncate(&value, MAX_LOG_VALUE_BYTES)
}

fn redact_url_queries(value: &str) -> String {
    value
        .split(' ')
        .map(|word| {
            if (word.starts_with("http://") || word.starts_with("https://")) && word.contains('?') {
                let (base, _) = word.split_once('?').unwrap_or((word, ""));
                format!("{}?<redacted>", base)
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicting_task_contexts_keeps_most_recent_half() {
        let base = Instant::now();
        let mut contexts: HashMap<String, TaskLogContext> = (0..10)
            .map(|i| {
                (
                    i.to_string(),
                    TaskLogContext {
                        context: LogContext::default(),
                        last_used: base + Duration::from_millis(i),
                    },
                )
            })
            .collect();
        evict_stale_task_contexts(&mut contexts);
        assert!(contexts.len() < 10 && !contexts.is_empty());
        assert!(contexts.contains_key("9"));
        assert!(!contexts.contains_key("0"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finished_task_contexts_do_not_accumulate() {
        for i in 0..(MAX_TASK_LOG_CONTEXTS * 3) {
            tokio::spawn(async move { set_session(&format!("s{i}")) })
                .await
                .unwrap();
        }
        let len = TASK_LOG_CONTEXTS.get().unwrap().lock().unwrap().len();
        assert!(len <= MAX_TASK_LOG_CONTEXTS, "{len}");
    }

    #[test]
    fn auth_log_redacts_secret_like_fields() {
        assert_eq!(redact_auth_field("api_key", "sk-secret"), "<redacted>");
        assert_eq!(
            redact_auth_field("callback_url", "https://example.com/?code=secret"),
            "<redacted>"
        );
    }

    #[test]
    fn auth_log_sanitizes_urls_and_control_characters() {
        assert_eq!(
            sanitize_log_value("failed\nhttps://login.example.com/cb?code=secret&state=abc"),
            "failed https://login.example.com/cb?<redacted>"
        );
    }

    #[test]
    fn structured_event_orders_and_redacts_fields() {
        let line = format_structured_event(
            "server_request",
            vec![("z", "last"), ("api_key", "sk-secret"), ("a field", "hello world")],
        );
        assert_eq!(
            line,
            "EVENT event=server_request a_field=\"hello world\" api_key=<redacted> z=last"
        );
    }

    #[test]
    fn structured_event_redacts_url_queries() {
        let line = format_structured_event(
            "callback",
            vec![("url", "https://example.test/cb?code=secret&state=abc")],
        );
        assert_eq!(line, "EVENT event=callback url=https://example.test/cb?<redacted>");
    }

    #[test]
    fn structured_event_keeps_non_secret_code_fields() {
        let line = format_structured_event("tool_done", vec![("exit_code", "127")]);
        assert_eq!(line, "EVENT event=tool_done exit_code=127");
    }

    #[test]
    fn diagnostic_field_names_are_not_redacted() {
        // The model-picker / login diagnostics intentionally avoid field names
        // that collide with the secret-redaction heuristic (which masks any
        // field whose name contains key/token/secret/credential/...). If any of
        // these regress, the uploaded logs we ask users for would show
        // `<redacted>` instead of the boolean/count/env-var-name we need.
        for name in [
            "env_var",
            "input_len",
            "optional",
            "anthropic_api",
            "openai_api",
            "azure_api",
            "copilot_cred",
            "session_provider",
            "routes_in",
            "by_provider",
            "requested_model",
            "route_provider",
        ] {
            assert_eq!(
                redact_auth_field(name, "value"),
                "value",
                "diagnostic field `{name}` should not be redacted",
            );
        }
    }

    #[test]
    fn cleanup_removes_only_old_horon_agent_logs() {
        use std::time::{Duration, SystemTime};

        let dir = std::env::temp_dir().join(format!(
            "horon-agent-log-cleanup-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("create temp log dir");

        let old_mtime = SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 30); // 30 days

        let write = |name: &str, age_old: bool| {
            let path = dir.join(name);
            let mut f = File::create(&path).expect("create file");
            f.write_all(b"x").ok();
            if age_old {
                f.set_modified(old_mtime).expect("set mtime");
            }
            path
        };

        // Old log files that SHOULD be deleted.
        let old_log = write("horon-agent-2000-01-01.log", true);
        let old_desktop = write("horon-agent-desktop-2000-01-01.log", true);
        // Recent log file that SHOULD survive.
        let new_log = write("horon-agent-2099-01-01.log", false);
        // Non-log data that SHOULD survive even though it is old.
        let old_memory = write("memory-events-2000-01-01.jsonl", true);
        let old_other = write("notes-2000-01-01.txt", true);
        // A subdirectory (e.g. `memory/`) must never be removed.
        let subdir = dir.join("memory");
        fs::create_dir_all(&subdir).expect("create subdir");

        cleanup_old_logs_in(&dir, Local::now());

        assert!(!old_log.exists(), "old horon-agent log should be deleted");
        assert!(!old_desktop.exists(), "old desktop log should be deleted");
        assert!(new_log.exists(), "recent horon-agent log must survive");
        assert!(old_memory.exists(), "memory-events jsonl must survive");
        assert!(old_other.exists(), "unrelated files must survive");
        assert!(subdir.is_dir(), "subdirectories must survive");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn timestamp_text_matches_the_chrono_format() {
        let cases = [
            (2026, 11, 16, 9, 5, 7, 4_000_000_u32),
            (1999, 12, 31, 23, 59, 59, 999_999_999),
            (987, 1, 2, 0, 0, 0, 0),
            // A leap second: the 60th second is carried in the nanoseconds.
            (2016, 12, 31, 23, 59, 59, 1_500_000_000),
            (12345, 6, 7, 8, 9, 10, 11_000_000),
            (-44, 3, 15, 12, 0, 0, 0),
        ];
        for (year, month, day, hour, minute, second, nano) in cases {
            let at = chrono::NaiveDate::from_ymd_opt(year, month, day)
                .and_then(|date| date.and_hms_nano_opt(hour, minute, second, nano))
                .expect("a valid date and time");
            let mut line = String::new();
            push_timestamp(&mut line, &at);
            assert_eq!(line, at.format(TIMESTAMP_FORMAT).to_string());
        }
    }

    #[test]
    fn context_prefix_text_keeps_its_shape() {
        let mut line = String::from("x");
        push_context_prefix_for(&mut line, &LogContext::default());
        assert_eq!(line, "x");

        let full = LogContext {
            server: Some("main".to_string()),
            session: Some("session-0123456789abcdef".to_string()),
            provider: Some("openai".to_string()),
            model: Some("gpt-5-mini".to_string()),
        };
        push_context_prefix_for(&mut line, &full);
        assert_eq!(line, "x[srv:main|ses:session-0123456789ab|prv:openai|mod:gpt] ");

        let mut line = String::new();
        let partial = LogContext {
            session: Some("short".to_string()),
            ..LogContext::default()
        };
        push_context_prefix_for(&mut line, &partial);
        assert_eq!(line, "[ses:short] ");
    }
}

#[cfg(test)]
#[path = "structured_line_tests.rs"]
mod structured_line_tests;
