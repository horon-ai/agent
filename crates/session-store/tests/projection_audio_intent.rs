//! Latest-user-intent coverage for audio prompts and blocks the mirror does not model.
//!
//! `validate_tools` decides which projected message is the latest user intent;
//! compaction coverage may not drop that message. These cases drive the public
//! replay surface so the check is observed, not asserted on internals.
use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use serde_json::{Value, json};

fn id(n: u128) -> EventId {
    EventId::parse(uuid::Uuid::from_u128(n).to_string()).unwrap()
}

fn event(n: u128, parent: Option<u128>, body: (KnownEventKind, Value)) -> EventEnvelope {
    EventDraft::new(
        SessionId::parse("projection-audio").unwrap(),
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
            json!({"metadata": SessionMetadata::default(), "branch_root": false}),
        ),
    )
}

fn user_message(content: Vec<ContentBlock>) -> StoredMessage {
    StoredMessage {
        original_input: None,
        arg_repairs: None,
        degradations: None,
        turn_start: false,
        id: "legacy-user".into(),
        role: Role::User,
        content,
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }
}

fn append_message(n: u128, parent: u128, message: StoredMessage) -> EventEnvelope {
    event(
        n,
        Some(parent),
        (KnownEventKind::MessageAppended, json!({"message": message})),
    )
}

fn append_text(n: u128, parent: u128, text: &str, role: Role) -> EventEnvelope {
    let mut message = user_message(vec![ContentBlock::Text {
        text: text.into(),
        cache_control: None,
    }]);
    message.role = role;
    append_message(n, parent, message)
}

fn graph(events: &[EventEnvelope]) -> Result<EventGraph, StoreError> {
    EventGraph::from_events(&SessionId::parse("projection-audio").unwrap(), events)
}

fn compact_payload(source: u128, covered: u128, kept: Option<u128>, digest: String) -> CompactedPayload {
    CompactedPayload {
        summary_text: "summary".into(),
        source_head_id: Some(id(source)),
        covered_through_event_id: id(covered),
        first_kept_event_id: kept.map(id),
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
fn audio_only_user_prompt_is_the_latest_user_intent() {
    // Given a user turn whose only content is recorded audio, then an assistant answer.
    let records = vec![
        root(),
        append_message(
            2,
            1,
            user_message(vec![ContentBlock::Audio {
                format: "wav".into(),
                data: "AAAA".into(),
            }]),
        ),
        append_text(3, 2, "answer", Role::Assistant),
    ];
    let source = graph(&records).unwrap();
    let audio_digest = source.prefix_digest(&id(2), &id(1), None).unwrap();
    let audio_kept_digest = source.prefix_digest(&id(3), &id(2), None).unwrap();
    // When coverage ends at the audio prompt while keeping only the answer.
    let mut dropped = records.clone();
    dropped.push(event(
        4,
        Some(3),
        (
            KnownEventKind::Compacted,
            json!(compact_payload(3, 2, Some(3), audio_kept_digest)),
        ),
    ));
    // Then the audio prompt is the latest intent, so dropping it is refused.
    assert_eq!(
        graph(&dropped).unwrap().replay(None).unwrap_err().code,
        StoreErrorCode::InvalidBoundary
    );
    // When coverage stops before the audio prompt.
    let mut kept = records;
    kept.push(event(
        4,
        Some(3),
        (
            KnownEventKind::Compacted,
            json!(compact_payload(3, 1, Some(2), audio_digest)),
        ),
    ));
    // Then the boundary is legal and both messages survive as originals.
    let projection = graph(&kept).unwrap().replay(None).unwrap();
    assert_eq!(projection.messages().len(), 2);
    assert_eq!(projection.active_messages().len(), 2);
}

#[test]
fn unmodelled_block_type_is_still_inert_and_never_the_latest_intent() {
    // Given a user turn carrying only a block the mirror does not model.
    let unknown = user_message(vec![ContentBlock::ProviderNative {
        provider: "openai".into(),
        item: json!({"type": "future_item"}),
    }]);
    let mut inert = vec![root(), append_message(2, 1, unknown.clone())];
    let inert_source = graph(&inert).unwrap();
    let inert_digest = inert_source.prefix_digest(&id(2), &id(2), None).unwrap();
    inert.push(event(
        3,
        Some(2),
        (
            KnownEventKind::Compacted,
            json!(compact_payload(2, 2, None, inert_digest)),
        ),
    ));
    // When full coverage drops it, then replay succeeds because it was never intent.
    let projection = graph(&inert).unwrap().replay(None).unwrap();
    assert_eq!(projection.messages().len(), 1);
    // Given the same shape with a text prompt the mirror does model.
    let mut text = vec![root(), append_text(2, 1, "hi", Role::User)];
    let text_source = graph(&text).unwrap();
    let text_digest = text_source.prefix_digest(&id(2), &id(2), None).unwrap();
    text.push(event(
        3,
        Some(2),
        (
            KnownEventKind::Compacted,
            json!(compact_payload(2, 2, None, text_digest)),
        ),
    ));
    // Then the identical coverage is refused, so the difference is the block type.
    assert_eq!(
        graph(&text).unwrap().replay(None).unwrap_err().code,
        StoreErrorCode::InvalidBoundary
    );
}
