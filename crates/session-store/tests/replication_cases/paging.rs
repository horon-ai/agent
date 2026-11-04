use super::*;

#[test]
fn fixed_closure_resumes_when_remote_pages_arrive_backwards() {
    // Given a history larger than one page.
    let a = Home::new();
    let b = Home::new();
    let mut events = vec![root()];
    events.extend((2..=260).map(|n| unknown(n, n - 1)));
    seed(&a, &events);
    let mut req = request(&[260]);
    let first = export_events(&a.paths(), &req).unwrap();
    assert_eq!(first.events.len(), 256);
    req.cursor = first.next_cursor.clone();
    let last = export_events(&a.paths(), &req).unwrap();
    assert_eq!(last.events.len(), 4);
    // When a later page arrives first, then the dependency page and retries.
    assert_eq!(
        import_events(&b.paths(), &last).unwrap_err().code,
        StoreErrorCode::MissingDependency
    );
    assert!(b.bytes().is_empty());
    import_events(&b.paths(), &first).unwrap();
    import_events(&b.paths(), &last).unwrap();
    import_events(&b.paths(), &first).unwrap();
    // Then no original is lost or duplicated.
    assert_eq!(a.bytes(), b.bytes());
}

#[test]
fn cursor_and_known_id_limits_are_checked() {
    // Given a multi-page source and its fixed-head cursor.
    let a = Home::new();
    let mut events = vec![root()];
    events.extend((2..=258).map(|n| unknown(n, n - 1)));
    seed(&a, &events);
    let mut req = request(&[258]);
    let first = export_events(&a.paths(), &req).unwrap();
    req.cursor = first.next_cursor;
    let mut changed = req.clone();
    changed.heads = vec![eid(257)];
    // When the head, cursor encoding or known-ID bound is violated.
    assert_eq!(
        export_events(&a.paths(), &changed).unwrap_err().code,
        StoreErrorCode::InvalidCursor
    );
    changed = req.clone();
    changed.cursor = Some("broken!".into());
    assert_eq!(
        export_events(&a.paths(), &changed).unwrap_err().code,
        StoreErrorCode::InvalidCursor
    );
    changed = request(&[258]);
    changed.known_ids = (1..=257).map(eid).collect();
    assert!(export_events(&a.paths(), &changed).is_err());
    // Then a valid known-ID set skips only the supplied originals.
    changed.known_ids = (1..=256).map(eid).collect();
    assert_eq!(export_events(&a.paths(), &changed).unwrap().events.len(), 2);
}

#[test]
fn escaped_wire_size_splits_pages_not_just_raw_event_lengths() {
    // Given three inline records whose strings double in size in the wire envelope.
    let a = Home::new();
    let b = Home::new();
    let mut originals = vec![root()];
    for n in 2..=4 {
        originals.push(seal(
            n,
            Some(n - 1),
            "Future-Event",
            json!({"escaped":"\"".repeat(1600*1024)}),
        ));
        assert!(originals.last().unwrap().blob().is_none());
    }
    seed(&a, &originals);
    let mut req = request(&[4]);
    // When pages are serialized with their actual escaping and envelope.
    let first = export_events(&a.paths(), &req).unwrap();
    assert!(serde_json::to_vec(&first).unwrap().len() <= MAX_WIRE_BYTES);
    assert!(first.events.len() < 4);
    assert!(first.next_cursor.is_some());
    import_events(&b.paths(), &first).unwrap();
    req.cursor = first.next_cursor;
    let last = export_events(&a.paths(), &req).unwrap();
    assert!(serde_json::to_vec(&last).unwrap().len() <= MAX_WIRE_BYTES);
    assert!(last.next_cursor.is_none());
    import_events(&b.paths(), &last).unwrap();
    // Then escaped page limits preserve every opaque original byte.
    assert_eq!(a.bytes(), b.bytes());
    let too_large = page(&originals);
    let before = b.bytes();
    assert!(serde_json::to_vec(&too_large).unwrap().len() > MAX_WIRE_BYTES);
    assert_eq!(
        import_events(&b.paths(), &too_large).unwrap_err().code,
        StoreErrorCode::RecordTooLarge
    );
    assert_eq!(before, b.bytes());
}

#[test]
fn canonical_order_uses_event_ids_not_arrival_or_timestamp() {
    // Given a source whose two siblings were appended in reverse ID order.
    let a = Home::new();
    seed(&a, &[root(), unknown(9, 1), unknown(2, 1)]);
    // When both branch heads are exported.
    let wire = export_events(&a.paths(), &request(&[9, 2])).unwrap();
    // Then dependencies precede children and available siblings use ascending IDs.
    assert_eq!(
        wire.events.iter().map(|e| e.event_id.clone()).collect::<Vec<_>>(),
        vec![eid(1), eid(2), eid(9)]
    );
    assert_eq!(wire.heads, vec![eid(2), eid(9)]);
}

#[test]
fn changed_closure_digest_invalidates_cursor_but_unrelated_append_does_not() {
    // Given a cursor over a fixed head.
    let a = Home::new();
    let mut events = vec![root()];
    events.extend((2..=258).map(|n| unknown(n, n - 1)));
    seed(&a, &events);
    let mut req = request(&[258]);
    req.cursor = export_events(&a.paths(), &req).unwrap().next_cursor;
    // When the log gains a descendant outside the fixed closure.
    seed(&a, &[unknown(259, 258)]);
    assert_eq!(export_events(&a.paths(), &req).unwrap().events.len(), 2);
    // Then the cursor remains usable, but a valid rehashed source replacement is rejected.
    let mut replacement = events[1].envelope().clone();
    replacement.payload = Some(json!({"different":true}));
    replacement.content_hash = replacement.content_hash_of().unwrap();
    events[1] = SealedEvent::Inline(replacement);
    let bytes = events
        .iter()
        .flat_map(|e| encode_record_line(e.envelope()).unwrap())
        .collect::<Vec<_>>();
    fs::write(a.paths().events_log(&sid()).unwrap(), bytes).unwrap();
    assert_eq!(
        export_events(&a.paths(), &req).unwrap_err().code,
        StoreErrorCode::InvalidCursor
    );
}
