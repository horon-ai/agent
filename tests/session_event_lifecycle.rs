//! Real-process session lifecycle: original log -> API -> restart -> replica.
//!
//! Every leg crosses a spawned `horon-agent serve` daemon and a spawned
//! `api-bridge` over real Unix sockets in two separate HOMEs, and the durable
//! projection is read from the store files rather than from memory.
#![cfg(unix)]
#![allow(clippy::await_holding_lock)]

#[path = "support/lifecycle.rs"]
mod lifecycle;

use horon_agent::session::event_store_api as store;
use horon_agent_sdk::{ApiEvent, ErrorKind, HoronAgentClient};
use lifecycle::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(180);

fn run_turn(client: &HoronAgentClient, session_id: &str, text: &str) -> String {
    let stream = client.events(Some(session_id));
    client
        .send_message(session_id, text, Vec::new(), None)
        .expect("send_message");
    let end = Instant::now() + WAIT;
    let mut streamed = String::new();
    while let Some(remaining) = end.checked_duration_since(Instant::now()) {
        let Some(event) = stream.next_timeout(remaining) else {
            panic!("turn for {text:?} never finished");
        };
        match event {
            ApiEvent::TextDelta { text, .. } => streamed.push_str(&text),
            ApiEvent::TurnDone { .. } => return streamed,
            ApiEvent::Error { message, .. } => panic!("turn for {text:?} failed: {message}"),
            _ => {}
        }
    }
    panic!("turn for {text:?} exceeded {WAIT:?}");
}

fn milestone(step: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("T17_STEP {} at {}ms", step, start.elapsed().as_millis());
}

fn context_messages(client: &HoronAgentClient, session_id: &str, count: usize, label: &str) {
    for index in 0..count {
        let body = format!("{label} {index}: {}", "lorem ipsum dolor sit amet ".repeat(60));
        client
            .send_context_message(session_id, &body)
            .expect("send_context_message");
    }
}

fn create_session(client: &HoronAgentClient, root: &Path) -> String {
    client
        .create_session(Some(root.display().to_string()))
        .expect("create_session")
        .session_id
}

