use super::*;
use anyhow::{Result, anyhow};

/// A temporary directory together with its canonical spelling.
///
/// On macOS `TempDir::path()` is `/var/folders/...`, `/var` is a symlink to
/// `private/var`, and `getcwd` (which a session records as its working
/// directory) always reports the canonical path. Bind the fixture to the
/// canonical spelling so it compares against what the product records. The
/// returned `TempDir` has to outlive the path it hands back.
pub(in crate::session) fn canonical_tempdir(prefix: &str) -> Result<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let path = dir.path().canonicalize().map_err(|e| anyhow!(e))?;
    Ok((dir, path))
}

#[test]
fn test_session_exists_roundtrip() -> Result<()> {
    let tmp_dir = std::env::temp_dir().join(format!(
        "horon-agent-session-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow!(e))?
            .as_nanos()
    ));
    // The store refuses any directory under the agent home that is not 0700 on
    // unix, and create_dir_all would leave this one at the umask default. The
    // recursive create also makes the home itself, which a plain create does not.
    crate::storage::create_store_dir(&tmp_dir.join("sessions"), true)?;

    assert!(!session_path_in_dir(&tmp_dir, "missing-session").exists());

    let session_path = session_path_in_dir(&tmp_dir, "exists-session");
    std::fs::write(&session_path, "{}")?;
    assert!(session_path.exists());

    let random_id = format!(
        "missing-session-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow!(e))?
            .as_nanos()
    );
    assert!(!session_exists(&random_id));
    Ok(())
}

#[test]
fn derive_session_provider_key_prefers_runtime_identity_over_transport() {
    let _lock = lock_env();
    let _runtime = EnvVarGuard::set("HORON_AGENT_RUNTIME_PROVIDER", "azure-openai");
    let _namespace = EnvVarGuard::set("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE", "azure-cache");
    let _active = EnvVarGuard::set("HORON_AGENT_ACTIVE_PROVIDER", "openrouter");

    assert_eq!(
        derive_session_provider_key("openrouter").as_deref(),
        Some("azure-openai")
    );
}

#[test]
fn derive_session_provider_key_falls_back_to_openrouter_namespace() {
    let _lock = lock_env();
    let _runtime = EnvVarGuard::remove("HORON_AGENT_RUNTIME_PROVIDER");
    let _namespace = EnvVarGuard::set("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE", "azure-openai");
    let _active = EnvVarGuard::set("HORON_AGENT_ACTIVE_PROVIDER", "openrouter");

    assert_eq!(
        derive_session_provider_key("openrouter").as_deref(),
        Some("azure-openai")
    );
}

#[test]
fn derive_session_provider_key_keeps_openai_compatible_profile_namespace() {
    let _lock = lock_env();
    let _runtime = EnvVarGuard::set("HORON_AGENT_RUNTIME_PROVIDER", "openai-compatible");
    let _namespace = EnvVarGuard::set("HORON_AGENT_OPENROUTER_CACHE_NAMESPACE", "zai");
    let _active = EnvVarGuard::set("HORON_AGENT_ACTIVE_PROVIDER", "openrouter");

    assert_eq!(derive_session_provider_key("openrouter").as_deref(), Some("zai"));
}

#[test]
fn save_label_becomes_the_session_title() {
    let mut session = Session::create_with_id(
        "session_save_label_123".to_string(),
        None,
        Some("Generated title".to_string()),
    );
    session.mark_saved(None);
    assert_eq!(session.display_title(), Some("Generated title"));

    session.mark_saved(Some("  yc mcp  ".to_string()));
    assert_eq!(session.save_label.as_deref(), Some("yc mcp"));
    assert_eq!(session.custom_title.as_deref(), Some("yc mcp"));
    assert_eq!(session.display_title(), Some("yc mcp"));

    // Legacy bookmarks saved a label without setting the title.
    session.custom_title = None;
    assert_eq!(session.display_title(), Some("yc mcp"));
    session.unmark_saved();
    assert_eq!(session.display_title(), Some("Generated title"));
}

#[test]
fn rename_title_preserves_generated_title_for_clear() {
    let mut session = Session::create_with_id(
        "session_rename_clear_123".to_string(),
        None,
        Some("Generated first prompt title".to_string()),
    );

    assert_eq!(session.display_title(), Some("Generated first prompt title"));
    session.rename_title(Some("Custom planning name".to_string()));
    assert_eq!(session.title.as_deref(), Some("Generated first prompt title"));
    assert_eq!(session.custom_title.as_deref(), Some("Custom planning name"));
    assert_eq!(session.display_title(), Some("Custom planning name"));

    session.rename_title(None);
    assert_eq!(session.title.as_deref(), Some("Generated first prompt title"));
    assert!(session.custom_title.is_none());
    assert_eq!(session.display_title(), Some("Generated first prompt title"));

    session.custom_title = Some("   ".to_string());
    assert_eq!(session.display_title(), Some("Generated first prompt title"));
}

#[test]
fn test_debug_memory_profile_reports_messages_and_provider_cache() {
    let mut session = Session::create_with_id(
        "session_memory_profile_test".to_string(),
        None,
        Some("Memory profile".to_string()),
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "hello world".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::ToolUse {
                id: "tool_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "echo hi"}),
                thought_signature: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: "tool_1".to_string(),
                content: "hi".to_string(),
                is_error: None,
            },
        ],
    );

    session.compaction = Some(StoredCompactionState {
        summary_text: "summary".to_string(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 7,
        original_turn_count: 9,
        compacted_count: 7,
        structured: None,
    });

    let _ = session.provider_messages();
    let profile = session.debug_memory_profile();

    assert_eq!(profile["messages"]["count"], 2);
    assert_eq!(profile["messages"]["released_prefix_count"], 0);
    assert_eq!(
        profile["messages"]["resident_tail_budget_bytes"],
        horon_agent_session_store::INLINE_TAIL_BYTES as u64
    );
    assert_eq!(profile["messages"]["memory"]["text_blocks"], 1);
    assert_eq!(profile["messages"]["memory"]["tool_use_blocks"], 1);
    assert_eq!(profile["messages"]["memory"]["tool_result_blocks"], 1);
    assert!(profile["messages"]["json_bytes"].as_u64().unwrap_or(0) > 0);
    assert_eq!(profile["provider_messages_cache"]["count"], 2);
    assert_eq!(profile["compaction"]["present"], true);
    assert_eq!(profile["compaction"]["covers_up_to_turn"], 7);
    assert_eq!(profile["compaction"]["original_turn_count"], 9);
    assert_eq!(profile["compaction"]["compacted_count"], 7);
    assert!(profile["provider_messages_cache"]["json_bytes"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn released_provider_message_cache_rebuilds_from_canonical_history() {
    let mut session = Session::create_with_id(
        "session_provider_cache_release_test".to_string(),
        None,
        Some("Provider cache release".to_string()),
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "large derived payload".repeat(1024),
            cache_control: None,
        }],
    );

    let before = session.provider_messages().to_vec();
    assert_eq!(before.len(), 1);
    assert_eq!(session.debug_memory_profile()["provider_messages_cache"]["count"], 1);

    session.release_provider_messages_cache();
    let released = session.debug_memory_profile();
    assert_eq!(released["provider_messages_cache"]["count"], 0);
    assert_eq!(released["provider_messages_cache"]["json_bytes"], 0);

    let rebuilt = session.provider_messages();
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(
        serde_json::to_value(rebuilt).unwrap(),
        serde_json::to_value(before).unwrap()
    );
}

#[test]
fn token_usage_totals_counts_cache_reported_inputs_only_when_cache_fields_exist() {
    let mut session = Session::create_with_id(
        "session_token_usage_totals_test".to_string(),
        None,
        Some("Token totals".to_string()),
    );
    session.add_message_ext(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
        None,
        Some(StoredTokenUsage {
            prompt_tokens: None,
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }),
    );
    session.add_message_ext(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
        None,
        Some(StoredTokenUsage {
            prompt_tokens: None,
            input_tokens: 200,
            output_tokens: 20,
            cache_read_input_tokens: Some(150),
            cache_creation_input_tokens: Some(25),
        }),
    );

    let totals = session.token_usage_totals();
    assert_eq!(totals.messages_with_token_usage, 2);
    assert_eq!(totals.input_tokens, 300);
    assert_eq!(totals.output_tokens, 30);
    assert_eq!(totals.cache_reported_input_tokens, 200);
    assert_eq!(totals.cache_read_input_tokens, 150);
    assert_eq!(totals.cache_creation_input_tokens, 25);
}

#[test]
fn initial_session_context_is_persisted_once_and_not_overwritten() {
    let mut session = Session::create_with_id(
        "session_context_test".to_string(),
        None,
        Some("Session context".to_string()),
    );

    assert!(session.ensure_initial_session_context_message());
    assert_eq!(session.active_context().len(), 1);
    let first = session.active_context()[0].content_preview();
    assert!(first.contains("# Session Context"));
    assert!(first.contains("OS:"));
    assert_eq!(
        session.active_context()[0].display_role,
        Some(StoredDisplayRole::System)
    );

    assert!(!session.ensure_initial_session_context_message());
    assert_eq!(session.active_context().len(), 1);

    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
    );
    assert!(!session.ensure_initial_session_context_message());
    assert_eq!(session.active_context().len(), 2);
}

#[test]
fn additional_dirs_change_injects_reminder_once() {
    let mut session = Session::create_with_id(
        "session_additional_dirs_reminder".to_string(),
        None,
        Some("additional dirs".to_string()),
    );
    session.working_dir = Some("/work".to_string());

    // The first list is captured by the session context itself.
    assert!(session.set_additional_dirs(vec!["/work/lib".to_string()]));
    assert!(session.ensure_initial_session_context_message());
    assert!(
        session.active_context()[0]
            .content_preview()
            .contains("Additional directories: /work/lib")
    );
    assert_eq!(
        session.take_pending_additional_dirs_reminder(),
        None,
        "a list the context already carries needs no reminder"
    );

    // One real message makes that context immutable.
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "start the work".to_string(),
            cache_control: None,
        }],
    );

    assert!(session.set_additional_dirs(vec!["/work/lib".to_string(), "/elsewhere".to_string()]));
    assert_eq!(
        session.take_pending_additional_dirs_reminder().as_deref(),
        Some("Additional directories: /work/lib, /elsewhere"),
        "a changed list has to reach the model through the next prompt"
    );
    assert_eq!(
        session.take_pending_additional_dirs_reminder(),
        None,
        "the reminder is one-time, not a standing notice"
    );
    assert!(
        !session.set_additional_dirs(vec!["/work/lib".to_string(), "/elsewhere".to_string()]),
        "an unchanged list is not a change"
    );
    assert_eq!(session.take_pending_additional_dirs_reminder(), None);
}

#[test]
fn session_additional_dirs_round_trip() -> Result<()> {
    let mut session = Session::create_with_id(
        "session_additional_dirs_round_trip".to_string(),
        None,
        Some("additional dirs".to_string()),
    );
    session.working_dir = Some("/work".to_string());
    assert!(session.set_additional_dirs(vec!["/work/one".to_string(), "/work/two".to_string()]));

    let encoded = serde_json::to_value(&session).expect("session metadata serializes");
    assert_eq!(
        encoded["additional_dirs"],
        serde_json::json!(["/work/one", "/work/two"]),
        "the list travels in the session metadata"
    );
    let restored: Session = serde_json::from_value(encoded).expect("session metadata round trips");
    assert_eq!(restored.additional_dirs(), vec!["/work/one", "/work/two"]);

    // A session that advertises nothing carries no key at all.
    let plain = Session::create_with_id("session_additional_dirs_empty".to_string(), None, None);
    let encoded = serde_json::to_value(&plain).expect("session metadata serializes");
    assert!(encoded.get("additional_dirs").is_none(), "{encoded}");
    assert!(plain.additional_dirs().is_empty());
    Ok(())
}

