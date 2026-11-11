//! Session id prefix resolution on the daemon's Subscribe path.
//!
//! The bridge resolves a prefix for the harness API (see
//! `harness-api-server/src/session_prefix.rs`); these cover the daemon's own
//! path that ACP and direct clients use, so a prefix attaches correctly with
//! either protocol version.

#![allow(clippy::await_holding_lock)]
use super::*;
use crate::agent::Agent;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use serde_json::json;

/// A request that names a session by some prefix of its id.
fn subscribe_to(target: &str) -> Request {
    Request::Subscribe {
        system_prompt: None,
        supports_user_response: false,
        supports_ask_requests: false,
        id: 1,
        working_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        target_session_id: Some(target.to_string()),
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
        acp: None,
    }
}

fn target_of(request: &Request) -> Option<&str> {
    match request {
        Request::Subscribe { target_session_id, .. } => target_session_id.as_deref(),
        _ => None,
    }
}

fn empty_sessions() -> SessionAgents {
    Arc::new(tokio::sync::RwLock::new(HashMap::new()))
}

/// Point the store at a fresh directory for the length of the test.
struct IsolatedHome {
    _home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl IsolatedHome {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let home = tempfile::TempDir::new().expect("isolated horon home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        Self {
            _home: home,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => crate::env::set_var("HORON_HOME", previous),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

fn store_session(id: &str) {
    let mut session = crate::session::Session::create_with_id(id.to_string(), None, None);
    session.working_dir = Some(std::env::temp_dir().to_string_lossy().into_owned());
    session.save_prepared().expect("persist the session for the scan");
}

#[tokio::test]
async fn a_unique_stored_prefix_resolves_on_subscribe() {
    let _home = IsolatedHome::new();
    store_session("session_fox_1790191044353_621ff7b9");
    let mut request = subscribe_to("session_fox");

    resolve_target_subscribe_prefix(&mut request, &empty_sessions())
        .await
        .expect("one stored match resolves");
    assert_eq!(target_of(&request), Some("session_fox_1790191044353_621ff7b9"));
}

#[tokio::test]
async fn an_exact_id_is_never_scanned_or_rewritten() {
    let _home = IsolatedHome::new();
    let id = "session_fox_1790191044353_621ff7b9";
    store_session(id);
    let mut request = subscribe_to(id);

    resolve_target_subscribe_prefix(&mut request, &empty_sessions())
        .await
        .expect("an exact id is already settled");
    assert_eq!(target_of(&request), Some(id));
}

#[tokio::test]
async fn a_prefix_shared_by_two_stored_sessions_is_an_error() {
    let _home = IsolatedHome::new();
    store_session("session_fox_1790191044353_621ff7b9");
    store_session("session_fox_1790191044354_621ff7b9");
    let mut request = subscribe_to("session_fox");

    let error = resolve_target_subscribe_prefix(&mut request, &empty_sessions())
        .await
        .expect_err("two matches must not pick one")
        .to_string();
    assert!(error.contains("matches 2 sessions"), "{error}");
    assert!(error.contains("session_fox_1790191044353_621ff7b9"), "{error}");
    assert_eq!(
        target_of(&request),
        Some("session_fox"),
        "an ambiguous prefix must not rewrite the request"
    );
}

#[tokio::test]
async fn a_prefix_matching_nothing_reaches_the_existing_unknown_session_error() {
    let _home = IsolatedHome::new();
    let mut request = subscribe_to("session_absent");

    resolve_target_subscribe_prefix(&mut request, &empty_sessions())
        .await
        .expect("an unmatched prefix is not ours to reject");
    assert_eq!(
        target_of(&request),
        Some("session_absent"),
        "the later lookup reports the session as unknown"
    );
}

#[tokio::test]
async fn a_live_unpersisted_session_is_a_prefix_candidate() {
    let _home = IsolatedHome::new();
    let sessions = empty_sessions();
    let id = "session_live_1790191044353_621ff7b9";
    sessions
        .write()
        .await
        .insert(id.to_string(), Arc::new(tokio::sync::Mutex::new(unused_agent().await)));
    let mut request = subscribe_to("session_live");

    resolve_target_subscribe_prefix(&mut request, &sessions)
        .await
        .expect("the live session resolves");
    assert_eq!(target_of(&request), Some(id));
}

#[tokio::test]
async fn a_prefix_below_the_minimum_that_matches_is_rejected() {
    let _home = IsolatedHome::new();
    store_session("session_fox_1790191044353_621ff7b9");
    // Five characters: short enough to be refused, long enough to match.
    let mut request = subscribe_to("sessi");

    let error = resolve_target_subscribe_prefix(&mut request, &empty_sessions())
        .await
        .expect_err("six characters are required")
        .to_string();
    assert!(error.contains("at least 6 characters"), "{error}");
    assert_eq!(target_of(&request), Some("sessi"));
}

/// An agent is only needed to occupy the live map for this test; it never runs.
async fn unused_agent() -> Agent {
    let provider: Arc<dyn Provider> = Arc::new(EmptyProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    Agent::new(provider, registry)
}

struct EmptyProvider;

#[async_trait::async_trait]
impl Provider for EmptyProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        anyhow::bail!("the prefix tests never run a model turn")
    }

    fn name(&self) -> &str {
        "empty"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(EmptyProvider)
    }
}

#[test]
fn an_ambiguous_error_names_the_candidates_it_reported() {
    // The bridge copies these ids into `details.accepted_ids`; the daemon path
    // puts the same text in the error. Both come from this one candidate list.
    let error = horon_agent_base::session_prefix::SessionPrefixError::Ambiguous {
        candidates: vec!["session_a_1790191044353".into(), "session_b_1790191044353".into()],
    };
    assert_eq!(error.candidates().len(), 2);
    assert!(error.to_string().contains("session_a_1790191044353"));
    assert!(json!({"error": error.to_string()})["error"].is_string());
}
