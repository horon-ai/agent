//! The session binding lives in two copies: the mirror file and the runtime journal.
//!
//! The journal states a `rev` with every binding a run row carries, so a mirror
//! whose rev the journal already holds can be written by rename without a flush,
//! and a file an older build wrote (no rev at all) still opens and still wins
//! until the next binding change gives it one.

use super::*;
use crate::durable_runtime::{Binding, DurableRuntimeStore};
use crate::session::event_wire::MirrorWrite;
use crate::session::event_wire::mirror_writes;
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

fn mirror(session: &Session) -> Result<std::path::PathBuf> {
    Ok(session_dir(session)?.join("local-runtime.json"))
}

fn read_mirror(session: &Session) -> Result<serde_json::Value> {
    Ok(serde_json::from_slice(&std::fs::read(mirror(session)?)?)?)
}

/// Rewrite the mirror by hand, as an older build or a crash would leave it.
fn write_mirror(session: &Session, rev: Option<u64>, status: &SessionStatus, pid: Option<u32>) -> Result<()> {
    let mut value = read_mirror(session)?;
    let object = value.as_object_mut().context("binding must be an object")?;
    match rev {
        Some(rev) => object.insert("rev".into(), serde_json::json!(rev)),
        None => object.remove("rev"),
    };
    object.insert("status".into(), serde_json::to_value(status)?);
    object.insert("last_pid".into(), serde_json::json!(pid));
    std::fs::write(mirror(session)?, serde_json::to_vec(&value)?)?;
    Ok(())
}

/// A journal row that states `status` and `pid` at `rev`, as a run boundary would.
fn append_binding(home: &std::path::Path, session: &Session, rev: u64, status: &SessionStatus, pid: u32) -> Result<()> {
    let root = home.join(crate::durable_runtime::JOURNAL_DIR_NAME);
    let mut journal = DurableRuntimeStore::open_in(&root, &session.id)?;
    let binding = Binding {
        rev,
        status: status.clone(),
        last_pid: Some(pid),
        last_active_at: None,
        working_dir: None,
        migration_epoch: Some(0),
        provider_session_id: None,
    };
    journal.append_run_started("run-1", None, Some(binding))?;
    Ok(())
}

/// A saved session, its mirror on disk, and the status a live process leaves it in.
fn saved_session(id: &str) -> Result<(Session, SessionStatus)> {
    let mut session = Session::create_with_id(id.to_owned(), None, None);
    session.add_message(Role::User, text("first prompt"));
    session.save()?;
    let live = session.status.clone();
    assert_ne!(live, SessionStatus::Closed, "a saved session is live");
    Ok((session, live))
}

/// A session an older build wrote opens without conversion, keeps its binding,
/// and only learns a rev when the binding actually changes.
#[test]
fn previous_build_mirror_opens_unchanged_and_then_earns_a_rev() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-binding-prev-build-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    let (session, live) = saved_session("prev_build")?;
    let working_dir = session.working_dir.clone();
    // Given a mirror an older build wrote: no rev, no journal row behind it.
    write_mirror(&session, None, &live, None)?;
    drop(session);
    let _ = mirror_writes::take("prev_build");

    // When it is opened by this build, nothing is lost and nothing is converted.
    let mut reopened = Session::load("prev_build")?;
    assert_eq!(reopened.status, live);
    assert_eq!(reopened.working_dir, working_dir);
    assert!(
        read_mirror(&reopened)?.get("rev").is_none(),
        "a load must not rewrite the mirror"
    );
    assert!(
        mirror_writes::take("prev_build").is_empty(),
        "a load must publish nothing"
    );

    // Then the first binding change attaches a rev through the flushing path.
    reopened.mark_closed();
    reopened.save()?;
    assert_eq!(
        mirror_writes::take("prev_build"),
        vec![MirrorWrite::Flushed],
        "a rev a journal row does not state must be flushed"
    );
    assert_eq!(read_mirror(&reopened)?["rev"], serde_json::json!(1));
    Ok(())
}

/// A binding the journal already states is written by rename alone: a crash can
/// only lose the file, and the journal answers the same value.
#[test]
fn journal_confirmed_binding_publishes_without_a_flush() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-binding-confirmed-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    // Given a saved session whose mirror is at rev 1.
    let (mut session, live) = saved_session("confirmed")?;
    assert_eq!(read_mirror(&session)?["rev"], serde_json::json!(1));
    let _ = mirror_writes::take("confirmed");

    // And a run row that already states the close this session is about to make.
    let closed = SessionStatus::Closed;
    append_binding(&home_path, &session, 2, &closed, 0)?;
    session.note_journal_binding(Binding {
        rev: 2,
        status: closed.clone(),
        last_pid: None,
        last_active_at: None,
        working_dir: session.working_dir.clone(),
        migration_epoch: Some(session.migration_epoch),
        provider_session_id: session.provider_session_id.clone(),
    });
    assert_ne!(live, closed);

    // When the binding is published, the file lands without a flush.
    session.mark_closed();
    session.save()?;
    assert_eq!(
        mirror_writes::take("confirmed"),
        vec![MirrorWrite::Renamed],
        "a journal-confirmed binding must not spend a flush"
    );
    assert_eq!(read_mirror(&session)?["rev"], serde_json::json!(2));
    assert_eq!(read_mirror(&session)?["status"], serde_json::json!("Closed"));
    Ok(())
}

