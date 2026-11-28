//! Every request of a session starts with every message of the previous one,
//! while the turn context and the memory summary still reach the model.

use super::*;
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use anyhow::Result;
use horon_agent_message_types::{ContentBlock, StreamEvent, ToolDefinition};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// The turn context the model is under for one request: the newest
/// turn-context reminder in the transcript, or what still rides in the dynamic
/// system prompt. Shared by the loop tests that assert on the turn context.
pub(crate) fn context_seen(messages: &[Message], system_dynamic: &str) -> String {
    let stored = messages
        .iter()
        .rev()
        .find_map(|message| match message.content.as_slice() {
            [ContentBlock::Text { text, .. }] if text.starts_with("<system-reminder>\n# Turn context\n") => {
                Some(text.clone())
            }
            _ => None,
        });
    format!("{system_dynamic}{}", stored.unwrap_or_default())
}

/// Each request's messages as sent, with the turn context the model was under.
type Recorded = Vec<(Vec<Value>, String)>;

#[derive(Clone, Default)]
struct RecordingProvider {
    rounds: Arc<Mutex<VecDeque<Vec<StreamEvent>>>>,
    requests: Arc<Mutex<Recorded>>,
}

impl RecordingProvider {
    fn new(rounds: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            rounds: Arc::new(Mutex::new(rounds.into())),
            requests: Arc::default(),
        }
    }

    fn next_events(&self) -> Vec<Result<StreamEvent>> {
        let events = self.rounds.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![
                StreamEvent::TextDelta("done".into()),
                StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".into()),
                },
            ]
        });
        events.into_iter().map(Ok).collect()
    }
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::iter(self.next_events())))
    }

    async fn complete_split(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
        _: &str,
        system_dynamic: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        // A turn context in the provider's dynamic channel would be inserted in
        // front of the current turn again, which is the rewrite this guards.
        assert!(
            system_dynamic.is_empty(),
            "the turn context rides in the transcript: {system_dynamic:?}"
        );
        let sent = messages
            .iter()
            .map(|message| json!({ "role": message.role, "content": message.content }))
            .collect();
        let seen = context_seen(messages, system_dynamic);
        self.requests.lock().unwrap().push((sent, seen));
        Ok(Box::pin(futures::stream::iter(self.next_events())))
    }

    fn name(&self) -> &str {
        "turn-context-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn tool_round(id: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolUseStart {
            id: id.into(),
            name: "missing_tool".into(),
        },
        StreamEvent::ToolInputDeltaFor {
            id: id.into(),
            delta: "{}".into(),
        },
        StreamEvent::ToolUseEndFor { id: id.into() },
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".into()),
        },
    ]
}

fn user_text(agent: &mut Agent, text: &str) {
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: text.into(),
            cache_control: None,
        }],
    );
}

/// Two turns: notes are set before the first, one tool round runs, the notes
/// change and a new user message arrives before the second. Returns every
/// request the provider saw.
async fn two_turns(streaming: bool) -> Recorded {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    std::fs::write(sandbox.root().join("config.toml"), "[tools]\ncontext_notes = true\n").unwrap();
    crate::config::Config::invalidate_cache();

    let provider = RecordingProvider::new(vec![tool_round("call-1")]);
    let mut agent = Agent::new(Arc::new(provider.clone()), Registry::empty());
    crate::context_notes::save(&agent.session.id, "- step one").unwrap();
    for (prompt, notes) in [("first task", None), ("second task", Some("- step two"))] {
        if let Some(notes) = notes {
            crate::context_notes::save(&agent.session.id, notes).unwrap();
        }
        user_text(&mut agent, prompt);
        if streaming {
            let (out, _events) = tokio::sync::mpsc::unbounded_channel();
            agent.run_turn_streaming_mpsc(out).await.unwrap();
        } else {
            agent.run_turn(false).await.unwrap();
        }
    }
    provider.requests.lock().unwrap().clone()
}

fn assert_each_request_extends_the_previous(requests: &[(Vec<Value>, String)]) {
    for (index, pair) in requests.windows(2).enumerate() {
        let (previous, next) = (&pair[0].0, &pair[1].0);
        assert!(
            next.len() > previous.len() && next[..previous.len()] == previous[..],
            "request {} rewrote request {index}:\n{previous:#?}\n--- next ---\n{next:#?}",
            index + 1
        );
    }
}

