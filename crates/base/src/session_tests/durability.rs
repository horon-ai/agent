//! Local state the event log cannot rebuild must survive a reopen.
//!
//! The selected head, its explicit flag, the external-source receipt and the
//! runtime binding are originals: they are flushed with the file that carries
//! them. The facade cache is derived, so losing it only costs a reconstruction
//! from the event log.

use super::*;
use anyhow::{Context, Result};
use horon_agent_session_store as store;

fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: value.into(),
        cache_control: None,
    }]
}

fn session_dir(session: &Session) -> Result<std::path::PathBuf> {
    session
        .event_log_path()?
        .parent()
        .map(std::path::Path::to_path_buf)
        .context("session directory missing")
}

#[test]
fn local_head_and_runtime_survive_reopen() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-local-state-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);

    // Given a committed session that then records a working directory and an
    // explicit head selection.
    let mut session = Session::create_with_id("local_state".into(), None, None);
    session.add_message(Role::User, text("first prompt"));
    session.save()?;
    let working_dir = std::env::current_dir()?.canonicalize()?.display().to_string();
    session.working_dir = Some(working_dir.clone());
    session.add_message(Role::User, text("second prompt"));
    session.event_state.explicit_head = true;
    session.save()?;
    let head = session.event_state.head.clone().context("head missing")?;
    let directory = session_dir(&session)?;

    // Then each local original exists on disk before the reopen.
    for name in ["local-head.json", "local-head-explicit.json", "local-runtime.json"] {
        assert!(directory.join(name).is_file(), "{name} must be published");
    }
    assert_eq!(
        serde_json::from_slice::<store::EventId>(&std::fs::read(directory.join("local-head.json"))?)?,
        head
    );
    assert!(serde_json::from_slice::<bool>(&std::fs::read(
        directory.join("local-head-explicit.json")
    )?)?);
    drop(session);

    // When the session is reopened from its originals.
    let reopened = Session::load("local_state")?;
    // Then the selected head, its intent and the runtime binding are unchanged.
    assert_eq!(reopened.event_state.head.as_ref(), Some(&head));
    assert!(reopened.event_state.explicit_head);
    assert_eq!(reopened.working_dir.as_deref(), Some(working_dir.as_str()));
    assert_eq!(reopened.history_messages()?.len(), 2);
    drop(reopened);

    // Given a legacy snapshot outside the store that names an external source.
    let snapshot = home_path.join("external_snapshot.json");
    let mut source = Session::create_with_id("external_source".into(), None, None);
    source.add_message(Role::User, text("snapshot body"));
    std::fs::write(&snapshot, serde_json::to_vec(&source)?)?;
    let mut loaded = Session::load_from_path(&snapshot)?;
    let recorded = loaded.event_state.external_source.clone().context("external source")?;
    // When it is saved, the receipt for that source is published beside the log.
    loaded.save()?;
    let receipt = session_dir(&loaded)?.join("local-explicit-source.json");
    assert!(receipt.is_file(), "the external-source receipt must be published");
    drop(loaded);

    // Then a reopen restores the same receipt instead of forgetting the source.
    let reopened = Session::load("external_source")?;
    assert_eq!(reopened.event_state.external_source, Some(recorded));
    assert_eq!(reopened.history_messages()?.len(), 1);
    Ok(())
}

/// A value that changes whenever the binding file is rewritten: its bytes plus,
/// on Unix, the inode a rename produces.
fn publication_token(path: &std::path::Path) -> Result<Vec<u8>> {
    let mut token = std::fs::read(path)?;
    // On a platform with inodes the token also covers the inode a rename
    // produces; elsewhere the adapter reports zero and the bytes alone decide.
    let stamp = horon_agent_platform::fs::file_stamp(&std::fs::metadata(path)?);
    token.extend_from_slice(&stamp.inode.to_le_bytes());
    Ok(token)
}

#[test]
fn local_runtime_skips_status_only_commits() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-local-runtime-skip-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);

    // Given a saved session whose runtime binding is on disk.
    let mut session = Session::create_with_id("runtime_skip".into(), None, None);
    session.add_message(Role::User, text("first prompt"));
    session.save()?;
    let binding = session_dir(&session)?.join("local-runtime.json");
    assert!(binding.is_file(), "the runtime binding must be published");
    let published = publication_token(&binding)?;

    // When a mid-turn batch appends originals and only the activity clock moves.
    session.last_active_at = Some(session.updated_at + chrono::Duration::seconds(5));
    session.add_message(Role::Assistant, text("mid-turn reply"));
    session.save()?;
    // Then no flush is spent on the binding.
    assert_eq!(
        publication_token(&binding)?,
        published,
        "a status-only commit must not rewrite the runtime binding"
    );

    // When the turn ends and the session settles.
    session.mark_closed();
    session.add_message(Role::User, text("after the turn"));
    session.save()?;
    // Then the binding is published once, carrying the settled status.
    let settled = publication_token(&binding)?;
    assert_ne!(settled, published, "the settled commit must publish the binding");
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&binding)?)?;
    assert_eq!(stored["status"], serde_json::json!("Closed"));

    // And a further commit in the same state writes nothing again.
    session.add_message(Role::User, text("same state"));
    session.save()?;
    assert_eq!(
        publication_token(&binding)?,
        settled,
        "an unchanged binding must not be republished"
    );
    Ok(())
}

#[test]
fn facade_cache_loss_rebuilds() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-facade-loss-")?;
    let _home = tests::EnvVarGuard::set("HORON_AGENT_HOME", &home_path);

    // Given a saved session whose derived facade cache carries its auxiliary state.
    let mut session = Session::create_with_id("facade_loss".into(), None, None);
    session.is_debug = true;
    session.add_message(Role::User, text("cached body"));
    session.save()?;
    let directory = session_dir(&session)?;
    let cache = directory.join("facade-cache.json");
    assert!(cache.is_file(), "the facade cache must be published");
    drop(session);

    // When the derived file is lost and then left truncated by a crash.
    for damage in ["lost", "truncated"] {
        match damage {
            "lost" => std::fs::remove_file(&cache)?,
            _ => std::fs::write(&cache, b"")?,
        }
        // Then the session is reconstructed from the originals instead of failing.
        let restored = Session::load("facade_loss")?;
        assert_eq!(restored.history_messages()?.len(), 1);
        assert_eq!(
            restored.auxiliary_value().get("is_debug"),
            Some(&serde_json::json!(true)),
            "the auxiliary state must come back from the event log"
        );
        // And the load republishes the cache it could not use.
        assert!(cache.is_file(), "the facade cache must be rebuilt on load");
        assert!(
            std::fs::metadata(&cache)?.len() > 0,
            "the rebuilt facade cache must not be empty"
        );
    }
    Ok(())
}
