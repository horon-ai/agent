//! Tests for the source-ordered snapshot boundary.
//!
//! The named filter is `snapshot_marker`: every test here proves one half of the
//! contract — a change recorded before the capture is inside the returned view,
//! and a change recorded after it is strictly newer than the returned `seq`.

use super::*;
use std::sync::Barrier;
use std::sync::atomic::{AtomicUsize, Ordering};

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

fn fresh(session_id: &str) -> Arc<SessionObservation> {
    forget_session(session_id);
    observation_for(session_id)
}

#[test]
fn snapshot_marker_reply_carries_one_marker_and_one_view() {
    let session = "snapshot-marker-identity";
    let observation = fresh(session);
    for index in 0..3 {
        observation.append_message(message("user", format!("m{index}")));
    }

    let view = observation.take_snapshot(&SnapshotRequest::default());

    assert_eq!(view.session_id, session);
    assert_eq!(view.seq, 3, "the boundary is the sequence of the captured state");
    assert_eq!(view.state.last_seq, view.seq, "the state reports the same boundary");
    assert!(!view.marker_id.is_empty(), "the reply names the marker it carries");
    assert_eq!(view.messages.len(), 3);
    assert!(!view.truncated);

    let second = observation.take_snapshot(&SnapshotRequest::default());
    assert_ne!(
        view.marker_id, second.marker_id,
        "each request mints its own marker identity"
    );
    assert_eq!(second.seq, view.seq, "no change happened between the two captures");
}

#[test]
fn snapshot_marker_an_event_emitted_after_the_capture_is_outside_the_view() {
    let session = "snapshot-marker-live";
    let observation = fresh(session);
    observation.append_message(message("user", "before"));

    let view = observation.take_snapshot(&SnapshotRequest::default());
    let after_capture = observation.append_message(message("assistant", "after"));

    assert!(
        after_capture > view.seq,
        "a change after the capture must be strictly newer than the boundary: {after_capture} vs {}",
        view.seq
    );
    assert_eq!(
        view.messages.len(),
        1,
        "the snapshot must not contain the change that came after its capture"
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
        let view = observation.take_snapshot(&SnapshotRequest {
            tail: Some(MAX_TAIL),
            ..Default::default()
        });
        snapshots.push((view.state.message_count, view.messages.len(), view.seq));
        std::thread::yield_now();
    }
    for handle in handles {
        handle.join().expect("writer thread");
    }

    // The reader loop above stops as soon as every append is *recorded*, which
    // is not the same instant as the last snapshot being taken: the loop checks
    // the counter, then snapshots. One snapshot taken after the writers have
    // joined is what makes "the last snapshot sees everything" deterministic
    // instead of a race against the reader's own timing.
    let last = observation.take_snapshot(&SnapshotRequest {
        tail: Some(MAX_TAIL),
        ..Default::default()
    });
    snapshots.push((last.state.message_count, last.messages.len(), last.seq));

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

    let view_a = observation.take_snapshot(&SnapshotRequest::default());
    let view_b = observation.take_snapshot(&SnapshotRequest::default());

    assert_eq!(view_a.generation, view_b.generation, "one owner, one generation");
    assert_eq!(view_a.seq, view_b.seq, "no change happened between the two attaches");
    assert_ne!(view_a.marker_id, view_b.marker_id, "each request gets its own marker");
    assert_eq!(view_a.messages.len(), view_b.messages.len());
}

#[test]
fn snapshot_marker_every_record_is_inside_the_tail_or_after_the_boundary() {
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
        let view = observation.take_snapshot(&SnapshotRequest {
            tail: Some(MAX_TAIL),
            max_bytes: Some(MAX_MAX_BYTES),
            ..Default::default()
        });
        // Every append advances the observed sequence by exactly one, so the
        // boundary's sequence *is* the number of records at or before it.
        // Anything the reader has not recorded yet is therefore strictly after
        // the boundary, and nothing can fall between the two sets.
        assert_eq!(
            view.state.message_count,
            usize::try_from(view.seq).expect("a record count fits usize"),
            "every record at or before the boundary is inside the snapshot"
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
    let last = observation.take_snapshot(&SnapshotRequest {
        tail: Some(MAX_TAIL),
        ..Default::default()
    });
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
    let idle = observation.take_snapshot(&SnapshotRequest::default());
    assert!(!idle.state.processing);

    let after = observation.set_processing(true);
    let busy = observation.take_snapshot(&SnapshotRequest::default());

    assert!(busy.state.processing, "a state change is part of the observation");
    assert_eq!(busy.state.last_change, "processing");
    assert!(after > idle.seq);
    assert_eq!(busy.seq, after);
}

#[test]
fn snapshot_marker_restart_changes_the_generation() {
    let session = "snapshot-marker-restart";
    let observation = fresh(session);
    observation.append_message(message("user", "m0"));
    let before = observation.take_snapshot(&SnapshotRequest::default());

    forget_session(session);
    let restarted = observation_for(session);
    let after = restarted.take_snapshot(&SnapshotRequest::default());

    assert_ne!(
        before.generation, after.generation,
        "a restart must not report the previous generation"
    );
    assert_eq!(after.seq, 0, "the new generation starts from its own sequence");
    assert!(after.messages.is_empty(), "a restart does not replay the old tail");
}

#[test]
fn snapshot_marker_bounds_the_tail_and_the_byte_budget() {
    let session = "snapshot-marker-bounds";
    let observation = fresh(session);
    for index in 0..300 {
        observation.append_message(message("user", format!("message-{index}")));
    }

    let oversized = observation.take_snapshot(&SnapshotRequest {
        tail: Some(u32::MAX),
        max_bytes: Some(u64::MAX),
        ..Default::default()
    });
    assert!(
        oversized.messages.len() <= MAX_TAIL as usize,
        "an oversized tail request is clamped to {MAX_TAIL}, got {}",
        oversized.messages.len()
    );
    assert!(oversized.truncated);

    let small = observation.take_snapshot(&SnapshotRequest {
        tail: Some(MAX_TAIL),
        max_bytes: Some(200),
        ..Default::default()
    });
    assert!(small.truncated, "the byte budget cut the tail");
    let bytes: usize = small.messages.iter().map(|m| m.role.len() + m.content.len()).sum();
    assert!(bytes <= 200, "the byte budget must hold, got {bytes}");
    assert!(small.next_history_cursor.is_some(), "a cut tail names its continuation");

    let cursor = small.next_history_cursor.clone().expect("cursor");
    let older = observation.take_snapshot(&SnapshotRequest {
        cursor: Some(cursor),
        ..SnapshotRequest::default()
    });
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
    let view = observation.take_snapshot(&SnapshotRequest {
        cursor: Some("before:1".to_string()),
        tail: Some(MAX_TAIL),
        ..Default::default()
    });
    assert!(
        view.truncated,
        "an unreachable page is reported as truncated, not faked"
    );
    assert!(
        view.next_history_cursor.is_some(),
        "the reply names the oldest page it can still serve"
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

    let mut delivered = Vec::new();
    for _ in 0..5 {
        delivered.push(observation.advance_delivery());
    }
    assert!(
        delivered.windows(2).all(|pair| pair[0] < pair[1]),
        "each delivery must be strictly newer than the last: {delivered:?}"
    );

    let view = observation.take_snapshot(&SnapshotRequest::default());
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