/// The list must not become a sealed metadata key.
///
/// `projection_payload::check_metadata` runs on every store read path and refuses
/// any metadata key it does not know with "must be preserved as inert legacy
/// auxiliary/extension", so a first-class `additional_dirs` key makes a build
/// that predates it fail `Session::load`. Stored in the inert extension map
/// instead, the list passes the older allow-list untouched and the older reader
/// carries it through.
#[test]
fn additional_dirs_pass_an_older_build_s_metadata_allow_list() -> Result<()> {
    // Verbatim copy of the store's metadata allow-list at the integration base
    // (10115a1:crates/session-store/src/projection_payload.rs). A payload with any
    // other top-level metadata key is refused there before the session decodes.
    const PRE_CHANGE_METADATA_KEYS: &[&str] = &[
        "parent_id",
        "title",
        "custom_title",
        "created_at",
        "updated_at",
        "system_prompt",
        "model_usage_turn_id",
        "provider_session_id",
        "cache_root_id",
        "provider_key",
        "plugin_id",
        "model",
        "route_api_method",
        "reasoning_effort",
        "subagent_model",
        "agent_model_overrides",
        "improve_mode",
        "autoreview_enabled",
        "autojudge_enabled",
        "working_dir",
        "short_name",
        "saved",
        "save_label",
        "migration_epoch",
        "legacy_extensions",
    ];

    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-additional-dirs-extension-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let id = "session_additional_dirs_older_build";
    let mut session = Session::create_with_id(id.to_string(), None, Some("additional dirs across builds".to_string()));
    session.working_dir = Some("/work".to_string());
    assert!(session.set_additional_dirs(vec!["/work/one".to_string(), "/work/two".to_string()]));

    let metadata = serde_json::to_value(session.event_metadata()?)?;
    for key in metadata.as_object().expect("metadata is an object").keys() {
        assert!(
            PRE_CHANGE_METADATA_KEYS.contains(&key.as_str()),
            "an older store refuses the metadata key {key}: {metadata}"
        );
    }
    assert_eq!(
        metadata["legacy_extensions"]["additional_dirs"],
        serde_json::json!(["/work/one", "/work/two"]),
        "the list is nested in the inert extension map, not in a sealed key"
    );

    // What an older build's load path does with that payload: drop the nested map
    // and extend the flat session object with its entries, where the key is an
    // untyped extension it preserves on the next save.
    let mut older_shape = metadata.clone();
    let object = older_shape.as_object_mut().expect("metadata is an object");
    let extensions = object
        .remove("legacy_extensions")
        .expect("the extension map is written");
    object.extend(extensions.as_object().expect("extensions are a map").clone());
    object.insert("id".into(), serde_json::json!(id));
    object.insert("messages".into(), serde_json::json!([]));
    object.insert("status".into(), serde_json::json!(SessionStatus::Closed));
    object.insert("last_pid".into(), serde_json::Value::Null);
    object.insert("last_active_at".into(), serde_json::Value::Null);
    let loaded: Session = serde_json::from_value(older_shape).expect("an older build loads the session");
    assert_eq!(loaded.additional_dirs(), vec!["/work/one", "/work/two"]);

    // And the real path, through the store that enforces that allow-list today.
    session.save()?;
    let reloaded = Session::load(id)?;
    assert_eq!(reloaded.additional_dirs(), vec!["/work/one", "/work/two"]);
    Ok(())
}

#[test]
#[allow(clippy::redundant_closure_call)]
fn initial_session_context_preserves_explicitly_bound_cwd_when_inserted() -> Result<()> {
    let _env_lock = lock_env();
    let original_cwd = std::env::current_dir().map_err(|e| anyhow!(e))?;
    let (_first_dir, first_path) = canonical_tempdir("horon-agent-session-context-first-")?;
    let (_second_dir, second_path) = canonical_tempdir("horon-agent-session-context-second-")?;

    std::env::set_current_dir(&first_path).map_err(|e| anyhow!(e))?;
    let mut session = Session::create_with_id(
        "session_context_cwd_refresh_test".to_string(),
        None,
        Some("Session context cwd refresh".to_string()),
    );
    assert_eq!(session.working_dir.as_deref(), Some(first_path.to_str().unwrap()));

    std::env::set_current_dir(&second_path).map_err(|e| anyhow!(e))?;
    let result: std::result::Result<(), anyhow::Error> = (|| {
        assert!(session.ensure_initial_session_context_message());
        let first = session.active_context()[0].content_preview();
        assert!(
            first.contains(&format!("Working directory: {}", first_path.display())),
            "session context should preserve the bound cwd, got: {first}"
        );
        assert_eq!(session.working_dir.as_deref(), Some(first_path.to_str().unwrap()));
        Ok(())
    })();
    std::env::set_current_dir(original_cwd).map_err(|e| anyhow!(e))?;
    result?;

    Ok(())
}

#[test]
#[allow(clippy::redundant_closure_call)]
fn initial_session_context_can_refresh_before_real_conversation() -> Result<()> {
    let _env_lock = lock_env();
    let original_cwd = std::env::current_dir().map_err(|e| anyhow!(e))?;
    let (_first_dir, first_path) = canonical_tempdir("horon-agent-session-context-stale-")?;
    let (_second_dir, second_path) = canonical_tempdir("horon-agent-session-context-real-")?;

    std::env::set_current_dir(&first_path).map_err(|e| anyhow!(e))?;
    let result: std::result::Result<(), anyhow::Error> = (|| {
        let mut session = Session::create_with_id(
            "session_context_remote_cwd_refresh_test".to_string(),
            None,
            Some("Remote cwd refresh".to_string()),
        );
        assert!(session.ensure_initial_session_context_message());
        assert!(
            session.active_context()[0]
                .content_preview()
                .contains(&format!("Working directory: {}", first_path.display()))
        );

        session.working_dir = Some(second_path.display().to_string());
        assert!(session.refresh_initial_session_context_message());
        let refreshed = session.active_context()[0].content_preview();
        assert!(
            refreshed.contains(&format!("Working directory: {}", second_path.display())),
            "session context should refresh to subscribed cwd, got: {refreshed}"
        );
        assert!(!refreshed.contains(&format!("Working directory: {}", first_path.display())));
        Ok(())
    })();
    std::env::set_current_dir(original_cwd).map_err(|e| anyhow!(e))?;
    result?;

    Ok(())
}

#[test]
#[allow(clippy::redundant_closure_call)]
fn initial_session_context_does_not_refresh_after_real_conversation() -> Result<()> {
    let _env_lock = lock_env();
    let original_cwd = std::env::current_dir().map_err(|e| anyhow!(e))?;
    let (_first_dir, first_path) = canonical_tempdir("horon-agent-session-context-original-")?;
    let (_second_dir, second_path) = canonical_tempdir("horon-agent-session-context-late-")?;

    std::env::set_current_dir(&first_path).map_err(|e| anyhow!(e))?;
    let result: std::result::Result<(), anyhow::Error> = (|| {
        let mut session = Session::create_with_id(
            "session_context_late_cwd_refresh_test".to_string(),
            None,
            Some("Late cwd refresh".to_string()),
        );
        assert!(session.ensure_initial_session_context_message());
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        );

        session.working_dir = Some(second_path.display().to_string());
        assert!(!session.refresh_initial_session_context_message());
        let original = session.active_context()[0].content_preview();
        assert!(original.contains(&format!("Working directory: {}", first_path.display())));
        assert!(!original.contains(&format!("Working directory: {}", second_path.display())));
        Ok(())
    })();
    std::env::set_current_dir(original_cwd).map_err(|e| anyhow!(e))?;
    result?;

    Ok(())
}

#[test]
fn existing_non_empty_session_does_not_get_retroactive_session_context() {
    let mut session = Session::create_with_id(
        "session_context_existing_test".to_string(),
        None,
        Some("Existing".to_string()),
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "already started".to_string(),
            cache_control: None,
        }],
    );

    assert!(!session.ensure_initial_session_context_message());
    assert_eq!(session.active_context().len(), 1);
    assert!(
        !session.active_context()[0]
            .content_preview()
            .contains("# Session Context")
    );
}

#[test]
fn load_startup_stub_preserves_metadata_but_skips_heavy_vectors() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-startup-stub-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let session_id = "session_startup_stub_roundtrip";
    let mut session = Session::create_with_id(
        session_id.to_string(),
        Some("parent_123".to_string()),
        Some("startup stub".to_string()),
    );
    session.model = Some("gpt-5.4".to_string());
    session.reasoning_effort = Some("high".to_string());
    session.provider_key = Some("openai".to_string());
    session.route_api_method = Some("openai-api".to_string());
    session.append_stored_message(StoredMessage {
        turn_start: false,
        id: "msg_1".to_string(),
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "hello world".to_string(),
            cache_control: None,
        }],
        timestamp: Some(Utc::now()),
        ..Default::default()
    });
    session.record_env_snapshot(EnvSnapshot {
        captured_at: Utc::now(),
        reason: "resume".to_string(),
        session_id: session_id.to_string(),
        working_dir: Some(temp_home.path().to_string_lossy().to_string()),
        provider: "openai".to_string(),
        model: "gpt-5.4".to_string(),
        horon_agent_version: "test".to_string(),
        horon_agent_git_hash: Some("abc123".to_string()),
        horon_agent_git_dirty: Some(false),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        pid: 123,
        is_debug: false,
        working_git: None,
    });
    session.record_memory_injection("summary".to_string(), "content".to_string(), 1, 5, Vec::new());
    session.record_replay_display_message("system", Some("Launch".to_string()), "boot");
    session.save()?;

    let stub = Session::load_startup_stub(session_id)?;
    assert_eq!(stub.id, session_id);
    assert_eq!(stub.parent_id.as_deref(), Some("parent_123"));
    assert_eq!(stub.title.as_deref(), Some("startup stub"));
    assert_eq!(stub.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(stub.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(stub.provider_key.as_deref(), Some("openai"));
    assert_eq!(stub.route_api_method.as_deref(), Some("openai-api"));
    assert!(stub.active_context().is_empty());
    assert!(stub.env_snapshots.is_empty());
    assert!(stub.memory_injections.is_empty());
    assert!(stub.replay_events.is_empty());
    Ok(())
}

#[test]
fn load_for_remote_startup_preserves_messages_and_replay_but_skips_heavy_vectors() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-remote-startup-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let session_id = "session_remote_startup_roundtrip";
    let mut session = Session::create_with_id(
        session_id.to_string(),
        Some("parent_remote".to_string()),
        Some("remote startup".to_string()),
    );
    session.model = Some("gpt-5.4".to_string());
    session.reasoning_effort = Some("medium".to_string());
    session.append_stored_message(StoredMessage {
        turn_start: false,
        id: "msg_remote_1".to_string(),
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "hello remote startup".to_string(),
            cache_control: None,
        }],
        timestamp: Some(Utc::now()),
        ..Default::default()
    });
    session.record_env_snapshot(EnvSnapshot {
        captured_at: Utc::now(),
        reason: "resume".to_string(),
        session_id: session_id.to_string(),
        working_dir: Some(temp_home.path().to_string_lossy().to_string()),
        provider: "openai".to_string(),
        model: "gpt-5.4".to_string(),
        horon_agent_version: "test".to_string(),
        horon_agent_git_hash: Some("abc123".to_string()),
        horon_agent_git_dirty: Some(false),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        pid: 123,
        is_debug: false,
        working_git: None,
    });
    session.record_memory_injection("summary".to_string(), "content".to_string(), 1, 5, Vec::new());
    session.record_replay_display_message("system", Some("Launch".to_string()), "boot");
    session.save()?;

    let loaded = Session::load_for_remote_startup(session_id)?;
    assert_eq!(loaded.id, session_id);
    assert_eq!(loaded.parent_id.as_deref(), Some("parent_remote"));
    assert_eq!(loaded.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(loaded.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(loaded.active_context().len(), 1);
    assert!(loaded.replay_events.is_empty());
    assert!(loaded.env_snapshots.is_empty());
    assert!(loaded.memory_injections.is_empty());
    Ok(())
}

#[test]
fn stream_rule_ledger_survives_a_reload() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-stream-rule-ledger-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let id = "session_stream_rule_ledger";
    let mut session = Session::create_with_id(id.to_string(), None, Some("rules".to_string()));
    session.stream_rule_ledger.insert("once".to_string(), 4);
    session.stream_rule_ledger.insert("gap".to_string(), 12);
    session.save()?;

    let loaded = Session::load(id)?;
    assert_eq!(loaded.stream_rule_ledger.get("once"), Some(&4));
    assert_eq!(loaded.stream_rule_ledger.get("gap"), Some(&12));
    Ok(())
}

#[test]
fn test_create_marks_debug_when_test_session_env_enabled() {
    let _env_lock = lock_env();
    let _test_flag = EnvVarGuard::set("HORON_AGENT_TEST_SESSION", "1");

    let s1 = Session::create(None, None);
    assert!(s1.is_debug);

    let s2 = Session::create_with_id("session_test_1".to_string(), None, None);
    assert!(s2.is_debug);
}

#[test]
fn test_create_not_debug_when_test_session_env_disabled() {
    let _env_lock = lock_env();
    let _test_flag = EnvVarGuard::set("HORON_AGENT_TEST_SESSION", "0");

    let s = Session::create(None, None);
    assert!(!s.is_debug);
}

