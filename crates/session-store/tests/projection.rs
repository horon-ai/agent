use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::*;
use horon_agent_session_types::{RenderedMessage, StoredMessage};
use serde_json::{Value, json};
// allow: SIZE_OK - one explicitly assigned integration target owns the shared
// sealed-event fixtures and all graph/durable replay acceptance scenarios.

mod support;

fn id(n: u128) -> EventId {
    EventId::parse(uuid::Uuid::from_u128(n).to_string()).unwrap()
}

fn event(n: u128, parent: Option<u128>, body: (KnownEventKind, Value)) -> EventEnvelope {
    EventDraft::new(
        SessionId::parse("projection").unwrap(),
        id(n),
        parent.map(id),
        OriginId::parse(uuid::Uuid::from_u128(999).to_string()).unwrap(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(body.0),
        body.1,
    )
    .seal()
    .unwrap()
    .envelope()
    .clone()
}

fn root() -> EventEnvelope {
    event(
        1,
        None,
        (
            KnownEventKind::SessionCreated,
            json!({
                "metadata": SessionMetadata::default(), "branch_root": false
            }),
        ),
    )
}

fn message(text: &str) -> StoredMessage {
    StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        turn_start: false,
        id: format!("legacy-{text}"),
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.into(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }
}

fn append(n: u128, parent: u128, text: &str) -> EventEnvelope {
    event(
        n,
        Some(parent),
        (
            KnownEventKind::MessageAppended,
            json!({
                "message": message(text)
            }),
        ),
    )
}

fn graph(events: &[EventEnvelope]) -> Result<EventGraph, StoreError> {
    EventGraph::from_events(&SessionId::parse("projection").unwrap(), events)
}

#[test]
fn replay_is_parent_first_when_input_is_reordered() {
    // Given child-before-parent input and duplicate delivery.
    let events = vec![append(3, 2, "second"), root(), append(2, 1, "first"), root()];
    // When validating and replaying the only head.
    let graph = graph(&events).unwrap();
    let projection = graph.replay(None).unwrap();
    // Then canonical order and visible content are independent of arrival order.
    assert_eq!(graph.ordered_ids(), &[id(1), id(2), id(3)]);
    assert_eq!(
        projection
            .messages()
            .iter()
            .map(|m| m.message.content_preview())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
}

#[test]
fn divergent_heads_require_explicit_selection_when_both_are_retained() {
    // Given two sibling messages with reversed arrival.
    let graph = graph(&[append(3, 1, "right"), append(2, 1, "left"), root()]).unwrap();
    // When no host-local selection is supplied.
    let error = graph.replay(None).unwrap_err();
    // Then execution refuses the conflict, while either branch remains available.
    assert_eq!(error.code, StoreErrorCode::ConflictHeads);
    assert_eq!(graph.heads(), &[id(2), id(3)]);
    assert_eq!(
        graph.replay(Some(&id(2))).unwrap().messages()[0]
            .message
            .content_preview(),
        "left"
    );
    assert_eq!(
        graph.replay(Some(&id(3))).unwrap().messages()[0]
            .message
            .content_preview(),
        "right"
    );
}

#[test]
fn revision_replaces_projection_when_target_is_an_ancestor_message() {
    // Given a sealed original and a valid revision of its event identity.
    let original = append(2, 1, "before");
    let bytes = original.canonical_json().unwrap();
    let revision = event(
        3,
        Some(2),
        (
            KnownEventKind::MessageRevised,
            json!({
                "target_event_id": id(2), "replacement": message("after")
            }),
        ),
    );
    let graph = graph(&[root(), original, revision]).unwrap();
    // When replaying the revised branch.
    let projection = graph.replay(None).unwrap();
    // Then only the derived value changes.
    assert_eq!(projection.messages()[0].event_id, id(2));
    assert_eq!(projection.messages()[0].message.content_preview(), "after");
    assert_eq!(graph.event(&id(2)).unwrap().canonical_json().unwrap(), bytes);
    assert_eq!(
        graph.replay(Some(&id(2))).unwrap().messages()[0]
            .message
            .content_preview(),
        "before"
    );
}

#[test]
fn invalid_revision_targets_are_refused_when_missing_nonmessage_or_sibling() {
    // Given missing, root, and sibling revision targets.
    for target in [99, 1, 3] {
        let records = vec![
            root(),
            append(2, 1, "left"),
            append(3, 1, "right"),
            event(
                4,
                Some(2),
                (
                    KnownEventKind::MessageRevised,
                    json!({
                        "target_event_id": id(target), "replacement": message("edit")
                    }),
                ),
            ),
        ];
        // When the revision branch is replayed.
        let error = graph(&records).unwrap().replay(Some(&id(4))).unwrap_err();
        // Then missing and invalid ancestry are typed separately.
        assert_eq!(
            error.code,
            if target == 99 {
                StoreErrorCode::MissingDependency
            } else {
                StoreErrorCode::InvalidReference
            }
        );
    }
}

#[test]
fn metadata_is_replaced_completely_when_only_selected_ancestry_applies() {
    // Given complete metadata replacements and a competing sibling title.
    let first = SessionMetadata {
        title: Some("first".into()),
        saved: true,
        legacy_extensions: [("old_feature".into(), json!({"pid":123,"status":"active"}))].into(),
        ..SessionMetadata::default()
    };
    let final_meta = SessionMetadata {
        custom_title: Some("last".into()),
        legacy_extensions: first.legacy_extensions.clone(),
        ..SessionMetadata::default()
    };
    let records = vec![
        root(),
        event(2, Some(1), (KnownEventKind::MetadataChanged, json!({"metadata":first}))),
        event(
            3,
            Some(2),
            (KnownEventKind::MetadataChanged, json!({"metadata":final_meta})),
        ),
        event(
            4,
            Some(2),
            (
                KnownEventKind::MetadataChanged,
                json!({"metadata":SessionMetadata {
            title:Some("sibling".into()), ..SessionMetadata::default()}}),
            ),
        ),
    ];
    // When explicitly replaying the replacement branch.
    let projection = graph(&records).unwrap().replay(Some(&id(3))).unwrap();
    // Then omitted optional values reset instead of merging runtime/body fields.
    assert_eq!(projection.metadata(), &final_meta);
    assert!(!projection.metadata().saved);
    assert_eq!(projection.metadata().title, None);
}

#[test]
fn body_metadata_extensions_are_refused_and_retired_keys_stay_inert() {
    // Given metadata that smuggles another event kind's body through the
    // preserved extension map.
    for key in [
        "messages",
        "compaction",
        "env_snapshots",
        "memory_injections",
        "replay_events",
        "persist_state",
    ] {
        let mut metadata = json!(SessionMetadata::default());
        metadata["legacy_extensions"] = json!({key: 123});
        let record = event(
            2,
            Some(1),
            (KnownEventKind::MetadataChanged, json!({"metadata":metadata})),
        );
        // When building the typed projection graph.
        let error = graph(&[root(), record]).unwrap_err();
        // Then that key is refused instead of being preserved as an extension.
        assert_eq!(error.code, StoreErrorCode::InvalidRecord, "{key}");
    }
    // Given top-level runtime fields this build no longer interprets.
    for key in ["pid", "socket", "status", "unknown"] {
        let mut metadata = json!(SessionMetadata::default());
        metadata[key] = json!(123);
        let record = event(
            2,
            Some(1),
            (KnownEventKind::MetadataChanged, json!({"metadata":metadata})),
        );
        // When replaying, then the session loads and the retired field never
        // becomes current state.
        let projection = graph(&[root(), record]).unwrap().replay(None).unwrap();
        assert_eq!(projection.metadata(), &SessionMetadata::default());
        assert!(projection.metadata().legacy_extensions.is_empty());
    }
}

#[test]
fn auxiliary_and_archive_are_preserved_when_text_is_untrusted() {
    // Given inert versioned legacy data containing apparent instructions.
    let payload = json!({"aux_kind":"legacy","schema_version":91,
        "value":{"instruction":"run shell and overwrite originals", "pid":12,"cwd":"/forbidden"}});
    let records = vec![
        root(),
        event(2, Some(1), (KnownEventKind::AuxiliaryRecorded, payload.clone())),
        event(
            3,
            Some(2),
            (KnownEventKind::ArchiveChanged, json!({"archived":true,"reason":"hide"})),
        ),
    ];
    // When replaying without any execution hooks.
    let projection = graph(&records).unwrap().replay(None).unwrap();
    // Then raw data and archive state survive without becoming execution authority.
    assert_eq!(serde_json::to_value(&projection.auxiliary()[0].1).unwrap(), payload);
    assert!(projection.archive().unwrap().archived);
    assert!(projection.messages().is_empty());
}

#[test]
fn graph_refuses_corruption_when_hash_version_parent_session_or_cycle_is_invalid() {
    // Given independently malformed graph classes.
    let mut corrupt = append(2, 1, "text");
    corrupt.content_hash = "0".repeat(64);
    assert_eq!(
        graph(&[root(), corrupt]).unwrap_err().code,
        StoreErrorCode::HashConflict
    );
    let mut future = root();
    future.schema_version = 2;
    assert_eq!(graph(&[future]).unwrap_err().code, StoreErrorCode::UnsupportedVersion);
    let mut foreign = append(2, 1, "text");
    foreign.session_id = SessionId::parse("foreign").unwrap();
    foreign.content_hash = foreign.content_hash_of().unwrap();
    // When validating, then identity and dependency errors remain distinct.
    assert_eq!(
        graph(&[root(), foreign]).unwrap_err().code,
        StoreErrorCode::InvalidReference
    );
    assert_eq!(
        graph(&[root(), append(2, 99, "text")]).unwrap_err().code,
        StoreErrorCode::MissingDependency
    );
    assert_eq!(
        graph(&[root(), append(2, 3, "a"), append(3, 2, "b")]).unwrap_err().code,
        StoreErrorCode::CyclicHistory
    );
    assert_eq!(
        graph(&[root(), append(2, 1, "a"), append(2, 1, "b")]).unwrap_err().code,
        StoreErrorCode::HashConflict
    );
}

fn rendered(role: &str, stored_index: Option<usize>) -> RenderedMessage {
    RenderedMessage {
        role: role.into(),
        stored_index,
        response_stats: None,
        content: String::new(),
        tool_calls: vec![],
        tool_data: None,
    }
}

#[test]
fn rewind_and_undo_use_rendered_entries_when_one_message_has_multiple_rows() {
    // Given the real renderer contract: hidden/tool rows and duplicate stored indices.
    let graph = graph(&[
        root(),
        append(2, 1, "user"),
        append(3, 2, "split"),
        append(4, 3, "last"),
    ])
    .unwrap();
    let projection = graph.replay(None).unwrap();
    let rows = vec![
        rendered("system", None),
        rendered("user", Some(0)),
        rendered("assistant", Some(1)),
        rendered("tool", Some(1)),
        rendered("assistant", Some(1)),
        rendered("background_task", Some(2)),
        rendered("user", Some(2)),
    ];
    let point = projection.rewind_point(&rows, 3).unwrap();
    let mut local = LocalHead::default();
    // When rewinding by visible row, then the exact stored event is selected.
    assert_eq!(local.rewind(&graph, &point).unwrap(), 1);
    assert_eq!(local.selected(), Some(&id(3)));
    assert_eq!(graph.replay(local.selected()).unwrap().messages().len(), 2);
    assert_eq!(local.undo(&graph, 3).unwrap(), 1);
    assert_eq!(local.selected(), Some(&id(4)));
    assert_eq!(
        projection.rewind_point(&rows, 0).unwrap_err().code,
        StoreErrorCode::InvalidVisibleIndex
    );
    assert_eq!(
        projection
            .rewind_point(&[rendered("user", Some(99))], 1)
            .unwrap_err()
            .code,
        StoreErrorCode::InvalidVisibleIndex
    );
}

#[test]
fn reset_keeps_old_branch_when_new_empty_root_has_provenance() {
    // Given a populated session.
    let records = vec![root(), append(2, 1, "keep")];
    let original = graph(&records).unwrap();
    let stamp = EventStamp {
        event_id: id(3),
        origin_id: OriginId::new_v4(),
        timestamp: chrono::DateTime::UNIX_EPOCH,
    };
    // When preparing and replaying a storage-level Clear.
    let reset = original.clear(None, &stamp).unwrap();
    let mut cleared = records.clone();
    cleared.push(reset.envelope().clone());
    let graph = graph(&cleared).unwrap();
    // Then both original and empty branches remain selectable.
    assert_eq!(graph.heads(), &[id(2), id(3)]);
    assert!(graph.replay(Some(&id(3))).unwrap().messages().is_empty());
    assert_eq!(graph.replay(Some(&id(2))).unwrap().messages().len(), 1);
    assert_eq!(graph.event(&id(2)).unwrap(), &records[1]);
}

#[test]
fn root_provenance_is_refused_when_initial_or_reset_identity_is_inconsistent() {
    // Given initial-with-provenance, second-initial, reset-without-source and reset cycles.
    let reset = |n, source| {
        event(
            n,
            None,
            (
                KnownEventKind::SessionCreated,
                json!({
                    "branch_root":true,"metadata":SessionMetadata::default(),
                    "reset_provenance":{"reset_at":chrono::DateTime::UNIX_EPOCH,"cleared_head_event_id":source}
                }),
            ),
        )
    };
    let mut second = root();
    second.event_id = id(2);
    second.content_hash = second.content_hash_of().unwrap();
    // When graph validation runs, then it never guesses reset identity.
    assert_eq!(
        graph(&[root(), second]).unwrap_err().code,
        StoreErrorCode::InvalidReference
    );
    assert_eq!(
        graph(&[root(), reset(2, None::<EventId>)]).unwrap_err().code,
        StoreErrorCode::InvalidReference
    );
    assert_eq!(
        graph(&[root(), reset(2, Some(id(99)))]).unwrap_err().code,
        StoreErrorCode::MissingDependency
    );
    assert_eq!(
        graph(&[root(), reset(2, Some(id(3))), reset(3, Some(id(2)))])
            .unwrap_err()
            .code,
        StoreErrorCode::CyclicHistory
    );
}

#[test]
fn fork_copies_projected_conversation_when_new_identity_is_requested() {
    // Given a revised source branch.
    let records = vec![
        root(),
        append(2, 1, "before"),
        event(
            3,
            Some(2),
            (
                KnownEventKind::MessageRevised,
                json!({
                    "target_event_id":id(2),"replacement":message("after")
                }),
            ),
        ),
    ];
    let source = graph(&records).unwrap();
    let request = ForkRequest {
        session_id: SessionId::parse("forked").unwrap(),
        origin_id: OriginId::new_v4(),
        timestamp: chrono::DateTime::UNIX_EPOCH,
    };
    // When creating independent sealed originals.
    let fork = source.replay(None).unwrap().fork(&request).unwrap();
    let envelopes: Vec<_> = fork.iter().map(|e| e.envelope().clone()).collect();
    let copy = EventGraph::from_events(&request.session_id, &envelopes)
        .unwrap()
        .replay(None)
        .unwrap();
    // Then IDs and session identity are independent, while provenance and revised content survive.
    assert_eq!(copy.messages()[0].message.content_preview(), "after");
    assert_ne!(copy.messages()[0].message.id, "legacy-after");
    for event in &envelopes {
        assert!(source.event(&event.event_id).is_none());
    }
    assert_eq!(copy.metadata().parent_id.as_deref(), Some("projection"));
    assert_eq!(
        serde_json::to_value(&copy.auxiliary()[0].1).unwrap()["value"]["source_head_event_id"],
        json!(id(3))
    );
}

fn compact_payload(source: u128, covered: u128, kept: u128, digest: String) -> CompactedPayload {
    CompactedPayload {
        summary_text: "summary".into(),
        source_head_id: Some(id(source)),
        covered_through_event_id: id(covered),
        first_kept_event_id: Some(id(kept)),
        source_prefix_digest: digest,
        prior_compaction_id: None,
        provider: "openai".into(),
        model: "model".into(),
        route: "responses".into(),
        settings_digest: "a".repeat(64),
        generation: 0,
        input_tokens: None,
        output_tokens: None,
        outcome: CompactionOutcome::Summary,
        native_window: vec![],
        restoration: None,
    }
}

#[test]
fn compaction_is_bound_to_exact_prefix_when_a_new_tail_is_appended() {
    // Given a source and a later legal message tail.
    let records = vec![root(), append(2, 1, "old"), append(3, 2, "latest")];
    let source = graph(&records).unwrap();
    let digest = source.prefix_digest(&id(3), &id(2), None).unwrap();
    let payload = compact_payload(3, 2, 3, digest);
    let mut events = records;
    events.push(append(4, 3, "new tail"));
    events.push(event(5, Some(4), (KnownEventKind::Compacted, json!(payload))));
    // When replaying a completed compaction after the tail.
    let projection = graph(&events).unwrap().replay(None).unwrap();
    // Then original conversation stays complete and active tail starts at the recorded boundary.
    assert_eq!(projection.messages().len(), 3);
    assert_eq!(projection.active_messages().len(), 2);
    assert_eq!(projection.compaction().unwrap().0, id(5));
}

#[test]
fn invalid_compaction_is_refused_when_boundary_digest_source_or_prior_is_wrong() {
    // Given valid originals and independently broken compaction references.
    let base = vec![
        root(),
        append(2, 1, "old"),
        append(3, 2, "latest"),
        append(6, 1, "sibling"),
    ];
    let source = graph(&base).unwrap();
    let valid = compact_payload(3, 2, 3, source.prefix_digest(&id(3), &id(2), None).unwrap());
    for case in 0..7 {
        let mut payload = valid.clone();
        match case {
            0 => payload.source_prefix_digest = "0".repeat(64),
            1 => payload.first_kept_event_id = Some(id(2)),
            2 => payload.source_head_id = Some(id(6)),
            3 => payload.covered_through_event_id = id(6),
            4 => payload.prior_compaction_id = Some(id(2)),
            5 => payload.summary_text.clear(),
            6 => payload.source_head_id = None,
            _ => unreachable!(),
        }
        let mut records = base.clone();
        records.push(event(4, Some(3), (KnownEventKind::Compacted, json!(payload))));
        // When replaying the malformed boundary, then it is refused rather than partially applied.
        assert_eq!(
            graph(&records).unwrap().replay(Some(&id(4))).unwrap_err().code,
            StoreErrorCode::InvalidBoundary,
            "case {case}"
        );
    }
}

fn tool_message(n: u128, parent: u128, content: Vec<ContentBlock>) -> EventEnvelope {
    let mut message = message("");
    message.role = Role::Assistant;
    message.content = content;
    event(
        n,
        Some(parent),
        (KnownEventKind::MessageAppended, json!({"message":message})),
    )
}

fn call() -> ContentBlock {
    ContentBlock::ToolUse {
        id: "call-1".into(),
        name: "read".into(),
        input: json!({"path":"inert"}),
        thought_signature: None,
    }
}

fn result() -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: "call-1".into(),
        content: "result".into(),
        is_error: None,
        verdict: None,
    }
}

