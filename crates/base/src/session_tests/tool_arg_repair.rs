//! A repaired call's record rides on the stored message it belongs to. Recording
//! it must not add a single write or flush to the way a session is saved, because
//! a repair is bookkeeping about the call and nothing about durability.

use super::tests::cases::canonical_tempdir;
use super::*;
use horon_agent_session_types::repair_record::{self, ArgRepairRecord};
use std::sync::{Arc, Mutex};

fn tool_result(call_id: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: call_id.to_string(),
        content: "output".into(),
        is_error: None,
    }
}

/// Record every I/O point this session passes through, in order.
fn watch_io(session: &mut Session) -> Result<Arc<Mutex<Vec<String>>>> {
    let points = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&points);
    session.set_event_io_hook(Box::new(move |point| {
        sink.lock().expect("io point log").push(format!("{point:?}"));
        Ok(())
    }))?;
    Ok(points)
}

fn stored_call(session: &Session) -> horon_agent_session_types::StoredMessage {
    session
        .history_messages()
        .expect("history")
        .into_iter()
        .find(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        })
        .expect("a stored tool result")
}

fn record(original: &str, change: &str) -> ArgRepairRecord {
    ArgRepairRecord {
        original_input: serde_json::from_str(original).expect("original input"),
        changes: vec![change.to_string()],
    }
}

#[test]
fn a_repaired_call_is_saved_with_the_same_io_as_an_unrepaired_one() -> Result<()> {
    // Given two sessions of the same shape, one of which appends a call whose
    // arguments a repair rewrote.
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-repair-flush-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);

    let mut plain = Session::create_with_id("repair_flush_plain".into(), None, None);
    let plain_points = watch_io(&mut plain)?;
    plain.add_message(Role::User, vec![tool_result("call-plain")]);
    plain.save()?;

    let mut repaired = Session::create_with_id("repair_flush_repaired".into(), None, None);
    let repaired_points = watch_io(&mut repaired)?;
    repair_record::remember(
        &repaired.id,
        "call-repaired",
        ArgRepairRecord {
            original_input: serde_json::json!({"flag": "true"}),
            changes: vec!["coerced `flag` to a boolean".to_string()],
        },
    );
    repaired.add_message(Role::User, vec![tool_result("call-repaired")]);
    repaired.save()?;

    // Then the write path is identical point for point.
    assert_eq!(
        *plain_points.lock().expect("plain points"),
        *repaired_points.lock().expect("repaired points"),
        "recording a repair must not add a write or a flush"
    );
    // And the record landed on the stored message, not next to it.
    let message = stored_call(&Session::load(&repaired.id)?);
    assert_eq!(message.original_input, Some(serde_json::json!({"flag": "true"})));
    assert_eq!(
        message.arg_repairs,
        Some(vec!["coerced `flag` to a boolean".to_string()])
    );
    Ok(())
}

#[test]
fn an_unrepaired_call_stores_neither_field() -> Result<()> {
    // Given a session whose only call needed no repair.
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-repair-absent-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);
    let mut session = Session::create_with_id("repair_absent".into(), None, None);
    session.add_message(Role::User, vec![tool_result("call-plain")]);
    session.save()?;
    // Then the stored message reads back with no repair record at all.
    let message = stored_call(&Session::load(&session.id)?);
    assert!(message.original_input.is_none());
    assert!(message.arg_repairs.is_none());
    Ok(())
}

#[test]
fn a_record_left_for_another_call_does_not_attach_to_this_one() -> Result<()> {
    // Given a record that belongs to a call the session never stores.
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-repair-other-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);
    let mut session = Session::create_with_id("repair_other".into(), None, None);
    repair_record::remember(
        &session.id,
        "call-abandoned",
        record(r#"{"flag": "true"}"#, "coerced `flag` to a boolean"),
    );
    session.add_message(Role::User, vec![tool_result("call-plain")]);
    session.save()?;
    // Then the unrelated message stays untouched.
    let message = stored_call(&Session::load(&session.id)?);
    assert!(message.original_input.is_none());
    // And the abandoned entry is still there for its own call, so a later
    // append of that call can still claim it.
    assert!(repair_record::take(&session.id, "call-abandoned").is_some());
    Ok(())
}

#[test]
fn two_sessions_reusing_one_call_id_keep_their_own_records() -> Result<()> {
    // Given two sessions whose provider both names the first call `call_0`,
    // each rewriting its arguments differently.
    let _lock = tests::lock_env();
    let (_home_dir, home_path) = canonical_tempdir("horon-agent-repair-shared-")?;
    let _home = tests::EnvVarGuard::set("HORON_HOME", &home_path);
    let mut first = Session::create_with_id("repair_shared_first".into(), None, None);
    let mut second = Session::create_with_id("repair_shared_second".into(), None, None);
    repair_record::remember(
        &first.id,
        "call_0",
        record(r#"{"timeout": "1"}"#, "coerced `timeout` to a number"),
    );
    repair_record::remember(
        &second.id,
        "call_0",
        record(r#"{"limit": "5"}"#, "coerced `limit` to a number"),
    );

    // When each session stores its own call.
    first.add_message(Role::User, vec![tool_result("call_0")]);
    first.save()?;
    second.add_message(Role::User, vec![tool_result("call_0")]);
    second.save()?;

    // Then each message carries its own session's rewrite and nothing else.
    let first_message = stored_call(&Session::load(&first.id)?);
    assert_eq!(first_message.original_input, Some(serde_json::json!({"timeout": "1"})));
    let second_message = stored_call(&Session::load(&second.id)?);
    assert_eq!(second_message.original_input, Some(serde_json::json!({"limit": "5"})));
    assert_eq!(
        second_message.arg_repairs,
        Some(vec!["coerced `limit` to a number".to_string()])
    );
    Ok(())
}

#[test]
fn forgetting_a_session_drops_only_its_own_records() {
    // Given one record left behind by each of two sessions under the same call id.
    repair_record::remember("forget_first", "call_0", record("{}", "coerced a"));
    repair_record::remember("forget_second", "call_0", record("{}", "coerced b"));

    // When one session is torn down before its call is stored.
    repair_record::forget_session("forget_first");

    // Then only that session's record is gone.
    assert!(repair_record::take("forget_first", "call_0").is_none());
    assert!(repair_record::take("forget_second", "call_0").is_some());
}
