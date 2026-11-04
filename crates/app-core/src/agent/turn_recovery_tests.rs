use super::*;
use crate::agent::turn_recovery_wiring::{TURN_RECOVERY_EXHAUSTED_PREFIX, TURN_RECOVERY_NUDGE_PREFIX};
use crate::auth::test_sandbox::AuthTestSandbox;
use crate::config::TurnRecoveryMode;
use crate::message::StreamEvent;
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc as tokio_mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Clone)]
struct ScriptedStopProvider {
    stops: Vec<ScriptedStop>,
    calls: Arc<AtomicUsize>,
    prompts: Arc<Mutex<Vec<Vec<Message>>>>,
    abort_on_call: Arc<Mutex<Option<(usize, horon_agent_agent_runtime::InterruptSignal)>>>,
}

#[derive(Clone)]
enum ScriptedStop {
    Stop(String, String),
    ToolCall(String, String, String),
    Error(String),
}

impl ScriptedStopProvider {
    fn new(stops: Vec<ScriptedStop>) -> Self {
        Self {
            stops,
            calls: Arc::new(AtomicUsize::new(0)),
            prompts: Arc::new(Mutex::new(Vec::new())),
            abort_on_call: Arc::new(Mutex::new(None)),
        }
    }

    fn arm_abort(&self, call: usize, signal: horon_agent_agent_runtime::InterruptSignal) {
        *self.abort_on_call.lock().unwrap() = Some((call, signal));
    }
}

#[async_trait]
impl Provider for ScriptedStopProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(messages.to_vec());
        if let Some((call, signal)) = self.abort_on_call.lock().unwrap().as_ref()
            && index + 1 == *call
        {
            signal.fire();
        }
        let script = self
            .stops
            .get(index)
            .cloned()
            .unwrap_or(ScriptedStop::Stop("end_turn".into(), "done".into()));
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            match script {
                ScriptedStop::Stop(reason, text) => {
                    if !text.is_empty() {
                        let _ = tx.send(Ok(StreamEvent::TextDelta(text))).await;
                    }
                    let _ = tx
                        .send(Ok(StreamEvent::MessageEnd {
                            stop_reason: Some(reason),
                        }))
                        .await;
                }
                ScriptedStop::ToolCall(reason, name, id) => {
                    let _ = tx
                        .send(Ok(StreamEvent::ToolUseStart {
                            id: id.clone(),
                            name: name.clone(),
                        }))
                        .await;
                    let _ = tx
                        .send(Ok(StreamEvent::ToolInputDeltaFor {
                            id: id.clone(),
                            delta: "{}".into(),
                        }))
                        .await;
                    let _ = tx.send(Ok(StreamEvent::ToolUseEndFor { id })).await;
                    let _ = tx
                        .send(Ok(StreamEvent::MessageEnd {
                            stop_reason: Some(reason),
                        }))
                        .await;
                }
                ScriptedStop::Error(message) => {
                    let _ = tx
                        .send(Ok(StreamEvent::Error {
                            message,
                            retry_after_secs: None,
                        }))
                        .await;
                }
            }
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "scripted-stop"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn write_turn_recovery(root: &std::path::Path, mode: &str) -> TurnRecoveryFlag {
    std::fs::write(
        root.join("config.toml"),
        format!("[features]\nturn_recovery = \"{mode}\"\nparallel_tools = true\n"),
    )
    .expect("the config is written");
    crate::config::invalidate_config_cache();
    TurnRecoveryFlag
}

struct TurnRecoveryFlag;

impl Drop for TurnRecoveryFlag {
    fn drop(&mut self) {
        if let Ok(dir) = std::env::var("HORON_AGENT_HOME") {
            std::fs::write(
                std::path::Path::new(&dir).join("config.toml"),
                "[features]\nturn_recovery = \"conservative\"\nparallel_tools = true\n",
            )
            .ok();
            crate::config::invalidate_config_cache();
        }
    }
}

fn nudges(agent: &Agent) -> Vec<String> {
    agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } if text.starts_with(TURN_RECOVERY_NUDGE_PREFIX) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn transcript_roles_and_text(agent: &Agent) -> Vec<(Role, String)> {
    agent
        .session
        .active_context()
        .iter()
        .map(|message| {
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.clone()),
                    ContentBlock::ToolUse { id, .. } => Some(format!("[tool_use {id}]")),
                    ContentBlock::ToolResult { tool_use_id, .. } => Some(format!("[tool_result {tool_use_id}]")),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" | ");
            (message.role.clone(), text)
        })
        .collect()
}

async fn run_blocking(provider: ScriptedStopProvider, prompt: &str) -> (Agent, Arc<AtomicUsize>) {
    let calls = Arc::clone(&provider.calls);
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::empty();
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: prompt.to_string(),
            cache_control: None,
        }],
    );
    agent.run_turn(false).await.expect("the turn must not error");
    (agent, calls)
}