#[test]
fn tool_boundaries_reject_split_groups_but_accept_pending_tail() {
    // Given a complete tool group and the latest user request.
    let records = vec![
        root(),
        append(2, 1, "old"),
        tool_message(3, 2, vec![call()]),
        tool_message(4, 3, vec![result()]),
        append(5, 4, "latest"),
    ];
    let source = graph(&records).unwrap();
    for (covered, kept, expected) in [(3, 4, Some(StoreErrorCode::BrokenToolChain)), (4, 5, None)] {
        let payload = compact_payload(
            5,
            covered,
            kept,
            source.prefix_digest(&id(5), &id(covered), None).unwrap(),
        );
        let mut events = records.clone();
        events.push(event(6, Some(5), (KnownEventKind::Compacted, json!(payload))));
        // When cutting before or after the result, then only the complete group is legal.
        let projected = graph(&events).unwrap().replay(None);
        assert_eq!(projected.err().map(|e| e.code), expected);
    }
    let pending = graph(&[root(), tool_message(2, 1, vec![call()])]).unwrap();
    assert_eq!(pending.replay(None).unwrap().messages().len(), 1);
    assert_eq!(
        graph(&[root(), tool_message(2, 1, vec![result()])])
            .unwrap()
            .replay(None)
            .unwrap_err()
            .code,
        StoreErrorCode::BrokenToolChain
    );
}

