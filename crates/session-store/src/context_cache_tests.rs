//! The cold inline suffix must equal the one incremental publication keeps.
//!
//! `CachedContext::push` publishes one message at a time and drops from the
//! front while the canonical array is over budget. A cold rebuild finds that
//! same suffix by walking back from the newest message once, so these cases pin
//! the equivalence at the exact budget edges, keep the complete index
//! validation, and replay a real log through the writer before reading it cold.

use super::{CachedContext, inline_suffix};
use crate::cache_values::CachedValue;
use crate::checkpoint::SessionCache;
use crate::index::{ActiveMessage, Index, Location, RecordRef};
use crate::{
    BlockingWriter, EventDraft, EventId, EventKind, INLINE_TAIL_BYTES, KnownEventKind, OriginId, PayloadEncoding,
    PayloadRef, ProjectedMessage, ReadProbe, SessionId, SessionMetadata, SessionPaths, StoreErrorCode,
};
use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_types::StoredMessage;
use std::path::PathBuf;

fn stored(text: &str) -> StoredMessage {
    StoredMessage {
        id: "message".into(),
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.into(),
            cache_control: None,
        }],
        ..StoredMessage::default()
    }
}

fn projected(text: &str) -> ProjectedMessage {
    ProjectedMessage {
        event_id: EventId::new_v4(),
        message: stored(text),
    }
}

fn message_json(message: &ProjectedMessage) -> Vec<u8> {
    serde_json::to_vec(message).unwrap()
}

fn tail_json(tail: &[ProjectedMessage]) -> Vec<u8> {
    serde_json::to_vec(tail).unwrap()
}

/// The canonical JSON length of one projected message.
fn projected_len(text: &str) -> usize {
    message_json(&projected(text)).len()
}

/// A message body whose projected JSON is exactly `bytes` long. ASCII adds one
/// byte per character and needs no escapes, so the target is exact.
fn text_of_projected_len(bytes: usize) -> String {
    let base = projected_len("");
    assert!(bytes >= base, "a message cannot shrink below its empty form");
    "a".repeat(bytes - base)
}

fn sized(bytes: usize) -> ProjectedMessage {
    projected(&text_of_projected_len(bytes))
}

/// One active append per message, in order; `blobbed` names the originals that
/// live in a blob instead of the record.
fn rows(messages: &[ProjectedMessage], blobbed: &[usize]) -> Index {
    let mut rows = Vec::with_capacity(messages.len());
    for (order, message) in messages.iter().enumerate() {
        let blob = if blobbed.contains(&order) {
            Some(PayloadRef {
                sha256: "0".repeat(64),
                size: 1,
                encoding: PayloadEncoding::CanonicalJson,
            })
        } else {
            None
        };
        let source = RecordRef {
            id: message.event_id.clone(),
            hash: format!("hash-{order}"),
            offset: order as u64,
            length: 1,
            blob,
        };
        rows.push(Location {
            id: message.event_id.clone(),
            hash: format!("hash-{order}"),
            offset: order as u64,
            length: 1,
            ordinal: order,
            sparse: false,
            message: true,
            blob: None,
            active: Some(ActiveMessage {
                order,
                source,
                revised: false,
            }),
        });
    }
    Index { rows }
}

fn incremental(active: &[ProjectedMessage], inline: &[bool]) -> (usize, Vec<ProjectedMessage>) {
    let mut context = CachedContext {
        session_id: SessionId::parse("cache").unwrap(),
        head: EventId::new_v4(),
        metadata: CachedValue::Inline(SessionMetadata::default()),
        archive: None,
        compaction: None,
        activity_at: chrono::DateTime::UNIX_EPOCH,
        first_prompt_title: None,
        active_count: 0,
        inline_from: 0,
        tail: Vec::new(),
    };
    for (message, &inline_original) in active.iter().zip(inline) {
        context.push(message, inline_original).unwrap();
    }
    (context.inline_from, context.tail)
}

