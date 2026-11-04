use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

fn entry(id: &str, pattern: &str, body: &str) -> crate::config::StreamRuleConfig {
    crate::config::StreamRuleConfig {
        id: id.to_string(),
        pattern: pattern.to_string(),
        body: body.to_string(),
        ..Default::default()
    }
}

fn scoped_entry(
    id: &str,
    pattern: &str,
    body: &str,
    scope: &[&str],
    delivery: crate::config::StreamRuleDelivery,
) -> crate::config::StreamRuleConfig {
    crate::config::StreamRuleConfig {
        id: id.to_string(),
        pattern: pattern.to_string(),
        body: body.to_string(),
        scope: scope.iter().map(|scope| scope.to_string()).collect(),
        delivery,
        ..Default::default()
    }
}

fn delta(text: &str) -> StreamEvent {
    StreamEvent::TextDelta(text.to_string())
}

fn end() -> StreamEvent {
    StreamEvent::MessageEnd {
        stop_reason: Some("end_turn".to_string()),
    }
}

fn hit_id(action: StreamRuleAction) -> String {
    match action {
        StreamRuleAction::Retry(hit) | StreamRuleAction::Capped(hit) => hit.id,
    }
}

#[test]
fn stream_rule_lookback_window_keeps_one_character_of_boundary_context() {
    let text = format!("{}{}", "y".repeat(LOOKBACK_BYTES), "z".repeat(10));

    let window = lookback_window(&text);

    assert_eq!(
        window.len(),
        LOOKBACK_BYTES + 1,
        "the window is the tail plus one character"
    );
    assert!(
        text.ends_with(window),
        "the window must stay a suffix of the streamed text"
    );
}

#[test]
fn stream_rule_lookback_window_never_splits_a_character() {
    let text = format!("{}가나다", "x".repeat(LOOKBACK_BYTES));

    let window = lookback_window(&text);

    assert!(text.ends_with(window));
    assert!(window.contains("가나다"));
}

#[test]
fn stream_rule_lookback_window_bounds_the_scan() {
    let mut state = TurnStreamRules::from_entries(&[entry("late", "FORBIDDEN", "body")]);

    let beyond = format!("FORBIDDEN{}", "x".repeat(LOOKBACK_BYTES + 100));
    assert!(
        state.check(&beyond).is_none(),
        "a phrase older than the 4 KiB lookback is out of view"
    );

    let inside = "xxFORBIDDENxx".to_string();
    assert_eq!(
        hit_id(state.check(&inside).expect("the phrase is inside the window")),
        "late"
    );
}

#[test]
fn stream_rule_matches_a_phrase_split_across_chunks() {
    let mut state = TurnStreamRules::from_entries(&[entry("split", "FORBIDDEN_PHRASE", "body")]);

    assert!(
        state.check("answer FORBIDDEN_").is_none(),
        "half a phrase is not a match"
    );
    let action = state
        .check("answer FORBIDDEN_PHRASE")
        .expect("the completed phrase matches");

    assert_eq!(hit_id(action), "split");
}

#[test]
fn stream_rule_never_retries_a_rule_that_already_matched() {
    let mut state = TurnStreamRules::from_entries(&[
        entry("first", "BAD", "first body"),
        entry("second", "BAD", "second body"),
    ]);

    let first = state.check("BAD").expect("the earlier entry wins a shared chunk");
    assert_eq!(hit_id(first), "first");
    state.record_injection(&StreamRuleHit {
        id: "first".to_string(),
        body: "first body".to_string(),
    });
    assert_eq!(
        hit_id(state.check("BAD").expect("the later entry is still available")),
        "second"
    );
    state.record_injection(&StreamRuleHit {
        id: "second".to_string(),
        body: "second body".to_string(),
    });

    assert!(state.check("BAD").is_none(), "a matched rule is never retried");
    assert_eq!(state.injections(), 2);
}

