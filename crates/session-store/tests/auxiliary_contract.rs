use horon_agent_session_store::*;
use serde_json::json;

#[test]
fn auxiliary_wire_and_borrowed_consumers_preserve_canonical_originals() {
    // Given each public auxiliary shape and fixed original identities.
    let fixtures = [
        json!({
            "aux_kind":"env_snapshot","captured_at":"1970-01-01T00:00:00Z",
            "reason":"capture","session_id":"aux-contract","working_dir":"/inert",
            "provider":"openai","model":"fixture","horon_agent_version":"fixture",
            "horon_agent_git_hash":null,"horon_agent_git_dirty":null,"os":"fixture",
            "arch":"fixture","pid":u32::MAX,"is_selfdev":false,"is_debug":true,
            "is_canary":false,"testing_build":null,"working_git":null
        }),
        json!({
            "aux_kind":"memory_injection","summary":"retained","content":"inert",
            "count":1,"memory_ids":["one"],"age_ms":u64::MAX,"before_message":3,
            "timestamp":"1970-01-01T00:00:00Z"
        }),
        json!({"aux_kind":"replay_event","schema_version":1,
            "value":{"large":u64::MAX,"negative_zero":-0.0,"items":[3,2,1]}}),
        json!({"aux_kind":"legacy","schema_version":1,
            "value":{"opaque":["inert",u64::MAX,-0.0]}}),
    ];
    let session = SessionId::parse("aux-contract").unwrap();
    let origin = OriginId::parse("11111111-1111-4111-8111-111111111111").unwrap();
    let root_id = EventId::parse("33333333-3333-4333-8333-333333333333").unwrap();
    let root = EventDraft::new(
        session.clone(),
        root_id.clone(),
        None,
        origin.clone(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(KnownEventKind::SessionCreated),
        json!({"metadata":SessionMetadata::default(),"branch_root":false}),
    )
    .seal()
    .unwrap();
    // Golden original hashes captured before changing the enum representation.
    let expected_hashes = [
        (
            "f458f3e79c7536b8ec381008d1656c55e987839a04c92ab2e3f78d49e3695d10",
            "630454dad6a6259afda4a27a0314e6ab3616904b93b01ca9cdb60e6ffde0bcf9",
        ),
        (
            "0690a118ebd2730850d8bb38ba89c57dcde2d2da98797a2cdb447480aaf28d2c",
            "af7bfe544ec352d03a3cff710169789af1da2d8feaab9cf03dc317f38030eff8",
        ),
        (
            "2f1a7f4f6d7f50bd752ae34b3419d625a1ab00a487826cc7924fa6395443a816",
            "74aefe7ed60e5257d8dd352441f2ec3c862f62f520e928139898a2408fc0d1ca",
        ),
        (
            "bc47adbd47c6818a984c88341903bbc4f0036d6a32926dfadcecb3c46fb05f66",
            "8dc6588bd2c8f85bb5878d63af51cb21c61178143cbf8c993a42fa79c2ae0283",
        ),
    ];
    for (raw, (expected_hash, expected_line_hash)) in fixtures.into_iter().zip(expected_hashes) {
        let typed: AuxiliaryRecordedPayload = serde_json::from_value(raw.clone()).unwrap();
        let event = EventDraft::new(
            session.clone(),
            EventId::parse("22222222-2222-4222-8222-222222222222").unwrap(),
            Some(root_id.clone()),
            origin.clone(),
            chrono::DateTime::UNIX_EPOCH,
            EventKind::new(KnownEventKind::AuxiliaryRecorded),
            serde_json::to_value(typed.clone()).unwrap(),
        )
        .seal()
        .unwrap();
        let before = encode_record_line(event.envelope()).unwrap();
        // When the real projection exposes the same borrowed variants used by callers.
        let graph = EventGraph::from_events(&session, &[root.envelope().clone(), event.envelope().clone()]).unwrap();
        let projection = graph.replay(None).unwrap();
        let payload = &projection.auxiliary()[0].1;
        let consumed = match payload {
            AuxiliaryRecordedPayload::EnvSnapshot(snapshot) => serde_json::to_value(snapshot).unwrap(),
            AuxiliaryRecordedPayload::MemoryInjection(injection) => serde_json::to_value(injection).unwrap(),
            AuxiliaryRecordedPayload::ReplayEvent { value, .. } | AuxiliaryRecordedPayload::Legacy { value, .. } => {
                value.clone()
            }
        };
        let mut expected = raw.clone();
        let kind = expected.as_object_mut().unwrap().remove("aux_kind").unwrap();
        let expected_consumer = expected.get("value").cloned().unwrap_or(expected);
        // Then serialized tags/fields, caller values and canonical original bytes agree.
        assert_eq!(consumed, expected_consumer);
        assert_eq!(serde_json::to_value(payload).unwrap(), raw);
        assert_eq!(
            encode_record_line(&decode_record_line(&before).unwrap()).unwrap(),
            before
        );
        assert_eq!(event.envelope().content_hash, expected_hash);
        assert_eq!(sha256_hex(&before), expected_line_hash);
        println!(
            "AUXILIARY_WIRE kind={} hash={} line_sha256={}",
            kind.as_str().unwrap(),
            event.envelope().content_hash,
            sha256_hex(&before)
        );
    }
}
