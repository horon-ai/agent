use super::*;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::ModelRoute;
use crate::provider::{EventStream, Provider};
use crate::todo::ConfidenceState;
use crate::tool::Registry;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[test]
fn server_reload_report_preserves_json_fields_and_exit_status_contract() {
    let cases = [
        (false, false, false, false),
        (true, true, true, false),
        (true, false, true, false),
        (true, false, false, true),
    ];

    for (had_listener, already_current, handoff_ready, should_fail) in cases {
        let report = ServerReloadReport {
            socket: "/tmp/horon-agent.sock".to_string(),
            had_listener,
            forced: false,
            reloaded: had_listener && !already_current,
            already_current,
            handoff_ready,
            detail: "test reload outcome".to_string(),
        };

        let json = serde_json::to_value(&report).expect("serialize reload report");
        assert_eq!(json["had_listener"], had_listener);
        assert_eq!(json["already_current"], already_current);
        assert_eq!(json["handoff_ready"], handoff_ready);

        let result = validate_server_reload_report(&report);
        assert_eq!(result.is_err(), should_fail, "report: {json}");
        if should_fail {
            assert!(
                result
                    .expect_err("not-ready handoff must fail")
                    .to_string()
                    .contains("never became ready")
            );
        }
    }
}

#[tokio::test]
async fn server_reload_without_listener_is_a_successful_json_noop() {
    let _env_lock = crate::storage::lock_test_env();
    let _saved = SavedEnv::capture(&["HORON_AGENT_HOME", "HORON_AGENT_SOCKET"]);
    let home = tempfile::tempdir().expect("tempdir");
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    crate::env::set_var("HORON_AGENT_SOCKET", home.path().join("isolated.sock"));

    let mut output = Vec::new();
    let result = run_server_reload_command_to(false, true, &mut output).await;

    result.expect("reload without a listener must be a successful no-op");
    let report: serde_json::Value = serde_json::from_slice(&output).expect("reload --json must emit one JSON report");
    assert_eq!(report["had_listener"], false);
    assert_eq!(report["already_current"], false);
    assert_eq!(report["handoff_ready"], false);
    assert_eq!(report["reloaded"], false);
    assert_eq!(report["forced"], false);
    assert!(
        report["detail"]
            .as_str()
            .is_some_and(|detail| { detail.contains("No running horon-agent server found") })
    );
}

#[tokio::test]
async fn memory_cli_project_import_uses_explicit_directory_and_persists() {
    let _guard = crate::storage::lock_test_env();
    let _saved = SavedEnv::capture(&["HORON_AGENT_HOME"]);
    let temp = tempfile::tempdir().expect("temp dir");
    crate::env::set_var("HORON_AGENT_HOME", temp.path().join("home"));
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).expect("create project");

    let entry = crate::memory::MemoryEntry::new(
        crate::memory::MemoryCategory::Fact,
        "persisted project memory".to_string(),
    );
    let input = temp.path().join("memories.json");
    std::fs::write(
        &input,
        serde_json::to_vec(&vec![entry.clone()]).expect("serialize memory"),
    )
    .expect("write import");

    run_memory_command_for_dir(
        MemorySubcommand::Import {
            input: input.display().to_string(),
            scope: "project".to_string(),
            overwrite: false,
        },
        Some(project.clone()),
    )
    .await
    .expect("import project memory");

    let reloaded = crate::memory::MemoryManager::new()
        .with_project_dir(project)
        .load_project_graph()
        .expect("reload project graph");
    assert_eq!(
        reloaded.get_memory(&entry.id).map(|memory| memory.content.as_str()),
        Some("persisted project memory")
    );
}

