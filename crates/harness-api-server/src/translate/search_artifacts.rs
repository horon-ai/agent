//! `search_sessions` and `list_artifacts`: two read-only surfaces that answer
//! from stored session records instead of the attached daemon connection.
//!
//! Neither surface talks to the daemon. A client showing several conversations
//! at once must be able to search them and list what they produced without
//! attaching to each in turn, which would disturb the very sessions it is
//! looking at. Both therefore read the same files the daemon persists, and both
//! are bounded: a search is capped in hits and sessions scanned by the existing
//! engine, and an artifact scan is capped in directory entries and results.
//!
//! No file contents are read here. `list_artifacts` returns metadata only, so a
//! conversation that wrote a 40 MB HTML report costs one `stat` per candidate,
//! and the client fetches the bytes with `read_file` when the user opens one.

use super::{BridgeState, Outbound};
use horon_agent_harness_api::{ApiEvent, ArtifactEntry, ArtifactKind, ErrorCode, ServerFrame, SessionSearchHit};
use horon_agent_session_store as store;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Hits a search returns when the request does not say. Enough to fill a
/// results list without turning a keystroke into a transcript scan of
/// everything on disk.
const DEFAULT_SEARCH_LIMIT: u32 = 20;
/// Hard ceiling on returned hits. The engine's own limit is the same number;
/// repeating it here keeps the reply bounded even if that default changes.
const MAX_SEARCH_LIMIT: u32 = 50;

/// Artifacts a conversation may report. A panel lists a few dozen; a build
/// directory with a thousand screenshots must not become a thousand-row reply.
const MAX_ARTIFACT_RESULTS: usize = 200;
/// Candidate paths taken from the transcript before any of them are statted.
const MAX_TRANSCRIPT_CANDIDATES: usize = 2_000;
/// Directory entries the working-directory scan may visit.
const MAX_SCAN_ENTRIES: usize = 50_000;
/// Files the working-directory scan may consider, before extension and mtime
/// filtering. Mirrors `find_files`, which faces the same project sizes.
const MAX_SCAN_FILES: usize = 20_000;

/// Extensions a conversation artifact may have, with the kind each maps to.
const ARTIFACT_EXTENSIONS: &[(&str, ArtifactKind)] = &[
    ("html", ArtifactKind::Html),
    ("png", ArtifactKind::Image),
    ("jpg", ArtifactKind::Image),
    ("jpeg", ArtifactKind::Image),
    ("webp", ArtifactKind::Image),
    ("gif", ArtifactKind::Image),
];

/// Directories never worth walking for conversation output.
const SKIPPED_DIRS: &[&str] = &[".git", "node_modules", "target", ".next"];

/// One stored transcript, reduced to what artifact discovery needs.
struct Transcript {
    /// Session start, epoch milliseconds. Absent only on records predating it.
    created_ms: Option<u64>,
    working_dir: Option<String>,
    /// Stored messages as JSON, so the canonical and legacy layouts share one
    /// scanner instead of two subtly different typed walkers.
    messages: Vec<Value>,
}

/// A JSON string field, or the empty string when it is absent or not a string.
///
/// Written out rather than `unwrap_or_default()` because this crate's guardrails
/// forbid that pattern in new production code: it hides whether absence was
/// expected.
#[allow(clippy::manual_unwrap_or_default)]
fn string_field<'a>(request: &'a Value, key: &str) -> &'a str {
    match request[key].as_str() {
        Some(value) => value,
        None => "",
    }
}