#[test]
fn test_recover_crashed_sessions_preserves_debug_flag() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-recover-debug-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());
    let _test_flag = EnvVarGuard::set("HORON_AGENT_TEST_SESSION", "0");

    let mut crashed = Session::create_with_id(
        "session_recover_debug_source".to_string(),
        None,
        Some("debug source".to_string()),
    );
    crashed.is_debug = true;
    crashed.mark_crashed(Some("test crash".to_string()));
    crashed.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
    );
    crashed.save()?;

    let recovered_ids = recover_crashed_sessions()?;
    assert_eq!(recovered_ids.len(), 1);

    let recovered = Session::load(&recovered_ids[0])?;
    assert!(recovered.is_debug);
    Ok(())
}

#[test]
fn test_recover_crashed_sessions_by_ids_restores_only_selected_group() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-recover-selected-crash-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());
    let _test_flag = EnvVarGuard::set("HORON_AGENT_TEST_SESSION", "0");

    let now = Utc::now();
    for (id, active_at) in [
        ("session_selected_crash", now),
        ("session_stale_unselected_crash", now - chrono::Duration::minutes(5)),
    ] {
        let mut crashed = Session::create_with_id(id.to_string(), None, Some(id.to_string()));
        crashed.mark_crashed(Some("test crash".to_string()));
        crashed.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("message from {id}"),
                cache_control: None,
            }],
        );
        crashed.last_active_at = Some(active_at);
        crashed.save()?;
    }

    let recovered_ids = recover_crashed_sessions_by_ids(&["session_selected_crash".to_string()])?;
    assert_eq!(recovered_ids.len(), 1);

    let recovered = Session::load(&recovered_ids[0])?;
    assert_eq!(recovered.parent_id.as_deref(), Some("session_selected_crash"));
    let stale = Session::load("session_stale_unselected_crash")?;
    assert!(matches!(stale.status, SessionStatus::Crashed { .. }));
    Ok(())
}

#[test]
fn untouched_session_is_not_persisted_until_real_conversation_starts() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-lazy-save-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let id = "session_untouched_lazy_save";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    assert!(session.ensure_initial_session_context_message());
    session.save()?;
    assert!(!session_exists(id));

    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
    );
    session.save()?;
    assert!(session_exists(id));
    Ok(())
}

#[test]
fn empty_fork_is_persisted_before_first_visible_message() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());
    let mut child = Session::create(Some("session_empty_parent".into()), None);
    child.append_fork_notice("session_empty_parent", "empty parent");
    assert_eq!(child.visible_conversation_message_count(), 0);
    child.save()?;

    let restored = Session::load(&child.id)?;
    assert_eq!(restored.parent_id.as_deref(), Some("session_empty_parent"));
    assert_eq!(restored.visible_conversation_message_count(), 0);
    assert!(
        restored
            .active_context()
            .last()
            .unwrap()
            .content_preview()
            .contains("forked")
    );
    assert!(!session_path("session_empty_parent")?.exists());
    Ok(())
}

#[test]
fn session_created_with_title_is_persisted_before_first_visible_message() -> Result<()> {
    // Regression for #1144: `Session::create(_, Some(title))` was skipped by
    // the untouched-session gate, so later lookups by id found nothing.
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-titled-save-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let id = "session_titled_eager_save";
    let mut session = Session::create_with_id(id.to_string(), None, Some("review".to_string()));
    assert!(session.ensure_initial_session_context_message());
    session.save()?;
    assert!(session_exists(id));

    let stub = Session::load_startup_stub(id)?;
    assert_eq!(stub.title.as_deref(), Some("review"));
    Ok(())
}

#[test]
fn test_save_persists_full_session_content() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-save-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let mut session = Session::create_with_id(
        "session_save_persist_test".to_string(),
        None,
        Some("save fidelity test".to_string()),
    );

    session.add_message(
        Role::User,
        vec![
            ContentBlock::ToolUse {
                id: "tool_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({}),
                thought_signature: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: "tool_1".to_string(),
                content: "OPENROUTER_API_KEY=sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
                is_error: None,
            },
        ],
    );

    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool_2".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({
                "command": "echo ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123"
            }),
            thought_signature: None,
        }],
    );

    session.save()?;

    let loaded = Session::load("session_save_persist_test")?;

    let ContentBlock::ToolResult { content, .. } = &loaded.active_context()[0].content[1] else {
        return Err(anyhow!("expected tool result block"));
    };
    assert!(content.contains("sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(!content.contains("[REDACTED_SECRET]"));

    let ContentBlock::ToolUse { input, .. } = &loaded.active_context()[1].content[0] else {
        return Err(anyhow!("expected tool use block"));
    };
    let input_str = input.to_string();
    assert!(input_str.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123"));
    assert!(!input_str.contains("[REDACTED_SECRET]"));
    Ok(())
}

#[test]
fn test_save_persists_compaction_state() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-compaction-save-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let mut session = Session::create_with_id(
        "session_compaction_persist_test".to_string(),
        None,
        Some("compaction persistence test".to_string()),
    );
    session.compaction = Some(StoredCompactionState {
        summary_text: "saved summary".to_string(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 8,
        original_turn_count: 8,
        compacted_count: 8,
        structured: None,
    });

    session.save()?;

    let loaded = Session::load("session_compaction_persist_test")?;
    assert_eq!(loaded.compaction, session.compaction);
    Ok(())
}

#[test]
fn test_save_persists_provider_key() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-provider-key-save-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let mut session = Session::create_with_id(
        "session_provider_key_persist_test".to_string(),
        None,
        Some("provider key persistence test".to_string()),
    );
    session.provider_key = Some("opencode".to_string());
    session.model = Some("anthropic/claude-sonnet-4".to_string());

    session.save()?;

    let loaded = Session::load("session_provider_key_persist_test")?;
    assert_eq!(loaded.provider_key.as_deref(), Some("opencode"));
    assert_eq!(loaded.model.as_deref(), Some("anthropic/claude-sonnet-4"));
    Ok(())
}

#[test]
fn test_save_persists_plugin_id_and_absent_loads_none() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-plugin-id-save-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let mut session = Session::create_with_id(
        "session_plugin_id_persist_test".to_string(),
        None,
        Some("plugin id persistence test".to_string()),
    );
    session.plugin_id = Some("router".to_string());
    // Provider/profile classification remains separate from the plugin id.
    session.provider_key = Some("plugin".to_string());
    session.model = Some("beta".to_string());
    session.save()?;

    let loaded = Session::load("session_plugin_id_persist_test")?;
    assert_eq!(loaded.plugin_id.as_deref(), Some("router"));
    assert_eq!(loaded.provider_key.as_deref(), Some("plugin"));
    assert_eq!(loaded.model.as_deref(), Some("beta"));

    let mut plain = Session::create_with_id(
        "session_plugin_id_absent_test".to_string(),
        None,
        Some("no plugin backend".to_string()),
    );
    plain.save()?;
    let plain_loaded = Session::load("session_plugin_id_absent_test")?;
    assert_eq!(plain_loaded.plugin_id, None);
    Ok(())
}

#[test]
fn test_save_persists_reasoning_effort() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-reasoning-effort-save-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let mut session = Session::create_with_id(
        "session_reasoning_effort_persist_test".to_string(),
        None,
        Some("reasoning effort persistence test".to_string()),
    );
    session.model = Some("gpt-5.4".to_string());
    session.reasoning_effort = Some("xhigh".to_string());

    session.save()?;

    let loaded = Session::load("session_reasoning_effort_persist_test")?;
    assert_eq!(loaded.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(loaded.reasoning_effort.as_deref(), Some("xhigh"));
    Ok(())
}

#[test]
fn test_save_appends_journal_and_load_replays_it() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-journal-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let mut session = Session::create_with_id(
        "session_journal_append_test".to_string(),
        None,
        Some("journal append test".to_string()),
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    );
    session.save()?;

    let snapshot_path = session_path("session_journal_append_test")?;
    let journal_path = session_journal_path("session_journal_append_test")?;
    assert!(!snapshot_path.exists());
    let prefix = std::fs::read(session.event_log_path()?)?;
    assert!(!journal_path.exists());

    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
    );
    session.save()?;

    assert!(!journal_path.exists());
    let journal = std::fs::read_to_string(session.event_log_path()?)?;
    assert!(journal.as_bytes().starts_with(&prefix));
    assert!(journal.contains("second"));

    let loaded = Session::load("session_journal_append_test")?;
    assert_eq!(loaded.active_context().len(), 2);
    assert_eq!(loaded.active_context()[1].content_preview(), "second");
    Ok(())
}

#[test]
fn test_save_checkpoints_after_full_mutation_and_clears_journal() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-checkpoint-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let mut session = Session::create_with_id(
        "session_journal_checkpoint_test".to_string(),
        None,
        Some("checkpoint test".to_string()),
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "one".to_string(),
            cache_control: None,
        }],
    );
    session.save()?;

    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "two".to_string(),
            cache_control: None,
        }],
    );
    session.save()?;

    let journal_path = session_journal_path("session_journal_checkpoint_test")?;
    assert!(!journal_path.exists());
    let original = std::fs::read(session.event_log_path()?)?;

    session.truncate_messages(1);
    session.title = Some("checkpointed title".to_string());
    session.save()?;

    assert!(!journal_path.exists());
    assert!(std::fs::read(session.event_log_path()?)?.starts_with(&original));

    let loaded = Session::load("session_journal_checkpoint_test")?;
    assert_eq!(loaded.title.as_deref(), Some("checkpointed title"));
    assert_eq!(loaded.active_context().len(), 1);
    assert_eq!(loaded.active_context()[0].content_preview(), "one");
    Ok(())
}

#[test]
fn test_journal_replay_skips_corrupt_line_and_keeps_tail() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-journal-corrupt-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let session_id = "session_journal_corrupt_tail_test";
    let mut session = Session::create_with_id(session_id.to_string(), None, Some("corrupt journal test".to_string()));
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    );
    save_legacy_fixture(&mut session)?;

    // Two good journal entries.
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
    );
    save_legacy_fixture(&mut session)?;
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "third".to_string(),
            cache_control: None,
        }],
    );
    save_legacy_fixture(&mut session)?;

    // Corrupt the middle line (torn write) but keep the final line intact.
    let journal_path = session_journal_path(session_id)?;
    let journal = std::fs::read_to_string(&journal_path)?;
    let lines: Vec<&str> = journal.lines().collect();
    assert_eq!(lines.len(), 2);
    let torn = &lines[0][..lines[0].len() / 2];
    std::fs::write(&journal_path, format!("{}\n{}\n", torn, lines[1]))?;

    // The last prompt ("third") must survive even though an earlier journal
    // line is unparseable.
    let sources = legacy_source_bytes(session_id)?;
    let loaded = Session::load(session_id)?;
    assert_eq!(loaded.active_context().len(), 2);
    assert_eq!(loaded.active_context()[0].content_preview(), "first");
    assert_eq!(loaded.active_context()[1].content_preview(), "third");

    let remote = Session::load_for_remote_startup(session_id)?;
    assert_eq!(remote.active_context().len(), 2);
    assert_eq!(remote.active_context()[1].content_preview(), "third");
    assert_legacy_sources_unchanged(&sources)?;
    Ok(())
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }
}

/// #1632: a process killed between writing the checkpoint snapshot and
/// deleting the journal must not duplicate the journaled messages on the next
/// load, and later genuinely-new journal entries must still replay.
#[test]
fn test_interrupted_checkpoint_does_not_duplicate_journal_messages() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-journal-interrupted-checkpoint-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let session_id = "session_interrupted_checkpoint_test";
    let mut session = Session::create_with_id(session_id.to_string(), None, Some("t".into()));
    session.add_message(Role::User, vec![text_block("start")]);
    save_legacy_fixture(&mut session)?;

    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "toolu_1".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
            thought_signature: None,
        }],
    );
    save_legacy_fixture(&mut session)?;
    session.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: "ok".into(),
            is_error: None,
        }],
    );
    save_legacy_fixture(&mut session)?;

    let journal_path = session_journal_path(session_id)?;
    let stale_journal = std::fs::read_to_string(&journal_path)?;
    assert_eq!(stale_journal.lines().count(), 2);

    // Checkpoint, then put the journal back as if the delete never happened.
    session.mark_messages_full_dirty();
    save_legacy_fixture(&mut session)?;
    assert!(!journal_path.exists());
    std::fs::write(&journal_path, &stale_journal)?;

    let sources = legacy_source_bytes(session_id)?;
    let loaded = Session::load(session_id)?;
    assert_eq!(loaded.active_context().len(), 3, "journal replay duplicated messages");
    let remote = Session::load_for_remote_startup(session_id)?;
    assert_eq!(remote.active_context().len(), 3);

    // A genuinely new entry after the stale prefix still replays.
    let mut resumed = loaded;
    resumed.add_message(Role::Assistant, vec![text_block("after restart")]);
    resumed.save()?;
    let reloaded = Session::load(session_id)?;
    assert_eq!(reloaded.active_context().len(), 4);
    assert_eq!(reloaded.active_context()[3].content_preview(), "after restart");
    let mut ids: Vec<_> = reloaded.active_context().iter().map(|m| m.id.clone()).collect();
    ids.dedup();
    assert_eq!(ids.len(), 4);
    assert_legacy_sources_unchanged(&sources)?;
    Ok(())
}

