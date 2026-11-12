/// A spawn from a named definition carries its tools, model, effort, schema,
/// and role prompt, and the caller's explicit values still win.
#[tokio::test]
async fn spawn_request_resolves_a_named_agent_definition() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _home = EnvGuard::set("HORON_HOME", home.path());

    let project = tempfile::tempdir().expect("project");
    let definitions = project.path().join(".horon/agents");
    std::fs::create_dir_all(&definitions).expect("definition dir");
    std::fs::write(
        definitions.join("scout.md"),
        "---\nname: scout\ndescription: Read-only investigator\ntools: [read, agentgrep, ls]\nmodel: gpt-5.5\neffort: low\noutput_schema:\n  type: object\n  properties:\n    findings:\n      type: array\n---\n\nInvestigate and report.\n",
    )
    .expect("write definition");

    let ctx = test_ctx("coord", project.path());
    let params: CommunicateInput = serde_json::from_value(json!({
        "action": "spawn",
        "label": "scout",
        "agent": "scout",
        "prompt": "map the read path"
    }))
    .expect("spawn input should deserialize");

    let request = super::agent_spawn::spawn_request(&ctx, &params, "scout".to_string()).expect("definition resolves");
    let Request::CommSpawn {
        initial_message,
        model,
        effort,
        output_schema,
        allowed_tools,
        ..
    } = request
    else {
        panic!("expected a spawn request");
    };
    assert_eq!(
        initial_message.as_deref(),
        Some("Investigate and report.\n\n---\n\nmap the read path")
    );
    assert_eq!(model.as_deref(), Some("gpt-5.5"));
    assert_eq!(effort.as_deref(), Some("low"));
    assert_eq!(
        allowed_tools,
        Some(vec!["read".to_string(), "agentgrep".to_string(), "ls".to_string()])
    );
    assert!(
        output_schema.is_some(),
        "a definition's output_schema must reach the child"
    );

    // An explicit spawn parameter overrides the definition.
    let explicit: CommunicateInput = serde_json::from_value(json!({
        "action": "spawn",
        "label": "scout",
        "agent": "scout",
        "model": "claude-opus-4-6",
        "effort": "max"
    }))
    .expect("spawn input should deserialize");
    let request = super::agent_spawn::spawn_request(&ctx, &explicit, "scout".to_string()).expect("definition resolves");
    let Request::CommSpawn { model, effort, .. } = request else {
        panic!("expected a spawn request");
    };
    assert_eq!(model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(effort.as_deref(), Some("max"));
}

/// An unknown definition name fails before any request is built.
#[tokio::test]
async fn spawn_request_rejects_an_unknown_agent_name() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _home = EnvGuard::set("HORON_HOME", home.path());
    let project = tempfile::tempdir().expect("project");

    let ctx = test_ctx("coord", project.path());
    let params: CommunicateInput = serde_json::from_value(json!({
        "action": "spawn",
        "label": "nobody",
        "agent": "missing"
    }))
    .expect("spawn input should deserialize");

    let error =
        super::agent_spawn::spawn_request(&ctx, &params, "nobody".to_string()).expect_err("unknown agent fails");
    assert!(error.to_string().contains("Unknown agent 'missing'"), "got: {error}");
}

/// Spawning without `agent` builds the same request as before: no allow list,
/// no definition prompt.
#[tokio::test]
async fn spawn_request_without_agent_is_unchanged() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _home = EnvGuard::set("HORON_HOME", home.path());
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx("coord", project.path());
    let params: CommunicateInput = serde_json::from_value(json!({
        "action": "spawn",
        "label": "plain",
        "prompt": "do the thing"
    }))
    .expect("spawn input should deserialize");

    let request = super::agent_spawn::spawn_request(&ctx, &params, "plain".to_string()).expect("plain spawn");
    let Request::CommSpawn {
        initial_message,
        model,
        effort,
        output_schema,
        allowed_tools,
        ..
    } = request
    else {
        panic!("expected a spawn request");
    };
    assert_eq!(initial_message.as_deref(), Some("do the thing"));
    assert_eq!(model, None);
    assert_eq!(effort, None);
    assert_eq!(output_schema, None);
    assert_eq!(allowed_tools, None);
}

