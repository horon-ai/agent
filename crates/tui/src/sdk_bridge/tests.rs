use super::*;

#[test]
fn connecting_to_a_missing_socket_reports_a_sentence() {
    let missing = std::env::temp_dir().join("horon-tui-bridge-missing.sock");
    let options = ConnectOptions {
        socket_path: Some(missing),
        ensure_runtime: false,
        request_timeout: Some(Duration::from_millis(200)),
        ..ConnectOptions::default()
    };
    let message = match connect(options, None, None) {
        Ok(_) => panic!("a missing socket must not connect"),
        Err(message) => message,
    };
    assert!(!message.is_empty());
}

#[test]
fn default_socket_is_named() {
    assert!(!default_socket().as_os_str().is_empty());
}

fn delta(index: usize) -> ApiEvent {
    ApiEvent::TextDelta {
        session_id: "s1".into(),
        text: index.to_string(),
        message_id: Some("m1".into()),
    }
}

fn burst(count: usize) -> std::collections::VecDeque<ApiEvent> {
    (0..count).map(delta).collect()
}

/// The text of every forwarded event, in the order the UI receives them.
fn forwarded_text(received: &Receiver<Incoming>) -> Vec<String> {
    received
        .try_iter()
        .map(|incoming| match incoming {
            Incoming::Event(event) => match *event {
                ApiEvent::TextDelta { text, .. } => text,
                other => panic!("unexpected event: {other:?}"),
            },
            other => panic!("unexpected incoming: {other:?}"),
        })
        .collect()
}

#[test]
fn one_pass_forwards_a_burst_in_order_up_to_the_batch_bound() {
    let (events, received) = mpsc::channel();
    let mut queue = burst(EVENT_BATCH * 2);

    let step = pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1");

    assert_eq!(step, Step::Flowing);
    let text = forwarded_text(&received);
    assert_eq!(text.len(), EVENT_BATCH, "one pass must stop at the bound");
    let expected: Vec<String> = (0..EVENT_BATCH).map(|index| index.to_string()).collect();
    assert_eq!(text, expected, "the pass reordered or dropped events");

    // The rest arrives on the next pass, in the same order.
    assert_eq!(
        pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1"),
        Step::Flowing
    );
    let text = forwarded_text(&received);
    assert_eq!(text.len(), EVENT_BATCH);
    let expected: Vec<String> = (EVENT_BATCH..EVENT_BATCH * 2).map(|index| index.to_string()).collect();
    assert_eq!(text, expected);
}

#[test]
fn a_burst_never_blocks_a_keystroke_for_a_command_poll() {
    // A command queued before the burst must still be taken as soon as the
    // pass ends, rather than after one COMMAND_POLL per forwarded event.
    let (events, received) = mpsc::channel();
    let (commands, incoming) = mpsc::channel();
    commands.send(Command::Cancel).expect("queue the command");
    let mut queue = burst(EVENT_BATCH * 3);

    assert_eq!(
        pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1"),
        Step::Flowing
    );
    assert_eq!(forwarded_text(&received).len(), EVENT_BATCH);

    match take_command(&incoming, true) {
        CommandPoll::Command(Command::Cancel) => {}
        other => panic!("a queued command was not taken: {other:?}"),
    }
}

#[test]
fn an_idle_pass_waits_on_the_stream_and_forwards_nothing() {
    // With nothing ready the pass must wait for the stream rather than poll
    // it forever; this one timeout is what keeps an idle client off the CPU.
    let (events, received) = mpsc::channel();
    let mut timeouts = Vec::new();
    let step = pump(
        &events,
        |timeout| {
            timeouts.push(timeout);
            Ok::<_, Error>(None)
        },
        || false,
        "s1",
    );

    assert_eq!(step, Step::Idle);
    assert_eq!(timeouts, vec![EVENT_POLL]);
    assert!(forwarded_text(&received).is_empty());
}

