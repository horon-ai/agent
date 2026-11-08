//! The request projection: what the provider receives versus what the session
//! stores, and the cost of a projection with no `pre_request` hook configured.

use super::*;
use crate::agent::Agent;
use crate::hooks::pre_request::RequestContext;
use crate::message::{Message, Role, StreamEvent, ToolDefinition};
use horon_agent_message_types::ContentBlock;
use std::sync::{Arc, Mutex};

/// Environment overrides for one test, restored on drop.
struct HookEnv {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl HookEnv {
    fn install(values: &[(&'static str, Option<String>)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect::<Vec<_>>();
        for (key, value) in values {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::invalidate_config_cache();
        Self { previous }
    }
}

impl Drop for HookEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::invalidate_config_cache();
    }
}

#[cfg(unix)]
fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write hook script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod hook script");
    path
}

#[cfg(unix)]
fn pre_request_env(script: &std::path::Path) -> HookEnv {
    pre_request_env_with_timeout(script, 5000)
}

#[cfg(unix)]
fn pre_request_env_with_timeout(script: &std::path::Path, timeout_ms: u64) -> HookEnv {
    HookEnv::install(&[
        (
            "HORON_AGENT_HOOK_PRE_REQUEST",
            Some(script.to_string_lossy().into_owned()),
        ),
        ("HORON_AGENT_HOOK_PRE_REQUEST_TIMEOUT_MS", Some(timeout_ms.to_string())),
    ])
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

#[cfg(unix)]
fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(unix)]
fn message_text(message: &Message) -> String {
    blocks_text(&message.content)
}

/// Every text block the session stored, in order.
#[cfg(unix)]
fn stored_text(agent: &Agent) -> String {
    agent
        .session
        .active_context()
        .iter()
        .map(|message| blocks_text(&message.content))
        .collect::<Vec<_>>()
        .join("|")
}

fn context<'a>(system: &'a str) -> RequestContext<'a> {
    RequestContext {
        session_id: "ses_projection",
        cwd: None,
        turn: 1,
        model: "projection-model",
        system,
    }
}

/// A provider that records the messages of every request and answers with one
/// completed round, so a turn can run without a network.
#[cfg(unix)]
#[derive(Clone, Default)]
struct RecordingProvider {
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
}

#[cfg(unix)]
#[async_trait::async_trait]
impl crate::provider::Provider for RecordingProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        self.requests.lock().expect("request log").push(messages.to_vec());
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta("done".into())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            }),
        ])))
    }

    fn name(&self) -> &str {
        "recording-projection-probe"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn crate::provider::Provider> {
        Arc::new(self.clone())
    }
}

