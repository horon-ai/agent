//! Child-agent results, addressed as `agent://`.
//!
//! The in-memory cache is a `LazyLock` in this process, so on its own it never
//! crosses a process boundary. A completed child therefore also writes a durable
//! copy under the owning session's store directory, and a read that misses the
//! cache falls back to it: a daemon restart no longer loses a result the parent
//! still needs. The child's own event log stays authoritative; the stored result
//! is the projection rendered here at completion.
//!
//! Every URL component is data: it is percent-decoded and matched against the
//! cached JSON, never joined onto a filesystem path. The durable copy is looked
//! up by a re-validated component under a fixed directory, so `agent://` still
//! cannot escape the working directory or address a real file.

use crate::message::redact_secrets;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};

pub(crate) const SCHEME: &str = "agent://";

pub(crate) const MAX_RESULTS: usize = 64;

pub(crate) const MAX_RESULT_BYTES: usize = 1024 * 1024;

pub(crate) const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;

/// Directory of durable results, under the owning session's store directory.
const STORED_DIR: &str = "child-results";

/// Evicted ids are remembered so a stale read reports eviction, not "unknown".
const MAX_TOMBSTONES: usize = 256;

/// A failure that is part of the `agent://` contract rather than an I/O error.
///
/// None of these ever resolves to `null` or to another child's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentUrlError {
    RangeNotApplicable,
    UnknownChild {
        child_id: String,
    },
    Evicted {
        child_id: String,
    },
    ChildNotCompleted {
        child_id: String,
        status: String,
    },
    ChildFailed {
        child_id: String,
        status: String,
        message: String,
    },
    ResultTooLarge {
        child_id: String,
        bytes: usize,
    },
    NoStructuredDetails {
        child_id: String,
    },
    NotProjectable {
        path: String,
    },
    FieldMissing {
        path: String,
    },
    IndexOutOfRange {
        path: String,
        index: usize,
        length: usize,
    },
    InvalidComponent {
        component: String,
    },
    InvalidIndex {
        component: String,
    },
}

impl fmt::Display for AgentUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RangeNotApplicable => write!(
                f,
                "agent:// results are not files: offset, limit, start_line, end_line, and hashline do not apply"
            ),
            Self::UnknownChild { child_id } => {
                write!(f, "no child result is cached for `{child_id}` in this process")
            }
            Self::Evicted { child_id } => write!(
                f,
                "the cached result for `{child_id}` was evicted by the retention bound ({MAX_RESULTS} results, {MAX_RESULT_BYTES} bytes each, {MAX_TOTAL_BYTES} bytes total); it is not available any more"
            ),
            Self::ChildNotCompleted { child_id, status } => write!(
                f,
                "child `{child_id}` is {status}, not completed, so it has no final result to project"
            ),
            Self::ChildFailed {
                child_id,
                status,
                message,
            } => write!(f, "child `{child_id}` ended with status {status}: {message}"),
            Self::ResultTooLarge { child_id, bytes } => write!(
                f,
                "the result for `{child_id}` is {bytes} bytes, over the {MAX_RESULT_BYTES}-byte per-result bound, so it was not retained"
            ),
            Self::NoStructuredDetails { child_id } => write!(
                f,
                "child `{child_id}` completed with plain text, so it has no structured field to project"
            ),
            Self::NotProjectable { path } => {
                write!(f, "`{path}` addresses into a value that is not an object or array")
            }
            Self::FieldMissing { path } => write!(f, "no field named `{path}` in the child result"),
            Self::IndexOutOfRange { path, index, length } => write!(
                f,
                "index {index} in `{path}` is out of range for an array of length {length}"
            ),
            Self::InvalidComponent { component } => {
                write!(f, "`{component}` is not a valid agent:// path component")
            }
            Self::InvalidIndex { component } => write!(
                f,
                "`{component}` is not a decimal array index; an array component must be all digits"
            ),
        }
    }
}

impl std::error::Error for AgentUrlError {}

#[derive(Debug, Clone)]
enum ResultState {
    Running { status: String },
    Completed { text: String, details: Option<Value> },
    Failed { status: String, message: String },
    TooLarge { bytes: usize },
}

impl ResultState {
    fn status_label(&self) -> &str {
        match self {
            Self::Running { status } => status,
            Self::Completed { .. } => "completed",
            Self::Failed { status, .. } => status,
            Self::TooLarge { .. } => "too_large",
        }
    }
}

#[derive(Debug)]
struct Entry {
    parent_session_id: String,
    state: ResultState,
    bytes: usize,
    sequence: u64,
}

#[derive(Debug, Default)]
struct Store {
    entries: HashMap<String, Entry>,
    tombstones: VecDeque<String>,
    total_bytes: usize,
    sequence: u64,
}

