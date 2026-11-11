#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::{Tool, ToolContext};
use crate::plan_mode::{self, PlanState};
use crate::provider::Provider;
use crate::tool::{Registry, ToolExecutionMode};
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        Err(anyhow::anyhow!("mock provider does not stream"))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockProvider)
    }
}

struct HomeGuard {
    _home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("isolated agent home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        crate::config::Config::invalidate_cache();
        Self { _home: home, previous }
    }

    fn with_config(contents: &str) -> Self {
        let guard = Self::new();
        let path = crate::config::Config::path().expect("config path");
        std::fs::create_dir_all(path.parent().expect("config parent")).expect("create config parent");
        std::fs::write(&path, contents).expect("write config");
        crate::config::Config::invalidate_cache();
        guard
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn context(session_id: &str) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "message".to_string(),
        tool_call_id: "submit-plan-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn plan_of(bytes: usize) -> String {
    "x".repeat(bytes)
}

#[test]
fn submit_plan_requires_plan_mode() {
    let _env = crate::storage::lock_test_env();
    let session = "submit-plan-requires-mode";

    let off = HomeGuard::with_config("[tools]\nplan_mode = false\n");
    plan_mode::clear_state(session);
    runtime().block_on(async {
        let refusal = super::SubmitPlanTool::new()
            .execute(json!({"plan": "# Plan\n\nDo the thing."}), context(session))
            .await
            .expect_err("the switch being off must refuse the call");
        let message = refusal.to_string();
        assert!(
            message.contains("plan mode is off"),
            "the refusal must name the setting: {message}"
        );
        assert_eq!(
            plan_mode::state_for(session),
            PlanState::Off,
            "a refused call must not move the state"
        );
    });
    drop(off);

    let on = HomeGuard::with_config("[tools]\nplan_mode = true\n");
    plan_mode::clear_state(session);
    runtime().block_on(async {
        let refusal = super::SubmitPlanTool::new()
            .execute(json!({"plan": "# Plan\n\nDo the thing."}), context(session))
            .await
            .expect_err("a session that is not planning must be refused");
        assert!(
            refusal.to_string().contains("/plan on"),
            "the refusal must say how to enter plan mode: {refusal}"
        );
        assert_eq!(plan_mode::state_for(session), PlanState::Off);
    });
    drop(on);

    plan_mode::clear_state(session);
}

#[test]
fn submit_plan_rejects_oversized_plan() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nplan_mode = true\n");
    let session = "submit-plan-oversized";
    plan_mode::clear_state(session);
    plan_mode::set_state(session, PlanState::Planning);

    runtime().block_on(async {
        let tool = super::SubmitPlanTool::new();
        let oversized = plan_of(super::MAX_PLAN_BYTES + 1);
        let refusal = tool
            .execute(json!({"plan": oversized}), context(session))
            .await
            .expect_err("a plan over the byte cap must be refused");
        assert!(
            refusal.to_string().contains("over the 256 KiB limit"),
            "the refusal must state the cap: {refusal}"
        );

        let too_many = json!({
            "plan": "# Plan",
            "files": (0..super::MAX_PLAN_FILES + 1)
                .map(|index| format!("src/file{index}.rs"))
                .collect::<Vec<_>>(),
        });
        let refusal = tool
            .execute(too_many, context(session))
            .await
            .expect_err("a plan naming too many files must be refused");
        assert!(refusal.to_string().contains("over the 128 file limit"), "{refusal}");

        let long_path = json!({
            "plan": "# Plan",
            "files": [plan_of(super::MAX_PLAN_FILE_BYTES + 1)],
        });
        let refusal = tool
            .execute(long_path, context(session))
            .await
            .expect_err("a path over the byte cap must be refused");
        assert!(refusal.to_string().contains("over the 1024 byte limit"), "{refusal}");

        assert_eq!(
            plan_mode::state_for(session),
            PlanState::Planning,
            "no refused submission may move the state"
        );
        assert!(
            plan_mode::pending_plan(session).is_none(),
            "no refused submission may leave a pending plan"
        );
    });

    plan_mode::clear_state(session);
}

