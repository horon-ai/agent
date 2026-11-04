//! Rebuildable auxiliary cache; messages always come from SessionCache's bounded representation.
use super::{Session, event_actor::Writer, event_store};
use anyhow::{Context, Result};
use horon_agent_session_store as store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The largest canonical record the facade keeps. A larger record is rebuilt from the event
/// log instead, so the auxiliary original is never truncated to fit a cache.
const FACADE_LIMIT: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Record {
    head: store::EventId,
    source_stamp: String,
    source_hash: String,
    active_start: usize,
    explicit_head: bool,
    prefix_usage: Option<crate::protocol::TokenUsageTotals>,
    prefix_visible: usize,
    compaction_executable: bool,
    tools: super::event_tools::ToolChain,
    auxiliary: Value,
}

fn stamp(paths: &store::SessionPaths, id: &store::SessionId) -> Result<String> {
    let meta = std::fs::metadata(paths.events_log(id)?)?;
    // Fixed-width decimal fields keep the stamp's length a function of the file's state alone.
    // The facade embeds this string in a record with a byte budget, so a clock or a log size that
    // happens to change its digit count must not move the publish decision at the limit.
    // Comparison is plain string equality, so padding changes no meaning.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!(
            "{}:{}:{:012}:{}:{:09}:{}:{:09}",
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec()
        ))
    }
    #[cfg(not(unix))]
    {
        let modified = meta.modified()?.duration_since(std::time::UNIX_EPOCH)?;
        Ok(format!(
            "{:012}:{:012}:{:09}",
            meta.len(),
            modified.as_secs(),
            modified.subsec_nanos()
        ))
    }
}

/// UTF-8 bytes of every string in `value`, which bounds its canonical JSON text from below:
/// string contents are copied verbatim and escapes only add bytes. Walking the value allocates
/// nothing, so a payload past the limit is rejected before any serialization happens.
fn string_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        Value::Array(items) => items.iter().map(string_bytes).sum(),
        Value::Object(map) => map.values().map(string_bytes).sum(),
        _ => 0,
    }
}

/// The auxiliary is the record's only unbounded field; the rest are identifiers, counters and a
/// tool chain of ids. Its string bytes are therefore a lower bound for the whole record, which
/// is enough to reject an oversized one before it is serialized.
fn record_of(
    session: &Session,
    paths: &store::SessionPaths,
    id: &store::SessionId,
    auxiliary: Value,
) -> Result<Record> {
    Ok(Record {
        head: session.event_state.head.clone().context("cache head missing")?,
        source_stamp: stamp(paths, id)?,
        source_hash: session.event_state.source_hash.clone().context("source hash missing")?,
        active_start: session.event_state.active_start,
        explicit_head: session.event_state.explicit_head,
        prefix_usage: session.event_state.prefix_usage,
        prefix_visible: session.event_state.prefix_visible,
        compaction_executable: session.event_state.compaction_executable,
        tools: session.event_state.tools.clone(),
        auxiliary,
    })
}

