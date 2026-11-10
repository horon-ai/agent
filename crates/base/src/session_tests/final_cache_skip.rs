//! The derived cache a one-shot run leaves behind is rebuilt by the next reader.
//!
//! The final commit of `horon-agent run` still writes its originals, its runtime
//! binding and its selected head; only the derived checkpoint and catalog row are
//! left. Every reader validates those against the log's stamp, so the untouched
//! cache is refused and rebuilt instead of being served stale.

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

fn checkpoint_stamp(directory: &std::path::Path) -> Result<Vec<u8>> {
    let mut token = std::fs::read(directory.join("checkpoint.json"))?;
    let stamp = horon_agent_platform::fs::file_stamp(&std::fs::metadata(directory.join("checkpoint.json"))?);
    token.extend_from_slice(&stamp.inode.to_le_bytes());
    Ok(token)
}

fn log_len(session: &Session) -> Result<u64> {
    Ok(std::fs::metadata(session.event_log_path()?)?.len())
}

#[test]
fn final_commit_leaves_its_cache_to_the_next_reader() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-final-cache-skip-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    // Given a one-shot session whose first commit paid for the derived cache.
    let mut session = Session::create_with_id("final_cache_skip".into(), None, None);
    session.set_defer_final_cache(true);
    session.add_message(Role::User, text("first prompt"));
    session.save()?;
    let directory = session_dir(&session)?;
    let published = checkpoint_stamp(&directory)?;

    // When the final answer is committed with the mark set.
    session.add_message(Role::Assistant, text("the answer"));
    session.mark_final_commit();
    session.save()?;
    // Then the originals grew but the derived checkpoint did not move.
    assert!(log_len(&session)? > 0, "the commit must still append originals");
    assert_eq!(
        checkpoint_stamp(&directory)?,
        published,
        "the final commit must not rebuild the derived cache"
    );
    // And the head the commit selected is on disk.
    let head = session.event_state.head.clone().context("head missing")?;
    let id = store::SessionId::parse("final_cache_skip")?;
    let paths = store_paths()?;
    assert_eq!(
        serde_json::from_slice::<store::EventId>(&std::fs::read(directory.join("local-head.json"))?)?,
        head,
        "the selected head is a local original and must still be published"
    );
    drop(session);

    // When the next reader opens it, the stale checkpoint is refused by its
    // stamp and rebuilt from the originals.
    let reopened = Session::load("final_cache_skip")?;
    // Then it sees the newest message and head.
    assert_eq!(reopened.history_messages()?.len(), 2);
    assert_eq!(reopened.event_state.head.as_ref(), Some(&head));
    assert_ne!(
        checkpoint_stamp(&session_dir(&reopened)?)?,
        published,
        "the reader must republish the checkpoint it could not trust"
    );
    // And the listing reports the same session from the rebuilt row.
    let row = store::catalog::row(&paths, &id, store::ReadProbe::default())?;
    assert_eq!(row.session_id, id);
    Ok(())
}

#[test]
fn final_commit_publishes_when_deferral_is_off() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-final-cache-on-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    // Given a session that never asked for the deferral.
    let mut session = Session::create_with_id("final_cache_on".into(), None, None);
    session.add_message(Role::User, text("first prompt"));
    session.save()?;
    let directory = session_dir(&session)?;
    let published = checkpoint_stamp(&directory)?;

    // When a commit is marked final anyway.
    session.add_message(Role::Assistant, text("the answer"));
    session.mark_final_commit();
    session.save()?;
    // Then the ordinary path still rebuilds the derived cache.
    assert_ne!(
        checkpoint_stamp(&directory)?,
        published,
        "without the deferral the commit must publish its cache"
    );
    let reopened = Session::load("final_cache_on")?;
    assert_eq!(reopened.history_messages()?.len(), 2);
    Ok(())
}

#[test]
fn the_mark_is_spent_by_one_commit() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-final-cache-once-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    // Given a deferring session whose final commit was already left.
    let mut session = Session::create_with_id("final_cache_once".into(), None, None);
    session.set_defer_final_cache(true);
    session.add_message(Role::User, text("first prompt"));
    session.save()?;
    session.add_message(Role::Assistant, text("the answer"));
    session.mark_final_commit();
    session.save()?;
    let directory = session_dir(&session)?;
    let skipped = checkpoint_stamp(&directory)?;

    // When one more commit runs without a new mark.
    session.add_message(Role::User, text("a later turn"));
    session.save()?;
    // Then that commit publishes, because the mark was consumed by its predecessor.
    assert_ne!(
        checkpoint_stamp(&directory)?,
        skipped,
        "the mark must not survive the commit that spent it"
    );
    Ok(())
}

#[test]
fn the_mark_is_consumed_at_the_start_of_its_commit() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-final-cache-consume-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    // Given a deferring session with the mark armed.
    let mut session = Session::create_with_id("final_cache_consume".into(), None, None);
    session.set_defer_final_cache(true);
    session.add_message(Role::User, text("first prompt"));
    session.save()?;
    session.add_message(Role::Assistant, text("the answer"));
    session.mark_final_commit();
    assert!(session.event_state.skip_cache_once, "the mark must be armed first");

    // When that commit runs.
    session.save()?;
    // Then the mark was taken by it, so nothing later inherits the skip.
    assert!(
        !session.event_state.skip_cache_once,
        "the commit must consume the mark before doing its work"
    );
    Ok(())
}

#[test]
fn a_stale_cache_without_a_dirty_marker_is_refused() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-final-cache-stale-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    // Given a session whose final commit left its derived cache alone.
    let mut session = Session::create_with_id("final_cache_stale".into(), None, None);
    session.set_defer_final_cache(true);
    session.add_message(Role::User, text("first prompt"));
    session.save()?;
    session.add_message(Role::Assistant, text("the answer"));
    session.mark_final_commit();
    session.save()?;
    let directory = session_dir(&session)?;
    let head = session.event_state.head.clone().context("head missing")?;
    drop(session);

    // When the dirty marker is gone but the checkpoint still names an old log.
    let dirty = store_paths()?.root().join(".catalog.dirty").join("final_cache_stale");
    if dirty.exists() {
        std::fs::remove_file(&dirty)?;
    }
    // Then the stamp comparison alone refuses it and the reader rebuilds.
    let reopened = Session::load("final_cache_stale")?;
    assert_eq!(reopened.history_messages()?.len(), 2);
    assert_eq!(reopened.event_state.head.as_ref(), Some(&head));
    assert!(
        std::fs::metadata(directory.join("checkpoint.json"))?.len() > 0,
        "the reader must publish the checkpoint it rebuilt"
    );
    Ok(())
}

#[test]
fn an_explicit_head_survives_a_skipped_final_commit() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-final-cache-head-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    // Given a session whose final commit selects an explicit head.
    let mut session = Session::create_with_id("final_cache_head".into(), None, None);
    session.set_defer_final_cache(true);
    session.add_message(Role::User, text("first prompt"));
    session.save()?;
    session.add_message(Role::Assistant, text("the answer"));
    session.event_state.explicit_head = true;
    session.mark_final_commit();
    session.save()?;
    let head = session.event_state.head.clone().context("head missing")?;
    drop(session);

    // Then the skipped derived cache does not cost the reader that head.
    let reopened = Session::load("final_cache_head")?;
    assert_eq!(reopened.event_state.head.as_ref(), Some(&head));
    assert!(reopened.event_state.explicit_head);
    assert_eq!(reopened.history_messages()?.len(), 2);
    Ok(())
}