#[test]
fn submit_plan_moves_state_to_pending_then_approved() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nplan_mode = true\n");
    let session = "submit-plan-pending-then-approved";
    plan_mode::clear_state(session);
    plan_mode::set_state(session, PlanState::Planning);

    let plan = "# Plan\n\n1. Read the module.\n2. Change one function.\n3. Run the test.";
    runtime().block_on(async {
        let output = super::SubmitPlanTool::new()
            .execute(
                json!({"plan": plan, "files": ["crates/base/src/config.rs"]}),
                context(session),
            )
            .await
            .expect("a valid plan must be queued");

        assert_eq!(
            plan_mode::state_for(session),
            PlanState::PendingApproval,
            "a submission must park the session in pending approval"
        );
        assert!(
            !plan_mode::allows_effects(session, crate::tool::ToolEffects::write()),
            "pending approval stays read-only"
        );

        let metadata = output.metadata.expect("the output carries the request id");
        let request_id = metadata["request_id"].as_str().expect("request id").to_string();
        let pending = plan_mode::pending_plan(session).expect("the plan text is kept");
        assert_eq!(pending.text, plan);
        assert_eq!(pending.request_id, request_id);
        assert_eq!(pending.files, vec!["crates/base/src/config.rs".to_string()]);

        assert!(
            super::SubmitPlanTool::new()
                .execute(json!({"plan": "# Second plan"}), context(session))
                .await
                .is_err(),
            "a second submission must wait for the first answer"
        );

        assert_eq!(
            plan_mode::observe_decision(&request_id, true).as_deref(),
            Some(session),
            "an approval recorded on the queue must move this session"
        );
        assert_eq!(plan_mode::state_for(session), PlanState::Approved);
        assert!(
            plan_mode::allows_effects(session, crate::tool::ToolEffects::write()),
            "an approved plan reopens writes"
        );
        assert!(
            plan_mode::pending_plan(session).is_none(),
            "the answered submission is no longer pending"
        );
        assert_eq!(plan_mode::approved_plan(session).as_deref(), Some(plan));
    });

    plan_mode::clear_state(session);
}

#[test]
fn plan_tool_joins_allow_list_only_when_enabled() {
    let _env = crate::storage::lock_test_env();
    let session = "submit-plan-allow-list";

    let off = HomeGuard::with_config("[tools]\nplan_mode = false\n");
    runtime().block_on(async {
        let core = crate::config::ToolConfig {
            profile: crate::config::CORE_TOOL_PROFILE.to_string(),
            ..crate::config::ToolConfig::default()
        };
        let allowed = core
            .selection_with_ambient(&crate::config::AmbientConfig::default())
            .allowed_tools
            .expect("core is an allow-list");
        assert!(
            !allowed.contains(super::SUBMIT_PLAN_TOOL),
            "plan mode off must leave the core allow-list alone, got {allowed:?}"
        );

        let registry = Registry::empty();
        registry.register_plan_tool_if_enabled().await;
        assert!(
            registry.tool_names().await.is_empty(),
            "the tool must be unregistered by default"
        );
    });
    drop(off);

    let on = HomeGuard::with_config("[tools]\nplan_mode = true\n");
    runtime().block_on(async {
        assert!(
            crate::config::config().tools.plan_mode,
            "the switch must be read from the written config"
        );
        let core = crate::config::ToolConfig {
            profile: crate::config::CORE_TOOL_PROFILE.to_string(),
            plan_mode: true,
            ..crate::config::ToolConfig::default()
        };
        let allowed = core
            .selection_with_ambient(&crate::config::AmbientConfig::default())
            .allowed_tools
            .expect("core is an allow-list");
        assert!(
            allowed.contains(super::SUBMIT_PLAN_TOOL),
            "plan mode on must join the tool into the allow-list, got {allowed:?}"
        );
        assert_eq!(
            allowed.len(),
            crate::config::CORE_TOOLS.len() + 2,
            "the plan tool joins the core surface and its dispatcher"
        );

        let none = crate::config::ToolConfig {
            profile: "none".to_string(),
            plan_mode: true,
            ..crate::config::ToolConfig::default()
        };
        let empty = none
            .selection_with_ambient(&crate::config::AmbientConfig::default())
            .allowed_tools
            .expect("none is an empty allow-list");
        assert!(
            empty.is_empty(),
            "an explicitly disabled surface must stay empty, got {empty:?}"
        );

        let registry = Registry::empty();
        registry.register_plan_tool_if_enabled().await;
        let names = registry.tool_names().await;
        assert!(
            names.iter().any(|name| name == super::SUBMIT_PLAN_TOOL),
            "the switch being on must register the tool, got {names:?}"
        );
        let definition = registry
            .definitions(None)
            .await
            .into_iter()
            .find(|definition| definition.name == super::SUBMIT_PLAN_TOOL)
            .expect("the registered tool must reach a definition list");
        assert!(
            definition
                .input_schema
                .get("properties")
                .is_some_and(|properties| properties.get("plan").is_some() && properties.get("files").is_some()),
            "the definition must carry both arguments, got {}",
            definition.input_schema
        );
    });
    drop(on);

    assert!(
        !crate::config::CORE_TOOLS.contains(&super::SUBMIT_PLAN_TOOL),
        "submit_plan must stay outside CORE_TOOLS, got {:?}",
        crate::config::CORE_TOOLS
    );
    assert_eq!(
        crate::config::CORE_TOOLS.len(),
        17,
        "the headless core surface stays at seventeen tools"
    );
    plan_mode::clear_state(session);
}

