//! Tests for the source-ordered snapshot boundary.
//!
//! The named filter is `snapshot_marker`: every test here proves one half of the
//! plan's acceptance — an event emitted between the state capture and the reply is
//! classified as live through the marker, and no record can fall between the tail
//! and the marker.

use super::*;
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn message(role: &str, content: impl Into<String>) -> HistoryMessage {
    HistoryMessage {
        turn_start: false,
        message_id: None,
        response_stats: None,
        role: role.to_string(),
        content: content.into(),
        tool_calls: None,
        tool_data: None,
        verdict: None,
    }
}

fn connection() -> (
    mpsc::UnboundedSender<ConnectionRecord>,
    mpsc::UnboundedReceiver<ConnectionRecord>,
) {
    mpsc::unbounded_channel()
}

fn marker_of(record: ConnectionRecord) -> SnapshotMarker {
    match record {
        ConnectionRecord::Marker(marker) => marker,
    }
}

fn fresh(session_id: &str) -> Arc<SessionObservation> {
    forget_session(session_id);
    observation_for(session_id)
}

#[test]
fn snapshot_marker_is_enqueued_before_the_reply_is_built() {
    let session = "snapshot-marker-order";
    let observation = fresh(session);
    for index in 0..3 {
        observation.append_message(message("user", format!("m{index}")));
    }
    let (tx, mut rx) = connection();

    let view = observation
        .take_snapshot(&SnapshotRequest::default(), &tx)
        .expect("the connection accepts the marker");

    let marker = marker_of(rx.try_recv().expect("the marker is already on the FIFO"));
    assert_eq!(marker.session_id, session);
    assert_eq!(marker.generation, view.generation);
    assert_eq!(marker.seq, view.seq);
    assert_eq!(marker.marker_id, view.marker_id);
    assert_eq!(view.messages.len(), 3);
    assert!(!view.truncated);
}

#[test]
fn snapshot_marker_an_event_emitted_between_capture_and_reply_is_live() {
    let session = "snapshot-marker-live";
    let observation = fresh(session);
    observation.append_message(message("user", "before"));
    let (tx, mut rx) = connection();

    // The state is captured and the marker enqueued, exactly as a reply would be
    // prepared. The publish then happens before the reply reaches the socket.
    let view = observation
        .take_snapshot(&SnapshotRequest::default(), &tx)
        .expect("the connection accepts the marker");
    let after_capture = observation.append_message(message("assistant", "after"));

    let marker = marker_of(rx.try_recv().expect("marker"));
    assert!(
        after_capture > marker.seq,
        "a change after the capture must be strictly newer than the marker: {after_capture} vs {}",
        marker.seq
    );
    assert_eq!(
        view.messages.len(),
        1,
        "the snapshot must not contain the change that came after its capture"
    );
    assert_eq!(
        view.state.last_seq, marker.seq,
        "the reply's own seq is the marker's seq"
    );
}