pub(crate) fn search_sessions(api_id: u64, request: &Value) -> Vec<Outbound> {
    let query = string_field(request, "query");
    if query.trim().is_empty() {
        return BridgeState::error_reply(api_id, ErrorCode::InvalidRequest, "query must not be empty");
    }
    let limit = request["limit"]
        .as_u64()
        .map(|limit| limit.min(u64::from(MAX_SEARCH_LIMIT)) as usize)
        .unwrap_or(DEFAULT_SEARCH_LIMIT as usize);
    if limit == 0 {
        return reply_search(api_id, query, Vec::new());
    }
    let Some(home) = BridgeState::horon_agent_home() else {
        return BridgeState::error_reply(api_id, ErrorCode::Internal, "could not resolve horon-agent home");
    };
    let sessions_dir = home.join("sessions");
    if !sessions_dir.is_dir() {
        return reply_search(api_id, query, Vec::new());
    }

    // The agent's own engine, not a second one: the same index, the same
    // scoring, and therefore the same answer whether the user searched from
    // the agent or from a client. An empty log id keeps the engine's warning
    // line from naming a session this connection is not attached to.
    let profile = horon_agent_session_types::SessionSearchQueryProfile::new(query);
    let options = horon_agent_app_core::tool::SearchOptions::for_harness_api(limit);
    let report =
        match horon_agent_app_core::tool::search_sessions_blocking(&sessions_dir, &profile, &options, "harness-api") {
            Ok(report) => report,
            Err(error) => {
                return BridgeState::error_reply(api_id, ErrorCode::Internal, &format!("search failed: {error}"));
            }
        };

    let results = report
        .results
        .into_iter()
        .take(limit)
        .map(|hit| SessionSearchHit {
            session_id: hit.session_id,
            title: hit.title,
            role: hit.role,
            preview: hit.snippet,
            at: hit.message_timestamp.unwrap_or(hit.updated_at).to_rfc3339(),
        })
        .collect();
    reply_search(api_id, query, results)
}

/// Route one of the two stored-record requests to its worker.
pub(crate) fn handle(req: &str, api_id: u64, request: &Value) -> Vec<Outbound> {
    match req {
        "search_sessions" => search_sessions(api_id, request),
        _ => list_artifacts(api_id, request),
    }
}

fn reply_search(api_id: u64, query: &str, results: Vec<SessionSearchHit>) -> Vec<Outbound> {
    vec![Outbound::Reply(ServerFrame::reply(
        api_id,
        ApiEvent::SessionSearchResults {
            query: query.to_string(),
            results,
        },
    ))]
}

pub(crate) fn list_artifacts(api_id: u64, request: &Value) -> Vec<Outbound> {
    let session_id = string_field(request, "session_id");
    if session_id.is_empty() {
        return BridgeState::error_reply(api_id, ErrorCode::InvalidRequest, "session_id must not be empty");
    }
    // A session id is interpolated into a path, so `session_original_path`
    // rejects anything that is not a bare id. A rejected id and a missing
    // record are the same answer to a client: there is no such session.
    let Some(record) = BridgeState::session_original_path(session_id) else {
        return unknown_session(api_id);
    };
    if !record.is_file() {
        return unknown_session(api_id);
    }
    let transcript = match read_transcript(session_id) {
        Ok(transcript) => transcript,
        Err((code, message)) => return BridgeState::error_reply(api_id, code, &message),
    };
    let Some(working_dir) = transcript.working_dir.as_deref() else {
        return BridgeState::error_reply(api_id, ErrorCode::InvalidRequest, "session has no working directory");
    };
    let root = match std::fs::canonicalize(working_dir) {
        Ok(root) => root,
        Err(error) => {
            return BridgeState::error_reply(
                api_id,
                ErrorCode::InvalidRequest,
                &format!("working directory is unavailable: {error}"),
            );
        }
    };
    // A session with no recorded start cannot claim "written since it began",
    // so its record's own timestamp stands in rather than listing everything.
    let started_ms = transcript.created_ms.or_else(|| file_mtime_ms(&record)).unwrap_or(0);

    let mut found: BTreeMap<String, (ArtifactKind, u64, u64)> = BTreeMap::new();
    for candidate in transcript_candidates(&transcript, MAX_TRANSCRIPT_CANDIDATES) {
        let Some(relative) = relative_candidate(&root, &candidate) else {
            continue;
        };
        let Some(kind) = artifact_kind(&relative) else {
            continue;
        };
        // `safe_session_path` re-checks the escape rules per candidate: a
        // transcript may name `../../.ssh/id_rsa` or a symlink that leaves the
        // project, and neither may appear in the list.
        let Ok(path) = BridgeState::safe_session_path(session_id, &relative) else {
            continue;
        };
        let Ok(meta) = path.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(mtime) = modified_ms(&meta) else {
            continue;
        };
        if mtime < started_ms {
            continue;
        }
        found.insert(relative, (kind, meta.len(), mtime));
    }
    for (relative, (kind, bytes, mtime)) in scan_working_dir(&root, started_ms) {
        found.entry(relative).or_insert((kind, bytes, mtime));
    }

    let mut artifacts: Vec<ArtifactEntry> = found
        .into_iter()
        .map(|(path, (kind, bytes, mtime_ms))| ArtifactEntry {
            path,
            kind,
            bytes,
            mtime_ms,
        })
        .collect();
    // Newest first: the report the agent just wrote is what the panel should
    // show, not whatever happens to sort first alphabetically.
    artifacts.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms).then_with(|| a.path.cmp(&b.path)));
    artifacts.truncate(MAX_ARTIFACT_RESULTS);
    vec![Outbound::Reply(ServerFrame::reply(
        api_id,
        ApiEvent::Artifacts {
            session_id: session_id.to_string(),
            artifacts,
        },
    ))]
}