#[test]
fn plan_command_toggles_and_reports() {
    let _env = crate::storage::lock_test_env();
    let session = "submit-plan-command";

    let off = HomeGuard::with_config("[tools]\nplan_mode = false\n");
    plan_mode::clear_state(session);
    let (data, message) = plan_mode::command(session, &[]).expect("status must work even with the switch off");
    assert_eq!(data["enabled"], false);
    assert_eq!(data["state"], "off");
    assert!(message.contains("Plan mode is off"), "{message}");
    assert!(
        plan_mode::command(session, &["on".to_string()]).is_err(),
        "turning plan mode on must be refused while the setting is off"
    );
    drop(off);

    let on = HomeGuard::with_config("[tools]\nplan_mode = true\n");
    plan_mode::clear_state(session);
    assert_eq!(plan_mode::state_for(session), PlanState::Off);

    let (data, _) = plan_mode::command(session, &["on".to_string()]).expect("on must succeed");
    assert_eq!(data["state"], "planning");
    assert_eq!(data["read_only"], true);
    assert_eq!(plan_mode::state_for(session), PlanState::Planning);

    let (data, message) = plan_mode::command(session, &["status".to_string()]).expect("status");
    assert_eq!(data["state"], "planning");
    assert!(message.contains("read-only"), "{message}");

    assert!(
        plan_mode::command(session, &["approve".to_string()]).is_err(),
        "approving with no pending plan must be refused"
    );
    assert!(
        plan_mode::command(session, &["reject".to_string()]).is_err(),
        "rejecting with no pending plan must be refused"
    );

    plan_mode::set_state(session, PlanState::PendingApproval);
    plan_mode::record_submission(
        session,
        plan_mode::SubmittedPlan {
            request_id: "req-plan-command".to_string(),
            text: "# Plan\n\nDo it.".to_string(),
            files: Vec::new(),
        },
    );
    let (data, message) = plan_mode::command(session, &["approve".to_string()]).expect("approve");
    assert_eq!(data["state"], "approved");
    assert_eq!(data["approved_plan_chars"], "# Plan\n\nDo it.".chars().count());
    assert!(message.contains("pinned"), "{message}");
    assert_eq!(plan_mode::state_for(session), PlanState::Approved);

    let (data, _) = plan_mode::command(session, &["off".to_string()]).expect("off");
    assert_eq!(data["state"], "off");
    assert_eq!(data["enabled"], true);
    assert_eq!(plan_mode::state_for(session), PlanState::Off);
    assert!(
        plan_mode::approved_plan(session).is_none(),
        "leaving plan mode drops the pinned plan"
    );

    assert!(
        plan_mode::command(session, &["typo".to_string()]).is_err(),
        "an unknown action must be refused"
    );
    assert!(
        plan_mode::command(session, &["on".to_string(), "off".to_string()]).is_err(),
        "two arguments must be refused"
    );

    drop(on);
    plan_mode::clear_state(session);
}