impl Store {
    fn insert(&mut self, child_id: &str, parent_session_id: &str, state: ResultState, bytes: usize) {
        if let Some(previous) = self.entries.remove(child_id) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.bytes);
        }
        while !self.entries.is_empty()
            && (self.entries.len() + 1 > MAX_RESULTS || self.total_bytes + bytes > MAX_TOTAL_BYTES)
        {
            self.evict_oldest();
        }
        self.sequence += 1;
        self.total_bytes += bytes;
        self.tombstones.retain(|id| id != child_id);
        self.entries.insert(
            child_id.to_string(),
            Entry {
                parent_session_id: parent_session_id.to_string(),
                state,
                bytes,
                sequence: self.sequence,
            },
        );
    }

    fn evict_oldest(&mut self) {
        let Some(oldest_id) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.sequence)
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        if let Some(entry) = self.entries.remove(&oldest_id) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
        }
        if self.tombstones.len() >= MAX_TOMBSTONES {
            self.tombstones.pop_front();
        }
        self.tombstones.push_back(oldest_id);
    }

    fn state_of(&self, child_id: &str) -> Result<&ResultState, AgentUrlError> {
        if let Some(entry) = self.entries.get(child_id) {
            return Ok(&entry.state);
        }
        if self.tombstones.iter().any(|id| id == child_id) {
            return Err(AgentUrlError::Evicted {
                child_id: child_id.to_string(),
            });
        }
        Err(AgentUrlError::UnknownChild {
            child_id: child_id.to_string(),
        })
    }

    fn children_of(&self, parent_session_id: &str) -> Vec<(&str, &str)> {
        let mut children: Vec<(&str, &str)> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.parent_session_id == parent_session_id)
            .map(|(id, entry)| (id.as_str(), entry.state.status_label()))
            .collect();
        children.sort_unstable();
        children
    }
}

fn store() -> &'static Mutex<Store> {
    static STORE: LazyLock<Mutex<Store>> = LazyLock::new(|| Mutex::new(Store::default()));
    &STORE
}

fn lock() -> MutexGuard<'static, Store> {
    store().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A component that can name a file under [`STORED_DIR`].
///
/// The id arrives from a URL, so it is checked again here: a decoded `/`, `..`
/// or separator never reaches a path join.
fn is_path_safe(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// The store directory of the session that owns the durable results.
///
/// A missing store or an unusable id is an I/O error, not an empty answer: the
/// caller decides whether to report it or to treat the copy as absent.
fn session_dir(parent_session_id: &str) -> std::io::Result<PathBuf> {
    if !is_path_safe(parent_session_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the owning session id is not a safe path component",
        ));
    }
    let paths = crate::session::store_paths().map_err(|error| std::io::Error::other(error.to_string()))?;
    let parent = horon_agent_session_store::SessionId::parse(parent_session_id)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string()))?;
    paths.session_dir(&parent).map_err(std::io::Error::other)
}

/// The `child-results/` directory of one session.
fn results_dir(parent_session_id: &str) -> std::io::Result<PathBuf> {
    Ok(session_dir(parent_session_id)?.join(STORED_DIR))
}

/// The text and optional details paths of one child's durable result.
fn stored_paths(parent_session_id: &str, child_id: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    if !is_path_safe(child_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the child id is not a safe path component",
        ));
    }
    let dir = results_dir(parent_session_id)?;
    Ok((dir.join(format!("{child_id}.md")), dir.join(format!("{child_id}.json"))))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)
}