#[test]
fn a_burst_polls_instead_of_blocking_after_the_first_event() {
    // Blocking again between events would cap throughput at one event per
    // EVENT_POLL, which is the defect the batch exists to remove.
    let (events, _received) = mpsc::channel();
    let mut queue = burst(3);
    let mut timeouts = Vec::new();

    let step = pump(
        &events,
        |timeout| {
            timeouts.push(timeout);
            Ok::<_, Error>(queue.pop_front())
        },
        || false,
        "s1",
    );

    assert_eq!(step, Step::Flowing);
    assert_eq!(
        timeouts,
        vec![EVENT_POLL, Duration::ZERO, Duration::ZERO, Duration::ZERO],
        "reads after the first event must not block"
    );
}

#[test]
fn a_stream_error_still_ends_the_bridge() {
    let (events, received) = mpsc::channel();

    let step = pump(
        &events,
        |_| Err::<Option<ApiEvent>, _>(Error::new(ErrorKind::EventBufferOverflow, "behind")),
        || false,
        "s1",
    );

    assert_eq!(step, Step::Closed);
    let messages: Vec<Incoming> = received.try_iter().collect();
    assert_eq!(messages.len(), 1);
    match &messages[0] {
        Incoming::Closed(reason) => assert!(reason.contains("behind"), "reason was {reason}"),
        other => panic!("expected a closed bridge, got {other:?}"),
    }
}

#[test]
fn the_ui_dropping_its_receiver_stops_the_worker_quietly() {
    let (events, received) = mpsc::channel();
    drop(received);
    let mut queue = burst(2);

    let step = pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1");

    assert_eq!(step, Step::UiGone, "no receiver is not a failure to report");
}

#[test]
fn a_closed_connection_is_reported_once_without_polling_the_dead_stream() {
    // The SDK answers `Ok(None)` for ever on a closed stream, so without
    // the closed check this pass would return Idle and the worker would go
    // round again: a dead connection must cost exactly one read and one
    // notice, not a spin.
    let (events, received) = mpsc::channel();
    let mut reads = 0;

    let step = pump(
        &events,
        |_| {
            reads += 1;
            Ok::<_, Error>(None)
        },
        || true,
        "s1",
    );

    assert_eq!(step, Step::Closed);
    assert_eq!(reads, 1, "the dead stream was read again");
    let messages: Vec<Incoming> = received.try_iter().collect();
    assert_eq!(messages.len(), 1, "the closed notice was sent more than once");
    match &messages[0] {
        Incoming::Closed(reason) => assert_eq!(reason, CONNECTION_CLOSED),
        other => panic!("expected a closed connection, got {other:?}"),
    }
}

#[test]
fn a_live_connection_that_timed_out_is_not_mistaken_for_a_closed_one() {
    let (events, received) = mpsc::channel();

    let step = pump(&events, |_| Ok::<_, Error>(None), || false, "s1");

    assert_eq!(step, Step::Idle);
    assert!(received.try_iter().next().is_none(), "a timeout is not a disconnection");
}

#[test]
fn an_event_of_another_session_is_not_forwarded() {
    // The subscription is unfiltered because the daemon rewrites a typed
    // prefix to the full id; the pass applies the filter against the id the
    // answer named. An event that names no session (a pong, an error) is
    // about the connection and still reaches the client.
    let (events, received) = mpsc::channel();
    let mut queue = burst(0);
    queue.push_back(ApiEvent::TextDelta {
        session_id: "s2".into(),
        text: "another session".into(),
        message_id: None,
    });
    queue.push_back(ApiEvent::Pong);
    queue.push_back(delta(0));

    let step = pump(&events, |_| Ok::<_, Error>(queue.pop_front()), || false, "s1");

    assert_eq!(step, Step::Flowing);
    let forwarded: Vec<Incoming> = received.try_iter().collect();
    assert_eq!(forwarded.len(), 2, "a foreign event was forwarded: {forwarded:?}");
    match &forwarded[0] {
        Incoming::Event(event) => {
            assert!(matches!(event.as_ref(), ApiEvent::Pong), "unexpected event: {event:?}");
        }
        other => panic!("unexpected incoming: {other:?}"),
    }
    match &forwarded[1] {
        Incoming::Event(event) => match &**event {
            ApiEvent::TextDelta { session_id, text, .. } => {
                assert_eq!(session_id.as_str(), "s1");
                assert_eq!(text.as_str(), "0");
            }
            other => panic!("unexpected event: {other:?}"),
        },
        other => panic!("unexpected incoming: {other:?}"),
    }
}