fn unknown_session(api_id: u64) -> Vec<Outbound> {
    BridgeState::error_reply(api_id, ErrorCode::UnknownSession, "session does not exist")
}

/// Read one session's stored transcript without writing anything.
///
/// The canonical event log is replayed read-only. The preserved legacy layout
/// is parsed as JSON rather than loaded through `Session::load`, because that
/// path migrates a legacy record into the event layout: listing artifacts must
/// not rewrite the user's session store.
fn read_transcript(session_id: &str) -> Result<Transcript, (ErrorCode, String)> {
    if BridgeState::canonical_layout(session_id) {
        return read_canonical_transcript(session_id);
    }
    read_legacy_transcript(session_id)
}

#[allow(clippy::manual_ok_err)]
fn read_canonical_transcript(session_id: &str) -> Result<Transcript, (ErrorCode, String)> {
    let Some(paths) = store_paths() else {
        return Err((ErrorCode::Internal, "could not resolve horon-agent home".into()));
    };
    let id = store::SessionId::parse(session_id).map_err(|error| (ErrorCode::InvalidRequest, error.message))?;
    let graph = store::EventGraph::open(&paths, &id).map_err(|error| (ErrorCode::Internal, error.message))?;
    let projection = graph
        .replay(None)
        .map_err(|error| (ErrorCode::Internal, error.message))?;
    let metadata = projection.metadata();
    Ok(Transcript {
        created_ms: nonneg_millis(metadata.created_at.timestamp_millis()),
        working_dir: metadata.working_dir.clone(),
        messages: projection
            .messages()
            .iter()
            .filter_map(|projected| match serde_json::to_value(&projected.message) {
                Ok(message) => Some(message),
                Err(_) => None,
            })
            .collect(),
    })
}

fn read_legacy_transcript(session_id: &str) -> Result<Transcript, (ErrorCode, String)> {
    let Some(path) = BridgeState::session_record_path(session_id) else {
        return Err((ErrorCode::InvalidRequest, "invalid session id".into()));
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|error| (ErrorCode::UnknownSession, format!("session does not exist: {error}")))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| (ErrorCode::Internal, format!("invalid session record: {error}")))?;
    let mut messages: Vec<Value> = match value["messages"].as_array() {
        Some(messages) => messages.clone(),
        None => Vec::new(),
    };
    // Recent turns of a legacy session live in its journal, which is appended
    // to after the snapshot. Without it, artifacts from the newest turns would
    // be missing exactly when the user is looking at them.
    let journal = path.with_file_name(format!("{session_id}.journal.jsonl"));
    if let Ok(text) = std::fs::read_to_string(journal) {
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(appended) = entry["append_messages"].as_array() {
                messages.extend(appended.iter().cloned());
            }
        }
    }
    Ok(Transcript {
        created_ms: value["created_at"].as_str().and_then(parse_rfc3339_ms),
        working_dir: value["working_dir"].as_str().map(str::to_string),
        messages,
    })
}

fn parse_rfc3339_ms(raw: &str) -> Option<u64> {
    match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(at) => nonneg_millis(at.timestamp_millis()),
        Err(_) => None,
    }
}

#[allow(clippy::manual_ok_err)]
fn nonneg_millis(millis: i64) -> Option<u64> {
    match u64::try_from(millis) {
        Ok(millis) => Some(millis),
        Err(_) => None,
    }
}

fn store_paths() -> Option<store::SessionPaths> {
    let home = BridgeState::horon_agent_home()?;
    let home = if home.exists() {
        match home.canonicalize() {
            Ok(home) => home,
            Err(_) => return None,
        }
    } else {
        home
    };
    Some(store::SessionPaths::new(home.join("sessions")))
}