#[cfg(unix)]
fn turn_agent(provider: RecordingProvider, prompt: &str) -> Agent {
    let mut agent = Agent::new(Arc::new(provider), crate::tool::Registry::empty());
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: prompt.to_string(),
            cache_control: None,
        }],
    );
    agent
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_changes_the_request_but_not_the_transcript() {
    // The sandbox takes the test environment lock; taking it twice deadlocks.
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().expect("auth sandbox");
    let temp = tempfile::TempDir::new().expect("temp dir");
    // Uppercases the user message without touching the message shape. The
    // prompt is unique so the substitution can only match the turn's own text,
    // never the system prompt the request also carries.
    let script = write_script(
        temp.path(),
        "upper.sh",
        "#!/bin/sh\nsed -e 's/hello projection probe/HELLO PROJECTION PROBE/'\n",
    );
    let _env = pre_request_env(&script);

    let provider = RecordingProvider::default();
    let recorded = Arc::clone(&provider.requests);
    let mut agent = turn_agent(provider, "hello projection probe");
    agent.run_turn(false).await.expect("the turn completes");

    let requests = recorded.lock().expect("request log");
    assert_eq!(requests.len(), 1, "exactly one provider request");
    let request_text = requests[0].iter().map(message_text).collect::<Vec<_>>().join("|");
    assert!(
        request_text.contains("HELLO PROJECTION PROBE"),
        "the hook rewrote the request body: {request_text}"
    );
    drop(requests);

    let transcript = stored_text(&agent);
    assert!(
        transcript.contains("hello projection probe") && !transcript.contains("HELLO PROJECTION PROBE"),
        "the stored transcript keeps the original text: {transcript}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_empty_stdout_keeps_the_request_identical() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = write_script(temp.path(), "silent.sh", "#!/bin/sh\ncat >/dev/null\nexit 0\n");
    let _env = pre_request_env(&script);

    let messages = (0..3)
        .map(|index| user_message(&format!("message {index}")))
        .collect::<Vec<_>>();
    let before = serde_json::to_string(&messages).expect("serialize the input messages");
    let projected = project(&context("SYSTEM"), &messages)
        .await
        .expect("the projection does not fail");

    assert!(
        projected.is_none(),
        "empty stdout produces no replacement, so the request sends its own messages unchanged"
    );
    assert_eq!(
        serde_json::to_string(&messages).expect("serialize the input messages"),
        before,
        "the projection never rewrites the messages it was given"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pre_request_hook_failure_keeps_the_request_unchanged() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let messages = (0..3)
        .map(|index| user_message(&format!("message {index}")))
        .collect::<Vec<_>>();
    let before = serde_json::to_string(&messages).expect("serialize the input messages");

    for (name, body, timeout_ms) in [
        ("failing.sh", "#!/bin/sh\ncat >/dev/null\nexit 1\n", 5000u64),
        ("garbage.sh", "#!/bin/sh\ncat >/dev/null\nprintf 'not json'\n", 5000),
        ("hang.sh", "#!/bin/sh\nsleep 30\n", 100),
    ] {
        let script = write_script(temp.path(), name, body);
        let _env = pre_request_env_with_timeout(&script, timeout_ms);
        let projected = project(&context("SYSTEM"), &messages)
            .await
            .expect("the projection does not fail");
        assert!(projected.is_none(), "{name} must fail open");
    }

    let missing = std::path::Path::new("/nonexistent/pre-request-hook");
    let _env = pre_request_env_with_timeout(missing, 5000);
    let projected = project(&context("SYSTEM"), &messages)
        .await
        .expect("the projection does not fail");
    assert!(projected.is_none(), "a missing binary must fail open");
    assert_eq!(
        serde_json::to_string(&messages).expect("serialize the input messages"),
        before,
        "a failed hook leaves the request messages exactly as they were"
    );
}

#[tokio::test]
async fn structural_unset_pre_request_is_free() {
    let _guard = crate::storage::lock_test_env();
    let _counters = crate::perf::lock_perf_counters().await;
    let _env = HookEnv::install(&[
        ("HORON_AGENT_HOOK_PRE_REQUEST", None),
        ("HORON_AGENT_HOOK_PRE_REQUEST_TIMEOUT_MS", None),
    ]);

    let messages = (0..8)
        .map(|index| user_message(&format!("message {index}")))
        .collect::<Vec<_>>();
    horon_agent_perf_counters::reset();
    let copies_before = super::request_extra_clone_counters();
    let projected = project(&context("SYSTEM"), &messages)
        .await
        .expect("the projection does not fail");
    let copies_after = super::request_extra_clone_counters();
    let spawns = horon_agent_perf_counters::snapshot().hook_process_spawns;

    assert!(
        projected.is_none(),
        "an unset hook leaves the request borrowing its own messages"
    );
    assert_eq!(spawns, 0, "an unset pre_request hook spawns no process");
    assert_eq!(
        copies_after.0 - copies_before.0,
        0,
        "an unset pre_request hook copies no message"
    );
    std::println!(
        "STRUCTURAL_PERF section=hooks source=request-messages request_messages_in_probe={} hook_process_spawns={} request_message_copies={}",
        messages.len(),
        spawns,
        copies_after.0 - copies_before.0
    );
}