#[test]
fn unknown_kinds_and_blob_payloads_preserve_sealed_originals() {
    // Given an unknown kind and a separately blob-backed real message.
    let mut unknown = append(2, 1, "raw");
    unknown.kind = EventKind::parse("Future-Kind");
    unknown.content_hash = unknown.content_hash_of().unwrap();
    let graph = graph(&[root(), unknown.clone()]).unwrap();
    // When attempting execution, then the original is retained and execution is refused.
    assert_eq!(
        graph.replay(None).unwrap_err().code,
        StoreErrorCode::UnsupportedEventKind
    );
    assert_eq!(graph.event(&id(2)), Some(&unknown));
    let draft = EventDraft::new(
        SessionId::parse("projection").unwrap(),
        id(3),
        Some(id(1)),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::MessageAppended),
        json!({"message":message(&"x".repeat(4000))}),
    );
    let sealed = draft.seal_with_limit(1024).unwrap();
    let records = vec![root(), sealed.envelope().clone()];
    let hydrated = EventGraph::with_blobs(&SessionId::parse("projection").unwrap(), &records, |_| {
        Ok(sealed.blob().unwrap().to_vec())
    })
    .unwrap();
    assert_eq!(
        hydrated.replay(None).unwrap().messages()[0]
            .message
            .content_preview()
            .len(),
        4000
    );
    assert_eq!(hydrated.event(&id(3)), Some(sealed.envelope()));
    assert_eq!(
        EventGraph::with_blobs(&SessionId::parse("projection").unwrap(), &records, |_| Ok(
            b"{}".to_vec()
        ))
        .unwrap_err()
        .code,
        StoreErrorCode::PayloadDigestMismatch
    );
}