/// A change no journal row states still flushes its file and directory.
#[test]
fn unconfirmed_binding_change_flushes() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-binding-unconfirmed-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    // Given a saved session, when a resume-relevant field changes mid-turn.
    let (mut session, _) = saved_session("unconfirmed")?;
    let _ = mirror_writes::take("unconfirmed");
    session.provider_session_id = Some("provider-session".into());
    session.add_message(Role::Assistant, text("mid-turn reply"));
    session.save()?;

    // Then that publish flushes and takes the next rev.
    assert_eq!(mirror_writes::take("unconfirmed"), vec![MirrorWrite::Flushed]);
    assert_eq!(read_mirror(&session)?["rev"], serde_json::json!(2));
    assert_eq!(
        read_mirror(&session)?["provider_session_id"],
        serde_json::json!("provider-session")
    );
    Ok(())
}

/// A mirror the journal never confirmed must not hand out a rev the file already
/// used, even after a compaction dropped the journal's binding.
#[test]
fn next_rev_stays_above_a_mirror_the_journal_forgot() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-binding-next-rev-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    // Given a mirror at rev 3 with no journal behind it, as an older build's
    // compaction leaves a session.
    let (mut session, live) = saved_session("next_rev")?;
    write_mirror(&session, Some(3), &live, None)?;
    let _ = mirror_writes::take("next_rev");

    // When the next binding is published.
    session.mark_closed();
    session.save()?;

    // Then it takes rev 4 rather than reusing a number the mirror already held.
    assert_eq!(read_mirror(&session)?["rev"], serde_json::json!(4));
    assert_eq!(mirror_writes::take("next_rev"), vec![MirrorWrite::Flushed]);
    Ok(())
}

/// Every combination of a missing, stale, damaged or ahead mirror resolves the
/// way the rule states, without an error.
#[test]
fn mirror_and_journal_resolve_to_the_newer_binding() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-binding-resolve-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    for (id, mirror_rev, journal_rev, pid) in [
        // Mirror lost by a crash, journal answers.
        ("resolve_missing", None, Some(2), 0),
        // Mirror lags the journal.
        ("resolve_older", Some(1), Some(2), 0),
        // Mirror damaged by a crash that wrote nothing.
        ("resolve_damaged", Some(0), Some(2), 0),
        // Mirror ahead of the journal: the row never became durable.
        ("resolve_ahead", Some(3), Some(2), 0),
        // A rev-less mirror an older build wrote after the journal row.
        ("resolve_unversioned", Some(u64::MAX), Some(2), 0),
    ] {
        let (session, live) = saved_session(id)?;
        if let Some(rev) = journal_rev {
            append_binding(&home_path, &session, rev, &live, pid + 7)?;
        }
        match (id, mirror_rev) {
            // A crash left the file empty: a damaged mirror states nothing.
            ("resolve_damaged", _) => std::fs::write(mirror(&session)?, b"")?,
            ("resolve_missing", _) => std::fs::remove_file(mirror(&session)?)?,
            ("resolve_unversioned", _) => write_mirror(&session, None, &live, None)?,
            (_, Some(rev)) => write_mirror(&session, Some(rev), &live, None)?,
            (_, None) => {}
        }
        drop(session);

        // Then the newest binding either copy states is the one a load applies.
        let reopened = Session::load(id)?;
        let (expected_status, expected_pid) = match id {
            "resolve_missing" | "resolve_older" | "resolve_damaged" => (live.clone(), Some(pid + 7)),
            _ => (live.clone(), None),
        };
        assert_eq!(reopened.status, expected_status, "{id}: status");
        assert_eq!(reopened.last_pid, expected_pid, "{id}: owner");
    }
    Ok(())
}

/// A stale or missing mirror never fails a listing or the crash classification.
#[test]
fn listing_and_crash_scan_tolerate_a_stale_mirror() -> Result<()> {
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = cases::canonical_tempdir("horon-agent-binding-readers-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    for (id, damage) in [
        ("readers_empty", "empty"),
        ("readers_lost", "lost"),
        ("readers_stale", "stale"),
    ] {
        let (session, live) = saved_session(id)?;
        match damage {
            "empty" => std::fs::write(mirror(&session)?, b"")?,
            "lost" => std::fs::remove_file(mirror(&session)?)?,
            _ => write_mirror(&session, Some(1), &live, None)?,
        }
        let paths = store_paths()?;
        let parsed = store::SessionId::parse(id)?;
        let row = store::catalog::row(&paths, &parsed, store::ReadProbe::default())?;
        assert!(
            row.last_active_at_ms.is_some(),
            "{id}: the listing falls back to the log"
        );
        drop(session);
        // The crash scan loads each candidate; a mirror it cannot use is not a failure.
        let _ = crate::session::crash::find_recent_crashed_sessions();
    }
    Ok(())
}
