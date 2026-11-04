use super::*;

#[test]
fn loader_boundary_id_lookup_prefers_events_and_refuses_corrupt_events() -> anyhow::Result<()> {
    let _lock = tests::lock_env();
    let home = tempfile::tempdir()?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", home.path());
    let mut canonical = Session::create_with_id("requested_id".into(), None, None);
    canonical.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "canonical event truth".into(),
            cache_control: None,
        }],
    );
    canonical.save()?;
    let log = canonical.event_log_path()?;
    let original = std::fs::read(&log)?;
    drop(canonical);
    let shadow = session_path("requested_id")?;
    let unrelated = Session::create_with_id("different_id".into(), None, None);
    crate::storage::write_json(&shadow, &unrelated)?;
    let shadow_bytes = std::fs::read(&shadow)?;

    let loaded = Session::load("requested_id")?;
    assert_eq!(loaded.id, "requested_id");
    assert_eq!(loaded.history_messages()?[0].content_preview(), "canonical event truth");
    assert_eq!(Session::load_from_path(&shadow)?.id, "different_id");
    assert_eq!(std::fs::read(&log)?, original);
    assert_eq!(std::fs::read(&shadow)?, shadow_bytes);

    // An invalid legacy shadow cannot hide an already valid canonical log.
    std::fs::write(&shadow, b"not JSON")?;
    assert_eq!(Session::load("requested_id")?.id, "requested_id");
    assert_eq!(std::fs::read(&shadow)?, b"not JSON");
    assert_eq!(std::fs::read(&log)?, original);

    // A corrupt canonical log cannot silently fall back to a valid snapshot.
    crate::storage::write_json(&shadow, &loaded)?;
    let fallback = std::fs::read(&shadow)?;
    drop(loaded);
    std::fs::write(&log, b"invalid event\n")?;
    assert!(Session::load("requested_id").is_err());
    assert_eq!(std::fs::read(&log)?, b"invalid event\n");
    assert_eq!(std::fs::read(&shadow)?, fallback);
    Ok(())
}

#[test]
fn loader_boundary_id_lookup_rejects_a_foreign_snapshot_without_events() -> anyhow::Result<()> {
    let _lock = tests::lock_env();
    let home = tempfile::tempdir()?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", home.path());
    let path = session_path("requested_missing")?;
    let unrelated = Session::create_with_id("different_missing".into(), None, None);
    crate::storage::write_json(&path, &unrelated)?;
    let source = std::fs::read(&path)?;

    assert!(Session::load("requested_missing").is_err());
    assert_eq!(Session::load_from_path(&path)?.id, "different_missing");
    assert_eq!(std::fs::read(&path)?, source);
    assert!(!home.path().join("sessions/requested_missing/events.jsonl").exists());
    assert!(!home.path().join("sessions/different_missing/events.jsonl").exists());
    Ok(())
}

#[test]
fn loader_boundary_explicit_title_backfill_preserves_priority_and_source() -> anyhow::Result<()> {
    let _lock = tests::lock_env();
    let home = tempfile::tempdir()?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", home.path());
    let path = home.path().join("exports/title-copy.json");
    for (title, custom, label, expected) in [
        (None, None, None, "Visible first question"),
        (Some("existing"), None, None, "existing"),
        (Some("existing"), None, Some("saved label"), "saved label"),
        (Some("existing"), Some("custom"), Some("saved label"), "custom"),
    ] {
        let value = serde_json::json!({
            "id": "title_source",
            "title": title,
            "custom_title": custom,
            "saved": label.is_some(),
            "save_label": label,
            "created_at": "2020-01-01T00:00:00Z",
            "updated_at": "2020-01-01T00:00:00Z",
            "messages": [{
                "id": "title_message",
                "role": "user",
                "content": [{"type": "text", "text": "Visible first question"}]
            }]
        });
        crate::storage::write_json(&path, &value)?;
        let source = std::fs::read(&path)?;

        let loaded = Session::load_from_path(&path)?;
        assert_eq!(loaded.display_title(), Some(expected));
        assert_eq!(loaded.title.as_deref(), title.or(Some("Visible first question")));
        assert_eq!(std::fs::read(&path)?, source);
        assert!(!home.path().join("sessions/title_source/events.jsonl").exists());
    }
    Ok(())
}

#[test]
fn explicit_export_paths_preserve_identity_source_and_save_guard() -> anyhow::Result<()> {
    let _lock = tests::lock_env();
    let home = tempfile::tempdir()?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", home.path());
    for (id, relative) in [
        ("renamed_id", "sessions/renamed_export.json"),
        ("elsewhere_id", "exports/elsewhere_id.json"),
    ] {
        let mut original = Session::create_with_id(id.into(), None, Some("export".into()));
        original.status = SessionStatus::Closed;
        original.last_pid = None;
        original.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: "original".into(),
                cache_control: None,
            }],
        );
        let path = home.path().join(relative);
        crate::storage::write_json(&path, &original)?;
        let bytes = std::fs::read(&path)?;
        let mut loaded = Session::load_from_path(&path)?;
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.history_messages()?.len(), 1);
        loaded.save()?;
        assert_eq!(Session::load(id)?.history_messages()?.len(), 1);
        assert_eq!(Session::load_from_path(&loaded.event_log_path()?)?.id, id);
        assert_eq!(std::fs::read(&path)?, bytes);
        let mut other = original.clone();
        other.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "different export".into(),
                cache_control: None,
            }],
        );
        let alternate = home.path().join(format!("exports/conflicting_{id}.json"));
        crate::storage::write_json(&alternate, &other)?;
        let mut detached = Session::load_from_path(&alternate)?;
        let canonical = std::fs::read(loaded.event_log_path()?)?;
        assert!(detached.save().is_err());
        assert_eq!(std::fs::read(loaded.event_log_path()?)?, canonical);
        let mut changed = bytes.clone();
        changed.push(b'\n');
        std::fs::write(&path, changed)?;
        assert!(loaded.save().is_err());
        assert!(Session::load(id).is_err());
    }
    Ok(())
}