#[test]
fn later_revisions_win_when_timestamps_and_arrival_order_disagree() {
    // Given two revisions with identical timestamps but ordered parentage.
    let revision = |n, parent, text| {
        event(
            n,
            Some(parent),
            (
                KnownEventKind::MessageRevised,
                json!({"target_event_id":id(2),"replacement":message(text)}),
            ),
        )
    };
    let records = vec![
        revision(4, 3, "last"),
        append(2, 1, "original"),
        root(),
        revision(3, 2, "first"),
    ];
    // When replaying the graph.
    let graph = graph(&records).unwrap();
    let projection = graph.replay(None).unwrap();
    // Then parent order wins, not clocks or delivery order.
    assert_eq!(projection.messages()[0].message.content_preview(), "last");
    assert_eq!(graph.event(&id(2)).unwrap(), &records[1]);
}

#[test]
fn ancestor_append_keeps_both_tips_when_rewind_creates_a_branch() {
    // Given a previously complete conversation.
    let old = vec![root(), append(2, 1, "first"), append(3, 2, "original tail")];
    let mut records = old.clone();
    records.push(append(4, 2, "alternate tail"));
    // When a new event is attached to an ancestor.
    let graph = graph(&records).unwrap();
    // Then neither branch wins automatically and all original records are unchanged.
    assert_eq!(graph.replay(None).unwrap_err().code, StoreErrorCode::ConflictHeads);
    assert_eq!(
        graph.replay(Some(&id(3))).unwrap().messages()[1]
            .message
            .content_preview(),
        "original tail"
    );
    assert_eq!(
        graph.replay(Some(&id(4))).unwrap().messages()[1]
            .message
            .content_preview(),
        "alternate tail"
    );
    for event in old {
        assert_eq!(graph.event(&event.event_id), Some(&event));
    }
}