fn assert_matches_incremental(case: Vec<(ProjectedMessage, bool)>) {
    let mut active = Vec::with_capacity(case.len());
    let mut inline = Vec::with_capacity(case.len());
    for (message, inline_original) in case {
        active.push(message);
        inline.push(inline_original);
    }
    let mut blobbed = Vec::new();
    for (order, inline_original) in inline.iter().enumerate() {
        if !*inline_original {
            blobbed.push(order);
        }
    }
    let index = rows(&active, &blobbed);
    let (expected_from, expected_tail) = incremental(&active, &inline);
    let start = inline_suffix(&active, &index).unwrap();
    assert_eq!(
        start, expected_from,
        "resident suffix starts at {start}, incremental publication kept {expected_from}"
    );
    assert_eq!(tail_json(&active[start..]), tail_json(&expected_tail));
}

#[test]
fn cold_suffix_matches_incremental_tail_at_budget_edges() {
    let budget = INLINE_TAIL_BYTES;
    // The empty and single-message cases exercise both ends of the suffix.
    assert_matches_incremental(Vec::new());
    assert_matches_incremental(vec![(projected("only message"), true)]);
    // One message whose array is exactly the budget is resident; one byte more
    // is not.
    assert_matches_incremental(vec![(sized(budget - 2), true)]);
    assert_matches_incremental(vec![(sized(budget - 1), true)]);
    // A message that cannot fit even alone starts an empty run.
    assert_matches_incremental(vec![(projected("kept"), true), (sized(budget + 1), true)]);
    // Two messages whose array is exactly the budget both stay; one byte more
    // drops only the oldest.
    let second = budget - 3 - 1000;
    assert_matches_incremental(vec![(sized(1000), true), (sized(second), true)]);
    assert_matches_incremental(vec![(sized(1000), true), (sized(second + 1), true)]);
    // A newest message that only just fits after the oldest is dropped leaves a
    // two-message run exactly at the budget, which is the revision that used to
    // re-serialize the whole tail.
    let third = budget - 3 - 15000;
    assert_matches_incremental(vec![(sized(1000), true), (sized(15000), true), (sized(third), true)]);
    // One byte more in that newest message drops both older ones.
    assert_matches_incremental(vec![
        (sized(1000), true),
        (sized(15000), true),
        (sized(third + 1), true),
    ]);
}

#[test]
fn cold_suffix_starts_a_fresh_run_at_a_blob_original() {
    assert_matches_incremental(vec![
        (projected("old"), true),
        (projected("blob resident"), false),
        (projected("newest"), true),
    ]);
    // A blob original as the newest message keeps nothing.
    assert_matches_incremental(vec![(projected("old"), true), (projected("blob resident"), false)]);
    // Small messages cross the budget several times over; the resident run is
    // the newest messages that fit, not the oldest.
    let many: Vec<_> = (0..64).map(|_| (sized(1000), true)).collect();
    assert_matches_incremental(many);
    let mut case: Vec<(ProjectedMessage, bool)> = (0..32).map(|_| (sized(900), true)).collect();
    case.push((projected("blob"), false));
    case.extend((0..32).map(|_| (sized(900), true)));
    assert_matches_incremental(case);
}

#[test]
fn cold_suffix_keeps_complete_active_index_validation() {
    let messages = vec![projected("released prefix"), projected("resident")];
    // An active message with no index row is refused even when it is far from
    // the resident suffix.
    let mut index = rows(&messages, &[]);
    index.rows.remove(0);
    let error = inline_suffix(&messages, &index).unwrap_err();
    assert_eq!(error.code, StoreErrorCode::CorruptLog);
    assert_eq!(error.message, "indexed event absent");
    // A row without a resident active source is refused the same way.
    let mut index = rows(&messages, &[]);
    index.rows[0].active = None;
    let error = inline_suffix(&messages, &index).unwrap_err();
    assert_eq!(error.message, "active source absent");
    // The first row carrying an identity wins, exactly as `Index::get` reports
    // it, so a later duplicate cannot silently supply a source.
    let mut index = rows(&messages, &[]);
    let duplicate = index.rows[1].clone();
    index.rows[1].active = None;
    index.rows.push(duplicate);
    let error = inline_suffix(&messages, &index).unwrap_err();
    assert_eq!(error.message, "active source absent");
}

