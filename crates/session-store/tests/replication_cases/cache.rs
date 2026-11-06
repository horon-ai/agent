use super::*;
use horon_agent_session_store::checkpoint::SessionCache;

fn metadata() -> SealedEvent {
    seal(
        2,
        Some(1),
        "metadata_changed",
        json!({"metadata":SessionMetadata {
            title:Some("imported".into()), ..Default::default()
        }}),
    )
}

#[test]
fn imported_originals_invalidate_cache_without_changing_local_selection() {
    // Given a host-local selection and previously materialized context.
    let h = Home::new();
    seed(&h, &[root()]);
    let cache = SessionCache::new(h.paths(), ReadProbe::default());
    assert_eq!(cache.recent(&sid(), None).unwrap().head, eid(1));
    let mut selection = LocalHead::default();
    selection
        .select(&EventGraph::open(&h.paths(), &sid()).unwrap(), &eid(1))
        .unwrap();
    // When a metadata event is imported without transporting local authority.
    let wire = page(&[metadata()]);
    import_events(&h.paths(), &wire).unwrap();
    assert!(h.paths().root().join(".catalog.dirty/replica").exists());
    let view = cache.recent(&sid(), None).unwrap();
    // Then original-driven cache repair sees the new metadata and the selection stays local.
    assert_eq!(view.head, eid(2));
    assert_eq!(view.metadata.title.as_deref(), Some("imported"));
    assert_eq!(selection.selected(), Some(&eid(1)));
    let object = serde_json::to_value(&wire).unwrap();
    assert_eq!(
        object
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "blob_refs",
            "events",
            "heads",
            "next_cursor",
            "protocol_version",
            "session_id"
        ]
    );
}

#[test]
fn cache_failure_remains_warning_after_import_ack_and_retry_repairs_it() {
    // Given an enabled cache with an injected real cache-publication sync failure.
    let h = Home::new();
    seed(&h, &[root()]);
    let mut fired = false;
    let mut writer = BlockingWriter::with_hook(
        h.paths(),
        sid(),
        Box::new(move |point| {
            if !fired && point == IoPoint::CacheStaged {
                fired = true;
                return Err(std::io::ErrorKind::StorageFull.into());
            }
            Ok(())
        }),
    )
    .unwrap();
    writer.enable_cache(ReadProbe::default());
    // When the original commits but the derived cache cannot synchronize.
    let wire = page(&[metadata()]);
    let result = writer.import_events(&wire).unwrap();
    assert_eq!(result.accepted_ids, vec![eid(2)]);
    assert!(writer.cache_warning().is_some());
    assert!(h.paths().root().join(".catalog.dirty/replica").exists());
    let before = h.bytes();
    writer.import_events(&wire).unwrap();
    // Then retransmission repairs the cache without rewriting the original log.
    assert!(writer.cache_warning().is_none());
    assert_eq!(before, h.bytes());
    drop(writer);
    assert_eq!(
        SessionCache::new(h.paths(), ReadProbe::default())
            .recent(&sid(), None)
            .unwrap()
            .head,
        eid(2)
    );
}