#[tokio::test]
async fn turn_recovery_continues_once_after_a_length_stop() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "conservative");
    assert_eq!(
        crate::config::config().features.turn_recovery,
        TurnRecoveryMode::Conservative,
        "the default mode must be conservative"
    );

    let provider = ScriptedStopProvider::new(vec![
        ScriptedStop::Stop("Length".into(), "partial answer".into()),
        ScriptedStop::Stop("Stop".into(), "final answer".into()),
    ]);
    let (agent, calls) = run_blocking(provider, "do the task").await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one continuation must cost exactly one extra provider call"
    );
    let injected = nudges(&agent);
    assert_eq!(injected.len(), 1, "exactly one nudge: {injected:?}");
    assert!(
        injected[0].contains("output budget truncated"),
        "the nudge must name the detected reason: {injected:?}"
    );
    let text = agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("final answer"),
        "the recovered turn must deliver the real completion: {text}"
    );
}

#[tokio::test]
async fn turn_recovery_caps_at_two_and_reports_exhaustion() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "conservative");

    let provider = ScriptedStopProvider::new(vec![
        ScriptedStop::Stop("Length".into(), "partial one".into()),
        ScriptedStop::Stop("Length".into(), "partial two".into()),
        ScriptedStop::Stop("Length".into(), "partial three".into()),
    ]);
    let (agent, calls) = run_blocking(provider, "do the task").await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "two continuations cost two extra calls, and the third stop ends the turn"
    );
    assert_eq!(nudges(&agent).len(), 2, "the cap is two continuations per run");
    let transcript = transcript_roles_and_text(&agent);
    let exhausted: Vec<&(Role, String)> = transcript
        .iter()
        .filter(|(_, text)| text.starts_with(TURN_RECOVERY_EXHAUSTED_PREFIX))
        .collect();
    assert_eq!(
        exhausted.len(),
        1,
        "the terminal notice must be stored once: {transcript:?}"
    );
    assert!(
        exhausted[0].1.contains("exhausted") && exhausted[0].1.contains("2 automatic continuations"),
        "the notice must say recovery was exhausted: {:?}",
        exhausted[0].1
    );
}

#[tokio::test]
async fn turn_recovery_off_never_nudges() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "off");
    assert_eq!(crate::config::config().features.turn_recovery, TurnRecoveryMode::Off);

    let provider = ScriptedStopProvider::new(vec![
        ScriptedStop::Stop("Length".into(), "partial answer".into()),
        ScriptedStop::Stop("Stop".into(), "final answer".into()),
    ]);
    let (agent, _calls) = run_blocking(provider, "do the task").await;

    assert!(
        nudges(&agent).is_empty(),
        "with recovery off the auto-continuation must never fire: {:?}",
        nudges(&agent)
    );
}

#[tokio::test]
async fn turn_recovery_never_fires_on_a_provider_error_that_mentions_budget() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "aggressive");

    let provider = ScriptedStopProvider::new(vec![ScriptedStop::Error(
        "upstream error: budget exceeded and length limit hit".into(),
    )]);
    let calls = Arc::clone(&provider.calls);
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::empty();
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "do the task".into(),
            cache_control: None,
        }],
    );

    let result = agent.run_turn(false).await;
    assert!(result.is_err(), "a provider error must fail the turn, got {result:?}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "an error must not be retried by the nudge path"
    );
    assert!(
        nudges(&agent).is_empty(),
        "an error string mentioning budget or length must never produce a nudge: {:?}",
        nudges(&agent)
    );
}

#[tokio::test]
async fn turn_recovery_never_fires_on_a_refusal() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "aggressive");

    let provider = ScriptedStopProvider::new(vec![ScriptedStop::Stop("refusal".into(), "I cannot help".into())]);
    let (agent, calls) = run_blocking(provider, "do the task").await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        nudges(&agent).is_empty(),
        "a refusal is deliberate: {:?}",
        nudges(&agent)
    );
}

#[tokio::test]
async fn turn_recovery_never_fires_on_a_stop_that_carried_tool_calls() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "aggressive");

    let provider = ScriptedStopProvider::new(vec![
        ScriptedStop::ToolCall("length".into(), "capture".into(), "first".into()),
        ScriptedStop::Stop("end_turn".into(), "final answer".into()),
    ]);
    let calls = Arc::clone(&provider.calls);
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::empty();
    registry.register("capture".to_string(), Arc::new(CaptureTool)).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "do the task".into(),
            cache_control: None,
        }],
    );

    agent.run_turn(false).await.expect("the turn must complete");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the tool round plus one follow-up request"
    );
    assert!(
        nudges(&agent).is_empty(),
        "a stop that carried tool calls must never be nudged: {:?}",
        nudges(&agent)
    );
    let tool_uses = agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
        .count();
    let tool_results = agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();
    assert_eq!(tool_uses, 1);
    assert_eq!(tool_results, 1, "the tool call keeps exactly one result");
}

