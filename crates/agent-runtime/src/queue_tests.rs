use super::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn message(content: &str) -> SoftInterruptMessage {
    SoftInterruptMessage {
        content: content.to_string(),
        images: Vec::new(),
        urgent: false,
        source: SoftInterruptSource::User,
        client_id: None,
        input_id: None,
    }
}

fn contents(queue: &SoftInterruptQueue) -> Vec<String> {
    queue
        .pending_snapshot()
        .into_iter()
        .map(|message| message.content)
        .collect()
}

#[test]
fn transfer_moves_pending_to_in_flight() {
    let queue = SoftInterruptQueue::new();
    {
        let mut pending = queue.lock().expect("queue lock");
        pending.push(message("one"));
        pending.push(message("two"));
    }

    let moved = queue.transfer_to_in_flight();

    assert_eq!(
        moved.iter().map(|message| message.content.as_str()).collect::<Vec<_>>(),
        vec!["one", "two"],
        "the moved messages keep their queue order"
    );
    assert_eq!(queue.pending_len(), 0, "nothing stays pending");
    assert_eq!(queue.in_flight_len(), 2, "both messages are staged as in flight");
    assert!(contents(&queue).is_empty());
}

#[test]
fn commit_clears_in_flight() {
    let queue = SoftInterruptQueue::new();
    queue.lock().expect("queue lock").push(message("one"));
    queue.transfer_to_in_flight();

    assert_eq!(queue.commit(), 1);

    assert_eq!(queue.in_flight_len(), 0, "a commit empties the staged messages");
    assert_eq!(queue.pending_len(), 0, "a commit does not resurrect them");
    assert_eq!(queue.commit(), 0, "a second commit has nothing left to clear");
}

#[test]
fn restore_returns_in_flight_to_pending() {
    let queue = SoftInterruptQueue::new();
    {
        let mut pending = queue.lock().expect("queue lock");
        pending.push(message("one"));
        pending.push(message("two"));
    }
    queue.transfer_to_in_flight();
    queue.lock().expect("queue lock").push(message("three"));

    assert_eq!(queue.restore(), 2);

    assert_eq!(queue.in_flight_len(), 0, "nothing is left staged");
    assert_eq!(
        contents(&queue),
        vec!["one", "two", "three"],
        "restored messages go back in front, in their original order"
    );
    assert_eq!(queue.restore(), 0, "a second restore has nothing to move");
}

/// One mutex owns both collections, so no half-transition is observable.
#[test]
fn transfer_takes_one_lock_at_a_time() {
    let queue = SoftInterruptQueue::new();
    queue.lock().expect("queue lock").push(message("one"));

    let held = queue.lock().expect("queue lock");
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker_queue = queue.clone();
    let worker = std::thread::spawn(move || {
        let moved = worker_queue.transfer_to_in_flight();
        let _ = done_tx.send(moved.len());
    });

    let blocked_until = Instant::now() + Duration::from_millis(150);
    while Instant::now() < blocked_until {
        assert!(
            done_rx.try_recv().is_err(),
            "a transfer must not run while another owner holds the queue"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(held.len(), 1, "the held guard still sees the message pending");

    drop(held);
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("transfer completes"),
        1
    );
    worker.join().expect("transfer thread");

    assert_eq!(queue.pending_len(), 0);
    assert_eq!(queue.in_flight_len(), 1);
    assert_eq!(
        queue.pending_len() + queue.in_flight_len(),
        1,
        "nothing was lost or duplicated"
    );
}

/// A value copy here would silently split one session's queue.
#[test]
fn clone_shares_the_same_queue() {
    let queue = SoftInterruptQueue::new();
    let clone = queue.clone();

    queue.lock().expect("queue lock").push(message("shared"));

    assert_eq!(clone.pending_len(), 1);
    assert_eq!(contents(&clone), vec!["shared"]);

    clone.transfer_to_in_flight();
    assert_eq!(queue.in_flight_len(), 1, "the original sees the clone's transfer");
    assert_eq!(queue.commit(), 1);
    assert_eq!(clone.in_flight_len(), 0, "and the clone sees the commit");

    let thread_queue = queue.clone();
    std::thread::spawn(move || thread_queue.lock().expect("queue lock").push(message("threaded")))
        .join()
        .expect("pusher thread");
    assert_eq!(
        contents(&clone),
        vec!["threaded"],
        "a clone shared across threads sees every push"
    );

    let poisoner = queue.clone();
    let _ = std::thread::spawn(move || {
        let _held = poisoner.lock().expect("queue lock");
        panic!("poison the queue lock");
    })
    .join();
    let recovered = queue.lock().expect_err("the lock is poisoned").into_inner();
    assert_eq!(
        recovered.len(),
        1,
        "a poisoned lock still hands back the recovered state"
    );
    drop(clone);
    assert_eq!(Arc::strong_count(&queue.inner), 1, "only the test holds the queue now");
}

#[test]
fn queue_high_water_increases_under_a_burst() {
    // The counters are process-wide, so this test resets and then reads a
    // delta rather than an absolute value: another test may push concurrently.
    horon_agent_perf_counters::reset();
    let queue = SoftInterruptQueue::new();

    queue.observe();
    let baseline = horon_agent_perf_counters::snapshot();
    assert_eq!(baseline.queue_depth_high_water, 0, "an empty queue reports no depth");

    for index in 0..64 {
        queue
            .lock()
            .expect("queue lock")
            .push(message(&format!("burst {index}")));
        queue.observe();
    }

    let after = horon_agent_perf_counters::snapshot();
    assert_eq!(
        after.queue_depth_high_water, 64,
        "the burst must raise the high-water mark to the deepest depth reached"
    );
    assert!(
        after.queue_bytes_high_water >= 64 * "burst 0".len() as u64,
        "buffered bytes must cover every queued message, got {}",
        after.queue_bytes_high_water
    );

    queue.transfer_to_in_flight();
    queue.observe();
    let drained = horon_agent_perf_counters::snapshot();
    assert_eq!(
        drained.queue_depth_high_water, 64,
        "moving messages in flight retains them, so the high-water mark holds"
    );

    queue.commit();
    queue.observe();
    let committed = horon_agent_perf_counters::snapshot();
    // The structural gate (scripts/perf/check_structural.py) reads this line and
    // asserts the queue bound, so the numbers it checks are the ones this test
    // just proved rather than a re-measurement.
    println!(
        "STRUCTURAL_PERF section=queue source=agent-runtime burst=64 queue_depth_high_water={} queue_bytes_high_water={} queue_bytes_buffered={}",
        committed.queue_depth_high_water, committed.queue_bytes_high_water, committed.queue_bytes_buffered
    );
    assert_eq!(committed.queue_bytes_buffered, 0, "a committed queue buffers nothing");
    assert_eq!(
        committed.queue_depth_high_water, 64,
        "the high-water mark never decreases"
    );
}

#[test]
fn depth_and_buffered_bytes_count_in_flight_messages() {
    let queue = SoftInterruptQueue::new();
    queue.lock().expect("queue lock").push(message("pending"));
    assert_eq!(queue.depth(), 1);
    let pending_bytes = queue.buffered_bytes();
    assert!(pending_bytes >= "pending".len());

    queue.transfer_to_in_flight();
    assert_eq!(queue.depth(), 1, "an in-flight message is still retained");
    assert_eq!(queue.buffered_bytes(), pending_bytes);

    queue.commit();
    assert_eq!(queue.depth(), 0);
    assert_eq!(queue.buffered_bytes(), 0);
}
