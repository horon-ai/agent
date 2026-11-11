//! A spawn's `SpawnExtras` reaches the child: an allow list narrows the child
//! registry and never widens it.

use crate::agent::Agent;
use crate::provider::Provider;
use crate::server::comm_session::{SpawnExtras, spawn_swarm_agent};
use crate::server::comm_worktree::{IsoApply, Isolation};
use crate::tool::Registry;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::{Mutex, RwLock, broadcast};

/// A definition's allow list reaches the child registry and nothing widens it.
#[tokio::test]
async fn allow_list_spawn_narrows_the_child_registry() {
    let _env = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let previous = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(super::MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut session = crate::session::Session::create_with_id("narrow_coord".to_string(), None, None);
    session.model = Some("mock".to_string());
    session.working_dir = Some(temp.path().to_string_lossy().into_owned());
    let agent = Agent::new_with_session(provider.clone(), registry, session, None);
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        "narrow_coord".to_string(),
        Arc::new(Mutex::new(agent)),
    )])));
    let state = crate::server::SwarmState::new(HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
    let mcp = Arc::new(crate::mcp::SharedMcpPool::new(crate::mcp::McpConfig::default()));

    let child_id = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        spawn_swarm_agent(
            "narrow_coord",
            "narrow-swarm",
            Some(temp.path().to_string_lossy().into_owned()),
            Some("read only".to_string()),
            Some(crate::config::SwarmSpawnMode::Headless),
            None,
            None,
            Some("narrow child".into()),
            Isolation::None,
            IsoApply::Apply,
            SpawnExtras {
                output_schema: None,
                allowed_tools: Some(vec!["read".to_string(), "ls".to_string()]),
                source_session: None,
            },
            &sessions,
            &Arc::new(RwLock::new(String::new())),
            &provider,
            &state.members,
            &state.swarms_by_id,
            &state.coordinators,
            &state.plans,
            &Arc::new(RwLock::new(std::collections::VecDeque::new())),
            &Arc::new(AtomicU64::new(0)),
            &broadcast::channel(16).0,
            &mcp,
            &Arc::new(RwLock::new(HashMap::new())),
        ),
    )
    .await
    .expect("the narrow spawn must not block")
    .expect("the narrow spawn succeeds");

    let child = sessions.read().await.get(&child_id).cloned().unwrap();
    {
        let child_guard = child.lock().await;
        let advertised = child_guard.tool_definitions_for_debug().await;
        let names: Vec<&str> = advertised.iter().map(|definition| definition.name.as_str()).collect();
        assert!(names.contains(&"read"), "read must stay reachable: {names:?}");
        assert!(names.contains(&"ls"), "ls must stay reachable: {names:?}");
        assert!(
            !names.contains(&"bash"),
            "a tool outside the definition's list must not be advertised: {names:?}"
        );
        assert!(
            !names.contains(&"write"),
            "a tool outside the definition's list must not be advertised: {names:?}"
        );
    }

    if let Some(previous) = previous {
        crate::env::set_var("HORON_HOME", previous);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

/// A sandboxed store plus the caller whose spawn tree the consult rule uses.
struct ConsultSandbox {
    dir: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
    sessions: Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>,
    provider: Arc<dyn Provider>,
}

impl ConsultSandbox {
    async fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let dir = tempfile::TempDir::new().unwrap();
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        crate::storage::create_store_dir(&dir.path().join("sessions"), false).expect("sessions dir");

        let provider: Arc<dyn Provider> = Arc::new(super::MockProvider);
        let registry = Registry::new(provider.clone()).await;
        let mut session = crate::session::Session::create_with_id("consult_coord".to_string(), None, None);
        session.model = Some("mock".to_string());
        session.working_dir = Some(dir.path().to_string_lossy().into_owned());
        let agent = Agent::new_with_session(provider.clone(), registry, session, None);
        let sessions = Arc::new(RwLock::new(HashMap::from([(
            "consult_coord".to_string(),
            Arc::new(Mutex::new(agent)),
        )])));
        Self {
            dir,
            _guard: guard,
            previous,
            sessions,
            provider,
        }
    }

    fn home(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// A stored session with one visible message. `parent` decides its tree.
    fn saved_session(&self, parent: Option<String>, text: &str) -> crate::session::Session {
        let mut session = crate::session::Session::create(parent, None);
        session.add_message(
            crate::message::Role::Assistant,
            vec![crate::message::ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        );
        session.save().expect("save the fixture session");
        session
    }

    async fn spawn_consult(&self, source_session: String) -> anyhow::Result<String> {
        let state = crate::server::SwarmState::new(HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
        let mcp = Arc::new(crate::mcp::SharedMcpPool::new(crate::mcp::McpConfig::default()));
        let (event_tx, _) = broadcast::channel(16);
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            spawn_swarm_agent(
                "consult_coord",
                "consult-swarm",
                Some(self.home().to_string_lossy().into_owned()),
                Some("What did the source conclude?".to_string()),
                Some(crate::config::SwarmSpawnMode::Headless),
                None,
                None,
                Some("consult".into()),
                Isolation::None,
                IsoApply::Apply,
                // Deliberately the whole profile: the server must narrow the
                // tools itself for a consult.
                SpawnExtras {
                    output_schema: None,
                    allowed_tools: None,
                    source_session: Some(source_session),
                },
                &self.sessions,
                &Arc::new(RwLock::new(String::new())),
                &self.provider,
                &state.members,
                &state.swarms_by_id,
                &state.coordinators,
                &state.plans,
                &Arc::new(RwLock::new(std::collections::VecDeque::new())),
                &Arc::new(AtomicU64::new(0)),
                &event_tx,
                &mcp,
                &Arc::new(RwLock::new(HashMap::new())),
            ),
        )
        .await
        .expect("the consult spawn must not block")
    }
}

impl Drop for ConsultSandbox {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => crate::env::set_var("HORON_HOME", previous),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

/// Every stored file of one session, path-sorted, so a comparison sees any
/// rewrite of the source's bytes.
fn stored_bytes(home: &std::path::Path, session_id: &str) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![home.join("sessions").join(session_id)];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push((path.clone(), std::fs::read(&path).expect("read a stored file")));
            }
        }
    }
    files.sort();
    files
}