#[test]
fn stream_rule_caps_at_three_injections() {
    let mut state = TurnStreamRules::from_entries(&[
        entry("a", "AAA", "a"),
        entry("b", "BBB", "b"),
        entry("c", "CCC", "c"),
        entry("d", "DDD", "d"),
    ]);

    for (id, pattern) in [("a", "AAA"), ("b", "BBB"), ("c", "CCC")] {
        let action = state.check(pattern).expect("the next rule matches");
        assert!(
            matches!(action, StreamRuleAction::Retry(_)),
            "{id} is still under the cap"
        );
        state.record_injection(&StreamRuleHit {
            id: id.to_string(),
            body: id.to_string(),
        });
    }

    match state.check("DDD") {
        Some(StreamRuleAction::Capped(hit)) => assert_eq!(hit.id, "d"),
        _ => panic!("the fourth distinct rule must hit the cap"),
    }
    assert_eq!(state.injections(), MAX_INJECTIONS);
}

#[test]
fn stream_rule_skips_entries_it_cannot_enforce() {
    let state = TurnStreamRules::from_entries(&[
        entry("", "x", "empty id"),
        entry("bad-regex", "(", "unparsable"),
        entry("ok", "x", "good"),
        entry("ok", "y", "duplicate id"),
    ]);

    assert_eq!(state.rule_count(), 1, "only the one enforceable rule survives");
}

#[test]
fn stream_rule_tool_scope_ignores_non_matching_tools() {
    let mut state = TurnStreamRules::from_entries(&[scoped_entry(
        "no-unwrap",
        "unwrap\\(\\)",
        "Use ? instead.",
        &["tool:write"],
        crate::config::StreamRuleDelivery::Interrupt,
    )]);

    assert!(
        state
            .check_tool(
                "edit",
                &serde_json::json!({"file_path": "a.rs", "new_string": "x.unwrap()"})
            )
            .is_none(),
        "a tool:<name> rule only watches the named tool"
    );
    match state
        .check_tool(
            "write",
            &serde_json::json!({"file_path": "a.rs", "content": "x.unwrap()"}),
        )
        .expect("the write call matches")
    {
        ToolRuleAction::Block(hit) => {
            assert_eq!(hit.id, "no-unwrap");
            assert_eq!(blocked_message(&hit), "Blocked by rule \"no-unwrap\": Use ? instead.");
        }
        _ => panic!("interrupt delivery must block"),
    }
    assert!(
        state
            .check_tool("write", &serde_json::json!({"content": "x.unwrap()"}))
            .is_none(),
        "a rule never fires twice in one turn"
    );
}

#[test]
fn stream_rule_tool_scope_honors_path_globs_and_passive_delivery() {
    let mut state = TurnStreamRules::from_entries(&[crate::config::StreamRuleConfig {
        id: "rust-only".to_string(),
        pattern: "TODO".to_string(),
        body: "Resolve TODOs before finishing.".to_string(),
        scope: vec!["tool".to_string()],
        paths: vec!["src/**/*.rs".to_string()],
        delivery: crate::config::StreamRuleDelivery::Passive,
        ..Default::default()
    }]);

    assert!(
        state
            .check_tool(
                "write",
                &serde_json::json!({"file_path": "notes/a.md", "content": "TODO"})
            )
            .is_none(),
        "a path glob outside the pattern does not fire"
    );
    match state
        .check_tool(
            "write",
            &serde_json::json!({"file_path": "src/lib.rs", "content": "// TODO"}),
        )
        .expect("the matching path and input fire")
    {
        ToolRuleAction::Passive(hit) => assert!(
            tool_passive_reminder(&hit).contains("<system-reminder>Rule \"rust-only\": Resolve TODOs"),
            "passive delivery names the rule and body"
        ),
        _ => panic!("passive delivery must not block"),
    }
}