/// The discovery action lists definitions and names the directories.
#[tokio::test]
async fn list_agent_defs_action_lists_discovered_definitions() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _home = EnvGuard::set("HORON_HOME", home.path());
    let project = tempfile::tempdir().expect("project");
    let definitions = project.path().join(".horon/agents");
    std::fs::create_dir_all(&definitions).expect("definition dir");
    std::fs::write(
        definitions.join("scout.md"),
        "---\nname: scout\ndescription: Read-only investigator\ntools: [read, ls]\n---\n\nInvestigate.\n",
    )
    .expect("write definition");

    let ctx = test_ctx("coord", project.path());
    let output = CommunicateTool::new()
        .execute(json!({"action": "list_agent_defs"}), ctx)
        .await
        .expect("discovery action should succeed");
    assert!(output.output.contains("scout"), "got: {}", output.output);
    assert!(
        output.output.contains("Read-only investigator"),
        "got: {}",
        output.output
    );
    assert!(output.output.contains("tools: read, ls"), "got: {}", output.output);
}

/// `list_agents` still means `list`: the discovery action has its own name.
#[test]
fn list_agents_still_resolves_to_list() {
    assert_eq!(super::canonical_swarm_action("list_agents"), "list");
    assert_eq!(super::canonical_swarm_action("list_agent_defs"), "list_agent_defs");
    assert_eq!(super::canonical_swarm_action("agent_defs"), "list_agent_defs");
}
/// A consult spawn names the source session and narrows the child to the
/// read-only tool set.
#[tokio::test]
async fn spawn_request_with_source_session_uses_the_consult_tools() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _home = EnvGuard::set("HORON_HOME", home.path());
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx("consult", project.path());
    let params: CommunicateInput = serde_json::from_value(json!({
        "action": "spawn",
        "label": "consult",
        "source_session": "session_source",
        "prompt": "What did we decide?"
    }))
    .expect("spawn input should deserialize");

    let request = super::agent_spawn::spawn_request(&ctx, &params, "consult".to_string()).expect("consult request");
    let Request::CommSpawn {
        source_session,
        allowed_tools,
        initial_message,
        ..
    } = request
    else {
        panic!("expected a spawn request");
    };
    assert_eq!(source_session.as_deref(), Some("session_source"));
    assert_eq!(initial_message.as_deref(), Some("What did we decide?"));
    assert_eq!(
        allowed_tools,
        Some(vec![
            "read".to_string(),
            "agentgrep".to_string(),
            "ls".to_string(),
            "yield".to_string(),
        ])
    );
}

/// A consult wins over a definition's tools: a definition cannot hand a consult
/// child a tool that writes.
#[tokio::test]
async fn consult_source_session_replaces_a_definitions_tools() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _home = EnvGuard::set("HORON_HOME", home.path());
    let project = tempfile::tempdir().expect("project");
    let definitions = project.path().join(".horon/agents");
    std::fs::create_dir_all(&definitions).expect("definition dir");
    std::fs::write(
        definitions.join("scout.md"),
        "---\nname: scout\ntools: [read, write, bash]\n---\n\nInvestigate.\n",
    )
    .expect("write definition");

    let ctx = test_ctx("consult", project.path());
    let params: CommunicateInput = serde_json::from_value(json!({
        "action": "spawn",
        "label": "consult",
        "agent": "scout",
        "source_session": "session_source"
    }))
    .expect("spawn input should deserialize");

    let request = super::agent_spawn::spawn_request(&ctx, &params, "consult".to_string()).expect("consult request");
    let Request::CommSpawn { allowed_tools, .. } = request else {
        panic!("expected a spawn request");
    };
    let allowed = allowed_tools.expect("a consult always carries an allow list");
    assert!(!allowed.iter().any(|tool| tool == "write" || tool == "bash"));
    assert!(allowed.iter().any(|tool| tool == "read"));
}

/// Without a source session the request carries neither a fork nor an allow
/// list, so an ordinary spawn stays unchanged.
#[tokio::test]
async fn spawn_request_without_source_session_is_a_plain_spawn() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _home = EnvGuard::set("HORON_HOME", home.path());
    let project = tempfile::tempdir().expect("project");
    let ctx = test_ctx("plain", project.path());
    let params: CommunicateInput = serde_json::from_value(json!({
        "action": "spawn",
        "label": "plain",
        "prompt": "do the thing"
    }))
    .expect("spawn input should deserialize");

    let request = super::agent_spawn::spawn_request(&ctx, &params, "plain".to_string()).expect("plain spawn");
    let Request::CommSpawn {
        source_session,
        allowed_tools,
        ..
    } = request
    else {
        panic!("expected a spawn request");
    };
    assert_eq!(source_session, None);
    assert_eq!(allowed_tools, None);
}
