//! The tool verdict on the v1 surface: the final `tool_call_update` of a call
//! names it under `_meta`, live and replayed, and a call without a verdict
//! keeps the frame it always had. `status` never changes its meaning.

use super::*;
use crate::protocol::HistoryMessage;

fn event(value: Value) -> ServerEvent {
    serde_json::from_value(value.clone()).unwrap_or_else(|err| panic!("not a ServerEvent ({err}): {value}"))
}

/// The final update of one `bash` call that ended as `done` says.
fn final_update(profile: AcpProfile, done: Value) -> Value {
    let mut mapper = EventMapper::new(profile, MapperOptions::default(), "t1".to_string());
    mapper.map_event(event(json!({"type":"tool_start","id":"tool1","name":"bash"})));
    let mut frames = mapper.map_event(event(done));
    assert_eq!(frames.len(), 1, "one final update: {frames:?}");
    frames.remove(0)
}

#[test]
fn v1_live_final_update_names_the_verdict_under_meta() {
    for profile in [AcpProfile::Standard, AcpProfile::Extended] {
        // `status` follows only the error: a rejected call that reported no
        // error text is still `completed`.
        let rejected = final_update(
            profile,
            json!({"type":"tool_done","id":"tool1","name":"bash","output":"no","verdict":"rejected"}),
        );
        assert_eq!(rejected["status"], "completed", "{rejected}");
        assert_eq!(rejected["_meta"], json!({"horon-agent": {"verdict": "rejected"}}));

        let failed = final_update(
            profile,
            json!({"type":"tool_done","id":"tool1","name":"bash","output":"","error":"boom","verdict":"failed"}),
        );
        assert_eq!(failed["status"], "failed", "{failed}");
        assert_eq!(failed["_meta"], json!({"horon-agent": {"verdict": "failed"}}));
    }
}

#[test]
fn v1_live_final_update_without_a_verdict_is_unchanged() {
    let plain = final_update(
        AcpProfile::Standard,
        json!({"type":"tool_done","id":"tool1","name":"bash","output":"ok"}),
    );
    assert!(plain.get("_meta").is_none(), "no verdict, no `_meta`: {plain}");
    let built = {
        let mut mapper = EventMapper::new(AcpProfile::Standard, MapperOptions::default(), "t1".to_string());
        mapper.map_event(event(json!({"type":"tool_start","id":"tool1","name":"bash"})));
        mapper
            .map_event(ServerEvent::tool_done("tool1", "bash", "ok", None))
            .remove(0)
    };
    assert_eq!(plain, built, "the wire event and the constructor map alike");
}

#[test]
fn v1_replay_names_the_stored_verdict_under_meta() {
    let history: Vec<HistoryMessage> = serde_json::from_value(json!([
        {"role":"user","content":"read both"},
        {"role":"assistant","content":"reading","tool_calls":["read","read"]},
        {"role":"tool","content":"boom","verdict":"failed",
            "tool_data":{"id":"call_1","name":"read","input":{"file_path":"a.rs"}}},
        {"role":"tool","content":"fn main() {}",
            "tool_data":{"id":"call_2","name":"read","input":{"file_path":"b.rs"}}}
    ]))
    .expect("history rows");
    let updates = replay_updates(&history, &[], &MapperOptions::default());
    let done = |id: &str| {
        updates
            .iter()
            .find(|update| update["sessionUpdate"] == "tool_call_update" && update["toolCallId"] == id)
            .unwrap_or_else(|| panic!("no final update for {id}: {updates:?}"))
    };
    let failed = done("call_1");
    assert_eq!(
        failed["status"], "completed",
        "history keeps no failed status: {failed}"
    );
    assert_eq!(failed["_meta"], json!({"horon-agent": {"verdict": "failed"}}));
    assert!(
        done("call_2").get("_meta").is_none(),
        "no verdict, no `_meta`: {updates:?}"
    );
    let carrying = updates.iter().filter(|update| update.get("_meta").is_some()).count();
    assert_eq!(
        carrying, 1,
        "only the final update of that call carries it: {updates:?}"
    );
}

/// The verdict is reported without a capability: neither `initialize` result
/// mentions it, so both keep the bytes they had.
#[test]
fn initialize_results_do_not_mention_the_verdict() {
    for profile in [AcpProfile::Standard, AcpProfile::Extended] {
        let v1 = initialize_result(profile).to_string();
        assert!(!v1.contains("verdict"), "{v1}");
        let v2 = v2::shapes::initialize_response(profile, false).to_string();
        assert!(!v2.contains("verdict"), "{v2}");
    }
}