/// A consult child starts from a copy of the source transcript, can only read,
/// and leaves the source's stored bytes untouched.
#[tokio::test]
async fn consult_spawn_forks_the_source_and_restricts_the_child() {
    let sandbox = ConsultSandbox::new().await;
    let source = sandbox.saved_session(Some("consult_coord".to_string()), "the source transcript marker");
    let before = stored_bytes(sandbox.home(), &source.id);
    assert!(!before.is_empty(), "the fixture source must be on disk before the fork");

    let child_id = sandbox
        .spawn_consult(source.id.clone())
        .await
        .expect("the consult spawns");
    let child = sandbox
        .sessions
        .read()
        .await
        .get(&child_id)
        .cloned()
        .expect("the child is registered");

    {
        let child_guard = child.lock().await;
        let child_session = child_guard.session_for_split();
        assert_eq!(
            child_session.parent_id.as_deref(),
            Some(source.id.as_str()),
            "a consult child is a fork of the source, not a plain child of the caller"
        );
        let transcript: Vec<String> = child_session
            .active_context()
            .iter()
            .map(|message| message.content_preview())
            .collect();
        assert!(
            transcript
                .iter()
                .any(|line| line.contains("the source transcript marker")),
            "the fork carries the source transcript: {transcript:?}"
        );
        assert!(
            transcript
                .last()
                .is_some_and(|line| line.contains(&source.id) && line.contains("forked")),
            "the fork notice names the source session: {transcript:?}"
        );

        let advertised = child_guard.tool_definitions_for_debug().await;
        let names: Vec<&str> = advertised.iter().map(|definition| definition.name.as_str()).collect();
        // `yield` joins the surface only when the spawn carries an
        // output_schema, and this consult has none, so it is not asserted here.
        for allowed in ["read", "agentgrep", "ls"] {
            assert!(names.contains(&allowed), "{allowed} must stay reachable: {names:?}");
        }
        for refused in ["write", "bash", "edit"] {
            assert!(
                !names.contains(&refused),
                "a consult child must not get {refused}: {names:?}"
            );
        }
    }

    assert_eq!(
        stored_bytes(sandbox.home(), &source.id),
        before,
        "the source session's stored bytes must not change"
    );
}

/// A source outside the caller's spawn tree is refused before a child exists.
#[tokio::test]
async fn consult_spawn_refuses_a_source_outside_the_callers_tree() {
    let sandbox = ConsultSandbox::new().await;
    let unrelated = sandbox.saved_session(None, "someone else's conversation");

    let error = sandbox
        .spawn_consult(unrelated.id.clone())
        .await
        .expect_err("an unrelated tree is refused");
    assert!(
        error.to_string().contains("outside this session's spawn tree"),
        "the refusal names the rule: {error}"
    );
    assert_eq!(
        sandbox.sessions.read().await.len(),
        1,
        "a refused consult creates no child session"
    );
}

/// An unknown source id is refused before a child exists.
#[tokio::test]
async fn consult_spawn_refuses_an_unknown_source() {
    let sandbox = ConsultSandbox::new().await;

    let error = sandbox
        .spawn_consult("session_missing_consult_source".to_string())
        .await
        .expect_err("an unknown source is refused");
    let message = error.to_string();
    assert!(
        message.contains("session_missing_consult_source"),
        "the refusal names the missing id: {message}"
    );
    assert_eq!(
        sandbox.sessions.read().await.len(),
        1,
        "a refused consult creates no child session"
    );
}