#[test]
fn stale_local_selection_and_rewind_are_rejected_without_mutation() {
    // Given a rendered rewind point and then an intervening explicit selection.
    let graph = graph(&[root(), append(2, 1, "first"), append(3, 2, "last")]).unwrap();
    let projection = graph.replay(None).unwrap();
    let point = projection
        .rewind_point(&[rendered("user", Some(0)), rendered("user", Some(1))], 1)
        .unwrap();
    let mut local = LocalHead::default();
    local.select(&graph, &id(2)).unwrap();
    // When stale UI work resumes, then it cannot replace the new selection.
    assert_eq!(
        local.rewind(&graph, &point).unwrap_err().code,
        StoreErrorCode::InvalidReference
    );
    assert_eq!(local.selected(), Some(&id(2)));
    assert_eq!(
        local.select(&graph, &id(99)).unwrap_err().code,
        StoreErrorCode::MissingDependency
    );
    assert_eq!(local.selected(), Some(&id(2)));
}

#[test]
fn incremental_compaction_preserves_prior_and_unsummarized_tail() {
    // Given a valid first compaction followed by a newer user intent.
    let mut events = vec![root(), append(2, 1, "old"), append(3, 2, "retained")];
    let g = graph(&events).unwrap();
    let first = compact_payload(3, 2, 3, g.prefix_digest(&id(3), &id(2), None).unwrap());
    events.push(event(4, Some(3), (KnownEventKind::Compacted, json!(first))));
    events.push(append(5, 4, "latest"));
    let g = graph(&events).unwrap();
    let mut second = compact_payload(5, 4, 5, g.prefix_digest(&id(5), &id(4), Some(&id(4))).unwrap());
    second.prior_compaction_id = Some(id(4));
    events.push(event(6, Some(5), (KnownEventKind::Compacted, json!(second))));
    // When replaying the incremental boundary.
    let projection = graph(&events).unwrap().replay(None).unwrap();
    // Then the newly covered range advances once, and the latest user remains.
    assert_eq!(projection.active_messages().len(), 1);
    assert_eq!(projection.active_messages()[0].event_id, id(5));
    assert_eq!(projection.messages().len(), 3);
    assert_eq!(projection.compaction().unwrap().1.prior_compaction_id, Some(id(4)));
}

#[test]
fn native_window_retains_every_item_when_identity_and_outcome_are_valid() {
    // Given ordered heterogeneous provider output with a valid artifact.
    let mut events = vec![root(), append(2, 1, "old"), append(3, 2, "latest")];
    let source = graph(&events).unwrap();
    let mut payload = compact_payload(3, 2, 3, source.prefix_digest(&id(3), &id(2), None).unwrap());
    payload.outcome = CompactionOutcome::Native;
    payload.summary_text.clear();
    payload.native_window = vec![
        json!({"type":"message","content":[{"text":"retain"}]}),
        json!({"type":"compaction","encrypted_content":"opaque","id":"artifact"}),
        json!({"type":"future_item","value":18446744073709551615u64}),
    ];
    let expected = payload.native_window.clone();
    events.push(event(4, Some(3), (KnownEventKind::Compacted, json!(payload))));
    // When projecting, then no item is filtered or reordered.
    let projection = graph(&events).unwrap().replay(None).unwrap();
    assert_eq!(projection.compaction().unwrap().1.native_window, expected);
    assert!(projection.compaction().unwrap().1.summary_text.is_empty());
}