#[derive(Clone)]
struct CaptureTool;

#[async_trait]
impl crate::tool::Tool for CaptureTool {
    fn name(&self) -> &str {
        "capture"
    }

    fn description(&self) -> &str {
        "Capture test input"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({"type":"object"})
    }

    async fn execute(&self, input: Value, _: crate::tool::ToolContext) -> Result<crate::tool::ToolOutput> {
        Ok(crate::tool::ToolOutput::new(format!("captured {input}")))
    }
}

#[tokio::test]
async fn turn_recovery_ignores_unknown_stop_reasons() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "aggressive");

    for reason in [
        "pause_turn",
        "tool_use",
        "some error with length in it",
        "upstream failure",
    ] {
        let provider = ScriptedStopProvider::new(vec![ScriptedStop::Stop(reason.into(), "text".into())]);
        let (agent, _calls) = run_blocking(provider, "do the task").await;
        assert!(
            nudges(&agent).is_empty(),
            "stop reason {reason:?} must not be classified as a recoverable stop"
        );
    }
}

fn prompt_text(prompt: &[Message]) -> String {
    prompt
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn turn_recovery_streaming_loop_continues_once_and_leaves_a_valid_transcript() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "conservative");

    let provider = ScriptedStopProvider::new(vec![
        ScriptedStop::Stop("length".into(), "partial answer".into()),
        ScriptedStop::Stop("end_turn".into(), "final answer".into()),
    ]);
    let calls = Arc::clone(&provider.calls);
    let prompts = Arc::clone(&provider.prompts);
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::empty();
    let mut agent = Agent::new(provider, registry);

    let (tx, mut rx) = tokio_mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("do the task", Vec::new(), None, tx)
        .await
        .expect("the turn must complete");

    let mut streamed = String::new();
    let mut stopped = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ServerEvent::TextDelta { text } => streamed.push_str(&text),
            ServerEvent::TurnStopped { message, .. } => stopped.push(message),
            _ => {}
        }
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one nudge means one extra provider call"
    );
    assert_eq!(nudges(&agent).len(), 1);
    assert!(
        streamed.contains("final answer"),
        "the client must see the real completion: {streamed}"
    );
    assert!(
        stopped.is_empty(),
        "an exhausted recovery must not be reported on the happy path: {stopped:?}"
    );

    let second_prompt = prompt_text(&prompts.lock().unwrap()[1]);
    assert!(
        second_prompt.contains(TURN_RECOVERY_NUDGE_PREFIX),
        "the nudge must reach the provider on the continued request: {second_prompt}"
    );

    let tool_uses = agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
        .count();
    let tool_results = agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();
    assert_eq!(tool_uses, tool_results, "every tool_use needs exactly one tool_result");
}

#[tokio::test]
async fn turn_recovery_streaming_reports_exhaustion_and_skips_the_nudge_after_abort() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "conservative");

    let provider = ScriptedStopProvider::new(vec![
        ScriptedStop::Stop("length".into(), "partial one".into()),
        ScriptedStop::Stop("length".into(), "partial two".into()),
        ScriptedStop::Stop("length".into(), "partial three".into()),
    ]);
    let calls = Arc::clone(&provider.calls);
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::empty();
    let mut agent = Agent::new(provider, registry);

    let (tx, mut rx) = tokio_mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("do the task", Vec::new(), None, tx)
        .await
        .expect("the turn must complete");

    let mut stopped = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::TurnStopped { reason, message, .. } = event {
            stopped.push((reason, message));
        }
    }

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(nudges(&agent).len(), 2);
    assert_eq!(stopped.len(), 1, "exhaustion must be reported once: {stopped:?}");
    assert!(
        stopped[0].1.contains("exhausted"),
        "the stop message must say recovery was exhausted: {stopped:?}"
    );

    let aborted_provider = ScriptedStopProvider::new(vec![
        ScriptedStop::Stop("length".into(), "partial one".into()),
        ScriptedStop::Stop("length".into(), "partial two".into()),
    ]);
    let aborted_calls = Arc::clone(&aborted_provider.calls);
    let aborted: Arc<dyn Provider> = Arc::new(aborted_provider.clone());
    let mut agent = Agent::new(aborted, Registry::empty());
    aborted_provider.arm_abort(2, agent.graceful_shutdown_signal());

    let (tx, mut rx) = tokio_mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("do the task", Vec::new(), None, tx)
        .await
        .expect("an aborted turn ends cleanly");

    assert_eq!(
        aborted_calls.load(Ordering::SeqCst),
        2,
        "the abort lands between the nudge and the next request, so exactly one nudge ran"
    );
    assert_eq!(
        nudges(&agent).len(),
        1,
        "the nudge already injected stays, and no further nudge may follow the abort"
    );
    let mut saw_recovery_stop = false;
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::TurnStopped { message, .. } = event
            && message.starts_with(TURN_RECOVERY_EXHAUSTED_PREFIX)
        {
            saw_recovery_stop = true;
        }
    }
    assert!(!saw_recovery_stop, "an aborted turn must not report a recovery stop");

    let tool_uses = agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
        .count();
    let tool_results = agent
        .session
        .active_context()
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();
    assert_eq!(tool_uses, tool_results, "an abort must leave a valid transcript");
}