#[test]
fn approved_plan_is_pinned_without_changing_static_prompt() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nplan_mode = true\n");
    let session = "submit-plan-pinned";
    plan_mode::clear_state(session);
    plan_mode::set_state(session, PlanState::Planning);

    let plan = "# Plan\n\n1. Touch one function in crates/base/src/config.rs.";
    runtime().block_on(async {
        let registry = Registry::empty();
        let agent = crate::agent::Agent::new_with_session(
            Arc::new(MockProvider) as Arc<dyn Provider>,
            registry,
            crate::session::Session::create_with_id(session.to_string(), None, None),
            Some(HashSet::new()),
        );

        let before = agent.debug_context().await;
        let static_before = before["system_prompt"]["static"]
            .as_str()
            .expect("static prompt")
            .to_string();

        let output = super::SubmitPlanTool::new()
            .execute(json!({"plan": plan}), context(session))
            .await
            .expect("the plan is queued");
        let request_id = output.metadata.as_ref().expect("metadata")["request_id"]
            .as_str()
            .expect("request id")
            .to_string();
        assert!(
            !agent.system_prompt_for_debug().contains(plan),
            "a plan awaiting an answer must not reach the prompt"
        );

        assert_eq!(plan_mode::observe_decision(&request_id, true).as_deref(), Some(session));

        let after = agent.debug_context().await;
        let static_after = after["system_prompt"]["static"]
            .as_str()
            .expect("static prompt")
            .to_string();
        assert_eq!(
            static_before, static_after,
            "pinning a plan must not touch the cached static prefix"
        );
        assert!(
            !static_after.contains(plan),
            "the plan must not be folded into the static prefix"
        );

        let pinned = agent.system_prompt_for_debug();
        assert!(
            pinned.contains("Approved plan"),
            "the approved plan needs its own pinned section: {pinned}"
        );
        assert!(
            pinned.contains(plan),
            "the approved plan text must be in the request context: {pinned}"
        );
    });

    plan_mode::clear_state(session);
}

/// A session parked in `PendingApproval` must always carry the plan that
/// releases it.
///
/// A session in `PendingApproval` must always carry the plan that releases it.
///
/// The ordering inside `execute` exists for this: the plan text is recorded
/// before the state moves, so the two can never disagree. The invariant half is
/// what this asserts.
///
/// It deliberately does not reproduce the rollback. The production arm that
/// restores the previous state when the enqueue fails is unreachable -
/// `request_permission` reports `Queued` unconditionally and has no failure
/// return - so this test proves the invariant and the release path, and nothing
/// about that arm. It is marked defensive in the source rather than described as
/// covered here.
#[test]
fn a_parked_session_always_carries_the_plan_that_releases_it() {
    let _env = crate::storage::lock_test_env();
    let _home = HomeGuard::with_config("[tools]\nplan_mode = true\n");
    let session = "submit-plan-half-state";
    plan_mode::clear_state(session);
    plan_mode::set_state(session, PlanState::Planning);

    runtime().block_on(async {
        super::SubmitPlanTool::new()
            .execute(json!({"plan": "# Plan\n\n1. Do the thing."}), context(session))
            .await
            .expect("a valid plan must be queued");

        assert_eq!(plan_mode::state_for(session), PlanState::PendingApproval);
        assert!(
            plan_mode::pending_plan(session).is_some(),
            "a session in PendingApproval must carry the plan that releases it"
        );

        // What this asserts, and what it does not: the plan is present while the
        // session is parked. It does NOT prove that moving `record_submission`
        // after `set_state` would fail it - that mutation was not run, and a
        // comment claiming otherwise would be the "asserts more than it verified"
        // pattern this review kept finding.
        let pending = plan_mode::pending_plan(session).expect("the plan is present");
        assert!(
            !pending.text.is_empty(),
            "the recorded plan must carry the text the user is approving"
        );

        // The release path works from the parked state, which is the only thing
        // the ordering has to guarantee.
        let released = plan_mode::approve_pending_plan(session);
        assert!(released.is_some(), "a parked session must be releasable");
        assert!(
            plan_mode::allows_effects(session, crate::tool::ToolEffects::write()),
            "approval reopens writes"
        );
    });
}
