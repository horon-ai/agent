//! IS-19 and IS-20 against a real daemon: a v2 audio prompt reaching the
//! model as `input_audio`, and a soft interrupt stored mid-turn reported live
//! as a `user_message` under the id a `session/resume` replay uses. A child
//! module of `acp_stdio_v2`, so it shares that target's connection helpers
//! without growing the parent file.

use super::*;

/// IS-19: a v2 prompt with an `audio/wav` block is inserted and its turn's
/// model request carries the clip as an `input_audio` part.
#[test]
fn is19_audio_prompt() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon
        .provider
        .script("IS19_AUDIO", Script::Text("is19 heard it".to_string()));
    let mut conn = Conn::v2(&daemon, "is19");
    let initialized = conn.initialize();
    assert_eq!(
        initialized["capabilities"]["session"]["prompt"]["audio"],
        json!({}),
        "{initialized}"
    );
    let session = conn.new_session(&work);
    conn.take_notes();
    let cursor = conn.cursor(&session);
    let prompt = conn.send(
        "session/prompt",
        json!({"sessionId": session, "prompt": [
            {"type": "audio", "mimeType": "audio/wav", "data": "UklGRgAAAABXQVZF"},
            {"type": "text", "text": "IS19_AUDIO listen"},
        ]}),
    );
    conn.wait("session/prompt", &prompt)
        .unwrap_or_else(|error| panic!("the audio prompt is inserted: {error}\n{}", daemon.logs()));
    assert_eq!(conn.await_idle_after(&session, cursor), "end_turn", "{}", daemon.logs());
    let body = daemon
        .provider
        .wait_for_request("IS19_AUDIO", WAIT)
        .unwrap_or_else(|error| panic!("{error}\n{}", daemon.logs()));
    assert!(
        body.contains("input_audio") && body.contains("UklGRgAAAABXQVZF"),
        "the model request carries the clip as input_audio: {body}"
    );
    conn.assert_schema_clean();
}

/// IS-20: a soft interrupt a plain client injects while the editor's turn runs
/// is written as a live `user_message`, and a later `session/resume` replays
/// that message under the same `messageId`.
#[test]
fn is20_live_soft_interrupt() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon.provider.script(
        "IS20_TURN",
        tool_turn(
            "bash",
            json!({"command": "sleep 2", "intent": "is20 interrupt window"}),
            "is20 done",
        ),
    );
    let mut conn = Conn::v2(&daemon, "is20");
    conn.initialize();
    let session = conn.new_session(&work);
    conn.take_notes();
    let cursor = conn.cursor(&session);
    let prompt = conn.send_prompt(&session, "IS20_TURN go");
    conn.wait("session/prompt", &prompt)
        .unwrap_or_else(|error| panic!("the prompt is inserted: {error}\n{}", daemon.logs()));
    conn.await_update(&session, "the running tool call", |update| {
        update["sessionUpdate"] == "tool_call_update" && update["status"] == "in_progress"
    });

    let mut peer = Peer::connect(&daemon.socket, &work, Some(&session), false);
    // While a turn runs the daemon only queues the interrupt (an ack, no
    // `done`), so it is written without waiting for a terminal.
    let id = peer.next_id;
    peer.write(json!({"type": "soft_interrupt", "id": id, "content": "IS20_INTERRUPT also check tests"}));
    let live = conn.await_update(&session, "the live soft interrupt", |update| {
        update["sessionUpdate"] == "user_message" && mentions(std::slice::from_ref(update), "IS20_INTERRUPT")
    });
    let message_id = live["messageId"].as_str().expect("a messageId").to_string();
    assert_eq!(conn.await_idle_after(&session, cursor), "end_turn", "{}", daemon.logs());
    conn.assert_schema_clean();

    let mut second = Conn::v2(&daemon, "is20b");
    second.initialize();
    second.ok(
        "session/resume",
        json!({"sessionId": session, "cwd": cwd_str(&work), "replayFrom": {"type": "start"}}),
    );
    let replayed = updates_of(&second.take_notes(), &session);
    let replayed_ids: Vec<&Value> = of_kind(&replayed, "user_message")
        .into_iter()
        .filter(|update| mentions(std::slice::from_ref(*update), "IS20_INTERRUPT"))
        .map(|update| &update["messageId"])
        .collect();
    assert_eq!(
        replayed_ids,
        [&json!(message_id)],
        "the replay keys the interrupt with the live id: {replayed:?}"
    );
    second.assert_schema_clean();
}