#[test]
fn stream_rule_passive_text_rules_never_retry() {
    let mut state = TurnStreamRules::from_entries(&[scoped_entry(
        "soft-note",
        "HEDGE",
        "State the answer directly.",
        &["text"],
        crate::config::StreamRuleDelivery::Passive,
    )]);

    assert!(state.check("HEDGE").is_none(), "passive text is not an interrupt");
    let hits = state.passive_text_hits("HEDGE");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "soft-note");
    assert!(
        text_passive_reminder(&hits[0])
            .contains("<system-reminder>Rule \"soft-note\" matched: State the answer directly."),
    );
    assert!(
        state.passive_text_hits("HEDGE").is_empty(),
        "a passive rule fires once per turn"
    );
    assert_eq!(state.injections(), 0, "passive delivery spends no injection budget");
}

#[test]
fn stream_rule_repeat_policies_read_and_write_the_ledger() {
    use crate::config::{StreamRuleConfig, StreamRuleRepeat};
    use std::collections::BTreeMap;

    let once = StreamRuleConfig {
        id: "once".to_string(),
        pattern: "FORBIDDEN".to_string(),
        body: "body".to_string(),
        repeat: StreamRuleRepeat::Once,
        ..Default::default()
    };
    let gap = StreamRuleConfig {
        id: "gap".to_string(),
        pattern: "GAPPED".to_string(),
        body: "body".to_string(),
        repeat: StreamRuleRepeat::AfterGap,
        repeat_gap: 3,
        ..Default::default()
    };

    let mut ledger = BTreeMap::new();
    ledger.insert("once".to_string(), 1);
    ledger.insert("gap".to_string(), 10);

    let fresh = TurnStreamRules::from_entries_with_ledger(&[once.clone(), gap.clone()], &ledger, 11);
    assert_eq!(fresh.rule_count(), 0, "once fired and the gap has not elapsed");

    let later = TurnStreamRules::from_entries_with_ledger(&[once, gap], &ledger, 13);
    assert_eq!(later.rule_count(), 1, "the gap elapsed at turn 13");

    let mut state = TurnStreamRules::from_entries_with_ledger(
        &[StreamRuleConfig {
            id: "gap".to_string(),
            pattern: "GAPPED".to_string(),
            body: "body".to_string(),
            repeat: StreamRuleRepeat::AfterGap,
            repeat_gap: 3,
            ..Default::default()
        }],
        &BTreeMap::new(),
        13,
    );
    assert_eq!(hit_id(state.check("GAPPED").expect("the gap rule matches")), "gap");
    state.record_injection(&StreamRuleHit {
        id: "gap".to_string(),
        body: "body".to_string(),
    });
    let mut ledger = BTreeMap::new();
    state.flush_ledger(&mut ledger);
    assert_eq!(ledger.get("gap"), Some(&13), "the fire is recorded at this turn");
}

#[derive(Clone)]
struct RuleProvider {
    rounds: Arc<std::sync::Mutex<VecDeque<Vec<StreamEvent>>>>,
    requests: Arc<AtomicUsize>,
}

