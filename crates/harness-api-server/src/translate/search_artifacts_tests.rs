//! Coverage for the two stored-record surfaces.
//!
//! Every case seeds its own `HORON_HOME` and asks through
//! `BridgeState::api_request_to_legacy`, the same entry point the socket loop
//! uses, so a passing test proves the dispatch arm as well as the worker.
use super::*;
use horon_agent_app_core::message::{ContentBlock, Role};
use horon_agent_app_core::session::Session;
use horon_agent_harness_api::ApiEvent;
use serde_json::json;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

struct ScopedHome {
    path: PathBuf,
    previous: Option<OsString>,
    _guard: MutexGuard<'static, ()>,
}

impl ScopedHome {
    fn new(label: &str) -> Self {
        let guard = crate::translate::horon_agent_home_test_lock();
        let previous = std::env::var_os("HORON_HOME");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "horon-agent-search-artifacts-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create isolated home");
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(path.join("sessions"))
                .expect("create private sessions dir");
        }
        // SAFETY: every test that mutates HORON_HOME holds this lock.
        unsafe { std::env::set_var("HORON_HOME", &path) };
        Self {
            path,
            previous,
            _guard: guard,
        }
    }

    fn sessions(&self) -> PathBuf {
        self.path.join("sessions")
    }
}
impl Drop for ScopedHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("HORON_HOME", value) },
            None => unsafe { std::env::remove_var("HORON_HOME") },
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// One stored session in the preserved legacy layout.
fn write_legacy_session(
    home: &Path,
    session_id: &str,
    working_dir: &Path,
    created: chrono::DateTime<chrono::Utc>,
    messages: serde_json::Value,
) {
    let path = home.join("sessions").join(format!("{session_id}.json"));
    std::fs::write(
        &path,
        json!({
            "id": session_id,
            "title": format!("Title for {session_id}"),
            "created_at": created.to_rfc3339(),
            "updated_at": created.to_rfc3339(),
            "working_dir": working_dir,
            "messages": messages,
        })
        .to_string(),
    )
    .expect("write session record");
}

fn user_message(text: &str) -> serde_json::Value {
    json!({"id": "m1", "role": "user", "content": [{"type": "text", "text": text}]})
}
fn tool_use_message(path: &str) -> serde_json::Value {
    json!({"id": "m2", "role": "assistant", "content": [{
        "type": "tool_use", "id": "call-1", "name": "write",
        "input": {"file_path": path, "content": "ignored"}
    }]})
}

fn only_reply(outbound: Vec<Outbound>) -> ApiEvent {
    assert_eq!(outbound.len(), 1, "expected exactly one reply");
    match outbound.into_iter().next().expect("one outbound") {
        Outbound::Reply(frame) => frame.event,
        other => panic!("expected an API reply, got {other:?}"),
    }
}

fn request(req: &str, fields: serde_json::Value) -> Vec<Outbound> {
    let mut frame = json!({"req": req, "id": 7});
    if let (Some(target), Some(fields)) = (frame.as_object_mut(), fields.as_object()) {
        target.extend(fields.clone());
    }
    BridgeState::default().api_request_to_legacy(&frame)
}

fn search(query: &str) -> ApiEvent {
    only_reply(request("search_sessions", json!({"query": query})))
}

fn artifacts(session_id: &str) -> ApiEvent {
    only_reply(request("list_artifacts", json!({"session_id": session_id})))
}

fn hits(event: ApiEvent) -> Vec<SessionSearchHit> {
    match event {
        ApiEvent::SessionSearchResults { results, .. } => results,
        other => panic!("expected session_search_results, got {other:?}"),
    }
}

fn entries(event: ApiEvent) -> Vec<ArtifactEntry> {
    match event {
        ApiEvent::Artifacts { artifacts, .. } => artifacts,
        other => panic!("expected artifacts, got {other:?}"),
    }
}

fn error_code(event: ApiEvent) -> (ErrorCode, String) {
    match event {
        ApiEvent::Error { code, message, .. } => (code, message),
        other => panic!("expected an error, got {other:?}"),
    }
}

/// Seed a session the way the agent itself writes one, so the search engine
/// sees the same records it sees in production.
fn seed_session(
    session_id: &str,
    working_dir: &Path,
    created: chrono::DateTime<chrono::Utc>,
    title: &str,
    prompt: &str,
) {
    let mut session = Session::create_with_id(session_id.to_string(), None, Some(title.to_string()));
    session.mark_closed();
    session.created_at = created;
    session.updated_at = created;
    session.working_dir = Some(working_dir.display().to_string());
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: prompt.to_string(),
            cache_control: None,
        }],
    );
    session.save().expect("save seeded session");
}