/// #1632: snapshots that were already written with duplicated messages (by
/// builds before idempotent replay) are repaired on load and on the next save.
#[test]
fn test_load_repairs_snapshot_with_duplicated_message_ids() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-duplicate-ids-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let session_id = "session_duplicate_ids_test";
    let mut session = Session::create_with_id(session_id.to_string(), None, Some("t".into()));
    session.add_message(Role::User, vec![text_block("one")]);
    session.add_message(Role::Assistant, vec![text_block("two")]);
    let dup = session.active_context()[1].clone();
    session.append_stored_message(dup);
    session.mark_messages_full_dirty();
    save_legacy_fixture(&mut session)?;
    let original = std::fs::read(session_path(session_id)?)?;

    let mut loaded = Session::load(session_id)?;
    assert_eq!(loaded.active_context().len(), 2);
    loaded.save()?;
    assert_eq!(std::fs::read(session_path(session_id)?)?, original);
    let raw = serde_json::to_value(Session::load(session_id)?)?;
    assert_eq!(raw["messages"].as_array().map(Vec::len), Some(2));
    Ok(())
}

#[test]
fn test_journal_replay_salvages_glued_entries_on_torn_line() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-journal-glued-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let session_id = "session_journal_glued_test";
    let mut session = Session::create_with_id(session_id.to_string(), None, Some("glued journal test".to_string()));
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    );
    save_legacy_fixture(&mut session)?;
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
    );
    save_legacy_fixture(&mut session)?;
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "third".to_string(),
            cache_control: None,
        }],
    );
    save_legacy_fixture(&mut session)?;

    // Simulate a torn append glued to the next complete entry: half of entry 1
    // followed (no newline) by all of entry 2 on the same line.
    let journal_path = session_journal_path(session_id)?;
    let journal = std::fs::read_to_string(&journal_path)?;
    let lines: Vec<&str> = journal.lines().collect();
    assert_eq!(lines.len(), 2);
    let torn = &lines[0][..lines[0].len() / 2];
    std::fs::write(&journal_path, format!("{}{}\n", torn, lines[1]))?;

    // The glued complete entry ("third") must be salvaged from the corrupt line.
    let sources = legacy_source_bytes(session_id)?;
    let loaded = Session::load(session_id)?;
    assert_eq!(loaded.active_context().len(), 2);
    assert_eq!(loaded.active_context()[0].content_preview(), "first");
    assert_eq!(loaded.active_context()[1].content_preview(), "third");
    assert_legacy_sources_unchanged(&sources)?;
    Ok(())
}

#[test]
fn test_corrupt_journal_heals_via_checkpoint_on_next_save() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-session-journal-heal-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let session_id = "session_journal_heal_test";
    let mut session = Session::create_with_id(session_id.to_string(), None, Some("heal journal test".to_string()));
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    );
    save_legacy_fixture(&mut session)?;
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
    );
    save_legacy_fixture(&mut session)?;

    // Corrupt the only journal line.
    let journal_path = session_journal_path(session_id)?;
    let journal = std::fs::read_to_string(&journal_path)?;
    let line = journal.lines().next().unwrap_or_default();
    std::fs::write(&journal_path, &line[..line.len() / 2])?;

    let original = std::fs::read(&journal_path)?;
    let mut loaded = Session::load(session_id)?;
    assert_eq!(loaded.active_context().len(), 1);

    // A forensic backup of the corrupt journal is kept.
    let backup_path = journal_path.with_extension("corrupt.jsonl");
    assert!(!backup_path.exists());
    assert_eq!(std::fs::read(&journal_path)?, original);

    // The next save checkpoints a full snapshot and removes the corrupt journal,
    // so the bad line is never replayed again.
    loaded.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "after heal".to_string(),
            cache_control: None,
        }],
    );
    loaded.save()?;
    assert_eq!(std::fs::read(&journal_path)?, original);

    let reloaded = Session::load(session_id)?;
    assert_eq!(reloaded.active_context().len(), 2);
    assert_eq!(reloaded.active_context()[1].content_preview(), "after heal");
    Ok(())
}

#[test]
fn test_redacted_for_export_redacts_tool_result_and_tool_input() -> Result<()> {
    let mut session = Session::create_with_id(
        "session_redact_persist_test".to_string(),
        None,
        Some("redaction test".to_string()),
    );

    session.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: "OPENROUTER_API_KEY=sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
            is_error: None,
        }],
    );

    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool_2".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({
                "command": "echo ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123",
                "api_key": "short-secret-value",
                "source": "fn add(a: i32, b: i32) -> i32 { a + b }"
            }),
            thought_signature: None,
        }],
    );

    let persisted = session.redacted_for_export()?;

    let first_content = &persisted.active_context()[0].content[0];
    let ContentBlock::ToolResult { content, .. } = first_content else {
        return Err(anyhow!("expected tool result block"));
    };
    assert!(content.contains("OPENROUTER_API_KEY=[REDACTED_SECRET]"));
    assert!(!content.contains("sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789"));

    let second_content = &persisted.active_context()[1].content[0];
    let ContentBlock::ToolUse { input, .. } = second_content else {
        return Err(anyhow!("expected tool use block"));
    };
    let input_str = input.to_string();
    assert!(input_str.contains("[REDACTED_SECRET]"));
    assert!(!input_str.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123"));
    assert!(!input_str.contains("short-secret-value"));
    assert!(input_str.contains("fn add(a: i32, b: i32)"));
    Ok(())
}

#[test]
fn test_redacted_for_export_redacts_provider_native_items_only_in_copy() -> Result<()> {
    let mut session = Session::create_with_id(
        "session_redact_native_test".to_string(),
        None,
        Some("redaction test".to_string()),
    );
    let item = serde_json::json!({
        "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
        "input": {"query": "why does ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123 fail"}
    });
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ProviderNative {
            provider: "anthropic".to_string(),
            item: item.clone(),
        }],
    );

    let persisted = session.redacted_for_export()?;
    let ContentBlock::ProviderNative { item: exported, .. } = &persisted.active_context()[0].content[0] else {
        return Err(anyhow!("expected provider-native block"));
    };
    assert!(!exported.to_string().contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123"));
    // The live session keeps the item verbatim for replay.
    let ContentBlock::ProviderNative { item: stored, .. } = &session.active_context()[0].content[0] else {
        return Err(anyhow!("expected provider-native block"));
    };
    assert_eq!(stored, &item);
    Ok(())
}

#[test]
fn test_redacted_for_export_redacts_replay_events() -> Result<()> {
    let mut session = Session::create_with_id(
        "session_redacted_replay_events_test".to_string(),
        None,
        Some("redacted replay events".to_string()),
    );

    session.record_replay_display_message(
        "swarm",
        Some("DM from fox".to_string()),
        "OPENROUTER_API_KEY=sk-or-v1-secret-value",
    );
    session.record_swarm_status_event(vec![crate::protocol::SwarmMemberStatus {
        session_id: "session_fox".to_string(),
        friendly_name: Some("fox".to_string()),
        status: "running".to_string(),
        detail: Some("ANTHROPIC_API_KEY=sk-ant-secret-value".to_string()),
        role: Some("agent".to_string()),
        is_headless: None,
        live_attachments: None,
        status_age_secs: None,
        output_tail: None,
        report_back_to_session_id: None,
        todo_progress: None,
        todo_items: Vec::new(),
        task_label: None,
        runtime: crate::protocol::SwarmMemberRuntime::default(),
    }]);
    session.record_swarm_plan_event(
        "swarm_test".to_string(),
        1,
        vec![crate::plan::PlanItem {
            content: "OPENROUTER_API_KEY=sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
            status: "pending".to_string(),
            priority: "high".to_string(),
            id: "task-1".to_string(),
            subsystem: None,
            file_scope: Vec::new(),
            blocked_by: vec![],
            assigned_to: None,
        }],
        vec![],
        Some("ANTHROPIC_API_KEY=sk-ant-secret-value".to_string()),
    );

    let redacted = session.redacted_for_export()?;
    assert_eq!(redacted.replay_events.len(), 3);

    let StoredReplayEventKind::DisplayMessage { content, .. } = &redacted.replay_events[0].kind else {
        return Err(anyhow!("expected display message replay event"));
    };
    assert!(content.contains("OPENROUTER_API_KEY=[REDACTED_SECRET]"));
    assert!(!content.contains("sk-or-v1-secret-value"));

    let StoredReplayEventKind::SwarmStatus { members } = &redacted.replay_events[1].kind else {
        return Err(anyhow!("expected swarm status replay event"));
    };
    let detail = members[0].detail.as_deref().unwrap_or_default();
    assert!(detail.contains("ANTHROPIC_API_KEY=[REDACTED_SECRET]"));
    assert!(!detail.contains("sk-ant-secret-value"));

    let StoredReplayEventKind::SwarmPlan { items, reason, .. } = &redacted.replay_events[2].kind else {
        return Err(anyhow!("expected swarm plan replay event"));
    };
    assert!(items[0].content.contains("OPENROUTER_API_KEY=[REDACTED_SECRET]"));
    assert!(
        !items[0]
            .content
            .contains("sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789")
    );
    let reason = reason.as_deref().unwrap_or_default();
    assert!(reason.contains("ANTHROPIC_API_KEY=[REDACTED_SECRET]"));
    assert!(!reason.contains("sk-ant-secret-value"));
    Ok(())
}

#[test]
fn test_summarize_tool_calls_includes_tool_only_assistant_messages() {
    let mut session = Session::create_with_id(
        "session_tool_summary_test".to_string(),
        None,
        Some("tool summary test".to_string()),
    );

    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool_1".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({
                "command": "pwd"
            }),
            thought_signature: None,
        }],
    );

    let summaries = summarize_tool_calls(&session, 10);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].tool_name, "bash");
    assert!(summaries[0].brief_output.contains("pwd"));
}

#[test]
fn test_render_messages_honors_system_display_role_override() {
    let mut session = Session::create_with_id(
        "session_display_role_test".to_string(),
        None,
        Some("display role test".to_string()),
    );

    session.add_message_with_display_role(
        Role::User,
        vec![ContentBlock::Text {
            text: "[Background Task Completed]\nTask: abc123 (bash)".to_string(),
            cache_control: None,
        }],
        Some(StoredDisplayRole::System),
    );

    let rendered = render_messages(&session);
    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].role, "system");
    assert!(rendered[0].content.contains("Background Task Completed"));
}

#[test]
fn legacy_scheduled_task_message_renders_as_system() {
    let mut session = Session::create(None, None);
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "[Scheduled task]\nA scheduled task for this session is now due.\n\nTask: check progress".to_string(),
            cache_control: None,
        }],
    );

    let rendered = render::render_messages(&session);
    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].role, "system");
    assert_eq!(session.visible_conversation_message_count(), 0);
}

#[test]
fn test_render_messages_shows_auto_poke_continuations_as_system_not_user() {
    // Regression: incomplete-todo and private-quality continuations are persisted as
    // Role::User so the model continues the turn, but the live UI hides them.
    // On reload/resume/remote attach the renderer must not resurrect them as
    // the user's last prompt.
    let mut session = Session::create_with_id(
        "session_render_auto_poke_test".to_string(),
        None,
        Some("auto poke render test".to_string()),
    );

    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "please fix the login bug".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "Working on it.".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: crate::todo::build_auto_poke_message(2),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: crate::todo::TODO_COMPLETION_CONTINUATION_MESSAGE.to_string(),
            cache_control: None,
        }],
    );

    let rendered = render_messages(&session);
    let user_messages: Vec<_> = rendered.iter().filter(|message| message.role == "user").collect();
    assert_eq!(
        user_messages.len(),
        1,
        "only the real prompt should render as a user message: {rendered:?}"
    );
    assert_eq!(user_messages[0].content, "please fix the login bug");

    let system_contents: Vec<_> = rendered
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| message.content.as_str())
        .collect();
    assert!(
        system_contents
            .iter()
            .any(|content| content.contains("incomplete todo")),
        "auto-poke continuation should render as system: {rendered:?}"
    );
    assert!(
        system_contents
            .iter()
            .any(|content| content.contains("Double-checking confidence")),
        "quality continuation should render as a short system notice: {rendered:?}"
    );
    // The model-facing instruction text stays out of the transcript.
    assert!(
        !system_contents
            .iter()
            .any(|content| content.contains("Validate the completed result")),
        "quality continuation leaked model-facing instructions: {rendered:?}"
    );
}