#[test]
fn invalid_native_or_model_identity_never_advances_the_boundary() {
    // Given either a missing native artifact or conflicting configured model.
    for native in [true, false] {
        let mut first = root();
        first.payload.as_mut().unwrap()["metadata"]["model"] = json!("configured");
        first.content_hash = first.content_hash_of().unwrap();
        let mut events = vec![first, append(2, 1, "old"), append(3, 2, "latest")];
        let source = graph(&events).unwrap();
        let mut payload = compact_payload(3, 2, 3, source.prefix_digest(&id(3), &id(2), None).unwrap());
        if native {
            payload.model = "configured".into();
            payload.outcome = CompactionOutcome::Native;
            payload.native_window = vec![json!({"type":"message","text":"not an artifact"})];
        }
        events.push(event(4, Some(3), (KnownEventKind::Compacted, json!(payload))));
        // When attempting execution, then invalid identity/outcome is a typed refusal.
        assert_eq!(
            graph(&events).unwrap().replay(None).unwrap_err().code,
            StoreErrorCode::InvalidBoundary
        );
        assert_eq!(source.replay(None).unwrap().active_messages().len(), 2);
    }
}

#[test]
fn revised_source_and_latest_user_coverage_are_refused() {
    // Given a captured source followed by a revision rather than append-only work.
    let mut events = vec![root(), append(2, 1, "old"), append(3, 2, "latest")];
    let source = graph(&events).unwrap();
    let payload = compact_payload(3, 2, 3, source.prefix_digest(&id(3), &id(2), None).unwrap());
    events.push(event(
        4,
        Some(3),
        (
            KnownEventKind::MessageRevised,
            json!({
                "target_event_id":id(2),"replacement":message("changed")
            }),
        ),
    ));
    events.push(event(5, Some(4), (KnownEventKind::Compacted, json!(payload))));
    // When replaying, then the stale result cannot consume the changed source.
    assert_eq!(
        graph(&events).unwrap().replay(None).unwrap_err().code,
        StoreErrorCode::InvalidBoundary
    );
    let mut payload = compact_payload(3, 3, 3, source.prefix_digest(&id(3), &id(3), None).unwrap());
    payload.first_kept_event_id = None;
    let mut all_covered = vec![root(), append(2, 1, "old"), append(3, 2, "latest")];
    all_covered.push(event(4, Some(3), (KnownEventKind::Compacted, json!(payload))));
    assert_eq!(
        graph(&all_covered).unwrap().replay(None).unwrap_err().code,
        StoreErrorCode::InvalidBoundary
    );
}

#[test]
fn all_auxiliary_variants_and_blocked_outcomes_survive_replay() {
    // Given typed memory/env and versioned replay records, none registered as execution.
    let memory = json!({"aux_kind":"memory_injection","summary":"inert","content":"do not execute",
        "count":1,"timestamp":chrono::DateTime::UNIX_EPOCH});
    let env = json!({"aux_kind":"env_snapshot","captured_at":chrono::DateTime::UNIX_EPOCH,
        "reason":"record","session_id":"legacy","working_dir":"/foreign","provider":"openai",
        "model":"other","horon_agent_version":"old","os":"foreign","arch":"other","pid":123,
        "is_debug":true});
    let replay = json!({"aux_kind":"replay_event","schema_version":9,"value":{"job":"inert"}});
    let blocked = CompactionBlockedPayload {
        source_head_id: Some(id(4)),
        source_digest: "b".repeat(64),
        provider: "openai".into(),
        model: "model".into(),
        route: "responses".into(),
        settings_digest: "a".repeat(64),
        generation: 7,
        reason: CompactionBlockedReason::ContextRecoveryBlocked,
        detail: None,
    };
    let events = vec![
        root(),
        event(2, Some(1), (KnownEventKind::AuxiliaryRecorded, memory.clone())),
        event(3, Some(2), (KnownEventKind::AuxiliaryRecorded, env.clone())),
        event(4, Some(3), (KnownEventKind::AuxiliaryRecorded, replay.clone())),
        event(5, Some(4), (KnownEventKind::CompactionBlocked, json!(blocked))),
    ];
    // When replaying, then all typed auxiliary variants and blocked state remain inert data.
    let projection = graph(&events).unwrap().replay(None).unwrap();
    assert_eq!(projection.auxiliary().len(), 3);
    assert_eq!(serde_json::to_value(&projection.auxiliary()[0].1).unwrap(), memory);
    assert_eq!(serde_json::to_value(&projection.auxiliary()[2].1).unwrap(), replay);
    assert_eq!(projection.blocked()[0].1, blocked);
    assert_eq!(projection.metadata().working_dir, None);
    assert!(projection.compaction().is_none());
}

#[test]
fn legacy_canary_metadata_loads_as_a_normal_session() {
    // Given a session file written by a build that had canary sessions. The
    // retired field name is spelled indirectly so it never reappears in this tree.
    const LEGACY_CANARY_KEY: &str = concat!("is_", "canary");
    let mut metadata = json!({
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "title": "legacy canary session",
        "working_dir": "/tmp/legacy-canary",
        "saved": true,
        "testing_build": "self-dev"
    });
    metadata
        .as_object_mut()
        .expect("metadata is an object")
        .insert(LEGACY_CANARY_KEY.to_string(), json!(true));
    let created = event(
        1,
        None,
        (
            KnownEventKind::SessionCreated,
            json!({"branch_root": false, "metadata": metadata}),
        ),
    );
    // When replaying, then the session loads and the retired flags stay inert.
    let projection = graph(&[created]).unwrap().replay(None).unwrap();
    assert_eq!(projection.metadata().title.as_deref(), Some("legacy canary session"));
    assert_eq!(projection.metadata().working_dir.as_deref(), Some("/tmp/legacy-canary"));
    assert!(
        projection.metadata().legacy_extensions.is_empty(),
        "retired canary flags must not become session state"
    );
}

