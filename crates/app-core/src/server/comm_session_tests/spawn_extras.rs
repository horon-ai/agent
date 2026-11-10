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
