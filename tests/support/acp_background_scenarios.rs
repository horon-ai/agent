//! IS-17 and IS-18 against a real daemon: background terminals in the v2
//! `state_update.background` list, `terminal/cancel`, and live terminal
//! output. A child module of `acp_stdio_v2`, so it shares that target's
//! connection helpers without growing the parent file.

use super::*;

/// The list every `state_update` of `session` carried, in order.
fn background_lists(updates: &[Value]) -> Vec<Value> {
    of_kind(updates, "state_update")
        .into_iter()
        .map(|update| update["background"].clone())
        .collect()
}

/// IS-17: a backgrounded `sleep 30` is announced as a cancellable terminal,
/// listed on the state updates after its tool call returned, stopped by
/// `terminal/cancel`, reported exited, and only then removed from the list.
/// A second `terminal/cancel` of the exited terminal is refused.
#[test]
fn is17_terminal_cancel_and_background() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon.provider.script(
        "IS17_BACKGROUND",
        tool_turn(
            "bash",
            json!({"command": "sleep 30", "run_in_background": true, "intent": "is17 background fixture"}),
            "is17 left it running",
        ),
    );
    let mut conn = Conn::v2(&daemon, "is17");
    conn.initialize();
    let session = conn.new_session(&work);
    conn.take_notes();
    let cursor = conn.cursor(&session);
    let prompt = conn.send_prompt(&session, "IS17_BACKGROUND start it");
    conn.wait("session/prompt", &prompt)
        .unwrap_or_else(|error| panic!("the prompt is inserted: {error}\n{}", daemon.logs()));
    assert_eq!(conn.await_idle_after(&session, cursor), "end_turn", "{}", daemon.logs());

    let listed = conn.await_update(&session, "a state listing the background terminal", |update| {
        update["sessionUpdate"] == "state_update"
            && update["background"]
                .as_array()
                .is_some_and(|list| list.iter().any(|item| item["type"] == "terminal"))
    });
    let terminal_id = listed["background"][0]["terminalId"]
        .as_str()
        .expect("a listed terminalId")
        .to_string();
    let before = updates_of(&conn.take_notes(), &session);
    let announced = before
        .iter()
        .position(|update| {
            update["sessionUpdate"] == "terminal_update"
                && update["terminalId"] == terminal_id.as_str()
                && update["capabilities"]["cancel"].is_object()
        })
        .unwrap_or_else(|| panic!("the terminal is announced cancellable: {before:?}"));
    let first_listing = before
        .iter()
        .position(|update| {
            update["sessionUpdate"] == "state_update"
                && update["background"]
                    .as_array()
                    .is_some_and(|list| list.iter().any(|item| item["terminalId"] == terminal_id.as_str()))
        })
        .unwrap_or_else(|| panic!("a snapshot lists it: {before:?}"));
    assert!(announced < first_listing, "announced before it is listed: {before:?}");
    assert!(
        background_lists(&before).iter().all(Value::is_array),
        "every state update carries the list: {before:?}"
    );

    assert_eq!(
        conn.ok(
            "terminal/cancel",
            json!({"sessionId": session, "terminalId": terminal_id})
        ),
        json!({}),
        "a listed terminal is cancelled"
    );
    let emptied = conn.await_update(&session, "the snapshot that removes the terminal", |update| {
        update["sessionUpdate"] == "state_update" && update["background"] == json!([])
    });
    assert_eq!(emptied["state"], "idle", "{emptied}");
    let after = updates_of(&conn.take_notes(), &session);
    let exited = after
        .iter()
        .position(|update| {
            update["sessionUpdate"] == "terminal_update"
                && update["terminalId"] == terminal_id.as_str()
                && update["exitStatus"].is_object()
        })
        .unwrap_or_else(|| panic!("the terminal reports its exit: {after:?}"));
    let removed = after
        .iter()
        .position(|update| update["sessionUpdate"] == "state_update" && update["background"] == json!([]))
        .unwrap_or_else(|| panic!("the removal snapshot: {after:?}"));
    assert!(exited < removed, "the exit precedes its removal: {after:?}");
    assert!(
        after[exited].get("capabilities").is_some_and(Value::is_null),
        "an exited terminal stops advertising cancel: {:?}",
        after[exited]
    );
    assert!(
        of_kind(&after, "tool_call_update").is_empty(),
        "cancelling the terminal leaves its tool call as it was: {after:?}"
    );

    let again = conn.err(
        "terminal/cancel",
        json!({"sessionId": session, "terminalId": terminal_id}),
    );
    assert_eq!(again["code"], -32602, "an exited terminal is not cancellable: {again}");
    conn.assert_schema_clean();
}

/// IS-18: a foreground shell command's output arrives as
/// `terminal_output_chunk`s while it runs, before its final snapshot.
#[test]
fn is18_live_terminal_output() {
    let daemon = AcpDaemon::start("acp");
    let _dump = LogDump(&daemon);
    let work = work_dir(&daemon);
    daemon.provider.script(
        "IS18_STREAM",
        tool_turn(
            "bash",
            json!({"command": "printf a; sleep 1; printf b", "intent": "is18 streaming fixture"}),
            "is18 done",
        ),
    );
    let mut conn = Conn::v2(&daemon, "is18");
    conn.initialize();
    let session = conn.new_session(&work);
    conn.take_notes();
    let (_, updates, stop) = conn.turn(&session, "IS18_STREAM go");
    assert_eq!(stop, "end_turn", "{}", daemon.logs());
    let chunks = of_kind(&updates, "terminal_output_chunk");
    assert!(!chunks.is_empty(), "live output arrived as chunks: {updates:?}");
    let terminal_id = chunks[0]["terminalId"].as_str().expect("a terminalId");
    let first_chunk = updates
        .iter()
        .position(|update| update["sessionUpdate"] == "terminal_output_chunk")
        .expect("a chunk");
    let exited = updates
        .iter()
        .position(|update| {
            update["sessionUpdate"] == "terminal_update"
                && update["terminalId"] == terminal_id
                && update["exitStatus"].is_object()
        })
        .unwrap_or_else(|| panic!("the final snapshot: {updates:?}"));
    assert!(first_chunk < exited, "output streams before the exit: {updates:?}");
    conn.assert_schema_clean();
}
