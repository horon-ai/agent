use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use horon_agent_agent_runtime::goal_continuation::{
    GOAL_CONTINUATION_CAP, GOAL_CONTINUATION_MESSAGE, GoalContinuation, GoalContinuationInput, decide_goal_continuation,
};

use crate::agent::goal_continuation::{goal_continue_log_line, goal_stop_log_line};
use crate::provider::EventStream;

#[derive(Clone)]
struct CountingProvider {
    requests: Arc<AtomicUsize>,
}

impl CountingProvider {
    fn new() -> Self {
        Self {
            requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn answer(&self) -> EventStream {
        self.requests.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta("working".into())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            }),
        ]))
    }
}

#[async_trait::async_trait]
impl Provider for CountingProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str, _: Option<&str>) -> Result<EventStream> {
        Ok(self.answer())
    }

    async fn complete_split(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        Ok(self.answer())
    }

    fn name(&self) -> &str {
        "goal-continuation-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn new_agent() -> (Agent, CountingProvider) {
    let provider = CountingProvider::new();
    let agent = Agent::new(Arc::new(provider.clone()), Registry::empty());
    (agent, provider)
}

fn attach_goal(agent: &Agent, title: &str) -> crate::goal::Goal {
    let goal = crate::goal::create_goal(
        crate::goal::GoalCreateInput {
            title: title.to_string(),
            scope: crate::goal::GoalScope::Global,
            ..crate::goal::GoalCreateInput::default()
        },
        None,
    )
    .expect("create the goal");
    crate::goal::attach_goal_to_session(agent.session_id(), &goal, None).expect("attach the goal");
    goal
}

fn complete_goal(goal: &crate::goal::Goal) {
    let updated = crate::goal::update_goal(
        &goal.id,
        Some(crate::goal::GoalScope::Global),
        None,
        crate::goal::GoalUpdateInput {
            status: Some(crate::goal::GoalStatus::Completed),
            ..crate::goal::GoalUpdateInput::default()
        },
    )
    .expect("update the goal");
    assert!(updated.is_some(), "the goal must still exist");
}

fn push_continuation(agent: &mut Agent) {
    agent.add_message_with_display_role(
        Role::User,
        vec![ContentBlock::Text {
            text: GOAL_CONTINUATION_MESSAGE.to_string(),
            cache_control: None,
        }],
        Some(crate::session::StoredDisplayRole::System),
    );
}

fn push_assistant_text(agent: &mut Agent, text: &str) {
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    );
}

fn stored_len(agent: &Agent) -> usize {
    agent.session.active_context().len()
}

#[tokio::test]
async fn an_active_goal_with_no_continuations_continues_and_stores_one_message() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");
    let before = stored_len(&agent);

    let continued = agent.maybe_continue_goal().expect("the hook runs");

    assert!(continued, "an active goal below the cap must continue the run");
    assert_eq!(
        stored_len(&agent),
        before + 1,
        "the continuation message is stored exactly once"
    );
    let last = agent
        .session
        .active_context()
        .last()
        .expect("the continuation message is the newest entry");
    assert_eq!(last.role, Role::User);
    assert_eq!(last.display_role, Some(crate::session::StoredDisplayRole::System));
    let stored_text = last.content.iter().find_map(|block| match block {
        ContentBlock::Text { text, .. } => Some(text.as_str()),
        _ => None,
    });
    assert_eq!(stored_text, Some(GOAL_CONTINUATION_MESSAGE));
}

#[tokio::test]
async fn eight_continuations_with_assistant_replies_stop_at_the_cap() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");
    for round in 0..8 {
        push_continuation(&mut agent);
        push_assistant_text(&mut agent, &format!("round {round}"));
    }

    assert_eq!(
        agent.consecutive_goal_continuations(),
        8,
        "the tail scan must count every stored continuation message, which only the \
         all-messages projection exposes"
    );

    let continued = agent.maybe_continue_goal().expect("the hook runs");

    assert!(!continued, "the cap stops the run");
    assert_eq!(
        agent.consecutive_goal_continuations(),
        8,
        "a stopped run stores no further continuation"
    );
}

