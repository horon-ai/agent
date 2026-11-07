use super::{Session, SessionStatus, active_pids_dir, session_exists, store_paths};
use crate::id::extract_session_name;
use crate::message::{ContentBlock, Role};
use crate::storage;
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashSet;

const RELEVANT_CRASH_GROUP_WINDOW_SECS: i64 = 60;

/// Recover crashed sessions from the most relevant crash group (text-only).
/// Returns new recovery session IDs (most recent first).
pub fn recover_crashed_sessions() -> Result<Vec<String>> {
    recover_crashed_sessions_matching(None)
}

/// Recover the specific crashed sessions chosen by the picker.
///
/// This is intentionally stricter than `recover_crashed_sessions`: the picker has
/// already guessed the relevant crash group, so this avoids restoring stale
/// crashed sessions that happen to still exist on disk.
pub fn recover_crashed_sessions_by_ids(session_ids: &[String]) -> Result<Vec<String>> {
    if session_ids.is_empty() {
        return Ok(Vec::new());
    }
    let allowed: HashSet<String> = session_ids.iter().cloned().collect();
    recover_crashed_sessions_for_allowed_ids(&allowed)
}

fn recover_crashed_sessions_for_allowed_ids(allowed_ids: &HashSet<String>) -> Result<Vec<String>> {
    let paths = store_paths()?;
    if !paths.root().exists() {
        return Ok(Vec::new());
    }

    let mut recovered_parents: HashSet<String> = HashSet::new();
    for source in horon_agent_session_store::reader::sources(&paths)? {
        let stem = source.session_id.as_str();
        if stem.starts_with("session_recovery_")
            && let Ok(session) = Session::load_startup_stub(stem)
            && let Some(parent) = session.parent_id
        {
            recovered_parents.insert(parent);
        }
    }

    let mut crashed = Vec::with_capacity(allowed_ids.len());
    for session_id in allowed_ids {
        if recovered_parents.contains(session_id) || !session_exists(session_id) {
            continue;
        }
        let mut session = Session::load(session_id)?;
        if session.detect_crash() {
            let _ = session.save();
        }
        if matches!(session.status, SessionStatus::Crashed { .. }) {
            crashed.push(session);
        }
    }

    recover_loaded_crashed_sessions(crashed)
}

fn recover_crashed_sessions_matching(allowed_ids: Option<&HashSet<String>>) -> Result<Vec<String>> {
    let paths = store_paths()?;
    if !paths.root().exists() {
        return Ok(Vec::new());
    }

    let mut sessions: Vec<Session> = Vec::new();
    for source in horon_agent_session_store::reader::sources(&paths)? {
        let stem = source.session_id.as_str();
        if let Ok(mut session) = Session::load(stem) {
            if session.detect_crash() {
                let _ = session.save();
            }
            sessions.push(session);
        }
    }

    // Track existing recovery sessions to avoid duplicates
    let mut recovered_parents: HashSet<String> = HashSet::new();
    for s in &sessions {
        if s.id.starts_with("session_recovery_")
            && let Some(parent) = s.parent_id.as_ref()
        {
            recovered_parents.insert(parent.clone());
        }
    }

    let mut crashed: Vec<Session> = sessions
        .into_iter()
        .filter(|s| matches!(s.status, SessionStatus::Crashed { .. }))
        .filter(|s| !recovered_parents.contains(&s.id))
        .collect();

    if let Some(allowed_ids) = allowed_ids {
        crashed.retain(|s| allowed_ids.contains(&s.id));
    }

    if crashed.is_empty() {
        return Ok(Vec::new());
    }

    if allowed_ids.is_none() {
        retain_relevant_crash_group(&mut crashed);
    }

    recover_loaded_crashed_sessions(crashed)
}

