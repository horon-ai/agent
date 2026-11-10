//! Diagnostic store-only save probe. Hook intervals include intervening code,
//! not just syscalls; this is not a reconnect benchmark or an acceptance gate.
use horon_agent_message_types::{ContentBlock, Role};
use horon_agent_session_store::*;
use horon_agent_session_types::StoredMessage;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn event(
    id: &SessionId,
    parent: Option<&SealedEvent>,
    kind: KnownEventKind,
    payload: Value,
) -> Result<SealedEvent, StoreError> {
    EventDraft::new(
        id.clone(),
        EventId::new_v4(),
        parent.map(|event| event.envelope().event_id.clone()),
        OriginId::new_v4(),
        chrono::DateTime::UNIX_EPOCH,
        EventKind::new(kind),
        payload,
    )
    .seal()
}

fn run(batched: bool, trial: usize) -> Result<Value, Box<dyn std::error::Error>> {
    let root = std::env::temp_dir()
        .canonicalize()?
        .join(format!("commit-io-{}", uuid::Uuid::new_v4()));
    let paths = SessionPaths::new(root.join("sessions"));
    let id = SessionId::parse("probe")?;
    let created = event(
        &id,
        None,
        KnownEventKind::SessionCreated,
        json!({"metadata": SessionMetadata::default(), "branch_root": false}),
    )?;
    let mut seed = BlockingWriter::open(paths.clone(), id.clone())?;
    seed.enable_cache(ReadProbe::default());
    seed.append(&created)?;
    let mut head = created;
    for role in [Role::User, Role::Assistant] {
        let message = StoredMessage {
            original_input: None,
            arg_repairs: None,
            turn_start: false,
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content: vec![ContentBlock::Text {
                text: "probe".into(),
                cache_control: None,
            }],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        };
        head = event(
            &id,
            Some(&head),
            KnownEventKind::MessageAppended,
            json!({"message": message}),
        )?;
        seed.append(&head)?;
    }
    drop(seed);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&trace);
    let origin = Instant::now();
    let hook = Box::new(move |point| {
        observed
            .lock()
            .map_err(|_| std::io::Error::other("trace poisoned"))?
            .push((format!("{point:?}"), origin.elapsed().as_secs_f64() * 1000.0));
        Ok(())
    });
    let mut writer = BlockingWriter::with_hook(paths.clone(), id.clone(), hook)?;
    let open_ms = origin.elapsed().as_secs_f64() * 1000.0;
    writer.enable_cache(ReadProbe::default());
    let metadata = event(
        &id,
        Some(&head),
        KnownEventKind::MetadataChanged,
        json!({"metadata": SessionMetadata::default()}),
    )?;
    let auxiliary = event(
        &id,
        Some(&metadata),
        KnownEventKind::AuxiliaryRecorded,
        json!({"aux_kind": "legacy", "schema_version": 1, "value": {"env_snapshots": [{"reason": "resume"}]}}),
    )?;
    let before = std::fs::read(paths.events_log(&id)?)?;
    trace
        .lock()
        .map_err(|_| std::io::Error::other("trace poisoned"))?
        .clear();
    let start_ms = origin.elapsed().as_secs_f64() * 1000.0;
    let commit_start = Instant::now();
    if batched {
        writer.batched(|writer| -> Result<(), StoreError> {
            writer.append(&metadata)?;
            writer.append(&auxiliary)?;
            writer.rebuild_cache(None)
        })?;
    } else {
        writer.append(&metadata)?;
        writer.append(&auxiliary)?;
    }
    let commit_ms = commit_start.elapsed().as_secs_f64() * 1000.0;
    let end_ms = origin.elapsed().as_secs_f64() * 1000.0;
    let rows = trace
        .lock()
        .map_err(|_| std::io::Error::other("trace poisoned"))?
        .clone();
    let mut intervals = Vec::new();
    let mut previous = ("BEGIN".to_string(), start_ms);
    for next in rows.into_iter().chain([("END".to_string(), end_ms)]) {
        intervals.push(json!({"from": previous.0, "to": next.0, "elapsed_ms": next.1 - previous.1}));
        previous = next;
    }
    drop(writer);
    let after = std::fs::read(paths.events_log(&id)?)?;
    let scan = full_scan(&paths, &id)?;
    if !after.starts_with(&before) || scan.damage.is_some() || scan.events.len() != 5 {
        return Err(std::io::Error::other("probe original preservation failed").into());
    }
    std::fs::remove_dir_all(&root)?;
    Ok(json!({"batched": batched, "trial": trial, "warmup": trial == 0,
        "open_ms": open_ms, "commit_ms": commit_ms, "intervals_including_intervening_code": intervals,
        "events": scan.events.len(), "prefix_preserved": true, "root_removed": root}))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut rows = Vec::new();
    for trial in 0..6 {
        for batched in if trial % 2 == 0 { [false, true] } else { [true, false] } {
            rows.push(run(batched, trial)?);
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"scope": "store-only", "samples": rows}))?
    );
    Ok(())
}