#[test]
fn original_survives_api_restart_and_replica_roundtrip() {
    let mut fx = Lifecycle::start("happy");
    let a = fx.a.client();
    let b = fx.b.client();
    assert!(a.supports("session_event_store_v1"));
    assert!(a.supports("session_replication_v1"));
    assert!(a.supports("session_history_cursor_v1"));

    let session = create_session(&a, &fx.root);
    milestone("created");
    assert_eq!(run_turn(&a, &session, "plain lifecycle message"), PLAIN_TEXT);
    milestone("plain turn");
    assert_eq!(
        run_turn(&a, &session, TOOL_PROMPT),
        TOOL_FINAL_TEXT,
        "the scripted tool turn completes with the post-tool reply"
    );
    assert!(
        fx.provider.requests().iter().any(|body| body.contains(TOOL_COMMAND)),
        "the tool result is replayed to the provider"
    );
    context_messages(&a, &session, 10, "pre-rewind context");
    milestone("pre-rewind context pushed");

    let heads = a.list_session_heads(&session).expect("heads").heads;
    assert_eq!(heads.len(), 1, "no divergence before the rewind");
    let full = store_projection(&fx.a.paths(), &session, Some(&heads[0]));
    assert!(
        full.messages.iter().any(|row| row.text.contains(TOOL_FINAL_TEXT)),
        "the tool turn is in the originals"
    );
    let rendered = a.get_history(&session).expect("history").len();
    assert!(rendered >= 4, "the transcript has rewind targets");
    let events_before_rewind = event_ids(&fx.a.paths(), &session);

    a.rewind(&session, rendered / 2)
        .expect("rewind to the middle transcript entry");
    milestone("rewound");
    let rendered_after = a.get_history(&session).expect("history after rewind").len();
    assert!(
        rendered_after < rendered,
        "the rewind shortens the visible transcript: {rendered_after} of {rendered}"
    );
    assert!(
        contains_all(&events_before_rewind, &event_ids(&fx.a.paths(), &session)),
        "the rewind appends its branch pointer and deletes no original"
    );
    let after_rewind = branches(&fx.a.paths(), &session);
    assert_eq!(
        after_rewind.len(),
        2,
        "the rewind keeps the abandoned branch and starts the selected one: {after_rewind:?}"
    );
    let abandoned = after_rewind
        .iter()
        .find(|(_, view)| view.messages == full.messages)
        .unwrap_or_else(|| panic!("the abandoned branch renders every original: {after_rewind:?}"));
    let selected = after_rewind
        .iter()
        .find(|(_, view)| view.messages.len() < full.messages.len())
        .unwrap_or_else(|| panic!("the selected branch renders the rewound prefix: {after_rewind:?}"));
    assert!(
        full.messages.starts_with(&selected.1.messages),
        "the selected branch is the preserved prefix of the abandoned branch"
    );

    assert_eq!(run_turn(&a, &session, "turn on the rewound branch"), PLAIN_TEXT);
    milestone("branch turn");
    context_messages(&a, &session, 10, "post-rewind context");
    milestone("post-rewind context pushed");
    let after_append = branches(&fx.a.paths(), &session);
    assert_eq!(
        after_append.len(),
        2,
        "appending to the rewound branch does not overwrite the abandoned one: {after_append:?}"
    );
    let abandoned_after = after_append
        .iter()
        .find(|(head, view)| head == &abandoned.0 && view.messages == full.messages)
        .unwrap_or_else(|| panic!("the abandoned branch is unchanged: {after_append:?}"));
    let branch_after = after_append
        .iter()
        .find(|(head, _)| head != &abandoned_after.0)
        .expect("the rewound branch head");
    assert!(
        branch_after.1.messages.starts_with(&selected.1.messages),
        "the rewound branch keeps its prefix and gains the new turn"
    );
    assert!(
        branch_after.1.messages.len() > selected.1.messages.len(),
        "the new turn is appended to the rewound branch"
    );

    a.rename_session(&session, Some("lifecycle title".to_string()))
        .expect("rename");
    a.archive_session(&session).expect("archive");
    let fork = a.fork_session(&session).expect("fork").session_id;
    milestone("renamed, archived, forked");

    a.compact(&session).expect("compact accepted");
    milestone("compact accepted");
    let summary =
        wait_for_compaction(&fx.a.paths(), &session, WAIT).unwrap_or_else(|error| panic!("{error}\n{}", fx.a.logs()));
    milestone("compaction durable");
    assert!(
        summary.contains(SUMMARY_GOAL),
        "the scripted summary is persisted: {summary}"
    );
    assert_eq!(run_turn(&a, &session, "turn reading the compacted context"), PLAIN_TEXT);
    milestone("post-compaction turn");
    let request = fx
        .provider
        .wait_for_request("Previous Conversation Summary", WAIT)
        .expect("the compacted summary reaches the provider");
    assert!(
        request.contains(SUMMARY_GOAL),
        "the provider request carries the scripted summary text"
    );

    let before_compaction_records = branches(&fx.a.paths(), &session);
    let compacted = before_compaction_records
        .iter()
        .find(|(_, view)| view.compaction.is_some())
        .unwrap_or_else(|| panic!("a branch carries the Compacted record: {before_compaction_records:?}"));
    assert!(
        compacted.1.messages.starts_with(&branch_after.1.messages),
        "compaction keeps every original message before the new turn"
    );
    assert_eq!(
        compacted.1.messages.len(),
        branch_after.1.messages.len() + 2,
        "only the post-compaction turn is appended"
    );
    assert_eq!(compacted.1.title.as_deref(), Some("lifecycle title"));
    assert_eq!(
        before_compaction_records.len(),
        2,
        "both branches survive the compaction"
    );

    let page = export_all(&a, &session);
    let fork_page = export_all(&a, &fork);
    let kinds = page_kinds(&page);
    assert!(
        kinds.iter().any(|kind| kind == "compacted"),
        "the exported originals carry the Compacted record: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "metadata_changed"),
        "the rename is a MetadataChanged record: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "archive_changed"),
        "the SDK archive is a durable ArchiveChanged record: {kinds:?}"
    );
    assert_eq!(page.heads.len(), 2, "the export fixes both retained heads");
    import_all(&b, &a, &page);
    import_all(&b, &a, &fork_page);
    milestone("imported");

    let source_before_restart = branches(&fx.a.paths(), &session);
    fx.a.restart();
    fx.b.restart();
    milestone("restarted");
    let b = fx.b.client();

    let source = branches(&fx.a.paths(), &session);
    assert_eq!(
        source, source_before_restart,
        "the restart does not change the originals"
    );
    let replica = branches(&fx.b.paths(), &session);
    assert_eq!(
        replica, source,
        "every retained head renders the same projection on the replica"
    );
    assert_eq!(
        page_pairs(&export_all(&b, &session)),
        page_pairs(&page),
        "canonical bytes survive the roundtrip unchanged"
    );

    let fork_replica = store_projection(&fx.b.paths(), &fork, None);
    let fork_source = store_projection(&fx.a.paths(), &fork, None);
    assert_eq!(fork_replica.messages, fork_source.messages);
    assert_eq!(
        fork_replica.title, fork_source.title,
        "the fork keeps the parent's title"
    );

    assert!(
        compacted.1.archived,
        "the archived branch keeps its record through compaction and the restart"
    );
    assert_eq!(
        source.iter().filter(|(_, view)| view.archived).count(),
        1,
        "exactly one retained head carries the ArchiveChanged record: {source:?}"
    );
    assert_eq!(
        replica.iter().filter(|(_, view)| view.archived).count(),
        1,
        "the replica projects the archived branch from the replicated record: {replica:?}"
    );
    for (label, home) in [("source", &fx.a.home), ("replica", &fx.b.home)] {
        let hint = std::fs::read_to_string(home.join("sdk-archive.json")).unwrap_or_default();
        assert!(
            !hint.contains(&session),
            "{label}: a canonical session records visibility in its event log, not the host-local hint file: {hint}"
        );
    }

    let replica_full = replica
        .iter()
        .find(|(_, view)| view.messages.iter().any(|row| row.text.contains(TOOL_FINAL_TEXT)))
        .expect("the replica holds the branch with the tool turn")
        .0
        .clone();
    let refused = fx.b.client().attach_session(&session);
    assert!(
        refused.is_err(),
        "a session with two retained heads refuses execution until one is selected"
    );
    let page = b
        .get_history_page(&session, Some(&replica_full), None, Some(200))
        .expect("the replica serves the read-only page path");
    assert!(
        page.messages.iter().any(|row| row.content.contains(TOOL_FINAL_TEXT)),
        "the replica renders the tool turn's assistant reply from the imported originals"
    );
    assert!(!page.messages.is_empty(), "the replica serves the branch it imported");
}