#[test]
fn test_render_messages_renders_reasoning_before_answer_in_stored_order() {
    // Regression: providers persist the assistant turn as `[Text, ReasoningTrace,
    // ToolUse]` (see agent/turn_loops.rs push order). On resume/re-render the
    // reasoning must still appear *before* the answer text to match the live
    // streaming order, even though the Text block is stored first.
    use crate::session::render::render_markup::REASONING_SENTINEL;

    let _env_lock = lock_env();
    let _mode = EnvVarGuard::set("HORON_AGENT_REASONING_DISPLAY", "full");
    crate::config::invalidate_config_cache();

    let mut session = Session::create_with_id(
        "session_render_reasoning_order_test".to_string(),
        None,
        Some("render reasoning order test".to_string()),
    );

    session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::Text {
                text: "Here is the answer.".to_string(),
                cache_control: None,
            },
            ContentBlock::ReasoningTrace {
                text: "step one\nstep two".to_string(),
            },
        ],
    );

    let rendered = render_messages(&session);
    assert_eq!(rendered.len(), 1);
    let content = &rendered[0].content;
    assert!(
        content.contains(&format!("*{0}step one{0}*", REASONING_SENTINEL)),
        "expected reasoning markup, got: {content:?}"
    );
    assert!(content.contains("Here is the answer."));
    let reasoning_pos = content.find("step two").unwrap();
    let answer_pos = content.find("Here is the answer.").unwrap();
    assert!(
        reasoning_pos < answer_pos,
        "reasoning should precede the answer text even when stored after it: {content:?}"
    );
}

#[test]
fn test_render_messages_renders_persisted_reasoning() {
    use crate::session::render::render_markup::REASONING_SENTINEL;

    let _env_lock = lock_env();
    let _mode = EnvVarGuard::set("HORON_AGENT_REASONING_DISPLAY", "full");
    crate::config::invalidate_config_cache();

    let mut session = Session::create_with_id(
        "session_render_reasoning_test".to_string(),
        None,
        Some("render reasoning test".to_string()),
    );

    session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::ReasoningTrace {
                text: "step one\nstep two".to_string(),
            },
            ContentBlock::Text {
                text: "Here is the answer.".to_string(),
                cache_control: None,
            },
        ],
    );

    let rendered = render_messages(&session);
    assert_eq!(rendered.len(), 1);
    let content = &rendered[0].content;
    // Reasoning lines are rendered as dim/italic markup with the sentinel.
    assert!(
        content.contains(&format!("*{0}step one{0}*", REASONING_SENTINEL)),
        "expected reasoning markup, got: {content:?}"
    );
    assert!(
        content.contains(&format!("*{0}step two{0}*", REASONING_SENTINEL)),
        "expected reasoning markup, got: {content:?}"
    );
    // Answer text follows the reasoning block.
    assert!(content.contains("Here is the answer."));
    let reasoning_end = content.find("step two").unwrap();
    let answer_start = content.find("Here is the answer.").unwrap();
    assert!(
        reasoning_end < answer_start,
        "reasoning should precede the answer text: {content:?}"
    );
}

#[test]
fn test_render_messages_renders_legacy_reasoning_variant() {
    use crate::session::render::render_markup::REASONING_SENTINEL;

    let _env_lock = lock_env();
    let _mode = EnvVarGuard::set("HORON_AGENT_REASONING_DISPLAY", "full");
    crate::config::invalidate_config_cache();

    let mut session = Session::create_with_id(
        "session_render_legacy_reasoning_test".to_string(),
        None,
        Some("render legacy reasoning test".to_string()),
    );

    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Reasoning {
            text: "legacy thought".to_string(),
        }],
    );

    let rendered = render_messages(&session);
    assert_eq!(rendered.len(), 1);
    assert!(
        rendered[0]
            .content
            .contains(&format!("*{0}legacy thought{0}*", REASONING_SENTINEL)),
        "expected legacy reasoning markup, got: {:?}",
        rendered[0].content
    );
}

#[test]
fn test_render_messages_hides_persisted_reasoning_in_current_mode() {
    use crate::session::render::render_markup::REASONING_SENTINEL;

    let _env_lock = lock_env();
    let _mode = EnvVarGuard::set("HORON_AGENT_REASONING_DISPLAY", "current");
    crate::config::invalidate_config_cache();

    let mut session = Session::create_with_id(
        "session_render_reasoning_current_test".to_string(),
        None,
        Some("render reasoning current test".to_string()),
    );

    session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::ReasoningTrace {
                text: "step one\nstep two\nstep three".to_string(),
            },
            ContentBlock::Text {
                text: "Here is the answer.".to_string(),
                cache_control: None,
            },
        ],
    );

    let rendered = render_messages(&session);
    assert_eq!(rendered.len(), 1);
    let content = &rendered[0].content;
    // In `current` mode only the *live* reasoning block is ever shown; it streams
    // then is discarded once the model answers. Re-rendered history therefore
    // shows no past reasoning at all (no trace line, no lines, no sentinel).
    assert!(
        !content.contains(REASONING_SENTINEL),
        "no reasoning markup expected in current mode on reload: {content:?}"
    );
    assert!(
        !content.contains("step one") && !content.contains("step two") && !content.contains("thought"),
        "individual reasoning lines/trace must not be replayed in current mode: {content:?}"
    );
    // The answer text is preserved.
    assert!(content.contains("Here is the answer."));
}

#[test]
fn test_render_messages_hides_persisted_reasoning_in_off_mode() {
    use crate::session::render::render_markup::REASONING_SENTINEL;

    let _env_lock = lock_env();
    let _mode = EnvVarGuard::set("HORON_AGENT_REASONING_DISPLAY", "off");
    crate::config::invalidate_config_cache();

    let mut session = Session::create_with_id(
        "session_render_reasoning_off_test".to_string(),
        None,
        Some("render reasoning off test".to_string()),
    );

    session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::ReasoningTrace {
                text: "secret thought".to_string(),
            },
            ContentBlock::Text {
                text: "Here is the answer.".to_string(),
                cache_control: None,
            },
        ],
    );

    let rendered = render_messages(&session);
    assert_eq!(rendered.len(), 1);
    let content = &rendered[0].content;
    assert!(
        !content.contains(REASONING_SENTINEL) && !content.contains("secret thought"),
        "reasoning must be hidden entirely in off mode: {content:?}"
    );
    assert!(content.contains("Here is the answer."));
}

#[test]
fn test_render_messages_honors_background_task_display_role_override() {
    let mut session = Session::create_with_id(
        "session_background_task_role_test".to_string(),
        None,
        Some("background task role test".to_string()),
    );

    session.add_message_with_display_role(
            Role::User,
            vec![ContentBlock::Text {
                text: "**Background task** `abc123` · `bash` · ✓ completed · 7.1s · exit 0\n\n_No output captured._\n\n_Full output:_ `bg action=\"output\" task_id=\"abc123\"`".to_string(),
                cache_control: None,
            }],
            Some(StoredDisplayRole::BackgroundTask),
        );

    let rendered = render_messages(&session);
    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].role, "background_task");
    assert!(rendered[0].content.contains("**Background task**"));
}

#[test]
fn test_render_messages_hides_internal_system_reminders() {
    let mut session = Session::create_with_id(
        "session_hidden_system_reminder_test".to_string(),
        None,
        Some("hidden reminder test".to_string()),
    );

    assert!(session.ensure_initial_session_context_message());
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "visible prompt".to_string(),
            cache_control: None,
        }],
    );

    let rendered = render_messages(&session);
    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].role, "user");
    assert_eq!(rendered[0].content, "visible prompt");
}

#[test]
fn test_render_messages_shows_recent_compacted_history_by_default() {
    let mut session = Session::create_with_id(
        "session_render_compacted_history_test".to_string(),
        None,
        Some("render compacted history test".to_string()),
    );

    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "old prompt".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "old response".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "current prompt".to_string(),
            cache_control: None,
        }],
    );
    session.compaction = Some(StoredCompactionState {
        summary_text: "old prompt and response".to_string(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 2,
        original_turn_count: 2,
        compacted_count: 2,
        structured: None,
    });

    let rendered = render_messages(&session);
    assert_eq!(rendered.len(), 4);
    assert_eq!(rendered[0].role, "system");
    assert!(rendered[0].content.contains("showing all 2"));
    assert_eq!(rendered[1].role, "user");
    assert_eq!(rendered[1].content, "old prompt");
    assert_eq!(rendered[2].role, "assistant");
    assert_eq!(rendered[2].content, "old response");
    assert_eq!(rendered[3].role, "user");
    assert_eq!(rendered[3].content, "current prompt");
}

#[test]
fn test_render_messages_can_expand_compacted_history_window() {
    let mut session = Session::create_with_id(
        "session_render_compacted_history_expand_test".to_string(),
        None,
        Some("render compacted history expand test".to_string()),
    );

    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "old prompt".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "old response".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "current prompt".to_string(),
            cache_control: None,
        }],
    );
    session.compaction = Some(StoredCompactionState {
        summary_text: "old prompt and response".to_string(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 2,
        original_turn_count: 2,
        compacted_count: 2,
        structured: None,
    });

    // A small compacted prefix (few renderable messages, a single turn) must
    // never be truncated, even when a tiny visible window is requested. A single
    // long turn in particular must always render in full.
    let (rendered, _images, info) = render_messages_and_images_with_compacted_history(&session, 1);
    let info = info.expect("compacted info");
    assert_eq!(info.total_messages, 2);
    assert_eq!(info.visible_messages, 2);
    assert_eq!(info.remaining_messages, 0);
    assert_eq!(info.hidden_user_prompts, 0);
    assert_eq!(rendered.len(), 4);
    assert!(rendered[0].content.contains("showing all 2"));
    assert_eq!(rendered[1].content, "old prompt");
    assert_eq!(rendered[2].content, "old response");
    assert_eq!(rendered[3].content, "current prompt");

    let (rendered_all, _images, info_all) = render_messages_and_images_with_compacted_history(&session, usize::MAX);
    let info_all = info_all.expect("compacted info");
    assert_eq!(info_all.visible_messages, 2);
    assert_eq!(info_all.remaining_messages, 0);
    assert_eq!(info_all.hidden_user_prompts, 0);
    assert_eq!(rendered_all.len(), 4);
    assert!(rendered_all[0].content.contains("showing all 2"));
    assert_eq!(rendered_all[1].content, "old prompt");
    assert_eq!(rendered_all[2].content, "old response");
    assert_eq!(rendered_all[3].content, "current prompt");
}

#[test]
fn test_compacted_history_truncates_only_when_long_and_many_turns() {
    let mut session = Session::create_with_id(
        "session_render_compacted_history_truncate_test".to_string(),
        None,
        Some("render compacted history truncate test".to_string()),
    );

    // Build a large compacted prefix: many turns, each with several visible
    // messages, well past both guardrails (>80 renderable, >5 turns).
    let prefix_turns = 20usize;
    for t in 0..prefix_turns {
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("prompt {t}"),
                cache_control: None,
            }],
        );
        // 4 assistant messages per turn -> 5 renderable per turn.
        for r in 0..4 {
            session.add_message(
                Role::Assistant,
                vec![ContentBlock::Text {
                    text: format!("response {t}.{r}"),
                    cache_control: None,
                }],
            );
        }
    }
    // Current (uncompacted) prompt after the compacted prefix.
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "current prompt".to_string(),
            cache_control: None,
        }],
    );

    let compacted_count = prefix_turns * 5; // every prefix message is compacted
    session.compaction = Some(StoredCompactionState {
        summary_text: "older compacted context".to_string(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: prefix_turns,
        original_turn_count: prefix_turns,
        compacted_count,
        structured: None,
    });

    let total_renderable = prefix_turns * 5; // 100

    // Request a small window: truncation kicks in because the prefix is long
    // and has many turns.
    let (rendered, _images, info) = render_messages_and_images_with_compacted_history(&session, 10);
    let info = info.expect("compacted info");
    assert_eq!(info.total_messages, total_renderable);
    assert!(info.visible_messages < total_renderable);
    assert!(info.remaining_messages > 0);
    assert_eq!(info.visible_messages + info.remaining_messages, total_renderable);
    assert!(info.hidden_user_prompts > 0);
    // The first rendered body message (after the marker) must be a user prompt
    // because we snap the window to a turn boundary.
    assert_eq!(rendered[1].role, "user");

    // Requesting everything shows the whole prefix with no hidden prompts.
    let (_rendered_all, _images, info_all) = render_messages_and_images_with_compacted_history(&session, usize::MAX);
    let info_all = info_all.expect("compacted info");
    assert_eq!(info_all.visible_messages, total_renderable);
    assert_eq!(info_all.remaining_messages, 0);
    assert_eq!(info_all.hidden_user_prompts, 0);
}

