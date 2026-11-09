//! Manual-QA round trip for `search_sessions` and `list_artifacts`.
//!
//! Run with `cargo test -p horon-agent-harness-api-server --test search_artifacts_qa -- --nocapture`
//! to print the exact JSON a client receives. The fixtures are written through
//! the agent's own `Session` API, so the store under test is the store the
//! daemon produces, not a hand-rolled imitation.

use horon_agent_app_core::message::{ContentBlock, Role};
use horon_agent_app_core::session::Session;
use horon_agent_harness_api::ServerFrame;
use horon_agent_harness_api_server::translate::{BridgeState, Outbound};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// `HORON_HOME` is process-global and the tests in this binary run in
/// parallel, so a test that changed it while another was seeding wrote its
/// fixture into the real `~/.horon`. Serialize every case here.
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Home {
    path: PathBuf,
    previous: Option<std::ffi::OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Home {
    fn new() -> Self {
        let guard = HOME_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os("HORON_HOME");
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("horon-agent-qa-{}-{unique}", std::process::id()));
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(path.join("sessions"))
                .expect("private sessions dir");
        }
        // SAFETY: the guard above is the only writer of this variable here.
        unsafe { std::env::set_var("HORON_HOME", &path) };
        Self {
            path,
            previous,
            _guard: guard,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("HORON_HOME", value) },
            None => unsafe { std::env::remove_var("HORON_HOME") },
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn seed(session_id: &str, working_dir: &Path, title: &str, prompt: &str) -> chrono::DateTime<chrono::Utc> {
    let created = chrono::Utc::now();
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
    created
}

fn send(request: Value) -> Value {
    let outbound = BridgeState::default().api_request_to_legacy(&request);
    assert_eq!(outbound.len(), 1, "expected one reply for {request}");
    match outbound.into_iter().next().expect("one outbound") {
        Outbound::Reply(frame) | Outbound::Serialized { frame, .. } => {
            serde_json::to_value(&frame).expect("serialize reply")
        }
        Outbound::Legacy(value) => panic!("expected a local reply, got a daemon request: {value}"),
    }
}

fn touch(path: &Path, bytes: usize) {
    std::fs::write(path, vec![b'x'; bytes]).expect("write artifact");
}

#[test]
fn qa_round_trip_prints_real_replies() {
    let home = Home::new();
    let project = home.path.join("qa-project");
    std::fs::create_dir_all(&project).expect("project dir");

    let alpha = seed(
        "session_qa_alpha_1770000001000",
        &project,
        "Release checklist",
        "the zanzibar release needs a retry budget and a rollback plan",
    );
    seed(
        "session_qa_beta_1770000002000",
        &project,
        "Index naming",
        "we named the search index zanzibar after the meeting",
    );
    let _ = alpha;

    touch(&project.join("report.html"), 128);
    touch(&project.join("chart.png"), 256);
    touch(&project.join("ignore.txt"), 32);

    let search = send(json!({
        "v": 1, "id": 1, "req": "search_sessions", "query": "zanzibar", "limit": 10
    }));
    let artifacts = send(json!({
        "v": 1, "id": 2, "req": "list_artifacts", "session_id": "session_qa_alpha_1770000001000"
    }));
    let blank = send(json!({"v": 1, "id": 3, "req": "search_sessions", "query": "   "}));

    println!(
        "SEARCH_SESSIONS {}",
        serde_json::to_string_pretty(&search).expect("json")
    );
    println!(
        "LIST_ARTIFACTS {}",
        serde_json::to_string_pretty(&artifacts).expect("json")
    );
    println!("BLANK_QUERY {}", serde_json::to_string_pretty(&blank).expect("json"));

    let results = search["results"].as_array().expect("results array");
    assert!(results.len() >= 2, "both seeded sessions must match: {search}");
    assert!(
        results
            .iter()
            .any(|hit| hit["session_id"] == "session_qa_alpha_1770000001000")
    );
    assert!(
        results
            .iter()
            .any(|hit| hit["session_id"] == "session_qa_beta_1770000002000")
    );
    assert!(results.iter().all(|hit| {
        hit["preview"]
            .as_str()
            .is_some_and(|preview| preview.to_lowercase().contains("zanzibar"))
    }));

    let entries = artifacts["artifacts"].as_array().expect("artifacts array");
    let mut paths: Vec<_> = entries.iter().filter_map(|entry| entry["path"].as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["chart.png", "report.html"], "{artifacts}");
    assert!(entries.iter().all(|entry| entry["bytes"].as_u64().unwrap_or(0) > 0));

    assert_eq!(blank["code"], "invalid_request", "{blank}");
}

#[test]
fn qa_unknown_session_reply_is_typed() {
    let _home = Home::new();
    let reply = send(json!({
        "v": 1, "id": 4, "req": "list_artifacts", "session_id": "session_qa_absent_1770000009000"
    }));
    println!(
        "UNKNOWN_SESSION {}",
        serde_json::to_string_pretty(&reply).expect("json")
    );
    assert_eq!(reply["code"], "unknown_session", "{reply}");
    assert_eq!(reply["v"], 1);
    assert_eq!(reply["reply_to"], 4);
    let _ = ServerFrame::event(horon_agent_harness_api::ApiEvent::Ok);
}