impl RuleProvider {
    fn new(rounds: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            rounds: Arc::new(std::sync::Mutex::new(rounds.into())),
            requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn scripted_requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Provider for RuleProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let events = self
            .rounds
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![delta("no scripted round left"), end()]);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }

    fn name(&self) -> &str {
        "stream-rule-test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

struct RuleConfigFile;

impl RuleConfigFile {
    fn write(root: &std::path::Path, toml: &str) -> Self {
        std::fs::write(root.join("config.toml"), toml).expect("the config is written");
        crate::config::invalidate_config_cache();
        Self
    }
}

impl Drop for RuleConfigFile {
    fn drop(&mut self) {
        if let Ok(dir) = std::env::var("HORON_AGENT_HOME") {
            let _ = std::fs::remove_file(std::path::Path::new(&dir).join("config.toml"));
        }
        crate::config::invalidate_config_cache();
    }
}

fn message_text(message: &StoredMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn transcript_entries(agent: &Agent) -> Vec<(String, Option<StoredDisplayRole>, String)> {
    agent
        .session
        .active_context()
        .iter()
        .map(|message| {
            (
                format!("{:?}", message.role),
                message.display_role,
                message_text(message),
            )
        })
        .collect()
}

fn injected_reminders(agent: &Agent) -> Vec<String> {
    transcript_entries(agent)
        .into_iter()
        .filter(|(_, display_role, _)| *display_role == Some(StoredDisplayRole::System))
        .map(|(_, _, text)| text)
        .filter(|text| text.starts_with("<system-reminder>Mid-stream rule"))
        .collect()
}

fn final_answer(agent: &Agent) -> Option<String> {
    transcript_entries(agent)
        .into_iter()
        .rev()
        .find(|(role, display_role, _)| role == "Assistant" && display_role.is_none())
        .map(|(_, _, text)| text)
}

fn drain_events(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn rollback_count(events: &[ServerEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, ServerEvent::RetryRollback { .. }))
        .count()
}

fn injected_event_contents(events: &[ServerEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::SoftInterruptInjected { content, point, .. } if point == STREAM_RULE_POINT => {
                Some(content.clone())
            }
            _ => None,
        })
        .collect()
}

fn turn_stop_messages(events: &[ServerEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ServerEvent::TurnStopped { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

const ONE_RULE: &str = r#"
[features]
stream_rules = true

[[stream_rules]]
id = "banned-phrase"
pattern = "FORBIDDEN_PHRASE"
body = "Never print FORBIDDEN_PHRASE. Rephrase without it."
"#;

const FOUR_RULES: &str = r#"
[features]
stream_rules = true

[[stream_rules]]
id = "rule-a"
pattern = "BANNED_A"
body = "body a"

[[stream_rules]]
id = "rule-b"
pattern = "BANNED_B"
body = "body b"

[[stream_rules]]
id = "rule-c"
pattern = "BANNED_C"
body = "body c"

[[stream_rules]]
id = "rule-d"
pattern = "BANNED_D"
body = "body d"
"#;

const OFF_WITH_RULE: &str = r#"
[features]
stream_rules = false

[[stream_rules]]
id = "banned-phrase"
pattern = "FORBIDDEN_PHRASE"
body = "Never print FORBIDDEN_PHRASE."
"#;

fn seeded_agent(provider: RuleProvider, prompt: &str) -> Agent {
    let mut agent = Agent::new(Arc::new(provider), Registry::empty());
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: prompt.to_string(),
            cache_control: None,
        }],
    );
    agent
}

#[tokio::test]
async fn stream_rule_aborts_and_retries_after_injecting_the_body() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _config = RuleConfigFile::write(sandbox.root(), ONE_RULE);
    let provider = RuleProvider::new(vec![
        vec![delta("Here is FORBIDDEN_"), delta("PHRASE in the answer"), end()],
        vec![delta("clean answer"), end()],
    ]);
    let mut agent = seeded_agent(provider.clone(), "say something");

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    assert_eq!(
        provider.scripted_requests(),
        2,
        "the aborted attempt is re-issued exactly once"
    );
    let events = drain_events(&mut rx);
    assert_eq!(
        rollback_count(&events),
        1,
        "the client is told to discard the aborted text"
    );
    let injected = injected_event_contents(&events);
    assert_eq!(injected.len(), 1, "exactly one injection event");
    assert!(injected[0].contains("Never print FORBIDDEN_PHRASE"));

    let reminders = injected_reminders(&agent);
    assert_eq!(
        reminders.len(),
        1,
        "exactly one injection entry in the transcript: {reminders:?}"
    );
    assert!(reminders[0].contains("banned-phrase"));
    assert_eq!(final_answer(&agent).as_deref(), Some("clean answer"));
    assert!(
        !transcript_entries(&agent)
            .iter()
            .any(|(_, _, text)| text.contains("FORBIDDEN_PHRASE in the answer")),
        "the aborted text must never be committed to the conversation"
    );
}