#[tokio::test]
async fn tool_rounds_in_the_tail_do_not_stop_the_count() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");
    for round in 0..8 {
        push_continuation(&mut agent);
        agent.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: format!("call-{round}"),
                name: "read".to_string(),
                input: serde_json::json!({"file_path": "src/lib.rs"}),
                thought_signature: None,
            }],
        );
        agent.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: format!("call-{round}"),
                content: "file body".to_string(),
                is_error: None,
                verdict: None,
            }],
        );
        push_assistant_text(&mut agent, &format!("round {round} done"));
    }

    assert_eq!(
        agent.consecutive_goal_continuations(),
        8,
        "a tool-result message must be passed over, not treated as the end of the run"
    );

    let continued = agent.maybe_continue_goal().expect("the hook runs");

    assert!(!continued, "eight tool-using rounds still reach the cap");
}

/// Open todos make the todo policy store a synthetic auto-poke between
/// continuations. Stopping the scan there pinned the count at zero, so the cap
/// never stopped the goal loop.
#[tokio::test]
async fn todo_auto_pokes_do_not_reset_the_continuation_cap() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");
    let auto_poke = crate::todo::build_auto_poke_message(2);
    assert!(
        crate::todo::is_auto_poke_message(&auto_poke),
        "the fixture must be a real auto-poke: {auto_poke}"
    );

    for round in 0..8 {
        push_continuation(&mut agent);
        push_assistant_text(&mut agent, &format!("round {round}"));
        agent.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: auto_poke.clone(),
                cache_control: None,
            }],
        );
        agent.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: format!("call-{round}"),
                name: "todo".to_string(),
                input: serde_json::json!({"todos": []}),
                thought_signature: None,
            }],
        );
        agent.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: format!("call-{round}"),
                content: "todos updated".to_string(),
                is_error: None,
                verdict: None,
            }],
        );
    }

    let counted = agent.consecutive_goal_continuations();
    println!("--- manual QA: consecutive continuations with auto-pokes = {counted} ---");
    assert_eq!(counted, 8, "auto-pokes are passed over, not counted as a reset");

    let continued = agent.maybe_continue_goal().expect("the hook runs");

    assert!(!continued, "the cap still stops the run when auto-pokes are present");
}

#[tokio::test]
async fn image_visual_context_does_not_reset_the_continuation_cap() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");

    for round in 0..8 {
        push_continuation(&mut agent);
        push_assistant_text(&mut agent, &format!("round {round}"));
        agent.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!(
                    "<system-reminder>\nA provider-native image generation call created `out-{round}.png`. HoronAgent attached the image pixels as visual context for future turns.\n</system-reminder>"
                ),
                cache_control: None,
            }],
        );
        agent.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: format!("call-{round}"),
                name: "read".to_string(),
                input: serde_json::json!({"file_path": "src/lib.rs"}),
                thought_signature: None,
            }],
        );
        agent.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: format!("call-{round}"),
                content: "file body".to_string(),
                is_error: None,
                verdict: None,
            }],
        );
    }

    let counted = agent.consecutive_goal_continuations();
    println!("--- manual QA: consecutive continuations with image visual context = {counted} ---");
    assert_eq!(
        counted, 8,
        "generated-image visual context is synthetic and must be passed over"
    );

    let continued = agent.maybe_continue_goal().expect("the hook runs");

    assert!(!continued, "the cap still stops the run when image context is present");
}