fn recover_loaded_crashed_sessions(mut crashed: Vec<Session>) -> Result<Vec<String>> {
    crashed.sort_by_key(|session| std::cmp::Reverse(crash_timestamp(session)));

    let mut new_ids = Vec::new();
    for old in crashed {
        let new_id = format!("session_recovery_{}", crate::id::new_id("rec"));
        let mut new_session = Session::create_with_id(new_id.clone(), Some(old.id.clone()), old.title.clone());
        new_session.custom_title = old.custom_title.clone();
        new_session.working_dir = old.working_dir.clone();
        new_session.provider_key = old.provider_key.clone();
        new_session.route_api_method = old.route_api_method.clone();
        new_session.model = old.model.clone();
        new_session.improve_mode = old.improve_mode;
        new_session.is_debug = old.is_debug;
        new_session.saved = old.saved;
        new_session.save_label = old.save_label.clone();
        new_session.provider_session_id = None;
        new_session.status = SessionStatus::Closed;

        // Add a recovery header
        new_session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("Recovered from crashed session `{}` ({})", old.id, old.display_name()),
                cache_control: None,
            }],
        );

        for msg in old.history_messages()? {
            let kept_blocks: Vec<ContentBlock> = msg
                .content
                .into_iter()
                .filter(|block| matches!(block, ContentBlock::Text { .. }))
                .collect();
            if kept_blocks.is_empty() {
                continue;
            }
            new_session.add_message(msg.role, kept_blocks);
        }

        new_session.save()?;
        new_ids.push(new_id);
    }

    Ok(new_ids)
}

fn crash_timestamp(session: &Session) -> DateTime<Utc> {
    session.last_active_at.unwrap_or(session.updated_at)
}

fn retain_relevant_crash_group(crashed: &mut Vec<Session>) -> Option<DateTime<Utc>> {
    let most_recent = crashed.iter().map(crash_timestamp).max()?;
    let crash_window = Duration::seconds(RELEVANT_CRASH_GROUP_WINDOW_SECS);
    crashed.retain(|s| {
        let delta = most_recent.signed_duration_since(crash_timestamp(s));
        delta >= Duration::zero() && delta <= crash_window
    });
    Some(most_recent)
}

/// Info about crashed sessions pending batch restore
#[derive(Debug, Clone)]
pub struct CrashedSessionsInfo {
    /// Session IDs in the guessed relevant restore group.
    pub session_ids: Vec<String>,
    /// Display names of sessions in the guessed relevant restore group.
    pub display_names: Vec<String>,
    /// When the most recent crash occurred
    pub most_recent_crash: DateTime<Utc>,
    /// Crashed sessions excluded because they were outside the relevant group.
    pub omitted_crashed_count: usize,
}

/// Detect crashed sessions that can be batch restored.
/// Returns info about crashed sessions within the crash window (60 seconds),
/// excluding any that have already been recovered.
pub fn detect_crashed_sessions() -> Result<Option<CrashedSessionsInfo>> {
    let paths = store_paths()?;
    if !paths.root().exists() {
        return Ok(None);
    }

    let mut sessions: Vec<Session> = Vec::new();
    for source in horon_agent_session_store::reader::sources(&paths)? {
        let stem = source.session_id.as_str();
        if let Ok(mut session) = Session::load(stem) {
            if session.detect_crash() {
                let _ = session.save();
            }
            sessions.push(session);
        }
    }

    // Track existing recovery sessions to avoid showing already-recovered crashes
    let mut recovered_parents: HashSet<String> = HashSet::new();
    for s in &sessions {
        if s.id.starts_with("session_recovery_")
            && let Some(parent) = s.parent_id.as_ref()
        {
            recovered_parents.insert(parent.clone());
        }
    }

    // Filter to crashed sessions that haven't been recovered
    let mut crashed: Vec<Session> = sessions
        .into_iter()
        .filter(|s| matches!(s.status, SessionStatus::Crashed { .. }))
        .filter(|s| !recovered_parents.contains(&s.id))
        .collect();

    if crashed.is_empty() {
        return Ok(None);
    }

    let total_unrecovered_crashed = crashed.len();
    let most_recent = retain_relevant_crash_group(&mut crashed).unwrap_or_else(Utc::now);

    if crashed.is_empty() {
        return Ok(None);
    }

    // Sort by most recent first
    crashed.sort_by_key(|session| std::cmp::Reverse(crash_timestamp(session)));

    let session_ids: Vec<String> = crashed.iter().map(|s| s.id.clone()).collect();
    let display_names: Vec<String> = crashed.iter().map(|s| s.display_name().to_string()).collect();

    Ok(Some(CrashedSessionsInfo {
        session_ids,
        display_names,
        most_recent_crash: most_recent,
        omitted_crashed_count: total_unrecovered_crashed.saturating_sub(crashed.len()),
    }))
}