fn touch(path: &Path, bytes: usize, mtime_ms: u64) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(path, vec![b'x'; bytes]).expect("write file");
    let modified = UNIX_EPOCH + std::time::Duration::from_millis(mtime_ms);
    let file = std::fs::File::options().write(true).open(path).expect("reopen file");
    file.set_modified(modified).expect("set mtime");
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn started_ago(ms: u64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_millis(now_ms().saturating_sub(ms) as i64).expect("timestamp")
}

#[test]
fn a_query_matching_two_sessions_returns_both_with_previews() {
    let home = ScopedHome::new("two-sessions");
    let project_a = home.path.join("project-a");
    let project_b = home.path.join("project-b");
    std::fs::create_dir_all(&project_a).expect("project a");
    std::fs::create_dir_all(&project_b).expect("project b");
    let created = started_ago(60_000);
    seed_session(
        "session_search_alpha_1770000001000",
        &project_a,
        created,
        "Alpha",
        "the zanzibar protocol needs a retry budget",
    );
    seed_session(
        "session_search_beta_1770000002000",
        &project_b,
        created,
        "Beta",
        "zanzibar is the name we settled on for the index",
    );
    assert!(home.sessions().join("session_search_alpha_1770000001000").is_dir());

    let results = hits(search("zanzibar"));
    let mut sessions: Vec<_> = results.iter().map(|hit| hit.session_id.as_str()).collect();
    sessions.sort_unstable();
    assert_eq!(
        sessions,
        [
            "session_search_alpha_1770000001000",
            "session_search_beta_1770000002000"
        ],
        "both sessions must match: {results:#?}"
    );
    for hit in &results {
        assert!(
            hit.preview.to_lowercase().contains("zanzibar"),
            "preview must contain the match: {hit:#?}"
        );
        assert_eq!(hit.role, "user");
        assert!(hit.title.is_some(), "stored title must reach the client: {hit:#?}");
        assert!(!hit.at.is_empty());
    }
}

#[test]
fn a_blank_query_is_rejected_as_invalid_request() {
    for query in ["", "   ", "\t\n "] {
        let (code, message) = error_code(search(query));
        assert_eq!(code, ErrorCode::InvalidRequest, "query {query:?}: {message}");
        assert!(message.contains("query"), "{message}");
    }
}

#[test]
fn a_query_with_quotes_percent_and_parent_paths_is_just_text() {
    let home = ScopedHome::new("hostile-query");
    let project = home.path.join("project");
    std::fs::create_dir_all(&project).expect("project");
    seed_session(
        "session_hostile_1770000003000",
        &project,
        started_ago(60_000),
        "Hostile",
        "literal 100% done; keep \"quoted\" words",
    );

    // Each of these must return a normal reply, never an error and never a
    // filesystem lookup: the query is search text, not a path or a pattern.
    for query in [
        "100%",
        "\"quoted\"",
        "../../etc/passwd",
        "'; DROP TABLE sessions; --",
        "%_%",
    ] {
        let event = search(query);
        assert!(
            !matches!(event, ApiEvent::Error { .. }),
            "query {query:?} must not fail: {event:?}"
        );
    }
    let results = hits(search("100%"));
    assert!(
        results
            .iter()
            .any(|hit| hit.role == "user" && hit.preview.contains("100%")),
        "{results:#?}"
    );
}

#[test]
fn artifacts_filter_by_extension_and_exclude_files_older_than_the_session() {
    let home = ScopedHome::new("artifact-filter");
    let project = home.path.join("project");
    std::fs::create_dir_all(&project).expect("project");
    let started = now_ms().saturating_sub(60_000);
    let fresh = started + 30_000;
    touch(&project.join("report.html"), 12, fresh);
    touch(&project.join("chart.png"), 34, fresh);
    touch(&project.join("photo.JPEG"), 56, fresh);
    touch(&project.join("clip.webp"), 78, fresh);
    touch(&project.join("notes.txt"), 10, fresh);
    touch(&project.join("main.rs"), 10, fresh);
    touch(&project.join("old-report.html"), 10, started.saturating_sub(120_000));
    touch(&project.join("old-shot.png"), 10, started.saturating_sub(120_000));
    write_legacy_session(
        &home.path,
        "session_artifacts_1770000004000",
        &project,
        started_ago(60_000),
        json!([user_message("make the report")]),
    );

    let found = entries(artifacts("session_artifacts_1770000004000"));
    let mut paths: Vec<_> = found.iter().map(|entry| entry.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        ["chart.png", "clip.webp", "photo.JPEG", "report.html"],
        "{found:#?}"
    );
    for entry in &found {
        let expected = match entry.path.as_str() {
            "report.html" => (ArtifactKind::Html, 12),
            "chart.png" => (ArtifactKind::Image, 34),
            "photo.JPEG" => (ArtifactKind::Image, 56),
            "clip.webp" => (ArtifactKind::Image, 78),
            other => panic!("unexpected artifact {other}"),
        };
        assert_eq!((entry.kind, entry.bytes), expected, "{entry:#?}");
        assert!(entry.mtime_ms >= started);
    }
}

