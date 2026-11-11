//! `[prewalk]` acceptance tests: one per line of issue #70's checklist.

use super::super::*;
use async_trait::async_trait;
use std::sync::Mutex as StdMutex;

use crate::auth::test_sandbox::AuthTestSandbox;
use crate::provider::{EventStream, RouteSelection};

const START_MODEL: &str = "plan-model";
const EXEC_MODEL: &str = "exec-model";
const PROFILE: &str = "prewalk-gw";

fn write_config(sandbox: &AuthTestSandbox, prewalk: &str, plan_mode: bool) {
    std::fs::write(
        sandbox.root().join("config.toml"),
        format!(
            "[tools]\nplan_mode = {plan_mode}\n\n[providers.{PROFILE}]\ndefault_model = \"{START_MODEL}\"\n\n[prewalk]\n{prewalk}"
        ),
    )
    .expect("write config file");
    crate::config::Config::invalidate_cache();
    crate::env::set_var("HORON_AGENT_NAMED_PROVIDER_PROFILE", PROFILE);
}

fn text_stream(text: &str) -> EventStream {
    Box::pin(futures::stream::iter(vec![
        Ok(StreamEvent::TextDelta(text.to_string())),
        Ok(StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        }),
    ]))
}

/// A provider that serves both models and remembers which one each request used.
struct PrewalkProvider {
    model: StdMutex<String>,
    calls: StdMutex<Vec<String>>,
}

impl PrewalkProvider {
    fn new() -> Self {
        Self {
            model: StdMutex::new(START_MODEL.to_string()),
            calls: StdMutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }
}

#[async_trait]
impl Provider for PrewalkProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let model = self.model.lock().expect("model lock").clone();
        self.calls.lock().expect("calls lock").push(model);
        Ok(text_stream("done"))
    }

    fn name(&self) -> &str {
        "prewalk-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        *self.model.lock().expect("model lock") = model.to_string();
        Ok(())
    }

    fn set_route_selection(&self, selection: &RouteSelection) -> anyhow::Result<()> {
        *self.model.lock().expect("model lock") = selection.model.clone();
        Ok(())
    }

    /// The catalog is what `prewalk` checks an `into` value against, so it has
    /// to offer both models this test switches between.
    fn available_models_for_switching(&self) -> Vec<String> {
        vec![START_MODEL.to_string(), EXEC_MODEL.to_string()]
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: StdMutex::new(self.model()),
            calls: StdMutex::new(Vec::new()),
        })
    }
}

fn setup(prewalk: &str, plan_mode: bool) -> (AuthTestSandbox, Arc<PrewalkProvider>, Agent) {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    write_config(&sandbox, prewalk, plan_mode);
    let provider = Arc::new(PrewalkProvider::new());
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let agent = Agent::new(provider_dyn, Registry::empty());
    (sandbox, provider, agent)
}

fn on(into: &str) -> String {
    format!("enabled = true\ninto = \"{into}\"\n")
}

/// One todo write, as the tool would leave it on disk.
fn write_todo(agent: &Agent, id: &str) {
    let todo = crate::todo::TodoItem {
        id: id.to_string(),
        content: format!("step {id}"),
        status: "pending".to_string(),
        priority: "medium".to_string(),
        ..Default::default()
    };
    crate::todo::save_todos(&agent.session.id, &[todo]).expect("todo write");
}

fn prewalk_events(agent: &Agent) -> Vec<String> {
    agent
        .session
        .replay_events
        .iter()
        .filter_map(|event| match &event.kind {
            crate::session::StoredReplayEventKind::Prewalk { into, refused, .. } => {
                Some(format!("{}:{:?}", into.clone().unwrap_or_default(), refused))
            }
            _ => None,
        })
        .collect()
}

async fn run(agent: &mut Agent) {
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "go".into(),
            cache_control: None,
        }],
    );
    agent.run_turn(false).await.expect("turn completes");
}

#[tokio::test]
async fn prewalk_switch_lands_on_the_request_after_the_first_todo_write() {
    let (_home, provider, mut agent) = setup(&on(EXEC_MODEL), false);
    run(&mut agent).await;
    assert_eq!(provider.calls(), vec![START_MODEL.to_string()], "no todo write yet");

    write_todo(&agent, "t1");
    run(&mut agent).await;

    assert_eq!(
        provider.calls(),
        vec![START_MODEL.to_string(), EXEC_MODEL.to_string()],
        "the request after the todo write uses the execution model"
    );
    assert_eq!(agent.provider_model(), EXEC_MODEL);
    assert!(
        agent.session.model.as_deref() == Some(EXEC_MODEL),
        "the switch is persisted"
    );
}

