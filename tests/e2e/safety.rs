// =============================================================================
// Ambient Mode Integration Tests
// =============================================================================

/// Test safety system: action classification
#[test]
fn test_safety_classification() {
    use horon_agent::safety::SafetySystem;

    let safety = SafetySystem::new();

    // Tier 1: auto-allowed
    assert!(safety.classify("read") == horon_agent::safety::ActionTier::AutoAllowed);
    assert!(safety.classify("glob") == horon_agent::safety::ActionTier::AutoAllowed);
    assert!(safety.classify("grep") == horon_agent::safety::ActionTier::AutoAllowed);
    assert!(safety.classify("memory") == horon_agent::safety::ActionTier::AutoAllowed);
    assert!(safety.classify("todoread") == horon_agent::safety::ActionTier::AutoAllowed);
    assert!(safety.classify("todowrite") == horon_agent::safety::ActionTier::AutoAllowed);

    // Tier 2: requires permission
    assert!(safety.classify("bash") == horon_agent::safety::ActionTier::RequiresPermission);
    assert!(safety.classify("edit") == horon_agent::safety::ActionTier::RequiresPermission);
    assert!(safety.classify("write") == horon_agent::safety::ActionTier::RequiresPermission);
    assert!(safety.classify("create_pull_request") == horon_agent::safety::ActionTier::RequiresPermission);
    assert!(safety.classify("send_email") == horon_agent::safety::ActionTier::RequiresPermission);

    // Case insensitive
    assert!(safety.classify("READ") == horon_agent::safety::ActionTier::AutoAllowed);
    assert!(safety.classify("Bash") == horon_agent::safety::ActionTier::RequiresPermission);
}

/// Test safety system: permission request queue + decision flow
#[test]
fn test_safety_permission_flow() {
    use horon_agent::safety::{ApprovalDefault, PermissionRequest, PermissionResult, SafetySystem, Urgency};

    let safety = SafetySystem::new();

    // Count existing pending requests (may have leftover state from other tests)
    let baseline = safety.pending_requests().len();

    // Queue a permission request
    let req = PermissionRequest {
        id: "test_perm_flow_001".to_string(),
        action: "create_pull_request".to_string(),
        description: "Create PR for auth fixes".to_string(),
        rationale: "Found 3 failing auth tests".to_string(),
        urgency: Urgency::High,
        wait: false,
        created_at: chrono::Utc::now(),
        context: None,
        run_id: None,
        expires_at: None,
        default_on_timeout: ApprovalDefault::Deny,
    };

    let result = safety.request_permission(req);
    assert!(matches!(result, PermissionResult::Queued { .. }));

    // Verify our request was added
    let pending = safety.pending_requests();
    assert_eq!(pending.len(), baseline + 1);
    assert!(
        pending
            .iter()
            .any(|p| p.action == "create_pull_request" && p.id == "test_perm_flow_001")
    );

    // Record an approval decision
    let _ = safety.record_decision("test_perm_flow_001", true, "test", Some("looks good".to_string()));

    // Verify our request was removed
    assert_eq!(safety.pending_requests().len(), baseline);
}

/// Test safety system: transcript saving
#[test]
fn test_safety_transcript() {
    use horon_agent::safety::{AmbientTranscript, SafetySystem, TranscriptStatus};

    let safety = SafetySystem::new();

    let transcript = AmbientTranscript {
        session_id: "test_ambient_001".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: TranscriptStatus::Complete,
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
        actions: vec![],
        pending_permissions: 0,
        summary: Some("Test cycle completed".to_string()),
        compactions: 0,
        memories_modified: 3,
        conversation: None,
    };

    // Should not panic
    let result = safety.save_transcript(&transcript);
    assert!(result.is_ok());
}

/// Test safety system: summary generation
#[test]
fn test_safety_summary_generation() {
    use horon_agent::safety::{ActionLog, ActionTier, SafetySystem};

    let safety = SafetySystem::new();

    // Log some actions
    safety.log_action(ActionLog {
        action_type: "memory_consolidation".to_string(),
        description: "Merged 2 duplicate memories".to_string(),
        tier: ActionTier::AutoAllowed,
        details: None,
        timestamp: chrono::Utc::now(),
    });

    safety.log_action(ActionLog {
        action_type: "memory_prune".to_string(),
        description: "Pruned 1 stale memory".to_string(),
        tier: ActionTier::AutoAllowed,
        details: None,
        timestamp: chrono::Utc::now(),
    });

    let summary = safety.generate_summary();
    assert!(summary.contains("Merged 2 duplicate memories"));
    assert!(summary.contains("Pruned 1 stale memory"));
}