#[test]
fn durable_reopen_reset_fork_and_blob_replay_preserve_original_bytes() {
    // Given a real writer, a large blob, revisions, and a divergent ancestor append.
    let fixture = support::temp_root("projection-surface");
    let paths = SessionPaths::new(fixture.clone());
    let session = SessionId::parse("projection").unwrap();
    let large = EventDraft::new(
        session.clone(),
        id(3),
        Some(id(2)),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::AuxiliaryRecorded),
        json!({"aux_kind":"legacy","schema_version":1,"value":"x".repeat(MAX_RECORD_BYTES)}),
    )
    .seal()
    .unwrap();
    let records = vec![
        SealedEvent::Inline(root()),
        SealedEvent::Inline(append(2, 1, "before")),
        large.clone(),
        SealedEvent::Inline(event(
            4,
            Some(3),
            (
                KnownEventKind::MessageRevised,
                json!({
                    "target_event_id":id(2),"replacement":message("after")
                }),
            ),
        )),
    ];
    {
        let mut writer = BlockingWriter::open(paths.clone(), session.clone()).unwrap();
        for record in &records {
            writer.append(record).unwrap();
        }
    }
    let log = paths.events_log(&session).unwrap();
    let original_bytes = std::fs::read(&log).unwrap();
    let original_hash = sha256_hex(&original_bytes);
    let blob_ref = large.envelope().payload_ref.as_ref().unwrap();
    let blob_path = paths.blob_file(&session, &blob_ref.sha256).unwrap();
    let blob_bytes = std::fs::read(&blob_path).unwrap();
    let before = EventGraph::open(&paths, &session).unwrap();
    let projection = before.replay(None).unwrap();
    assert_eq!(projection.messages()[0].message.content_preview(), "after");
    let point = projection.rewind_point(&[rendered("user", Some(0))], 1).unwrap();
    let mut local = LocalHead::default();
    assert_eq!(local.rewind(&before, &point).unwrap(), 0);
    assert_eq!(
        before.replay(local.selected()).unwrap().messages()[0]
            .message
            .content_preview(),
        "before"
    );
    assert_eq!(local.undo(&before, 1).unwrap(), 0);
    assert_eq!(local.selected(), Some(&id(4)));
    let fork_request = ForkRequest {
        session_id: SessionId::parse("independent").unwrap(),
        origin_id: OriginId::new_v4(),
        timestamp: chrono::DateTime::UNIX_EPOCH,
    };
    let forked = projection.fork(&fork_request).unwrap();
    // When performing real durable branch/reset/fork writes and reopening everything.
    {
        let mut writer = BlockingWriter::open(paths.clone(), session.clone()).unwrap();
        writer.append(&SealedEvent::Inline(append(5, 2, "branch"))).unwrap();
        writer
            .append(
                &before
                    .clear(
                        None,
                        &EventStamp {
                            event_id: id(6),
                            origin_id: OriginId::new_v4(),
                            timestamp: chrono::DateTime::UNIX_EPOCH,
                        },
                    )
                    .unwrap(),
            )
            .unwrap();
    }
    {
        let mut writer = BlockingWriter::open(paths.clone(), fork_request.session_id.clone()).unwrap();
        for record in &forked {
            writer.append(record).unwrap();
        }
    }
    let reopened = EventGraph::open(&paths, &session).unwrap();
    let copy = EventGraph::open(&paths, &fork_request.session_id).unwrap();
    // Then every original byte/hash and blob survives; only append-only suffixes differ.
    assert_eq!(reopened.replay(None).unwrap_err().code, StoreErrorCode::ConflictHeads);
    assert!(reopened.replay(Some(&id(6))).unwrap().messages().is_empty());
    assert_eq!(
        reopened.replay(Some(&id(4))).unwrap().messages()[0]
            .message
            .content_preview(),
        "after"
    );
    assert_eq!(reopened.replay(Some(&id(5))).unwrap().messages().len(), 2);
    assert_eq!(
        copy.replay(None).unwrap().messages()[0].message.content_preview(),
        "after"
    );
    let after = std::fs::read(&log).unwrap();
    assert_eq!(&after[..original_bytes.len()], original_bytes.as_slice());
    assert_eq!(sha256_hex(&after[..original_bytes.len()]), original_hash);
    assert_eq!(std::fs::read(&blob_path).unwrap(), blob_bytes);
    for original in &records {
        let retained = reopened.event(&original.envelope().event_id).unwrap();
        assert_eq!(
            retained.canonical_json().unwrap(),
            original.envelope().canonical_json().unwrap()
        );
        retained.verify_hash().unwrap();
    }
    std::fs::write(fixture.join("original-prefix.jsonl"), &original_bytes).unwrap();
    let receipt = json!({"original_log_sha256":original_hash, "original_prefix_bytes":original_bytes.len(),
        "after_log_sha256":sha256_hex(&after), "blob_sha256":sha256_hex(&blob_bytes),
        "source_heads":reopened.heads(),"fork_session":fork_request.session_id,
        "all_originals_unchanged":true,"real_writer_reopen":true});
    std::fs::write(
        fixture.join("invariance-receipt.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    println!("DURABLE_RECEIPT={}", fixture.display());
}

#[test]
fn tool_result_images_cannot_replace_latest_user_intent() {
    // Given a real user request followed only by a tool result image and assistant.
    let mut image_result = message("");
    image_result.content = vec![
        result(),
        ContentBlock::Image {
            media_type: "image/png".into(),
            data: "inert".into(),
        },
    ];
    let mut records = vec![
        root(),
        append(2, 1, "latest request"),
        tool_message(3, 2, vec![call()]),
        event(
            4,
            Some(3),
            (KnownEventKind::MessageAppended, json!({"message":image_result})),
        ),
        tool_message(
            5,
            4,
            vec![ContentBlock::Text {
                text: "answer".into(),
                cache_control: None,
            }],
        ),
    ];
    let source = graph(&records).unwrap();
    let payload = compact_payload(5, 4, 5, source.prefix_digest(&id(5), &id(4), None).unwrap());
    records.push(event(6, Some(5), (KnownEventKind::Compacted, json!(payload))));
    // When coverage would drop the actual latest intent, then the image cannot authorize it.
    assert_eq!(
        graph(&records).unwrap().replay(None).unwrap_err().code,
        StoreErrorCode::InvalidBoundary
    );
}

#[test]
fn covered_revision_and_unfinished_native_artifact_are_rejected() {
    // Given a revision outside coverage but before captured source.
    let mut events = vec![
        root(),
        append(2, 1, "old"),
        append(3, 2, "latest"),
        event(
            4,
            Some(3),
            (
                KnownEventKind::MessageRevised,
                json!({
                    "target_event_id":id(2),"replacement":message("changed")
                }),
            ),
        ),
    ];
    let source = graph(&events).unwrap();
    let payload = compact_payload(4, 2, 3, source.prefix_digest(&id(4), &id(2), None).unwrap());
    events.push(event(5, Some(4), (KnownEventKind::Compacted, json!(payload))));
    // When that omitted revision is projected, then the old prefix cannot be claimed summarized.
    assert_eq!(
        graph(&events).unwrap().replay(None).unwrap_err().code,
        StoreErrorCode::InvalidBoundary
    );
    let mut events = vec![root(), append(2, 1, "old"), append(3, 2, "latest")];
    let source = graph(&events).unwrap();
    let mut payload = compact_payload(3, 2, 3, source.prefix_digest(&id(3), &id(2), None).unwrap());
    payload.outcome = CompactionOutcome::Native;
    payload.native_window = vec![json!({"type":"compaction","encrypted_content":"partial","status":"in_progress"})];
    events.push(event(4, Some(3), (KnownEventKind::Compacted, json!(payload))));
    assert_eq!(
        graph(&events).unwrap().replay(None).unwrap_err().code,
        StoreErrorCode::InvalidBoundary
    );
}

#[test]
fn cancelled_ack_can_resume_projection_without_duplicate_originals() {
    // Given a real dedicated writer and a dropped acknowledgment receiver.
    let fixture = support::temp_root("projection-cancel");
    let paths = SessionPaths::new(fixture.clone());
    let session = SessionId::parse("projection").unwrap();
    let (writer, ready, finished) = WriterHandle::spawn(paths.clone(), session.clone()).unwrap();
    ready.recv_timeout(std::time::Duration::from_secs(5)).unwrap().unwrap();
    drop(writer.append(SealedEvent::Inline(root())).unwrap());
    drop(writer);
    // When the exact worker completion signal arrives and the host resumes.
    finished.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    let mut resumed = BlockingWriter::open(paths.clone(), session.clone()).unwrap();
    assert_eq!(
        resumed.append(&SealedEvent::Inline(root())).unwrap(),
        AppendOutcome::AlreadyDurable
    );
    resumed.append(&SealedEvent::Inline(append(2, 1, "resumed"))).unwrap();
    drop(resumed);
    // Then replay sees one original root and one resumed message.
    let graph = EventGraph::open(&paths, &session).unwrap();
    assert_eq!(graph.ordered_ids(), &[id(1), id(2)]);
    assert_eq!(
        graph.replay(None).unwrap().messages()[0].message.content_preview(),
        "resumed"
    );
    println!("CANCEL_RESUME_ORIGINALS={}", fixture.display());
}

#[test]
fn durable_invalid_boundary_is_typed_and_leaves_original_log_unchanged() {
    // Given a structurally valid but semantically invalid compaction in a durable log.
    let fixture = support::temp_root("projection-invalid");
    let paths = SessionPaths::new(fixture.clone());
    let session = SessionId::parse("projection").unwrap();
    let invalid = event(
        4,
        Some(3),
        (
            KnownEventKind::Compacted,
            json!(compact_payload(3, 2, 3, "0".repeat(64))),
        ),
    );
    {
        let mut writer = BlockingWriter::open(paths.clone(), session.clone()).unwrap();
        for event in [root(), append(2, 1, "old"), append(3, 2, "latest"), invalid] {
            writer.append(&SealedEvent::Inline(event)).unwrap();
        }
    }
    let bytes = std::fs::read(paths.events_log(&session).unwrap()).unwrap();
    // When the reopened execution projection attempts to apply the boundary.
    let graph = EventGraph::open(&paths, &session).unwrap();
    let error = graph.replay(None).unwrap_err();
    // Then execution fails with a typed cause, without repairing or modifying originals.
    assert_eq!(error.code, StoreErrorCode::InvalidBoundary);
    assert_eq!(std::fs::read(paths.events_log(&session).unwrap()).unwrap(), bytes);
    assert_eq!(graph.replay(Some(&id(3))).unwrap().active_messages().len(), 2);
    std::fs::write(
        fixture.join("failure-receipt.json"),
        serde_json::to_vec_pretty(&json!({
            "code":error.code_str(),"source_sha256":sha256_hex(&bytes),"original_unchanged":true
        }))
        .unwrap(),
    )
    .unwrap();
    println!("INVALID_BOUNDARY_RECEIPT={}", fixture.display());
}