#[test]
fn test_compacted_history_never_truncates_single_long_turn() {
    let mut session = Session::create_with_id(
        "session_render_compacted_history_single_turn_test".to_string(),
        None,
        Some("render compacted history single turn test".to_string()),
    );

    // A single turn with a huge number of visible messages (well over 80).
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "the one long prompt".to_string(),
            cache_control: None,
        }],
    );
    for r in 0..150 {
        session.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: format!("long response chunk {r}"),
                cache_control: None,
            }],
        );
    }
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "current prompt".to_string(),
            cache_control: None,
        }],
    );

    let compacted_count = 151; // prompt + 150 responses
    session.compaction = Some(StoredCompactionState {
        summary_text: "older compacted context".to_string(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count,
        structured: None,
    });

    // Even with a tiny requested window, a single long turn is never truncated.
    let (_rendered, _images, info) = render_messages_and_images_with_compacted_history(&session, 5);
    let info = info.expect("compacted info");
    assert_eq!(info.total_messages, compacted_count);
    assert_eq!(info.visible_messages, compacted_count);
    assert_eq!(info.remaining_messages, 0);
    assert_eq!(info.hidden_user_prompts, 0);
}

#[test]
fn test_compacted_history_window_counts_renderable_messages_not_hidden_reminders() {
    let mut session = Session::create_with_id(
        "session_render_compacted_history_hidden_budget_test".to_string(),
        None,
        Some("render compacted history hidden budget test".to_string()),
    );

    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "older visible prompt".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "<system-reminder>hidden reminder one</system-reminder>".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "previous visible assistant response".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "<system-reminder>hidden reminder two</system-reminder>".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "current prompt".to_string(),
            cache_control: None,
        }],
    );
    session.compaction = Some(StoredCompactionState {
        summary_text: "older compacted context".to_string(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 4,
        original_turn_count: 4,
        compacted_count: 4,
        structured: None,
    });

    let (rendered, _images, info) = render_messages_and_images_with_compacted_history(&session, 1);
    let info = info.expect("compacted info");

    // Hidden system reminders are never counted as renderable messages, so the
    // small prefix (2 renderable, 1 turn) is shown in full rather than truncated.
    assert_eq!(info.total_messages, 2);
    assert_eq!(info.visible_messages, 2);
    assert_eq!(info.remaining_messages, 0);
    assert_eq!(info.hidden_user_prompts, 0);
    assert_eq!(rendered.len(), 4);
    assert!(rendered[0].content.contains("showing all 2"));
    assert_eq!(rendered[1].role, "user");
    assert_eq!(rendered[1].content, "older visible prompt");
    assert_eq!(rendered[2].role, "assistant");
    assert_eq!(rendered[2].content, "previous visible assistant response");
    assert_eq!(rendered[3].content, "current prompt");
    assert!(rendered.iter().all(|msg| !msg.content.contains("hidden reminder")));
}

#[test]
fn test_render_messages_and_images_share_tool_resolution_and_labels() {
    let mut session = Session::create_with_id(
        "session_render_bundle_test".to_string(),
        None,
        Some("render bundle test".to_string()),
    );

    session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::ToolUse {
                id: "tool_img_1".to_string(),
                name: "view_image".to_string(),
                input: serde_json::json!({"file_path": "/tmp/screenshot.png"}),
                thought_signature: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: "tool_img_1".to_string(),
                content: "rendered image".to_string(),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "abcd".to_string(),
            },
            ContentBlock::Text {
                text: "[Attached image associated with the preceding tool result: screenshot.png]".to_string(),
                cache_control: None,
            },
        ],
    );

    let (rendered, images) = render_messages_and_images(&session);
    // The `[Attached image associated with the preceding tool result: ...]`
    // text block is synthetic image metadata, not a visible message. It must be
    // folded into the image label and never rendered as a (user) message,
    // otherwise it leaks out as a bogus "last prompt".
    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].role, "tool");
    assert_eq!(rendered[0].content, "rendered image");
    assert!(
        !rendered.iter().any(|m| m.content.contains("Attached image associated")),
        "attached-image label must not render as its own message"
    );
    assert_eq!(
        rendered[0].tool_data.as_ref().map(|tool| tool.name.as_str()),
        Some("view_image")
    );

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].label.as_deref(), Some("screenshot.png"));
    assert_eq!(images[0].media_type, "image/png");
    assert_eq!(
        images[0].source,
        RenderedImageSource::ToolResult {
            tool_name: "view_image".to_string(),
        }
    );
}

#[test]
fn reasoning_trace_survives_session_save_and_load() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("horon-agent-reasoning-persist-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("HORON_HOME", temp_home.path().as_os_str());

    let session_id = "session_reasoning_trace_roundtrip";
    let mut session = Session::create_with_id(session_id.to_string(), None, None);
    session.append_stored_message(StoredMessage {
        turn_start: false,
        id: "msg_assistant".to_string(),
        role: Role::Assistant,
        content: vec![
            ContentBlock::ReasoningTrace {
                text: "step 1: consider the run loop ordering".to_string(),
            },
            ContentBlock::Text {
                text: "Here is my answer.".to_string(),
                cache_control: None,
            },
        ],
        timestamp: Some(Utc::now()),
        ..Default::default()
    });
    session.save()?;

    // The reasoning must be persisted to the on-disk transcript, not just held
    // in memory, so it can be recalled/debugged after a restart.
    let raw = std::fs::read_to_string(session.event_log_path()?)?;
    assert!(
        raw.contains("reasoning_trace"),
        "transcript should serialize reasoning_trace block"
    );
    assert!(raw.contains("step 1: consider the run loop ordering"));

    let loaded = Session::load(session_id)?;
    let assistant = loaded
        .active_context()
        .iter()
        .find(|m| m.role == Role::Assistant)
        .ok_or_else(|| anyhow!("assistant message missing after reload"))?;
    let has_trace = assistant.content.iter().any(|b| {
        matches!(
            b,
            ContentBlock::ReasoningTrace { text }
                if text == "step 1: consider the run loop ordering"
        )
    });
    assert!(has_trace, "ReasoningTrace must survive save/load roundtrip");
    Ok(())
}

#[test]
fn test_render_images_anchors_tool_and_user_images() {
    let mut session = Session::create_with_id(
        "session_render_image_anchor_test".to_string(),
        None,
        Some("image anchor test".to_string()),
    );

    // Prompt 0 with a pasted image.
    session.add_message(
        Role::User,
        vec![
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "user-image-data".to_string(),
            },
            ContentBlock::Text {
                text: "look at this".to_string(),
                cache_control: None,
            },
        ],
    );
    // Assistant calls a tool.
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool-call-1".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "shot.png"}),
            thought_signature: None,
        }],
    );
    // Tool result with an attached image.
    session.add_message(
        Role::User,
        vec![
            ContentBlock::ToolResult {
                tool_use_id: "tool-call-1".to_string(),
                content: "read image".to_string(),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "tool-image-data".to_string(),
            },
        ],
    );

    let (_, images) = render_messages_and_images(&session);
    assert_eq!(images.len(), 2);
    assert_eq!(
        images[0].anchor,
        Some(RenderedImageAnchor::UserPrompt { ordinal: 0 }),
        "pasted user image should anchor to its prompt"
    );
    assert_eq!(
        images[1].anchor,
        Some(RenderedImageAnchor::ToolCall {
            id: "tool-call-1".to_string()
        }),
        "tool image should anchor to its tool call"
    );
}

#[test]
fn test_render_images_attached_label_message_does_not_shift_prompt_ordinals() {
    let mut session = Session::create_with_id(
        "session_render_image_label_ordinal_test".to_string(),
        None,
        Some("image label ordinal test".to_string()),
    );

    // Tool flow that produces a labeled image: the synthetic label text message
    // must not count as a user prompt for anchoring.
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool-call-2".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "shot.png"}),
            thought_signature: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![
            ContentBlock::ToolResult {
                tool_use_id: "tool-call-2".to_string(),
                content: "read image".to_string(),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "tool-image-data".to_string(),
            },
            ContentBlock::Text {
                text: "[Attached image associated with the preceding tool result: shot.png]".to_string(),
                cache_control: None,
            },
        ],
    );
    // A real follow-up prompt with an image: must be ordinal 0 (first prompt).
    session.add_message(
        Role::User,
        vec![
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "second-user-image".to_string(),
            },
            ContentBlock::Text {
                text: "and this one".to_string(),
                cache_control: None,
            },
        ],
    );

    let (_, images) = render_messages_and_images(&session);
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].label.as_deref(), Some("shot.png"));
    assert_eq!(
        images[1].anchor,
        Some(RenderedImageAnchor::UserPrompt { ordinal: 0 }),
        "label-only messages must not consume prompt ordinals"
    );
}

#[test]
fn fork_notice_is_model_visible_but_hidden_from_transcript() {
    let mut session = Session::create(None, None);
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "original request".to_string(),
            cache_control: None,
        }],
    );

    session.append_fork_notice("session_parent_abc", "otter");

    let notice = session.active_context().last().expect("fork notice appended");
    assert_eq!(notice.role, Role::User);
    assert_eq!(notice.display_role, Some(StoredDisplayRole::System));
    let text = notice.content_preview();
    assert!(text.contains("<system-reminder>"));
    assert!(text.contains("forked"));
    assert!(text.contains("session_parent_abc"));
    assert!(text.contains("otter"));

    // Model-visible: included in the provider message list.
    let provider_messages = session.messages_for_provider_uncached();
    assert!(
        provider_messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Text { text, .. } if text.contains("forked")
                )
            })
        }),
        "fork notice must reach the model"
    );

    // Transcript-hidden: not rendered as a visible user message.
    let (rendered, _) = render_messages_and_images(&session);
    assert!(
        !rendered
            .iter()
            .any(|message| message.role == "user" && message.content.contains("forked")),
        "fork notice must not render as a visible user message"
    );
}

/// The guard's effect has to be visible to the OS, not only to Rust: a real
/// `pmset -g assertions` read must show the sleep assertion the guard created.
///
/// `pmset` has no pid option and prints the whole machine's table, but each row
/// is prefixed with its owning process (`pid <pid>(<name>): ...`), so every read
/// below is scoped to this test process. The reason string itself is hardcoded
/// in `StreamingGuard::new` and cannot be made unique to this test, and the pid
/// scope alone cannot tell this test's own guards apart: two live assertions
/// from one process share pid and reason. Each row also carries that
/// assertion's bracketed id (`[0x...]`), which differs per assertion, so each
/// stage asserts on the ids that appeared or disappeared relative to the read
/// taken before it. Without that, a row left behind by stage 1 would satisfy
/// stage 3's registration check even if stage 3's guard never registered.
///
/// The release is asynchronous: `PowerAssertion::drop` calls
/// `IOPMAssertionRelease`, and powerd drops the row from its table some time
/// after that call returns. A single read taken immediately after the drop
/// asserts on a race; waiting for the row to disappear, bounded by a deadline,
/// asserts on the guarantee.
#[cfg(target_os = "macos")]
#[test]
fn streaming_guard_creates_visible_macos_sleep_assertion() {
    let _lock = lock_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HORON_HOME", temp.path());

    let reason = "HoronAgent streaming model response";
    // Other tests in this binary share the process; whatever matching rows
    // already exist belong to none of the guards created below.
    let baseline = own_streaming_row_ids(&pmset_assertions(), reason);

    // 1. The guard holds a live assertion, and the OS lists that assertion
    //    under this process: exactly one row appears, and its id is remembered
    //    so stage 3 can prove it created a different assertion.
    let mut streaming = StreamingGuard::new("session_power");
    assert!(
        streaming.assertion_is_active(),
        "StreamingGuard must hold a macOS sleep-prevention assertion while streaming"
    );
    let stdout = pmset_assertions();
    let after_stage_one = own_streaming_row_ids(&stdout, reason);
    let stage_one_ids = ids_added_since(&baseline, &after_stage_one);
    assert_eq!(
        stage_one_ids.len(),
        1,
        "creating the guard must add exactly one row for this process; output was:\n{stdout}"
    );

    // 2. In-process truth: the release path `Drop` runs clears the assertion.
    //    A dropped value cannot be queried, so the release is driven through the
    //    same code while the value is still live. The later `drop` then exercises
    //    the real `Drop` path on an already-released assertion.
    streaming.release_sleep_assertion();
    assert!(
        !streaming.assertion_is_active(),
        "releasing the guard's assertion must clear the in-process state"
    );
    drop(streaming);

    //    OS truth for that explicit release: the id created in stage 1 has to
    //    leave the table. Waiting for that specific id rather than for any row
    //    also gives stage 3 a clean baseline.
    wait_for_own_streaming_ids(
        reason,
        "the explicitly released assertion must leave pmset's table within 5s",
        |ids| !ids.contains(&stage_one_ids[0]),
    );

    // 3. OS truth: a guard dropped without an explicit release removes its row
    //    from the machine table, bounded by a deadline. The row leaves powerd's
    //    table asynchronously after `IOPMAssertionRelease` returns, so a single
    //    immediate read would assert on a race rather than on the guarantee.
    let streaming = StreamingGuard::new("session_power");
    let stdout = pmset_assertions();
    let after_stage_three = own_streaming_row_ids(&stdout, reason);
    let stage_three_ids = ids_added_since(&after_stage_one, &after_stage_three);
    assert_eq!(
        stage_three_ids.len(),
        1,
        "the second guard must add its own row to the table; output was:\n{stdout}"
    );
    assert_ne!(
        stage_three_ids[0], stage_one_ids[0],
        "stage 3 must create a new assertion id, not observe stage 1's"
    );
    drop(streaming);

    wait_for_own_streaming_ids(
        reason,
        "the streaming assertion must be released within 5s of guard drop",
        |ids| !ids.contains(&stage_three_ids[0]),
    );
}