#[tokio::test]
async fn response_recovery_prompt_does_not_reset_the_continuation_cap() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");
    let recovery_prompt = format!(
        "{} Provide the final answer to the user's last request using the tool results above. Do not call more tools unless absolutely necessary.</system-reminder>",
        Agent::EMPTY_POST_TOOL_CONTINUATION_PREFIX
    );

    for round in 0..8 {
        push_continuation(&mut agent);
        push_assistant_text(&mut agent, &format!("round {round}"));
        agent.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: recovery_prompt.clone(),
                cache_control: None,
            }],
        );
        agent.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: format!("call-{round}"),
                name: "read".to_string(),
                input: serde_json::json!({"file_path": "src/lib.rs"}),
                thought_signature: None,
            }],
        );
        agent.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: format!("call-{round}"),
                content: "file body".to_string(),
                is_error: None,
                verdict: None,
            }],
        );
    }

    let counted = agent.consecutive_goal_continuations();
    println!("--- manual QA: consecutive continuations with the recovery prompt = {counted} ---");
    assert_eq!(
        counted, 8,
        "the empty-response recovery prompt is synthetic and must be passed over"
    );

    let continued = agent.maybe_continue_goal().expect("the hook runs");

    assert!(
        !continued,
        "the cap still stops the run when the recovery prompt is present"
    );
}

#[tokio::test]
async fn real_user_text_still_ends_the_scan() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");
    for round in 0..8 {
        push_continuation(&mut agent);
        push_assistant_text(&mut agent, &format!("round {round}"));
    }
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "please stop and answer me".to_string(),
            cache_control: None,
        }],
    );

    let counted = agent.consecutive_goal_continuations();
    println!("--- manual QA: consecutive continuations behind real user text = {counted} ---");
    assert_eq!(counted, 0, "a genuine user message ends the tail scan");
}

#[tokio::test]
async fn a_completed_goal_stops_the_run() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    let goal = attach_goal(&agent, "ship the release");
    complete_goal(&goal);
    let before = stored_len(&agent);

    let continued = agent.maybe_continue_goal().expect("the hook runs");

    assert!(!continued, "only an active goal continues");
    assert_eq!(stored_len(&agent), before, "a stopped run stores nothing");
}

#[tokio::test]
async fn a_session_without_an_attached_goal_stops_the_run() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    let before = stored_len(&agent);

    let continued = agent.maybe_continue_goal().expect("the hook runs");

    assert!(!continued, "a session with no goal never continues");
    assert_eq!(stored_len(&agent), before, "a stopped run stores nothing");
}

#[tokio::test]
async fn a_goal_attached_to_the_session_is_seen_by_the_hook() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    let goal = crate::goal::create_goal(
        crate::goal::GoalCreateInput {
            title: "ship the release".to_string(),
            scope: crate::goal::GoalScope::Global,
            ..crate::goal::GoalCreateInput::default()
        },
        None,
    )
    .expect("create the goal");
    crate::goal::attach_goal_to_session(agent.session_id(), &goal, None).expect("attach the goal");

    let attached = crate::goal::load_attached_goal(agent.session_id(), None)
        .expect("read the attachment")
        .expect("the attachment resolves to the goal");
    assert_eq!(attached.status, crate::goal::GoalStatus::Active);
    assert!(
        agent.maybe_continue_goal().expect("the hook runs"),
        "the attached goal reaches the hook"
    );
}

#[tokio::test]
async fn a_queued_user_message_stops_the_run_without_being_consumed() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");
    agent.queue_soft_interrupt(
        "please stop and answer me".into(),
        Vec::new(),
        false,
        SoftInterruptSource::User,
    );
    let before = stored_len(&agent);

    let continued = agent.maybe_continue_goal().expect("the hook runs");

    assert!(!continued, "pending user input wins over the goal");
    assert_eq!(
        agent.soft_interrupt_queue().pending_len(),
        1,
        "the queued message stays queued for the next turn"
    );
    assert_eq!(stored_len(&agent), before, "no continuation is stored behind it");
}