fn temp_root(prefix: &str) -> PathBuf {
    let base = std::env::temp_dir().canonicalize().unwrap();
    let root = base.join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    horon_agent_platform::fs::create_private_dir(&root).unwrap();
    root
}

fn created(id: &SessionId) -> crate::SealedEvent {
    EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        None,
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::SessionCreated),
        serde_json::json!({"metadata": SessionMetadata::default(), "branch_root": false}),
    )
    .seal()
    .unwrap()
}

fn appended(parent: &crate::SealedEvent, text: &str) -> crate::SealedEvent {
    EventDraft::new(
        parent.envelope().session_id.clone(),
        EventId::new_v4(),
        Some(parent.envelope().event_id.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::MessageAppended),
        serde_json::json!({"message": stored(text)}),
    )
    .seal()
    .unwrap()
}

#[test]
fn cold_rebuild_reproduces_the_incremental_restore_of_a_real_log() {
    let paths = SessionPaths::new(temp_root("cold-checkpoint"));
    let id = SessionId::parse("cache").unwrap();
    let mut writer = BlockingWriter::open(paths.clone(), id.clone()).unwrap();
    writer.enable_cache(ReadProbe::default());
    let root = created(&id);
    writer.append(&root).unwrap();
    // Bodies that straddle the resident budget: one message that fills the
    // array exactly, one past the whole budget, and ordinary ones between them.
    let medium = "m".repeat(2000);
    let texts = [
        "first user intent".to_string(),
        text_of_projected_len(INLINE_TAIL_BYTES - 2),
        "after an exact fill".to_string(),
        medium.clone(),
        medium.clone(),
        medium.clone(),
        text_of_projected_len(INLINE_TAIL_BYTES + 1),
        "final message".to_string(),
    ];
    let mut previous = root;
    let mut ids = Vec::new();
    for text in &texts {
        let next = appended(&previous, text);
        writer.append(&next).unwrap();
        assert!(writer.cache_warning().is_none(), "{:?}", writer.cache_warning());
        ids.push(next.envelope().event_id.clone());
        previous = next;
    }
    // The incremental publications are the oracle for the restored tail.
    let warm = SessionCache::new(paths.clone(), ReadProbe::default())
        .tail_view(&id, None)
        .unwrap();
    let listed = crate::catalog::list(&paths, ReadProbe::default());
    let warm_title = listed.unwrap()[0].generated_title.clone();
    drop(writer);
    // Given a cache with no checkpoint, a read reconstructs the context cold.
    let directory = paths.session_dir(&id).unwrap();
    std::fs::remove_file(directory.join("checkpoint.json")).unwrap();
    let cache = SessionCache::new(paths.clone(), ReadProbe::default());
    let cold = cache.tail_view(&id, None).unwrap();
    // Then the resident suffix and coverage are the published ones.
    assert_eq!(cold.coverage, warm.coverage);
    let cold_ids: Vec<_> = cold.messages.iter().map(|m| m.event_id.clone()).collect();
    let warm_ids: Vec<_> = warm.messages.iter().map(|m| m.event_id.clone()).collect();
    assert_eq!(cold_ids, warm_ids);
    assert_eq!(warm.coverage.active_messages, texts.len());
    assert_eq!(warm.coverage.inline_messages, 1);
    assert_eq!(warm.coverage.inline_from, texts.len() - 1);
    assert_eq!(cold.messages[0].message.content_preview(), "final message");
    // And the complete restore still returns every active message in order.
    let restored = cache.recent(&id, None).unwrap();
    let restored_ids: Vec<_> = restored.tail.iter().map(|m| m.event_id.clone()).collect();
    assert_eq!(restored_ids, ids);
    assert_eq!(restored.tail.len(), texts.len());
    let listed = crate::catalog::list(&paths, ReadProbe::default()).unwrap();
    assert_eq!(listed[0].generated_title, warm_title);
    assert_eq!(warm_title.as_deref(), Some("first user intent"));
    std::fs::remove_dir_all(paths.root()).unwrap();
}
