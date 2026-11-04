use super::helpers::*;
use horon_agent_session_store::legacy::*;
use horon_agent_session_store::*;
use serde_json::{Value, json};
use std::fs;

#[test]
fn historical_vectors_and_raw_unknown_fields_are_preserved_as_inert_data() {
    // Given old auxiliary/cache data and appended vectors with future fields.
    let home = fixture();
    let mut raw = snapshot();
    raw["env_snapshots"] = json!([{"old":true}]);
    raw["memory_injections"] = json!([{"unknown":u64::MAX}]);
    raw["replay_events"] = json!([{"instruction":"execute nothing"}]);
    raw["provider_messages_cache"] = json!([{"opaque":[9,8,7]}]);
    put(&home, "sessions/old.json", &serde_json::to_vec(&raw).unwrap());
    let mut appended: Value =
        serde_json::from_str(&journal(vec![msg("four", "assistant")], "2020-01-03T00:00:00Z")).unwrap();
    appended["append_env_snapshots"] = json!([{"new":true}]);
    appended["append_memory_injections"] = json!([{"second":2}]);
    appended["append_replay_events"] = json!([{"second":3}]);
    appended["meta"]["future_meta"] = json!({"exact":u64::MAX});
    appended["future_entry"] = json!(["ordered", 3, 2, 1]);
    put(
        &home,
        "sessions/old.journal.jsonl",
        &serde_json::to_vec(&appended).unwrap(),
    );
    let before = hashes(&home);
    // When converting to events.
    source(&home).migrate(&paths(&home)).unwrap();
    // Then vectors retain order, raw future metadata survives, cache is inert.
    let view = view(&home);
    assert_eq!(
        view.metadata().legacy_extensions["future_meta"]["exact"],
        json!(u64::MAX)
    );
    let AuxiliaryRecordedPayload::Legacy { value, .. } = &view.auxiliary()[0].1 else {
        panic!("legacy auxiliary");
    };
    assert_eq!(value["env_snapshots"], json!([{"old":true},{"new":true}]));
    assert_eq!(value["memory_injections"][0]["unknown"], json!(u64::MAX));
    assert_eq!(value["replay_events"].as_array().unwrap().len(), 2);
    assert_eq!(value["provider_messages_cache"], json!([{"opaque":[9,8,7]}]));
    assert!(
        !view
            .metadata()
            .legacy_extensions
            .contains_key("provider_messages_cache")
    );
    preserved(&home, &before);
}

#[test]
fn missing_compaction_identity_is_explicit_and_raw_artifact_is_not_lost() {
    // Given a legacy compaction with no provable provider/route identity.
    let home = fixture();
    let mut raw = snapshot();
    raw["model"] = Value::Null;
    raw["compaction"] = json!({"summary_text":"old summary","compacted_count":2,
        "covers_up_to_turn":20,"original_turn_count":20,"future_window":[3,1,2]});
    put(&home, "sessions/old.json", &serde_json::to_vec(&raw).unwrap());
    // When preserving rather than inventing a valid D4 identity.
    let report = source(&home).migrate(&paths(&home)).unwrap();
    // Then the warning and exact artifact survive, with full original history available.
    assert!(
        report
            .receipt
            .diagnostics
            .iter()
            .any(|d| matches!(d, LegacyDiagnostic::CompactionPreservedInert { .. }))
    );
    let view = view(&home);
    assert!(view.compaction().is_none());
    let AuxiliaryRecordedPayload::Legacy { value, .. } = &view.auxiliary()[0].1 else {
        panic!("legacy auxiliary");
    };
    assert_eq!(value["compaction"], raw["compaction"]);
    assert_eq!(view.active_messages().len(), 3);
}

#[test]
fn absent_times_use_epoch_and_large_auxiliary_uses_original_blob() {
    // Given missing legacy times and a >8MiB auxiliary document.
    let home = fixture();
    let mut raw = snapshot();
    raw.as_object_mut().unwrap().remove("created_at");
    raw.as_object_mut().unwrap().remove("updated_at");
    raw["provider_messages_cache"] = json!({"large":"z".repeat(8*1024*1024+1)});
    put(&home, "sessions/old.json", &serde_json::to_vec(&raw).unwrap());
    let before = hashes(&home);
    // When migrating real blob storage.
    source(&home).migrate(&paths(&home)).unwrap();
    // Then timestamp fallback and full blob value are exact.
    let report = full_scan(&paths(&home), &SessionId::parse("old").unwrap()).unwrap();
    assert_eq!(report.events[0].timestamp, chrono::DateTime::UNIX_EPOCH);
    assert!(report.events.iter().any(|e| e.payload_ref.is_some()));
    let view = view(&home);
    let AuxiliaryRecordedPayload::Legacy { value, .. } = &view.auxiliary()[0].1 else {
        panic!("legacy auxiliary");
    };
    assert_eq!(value["provider_messages_cache"], raw["provider_messages_cache"]);
    assert!(
        !fs::read_dir(home.join("sessions/old/blobs"))
            .unwrap()
            .collect::<Vec<_>>()
            .is_empty()
    );
    preserved(&home, &before);
}