/// Find recent crashed sessions for showing resume hints.
///
/// Uses a fast O(n) scan of `~/.horon-agent/active_pids/` (typically 0-5 files)
/// instead of scanning the full sessions directory (tens of thousands).
/// Each file in active_pids/ contains a PID; if that PID is dead, the
/// session crashed. We then load only those specific session files.
///
/// Falls back to the legacy directory scan if active_pids/ doesn't exist
/// (first run after upgrade).
pub fn find_recent_crashed_sessions() -> Vec<(String, String)> {
    if let Some(results) = find_crashed_via_pid_files() {
        return results;
    }
    find_crashed_legacy_scan()
}

/// Fast path: check active_pids/ directory for dead PIDs.
fn find_crashed_via_pid_files() -> Option<Vec<(String, String)>> {
    let dir = active_pids_dir()?;
    if !dir.exists() {
        return None;
    }

    let entries = std::fs::read_dir(&dir).ok()?;
    let cutoff = Utc::now() - Duration::hours(24);
    let mut crashed: Vec<(String, String, DateTime<Utc>)> = Vec::new();

    for entry in entries.flatten() {
        let session_id = match entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        let pid_str = match std::fs::read_to_string(entry.path()) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let pid: u32 = match pid_str.trim().parse() {
            Ok(p) => p,
            Err(_) => {
                let _ = std::fs::remove_file(entry.path());
                continue;
            }
        };

        if is_pid_running(pid) {
            continue;
        }

        match Session::load(&session_id) {
            Ok(mut session) => {
                // Idle-vs-interrupted disposition (upstream #988, extended to
                // the PID detector): losing the process is only a crash when
                // it interrupts unfinished work. A leftover streaming marker
                // proves the session was mid-turn when its owner died
                // (`StreamingGuard` clears the marker on every exit path —
                // normal return, `?`, interrupt, panic — so only a killed
                // process leaves one behind). No marker = idle = ordinary
                // close, same as the disconnect path's idle rule. This stops
                // daemon restarts stamping every owned idle session Crashed.
                // Fail toward crash when the marker dir is unreadable: crash
                // recovery is fail-noisy, never fail-silent.
                if had_streaming_marker(&session_id) {
                    session.mark_crashed(Some(format!(
                        "Process {} exited unexpectedly (no shutdown signal captured)",
                        pid
                    )));
                } else {
                    session.mark_closed();
                }
                let _ = session.save();
                let ts = session.last_active_at.unwrap_or(session.updated_at);
                if ts <= cutoff {
                    continue;
                }
                let name = extract_session_name(&session_id).unwrap_or(&session_id).to_string();
                crashed.push((session_id, name, ts));
            }
            Err(_) => {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    crashed.sort_by(|a, b| b.2.cmp(&a.2));
    Some(crashed.into_iter().map(|(id, name, _)| (id, name)).collect())
}

/// True when a streaming marker file exists for this session — i.e. the
/// session was mid-turn when its owner died. Any content counts (even a
/// stale marker naming the dead owner): `StreamingGuard` removes the file
/// on every orderly exit, so presence alone proves interruption. Missing
/// marker dir or I/O error returns true (fail toward crash recovery, never
/// silently close a session that may hold interrupted work).
fn had_streaming_marker(session_id: &str) -> bool {
    let Some(dir) = storage::streaming_pids_dir() else {
        return true;
    };
    match std::fs::read_dir(&dir) {
        Ok(_) => dir.join(session_id).exists(),
        Err(_) => true,
    }
}

/// One crashed-session candidate: id, short name, updated at, last active at.
type LegacyCandidate = (String, Option<String>, DateTime<Utc>, Option<DateTime<Utc>>);

/// Legacy fallback: scan the full sessions directory.
///
/// The historical status of an old snapshot is not runtime state, so it is read
/// from the preserved auxiliary through the one legacy module rather than
/// resurrected on a loaded session. A canonical log whose own replay disagrees
/// with its aux history is not a legacy crash.
fn find_crashed_legacy_scan() -> Vec<(String, String)> {
    let paths = match store_paths() {
        Ok(paths) => paths,
        Err(_) => return Vec::new(),
    };
    if !paths.root().exists() {
        return Vec::new();
    }

    let cutoff = Utc::now() - Duration::hours(24);
    let cutoff_system = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(24 * 3600))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let home = match paths.root().parent() {
        Some(home) => home.to_path_buf(),
        None => return Vec::new(),
    };

    let mut recovered_parents: HashSet<String> = HashSet::new();
    let mut candidates: Vec<LegacyCandidate> = Vec::new();

    let sources = match horon_agent_session_store::reader::sources(&paths) {
        Ok(sources) => sources,
        Err(_) => return Vec::new(),
    };

    for source in sources {
        if source.modified < cutoff_system || source.size == 0 {
            continue;
        }
        let id = source.session_id;
        let read = match horon_agent_session_store::legacy::LegacySource::new(&home, id.clone())
            .and_then(|source| source.read())
        {
            Ok(read) => read,
            Err(_) => continue,
        };
        let crashed = read
            .auxiliary
            .get("status")
            .is_some_and(|status| status.get("Crashed").is_some())
            || read.auxiliary.get("status").is_some_and(|status| status == "crashed");
        if id.as_str().starts_with("session_recovery_")
            && let Some(parent) = read.metadata.parent_id.as_ref()
        {
            recovered_parents.insert(parent.clone());
        }
        if crashed {
            let last_active_at = read
                .auxiliary
                .get("last_active_at")
                .and_then(|value| serde_json::from_value(value.clone()).ok());
            candidates.push((
                id.as_str().to_owned(),
                read.metadata.short_name,
                read.metadata.updated_at,
                last_active_at,
            ));
        }
    }

    let mut crashed: Vec<_> = candidates
        .into_iter()
        .filter(|(id, _, _, _)| !recovered_parents.contains(id))
        .filter(|(_, _, updated_at, last_active_at)| last_active_at.unwrap_or(*updated_at) > cutoff)
        .collect();

    crashed.sort_by(|a, b| b.2.cmp(&a.2));

    crashed
        .into_iter()
        .map(|(id, short_name, _, _)| {
            let name = short_name
                .or_else(|| extract_session_name(&id).map(ToOwned::to_owned))
                .unwrap_or_else(|| id.clone());
            (id, name)
        })
        .collect()
}

pub(super) fn is_pid_running(pid: u32) -> bool {
    crate::platform::is_process_running(pid)
}

// ---------------------------------------------------------------------------
// Active PID tracking
// ---------------------------------------------------------------------------
// Lightweight files in ~/.horon-agent/active_pids/<session_id> containing the PID.
// Written on mark_active(), removed on mark_closed()/mark_crashed().
// On startup we only need to scan this tiny directory (usually 0-5 files)
// instead of the entire sessions/ directory (tens of thousands of files).

fn normalize_resume_lookup_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn session_matches_resume_title(session: &Session, normalized_query: &str) -> bool {
    if normalized_query.is_empty() {
        return false;
    }

    session
        .display_title()
        .map(normalize_resume_lookup_text)
        .is_some_and(|title| title == normalized_query || title.contains(normalized_query))
}

/// Given a *bare* external provider id, return the id of a locally-imported
/// snapshot (`imported_<tool>_<id>`) if one already exists on disk.
///
/// External CLI sessions (OpenCode, Codex, Claude Code) are imported into the
/// horon-agent store under a stable `imported_<tool>_<provider_id>` stem. Resuming the
/// full imported id always works, but a *bare* provider id (e.g. the OpenCode
/// `ses_...` shown in the resume picker / reload handoff) previously only
/// resolved by re-importing from the external tool's own storage. Once the user
/// removed or reinstalled that tool, the re-import failed and resume hard-exited
/// with "No session found matching 'ses_...'" even though the imported snapshot
/// was still sitting in `~/.horon-agent/sessions` (issue #336).
///
/// Pi sessions are intentionally excluded: their imported id is a hash of the
/// session *path*, not the provider id, so there is no bare-id mapping.
fn resolve_imported_snapshot_id(provider_id: &str) -> Option<String> {
    // Don't double-prefix an id that is already an imported stem.
    if provider_id.starts_with("imported_") {
        return None;
    }

    [
        crate::import::imported_opencode_session_id(provider_id),
        crate::import::imported_codex_session_id(provider_id),
        crate::import::imported_claude_code_session_id(provider_id),
        crate::import::imported_cursor_session_id(provider_id),
    ]
    .into_iter()
    .find(|candidate| session_exists(candidate))
}

/// Find a session by ID, memorable name, generated title, or custom rename.
/// If the input doesn't load as a full session ID, scan recent session snapshots
/// and return the newest matching short name/title.
/// Returns the full session ID if found.
pub fn find_session_by_name_or_id(name_or_id: &str) -> Result<String> {
    // Try loading directly first so stable imported IDs like `imported_codex_*`
    // or other explicit session ids can be resumed without going through the
    // short-name matcher.
    match Session::load(name_or_id) {
        Ok(_) => return Ok(name_or_id.to_string()),
        Err(e) => {
            if session_exists(name_or_id) {
                anyhow::bail!(
                    "Session '{}' exists but failed to load (possibly corrupt):\n  {}",
                    name_or_id,
                    e
                );
            }
        }
    }

    // A *bare* external provider id (e.g. an OpenCode `ses_...` or a Codex/Claude
    // session id) may already have a locally-imported snapshot stored under an
    // `imported_<tool>_<id>` stem. Resolve to that snapshot before falling back to
    // re-importing from the external tool's storage, which fails outright once the
    // user has removed/reinstalled that tool (issue #336).
    if let Some(imported_id) = resolve_imported_snapshot_id(name_or_id) {
        return Ok(imported_id);
    }

    // A full generated id (`session_<name>_<millis>_<hex>`) can never match a
    // short name or title, so the scan below (which parses every session file
    // on disk) is pure overhead. With thousands of sessions that scan took
    // ~15s and stalled reload handoffs for ids that only exist server-side.
    if crate::id::is_generated_session_id(name_or_id) {
        anyhow::bail!("No session found matching '{}'", name_or_id);
    }

    // Otherwise, search for a session with matching short name or title.
    let paths = store_paths()?;
    if !paths.root().exists() {
        anyhow::bail!("No sessions found");
    }

    let normalized_query = normalize_resume_lookup_text(name_or_id);
    let mut exact_matches: Vec<(String, chrono::DateTime<chrono::Utc>)> = Vec::new();
    let mut title_matches: Vec<(String, chrono::DateTime<chrono::Utc>)> = Vec::new();

    for source in horon_agent_session_store::reader::sources(&paths)? {
        let stem = source.session_id.as_str();

        let short_name_matches = extract_session_name(stem).is_some_and(|short| short == name_or_id);
        if short_name_matches {
            if let Ok(session) = Session::load_startup_stub(stem).or_else(|_| Session::load(stem)) {
                exact_matches.push((stem.to_string(), session.updated_at));
            }
            continue;
        }

        let Ok(session) = Session::load_startup_stub(stem).or_else(|_| Session::load(stem)) else {
            continue;
        };
        if session.short_name.as_deref() == Some(name_or_id) {
            exact_matches.push((stem.to_string(), session.updated_at));
        } else if session_matches_resume_title(&session, &normalized_query) {
            title_matches.push((stem.to_string(), session.updated_at));
        }
    }

    let matches = if exact_matches.is_empty() {
        &mut title_matches
    } else {
        &mut exact_matches
    };

    if matches.is_empty() {
        anyhow::bail!("No session found matching '{}'", name_or_id);
    }

    // Sort by updated_at descending and return the most recent match.
    matches.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(matches[0].0.clone())
}

#[cfg(test)]
mod batch_crash_tests {
    use super::*;

    #[test]
    fn test_crashed_sessions_info_struct() {
        let info = CrashedSessionsInfo {
            session_ids: vec!["session_test_1".to_string(), "session_test_2".to_string()],
            display_names: vec!["fox".to_string(), "oak".to_string()],
            most_recent_crash: Utc::now(),
            omitted_crashed_count: 0,
        };
        assert_eq!(info.session_ids.len(), 2);
        assert_eq!(info.display_names.len(), 2);
        assert_eq!(info.display_names[0], "fox");
    }

    fn isolated_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        crate::storage::create_store_dir(&temp.path().join("sessions"), true).expect("sessions dir");
        crate::env::set_var("HORON_AGENT_HOME", temp.path());
        (temp, guard)
    }

    #[test]
    fn crash_scan_reads_both_layouts_and_a_corrupt_session_cannot_hide_another() {
        let (temp, _guard) = isolated_home();

        let mut canonical = Session::create_with_id(
            "session_healthy_1770000001000".to_string(),
            None,
            Some("Healthy plan".to_string()),
        );
        canonical.status = SessionStatus::Closed;
        canonical.save().expect("save canonical session");

        let legacy_id = "session_legacytitle_1770000002000";
        let mut legacy = Session::create_with_id(legacy_id.to_string(), None, None);
        legacy.mark_closed();
        legacy.last_pid = None;
        legacy.rename_title(Some("Legacy plan".to_string()));
        let legacy_path = super::super::session_path(legacy_id).expect("legacy path");
        storage::write_json(&legacy_path, &legacy).expect("legacy fixture");

        let corrupt_id = "session_corrupt_1770000003000";
        let corrupt_dir = temp.path().join("sessions").join(corrupt_id);
        crate::storage::create_store_dir(&corrupt_dir, false).expect("corrupt session dir");
        let corrupt_bytes: &[u8] = b"{not a record}\n";
        std::fs::write(corrupt_dir.join("events.jsonl"), corrupt_bytes).expect("corrupt log");

        let pid_dir = storage::active_pids_dir().expect("active pid dir");
        crate::storage::create_store_dir(&pid_dir, true).expect("active pid dir created");
        std::fs::write(pid_dir.join(corrupt_id), "999999").expect("stale pid for corrupt");
        std::fs::write(pid_dir.join("session_absent_1770000004000"), "999998").expect("stale pid for absent");

        let crashed = find_recent_crashed_sessions();
        let listed: Vec<&str> = crashed.iter().map(|(id, _)| id.as_str()).collect();
        assert!(
            !listed.contains(&corrupt_id),
            "an unreadable session must not be reported as recovered: {listed:?}"
        );
        assert!(
            !pid_dir.join(corrupt_id).exists(),
            "the dead pid file for an unreadable session is cleared"
        );

        assert_eq!(
            find_session_by_name_or_id("healthy").expect("canonical session by name"),
            "session_healthy_1770000001000",
            "a corrupt neighbour must not remove a healthy canonical session"
        );
        assert_eq!(
            find_session_by_name_or_id("legacytitle").expect("legacy session by title"),
            legacy_id,
            "a corrupt neighbour must not remove a preserved legacy session"
        );
        assert_eq!(
            std::fs::read(corrupt_dir.join("events.jsonl")).expect("corrupt bytes"),
            corrupt_bytes,
            "raw fallback must not rewrite an unreadable original"
        );

        crate::env::remove_var("HORON_AGENT_HOME");
    }

    #[test]
    fn legacy_crash_scan_finds_a_preserved_snapshot_and_ignores_a_corrupt_peer() {
        let (temp, _guard) = isolated_home();

        let legacy_id = "session_crashlegacy_1770000007000";
        let mut legacy = Session::create_with_id(legacy_id.to_string(), None, None);
        legacy.mark_crashed(Some("Process 999999 exited unexpectedly".to_string()));
        legacy.last_pid = None;
        legacy.rename_title(Some("Crashed legacy".to_string()));
        let legacy_path = super::super::session_path(legacy_id).expect("legacy path");
        storage::write_json(&legacy_path, &legacy).expect("legacy fixture");
        assert!(
            !temp.path().join("sessions").join(legacy_id).exists(),
            "the fixture must stay in the preserved legacy layout"
        );

        let corrupt = "session_crashcorrupt_1770000008000";
        let corrupt_dir = temp.path().join("sessions").join(corrupt);
        crate::storage::create_store_dir(&corrupt_dir, false).expect("corrupt dir");
        {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(corrupt_dir.join("events.jsonl")).expect("corrupt log");
            file.write_all(b"{not a record}\n").expect("bytes");
        }

        let found = find_crashed_legacy_scan();
        let listed: Vec<&str> = found.iter().map(|(id, _)| id.as_str()).collect();
        assert!(
            listed.contains(&legacy_id),
            "the legacy scan must see a preserved snapshot: {listed:?}"
        );
        assert!(
            !listed.contains(&corrupt),
            "an unreadable canonical log is not a crashed session: {listed:?}"
        );
        assert_eq!(
            found
                .iter()
                .find(|(id, _)| id == legacy_id)
                .map(|(_, name)| name.as_str()),
            Some("crashlegacy"),
            "the scan reports the short name the snapshot carries"
        );

        crate::env::remove_var("HORON_AGENT_HOME");
    }

    #[test]
    fn idle_dead_pid_without_streaming_marker_is_not_a_crash() {
        // Upstream #988 extended to the PID detector: a dead owner PID with
        // no leftover streaming marker means the session was idle — ordinary
        // close, not crash. Daemon restarts must not stamp idle sessions.
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        crate::env::set_var("HORON_AGENT_HOME", temp.path());

        let dir = storage::streaming_pids_dir().expect("streaming dir");
        std::fs::create_dir_all(&dir).expect("marker dir");
        assert!(!had_streaming_marker("session_idle_1"), "no marker file means idle");

        crate::env::remove_var("HORON_AGENT_HOME");
    }

    #[test]
    fn leftover_streaming_marker_means_interrupted() {
        // A marker file left behind (StreamingGuard clears it on every
        // orderly exit) proves the owner died mid-turn — genuine crash.
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        crate::env::set_var("HORON_AGENT_HOME", temp.path());

        let dir = storage::streaming_pids_dir().expect("streaming dir");
        std::fs::create_dir_all(&dir).expect("marker dir");
        std::fs::write(dir.join("session_midturn_1"), "999999").expect("marker");
        assert!(
            had_streaming_marker("session_midturn_1"),
            "leftover marker means interrupted"
        );
        assert!(!had_streaming_marker("session_other_1"), "other sessions unaffected");

        crate::env::remove_var("HORON_AGENT_HOME");
    }

    #[test]
    fn find_session_by_name_or_id_matches_custom_title() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        crate::env::set_var("HORON_AGENT_HOME", temp.path());

        let session_id = "session_renamecli_1770000000000";
        let mut session = Session::create_with_id(
            session_id.to_string(),
            None,
            Some("Generated planning title".to_string()),
        );
        session.status = SessionStatus::Closed;
        session.rename_title(Some("RenameTest".to_string()));
        session.save().expect("save renamed session");

        assert_eq!(
            find_session_by_name_or_id("renametest").expect("resolve custom title"),
            session_id
        );
        assert_eq!(
            find_session_by_name_or_id("Rename").expect("resolve title fragment"),
            session_id
        );

        let legacy_id = "session_legacytitle_1760000000000";
        let mut legacy = Session::create_with_id(legacy_id.into(), None, None);
        legacy.status = SessionStatus::Closed;
        legacy.last_pid = None;
        legacy.rename_title(Some("RenameLegacy".into()));
        legacy.updated_at = session.updated_at - Duration::hours(1);
        let legacy_path = super::super::session_path(legacy_id).expect("legacy path");
        storage::write_json(&legacy_path, &legacy).expect("legacy fixture");
        let original = std::fs::read(&legacy_path).expect("legacy bytes");
        assert_eq!(
            find_session_by_name_or_id("renamelegacy").expect("legacy title"),
            legacy_id
        );
        assert_eq!(
            find_session_by_name_or_id("legacytitle").expect("legacy short name"),
            legacy_id
        );
        assert_eq!(
            find_session_by_name_or_id("rename").expect("newest mixed-format title"),
            session_id
        );
        assert_eq!(std::fs::read(legacy_path).expect("preserved legacy"), original);

        crate::env::remove_var("HORON_AGENT_HOME");
    }

    #[test]
    fn find_session_by_name_or_id_accepts_imported_session_ids() -> anyhow::Result<()> {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir()?;
        crate::env::set_var("HORON_AGENT_HOME", temp.path());

        let imported_id = "imported_codex_test_resume";
        let mut session = Session::create_with_id(imported_id.to_string(), None, Some("Imported".to_string()));
        session.status = SessionStatus::Closed;
        session.save()?;

        let resolved = find_session_by_name_or_id(imported_id)?;
        assert_eq!(resolved, imported_id);

        crate::env::remove_var("HORON_AGENT_HOME");
        Ok(())
    }

    /// Regression test for issue #336: resuming a *bare* external provider id
    /// (e.g. an OpenCode `ses_...`) must resolve to its already-imported local
    /// snapshot even when the external tool's storage has been removed, instead
    /// of hard-failing with "No session found matching 'ses_...'".
    #[test]
    fn find_session_by_name_or_id_resolves_bare_opencode_id_to_imported_snapshot() -> anyhow::Result<()> {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir()?;
        crate::env::set_var("HORON_AGENT_HOME", temp.path());

        let provider_id = "ses_2c72f8f4cffee6Qh7GId7D81Se";
        let imported_id = crate::import::imported_opencode_session_id(provider_id);
        let mut session = Session::create_with_id(
            imported_id.clone(),
            None,
            Some("Analyzing /provider routing".to_string()),
        );
        session.status = SessionStatus::Closed;
        session.provider_session_id = Some(provider_id.to_string());
        session.provider_key = Some("opencode".to_string());
        session.save()?;

        // No external OpenCode store exists under HORON_AGENT_HOME/external, so the only
        // way to resolve the bare id is via the local imported snapshot.
        let resolved = find_session_by_name_or_id(provider_id)?;
        assert_eq!(resolved, imported_id);

        crate::env::remove_var("HORON_AGENT_HOME");
        Ok(())
    }

    /// A bare provider id that has *no* imported snapshot on disk must still fail
    /// (so the caller can fall back to external re-import or surface an error).
    #[test]
    fn find_session_by_name_or_id_bare_id_without_snapshot_still_errors() -> anyhow::Result<()> {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::tempdir()?;
        crate::env::set_var("HORON_AGENT_HOME", temp.path());
        // The store refuses any directory under the agent home that is not 0700
        // on unix, and create_dir_all would leave this one at the umask default.
        crate::storage::create_store_dir(&temp.path().join("sessions"), false)?;

        let err = find_session_by_name_or_id("ses_does_not_exist_anywhere");
        assert!(err.is_err(), "expected unknown bare id to error");

        crate::env::remove_var("HORON_AGENT_HOME");
        Ok(())
    }
}