#[test]
fn child_exit_before_and_after_ack_loses_no_original() {
    let mut fx = Lifecycle::start("ack");
    let a = fx.a.client();
    let session = create_session(&a, &fx.root);

    let acked = "message acknowledged before the crash";
    assert_eq!(run_turn(&a, &session, "acked turn"), PLAIN_TEXT);
    a.send_context_message(&session, acked).expect("context ack");
    let durable = event_ids(&fx.a.paths(), &session);
    let acked_view = store_projection(&fx.a.paths(), &session, None).messages;
    drop(a);

    fx.a.kill_hard();
    assert_eq!(
        parsable_log_lines(&fx.a.paths(), &session),
        raw_log_lines(&fx.a.paths(), &session).len(),
        "a hard kill after the acknowledgement leaves only complete records"
    );
    fx.a.restart();
    assert!(
        contains_all(&durable, &event_ids(&fx.a.paths(), &session)),
        "an acknowledged message is durable across the crash"
    );
    assert_eq!(
        store_projection(&fx.a.paths(), &session, None).messages,
        acked_view,
        "the acknowledged projection survives the crash"
    );

    let a = fx.a.client();
    a.attach_session(&session).expect("reattach after the restart");
    fx.provider.hold_turn();
    let stream = a.events(Some(&session));
    a.send_message(&session, HOLD_TURN_PROMPT, Vec::new(), None)
        .expect("send held turn");
    fx.provider
        .wait_for_request(HOLD_TURN_PROMPT, WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", fx.a.logs()));
    let before_kill = event_ids(&fx.a.paths(), &session);
    let in_flight = store_projection(&fx.a.paths(), &session, None).messages;

    fx.a.kill_hard();
    drop(stream);
    fx.provider.release_turn();

    assert_eq!(
        parsable_log_lines(&fx.a.paths(), &session),
        raw_log_lines(&fx.a.paths(), &session).len(),
        "killing the daemon mid-turn leaves no partial record"
    );
    fx.a.restart();
    assert!(
        contains_all(&before_kill, &event_ids(&fx.a.paths(), &session)),
        "no acknowledged event is lost: {} before, {} after",
        before_kill.len(),
        event_ids(&fx.a.paths(), &session).len()
    );
    let recovered = store_projection(&fx.a.paths(), &session, None).messages;
    assert!(
        recovered.starts_with(&in_flight),
        "the crash leaves every already-recorded original in place, in order"
    );
    let extra: Vec<&StoredMessage> = recovered[in_flight.len()..].iter().collect();
    for row in &extra {
        assert!(
            row.text.contains(PLAIN_TEXT),
            "the only rows added after the crash are complete scripted replies, never a torn record: {row:?}"
        );
    }
    assert!(
        extra.len() <= 1,
        "the interrupted turn is recorded at most once: {}",
        extra.len()
    );

    let a = fx.a.client();
    a.attach_session(&session).expect("reattach after the crash");
    let page = export_all(&a, &session);
    assert!(
        page.events.len() >= recovered.len(),
        "the recovered session still exports every event"
    );
    assert_eq!(
        run_turn(&a, &session, "turn after the recovery"),
        PLAIN_TEXT,
        "the recovered daemon accepts a new turn"
    );
}

#[test]
fn partial_blob_is_refused_until_the_transfer_completes() {
    let fx = Lifecycle::start("blob");
    let a = fx.a.client();
    let b = fx.b.client();
    let session = store::SessionId::parse("t17_partial_blob").expect("session id");
    let mut writer = store::BlockingWriter::open(fx.a.paths(), session.clone()).expect("writer");
    let root = append_root(&mut writer, &session);
    let big = "b".repeat(9 * 1024 * 1024);
    let event = store::EventDraft::new(
        session.clone(),
        store::EventId::new_v4(),
        Some(root),
        store::OriginId::new_v4(),
        store::SessionMetadata::default().created_at,
        store::EventKind::new(store::KnownEventKind::MessageAppended),
        json!(text_message("big", "user", &big)),
    )
    .seal()
    .expect("big message");
    assert!(
        matches!(event, store::SealedEvent::Blob { .. }),
        "a 9 MiB message externalizes its payload"
    );
    writer.append(&event).expect("append big message");
    drop(writer);

    let id = session.as_str().to_owned();
    let page = export_all(&a, &id);
    let reference = page.blob_refs.first().expect("a payload blob reference").clone();
    let chunk = a
        .export_session_blob(&id, &reference.sha256, reference.size, 0)
        .expect("first chunk");
    assert!(!chunk.complete, "9 MiB needs several chunks");
    b.import_session_blob(&id, &chunk).expect("partial chunk");

    let refused = b
        .import_session_events(&page, None)
        .expect_err("an incomplete blob refuses the referencing events");
    assert_eq!(
        refused.kind,
        ErrorKind::Harness(horon_agent_sdk::api::ErrorCode::InvalidRequest)
    );
    assert_eq!(
        refused.details.expect("typed detail").store_code,
        "missing_dependency",
        "the refusal names the missing blob"
    );
    let log = fx.b.paths().events_log(&session).expect("log path");
    assert!(
        !log.exists() || event_ids(&fx.b.paths(), &id).is_empty(),
        "no event referencing the incomplete blob is accepted"
    );

    let mut offset = chunk.next_offset;
    while offset < reference.size {
        let next = a
            .export_session_blob(&id, &reference.sha256, reference.size, offset)
            .expect("next chunk");
        b.import_session_blob(&id, &next).expect("complete chunk");
        offset = next.next_offset;
        if next.complete {
            break;
        }
    }
    assert!(
        b.session_blob_status(&id, &reference.sha256, reference.size)
            .expect("blob status")
            .complete,
        "the transfer completes"
    );
    b.import_session_events(&page, None)
        .expect("the completed blob unblocks the events");
    let replica = store_projection(&fx.b.paths(), &id, None);
    assert_eq!(
        replica.messages.len(),
        1,
        "the session root carries no message; only the big message does"
    );
    assert_eq!(replica.messages[0].text.len(), 9 * 1024 * 1024, "the payload is intact");
    assert_eq!(replica.messages, store_projection(&fx.a.paths(), &id, None).messages);
}

#[test]
fn stale_compaction_job_is_discarded_and_originals_kept() {
    let fx = Lifecycle::start("stale");
    let a = fx.a.client();
    let session = create_session(&a, &fx.root);
    context_messages(&a, &session, 10, "stale context");
    let head0 = a.list_session_heads(&session).expect("heads").heads[0].clone();
    let full = store_projection(&fx.a.paths(), &session, Some(&head0));
    let before_events = event_ids(&fx.a.paths(), &session);

    fx.provider.hold_summary();
    a.compact(&session).expect("compact accepted");
    fx.provider
        .wait_for_request(SUMMARY_ROUTE_KEY, WAIT)
        .expect("the summary request is in flight");

    let rendered = a.get_history(&session).expect("history").len();
    a.rewind(&session, rendered / 2)
        .expect("rewind while the job is in flight");
    let rendered_after = a.get_history(&session).expect("rewound history").len();
    assert!(
        rendered_after < rendered,
        "the rewind took effect while the job was in flight: {rendered_after} of {rendered}"
    );
    let rewound_prefix = branches(&fx.a.paths(), &session)
        .into_iter()
        .min_by_key(|(_, view)| view.messages.len())
        .expect("the rewound branch");
    fx.provider.release_summary();

    assert_eq!(run_turn(&a, &session, "turn after the stale job"), PLAIN_TEXT);
    let views = branches(&fx.a.paths(), &session);
    assert_eq!(views.len(), 2, "the rewound branch and the abandoned head remain");
    assert!(
        views.iter().all(|(_, view)| view.compaction.is_none()),
        "a job captured before the rewind is not applied to the rewound branch"
    );
    assert!(
        contains_all(&before_events, &event_ids(&fx.a.paths(), &session)),
        "every pre-job event is still in the log"
    );
    let abandoned = views
        .iter()
        .find(|(_, view)| view.messages == full.messages)
        .unwrap_or_else(|| panic!("the abandoned branch renders every pre-job message: {views:?}"));
    let branch = views
        .iter()
        .find(|(head, _)| head != &abandoned.0)
        .expect("the rewound branch head");
    assert!(
        branch.1.messages.starts_with(&rewound_prefix.1.messages),
        "the rewound branch keeps the prefix it had before the job finished"
    );
    assert!(
        branch.1.messages.len() > rewound_prefix.1.messages.len(),
        "the post-rewind turn was appended to the rewound branch"
    );
    assert!(
        branch.1.messages.len() < full.messages.len(),
        "the rewound branch is shorter than the abandoned head: {} vs {}",
        branch.1.messages.len(),
        full.messages.len()
    );
    let page = export_all(&a, &session);
    assert_eq!(
        page.heads.len(),
        2,
        "the export carries both retained heads: {:?}",
        page.heads
    );
}

#[test]
fn concurrent_divergent_heads_are_both_retained() {
    let fx = Lifecycle::start("heads");
    let a = fx.a.client();
    let b = fx.b.client();

    let session = store::SessionId::parse("t17_divergent_heads").expect("session id");
    let base = {
        let mut writer = store::BlockingWriter::open(fx.a.paths(), session.clone()).expect("seed writer");
        let root = append_root(&mut writer, &session);
        append_message(
            &mut writer,
            &session,
            &root,
            "shared-base",
            "user",
            "shared base message",
        )
    };
    let id = session.as_str().to_owned();
    let seeded = export_all(&a, &id);
    assert_eq!(seeded.heads, vec![base.as_str().to_owned()]);
    import_all(&b, &a, &seeded);
    assert_eq!(
        store_projection(&fx.b.paths(), &id, None).messages.len(),
        1,
        "the replica holds the shared base"
    );

    for (paths, label, text) in [
        (fx.a.paths(), "branch-a", "divergent branch A"),
        (fx.b.paths(), "branch-b", "divergent branch B"),
    ] {
        let mut writer =
            store::BlockingWriter::open(paths, session.clone()).expect("each HOME writes its own divergent branch");
        append_message(&mut writer, &session, &base, label, "user", text);
    }

    let from_b = export_all(&b, &id);
    match a.import_session_events(&from_b, None) {
        Ok(result) => assert!(
            result.conflict_heads.is_empty(),
            "a clean accept reports no conflict: {result:?}"
        ),
        Err(error) => {
            let details = error.details.clone().expect("typed detail");
            assert_eq!(
                details.store_code, "conflict_heads",
                "a diverged import reports the retained conflict heads"
            );
            assert!(
                !details.accepted_ids.is_empty() && details.retry_cursor.is_none(),
                "the accepted prefix is reported with the conflict: {details:?}"
            );
        }
    }
    let heads = a.list_session_heads(&id).expect("heads").heads;
    assert_eq!(heads.len(), 2, "both divergent heads are retained: {heads:?}");

    let mut texts = Vec::new();
    for head in heads {
        let projection = store_projection(&fx.a.paths(), &id, Some(&head));
        texts.push(
            projection
                .messages
                .iter()
                .map(|row| row.text.clone())
                .collect::<Vec<_>>()
                .join("|"),
        );
    }
    assert!(
        texts.iter().any(|text| text.contains("divergent branch A")),
        "branch A survives: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains("divergent branch B")),
        "branch B is imported beside it: {texts:?}"
    );
    assert!(
        texts.iter().all(|text| text.contains("shared base message")),
        "both branches keep the shared base: {texts:?}"
    );

    let from_a = export_all(&a, &id);
    let _ = b.import_session_events(&from_a, None);
    assert_eq!(
        page_pairs(&export_all(&a, &id)).len(),
        page_pairs(&export_all(&b, &id)).len(),
        "both HOMEs hold the same event set"
    );
}

#[test]
fn old_client_without_new_capabilities_keeps_the_transcript() {
    let fx = Lifecycle::start("old");
    let a = fx.a.client();
    let session = create_session(&a, &fx.root);
    run_turn(&a, &session, "old client baseline message");
    context_messages(&a, &session, 8, "old client context");
    a.compact(&session).expect("compact");
    wait_for_compaction(&fx.a.paths(), &session, WAIT).unwrap_or_else(|error| panic!("{error}\n{}", fx.a.logs()));

    let page = a
        .get_history_page(&session, None, None, Some(200))
        .expect("cursor page");
    let modern: Vec<String> = page
        .messages
        .iter()
        .filter(|row| row.role == "user")
        .map(|row| row.content.clone())
        .collect();
    assert!(!modern.is_empty(), "the cursor page has user rows");

    let stream = UnixStream::connect(&fx.a.api_socket).expect("connect");
    stream.set_read_timeout(Some(WAIT)).expect("timeout");
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    writeln!(
        writer,
        "{}",
        json!({"v":1,"id":1,"req":"hello","min_version":1,"max_version":1,
               "client":"t17-old-client/0.1"})
    )
    .expect("hello");
    writer.flush().expect("flush");
    reader.read_line(&mut line).expect("hello reply");
    let hello: Value = serde_json::from_str(&line).expect("hello json");
    assert_eq!(hello["ev"], "hello_ok");
    assert_eq!(
        hello["version"], 1,
        "the wire major version is unchanged for an old client: {hello}"
    );

    writeln!(
        writer,
        "{}",
        json!({"v":1,"id":2,"req":"attach_session","session_id":session})
    )
    .expect("attach");
    writer.flush().expect("flush");
    let end = Instant::now() + WAIT;
    let mut attached = false;
    while Instant::now() < end && !attached {
        line.clear();
        if reader.read_line(&mut line).expect("frame") == 0 {
            break;
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if frame["reply_to"] == 2 && frame["ev"] == "error" {
            panic!("attach refused: {frame}");
        }
        attached = frame["ev"] == "attached";
    }
    assert!(attached, "an old client can attach");

    writeln!(writer, "{}", json!({"v":1,"id":3,"req":"get_history"})).expect("get_history");
    writer.flush().expect("flush");
    let mut legacy: Option<Vec<String>> = None;
    while Instant::now() < end && legacy.is_none() {
        line.clear();
        if reader.read_line(&mut line).expect("frame") == 0 {
            break;
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if frame["reply_to"] == 3 && frame["ev"] == "history" {
            legacy = Some(
                frame["messages"]
                    .as_array()
                    .expect("history rows")
                    .iter()
                    .filter(|row| row["role"] == "user")
                    .map(|row| row["content"].as_str().unwrap_or_default().to_string())
                    .collect(),
            );
        }
    }
    let legacy = legacy.expect("an old client receives history");
    for row in &legacy {
        assert!(
            modern.iter().any(|candidate| candidate == row),
            "the legacy transcript shows no row the cursor page does not: {row:?}"
        );
    }
    assert!(
        legacy.iter().any(|row| row.contains("old client baseline message")),
        "the legacy transcript still shows the pre-compaction original: {} rows",
        legacy.len()
    );
    assert!(
        legacy.len() + 1 >= modern.len(),
        "the legacy transcript covers essentially the same user rows: {} of {}",
        legacy.len(),
        modern.len()
    );

    writeln!(writer, "{}", json!({"v":1,"id":4,"req":"ping"})).expect("ping");
    writer.flush().expect("flush");
    let mut pong = false;
    while Instant::now() < end && !pong {
        line.clear();
        if reader.read_line(&mut line).expect("frame") == 0 {
            break;
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        pong = frame["reply_to"] == 4 && frame["ev"] == "pong";
    }
    assert!(pong, "the old connection stays usable");
}