#[test]
fn zero_count_transfer_summary_is_active_when_child_has_no_messages() {
    // Given the normal transfer child contract: Closed, empty history, inherited identity.
    let home = fixture();
    let mut raw = snapshot();
    raw["status"] = json!("Closed");
    raw["messages"] = json!([]);
    raw["compaction"] = json!({"summary_text":"Carry the unrepeated deployment decision.",
        "compacted_count":0,"covers_up_to_turn":2,"original_turn_count":2});
    let old: horon_agent_session_types::StoredCompactionState =
        serde_json::from_value(raw["compaction"].clone()).unwrap();
    put(&home, "sessions/old.json", &serde_json::to_vec(&raw).unwrap());
    let before = hashes(&home);
    // When the actual migration publishes that child.
    let report = source(&home).migrate(&paths(&home)).unwrap();
    // Then the inherited summary is active, bound to the real creation event.
    let projection = view(&home);
    let compacted = &projection.compaction().expect("transfer summary must remain active").1;
    assert_eq!(compacted.summary_text, old.summary_text);
    assert_eq!(compacted.provider, raw["provider_key"]);
    assert_eq!(compacted.model, raw["model"]);
    assert_eq!(compacted.route, raw["route_api_method"]);
    assert_eq!(
        compacted.source_head_id.as_ref(),
        Some(&compacted.covered_through_event_id)
    );
    assert!(compacted.first_kept_event_id.is_none());
    assert!(projection.active_messages().is_empty());
    assert!(projection.messages().is_empty());
    assert!(
        !report
            .receipt
            .diagnostics
            .iter()
            .any(|d| matches!(d, LegacyDiagnostic::CompactionPreservedInert { .. }))
    );
    preserved(&home, &before);
}

#[test]
fn zero_count_transfer_summary_keeps_every_later_message_active() {
    // Given an inherited transfer summary followed by new messages.
    let home = fixture();
    let mut raw = snapshot();
    raw["status"] = json!("Closed");
    raw["compaction"] = json!({"summary_text":"Earlier parent work remains relevant.",
        "compacted_count":0,"covers_up_to_turn":20,"original_turn_count":20});
    let expected: Vec<horon_agent_session_types::StoredMessage> =
        serde_json::from_value(raw["messages"].clone()).unwrap();
    put(&home, "sessions/old.json", &serde_json::to_vec(&raw).unwrap());
    let before = hashes(&home);
    // When migrating without treating the old parent's count as a local offset.
    source(&home).migrate(&paths(&home)).unwrap();
    // Then the exact summary and entire local tail remain active.
    let projection = view(&home);
    let compacted = &projection.compaction().expect("transfer summary must remain active").1;
    assert_eq!(compacted.summary_text, raw["compaction"]["summary_text"]);
    assert_eq!(
        compacted.first_kept_event_id.as_ref(),
        Some(&projection.messages()[0].event_id)
    );
    let active = projection
        .active_messages()
        .iter()
        .map(|m| &m.message)
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_value(active).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(projection.messages().len(), 3);
    preserved(&home, &before);
}

#[test]
fn transfer_summary_remains_inert_when_identity_or_coverage_is_unprovable() {
    // Given missing identity or coverage that would discard the latest user.
    for (missing, count) in [("model", 0), ("route_api_method", 0), ("", 3)] {
        let home = fixture();
        let mut raw = snapshot();
        raw["status"] = json!("Closed");
        if !missing.is_empty() {
            raw[missing] = Value::Null;
        }
        raw["compaction"] = json!({"summary_text":"Preserve but do not invent.",
            "compacted_count":count,"covers_up_to_turn":3,"original_turn_count":3});
        put(&home, "sessions/old.json", &serde_json::to_vec(&raw).unwrap());
        let before = hashes(&home);
        // When migration cannot prove a valid active summary.
        let report = source(&home).migrate(&paths(&home)).unwrap();
        // Then the exact artifact and all messages survive with explicit diagnostics.
        let projection = view(&home);
        assert!(projection.compaction().is_none());
        assert_eq!(projection.active_messages().len(), 3);
        assert!(
            report
                .receipt
                .diagnostics
                .iter()
                .any(|d| matches!(d, LegacyDiagnostic::CompactionPreservedInert { .. }))
        );
        let AuxiliaryRecordedPayload::Legacy { value, .. } = &projection.auxiliary()[0].1 else {
            panic!("legacy auxiliary");
        };
        assert_eq!(value["compaction"], raw["compaction"]);
        preserved(&home, &before);
    }
}