#[tokio::test]
async fn memory_cli_project_import_fails_without_durable_project_store() {
    let temp = tempfile::tempdir().expect("temp dir");
    let input = temp.path().join("memories.json");
    std::fs::write(&input, "[]").expect("write import");

    let error = run_memory_command_for_dir(
        MemorySubcommand::Import {
            input: input.display().to_string(),
            scope: "project".to_string(),
            overwrite: false,
        },
        None,
    )
    .await
    .expect_err("project import must require a durable store");

    assert!(error.to_string().contains("without a project directory"));
}

#[tokio::test]
async fn memory_cli_semantic_requires_jev_but_keyword_search_remains_local() {
    let _guard = crate::storage::lock_test_env();
    let keys = [
        "HORON_AGENT_HOME",
        "HORON_AGENT_API_KEY",
        "OPENROUTER_API_KEY",
        "TYPESAFE_API_KEY",
        "AIMLAPI_API_KEY",
        "HORON_AGENT_MEMORY_JEV_PROVIDER",
    ];
    let _saved = SavedEnv::capture(&keys);
    let temp = tempfile::tempdir().expect("temp dir");
    crate::env::set_var("HORON_AGENT_HOME", temp.path().join("home"));
    for key in &keys[1..] {
        crate::env::remove_var(key);
    }
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let manager = crate::memory::MemoryManager::new().with_project_dir(&project);
    manager
        .remember_project(crate::memory::MemoryEntry::new(
            crate::memory::MemoryCategory::Fact,
            "cli-jev-probe without embedding",
        ))
        .unwrap();
    assert!(
        manager
            .list_all()
            .unwrap()
            .iter()
            .all(|entry| entry.embedding.is_none())
    );

    let command = |semantic| MemorySubcommand::Search {
        query: "cli-jev-probe".into(),
        semantic,
    };
    run_memory_command_for_dir(command(false), Some(project.clone()))
        .await
        .expect("local keyword search must remain available without credentials");
    let error = run_memory_command_for_dir(command(true), Some(project))
        .await
        .expect_err("--semantic must report missing Jev access, not silently use embeddings");
    assert!(error.to_string().contains("Jev memory search failed"));
    assert!(!format!("{error:#}").contains("cli-jev-probe"));

    // Scope still comes from the explicit directory, never the process cwd.
    run_memory_command_for_dir(command(true), Some(temp.path().join("empty-project")))
        .await
        .expect("an empty project must not leak another project's candidates");
}

struct SavedEnv {
    vars: Vec<(String, Option<String>)>,
}

impl SavedEnv {
    fn capture(keys: &[&str]) -> Self {
        Self {
            vars: keys
                .iter()
                .map(|key| (key.to_string(), std::env::var(key).ok()))
                .collect(),
        }
    }
}

impl Drop for SavedEnv {
    fn drop(&mut self) {
        for (key, value) in &self.vars {
            if let Some(value) = value {
                crate::env::set_var(key, value);
            } else {
                crate::env::remove_var(key);
            }
        }
    }
}

struct TestProvider;

#[async_trait]
impl Provider for TestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            let _ = tx.send(Ok(StreamEvent::TextDelta("ok".to_string()))).await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

struct FailingTestProvider;