#[test]
fn snapshot_marker_concurrent_append_never_loses_a_record() {
    let session = "snapshot-marker-concurrent";
    let observation = fresh(session);
    let writers = 4usize;
    let per_writer = 60usize;
    let total = writers * per_writer;

    let barrier = Arc::new(Barrier::new(writers + 1));
    let stop = Arc::new(AtomicBool::new(false));
    let appended = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for writer in 0..writers {
        let observation = Arc::clone(&observation);
        let barrier = Arc::clone(&barrier);
        let appended = Arc::clone(&appended);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for index in 0..per_writer {
                observation.append_message(message("user", format!("w{writer}-{index}")));
                appended.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }

    barrier.wait();
    let mut snapshots = Vec::new();
    while appended.load(Ordering::SeqCst) < total {
        let (tx, mut rx) = connection();
        let view = observation
            .take_snapshot(
                &SnapshotRequest {
                    tail: Some(MAX_TAIL),
                    ..Default::default()
                },
                &tx,
            )
            .expect("the connection accepts the marker");
        let marker = marker_of(rx.try_recv().expect("marker"));
        assert_eq!(marker.seq, view.seq);
        snapshots.push((view.state.message_count, view.messages.len(), view.seq));
        std::thread::yield_now();
    }
    stop.store(true, Ordering::SeqCst);
    for handle in handles {
        handle.join().expect("writer thread");
    }

    // The reader loop above stops as soon as every append is *recorded*, which
    // is not the same instant as the last snapshot being taken: the loop checks
    // the counter, then snapshots. One snapshot taken after the writers have
    // joined is what makes "the last snapshot sees everything" deterministic
    // instead of a race against the reader's own timing.
    {
        let (tx, mut rx) = connection();
        let view = observation
            .take_snapshot(
                &SnapshotRequest {
                    tail: Some(MAX_TAIL),
                    ..Default::default()
                },
                &tx,
            )
            .expect("the connection accepts the marker");
        let marker = marker_of(rx.try_recv().expect("marker"));
        assert_eq!(marker.seq, view.seq);
        snapshots.push((view.state.message_count, view.messages.len(), view.seq));
    }

    assert_eq!(appended.load(Ordering::SeqCst), total);
    assert!(!snapshots.is_empty(), "the reader must have taken snapshots");
    let mut previous_count = 0usize;
    let mut previous_seq = 0u64;
    for (count, returned, seq) in snapshots {
        assert!(
            count >= previous_count,
            "message count must not go backwards: {count} after {previous_count}"
        );
        assert!(seq >= previous_seq, "the observed sequence must not go backwards");
        assert!(returned <= count);
        assert_eq!(
            returned,
            count.min(MAX_TAIL as usize),
            "a snapshot with room returns the newest {MAX_TAIL} of {count}"
        );
        previous_count = count;
        previous_seq = seq;
    }
    assert_eq!(
        previous_count, total,
        "the last snapshot must see every appended record"
    );
}

#[test]
fn snapshot_marker_attach_a_and_b_see_the_same_boundary() {
    let session = "snapshot-marker-attach-ab";
    let observation = fresh(session);
    for index in 0..5 {
        observation.append_message(message("user", format!("m{index}")));
    }
    let (tx_a, mut rx_a) = connection();
    let (tx_b, mut rx_b) = connection();

    let view_a = observation
        .take_snapshot(&SnapshotRequest::default(), &tx_a)
        .expect("A accepts the marker");
    let view_b = observation
        .take_snapshot(&SnapshotRequest::default(), &tx_b)
        .expect("B accepts the marker");

    let marker_a = marker_of(rx_a.try_recv().expect("A marker"));
    let marker_b = marker_of(rx_b.try_recv().expect("B marker"));
    assert_eq!(view_a.generation, view_b.generation, "one owner, one generation");
    assert_eq!(
        marker_a.seq, marker_b.seq,
        "no change happened between the two attaches"
    );
    assert_ne!(
        marker_a.marker_id, marker_b.marker_id,
        "each request gets its own marker"
    );
    assert_eq!(marker_a.marker_id, view_a.marker_id);
    assert_eq!(marker_b.marker_id, view_b.marker_id);
    assert!(rx_a.try_recv().is_err(), "A's marker must not be delivered on B's FIFO");
}

#[test]
fn snapshot_marker_enqueue_failure_is_not_reported_as_success() {
    let session = "snapshot-marker-closed";
    let observation = fresh(session);
    observation.append_message(message("user", "m0"));
    let (tx, rx) = connection();
    drop(rx);

    let error = observation
        .take_snapshot(&SnapshotRequest::default(), &tx)
        .expect_err("a closed FIFO cannot take the boundary");
    assert_eq!(error, SnapshotError::MarkerRejected);
    assert_eq!(
        observation.seq(),
        1,
        "a refused snapshot must not advance the observed sequence"
    );
}

#[test]
fn snapshot_marker_every_record_is_inside_the_tail_or_after_the_marker() {
    let session = "snapshot-marker-never-lost";
    let observation = fresh(session);
    let writers = 3usize;
    let per_writer = 50usize;
    let total = writers * per_writer;
    let appended = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(writers + 1));
    let mut handles = Vec::new();
    for writer in 0..writers {
        let observation = Arc::clone(&observation);
        let barrier = Arc::clone(&barrier);
        let appended = Arc::clone(&appended);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for index in 0..per_writer {
                observation.append_message(message("user", format!("w{writer}-{index}")));
                appended.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }

    barrier.wait();
    let mut snapshots = 0usize;
    while appended.load(Ordering::SeqCst) < total {
        let (tx, mut rx) = connection();
        let view = observation
            .take_snapshot(
                &SnapshotRequest {
                    tail: Some(MAX_TAIL),
                    max_bytes: Some(MAX_MAX_BYTES),
                    ..Default::default()
                },
                &tx,
            )
            .expect("the connection accepts the marker");
        let marker = marker_of(rx.try_recv().expect("marker"));
        // Every append advances the observed sequence by exactly one, so the
        // marker's sequence *is* the number of records at or before it. Anything
        // the reader has not recorded yet is therefore strictly after the marker,
        // and nothing can fall between the two sets.
        assert_eq!(
            view.state.message_count,
            usize::try_from(marker.seq).expect("a record count fits usize"),
            "every record at or before the marker is inside the snapshot"
        );
        assert_eq!(
            view.state.last_seq, marker.seq,
            "the reply reports the marker's own sequence"
        );
        assert_eq!(
            view.messages.len(),
            view.state.message_count.min(MAX_TAIL as usize),
            "the returned tail is the newest part of exactly those records"
        );
        snapshots += 1;
        std::thread::yield_now();
    }
    for handle in handles {
        handle.join().expect("writer thread");
    }

    assert!(snapshots > 0, "at least one snapshot was taken while appends ran");
    let (tx, mut rx) = connection();
    let last = observation
        .take_snapshot(
            &SnapshotRequest {
                tail: Some(MAX_TAIL),
                ..Default::default()
            },
            &tx,
        )
        .expect("the connection accepts the marker");
    let _ = rx.try_recv();
    assert_eq!(
        last.state.message_count, total,
        "the final snapshot contains every record exactly once"
    );
    assert_eq!(usize::try_from(last.seq).expect("fits"), total);
}

#[test]
fn snapshot_marker_reports_a_state_change_as_a_newer_sequence() {
    let session = "snapshot-marker-state";
    let observation = fresh(session);
    let (tx, mut rx) = connection();
    let idle = observation
        .take_snapshot(&SnapshotRequest::default(), &tx)
        .expect("the connection accepts the marker");
    let idle_marker = marker_of(rx.try_recv().expect("marker"));
    assert!(!idle.state.processing);

    let after = observation.set_processing(true);
    let (tx, mut rx) = connection();
    let busy = observation
        .take_snapshot(&SnapshotRequest::default(), &tx)
        .expect("the connection accepts the marker");
    let busy_marker = marker_of(rx.try_recv().expect("marker"));

    assert!(busy.state.processing, "a state change is part of the observation");
    assert_eq!(busy.state.last_change, "processing");
    assert!(after > idle_marker.seq);
    assert_eq!(busy_marker.seq, after);
}

#[test]
fn snapshot_marker_restart_changes_the_generation() {
    let session = "snapshot-marker-restart";
    let observation = fresh(session);
    observation.append_message(message("user", "m0"));
    let (tx, mut rx) = connection();
    let before = observation
        .take_snapshot(&SnapshotRequest::default(), &tx)
        .expect("the connection accepts the marker");
    let before_marker = marker_of(rx.try_recv().expect("marker"));

    forget_session(session);
    let restarted = observation_for(session);
    let (tx, mut rx) = connection();
    let after = restarted
        .take_snapshot(&SnapshotRequest::default(), &tx)
        .expect("the connection accepts the marker");
    let after_marker = marker_of(rx.try_recv().expect("marker"));

    assert_ne!(
        before_marker.generation, after_marker.generation,
        "a restart must not report the previous generation"
    );
    assert_eq!(after_marker.seq, 0, "the new generation starts from its own sequence");
    assert!(
        after.seq < before.seq || before.seq == 0,
        "the new sequence is not a continuation of the old one"
    );
    assert!(after.messages.is_empty(), "a restart does not replay the old tail");
}

#[test]
fn snapshot_marker_bounds_the_tail_and_the_byte_budget() {
    let session = "snapshot-marker-bounds";
    let observation = fresh(session);
    for index in 0..300 {
        observation.append_message(message("user", format!("message-{index}")));
    }

    let (tx, mut rx) = connection();
    let oversized = observation
        .take_snapshot(
            &SnapshotRequest {
                tail: Some(u32::MAX),
                max_bytes: Some(u64::MAX),
                ..Default::default()
            },
            &tx,
        )
        .expect("the connection accepts the marker");
    let _ = rx.try_recv();
    assert!(
        oversized.messages.len() <= MAX_TAIL as usize,
        "an oversized tail request is clamped to {MAX_TAIL}, got {}",
        oversized.messages.len()
    );
    assert!(oversized.truncated);

    let (tx, mut rx) = connection();
    let small = observation
        .take_snapshot(
            &SnapshotRequest {
                tail: Some(MAX_TAIL),
                max_bytes: Some(200),
                ..Default::default()
            },
            &tx,
        )
        .expect("the connection accepts the marker");
    let _ = rx.try_recv();
    assert!(small.truncated, "the byte budget cut the tail");
    let bytes: usize = small.messages.iter().map(|m| m.role.len() + m.content.len()).sum();
    assert!(bytes <= 200, "the byte budget must hold, got {bytes}");
    assert!(small.next_history_cursor.is_some(), "a cut tail names its continuation");

    let cursor = small.next_history_cursor.clone().expect("cursor");
    let (tx, mut rx) = connection();
    let older = observation
        .take_snapshot(
            &SnapshotRequest {
                cursor: Some(cursor),
                ..SnapshotRequest::default()
            },
            &tx,
        )
        .expect("the connection accepts the marker");
    let _ = rx.try_recv();
    assert!(!older.messages.is_empty(), "the cursor reaches an older page");
    let newest = small.messages.last().map(|m| m.content.clone());
    assert_ne!(
        older.messages.last().map(|m| m.content.clone()),
        newest,
        "the older page is strictly older than the newest one"
    );
}

#[test]
fn snapshot_marker_cursor_before_the_retained_window_is_truncated() {
    let session = "snapshot-marker-retained";
    let observation = fresh(session);
    for index in 0..(RETAINED_MESSAGES + 50) {
        observation.append_message(message("user", format!("m{index}")));
    }
    let (tx, mut rx) = connection();
    let view = observation
        .take_snapshot(
            &SnapshotRequest {
                cursor: Some("before:1".to_string()),
                tail: Some(MAX_TAIL),
                ..Default::default()
            },
            &tx,
        )
        .expect("the connection accepts the marker");
    let _ = rx.try_recv();
    assert!(
        view.truncated,
        "an unreachable page is reported as truncated, not faked"
    );
    assert!(
        view.next_history_cursor.is_some(),
        "the reply names the oldest page it can still serve"
    );
}

#[tokio::test]
async fn snapshot_marker_handler_writes_the_reply_after_the_marker() {
    let session = "snapshot-marker-handler";
    let observation = fresh(session);
    observation.append_message(message("user", "hello"));
    let (tx, mut rx) = connection();
    let mut out: Vec<u8> = Vec::new();

    handle_get_snapshot(7, session, SnapshotRequest::default(), &tx, &mut out)
        .await
        .expect("the handler writes the reply");

    let marker = marker_of(rx.try_recv().expect("the marker precedes the reply"));
    let text = String::from_utf8(out).expect("utf8 frames");
    let reply: serde_json::Value = serde_json::from_str(text.lines().next().expect("one frame")).expect("json reply");
    assert_eq!(reply["type"], serde_json::json!("snapshot"));
    assert_eq!(reply["id"], serde_json::json!(7));
    assert_eq!(reply["session_id"], serde_json::json!(session));
    assert_eq!(reply["marker_id"], serde_json::json!(marker.marker_id));
    assert_eq!(reply["generation"], serde_json::json!(marker.generation));
    assert_eq!(reply["seq"], serde_json::json!(marker.seq));
    assert_eq!(reply["messages"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn snapshot_marker_handler_refuses_when_the_fifo_is_closed() {
    let session = "snapshot-marker-handler-closed";
    let observation = fresh(session);
    observation.append_message(message("user", "hello"));
    let (tx, rx) = connection();
    drop(rx);
    let mut out: Vec<u8> = Vec::new();

    handle_get_snapshot(9, session, SnapshotRequest::default(), &tx, &mut out)
        .await
        .expect("the handler still answers, with a refusal");

    let text = String::from_utf8(out).expect("utf8 frames");
    let reply: serde_json::Value = serde_json::from_str(text.lines().next().expect("one frame")).expect("json reply");
    assert_eq!(
        reply["type"],
        serde_json::json!("error"),
        "a refused marker is not a snapshot"
    );
    assert_eq!(reply["id"], serde_json::json!(9));
    assert!(
        reply["message"].as_str().unwrap_or_default().contains("marker"),
        "the refusal names the marker: {reply}"
    );
}

/// The delivery sequence a live event carries must strictly increase per
/// delivery, and a snapshot taken afterwards must bound every delivery before
/// it.
///
/// This is what lets a reconnecting client prove "everything after seq N is
/// new" instead of inferring order from arrival: two events delivered after the
/// same state change still have to be ordered, so the number cannot repeat the
/// last change's sequence. The boundary is inclusive of the deliveries that
/// happened before the capture, so a client applies an event at `seq` equal to
/// its snapshot only when that event was already inside the view.
#[test]
fn snapshot_marker_delivery_sequence_strictly_increases_and_bounds_the_snapshot() {
    let session = "snapshot-marker-delivery-sequence";
    let observation = fresh(session);
    observation.append_message(message("user", "hello"));
    let (tx, _rx) = connection();

    let mut delivered = Vec::new();
    for _ in 0..5 {
        delivered.push(observation.advance_delivery());
    }
    assert!(
        delivered.windows(2).all(|pair| pair[0] < pair[1]),
        "each delivery must be strictly newer than the last: {delivered:?}"
    );

    let view = observation
        .take_snapshot(&SnapshotRequest::default(), &tx)
        .expect("the marker is accepted");
    let last_before = *delivered.last().expect("deliveries");
    assert!(
        view.seq >= last_before,
        "the snapshot boundary {} must cover every delivery before it, including {last_before}",
        view.seq
    );

    // A delivery after the capture is strictly outside the boundary, which is
    // the half a client relies on to call an event new.
    let after = observation.advance_delivery();
    assert!(
        after > view.seq,
        "a delivery after the capture ({after}) must be newer than the boundary {}",
        view.seq
    );
}