#[cfg(target_os = "macos")]
fn pmset_assertions() -> String {
    let output = std::process::Command::new("pmset")
        .args(["-g", "assertions"])
        .output()
        .expect("pmset -g assertions should run on macOS");
    assert!(output.status.success(), "pmset should succeed");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The bracketed assertion ids of this process's `pmset -g assertions` rows that
/// mention `needle`.
///
/// `pmset` has no pid filter option, so the owning pid is read off the row
/// prefix instead of matching the needle against the machine-global table. Each
/// row also prints `[0x...]`, which identifies that one assertion: its low 16
/// bits are the `IOPMAssertionID` that `IOPMAssertionCreateWithName` returned.
/// Verified on macOS by creating two assertions with one reason from one process
/// and reading both ids back: the returned ids `0x9d88`/`0x9d89` appeared as
/// `[0x...9d88]`/`[0x...9d89]`, so two live assertions from a single process are
/// distinguishable here.
#[cfg(target_os = "macos")]
fn own_streaming_row_ids(stdout: &str, needle: &str) -> Vec<String> {
    let own_prefix = format!("pid {}(", std::process::id());
    stdout
        .lines()
        .filter(|line| line.trim_start().starts_with(&own_prefix) && line.contains(needle))
        .filter_map(|line| {
            let open = line.find('[')?;
            let close = line[open..].find(']')? + open;
            Some(line[open + 1..close].to_string())
        })
        .collect()
}

/// The ids in `after` that were not in `before`. Comparing two reads of the same
/// process's table is what makes a stage's own assertion identifiable when the
/// process holds other assertions that match the same reason string.
#[cfg(target_os = "macos")]
fn ids_added_since(before: &[String], after: &[String]) -> Vec<String> {
    after.iter().filter(|id| !before.contains(id)).cloned().collect()
}

/// Poll `pmset` until `done` accepts this process's matching assertion ids,
/// bounded by 5s. Fails with `context` and the last table read if the deadline
/// passes first.
#[cfg(target_os = "macos")]
fn wait_for_own_streaming_ids<F>(reason: &str, context: &str, done: F)
where
    F: Fn(&[String]) -> bool,
{
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let stdout = pmset_assertions();
        if done(&own_streaming_row_ids(&stdout, reason)) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{context}; output was:\n{stdout}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Issue #432: `/rewind N` must interpret N against the same numbered list the
/// TUI shows, even in tool-heavy sessions where stored user-role tool-result
/// messages vastly outnumber real prompts.
#[test]
fn test_rewind_targets_match_rendered_transcript_numbering() {
    let mut session = Session::create_with_id(
        "session_rewind_numbering_test".to_string(),
        None,
        Some("rewind numbering".to_string()),
    );

    // Turn 1: prompt, assistant tool call, tool result, assistant answer.
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "prompt-1".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool_1".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
            thought_signature: None,
        }],
    );
    // Tool results are stored as user-role messages; the old index mapping
    // counted them as rewind targets even though the UI never numbers them.
    session.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: "file-a file-b".to_string(),
            is_error: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "answer-1".to_string(),
            cache_control: None,
        }],
    );

    // Turn 2: prompt + answer.
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "prompt-2".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "answer-2".to_string(),
            cache_control: None,
        }],
    );

    // The numbered /rewind list shows user/assistant transcript entries only:
    // 1 prompt-1, 2 answer-1, 3 prompt-2, 4 answer-2.
    let rendered_targets: Vec<String> = render_messages(&session)
        .into_iter()
        .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
        .map(|m| m.content)
        .collect();
    assert_eq!(rendered_targets, ["prompt-1", "answer-1", "prompt-2", "answer-2"]);

    let targets = session.rewind_target_stored_indices();
    assert_eq!(session.rewind_target_count(), 4);
    assert_eq!(targets.len(), 4);

    // Rewinding to entry 3 ("prompt-2") must keep everything through the
    // stored prompt-2 message (stored index 4 → len 5) and drop answer-2.
    assert_eq!(targets[2], 4);
    let mut rewound = session.clone();
    rewound.truncate_messages(targets[2] + 1);
    let remaining: Vec<String> = render_messages(&rewound)
        .into_iter()
        .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
        .map(|m| m.content)
        .collect();
    assert_eq!(remaining, ["prompt-1", "answer-1", "prompt-2"]);

    // The old stored-message mapping counted the tool result as target 3,
    // which would have chopped the transcript mid-turn (the #432 bug).
    assert_eq!(
        session.stored_len_for_visible_conversation_message(3),
        Some(3),
        "sanity: raw stored counting diverges, which is why rewind must not use it"
    );
}

/// Issue #688: `/rewind N` after an undo must honour the new N, not repeat the
/// previous rewind's target.
///
/// The reporter described the second `/rewind N` in a session ignoring its
/// argument and landing wherever the first one did. #432 covered the *first*
/// rewind's numbering; nothing covered rewind -> undo -> rewind, which is the
/// sequence that loses transcript if the target list is computed once and
/// reused. Drive the real agent-side operations so the whole cycle is pinned.
#[test]
fn test_rewind_after_undo_uses_the_new_target_not_the_previous_one() {
    let mut session = Session::create_with_id(
        "session_rewind_repeat_test".to_string(),
        None,
        Some("rewind repeat".to_string()),
    );
    for turn in 1..=6 {
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("prompt-{turn}"),
                cache_control: None,
            }],
        );
        session.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: format!("answer-{turn}"),
                cache_control: None,
            }],
        );
    }

    let numbered = |session: &Session| -> Vec<String> {
        render_messages(session)
            .into_iter()
            .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
            .map(|m| m.content)
            .collect()
    };

    let full = numbered(&session);
    assert_eq!(full.len(), 12);
    let before_rewind = session.active_context().to_vec();

    // First rewind: to entry 4 ("answer-2").
    let targets = session.rewind_target_stored_indices();
    session.truncate_messages(targets[4 - 1] + 1);
    assert_eq!(numbered(&session).len(), 4);
    assert_eq!(numbered(&session).last().unwrap(), "answer-2");

    // Undo restores the full transcript, exactly as `Agent::undo_rewind` does.
    session.replace_messages(before_rewind);
    assert_eq!(numbered(&session), full, "undo must restore the original transcript");

    // Second rewind to a *different, larger* N. The bug report says this lands
    // back on the first rewind's target; it must honour 11.
    let targets = session.rewind_target_stored_indices();
    assert_eq!(
        targets.len(),
        12,
        "targets must be recomputed against the restored transcript"
    );
    session.truncate_messages(targets[11 - 1] + 1);

    let after = numbered(&session);
    assert_eq!(
        after.len(),
        11,
        "rewind 11 must keep 11 entries, not fall back to the earlier target of 4"
    );
    assert_eq!(after.last().unwrap(), "prompt-6");
    assert_eq!(session.rewind_target_count(), 11);
}

#[test]
fn restored_tool_image_boundaries_follow_returned_history_rows() {
    let mut session = Session::create_with_id("image-boundaries".into(), None, None);
    let text = |text: &str| ContentBlock::Text {
        text: text.into(),
        cache_control: None,
    };
    let result = |id: &str| ContentBlock::ToolResult {
        tool_use_id: id.into(),
        content: "read image".into(),
        is_error: None,
    };
    let image = |data: &str| ContentBlock::Image {
        media_type: "image/png".into(),
        data: data.into(),
    };
    session.add_message(Role::User, vec![text("prompt")]);
    session.add_message(
        Role::Assistant,
        vec![
            text("before read"),
            ContentBlock::ToolUse {
                id: "read-1".into(),
                name: "read".into(),
                input: serde_json::json!({}),
                thought_signature: None,
            },
        ],
    );
    session.add_message(
        Role::User,
        vec![
            result("read-1"),
            image("one"),
            image("two"),
            result("read-2"),
            image("three"),
        ],
    );
    session.add_message(Role::Assistant, vec![text("after read")]);
    let (messages, images) = render_messages_and_images(&session);
    assert_eq!(
        messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
        ["user", "assistant", "tool", "tool", "assistant"]
    );
    assert_eq!(
        images.iter().map(|i| i.history_message_index).collect::<Vec<_>>(),
        [Some(3), Some(3), Some(4)]
    );
    assert_eq!(messages[images[2].history_message_index.unwrap()].content, "after read");
    assert_eq!(
        images[0].anchor,
        Some(RenderedImageAnchor::ToolCall { id: "read-1".into() })
    );

    // Compaction adds a synthetic notice. Boundaries count returned rows, not
    // stored message indices or user-prompt ordinals.
    session.compaction = Some(StoredCompactionState {
        summary_text: "summary".into(),
        openai_encrypted_content: None,
        native_window: Vec::new(),
        native_route: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
        structured: None,
    });
    let (messages, images, _) = render_messages_and_images_with_compacted_history(&session, usize::MAX);
    assert_eq!(messages[0].role, "system");
    assert_eq!(
        images.iter().map(|i| i.history_message_index).collect::<Vec<_>>(),
        [Some(4), Some(4), Some(5)]
    );
}

#[test]
fn restored_tool_image_boundary_can_be_history_end() {
    let mut session = Session::create_with_id("image-tail-boundary".into(), None, None);
    session.add_message(
        Role::User,
        vec![
            ContentBlock::ToolResult {
                tool_use_id: "orphan".into(),
                content: String::new(),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".into(),
                data: "image".into(),
            },
        ],
    );
    let (messages, images) = render_messages_and_images(&session);
    assert_eq!(images[0].history_message_index, Some(messages.len()));
}

#[test]
fn rendered_image_history_boundary_is_backward_compatible() {
    let legacy = serde_json::json!({"media_type": "image/png", "data": "bytes", "label": null,
        "source": {"kind": "tool_result", "tool_name": "read"}, "anchor": {"kind": "tool_call", "id": "read-1"}});
    let mut image: RenderedImage = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(image.history_message_index, None);
    assert_eq!(serde_json::to_value(&image).unwrap(), legacy);
    image.history_message_index = Some(2);
    let encoded = serde_json::to_value(&image).unwrap();
    assert_eq!(encoded["history_message_index"], 2);
    assert_eq!(serde_json::from_value::<RenderedImage>(encoded).unwrap(), image);
}