#[tokio::test]
async fn prewalk_does_not_switch_twice_on_a_second_todo_write() {
    let (_home, provider, mut agent) = setup(&on(EXEC_MODEL), false);
    write_todo(&agent, "t1");
    run(&mut agent).await;
    write_todo(&agent, "t2");
    run(&mut agent).await;

    assert_eq!(
        provider.calls(),
        vec![START_MODEL.to_string(), EXEC_MODEL.to_string(), EXEC_MODEL.to_string()],
        "the second todo write changes nothing"
    );
    assert_eq!(prewalk_events(&agent).len(), 1, "one handoff decision per session");
}

#[tokio::test]
async fn prewalk_does_not_switch_again_after_a_resume() {
    let (_home, provider, mut agent) = setup(&on(EXEC_MODEL), false);
    write_todo(&agent, "t1");
    run(&mut agent).await;
    let session_id = agent.session.id.clone();
    agent.save_session().expect("session saved");

    // A resumed process rebuilds the session from the stored file and keeps
    // the same provider, which still refuses to switch twice.
    let mut resumed = Agent::new(provider.clone(), Registry::empty());
    resumed.session = crate::session::Session::load(&session_id).expect("session load");
    write_todo(&resumed, "t2");
    run(&mut resumed).await;

    assert_eq!(
        provider.calls(),
        vec![START_MODEL.to_string(), EXEC_MODEL.to_string(), EXEC_MODEL.to_string()],
        "the resumed session stays on the execution model"
    );
    assert_eq!(prewalk_events(&resumed).len(), 1);
}

#[tokio::test]
async fn prewalk_waits_for_plan_approval_when_plan_mode_is_on() {
    let (_home, _provider, mut agent) = setup(&on(EXEC_MODEL), true);
    let session_id = agent.session.id.clone();
    crate::plan_mode::set_state(&session_id, crate::plan_mode::PlanState::Planning);
    write_todo(&agent, "t1");

    agent.apply_prewalk();
    assert_eq!(
        agent.provider_model(),
        START_MODEL,
        "a todo write during planning does not hand off"
    );

    crate::plan_mode::set_state(&session_id, crate::plan_mode::PlanState::Approved);
    agent.apply_prewalk();
    assert_eq!(agent.provider_model(), EXEC_MODEL, "approval is the handoff point");
    crate::plan_mode::clear_state(&session_id);
}

#[tokio::test]
async fn prewalk_unknown_into_keeps_the_model_and_records_one_refusal() {
    let (_home, _provider, mut agent) = setup(&on("not-in-this-profile"), false);
    write_todo(&agent, "t1");

    agent.apply_prewalk();
    agent.apply_prewalk();

    assert_eq!(
        agent.provider_model(),
        START_MODEL,
        "an unknown model never replaces the session model"
    );
    let events = prewalk_events(&agent);
    assert_eq!(
        events.len(),
        1,
        "the refusal is recorded once, so it is warned about once"
    );
    assert!(events[0].contains("not-in-this-profile"));
}

#[tokio::test]
async fn prewalk_validation_requires_into_when_enabled() {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    std::fs::write(
        sandbox.root().join("config.toml"),
        "[prewalk]\nenabled = true\n".to_string(),
    )
    .expect("write config file");
    crate::config::Config::invalidate_cache();

    let error = crate::config::Config::load_strict().expect_err("enabling without `into` fails");
    let message = error.to_string();
    assert!(
        message.contains("prewalk.into"),
        "the message names the missing key: {message}"
    );

    std::fs::write(
        sandbox.root().join("config.toml"),
        "[prewalk]\nenabled = false\n".to_string(),
    )
    .expect("write config file");
    crate::config::Config::invalidate_cache();
    crate::config::Config::load_strict().expect("an off section needs no model");
}

#[tokio::test]
async fn prewalk_off_keeps_the_session_model() {
    let (_home, provider, mut agent) = setup("", false);
    write_todo(&agent, "t1");

    agent.apply_prewalk();
    run(&mut agent).await;

    assert_eq!(agent.provider_model(), START_MODEL);
    assert_eq!(provider.calls(), vec![START_MODEL.to_string()]);
    assert!(prewalk_events(&agent).is_empty(), "an off section records nothing");
}
