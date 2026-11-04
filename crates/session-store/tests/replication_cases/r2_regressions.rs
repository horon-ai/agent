use super::*;
use horon_agent_session_store::record_exchange::*;

fn original(n: u64, parent: Option<u64>, kind: &str, payload: Value) -> SealedEvent {
    EventDraft::new(
        SessionId::parse("independent").unwrap(),
        identity(n),
        parent.map(identity),
        OriginId::parse("10000000-0000-4000-8000-000000000001").unwrap(),
        serde_json::from_value(json!("2020-01-01T00:00:00Z")).unwrap(),
        EventKind::parse(kind),
        payload,
    )
    .seal()
    .unwrap()
}

fn identity(n: u64) -> EventId {
    EventId::parse(format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
}

fn maximum_original() -> SealedEvent {
    let empty = original(2, Some(1), "future-verifier", json!({"quotes":""}));
    let overhead = encode_record_line(empty.envelope()).unwrap().len();
    let large = original(
        2,
        Some(1),
        "future-verifier",
        json!({"quotes":"\"".repeat((MAX_RECORD_BYTES-overhead)/2)}),
    );
    assert!(large.blob().is_none());
    assert_eq!(encode_record_line(large.envelope()).unwrap().len(), 8_388_608);
    large
}

#[test]
fn maximum_existing_original_completes_fixed_head_transfer() {
    let a = Home::new();
    let b = Home::new();
    let id = SessionId::parse("independent").unwrap();
    let events = [
        original(
            1,
            None,
            "session_created",
            json!({"metadata":SessionMetadata::default(),"branch_root":false}),
        ),
        maximum_original(),
        original(3, Some(2), "future-verifier", json!({"tail":true})),
    ];
    let mut writer = BlockingWriter::open(a.paths(), id.clone()).unwrap();
    for event in &events {
        writer.append(event).unwrap();
    }
    drop(writer);
    let before = fs::read(a.paths().events_log(&id).unwrap()).unwrap();
    let mut request = ExportRequest {
        protocol_version: 1,
        session_id: id.clone(),
        heads: vec![identity(3)],
        known_ids: vec![],
        cursor: None,
    };
    let mut ids = Vec::new();
    let mut references = 0;
    let mut pages = 0;
    loop {
        let page = export_events(&a.paths(), &request).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_WIRE_BYTES);
        if pages == 0 {
            let mut candidate = page.clone();
            candidate.events = vec![events[1].envelope().canonical_record().unwrap()];
            let with_cursor = serde_json::to_vec(&candidate).unwrap().len();
            assert_eq!(with_cursor, 16_777_555);
            candidate.next_cursor = None;
            assert_eq!(serde_json::to_vec(&candidate).unwrap().len(), 16_777_166);
            println!("MAXIMUM_ORIGINAL line=8388608 wire_with_cursor={with_cursor}");
        }
        for reference in &page.record_refs {
            references += 1;
            assert_ne!(reference.sha256, reference.content_hash);
            let mut offset = record_blob_status(&b.paths(), &id, reference).unwrap().status.received;
            while offset < reference.size {
                let chunk = export_record_blob(&a.paths(), &id, reference, offset).unwrap();
                assert!(chunk.chunk.next_offset - offset <= 1024 * 1024);
                offset = import_record_blob(&b.paths(), &id, &chunk).unwrap().status.received;
            }
            assert!(record_blob_status(&b.paths(), &id, reference).unwrap().status.complete);
        }
        let records = page.resolve_records(&b.paths()).unwrap();
        assert!(records.iter().map(|r| r.canonical_json.len()).sum::<usize>() <= MAX_WIRE_BYTES);
        ids.extend(records.iter().map(|e| e.event_id.clone()));
        import_events(&b.paths(), &page).unwrap();
        import_events(&b.paths(), &page).unwrap();
        pages += 1;
        request.cursor = page.next_cursor;
        if request.cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids, vec![identity(1), identity(2), identity(3)]);
    assert_eq!(references, 1);
    assert_eq!(pages, 2);
    assert_eq!(fs::read(a.paths().events_log(&id).unwrap()).unwrap(), before);
    assert_eq!(fs::read(b.paths().events_log(&id).unwrap()).unwrap(), before);
    println!(
        "MAXIMUM_TRANSFER pages={pages} referenced={references} events=3 sha256={}",
        sha256_hex(&before)
    );
}

#[test]
fn every_repeated_prefix_sync_failure_retains_acknowledgements_and_cursor() {
    for boundary in [IoPoint::LogDirectorySync, IoPoint::LogFileSync] {
        let h = Home::new();
        seed(&h, &[root()]);
        let wire = page(&[unknown(2, 1), unknown(3, 2)]);
        let mut hits = 0;
        let mut writer = BlockingWriter::with_hook(
            h.paths(),
            sid(),
            Box::new(move |point| {
                if point == boundary {
                    hits += 1;
                    if (2..=4).contains(&hits) {
                        return Err(std::io::ErrorKind::StorageFull.into());
                    }
                }
                Ok(())
            }),
        )
        .unwrap();
        let first = writer.import_events(&wire).unwrap_err();
        assert_eq!(first.details.accepted_ids, vec![eid(2).as_str()]);
        let mut token = first.details.retry_cursor.unwrap();
        for _ in 0..2 {
            let error = writer.import_events_resuming(&wire, Some(&token)).unwrap_err();
            assert!(
                error.details.retry_cursor.is_some(),
                "repeat failure must retain a cursor"
            );
            assert_eq!(error.details.accepted_ids, vec![eid(2).as_str()]);
            token = error.details.retry_cursor.unwrap();
        }
        let before = h.bytes();
        writer.import_events_resuming(&wire, Some(&token)).unwrap();
        drop(writer);
        import_events(&h.paths(), &wire).unwrap();
        assert_eq!(h.bytes(), before);
        assert_eq!(full_scan(&h.paths(), &sid()).unwrap().events.len(), 3);
        println!("REPEATED_PREFIX {boundary:?} failures=3 accepted=1 cursor=true final=3");
    }
}