fn remove_file(file: &Path) -> Result<()> {
    match std::fs::remove_file(file) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

// Per-thread counts of the two decisions the oversized path makes, so a test can prove the record
// was rejected by the string bound before serialization instead of trusting a wall-clock reading.
// The canonical-pass count alone cannot guard that: it stays zero whether the early bound or the
// counted length rejected the record, so the early bound could be deleted without a failure.
#[cfg(test)]
thread_local! {
    static CANONICAL_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static EARLY_REJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_canonical_pass() {
    CANONICAL_PASSES.with(|passes| passes.set(passes.get() + 1));
}

#[cfg(test)]
fn note_early_rejection() {
    EARLY_REJECTIONS.with(|rejections| rejections.set(rejections.get() + 1));
}

#[cfg(test)]
fn canonical_passes() -> usize {
    CANONICAL_PASSES.with(std::cell::Cell::get)
}

#[cfg(test)]
fn early_rejections() -> usize {
    EARLY_REJECTIONS.with(std::cell::Cell::get)
}

/// Length of the record's canonical JSON without building it: the same bytes, counted through a
/// sink instead of a `String`. Field order does not change the total length, so this equals
/// `canonical_json` of the same record. A record that cannot serialize surfaces as an error, so a
/// malformed auxiliary fails the publish instead of looking like an empty record.
fn serialized_len<T: serde::Serialize>(value: &T) -> Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(buffer.len());
            Ok(buffer.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).context("facade record cannot be serialized")?;
    Ok(counter.0)
}

/// Drops a derived cache file that no longer describes this session, so a record that grew past
/// the limit is not left behind as a stale entry for the state it used to hold.
fn drop_cache(session: &Session, file: PathBuf) -> Result<()> {
    if let Some(writer) = &session.event_state.writer {
        writer.call(move |_| remove_file(&file))?;
    } else {
        remove_file(&file)?;
    }
    Ok(())
}

pub(super) fn publish(session: &Session) -> Result<()> {
    if session.persist_state.transcript_stripped || session.event_state.auxiliary_stripped {
        return Ok(());
    }
    let paths = session
        .event_state
        .paths
        .clone()
        .context("cache session paths missing")?;
    let id = store::SessionId::parse(&session.id)?;
    let file = paths.session_dir(&id)?.join("facade-cache.json");
    let auxiliary = session.auxiliary_value();
    // Large auxiliary originals remain queryable; never truncate them into a cache.
    if string_bytes(&auxiliary) > FACADE_LIMIT {
        #[cfg(test)]
        note_early_rejection();
        return drop_cache(session, file);
    }
    let record = record_of(session, &paths, &id, auxiliary)?;
    // Counting the bytes walks the record but allocates no copy of it, so an oversized payload
    // costs a scan and never the megabyte string plus value tree it would have become.
    if serialized_len(&record)? > FACADE_LIMIT {
        return drop_cache(session, file);
    }
    let payload = serde_json::to_value(record)?;
    #[cfg(test)]
    note_canonical_pass();
    let bytes = store::canonical_json(&payload)?;
    if bytes.len() > FACADE_LIMIT {
        return drop_cache(session, file);
    }
    let encoded = json!({"payload":payload,"sha256":store::sha256_hex(bytes.as_bytes())});
    if let Some(writer) = &session.event_state.writer {
        writer.call(move |_| event_store::publish_local(&file, &encoded))?;
    } else {
        event_store::publish_local(&file, &encoded)?;
    }
    Ok(())
}

pub(super) fn restore(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Option<Session>> {
    let path = paths.session_dir(id)?.join("facade-cache.json");
    let bytes = match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 2 * 1024 * 1024 => bytes,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    let Some(payload) = value.get("payload") else {
        return Ok(None);
    };
    if value["sha256"].as_str() != Some(&store::sha256_hex(store::canonical_json(payload)?.as_bytes())) {
        return Ok(None);
    }
    let Ok(record) = serde_json::from_value::<Record>(payload.clone()) else {
        return Ok(None);
    };
    if record.source_stamp != stamp(paths, id)?
        || event_store::selected(paths, id)?.is_some_and(|head| head != record.head)
    {
        return Ok(None);
    }
    // The durable flag carries the selection intent: choosing the current head explicitly
    // changes only that flag, so a cached payload must never restore a stale value.
    let explicit_head = event_store::explicit_flag(paths, id)?.unwrap_or(record.explicit_head);
    let selected = explicit_head.then_some(&record.head);
    let cache = store::checkpoint::SessionCache::new(paths.clone(), store::ReadProbe::default());
    let recent = match cache.recent(id, selected) {
        Ok(recent) => recent,
        Err(error) if error.code == store::StoreErrorCode::Busy => {
            let writer = Writer::open(paths.clone(), id.clone())?;
            let selected = selected.cloned();
            writer.call(move |writer| Ok(writer.refresh_cache(selected.as_ref(), store::ReadProbe::default())?))?
        }
        Err(error) => return Err(error.into()),
    };
    if recent.head != record.head {
        return Ok(None);
    }
    let source_metadata = recent.metadata.clone();
    let mut value = serde_json::to_value(&recent.metadata)?;
    let object = value.as_object_mut().context("invalid metadata")?;
    object.remove("legacy_extensions");
    object.extend(recent.metadata.legacy_extensions);
    object.insert("id".into(), json!(id.as_str()));
    object.insert(
        "messages".into(),
        json!(recent.tail.iter().map(|m| &m.message).collect::<Vec<_>>()),
    );
    for key in [
        "env_snapshots",
        "memory_injections",
        "replay_events",
        "pruned_tool_results",
        "stream_rule_ledger",
        "compaction",
        "is_debug",
    ] {
        if let Some(value) = record.auxiliary.get(key) {
            object.insert(key.into(), value.clone());
        }
    }
    object.insert("status".into(), json!(super::SessionStatus::Closed));
    object.insert("last_pid".into(), Value::Null);
    object.insert("last_active_at".into(), Value::Null);
    let mut session: Session = serde_json::from_value(value)?;
    if let Some((_, payload)) = &recent.compaction {
        session.restore_event_compaction(payload, (record.active_start, record.active_start + recent.tail.len()));
    }
    session.event_state = event_store::EventState {
        source_metadata: Some(source_metadata),
        paths: Some(paths.clone()),
        bound_id: Some(id.as_str().into()),
        head: Some(record.head),
        source_hash: Some(record.source_hash),
        active_start: record.active_start,
        explicit_head,
        prefix_usage: record.prefix_usage,
        prefix_visible: record.prefix_visible,
        compaction_executable: record.compaction_executable,
        tools: record.tools,
        auxiliary: json!({"compaction":session.compaction}),
        ..Default::default()
    };
    session.event_state.identity = Some(session.route_identity());
    session.restore_local_runtime(paths, id)?;
    session.event_state.local_binding = Some(super::event_wire::LocalRuntime::capture(&session));
    session.refresh_agent_model_overrides()?;
    session.event_state.metadata = Some(session.event_metadata()?);
    session.reset_persist_state(true);
    session.reset_provider_messages_cache();
    session.mark_memory_profile_dirty();
    session.event_state.auxiliary_digest = Some(store::sha256_hex(
        store::canonical_json(&session.auxiliary_value())?.as_bytes(),
    ));
    Ok(Some(session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, Role};
    use crate::session::tests::{EnvVarGuard, cases::canonical_tempdir, lock_env};
    use std::time::Instant;

    fn text(value: &str) -> Vec<ContentBlock> {
        vec![ContentBlock::Text {
            text: value.into(),
            cache_control: None,
        }]
    }

    /// The fits below compare the record's exact canonical length, so the fixture must not carry
    /// a live clock reading: chrono trims trailing zeros in the fraction, which changes how many
    /// bytes this snapshot adds and moves the target the fit is converging on.
    fn snapshot(session_id: &str, pad: usize) -> crate::session::EnvSnapshot {
        crate::session::EnvSnapshot {
            captured_at: chrono::DateTime::from_timestamp(1_700_000_000, 123_456_789).expect("fixed instant"),
            reason: "p".repeat(pad),
            session_id: session_id.to_string(),
            working_dir: None,
            provider: "openai".to_string(),
            model: "gpt-5.4".to_string(),
            horon_agent_version: "test".to_string(),
            horon_agent_git_hash: None,
            horon_agent_git_dirty: None,
            os: "macos".to_string(),
            arch: "arm64".to_string(),
            pid: 1,
            is_selfdev: false,
            is_debug: false,
            is_canary: false,
            testing_build: None,
            working_git: None,
        }
    }

    fn new_session(id: &str) -> Session {
        let mut session = Session::create_with_id(id.to_string(), None, None);
        session.add_message(Role::User, text("facade threshold probe"));
        session
    }

    fn set_pad(session: &mut Session, pad: usize) {
        let id = session.id.clone();
        session.env_snapshots.clear();
        session.record_env_snapshot(snapshot(&id, pad));
    }

    fn cache_file(session: &Session) -> Result<PathBuf> {
        let paths = session
            .event_state
            .paths
            .clone()
            .context("cache session paths missing")?;
        let id = store::SessionId::parse(&session.id)?;
        Ok(paths.session_dir(&id)?.join("facade-cache.json"))
    }

    /// The exact canonical size the publish decision reads, measured rather than assumed.
    fn record_canonical_len(session: &Session) -> Result<usize> {
        let paths = session
            .event_state
            .paths
            .clone()
            .context("cache session paths missing")?;
        let id = store::SessionId::parse(&session.id)?;
        let record = record_of(session, &paths, &id, session.auxiliary_value())?;
        let payload = serde_json::to_value(record)?;
        Ok(store::canonical_json(&payload)?.len())
    }

    fn restored_pad(session: &Session) -> usize {
        session
            .auxiliary_value()
            .get("env_snapshots")
            .and_then(|snapshots| snapshots.get(0))
            .and_then(|snapshot| snapshot.get("reason"))
            .and_then(Value::as_str)
            .map(str::len)
            .unwrap_or(0)
    }

    /// Grows the auxiliary until the canonical record is exactly `target` bytes. The pad is a
    /// plain run of ASCII, so each correction lands in one step once the fixed overhead is known.
    fn fit_record(mut session: Session, target: usize) -> Result<(Session, usize)> {
        let mut pad = target.saturating_sub(1024);
        for _ in 0..4 {
            set_pad(&mut session, pad);
            session.save()?;
            let measured = record_canonical_len(&session)?;
            if measured == target {
                return Ok((session, pad));
            }
            let corrected = if measured > target {
                pad.saturating_sub(measured - target)
            } else {
                pad + (target - measured)
            };
            if corrected == pad {
                break;
            }
            pad = corrected;
        }
        anyhow::bail!("could not size the facade record to {target} bytes")
    }

    #[test]
    fn counted_record_length_equals_its_canonical_length() -> Result<()> {
        let _lock = lock_env();
        let (_home_dir, home_path) = canonical_tempdir("horon-agent-facade-length-")?;
        let _home = EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
        let mut session = new_session("facade_length");
        set_pad(&mut session, 4096);
        session.save()?;
        let paths = session.event_state.paths.clone().context("paths")?;
        let id = store::SessionId::parse(&session.id)?;
        let record = record_of(&session, &paths, &id, session.auxiliary_value())?;
        assert_eq!(
            serialized_len(&record)?,
            record_canonical_len(&session)?,
            "the counted length must be the canonical length, or the threshold decision is wrong"
        );
        Ok(())
    }

    #[test]
    fn counted_record_length_reports_an_unserializable_record() {
        struct Unserializable;
        impl serde::Serialize for Unserializable {
            fn serialize<S>(&self, _serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(serde::ser::Error::custom("intentional failure"))
            }
        }
        let error = serialized_len(&Unserializable).expect_err("a record that cannot serialize must not look empty");
        assert!(
            error.to_string().contains("cannot be serialized"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn facade_caches_just_under_the_limit_and_skips_just_over_it() -> Result<()> {
        let _lock = lock_env();
        let (_home_dir, home_path) = canonical_tempdir("horon-agent-facade-threshold-")?;
        let _home = EnvVarGuard::set("HORON_AGENT_HOME", &home_path);

        let (under, under_pad) = fit_record(new_session("facade_under"), FACADE_LIMIT - 1)?;
        assert_eq!(record_canonical_len(&under)?, FACADE_LIMIT - 1);
        assert!(
            cache_file(&under)?.exists(),
            "a record one byte under the limit must be cached"
        );

        let (over, _over_pad) = fit_record(new_session("facade_over"), FACADE_LIMIT + 1)?;
        assert_eq!(record_canonical_len(&over)?, FACADE_LIMIT + 1);
        assert!(
            !cache_file(&over)?.exists(),
            "a record one byte over the limit must not be cached"
        );

        // The oversized record costs no canonical pass at all. This case sits a margin over the
        // limit so the auxiliary's string bytes alone stay under it: the counted length is what
        // has to reject this record, and the early string bound must stay out of the way.
        let (near, near_pad) = fit_record(new_session("facade_near"), FACADE_LIMIT + 256)?;
        let canonical_before = canonical_passes();
        let early_before = early_rejections();
        let mut near_again = near;
        near_again.save()?;
        let counted_passes = canonical_passes() - canonical_before;
        let counted_early = early_rejections() - early_before;
        assert_eq!(
            counted_passes, 0,
            "an oversized record must be rejected before serialization"
        );
        assert_eq!(
            counted_early, 0,
            "this record's string bytes are under the limit, so the counted length is what must reject it"
        );

        // A record whose auxiliary string bytes alone exceed the limit is the case the early bound
        // exists for. The counted length would reject it too, so the early-rejection counter is the
        // only thing that proves it never reached the walk - and deleting the bound must fail here.
        let mut bulky = new_session("facade_bulky");
        set_pad(&mut bulky, FACADE_LIMIT + 4096);
        assert!(
            string_bytes(&bulky.auxiliary_value()) > FACADE_LIMIT,
            "this case must be oversized in string bytes, not only in canonical bytes"
        );
        let bulky_before = canonical_passes();
        let early_before = early_rejections();
        bulky.save()?;
        let bulky_early = early_rejections() - early_before;
        let bulky_passes = canonical_passes() - bulky_before;
        assert_eq!(
            bulky_early, 1,
            "an oversized record must be rejected by the string bound before it is serialized"
        );
        assert_eq!(
            bulky_passes, 0,
            "an oversized record must be rejected before serialization"
        );
        assert!(
            !cache_file(&bulky)?.exists(),
            "a record past the string bound must not be cached"
        );

        let cached_before = canonical_passes();
        let mut under_again = under.clone();
        under_again.save()?;
        let cached_passes = canonical_passes() - cached_before;
        println!(
            "facade canonical passes: {cached_passes} at the cached pad, {counted_passes} at the counted-length pad, \
             {bulky_passes} at the string-bound pad ({bulky_early} early rejection)"
        );

        let warm_started = Instant::now();
        let warm = Session::load(&under.id)?;
        let warm_micros = warm_started.elapsed().as_micros();
        let cold_started = Instant::now();
        let cold = Session::load(&near_again.id)?;
        let cold_micros = cold_started.elapsed().as_micros();
        println!(
            "facade record {under_pad}-byte pad / {} canonical bytes: cached, restore {warm_micros} us; \
             near-limit record: skipped, restore {cold_micros} us",
            record_canonical_len(&under)?,
        );

        // The oversized auxiliary is not truncated: it comes back whole from the event log.
        assert_eq!(restored_pad(&warm), under_pad);
        assert_eq!(restored_pad(&cold), near_pad);
        Ok(())
    }

    #[test]
    fn facade_drops_the_entry_when_the_auxiliary_grows_past_the_limit() -> Result<()> {
        let _lock = lock_env();
        let (_home_dir, home_path) = canonical_tempdir("horon-agent-facade-stale-")?;
        let _home = EnvVarGuard::set("HORON_AGENT_HOME", &home_path);

        let (small, _) = fit_record(new_session("facade_stale"), FACADE_LIMIT / 2)?;
        assert!(cache_file(&small)?.exists(), "the small record must be cached");

        let (grown, grown_pad) = fit_record(small, FACADE_LIMIT + 1)?;
        assert!(
            !cache_file(&grown)?.exists(),
            "the entry for the replaced auxiliary must be dropped, not left stale"
        );

        let loaded = Session::load(&grown.id)?;
        assert_eq!(
            restored_pad(&loaded),
            grown_pad,
            "a replaced auxiliary must never be served from the previous entry"
        );
        Ok(())
    }

    /// A restore rebuilds this session's own checkpoint even while another owner
    /// holds the shared catalog: the listing row is deferred and stays
    /// discoverable, and it never turns a valid per-session read into a failure.
    #[test]
    fn restore_rebuilds_a_missing_checkpoint_while_the_catalog_is_owned_elsewhere() -> Result<()> {
        let _lock = lock_env();
        let (_home_dir, home_path) = canonical_tempdir("horon-agent-catalog-contended-")?;
        let _home = EnvVarGuard::set("HORON_AGENT_HOME", &home_path);
        let mut session = new_session("catalog_contended_restore");
        session.save()?;
        let session_id = session.id.clone();
        let paths = session
            .event_state
            .paths
            .clone()
            .context("cache session paths missing")?;
        let id = store::SessionId::parse(&session_id)?;
        let directory = paths.session_dir(&id)?;
        let expected = session.event_state.head.clone().context("cache head missing")?;
        // Given a session whose derived checkpoint is gone, and a catalog owned by
        // another process.
        std::fs::remove_file(directory.join("checkpoint.json"))?;
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.root().join(".catalog.lock"))?;
        held.try_lock().expect("hold the shared catalog");
        drop(session);
        // When the session is restored while the shared catalog is contended.
        let restored = Session::load(&session_id)?;
        // Then the per-session read succeeds from the checkpoint it rebuilt itself.
        assert_eq!(restored.history_messages()?.len(), 1);
        assert_eq!(restored.event_state.head.as_ref(), Some(&expected));
        assert!(
            directory.join("checkpoint.json").is_file(),
            "the per-session checkpoint must be published under catalog contention"
        );
        // And the listing row it could not publish stays due, not forgotten.
        assert!(paths.root().join(".catalog.dirty").join(id.as_str()).is_file());
        drop(held);
        // Then a released catalog publishes the due row and clears the marker.
        let rows = store::catalog::list(&paths, store::ReadProbe::default())?;
        assert_eq!(rows.len(), 1);
        assert!(!paths.root().join(".catalog.dirty").join(id.as_str()).exists());
        Ok(())
    }
}