#[async_trait]
impl Provider for FailingTestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!("one-shot sentinel failure"))
    }

    fn name(&self) -> &str {
        "failing-test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

struct OpenRouterSlotProvider;

#[async_trait]
impl Provider for OpenRouterSlotProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!("the provider label test never sends a request"))
    }

    fn name(&self) -> &str {
        "openrouter"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[test]
fn run_report_names_local_profile_instead_of_openrouter_slot() {
    // Ollama/LM Studio share the OpenRouter transport slot (#804).
    assert_eq!(
        run_report_provider_name(&OpenRouterSlotProvider, Some("ollama")),
        "ollama"
    );
    assert_eq!(
        run_report_provider_name(&OpenRouterSlotProvider, Some("lmstudio")),
        "lmstudio"
    );
    // Public OpenRouter and unknown identities keep the slot id.
    assert_eq!(
        run_report_provider_name(&OpenRouterSlotProvider, Some("openrouter")),
        "openrouter"
    );
    assert_eq!(run_report_provider_name(&OpenRouterSlotProvider, None), "openrouter");
    assert_eq!(
        run_report_provider_name(&OpenRouterSlotProvider, Some("  ")),
        "openrouter"
    );
    // Other providers keep their established name.
    assert_eq!(run_report_provider_name(&TestProvider, Some("openai-api")), "test");
}

#[test]
fn test_parse_tailscale_dns_name_trims_trailing_dot() {
    let payload = br#"{"Self":{"DNSName":"yashmacbook.tailabc.ts.net."}}"#;
    let parsed = parse_tailscale_dns_name(payload);
    assert_eq!(parsed.as_deref(), Some("yashmacbook.tailabc.ts.net"));
}

#[test]
fn test_parse_tailscale_dns_name_handles_missing_or_empty() {
    let missing = br#"{"Self":{}}"#;
    assert!(parse_tailscale_dns_name(missing).is_none());

    let empty = br#"{"Self":{"DNSName":"   "}}"#;
    assert!(parse_tailscale_dns_name(empty).is_none());
}

#[test]
fn test_parse_tailscale_dns_name_invalid_json() {
    assert!(parse_tailscale_dns_name(b"not-json").is_none());
}

#[test]
fn collect_cli_model_names_prefers_available_routes_and_dedupes() {
    let routes = vec![
        ModelRoute {
            model: "gpt-5.4".to_string(),
            provider: "OpenAI".to_string(),
            api_method: "openai-oauth".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
            usage: None,
        },
        ModelRoute {
            model: "gpt-5.4".to_string(),
            provider: "auto".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
            usage: None,
        },
        ModelRoute {
            model: "openrouter models".to_string(),
            provider: "—".to_string(),
            api_method: "openrouter".to_string(),
            available: false,
            detail: "OPENROUTER_API_KEY not set".to_string(),
            cheapness: None,
            usage: None,
        },
    ];

    let models = collect_cli_model_names(&routes, vec!["gpt-5.4".to_string(), "claude-sonnet-4".to_string()]);

    assert_eq!(models, vec!["gpt-5.4", "claude-sonnet-4"]);
}

#[test]
fn cli_route_display_uses_typed_api_methods() {
    assert_eq!(cli_api_method_display("openai-oauth"), "oauth");
    assert_eq!(cli_api_method_display("openai-api-key"), "api key");
    assert_eq!(cli_api_method_display("openai-compatible:cerebras"), "api key");
    assert_eq!(cli_api_method_display("mock-auth:profile"), "mock-auth");
    assert_eq!(
        cli_route_provider_display("DeepSeek", "openrouter"),
        "OpenRouter/DeepSeek"
    );
}

fn test_todo(
    id: &str,
    status: &str,
    priority: &str,
    confidence: Option<ConfidenceState>,
    completion_confidence: Option<ConfidenceState>,
) -> crate::todo::TodoItem {
    crate::todo::TodoItem {
        id: id.to_string(),
        content: format!("todo {id}"),
        status: status.to_string(),
        priority: priority.to_string(),
        confidence,
        completion_confidence,
        ..Default::default()
    }
}

#[test]
fn run_auto_poke_followup_targets_below_threshold_todos() {
    let todos = vec![
        test_todo(
            "a",
            "completed",
            "high",
            Some(ConfidenceState::Plausible),
            Some(ConfidenceState::Plausible),
        ),
        test_todo(
            "b",
            "completed",
            "low",
            Some(ConfidenceState::Plausible),
            Some(ConfidenceState::Plausible),
        ),
    ];

    let followup = build_run_auto_poke_follow_up_from_todos(&todos, false, None);

    match followup {
        Some(RunAutoPokeFollowUp::ConfidenceSummary {
            total_todos, message, ..
        }) => {
            assert_eq!(total_todos, 2);
            assert!(message.starts_with(crate::todo::TODO_COMPLETION_CONTINUATION_MESSAGE));
            assert!(message.contains("Validate further:"));
            assert!(message.contains("\"todo a\""));
            assert!(message.contains("\"todo b\""));
            assert!(!message.to_ascii_lowercase().contains("threshold"));
        }
        _ => panic!("expected confidence-summary follow-up"),
    }
}

#[test]
fn run_auto_poke_followup_challenges_abrupt_confidence_once() {
    let mut todo = test_todo(
        "a",
        "completed",
        "high",
        Some(ConfidenceState::Speculative),
        Some(ConfidenceState::Verified),
    );
    todo.confidence_history = vec![ConfidenceState::Speculative, ConfidenceState::Verified];

    let todos = [todo];
    match build_run_auto_poke_follow_up_from_todos(&todos, false, None) {
        Some(RunAutoPokeFollowUp::ConfidenceSummary {
            message,
            confidence_spike_challenge,
            ..
        }) => {
            assert!(confidence_spike_challenge);
            assert!(message.starts_with(crate::todo::TODO_CONFIDENCE_SPIKE_CONTINUATION_MESSAGE));
        }
        _ => panic!("expected confidence-spike challenge"),
    }
    assert!(build_run_auto_poke_follow_up_from_todos(&todos, true, None).is_none());
}

#[test]
fn run_auto_poke_followup_silent_when_confident_and_earned() {
    // All above threshold and no spikes: the old behavior sent an "all good"
    // summary anyway; now we spend no tokens and end the run.
    let todos = vec![
        {
            let mut todo = test_todo(
                "a",
                "completed",
                "high",
                Some(ConfidenceState::Verified),
                Some(ConfidenceState::Verified),
            );
            todo.confidence_history = vec![
                ConfidenceState::Plausible,
                ConfidenceState::Plausible,
                ConfidenceState::Validated,
                ConfidenceState::Verified,
            ];
            todo
        },
        test_todo(
            "b",
            "completed",
            "low",
            Some(ConfidenceState::Validated),
            Some(ConfidenceState::Validated),
        ),
    ];
    assert!(build_run_auto_poke_follow_up_from_todos(&todos, false, None).is_none());
}

#[test]
fn run_auto_poke_followup_prioritizes_incomplete_todos() {
    let todos = vec![
        test_todo(
            "a",
            "completed",
            "high",
            Some(ConfidenceState::Plausible),
            Some(ConfidenceState::Plausible),
        ),
        test_todo("b", "in_progress", "medium", Some(ConfidenceState::Plausible), None),
    ];

    let followup = build_run_auto_poke_follow_up_from_todos(&todos, false, None);

    match followup {
        Some(RunAutoPokeFollowUp::Incomplete { count, message }) => {
            assert_eq!(count, 1);
            assert_eq!(
                message,
                "You have 1 incomplete todo. Continue working, or update the todo tool."
            );
        }
        _ => panic!("expected incomplete-todo follow-up"),
    }
}

#[test]
fn run_auto_poke_treats_completion_synonyms_and_case_as_finished() {
    for status in ["done", "finished", "complete", "Completed", " DONE "] {
        let todos = vec![test_todo(
            "a",
            status,
            "high",
            Some(ConfidenceState::Verified),
            Some(ConfidenceState::Verified),
        )];
        assert!(
            build_run_auto_poke_follow_up_from_todos(&todos, false, None).is_none(),
            "status {status:?} should not trigger an incomplete-todo poke"
        );
    }
}

#[test]
fn run_auto_poke_treats_cancelled_spelling_variants_as_finished() {
    for status in ["cancelled", "canceled", "Cancelled"] {
        let todos = vec![test_todo("a", status, "high", None, None)];
        assert!(
            build_run_auto_poke_follow_up_from_todos(&todos, false, None).is_none(),
            "status {status:?} should not trigger an incomplete-todo poke"
        );
    }
}

/// Headless `horon-agent run` is what the benchmarks and scripted use go through, so
/// the deferred quality review must reach that path too, not only the TUI.
#[test]
fn run_auto_poke_delivers_the_deferred_gate_digest_before_confidence() {
    let todos = vec![test_todo(
        "a",
        "completed",
        "high",
        Some(ConfidenceState::Plausible),
        Some(ConfidenceState::Plausible),
    )];
    // Without a digest, the confidence gate is what fires.
    assert!(matches!(
        build_run_auto_poke_follow_up_from_todos(&todos, false, None),
        Some(RunAutoPokeFollowUp::ConfidenceSummary { .. })
    ));
    // With one, the weak points are reviewed first, since that work can change
    // the very assessments the confidence gate judges.
    match build_run_auto_poke_follow_up_from_todos(&todos, false, Some("review these points".to_string())) {
        Some(RunAutoPokeFollowUp::GateDigest { message }) => {
            assert_eq!(message, "review these points");
        }
        _ => panic!("expected the gate digest to take precedence"),
    }
}

/// Open todos mean the agent is still working, so the review must wait for the
/// turn to actually end rather than interrupting mid-flight.
#[test]
fn run_auto_poke_prefers_incomplete_todos_over_the_gate_digest() {
    let todos = vec![test_todo(
        "a",
        "in_progress",
        "high",
        Some(ConfidenceState::Plausible),
        None,
    )];
    assert!(matches!(
        build_run_auto_poke_follow_up_from_todos(&todos, false, Some("review these points".to_string())),
        Some(RunAutoPokeFollowUp::Incomplete { .. })
    ));
}

/// Regression: the digest is consumed from the log before the follow-up is
/// chosen, so a turn with open todos must not destroy it. Auto-poke iterates
/// many times with open todos on a long run, and each pass used to silently
/// discard the observations, meaning the reminder never survived to delivery.
#[test]
fn open_todos_do_not_consume_the_pending_gate_digest() {
    let _guard = crate::storage::lock_test_env();
    let previous_home = std::env::var_os("HORON_AGENT_HOME");
    let dir = tempfile::TempDir::new().expect("tempdir");
    crate::env::set_var("HORON_AGENT_HOME", dir.path());
    let session = "run-gate-digest-open-todos";

    crate::todo::append_gate_observations(
        session,
        &[crate::todo::GateObservation {
            kind: crate::todo::GateObservationKind::IntentUnderstanding,
            group: None,
            state: Some("partial".to_string()),
        }],
    )
    .expect("append");

    let open = vec![test_todo(
        "a",
        "in_progress",
        "high",
        Some(ConfidenceState::Plausible),
        None,
    )];
    assert!(matches!(
        build_run_auto_poke_follow_up_from_todos(
            &open,
            false,
            take_run_gate_digest_if_turn_ended(session, false, &open),
        ),
        Some(RunAutoPokeFollowUp::Incomplete { .. })
    ));
    assert!(
        !crate::todo::load_gate_observations(session).expect("reload").is_empty(),
        "observations must survive a poke iteration that still has open work"
    );

    // Once the work closes, the reminder is still there to deliver.
    let done = vec![test_todo(
        "a",
        "completed",
        "high",
        Some(ConfidenceState::Plausible),
        Some(ConfidenceState::Verified),
    )];
    match build_run_auto_poke_follow_up_from_todos(
        &done,
        false,
        take_run_gate_digest_if_turn_ended(session, false, &done),
    ) {
        Some(RunAutoPokeFollowUp::GateDigest { message }) => {
            assert!(message.starts_with(crate::todo::TODO_GATE_DIGEST_PREFIX));
        }
        other => panic!("expected the preserved digest to be delivered, got {other:?}"),
    }
    assert!(
        crate::todo::load_gate_observations(session).expect("reload").is_empty(),
        "delivering the digest should consume the log"
    );

    match previous_home {
        Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
}

/// The log must be consumed on delivery, or one turn's observations would leak
/// into the next turn and be raised again against work they never described.
#[test]
fn take_run_gate_digest_consumes_the_log_and_respects_delivery() {
    let _guard = crate::storage::lock_test_env();
    let previous_home = std::env::var_os("HORON_AGENT_HOME");
    let dir = tempfile::TempDir::new().expect("tempdir");
    crate::env::set_var("HORON_AGENT_HOME", dir.path());
    let session = "run-gate-digest";

    crate::todo::append_gate_observations(
        session,
        &[crate::todo::GateObservation {
            kind: crate::todo::GateObservationKind::IntentUnderstanding,
            group: None,
            state: Some("partial".to_string()),
        }],
    )
    .expect("append");

    // Already delivered this turn: no second reminder, and the log is left for
    // the delivering path to have handled.
    assert!(take_run_gate_digest(session, true).is_none());

    let digest = take_run_gate_digest(session, false).expect("unresolved point should surface");
    assert!(digest.starts_with(crate::todo::TODO_GATE_DIGEST_PREFIX));
    // Consumed, so the next turn starts clean.
    assert!(crate::todo::load_gate_observations(session).expect("reload").is_empty());
    assert!(take_run_gate_digest(session, false).is_none());

    match previous_home {
        Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
        None => crate::env::remove_var("HORON_AGENT_HOME"),
    }
}

#[test]
fn run_auto_poke_followup_rechecks_completion_confidence_until_it_passes() {
    let needs_validation = vec![test_todo(
        "a",
        "completed",
        "high",
        Some(ConfidenceState::Plausible),
        Some(ConfidenceState::Plausible),
    )];
    assert!(matches!(
        build_run_auto_poke_follow_up_from_todos(&needs_validation, false, None),
        Some(RunAutoPokeFollowUp::ConfidenceSummary { .. })
    ));
    assert!(matches!(
        build_run_auto_poke_follow_up_from_todos(&needs_validation, false, None),
        Some(RunAutoPokeFollowUp::ConfidenceSummary { .. })
    ));

    let validated = vec![test_todo(
        "a",
        "completed",
        "high",
        Some(ConfidenceState::Plausible),
        Some(ConfidenceState::Verified),
    )];
    // A normal validation gain is not a reason to spend another model turn.
    assert!(build_run_auto_poke_follow_up_from_todos(&validated, false, None).is_none());
    assert!(build_run_auto_poke_follow_up_from_todos(&validated, true, None).is_none());
}

#[test]
fn collect_cli_model_names_falls_back_when_no_routes_are_available() {
    let routes = vec![ModelRoute {
        model: "claude-opus-4-6".to_string(),
        provider: "Anthropic".to_string(),
        api_method: "claude-oauth".to_string(),
        available: false,
        detail: "no credentials".to_string(),
        cheapness: None,
        usage: None,
    }];

    let models = collect_cli_model_names(&routes, vec!["gpt-5.4".to_string()]);

    assert_eq!(models, vec!["claude-opus-4-6", "gpt-5.4"]);
}

#[test]
fn list_cli_providers_includes_auto_and_openai_compatible_profiles() {
    let providers = super::report_info::list_cli_providers();
    assert!(providers.iter().any(|provider| provider.id == "auto"));
    assert!(providers.iter().any(|provider| {
        provider.id == "openai-compatible"
            && provider.display_name == "OpenAI-compatible"
            && provider.auth_kind.as_deref() == Some("api key")
    }));
    assert!(providers.iter().any(|provider| provider.id == "groq"));
    assert!(providers.iter().any(|provider| provider.id == "xai"));
}

#[test]
fn list_cli_providers_includes_conifer() {
    let providers = super::report_info::list_cli_providers();
    let conifer = providers
        .iter()
        .find(|provider| provider.id == "conifer")
        .expect("Conifer should be discoverable through `provider list`");

    assert_eq!(conifer.display_name, "Conifer");
    assert_eq!(conifer.auth_kind.as_deref(), Some("api key"));
}

#[test]
fn version_command_plain_output_includes_core_fields() {
    let report = super::report_info::VersionReport {
        version: "v1.2.3 (abc1234)".to_string(),
        semver: "1.2.3".to_string(),
        base_semver: "1.2.0".to_string(),
        update_semver: "1.2.0".to_string(),
        git_hash: "abc1234".to_string(),
        git_tag: "v1.2.3".to_string(),
        build_time: "2026-03-18 18:00:00 +0000".to_string(),
        git_date: "2026-03-18 17:59:00 +0000".to_string(),
        release_build: false,
        build_profile: "release".to_string(),
        build_features: vec!["pdf".to_string()],
    };
    let text = format!(
        "version\t{}\nsemver\t{}\nbase_semver\t{}\nupdate_semver\t{}\ngit_hash\t{}\ngit_tag\t{}\nbuild_time\t{}\ngit_date\t{}\nrelease_build\t{}\nbuild_profile\t{}\nbuild_features\t{}\n",
        report.version,
        report.semver,
        report.base_semver,
        report.update_semver,
        report.git_hash,
        report.git_tag,
        report.build_time,
        report.git_date,
        report.release_build,
        report.build_profile,
        report.build_features.join(",")
    );

    assert!(text.contains("version\tv1.2.3 (abc1234)"));
    assert!(text.contains("semver\t1.2.3"));
    assert!(text.contains("git_hash\tabc1234"));
    assert!(text.contains("release_build\tfalse"));
    assert!(text.contains("build_profile\trelease"));
    assert!(text.contains("build_features\tpdf"));
}

#[tokio::test]
async fn restore_agent_session_if_requested_restores_resumed_session() {
    let _guard = crate::storage::lock_test_env();

    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut original = crate::agent::Agent::new(provider.clone(), registry);
    let original_session_id = original.session_id().to_string();
    original
        .run_once_capture("seed session for resume test")
        .await
        .expect("seed session");

    let registry = Registry::new(provider.clone()).await;
    let mut resumed = crate::agent::Agent::new(provider, registry);
    let fresh_session_id = resumed.session_id().to_string();
    assert_ne!(fresh_session_id, original_session_id);

    restore_agent_session_if_requested(&mut resumed, Some(&original_session_id)).expect("restore session");

    assert_eq!(resumed.session_id(), original_session_id);
}

#[tokio::test]
async fn one_shot_output_modes_close_sessions_and_clear_active_pid_markers() {
    let _guard = crate::storage::lock_test_env();
    let _saved = SavedEnv::capture(&["HORON_AGENT_HOME", "HORON_AGENT_RUN_AUTO_POKE"]);
    let temp = tempfile::tempdir().expect("tempdir");
    crate::env::set_var("HORON_AGENT_HOME", temp.path());
    crate::env::set_var("HORON_AGENT_RUN_AUTO_POKE", "0");

    for (mode, emit_json, emit_ndjson) in [("plain", false, false), ("json", true, false), ("ndjson", false, true)] {
        let provider: Arc<dyn Provider> = Arc::new(TestProvider);
        let registry = Registry::new(provider.clone()).await;
        let mut agent = crate::agent::Agent::new(provider.clone(), registry);
        let session_id = agent.session_id().to_string();
        let marker = crate::storage::active_pids_dir()
            .expect("active PID directory")
            .join(&session_id);
        assert!(marker.exists(), "{mode} session should start active");

        run_single_message_with_agent(
            &mut agent,
            provider,
            "Return the test response.",
            emit_json,
            emit_ndjson,
        )
        .await
        .unwrap_or_else(|error| panic!("{mode} run failed: {error:#}"));

        assert!(!marker.exists(), "{mode} run left an active PID marker behind");
        let persisted =
            crate::session::Session::load(&session_id).unwrap_or_else(|error| panic!("load {mode} session: {error:#}"));
        assert!(
            matches!(persisted.status, crate::session::SessionStatus::Closed),
            "{mode} run persisted status {:?}",
            persisted.status
        );
        assert!(
            !crate::session::find_recent_crashed_sessions()
                .iter()
                .any(|(id, _)| id == &session_id),
            "{mode} run was rediscovered as a stale-PID crash"
        );
    }
}

#[tokio::test]
async fn resumed_one_shot_closes_the_restored_session() {
    let _guard = crate::storage::lock_test_env();
    let _saved = SavedEnv::capture(&["HORON_AGENT_HOME", "HORON_AGENT_RUN_AUTO_POKE"]);
    let temp = tempfile::tempdir().expect("tempdir");
    crate::env::set_var("HORON_AGENT_HOME", temp.path());
    crate::env::set_var("HORON_AGENT_RUN_AUTO_POKE", "0");

    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut original = crate::agent::Agent::new(provider.clone(), registry);
    original
        .run_once_capture("Seed the resumed one-shot session.")
        .await
        .expect("seed resumed session");
    let session_id = original.session_id().to_string();
    original.mark_closed();

    let registry = Registry::new(provider.clone()).await;
    let mut resumed = crate::agent::Agent::new(provider.clone(), registry);
    restore_agent_session_if_requested(&mut resumed, Some(&session_id)).expect("restore one-shot session");
    let marker = crate::storage::active_pids_dir()
        .expect("active PID directory")
        .join(&session_id);
    assert!(marker.exists(), "restored session should be active while running");

    run_single_message_with_agent(
        &mut resumed,
        provider,
        "Finish the resumed one-shot session.",
        true,
        false,
    )
    .await
    .expect("run resumed one-shot session");

    assert!(!marker.exists(), "resumed run left its active PID marker");
    let persisted = crate::session::Session::load(&session_id).expect("load resumed session");
    assert!(matches!(persisted.status, crate::session::SessionStatus::Closed));
    assert!(
        !crate::session::find_recent_crashed_sessions()
            .iter()
            .any(|(id, _)| id == &session_id),
        "resumed run was rediscovered as a stale-PID crash"
    );
}

#[tokio::test]
async fn one_shot_cleanup_preserves_the_original_command_error() {
    let _guard = crate::storage::lock_test_env();
    let _saved = SavedEnv::capture(&["HORON_AGENT_HOME", "HORON_AGENT_RUN_AUTO_POKE"]);
    let temp = tempfile::tempdir().expect("tempdir");
    crate::env::set_var("HORON_AGENT_HOME", temp.path());
    crate::env::set_var("HORON_AGENT_RUN_AUTO_POKE", "0");

    for (mode, emit_json, emit_ndjson) in [("plain", false, false), ("json", true, false), ("ndjson", false, true)] {
        let provider: Arc<dyn Provider> = Arc::new(FailingTestProvider);
        let registry = Registry::new(provider.clone()).await;
        let mut agent = crate::agent::Agent::new(provider.clone(), registry);
        let session_id = agent.session_id().to_string();
        let marker = crate::storage::active_pids_dir()
            .expect("active PID directory")
            .join(&session_id);

        let error =
            match run_single_message_with_agent(&mut agent, provider, "Fail this run.", emit_json, emit_ndjson).await {
                Ok(()) => panic!("{mode} provider failure should remain the command result"),
                Err(error) => error,
            };

        assert!(
            error.to_string().contains("one-shot sentinel failure"),
            "{mode} changed the original error: {error:#}"
        );
        assert!(!marker.exists(), "failed {mode} run left an active PID marker");
        let persisted = crate::session::Session::load(&session_id)
            .unwrap_or_else(|error| panic!("load failed {mode} session: {error:#}"));
        assert!(matches!(persisted.status, crate::session::SessionStatus::Closed));
    }
}