fn assert_requests_only_grow(requests: &[(Vec<Value>, String)]) {
    assert_eq!(requests.len(), 3, "a tool round and its answer, then the second turn");
    assert_each_request_extends_the_previous(requests);
    assert!(requests[0].1.contains("- step one"), "{:?}", requests[0].1);
    assert!(
        requests[2].1.contains("- step two") && !requests[2].1.contains("- step one"),
        "the newest turn context carries the changed notes: {:?}",
        requests[2].1
    );
    let copies = requests[2]
        .0
        .iter()
        .filter(|message| message.to_string().contains("# Turn context"))
        .count();
    assert_eq!(copies, 2, "one copy per distinct context, none for an unchanged one");
}

#[tokio::test]
async fn requests_only_grow_across_turns_on_the_non_streaming_path() {
    assert_requests_only_grow(&two_turns(false).await);
}

#[tokio::test]
async fn requests_only_grow_across_turns_on_the_streaming_path() {
    assert_requests_only_grow(&two_turns(true).await);
}

/// A tool that only reports it ran, registered under the given name.
struct NamedTool(&'static str);

#[async_trait::async_trait]
impl crate::tool::Tool for NamedTool {
    fn name(&self) -> &str {
        self.0
    }

    fn description(&self) -> &str {
        "turn-context test double"
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    async fn execute(&self, _: Value, _: crate::tool::ToolContext) -> Result<crate::tool::ToolOutput> {
        Ok(crate::tool::ToolOutput::new("ran"))
    }
}

#[tokio::test]
async fn the_batch_nudge_stays_in_every_later_request() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let registry = Registry::empty();
    for name in ["batch", "probe"] {
        registry.register(name.to_string(), Arc::new(NamedTool(name))).await;
    }
    // Four rounds of one call each: the nudge is owed after the third.
    let rounds = (1..=4)
        .map(|round| {
            let mut events = tool_round(&format!("call-{round}"));
            if let StreamEvent::ToolUseStart { name, .. } = &mut events[0] {
                *name = "probe".into();
            }
            events[1] = StreamEvent::ToolInputDeltaFor {
                id: format!("call-{round}"),
                delta: json!({ "round": round }).to_string(),
            };
            events
        })
        .collect();
    let provider = RecordingProvider::new(rounds);
    let mut agent = Agent::new(Arc::new(provider.clone()), registry);
    user_text(&mut agent, "probe four times");
    agent.run_turn(false).await.unwrap();

    let requests = provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 5, "four tool rounds and the answer");
    assert_each_request_extends_the_previous(&requests);
    let nudged =
        |request: &(Vec<Value>, String)| request.0.iter().any(|m| m.to_string().contains("use the batch tool"));
    assert!(!nudged(&requests[2]), "no nudge before the third single-call round");
    assert!(
        nudged(&requests[3]) && nudged(&requests[4]),
        "the nudge is sent and kept"
    );
}

#[tokio::test]
async fn the_same_memory_summary_is_stored_and_sent_once() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let previous = std::env::var_os("HORON_AGENT_PERSIST_MEMORY_INJECTIONS");
    crate::env::remove_var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS");
    crate::config::invalidate_config_cache();

    let mut agent = Agent::new(Arc::new(RecordingProvider::default()), Registry::empty());
    let memory = crate::memory::PendingMemory {
        prompt: "# Memory Summary\n\n1. Prefer small commits".to_string(),
        display_prompt: None,
        computed_at: std::time::Instant::now(),
        count: 1,
        memory_ids: Vec::new(),
    };
    let before = agent.session.active_context().len();

    let transcript = agent.session.messages_for_provider();
    let (_, persisted) = agent
        .prepare_memory_injection_message(&transcript, &memory)
        .expect("the default stores a new summary");
    assert!(persisted);
    let transcript = agent.session.messages_for_provider();
    assert!(
        agent.prepare_memory_injection_message(&transcript, &memory).is_none(),
        "the next request already carries the same summary"
    );
    assert_eq!(agent.session.active_context().len(), before + 1);

    match previous {
        Some(value) => crate::env::set_var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS", value),
        None => crate::env::remove_var("HORON_AGENT_PERSIST_MEMORY_INJECTIONS"),
    }
    crate::config::invalidate_config_cache();
}