#[tokio::test]
async fn stream_rule_caps_injections_and_reports_the_rule() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _config = RuleConfigFile::write(sandbox.root(), FOUR_RULES);
    let banned_round = || {
        vec![
            delta("BANNED_A and BANNED_B"),
            delta(" and BANNED_C and BANNED_D"),
            end(),
        ]
    };
    let provider = RuleProvider::new(vec![banned_round(), banned_round(), banned_round(), banned_round()]);
    let mut agent = seeded_agent(provider.clone(), "say something");

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    assert_eq!(
        provider.scripted_requests(),
        4,
        "three retries after three injections, then the capped attempt"
    );
    let events = drain_events(&mut rx);
    assert_eq!(rollback_count(&events), 3, "only the three retried attempts roll back");
    assert_eq!(
        injected_event_contents(&events)
            .iter()
            .filter(|content| content.contains("rule-a"))
            .count(),
        1
    );

    let reminders = injected_reminders(&agent);
    assert_eq!(reminders.len(), 3, "the cap stops at three injections: {reminders:?}");
    let stops = turn_stop_messages(&events);
    assert_eq!(stops.len(), 1, "the cap ends the turn instead of retrying again");
    assert!(
        stops[0].contains("rule-d"),
        "the stop names the rule that hit the cap: {stops:?}"
    );
    assert_eq!(
        transcript_entries(&agent)
            .last()
            .map(|(_, _, text)| text.contains("rule-d")),
        Some(true),
        "the stop notice is the last transcript entry"
    );
}

#[tokio::test]
async fn stream_rule_feature_off_leaves_output_unchanged() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _config = RuleConfigFile::write(sandbox.root(), OFF_WITH_RULE);
    let provider = RuleProvider::new(vec![vec![
        delta("Here is FORBIDDEN_PHRASE"),
        delta(" and the answer continues"),
        end(),
    ]]);
    let mut agent = seeded_agent(provider.clone(), "say something");

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    assert_eq!(provider.scripted_requests(), 1, "the feature being off means no retry");
    let events = drain_events(&mut rx);
    assert_eq!(rollback_count(&events), 0);
    assert!(injected_event_contents(&events).is_empty());
    assert!(injected_reminders(&agent).is_empty());
    assert_eq!(
        final_answer(&agent).as_deref(),
        Some("Here is FORBIDDEN_PHRASE and the answer continues"),
        "the response is emitted exactly as the provider wrote it"
    );
}

#[tokio::test]
async fn stream_rule_manual_qa_transcript_order() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _config = RuleConfigFile::write(sandbox.root(), ONE_RULE);
    let provider = RuleProvider::new(vec![
        vec![delta("Here is FORBIDDEN_"), delta("PHRASE in the answer"), end()],
        vec![delta("clean answer"), end()],
    ]);
    let mut agent = seeded_agent(provider.clone(), "say something");

    let (out, mut rx) = mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(out).await.unwrap();

    for (index, event) in drain_events(&mut rx).iter().enumerate() {
        match event {
            ServerEvent::TextDelta { text } => std::println!("QA stream_rule event[{index}] text_delta={text}"),
            ServerEvent::RetryRollback { attempt, max } => {
                std::println!("QA stream_rule event[{index}] retry_rollback attempt={attempt} max={max}")
            }
            ServerEvent::SoftInterruptInjected { content, point, .. } => {
                std::println!("QA stream_rule event[{index}] injected point={point} content={content}")
            }
            ServerEvent::MessageEnd { stop_reason } => {
                std::println!("QA stream_rule event[{index}] message_end stop_reason={stop_reason:?}")
            }
            other => std::println!("QA stream_rule event[{index}] other={other:?}"),
        }
    }
    for (index, (role, display_role, text)) in transcript_entries(&agent).iter().enumerate() {
        std::println!("QA stream_rule transcript[{index}] role={role} display_role={display_role:?} text={text}");
    }
    std::println!("QA stream_rule injections={}", injected_reminders(&agent).len());
    std::println!("QA stream_rule provider_requests={}", provider.scripted_requests());

    assert_eq!(final_answer(&agent).as_deref(), Some("clean answer"));
    assert_eq!(injected_reminders(&agent).len(), 1);
}