#[test]
fn cache_prompt_totals_preserve_mixed_provider_accounting_and_legacy_unknown() {
    let mut session = Session::create_with_id("cache_prompt_totals".into(), None, None);
    for usage in [
        // Inclusive OpenAI input: read and write are subsets.
        StoredTokenUsage {
            prompt_tokens: Some(10_000),
            input_tokens: 10_000,
            output_tokens: 100,
            cache_read_input_tokens: Some(6_000),
            cache_creation_input_tokens: Some(2_000),
        },
        // Anthropic uncached input: read and write are disjoint.
        StoredTokenUsage {
            prompt_tokens: Some(10_000),
            input_tokens: 1_000,
            output_tokens: 100,
            cache_read_input_tokens: Some(7_000),
            cache_creation_input_tokens: Some(2_000),
        },
    ] {
        let json = serde_json::to_string(&usage).unwrap();
        let restored: StoredTokenUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.prompt_tokens, Some(10_000));
        session.add_message_ext(Role::Assistant, vec![], None, Some(restored));
    }
    let totals = session.token_usage_totals();
    assert_eq!(totals.cache_prompt_tokens, Some(20_000));
    assert_eq!(totals.cache_reported_input_tokens, 11_000);
    assert_eq!(totals.cache_read_input_tokens, 13_000);
    assert_eq!(totals.cache_creation_input_tokens, 4_000);
    let legacy: StoredTokenUsage = serde_json::from_str(r#"{"input_tokens":10000,"output_tokens":100,"cache_read_input_tokens":6000,"cache_creation_input_tokens":2000}"#).unwrap();
    assert_eq!(legacy.prompt_tokens, None);
    session.add_message_ext(Role::Assistant, vec![], None, Some(legacy));
    assert_eq!(session.token_usage_totals().cache_prompt_tokens, None);
    assert_eq!(session.token_usage_totals().cache_read_input_tokens, 19_000);
}

#[test]
fn system_prompt_persists_before_first_message_and_across_metadata_updates() -> Result<()> {
    let _lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("HORON_HOME", home.path());
    for prompt in ["custom system prompt", ""] {
        let mut session = Session::create(None, None);
        assert_eq!(session.system_prompt, None);
        session.system_prompt = Some(prompt.into());
        session.save()?;
        assert_eq!(Session::load(&session.id)?.system_prompt.as_deref(), Some(prompt));
        assert_eq!(
            Session::load_startup_stub(&session.id)?.system_prompt.as_deref(),
            Some(prompt)
        );
        // Unchanged prompt survives metadata-only journal persistence too.
        session.model = Some("test-model".into());
        session.save()?;
        assert_eq!(Session::load(&session.id)?.system_prompt.as_deref(), Some(prompt));
        session.system_prompt = Some("replacement".into());
        session.save()?;
        assert_eq!(
            Session::load_startup_stub(&session.id)?.system_prompt.as_deref(),
            Some("replacement")
        );
        session.system_prompt = None;
        session.save()?;
        assert_eq!(Session::load(&session.id)?.system_prompt, None);
    }
    Ok(())
}

#[test]
fn system_prompt_missing_in_legacy_session_defaults_to_none() -> Result<()> {
    let session = Session::create_with_id("legacy-prompt-test".into(), None, None);
    let json = serde_json::to_value(&session)?;
    assert!(json.get("system_prompt").is_none());
    let restored: Session = serde_json::from_value(json)?;
    assert_eq!(restored.system_prompt, None);
    Ok(())
}

#[test]
fn first_visible_user_prompt_becomes_the_generated_title() {
    let mut session = Session::create_with_id("session_prompt_title_1".to_string(), None, None);
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "<system-reminder>\n# Session Context\n</system-reminder>".into(),
            cache_control: None,
        }],
    );
    session.add_message_with_display_role(
        Role::User,
        vec![ContentBlock::Text {
            text: "background finished".into(),
            cache_control: None,
        }],
        Some(StoredDisplayRole::BackgroundTask),
    );
    assert_eq!(session.title, None);
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "<transcription>\nFix the   sidebar names\n</transcription>".into(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "second prompt".into(),
            cache_control: None,
        }],
    );
    assert_eq!(session.display_title(), Some("Fix the sidebar names"));

    session.rename_title(Some("Custom".into()));
    assert_eq!(session.display_title(), Some("Custom"));
}

// Produces the superseded format only as an explicit legacy input fixture.
fn save_legacy_fixture(session: &mut Session) -> Result<()> {
    use super::super::journal::{PersistVectorMode, SessionJournalEntry};
    session.updated_at += chrono::Duration::seconds(1);
    let snapshot = session_path(&session.id)?;
    let journal = session_journal_path(&session.id)?;
    if !snapshot.exists() || session.persist_state.messages_mode == PersistVectorMode::Full {
        let mut old = session.history_snapshot()?;
        old.status = SessionStatus::Closed;
        old.last_pid = None;
        crate::storage::write_json(&snapshot, &old)?;
        if journal.exists() {
            std::fs::remove_file(journal)?;
        }
    } else {
        let mut meta = session.journal_meta();
        meta.status = SessionStatus::Closed;
        meta.last_pid = None;
        let entry = SessionJournalEntry {
            meta,
            append_messages: session.active_context()[session.persist_state.messages_len..].to_vec(),
            append_env_snapshots: Vec::new(),
            append_memory_injections: Vec::new(),
            append_replay_events: Vec::new(),
        };
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(journal)?;
        writeln!(file, "{}", serde_json::to_string(&entry)?)?;
        file.sync_all()?;
    }
    session.reset_persist_state(true);
    Ok(())
}

fn legacy_source_bytes(id: &str) -> Result<Vec<(std::path::PathBuf, Vec<u8>)>> {
    let snapshot = session_path(id)?;
    [
        snapshot.clone(),
        session_journal_path(id)?,
        snapshot.with_extension("bak"),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .map(|path| Ok((path.clone(), std::fs::read(path)?)))
    .collect()
}
fn assert_legacy_sources_unchanged(sources: &[(std::path::PathBuf, Vec<u8>)]) -> Result<()> {
    for (path, original) in sources {
        assert_eq!(&std::fs::read(path)?, original);
    }
    Ok(())
}

/// FNV-1a over a byte slice, printed in evidence so two runs can be compared.
fn projection_digest(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn cached_projection_bytes(session: &mut Session) -> Vec<u8> {
    serde_json::to_vec(session.provider_messages()).expect("cached projection json")
}

fn uncached_projection_bytes(session: &Session) -> Vec<u8> {
    serde_json::to_vec(&session.messages_for_provider_uncached()).expect("uncached projection json")
}

fn screenshot_message(index: usize) -> StoredMessage {
    StoredMessage {
        turn_start: false,
        id: format!("msg_{index}"),
        role: if index.is_multiple_of(2) {
            Role::User
        } else {
            Role::Assistant
        },
        content: vec![
            ContentBlock::ToolUse {
                id: format!("call_{index}"),
                name: "read".into(),
                input: serde_json::json!({}),
                thought_signature: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: format!("call_{index}"),
                content: format!("Captured main display #{index}"),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "A".repeat(4 * 1024),
            },
        ],
        ..StoredMessage::default()
    }
}

#[test]
fn provider_context_cache_key_tracks_revision_route_and_compaction_generation() {
    let mut session = Session::create_with_id(
        "session_provider_cache_key_test".to_string(),
        None,
        Some("Provider cache key".to_string()),
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first prompt".to_string(),
            cache_control: None,
        }],
    );

    let _ = session.provider_messages();
    let key_after_first_build = session.provider_context_cache_key();
    let materializations = session.provider_context_stats().0;

    // Unchanged inputs reuse the cache instead of rebuilding it.
    let _ = session.provider_messages();
    assert_eq!(session.provider_context_stats().0, materializations);
    assert_eq!(session.provider_context_cache_key(), key_after_first_build);

    // Same message count, different body: only the revision moves, and the key
    // must move with it or the stale projection would be served.
    let mut revised = session.active_context()[0].clone();
    revised.content = vec![ContentBlock::Text {
        text: "second prompt".to_string(),
        cache_control: None,
    }];
    session.replace_messages(vec![revised]);
    assert_ne!(
        session.provider_context_cache_key(),
        key_after_first_build,
        "a new message revision must change the cache key"
    );
    let _ = session.provider_messages();
    assert_eq!(
        session.provider_context_stats().0,
        materializations + 1,
        "the changed revision rebuilds the projection"
    );
    let rebuilt_text = session.provider_messages()[0]
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        });
    assert_eq!(
        rebuilt_text.as_deref(),
        Some("second prompt"),
        "the rebuilt projection carries the revised body"
    );

    // A route change invalidates the projection even with an identical transcript.
    let key_before_route = session.provider_context_cache_key();
    session.model = Some("model-a".to_string());
    assert_ne!(session.provider_context_cache_key(), key_before_route);
    let key_after_route = session.provider_context_cache_key();
    session.route_api_method = Some("openai-api".to_string());
    assert_ne!(session.provider_context_cache_key(), key_after_route);

    // A new compaction generation invalidates it as well.
    let key_before_generation = session.provider_context_cache_key();
    session.set_compaction_generation(7);
    assert_ne!(session.provider_context_cache_key(), key_before_generation);
    let key_after_generation = session.provider_context_cache_key();
    session.set_compaction_generation(7);
    assert_eq!(
        session.provider_context_cache_key(),
        key_after_generation,
        "recording the same generation is not a change"
    );
    session.set_compaction_generation(8);
    assert_ne!(session.provider_context_cache_key(), key_after_generation);
}

#[test]
fn provider_projection_bytes_match_the_uncached_construction() {
    let mut session = Session::create_with_id(
        "session_provider_projection_bytes_test".to_string(),
        None,
        Some("Provider projection bytes".to_string()),
    );
    // Past the decay trigger so the image branches of the projection run.
    for index in 0..(horon_agent_compaction_core::TOOL_IMAGE_DECAY_TRIGGER_COUNT + 4) {
        session.messages.push(screenshot_message(index));
        session.mark_messages_full_dirty();
    }

    let uncached = uncached_projection_bytes(&session);
    let cached = cached_projection_bytes(&mut session);
    assert_eq!(
        cached, uncached,
        "the cached projection must be byte-identical to the uncached construction"
    );
    println!("DECAY digest={} bytes={}", projection_digest(&cached), cached.len());

    session.strip_all_images();
    let uncached = uncached_projection_bytes(&session);
    let cached = cached_projection_bytes(&mut session);
    assert_eq!(cached, uncached, "the stripped projection must stay byte-identical");
    println!("STRIP digest={} bytes={}", projection_digest(&cached), cached.len());

    session.strip_oversized_images(8 * 1024);
    let uncached = uncached_projection_bytes(&session);
    let cached = cached_projection_bytes(&mut session);
    assert_eq!(cached, uncached, "the budgeted projection must stay byte-identical");
    println!("BUDGET digest={} bytes={}", projection_digest(&cached), cached.len());
}

/// Provider projection digest for the fixed fixture below, captured from the
/// tree before the projection cache landed and pinned here so any change to what the provider
/// receives (message order, content, image decay, stripping, budget) fails.
const PROVIDER_PROJECTION_BASELINE_DIGEST: &str = "53f3177785204cdd";

#[test]
fn provider_projection_digest_matches_the_pre_change_baseline() {
    let mut session = Session::create_with_id(
        "session_provider_projection_digest".to_string(),
        None,
        Some("Provider projection digest".to_string()),
    );
    for index in 0..(horon_agent_compaction_core::TOOL_IMAGE_DECAY_TRIGGER_COUNT + 4) {
        session.messages.push(StoredMessage {
            turn_start: false,
            id: format!("msg_{index}"),
            role: if index.is_multiple_of(2) {
                Role::User
            } else {
                Role::Assistant
            },
            content: vec![
                ContentBlock::Text {
                    text: format!("turn {index}"),
                    cache_control: None,
                },
                ContentBlock::ToolUse {
                    id: format!("call_{index}"),
                    name: "read".into(),
                    input: serde_json::json!({"path": "src/main.rs"}),
                    thought_signature: None,
                },
                ContentBlock::ToolResult {
                    tool_use_id: format!("call_{index}"),
                    content: format!("Captured main display #{index}"),
                    is_error: None,
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "A".repeat(4 * 1024),
                },
            ],
            timestamp: None,
            ..Default::default()
        });
        session.mark_messages_full_dirty();
    }

    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    let decayed = serde_json::to_vec(session.provider_messages()).expect("decayed projection");
    feed(&decayed);
    let decayed_len = decayed.len();

    session.strip_all_images();
    let stripped = serde_json::to_vec(session.provider_messages()).expect("stripped projection");
    feed(&stripped);
    let stripped_len = stripped.len();

    session.strip_oversized_images(8 * 1024);
    let budgeted = serde_json::to_vec(session.provider_messages()).expect("budgeted projection");
    feed(&budgeted);
    let budgeted_len = budgeted.len();

    println!("QA_PROJECTION digest={hash:016x} decayed={decayed_len} stripped={stripped_len} budgeted={budgeted_len}");
    assert_eq!(
        format!("{hash:016x}"),
        PROVIDER_PROJECTION_BASELINE_DIGEST,
        "the provider must receive the same bytes as before the change"
    );
    assert_eq!((decayed_len, stripped_len, budgeted_len), (30795, 7035, 7035));
}