/// Write the durable copy of a completed child's result.
///
/// The bytes are redacted before they reach the disk, so a secret a child echoed
/// is no more readable from the file than from the cache. The caller logs a
/// failure: the in-memory result stays valid either way.
pub(crate) fn persist_completed(
    child_id: &str,
    parent_session_id: &str,
    text: &str,
    details: Option<&Value>,
) -> std::io::Result<()> {
    let owner_dir = session_dir(parent_session_id)?;
    if !owner_dir.exists() {
        // The session itself is not stored, so it owns nowhere to keep a copy.
        // This is what keeps an in-memory fixture from writing into a real store.
        return Ok(());
    }
    let (text_path, details_path) = stored_paths(parent_session_id, child_id)?;
    std::fs::create_dir_all(owner_dir.join(STORED_DIR))?;
    write_atomically(&text_path, redact_secrets(text).as_bytes())?;
    match details.map(|value| redact_secrets(&value.to_string())) {
        Some(json) => write_atomically(&details_path, json.as_bytes()),
        None => match std::fs::remove_file(&details_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
    }
}

/// The durable copy of one child's result, when this session has one.
///
/// A copy that cannot be read is absent rather than an error: the read path
/// already has a cache miss to report, and a damaged file must not replace that
/// report with an I/O failure.
fn load_stored(parent_session_id: &str, child_id: &str) -> Option<ResultState> {
    let Ok((text_path, details_path)) = stored_paths(parent_session_id, child_id) else {
        return None;
    };
    let text = match std::fs::read_to_string(text_path) {
        Ok(text) => text,
        Err(_) => return None,
    };
    let details = match std::fs::read_to_string(details_path) {
        Ok(json) => Result::ok(serde_json::from_str::<Value>(&json)),
        Err(_) => None,
    };
    Some(ResultState::Completed { text, details })
}

/// Child ids this session wrote a durable result for.
fn stored_children(parent_session_id: &str) -> Vec<String> {
    let Ok(dir) = results_dir(parent_session_id) else {
        return Vec::new();
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "md") {
                path.file_stem().and_then(|stem| stem.to_str()).map(str::to_string)
            } else {
                None
            }
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub(crate) fn is_agent_url(path: &str) -> bool {
    path.get(..SCHEME.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(SCHEME))
}

pub(crate) fn strip_scheme(path: &str) -> Option<&str> {
    is_agent_url(path).then(|| &path[SCHEME.len()..])
}

/// Only objects and arrays qualify: a bare scalar has no field to project.
pub(crate) fn structured_details_from_text(text: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(text) {
        Ok(value @ (Value::Object(_) | Value::Array(_))) => Some(value),
        _ => None,
    }
}

pub(crate) fn publish_running(child_id: &str, parent_session_id: &str, status: &str) {
    let mut store = lock();
    store.insert(
        child_id,
        parent_session_id,
        ResultState::Running {
            status: status.to_string(),
        },
        0,
    );
}

/// An oversized result still occupies a slot and reads back as a typed error
/// instead of vanishing into "unknown".
pub(crate) fn publish_completed(
    child_id: &str,
    parent_session_id: &str,
    text: &str,
    details: Option<Value>,
) -> Result<(), AgentUrlError> {
    let details_bytes = details.as_ref().map(|value| value.to_string().len()).unwrap_or(0);
    let bytes = text.len() + details_bytes;
    let mut store = lock();
    if bytes > MAX_RESULT_BYTES {
        store.insert(child_id, parent_session_id, ResultState::TooLarge { bytes }, 0);
        return Err(AgentUrlError::ResultTooLarge {
            child_id: child_id.to_string(),
            bytes,
        });
    }
    if let Err(error) = persist_completed(child_id, parent_session_id, text, details.as_ref()) {
        crate::logging::warn(&format!(
            "[agent://] the durable copy of `{child_id}` was not written: {error}"
        ));
    }
    store.insert(
        child_id,
        parent_session_id,
        ResultState::Completed {
            text: text.to_string(),
            details,
        },
        bytes,
    );
    Ok(())
}

pub(crate) fn publish_failed(child_id: &str, parent_session_id: &str, status: &str, message: &str) {
    // The failure text is retained and served to readers, so it is charged to the budget like
    // any other retained payload. A message past the per-result bound keeps its slot as a typed
    // error instead, so one huge failure cannot push the total past MAX_TOTAL_BYTES.
    let bytes = status.len() + message.len();
    let mut store = lock();
    if bytes > MAX_RESULT_BYTES {
        store.insert(child_id, parent_session_id, ResultState::TooLarge { bytes }, 0);
        return;
    }
    store.insert(
        child_id,
        parent_session_id,
        ResultState::Failed {
            status: status.to_string(),
            message: message.to_string(),
        },
        bytes,
    );
}

/// Test-only view of the retention ledger: what one child is charged and the running total.
#[cfg(test)]
pub(crate) fn accounted_bytes_for_test(child_id: &str) -> Option<usize> {
    lock().entries.get(child_id).map(|entry| entry.bytes)
}

#[cfg(test)]
pub(crate) fn total_bytes_for_test() -> usize {
    lock().total_bytes
}

/// The returned text has the secret-redaction pass applied, so a token that a
/// child echoed back is never surfaced to the reader.
pub(crate) fn read(rest: &str, parent_session_id: &str) -> anyhow::Result<String> {
    let rendered = render(rest, parent_session_id)?;
    Ok(redact_secrets(&rendered))
}

fn render(rest: &str, parent_session_id: &str) -> Result<String, AgentUrlError> {
    if rest.is_empty() {
        return Ok(render_listing(parent_session_id));
    }

    let mut raw_components = rest.split('/');
    let child_id = decode_component(raw_components.next().unwrap_or_default())?;
    if child_id.is_empty() {
        return Err(AgentUrlError::InvalidComponent {
            component: String::new(),
        });
    }
    let components = raw_components.map(decode_component).collect::<Result<Vec<_>, _>>()?;

    let cached = lock().state_of(&child_id).cloned();
    let state = match cached {
        Ok(state) => state,
        // The durable copy outlives this process and the cache bound, so a miss
        // or an eviction still answers with the result the child published.
        Err(missed @ (AgentUrlError::UnknownChild { .. } | AgentUrlError::Evicted { .. })) => {
            match load_stored(parent_session_id, &child_id) {
                Some(state) => state,
                None => return Err(missed),
            }
        }
        Err(other) => return Err(other),
    };

    match state {
        ResultState::Running { status } => {
            if components.is_empty() {
                Ok(format!(
                    "child `{child_id}` is {status}: no final result is available yet. Read `{SCHEME}{child_id}` again after it completes."
                ))
            } else {
                Err(AgentUrlError::ChildNotCompleted {
                    child_id: child_id.clone(),
                    status,
                })
            }
        }
        ResultState::Completed { text, details } => {
            if components.is_empty() {
                return Ok(text);
            }
            let details = details.ok_or_else(|| AgentUrlError::NoStructuredDetails {
                child_id: child_id.clone(),
            })?;
            project(&details, &components)
        }
        ResultState::Failed { status, message } => Err(AgentUrlError::ChildFailed {
            child_id: child_id.clone(),
            status,
            message,
        }),
        ResultState::TooLarge { bytes } => Err(AgentUrlError::ResultTooLarge {
            child_id: child_id.clone(),
            bytes,
        }),
    }
}

/// The listing names every child the cache holds plus every child with a durable
/// copy the cache already dropped, so a restarted daemon still points at results
/// it can serve.
fn render_listing(parent_session_id: &str) -> String {
    let mut children: Vec<(String, String)> = {
        let store = lock();
        store
            .children_of(parent_session_id)
            .into_iter()
            .map(|(child_id, status)| (child_id.to_string(), status.to_string()))
            .collect()
    };
    for child_id in stored_children(parent_session_id) {
        if !children.iter().any(|(known, _)| known == &child_id) {
            children.push((child_id, "stored".to_string()));
        }
    }
    if children.is_empty() {
        return format!(
            "no child results are cached for this session; a completed child publishes one as `{SCHEME}<child-id>`"
        );
    }
    children.sort();
    let mut output = format!("{} child result(s) for this session:\n", children.len());
    for (child_id, status) in children {
        output.push_str(&format!("  {SCHEME}{child_id}  {status}\n"));
    }
    output.push_str(&format!(
        "Read one with `{SCHEME}<child-id>`, or project a field with `{SCHEME}<child-id>/<field>`.\n"
    ));
    output
}

fn project(details: &Value, components: &[String]) -> Result<String, AgentUrlError> {
    let mut current = details;
    let mut walked = String::new();
    for component in components {
        if !walked.is_empty() {
            walked.push('/');
        }
        walked.push_str(component);
        current = match current {
            Value::Object(map) => map
                .get(component)
                .ok_or_else(|| AgentUrlError::FieldMissing { path: walked.clone() })?,
            Value::Array(items) => {
                let index = parse_index(component)?;
                items.get(index).ok_or_else(|| AgentUrlError::IndexOutOfRange {
                    path: walked.clone(),
                    index,
                    length: items.len(),
                })?
            }
            _ => {
                return Err(AgentUrlError::NotProjectable { path: walked.clone() });
            }
        };
    }
    Ok(match current {
        Value::String(text) => text.clone(),
        Value::Object(_) | Value::Array(_) => {
            serde_json::to_string_pretty(current).unwrap_or_else(|_| current.to_string())
        }
        other => other.to_string(),
    })
}

fn parse_index(component: &str) -> Result<usize, AgentUrlError> {
    if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AgentUrlError::InvalidIndex {
            component: component.to_string(),
        });
    }
    component.parse::<usize>().map_err(|_| AgentUrlError::InvalidIndex {
        component: component.to_string(),
    })
}

/// Decoding happens after the split on `/`, so `%2F` stays inside its component
/// and can never introduce a new one or a filesystem separator.
fn decode_component(component: &str) -> Result<String, AgentUrlError> {
    if !component.contains('%') {
        return Ok(component.to_string());
    }
    let bytes = component.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let Some(high) = bytes.get(index + 1).and_then(|byte| hex_value(*byte)) else {
            return Err(AgentUrlError::InvalidComponent {
                component: component.to_string(),
            });
        };
        let Some(low) = bytes.get(index + 2).and_then(|byte| hex_value(*byte)) else {
            return Err(AgentUrlError::InvalidComponent {
                component: component.to_string(),
            });
        };
        decoded.push(high * 16 + low);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| AgentUrlError::InvalidComponent {
        component: component.to_string(),
    })
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    *lock() = Store::default();
}

#[cfg(test)]
mod tests;