#[tokio::test]
async fn turn_recovery_nudge_is_not_a_system_reminder_for_the_tool_result_predicate() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "conservative");

    let provider = ScriptedStopProvider::new(vec![
        ScriptedStop::Stop("length".into(), "partial answer".into()),
        ScriptedStop::Stop("end_turn".into(), "final answer".into()),
    ]);
    let (agent, _calls) = run_blocking(provider, "do the task").await;

    let stored = agent.session.active_context();
    let nudge = stored
        .iter()
        .find(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::Text { text, .. } if text.starts_with(TURN_RECOVERY_NUDGE_PREFIX))
            })
        })
        .expect("the nudge is stored");
    let nudge_only = vec![Message {
        role: nudge.role.clone(),
        content: nudge.content.clone(),
        timestamp: nudge.timestamp,
        tool_duration_ms: nudge.tool_duration_ms,
    }];
    assert!(
        !Agent::messages_end_with_tool_result(&nudge_only),
        "the nudge alone must not look like a tool result to the empty-response predicate"
    );
}

#[tokio::test]
async fn turn_recovery_nudge_is_absent_when_the_class_is_clean() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "aggressive");

    let provider = ScriptedStopProvider::new(vec![ScriptedStop::Stop(
        "end_turn".into(),
        "The migration is complete.".into(),
    )]);
    let (agent, calls) = run_blocking(provider, "do the task").await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a clean stop must not cost another request"
    );
    assert!(
        nudges(&agent).is_empty(),
        "a clean stop must inject nothing into history: {:?}",
        nudges(&agent)
    );
}

#[tokio::test]
async fn turn_recovery_aggressive_only_adds_the_semantic_class() {
    let sandbox = AuthTestSandbox::new().unwrap();
    let _flag = write_turn_recovery(sandbox.root(), "conservative");

    let promise = || {
        ScriptedStopProvider::new(vec![
            ScriptedStop::Stop("end_turn".into(), "I'll now run the full test suite.".into()),
            ScriptedStop::Stop("end_turn".into(), "done".into()),
        ])
    };
    let (conservative_agent, conservative_calls) = run_blocking(promise(), "do the task").await;
    assert_eq!(conservative_calls.load(Ordering::SeqCst), 1);
    assert!(
        nudges(&conservative_agent).is_empty(),
        "conservative must not act on the semantic class"
    );

    let _flag = write_turn_recovery(sandbox.root(), "aggressive");
    let (aggressive_agent, aggressive_calls) = run_blocking(promise(), "do the task").await;
    assert_eq!(aggressive_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        nudges(&aggressive_agent).len(),
        1,
        "aggressive acts on the semantic class"
    );
}

#[test]
fn turn_recovery_mode_parses_and_falls_back() {
    assert_eq!(TurnRecoveryMode::parse("off"), Some(TurnRecoveryMode::Off));
    assert_eq!(
        TurnRecoveryMode::parse(" CONSERVATIVE "),
        Some(TurnRecoveryMode::Conservative)
    );
    assert_eq!(
        TurnRecoveryMode::parse("aggressive"),
        Some(TurnRecoveryMode::Aggressive)
    );
    assert_eq!(TurnRecoveryMode::parse("auto"), None);
    assert_eq!(TurnRecoveryMode::default(), TurnRecoveryMode::Conservative);

    let parsed: crate::config::FeatureConfig =
        serde_json::from_str(r#"{"turn_recovery": "nonsense"}"#).expect("an unknown mode must still parse");
    assert_eq!(
        parsed.turn_recovery,
        TurnRecoveryMode::Conservative,
        "an unknown value must fall back to the default mode"
    );
    let explicit: crate::config::FeatureConfig =
        serde_json::from_str(r#"{"turn_recovery": "off"}"#).expect("a known mode parses");
    assert_eq!(explicit.turn_recovery, TurnRecoveryMode::Off);
}