#[tokio::test]
async fn the_continuation_logs_its_counter() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");
    let session_id = agent.session_id().to_string();

    assert!(agent.maybe_continue_goal().expect("the hook runs"));
    assert_eq!(agent.consecutive_goal_continuations(), 1);
    println!("{}", goal_continue_log_line(0, &session_id));

    assert!(agent.maybe_continue_goal().expect("the hook runs"));
    assert_eq!(agent.consecutive_goal_continuations(), 2);

    let first = goal_continue_log_line(0, "ses_example");
    assert_eq!(
        first,
        format!("[goal] auto-continue 1/{GOAL_CONTINUATION_CAP} session=ses_example")
    );
    assert_eq!(
        goal_continue_log_line(7, "ses_example"),
        format!("[goal] auto-continue 8/{GOAL_CONTINUATION_CAP} session=ses_example")
    );
    assert_eq!(
        goal_stop_log_line("cap", "ses_example"),
        "[goal] stop reason=cap session=ses_example"
    );
}

const LOG_EMISSION_CHILD: &str = "agent::goal_continuation_tests::the_continuation_emits_its_log_line_child";

fn goal_log_lines(log_path: &std::path::Path, session_id: &str) -> Vec<String> {
    let contents = std::fs::read_to_string(log_path).expect("the logger writes under the sandboxed home");
    contents
        .lines()
        .filter(|line| line.contains("[goal]") && line.contains(session_id))
        .map(str::to_string)
        .collect()
}

/// Proves the hook emits a real `[goal]` log line in both branches.
///
/// `logging::init()` builds one process-global `Logger` whose file is fixed at
/// init time, so an in-process test reads whatever path some other initializer
/// in this binary happened to open. The assertions therefore run in a child of
/// this same test binary, selected with `--exact`: there the logger is
/// guaranteed fresh and `HORON_HOME` is the sandbox that child creates.
#[test]
fn the_continuation_emits_its_log_line() {
    let output = std::process::Command::new(std::env::current_exe().expect("the test binary path"))
        .args(["--exact", LOG_EMISSION_CHILD, "--ignored", "--nocapture"])
        .output()
        .expect("spawn the isolated log-emission child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("--- isolated child {LOG_EMISSION_CHILD} ---\n{stdout}");
    if !stderr.trim().is_empty() {
        println!("--- isolated child stderr ---\n{stderr}");
    }
    assert!(
        output.status.success(),
        "the isolated child must pass, got {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains("1 passed"),
        "the child must run exactly one test; a name mismatch reports zero tests\n{stdout}"
    );
}

#[tokio::test]
#[ignore = "spawned by the_continuation_emits_its_log_line so the process-global logger is fresh"]
async fn the_continuation_emits_its_log_line_child() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    crate::logging::init();
    let log_path = crate::logging::log_path().expect("the sandboxed home resolves a log path");
    assert!(
        log_path.starts_with(sandbox.root()),
        "a fresh process must log under the sandbox, got {}",
        log_path.display()
    );

    let (mut agent, _provider) = new_agent();
    attach_goal(&agent, "ship the release");
    let session_id = agent.session_id().to_string();

    assert!(agent.maybe_continue_goal().expect("the hook runs"));
    assert_eq!(agent.consecutive_goal_continuations(), 1);

    let after_continue = goal_log_lines(&log_path, &session_id);
    println!("--- emitted goal lines after the continue ---");
    for line in &after_continue {
        println!("{line}");
    }
    let continue_line = after_continue
        .iter()
        .find(|line| line.contains(&format!("[goal] auto-continue 1/{GOAL_CONTINUATION_CAP}")))
        .unwrap_or_else(|| panic!("the continue branch must log its counter, got {after_continue:?}"));
    assert!(
        continue_line.contains(&session_id),
        "the continue line names the session, got {continue_line}"
    );

    for round in 1..usize::from(GOAL_CONTINUATION_CAP) {
        push_continuation(&mut agent);
        push_assistant_text(&mut agent, &format!("round {round}"));
    }
    assert_eq!(agent.consecutive_goal_continuations(), GOAL_CONTINUATION_CAP);

    assert!(!agent.maybe_continue_goal().expect("the hook runs"));

    let after_stop = goal_log_lines(&log_path, &session_id);
    println!("--- emitted goal lines after the stop ---");
    for line in &after_stop {
        println!("{line}");
    }
    let stop_line = after_stop
        .iter()
        .find(|line| line.contains("[goal] stop reason="))
        .unwrap_or_else(|| panic!("the stop branch must log its reason, got {after_stop:?}"));
    assert!(
        stop_line.contains("reason=cap"),
        "the cap stops the run, got {stop_line}"
    );
    assert!(
        stop_line.contains(&session_id),
        "the stop line names the session, got {stop_line}"
    );
}