/// Paths the transcript names as files it touched.
///
/// Images stored inline in a message carry bytes, not a path, so they have no
/// filesystem identity to list: the entry type is metadata only, and `history`
/// already delivers those images to clients. What is discoverable here is the
/// path a tool call wrote or read, which is how a generated image or report
/// reaches the list.
fn transcript_candidates(transcript: &Transcript, limit: usize) -> Vec<String> {
    let mut candidates = Vec::new();
    for message in &transcript.messages {
        let Some(blocks) = message["content"].as_array() else {
            continue;
        };
        for block in blocks {
            if block["type"] != "tool_use" {
                continue;
            }
            let input = &block["input"];
            for key in ["path", "file_path", "filePath"] {
                let Some(raw) = input[key].as_str() else {
                    continue;
                };
                candidates.push(raw.to_string());
                if candidates.len() >= limit {
                    return candidates;
                }
            }
        }
    }
    candidates
}

/// Resolve one transcript path to a `/`-separated path under the session root.
///
/// A tool call may name an absolute path, a relative one, a URL, or something
/// with quotes and `..` in it. Only a path that lands inside the canonical
/// working directory is kept; everything else is dropped rather than guessed
/// at.
fn relative_candidate(root: &Path, raw: &str) -> Option<String> {
    let raw = raw.trim().trim_matches(|ch| matches!(ch, '"' | '\'' | '`'));
    if raw.is_empty() || raw.contains('\0') || raw.contains('*') {
        return None;
    }
    let lowered = raw.to_ascii_lowercase();
    if lowered.contains("://") || raw.starts_with("//") {
        return None;
    }
    let path = Path::new(raw);
    let relative = if path.is_absolute() {
        match path.strip_prefix(root) {
            Ok(relative) => relative,
            Err(_) => return None,
        }
    } else {
        path
    };
    if relative.components().any(|part| {
        matches!(
            part,
            std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }
    let relative = relative.to_string_lossy().replace('\\', "/");
    (!relative.is_empty()).then_some(relative)
}

/// Walk the working directory for files a conversation could have produced.
///
/// Bounded three ways so a huge project cannot make this request unbounded:
/// at most [`MAX_SCAN_ENTRIES`] directory entries visited, at most
/// [`MAX_SCAN_FILES`] files considered, and the same heavy directories the
/// file tools skip. Symlinks are not followed, so the walk cannot leave the
/// project or loop.
fn scan_working_dir(root: &Path, started_ms: u64) -> Vec<(String, (ArtifactKind, u64, u64))> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut scanned_entries = 0usize;
    let mut considered = 0usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            scanned_entries += 1;
            if scanned_entries > MAX_SCAN_ENTRIES {
                return found;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if !SKIPPED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()) {
                    pending.push(entry.path());
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            considered += 1;
            if considered > MAX_SCAN_FILES {
                return found;
            }
            let path = entry.path();
            let relative = match path.strip_prefix(root) {
                Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            let Some(kind) = artifact_kind(&relative) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let Some(mtime) = modified_ms(&meta) else {
                continue;
            };
            // Anything older than the session predates the conversation, so it
            // is project content, not output.
            if mtime < started_ms {
                continue;
            }
            found.push((relative, (kind, meta.len(), mtime)));
        }
    }
    found
}

fn artifact_kind(relative: &str) -> Option<ArtifactKind> {
    let extension = Path::new(relative)
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase();
    ARTIFACT_EXTENSIONS
        .iter()
        .find(|(candidate, _)| *candidate == extension)
        .map(|(_, kind)| *kind)
}

#[allow(clippy::manual_ok_err)]
fn system_time_ms(time: SystemTime) -> Option<u64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => Some(elapsed.as_millis() as u64),
        Err(_) => None,
    }
}

fn modified_ms(meta: &std::fs::Metadata) -> Option<u64> {
    match meta.modified() {
        Ok(time) => system_time_ms(time),
        Err(_) => None,
    }
}

fn file_mtime_ms(path: &Path) -> Option<u64> {
    match path.metadata() {
        Ok(meta) => modified_ms(&meta),
        Err(_) => None,
    }
}

#[cfg(test)]
#[path = "search_artifacts_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) fn artifact_scan_bounds_for_test() -> (usize, usize, usize) {
    (MAX_SCAN_ENTRIES, MAX_SCAN_FILES, MAX_ARTIFACT_RESULTS)
}