#[test]
fn a_transcript_path_outside_the_project_is_never_listed() {
    let home = ScopedHome::new("artifact-escape");
    let project = home.path.join("project");
    std::fs::create_dir_all(&project).expect("project");
    let outside = home.path.join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    touch(&outside.join("secret.png"), 8, now_ms());
    touch(&project.join("kept.png"), 9, now_ms());
    write_legacy_session(
        &home.path,
        "session_escape_1770000005000",
        &project,
        started_ago(120_000),
        json!([
            user_message("look at these"),
            tool_use_message("../../outside/secret.png"),
            tool_use_message(&outside.join("secret.png").display().to_string()),
            tool_use_message("kept.png"),
        ]),
    );

    let found = entries(artifacts("session_escape_1770000005000"));
    assert_eq!(
        found.iter().map(|entry| entry.path.as_str()).collect::<Vec<_>>(),
        ["kept.png"],
        "{found:#?}"
    );
}

#[test]
fn an_unknown_session_id_is_reported_as_unknown_session() {
    let _home = ScopedHome::new("unknown-session");
    for session_id in [
        "session_missing_1770000006000",
        "../escape",
        "session_not_a_real_session",
    ] {
        let (code, message) = error_code(only_reply(request("list_artifacts", json!({"session_id": session_id}))));
        assert_eq!(code, ErrorCode::UnknownSession, "id {session_id:?}: {message}");
    }
    let (code, message) = error_code(only_reply(request("list_artifacts", json!({"session_id": ""}))));
    assert_eq!(code, ErrorCode::InvalidRequest, "{message}");
}

#[test]
fn a_session_with_no_artifacts_returns_an_empty_list() {
    let home = ScopedHome::new("empty-artifacts");
    let project = home.path.join("project");
    std::fs::create_dir_all(&project).expect("project");
    touch(&project.join("notes.txt"), 10, now_ms());
    write_legacy_session(
        &home.path,
        "session_empty_1770000007000",
        &project,
        started_ago(60_000),
        json!([user_message("nothing visual here")]),
    );

    let found = entries(artifacts("session_empty_1770000007000"));
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn a_deleted_candidate_is_omitted_rather_than_reported_at_zero_bytes() {
    let home = ScopedHome::new("stale-artifact");
    let project = home.path.join("project");
    std::fs::create_dir_all(&project).expect("project");
    let stale = project.join("gone.png");
    touch(&stale, 40, now_ms());
    write_legacy_session(
        &home.path,
        "session_stale_1770000008000",
        &project,
        started_ago(60_000),
        json!([user_message("write a chart"), tool_use_message("gone.png")]),
    );
    assert_eq!(entries(artifacts("session_stale_1770000008000")).len(), 1);
    std::fs::remove_file(&stale).expect("delete candidate");

    let found = entries(artifacts("session_stale_1770000008000"));
    assert!(found.is_empty(), "a deleted file must not be listed: {found:#?}");
}

#[test]
fn the_scan_is_bounded() {
    let (entries, files, results) = artifact_scan_bounds_for_test();
    assert!(entries > 0 && files > 0 && results > 0);
    assert!(files <= entries, "a file budget above the entry budget is unreachable");
}

#[test]
fn a_flood_of_candidates_is_capped_at_the_result_bound() {
    let home = ScopedHome::new("artifact-flood");
    let project = home.path.join("project");
    std::fs::create_dir_all(&project).expect("project");
    let (_, _, max_results) = artifact_scan_bounds_for_test();
    let mtime = now_ms();
    for index in 0..(max_results + 40) {
        touch(&project.join(format!("shot-{index:04}.png")), 4, mtime);
    }
    write_legacy_session(
        &home.path,
        "session_flood_1770000009000",
        &project,
        started_ago(60_000),
        json!([user_message("make a lot of charts")]),
    );

    let found = entries(artifacts("session_flood_1770000009000"));
    assert_eq!(found.len(), max_results, "the reply must stop at the bound");
    assert!(
        found.windows(2).all(|pair| pair[0].mtime_ms >= pair[1].mtime_ms),
        "newest first"
    );
}

#[test]
fn a_search_returns_at_most_the_requested_limit() {
    let home = ScopedHome::new("search-limit");
    let project = home.path.join("project");
    std::fs::create_dir_all(&project).expect("project");
    for index in 0..4 {
        seed_session(
            &format!("session_limit_{index}_177000001{index}000"),
            &project,
            started_ago(60_000),
            &format!("Limit {index}"),
            "quokka appears here",
        );
    }
    let event = only_reply(request("search_sessions", json!({"query": "quokka", "limit": 2})));
    assert!(hits(event).len() <= 2);
}