#[tokio::test]
async fn a_turn_loop_with_an_active_goal_keeps_asking_until_the_cap() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut with_goal, goal_provider) = new_agent();
    attach_goal(&with_goal, "ship the release");
    with_goal.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "start the release".into(),
            cache_control: None,
        }],
    );

    with_goal.run_turn(false).await.expect("the run completes");

    let goal_requests = goal_provider.request_count();
    let continuations = with_goal
        .session
        .active_context()
        .iter()
        .filter(|message| {
            message.content.iter().any(|block| match block {
                ContentBlock::Text { text, .. } => text.starts_with(GOAL_CONTINUATION_MESSAGE),
                _ => false,
            })
        })
        .count();
    println!("active goal: requests={goal_requests} continuations={continuations} cap={GOAL_CONTINUATION_CAP}");
    assert_eq!(
        continuations,
        usize::from(GOAL_CONTINUATION_CAP),
        "the run spends the whole cap and no more"
    );
    assert_eq!(
        goal_requests,
        usize::from(GOAL_CONTINUATION_CAP) + 1,
        "one initial request plus one per continuation"
    );

    let (mut without_goal, plain_provider) = new_agent();
    without_goal.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "start the release".into(),
            cache_control: None,
        }],
    );

    without_goal.run_turn(false).await.expect("the run completes");

    let plain_requests = plain_provider.request_count();
    println!("no goal: requests={plain_requests}");
    assert_eq!(plain_requests, 1, "a session with no goal asks once and stops");
    assert!(
        goal_requests > plain_requests,
        "an active goal asks more than a plain session, got {goal_requests} against {plain_requests}"
    );
}

#[tokio::test]
async fn the_streaming_loop_also_continues_an_active_goal() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let (mut with_goal, goal_provider) = new_agent();
    attach_goal(&with_goal, "ship the release");
    with_goal.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "start the release".into(),
            cache_control: None,
        }],
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    with_goal
        .run_turn_streaming_mpsc(tx)
        .await
        .expect("the streaming run completes");
    while rx.try_recv().is_ok() {}

    let goal_requests = goal_provider.request_count();
    println!("streaming with goal: requests={goal_requests}");
    assert_eq!(
        goal_requests,
        usize::from(GOAL_CONTINUATION_CAP) + 1,
        "the streaming loop continues an active goal up to the cap"
    );
    assert_eq!(
        with_goal.turn_state().outcome(),
        Some(TurnOutcome::Stop),
        "the run ends instead of looping forever"
    );

    let (mut without_goal, plain_provider) = new_agent();
    without_goal.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "start the release".into(),
            cache_control: None,
        }],
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    without_goal
        .run_turn_streaming_mpsc(tx)
        .await
        .expect("the streaming run completes");
    while rx.try_recv().is_ok() {}

    assert_eq!(
        plain_provider.request_count(),
        1,
        "a streaming session with no goal asks once and stops"
    );
}

#[test]
fn the_policy_under_test_still_continues_an_active_goal() {
    let input = GoalContinuationInput {
        goal_status: Some("active"),
        is_graceful_shutdown: false,
        pending_user_interrupt: false,
        consecutive_continuations: 0,
    };
    assert_eq!(decide_goal_continuation(&input), GoalContinuation::Continue);
}