#[test]
fn a_closed_command_channel_stops_the_worker() {
    let (commands, incoming) = mpsc::channel::<Command>();
    drop(commands);

    assert!(matches!(take_command(&incoming, true), CommandPoll::Disconnected));
    assert!(matches!(take_command(&incoming, false), CommandPoll::Disconnected));
}

/// A session record as the daemon serialises one; only the two fields
/// without a default are required, so this is what a record whose metadata
/// never filled in looks like on the wire.
fn record(json: &str) -> SessionInfo {
    serde_json::from_str(json).expect("a session record")
}

#[test]
fn a_session_with_no_metadata_still_reads_as_a_row() {
    let bare = record(r#"{"session_id":"session_ant_1794809866522","status":"idle"}"#);
    assert_eq!(title_of(&bare), None, "a record with no title named one");
    assert_eq!(detail(&bare), "idle", "a record with no time said nothing about itself");
    assert_eq!(activity(&bare), None);
    // It cannot be placed in another directory, so it is not hidden from
    // the list of the directory the client is in.
    assert!(in_directory(&bare, Some("/tmp")));
}

#[test]
fn a_blank_title_is_no_title_and_a_long_one_is_cut() {
    let blank = record(r#"{"session_id":"a","status":"idle","title":"   "}"#);
    assert_eq!(title_of(&blank), None);
    let long = record(&format!(
        r#"{{"session_id":"a","status":"idle","title":"{}"}}"#,
        "t".repeat(LABEL_CHARS + 20)
    ));
    let title = title_of(&long).expect("a titled session");
    assert_eq!(title.chars().count(), LABEL_CHARS);
}

#[test]
fn the_newest_session_comes_first_and_undated_ones_last() {
    let older = record(r#"{"session_id":"older","status":"idle","updated_at_ms":1000}"#);
    let newer = record(r#"{"session_id":"newer","status":"idle","last_active_at_ms":2000}"#);
    let bare = record(r#"{"session_id":"bare","status":"idle"}"#);
    let mut sessions = [bare.clone(), older.clone(), newer.clone()];
    sessions.sort_by_key(|session| std::cmp::Reverse(activity(session)));
    let order: Vec<&str> = sessions.iter().map(|session| session.session_id.as_str()).collect();
    assert_eq!(order, vec!["newer", "older", "bare"]);
}

#[test]
fn a_row_time_reads_as_a_local_moment() {
    let stamped = record(r#"{"session_id":"a","status":"idle","updated_at_ms":1794809866522}"#);
    let detail = detail(&stamped);
    assert_eq!(detail.len(), 11, "the row time is not a date and a time: {detail}");
    assert!(detail.contains(':'), "the row time has no clock: {detail}");
    assert_eq!(moment(1794809866522).as_deref(), Some(detail.as_str()));
}

#[test]
fn a_directory_is_matched_through_another_spelling_and_only_then() {
    let temp = tempfile::tempdir().expect("a directory");
    let real = temp.path().join("work");
    std::fs::create_dir_all(&real).expect("create the directory");
    // The same directory under a different string, on every OS: only
    // resolving both sides can tell that the two are one place.
    let alias = real.join("..").join("work");

    assert!(same_path(
        real.to_str().expect("a path"),
        real.to_str().expect("a path")
    ));
    assert!(!same_path("/one/place", "/another/place"));
    assert!(
        same_path(real.to_str().expect("a path"), alias.to_str().expect("a path")),
        "a working directory spelled another way was treated as another one"
    );

    let here: SessionInfo = serde_json::from_value(serde_json::json!({
        "session_id": "a",
        "status": "idle",
        "working_dir": real.to_str().expect("a path"),
    }))
    .expect("a session record");
    let elsewhere = record(r#"{"session_id":"b","status":"idle","working_dir":"/somewhere/else"}"#);
    let wanted = real.to_str().expect("a path");
    assert!(in_directory(&here, Some(wanted)));
    assert!(!in_directory(&elsewhere, Some(wanted)));
    assert!(
        in_directory(&elsewhere, None),
        "a client with no directory hid sessions"
    );
}
