//! IS-6 to IS-11 for the v2 session update mapper. Every frame a test maps is
//! validated against the vendored `schema-v2.0.0-alpha.13` and checked for v1-only
//! shapes.

use super::mapper::{V2Mapper, config_option_update};
use super::tests::assert_update_matches;
use crate::protocol::ServerEvent;
use serde_json::{Value, json};
use std::path::PathBuf;

const CWD: &str = "/work";

fn mapper() -> V2Mapper {
    V2Mapper::new("m1", Some(PathBuf::from(CWD)))
}

/// A v1-only variant or field anywhere in `frame`.
fn v1_only(frame: &Value) -> Option<String> {
    if let Some(kind) = frame.get("sessionUpdate").and_then(Value::as_str)
        && ["tool_call", "plan", "current_mode_update"].contains(&kind)
    {
        return Some(format!("variant {kind}"));
    }
    match frame {
        Value::Object(object) => object.iter().find_map(|(key, value)| {
            if key == "oldText" || key == "newText" {
                Some(format!("field {key}"))
            } else {
                v1_only(value)
            }
        }),
        Value::Array(items) => items.iter().find_map(v1_only),
        _ => None,
    }
}

#[track_caller]
fn check(frames: &[Value]) {
    for frame in frames {
        assert_update_matches(frame);
        assert_eq!(v1_only(frame), None, "{frame}");
    }
}

#[track_caller]
fn map(mapper: &mut V2Mapper, event: Value) -> Vec<Value> {
    let event: ServerEvent = serde_json::from_value(event.clone()).unwrap_or_else(|err| panic!("{event}: {err}"));
    let frames = mapper.map_event(event);
    check(&frames);
    frames
}

#[track_caller]
fn map_all(mapper: &mut V2Mapper, events: &[Value]) -> Vec<Value> {
    events.iter().flat_map(|event| map(mapper, event.clone())).collect()
}

#[track_caller]
fn end(mapper: &mut V2Mapper, stop_reason: &str) -> Vec<Value> {
    let frames = mapper.run_ended(Some(stop_reason));
    check(&frames);
    frames
}

fn text(delta: &str) -> Value {
    json!({"type":"text_delta","text":delta})
}

fn thought(delta: &str) -> Value {
    json!({"type":"reasoning_delta","text":delta})
}

fn start(id: &str, name: &str) -> Value {
    json!({"type":"tool_start","id":id,"name":name})
}

fn input(id: &str, input: &Value) -> Value {
    json!({"type":"tool_input","id":id,"delta":input.to_string()})
}

fn exec(id: &str, name: &str) -> Value {
    json!({"type":"tool_exec","id":id,"name":name})
}

fn done(id: &str, name: &str, output: &str) -> Value {
    json!({"type":"tool_done","id":id,"name":name,"output":output})
}

fn exit(id: &str, code: i32) -> Value {
    json!({"type":"tool_exit","id":id,"exit_code":code})
}

/// A whole tool round: start, input, exec, done.
fn round(id: &str, name: &str, args: &Value, output: &str) -> Vec<Value> {
    vec![start(id, name), input(id, args), exec(id, name), done(id, name, output)]
}

fn of_kind<'a>(frames: &'a [Value], kind: &str) -> Vec<&'a Value> {
    frames.iter().filter(|frame| frame["sessionUpdate"] == kind).collect()
}

fn terminal_updates(frames: &[Value]) -> Vec<&Value> {
    of_kind(frames, "terminal_update")
}

/// The complete output a terminal update carries, as the base64 the v2
/// `TerminalOutput` requires.
#[track_caller]
fn assert_output(update: &Value, expected: &str) {
    assert_eq!(
        update["output"]["data"],
        crate::acp_render::base64_encode(expected.as_bytes()),
        "{update}"
    );
}

#[test]
fn chunks_carry_message_ids() {
    let mut mapper = mapper();
    let frames = map_all(&mut mapper, &[thought("hmm"), text("hel"), text("lo")]);
    assert_eq!(
        frames,
        [
            json!({"sessionUpdate":"agent_thought_chunk","messageId":"m1:t1","content":{"type":"text","text":"hmm"}}),
            json!({"sessionUpdate":"agent_message_chunk","messageId":"m1:a1","content":{"type":"text","text":"hel"}}),
            json!({"sessionUpdate":"agent_message_chunk","messageId":"m1:a1","content":{"type":"text","text":"lo"}}),
        ]
    );
    // A run that stored no message is keyed by its run id.
    let mut by_run = V2Mapper::new("run-7", Some(PathBuf::from(CWD)));
    assert_eq!(map(&mut by_run, text("x"))[0]["messageId"], "run-7:a1");
}

#[test]
fn ids_advance_after_tool_round() {
    let mut mapper = mapper();
    let mut events = vec![text("before")];
    // Two calls in one round advance the ids once.
    events.extend(round("c1", "read", &json!({"path":"a"}), "A"));
    events.extend(round("c2", "read", &json!({"path":"b"}), "B"));
    events.extend([thought("next"), text("after")]);
    events.extend(round("c3", "ls", &json!({}), ""));
    events.push(text("last"));
    let frames = map_all(&mut mapper, &events);
    let ids: Vec<&str> = frames
        .iter()
        .filter(|frame| {
            frame["sessionUpdate"]
                .as_str()
                .is_some_and(|kind| kind.ends_with("_chunk"))
        })
        .filter_map(|frame| frame.get("messageId").and_then(Value::as_str))
        .collect();
    assert_eq!(ids, ["m1:a1", "m1:t2", "m1:a2", "m1:a3"], "{frames:?}");
}

#[test]
fn text_replace_becomes_whole_message() {
    let mut mapper = mapper();
    map_all(&mut mapper, &[text("clean "), text("<garbled tool markup")]);
    let frames = map(&mut mapper, json!({"type":"text_replace","text":"clean"}));
    assert_eq!(
        frames,
        [json!({"sessionUpdate":"agent_message","messageId":"m1:a1","content":[{"type":"text","text":"clean"}]})]
    );
    // The replacement is the message text later corrections apply to.
    let frames = map(
        &mut mapper,
        json!({"type":"text_suffix_replace","old_text":"zz","new_text":"AN"}),
    );
    assert_eq!(frames[0]["content"], json!([{"type":"text","text":"clean\nAN"}]));
}

#[test]
fn suffix_replace_mismatch_appends_like_v1() {
    let mut mapper = mapper();
    map_all(&mut mapper, &[text("see "), text("[1]")]);
    let matched = map(
        &mut mapper,
        json!({"type":"text_suffix_replace","old_text":"[1]","new_text":"[docs](https://x)"}),
    );
    assert_eq!(
        matched,
        [json!({"sessionUpdate":"agent_message","messageId":"m1:a1",
            "content":[{"type":"text","text":"see [docs](https://x)"}]})]
    );
    let appended = map(
        &mut mapper,
        json!({"type":"text_suffix_replace","old_text":"not the end","new_text":"Sources: x"}),
    );
    assert_eq!(
        appended[0]["content"],
        json!([{"type":"text","text":"see [docs](https://x)\nSources: x"}])
    );
}

#[test]
fn first_tool_update_is_minimal_then_input_patch() {
    let mut mapper = mapper();
    let created = map(&mut mapper, start("c1", "edit"));
    assert_eq!(
        created,
        [
            json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","name":"edit","title":"Editing files",
            "kind":"edit","status":"pending"})
        ]
    );
    let args = json!({"file_path":"src/lib.rs","old_string":"a","new_string":"b"});
    let delta = args.to_string();
    let (head, tail) = delta.split_at(delta.len() / 2);
    for part in [head, tail] {
        assert!(
            map(&mut mapper, json!({"type":"tool_input","id":"c1","delta":part})).is_empty(),
            "input streams without updates"
        );
    }
    let patched = map(&mut mapper, exec("c1", "edit"));
    assert_eq!(
        patched,
        [
            json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"in_progress",
            "rawInput":args,"locations":[{"path":"/work/src/lib.rs"}]})
        ]
    );
}

#[test]
fn first_tool_update_creates_call() {
    // A call first seen at its execution or its result is still created by
    // the first update the client sees for it.
    for first in [exec("c9", "read"), done("c9", "read", "text")] {
        let mut mapper = mapper();
        let frames = map(&mut mapper, first);
        assert_eq!(
            frames[0],
            json!({"sessionUpdate":"tool_call_update","toolCallId":"c9","name":"read","title":"Reading file",
                "kind":"read","status":"pending"}),
            "{frames:?}"
        );
    }
}

#[test]
fn diff_before_tool_start_is_buffered() {
    let mut mapper = mapper();
    let diff = json!({"type":"tool_diff","id":"c1","diffs":[{"path":"a.txt","old_text":"x\n","new_text":"y\n"}]});
    assert!(map(&mut mapper, diff).is_empty(), "nothing before the creating update");
    let frames = map(&mut mapper, start("c1", "edit"));
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame["sessionUpdate"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["tool_call_update", "tool_call_content_chunk"]
    );
    assert_eq!(frames[0]["status"], "pending");
    assert_eq!(frames[1]["content"]["type"], "diff");

    // A call whose start never comes is created when the run ends.
    let orphan = json!({"type":"tool_diff","id":"c2","diffs":[{"path":"/abs/b.txt","new_text":"new\n"}]});
    assert!(map(&mut mapper, orphan).is_empty());
    let frames = end(&mut mapper, "end_turn");
    assert_eq!(frames.len(), 2, "{frames:?}");
    assert_eq!(frames[0]["toolCallId"], "c2");
    assert_eq!(frames[1]["content"]["changes"][0]["operation"], "add");
}

#[test]
fn diff_becomes_changes_and_git_patch() {
    let mut mapper = mapper();
    map(&mut mapper, start("c1", "edit"));
    let frames = map(
        &mut mapper,
        json!({"type":"tool_diff","id":"c1","diffs":[{"path":"src/a.txt","old_text":"old\n","new_text":"new\n"}]}),
    );
    let content = &frames[0]["content"];
    assert_eq!(frames[0]["sessionUpdate"], "tool_call_content_chunk");
    assert_eq!(
        content["changes"],
        json!([{"operation":"modify","path":"/work/src/a.txt","fileType":"text"}])
    );
    assert_eq!(content["patch"]["format"], "git_patch");
    let patch = content["patch"]["text"].as_str().expect("patch text");
    for line in ["--- /work/src/a.txt", "+++ /work/src/a.txt", "-old", "+new"] {
        assert!(
            patch.lines().any(|patch_line| patch_line == line),
            "{line:?} in {patch}"
        );
    }

    // A write to a new file is an add against /dev/null.
    map(&mut mapper, start("c2", "write"));
    let frames = map(
        &mut mapper,
        json!({"type":"tool_diff","id":"c2","diffs":[{"path":"new.txt","new_text":"hello\n"}]}),
    );
    let content = &frames[0]["content"];
    assert_eq!(
        content["changes"],
        json!([{"operation":"add","path":"/work/new.txt","fileType":"text"}])
    );
    let patch = content["patch"]["text"].as_str().expect("patch text");
    assert!(patch.lines().any(|line| line == "--- /dev/null"), "{patch}");
}

#[test]
fn delete_and_move_diffs_map_operations() {
    let mut mapper = mapper();
    map(&mut mapper, start("c1", "apply_patch"));
    let frames = map(
        &mut mapper,
        json!({"type":"tool_diff","id":"c1","diffs":[
            {"path":"gone.txt","old_text":"bye\n","new_text":"","deleted":true},
            {"path":"to.txt","old_path":"from.txt","old_text":"same\n","new_text":"same\n"},
        ]}),
    );
    let content = &frames[0]["content"];
    assert_eq!(
        content["changes"],
        json!([
            {"operation":"delete","path":"/work/gone.txt","fileType":"text"},
            {"operation":"move","oldPath":"/work/from.txt","path":"/work/to.txt","fileType":"text"},
        ])
    );
    let patch = content["patch"]["text"].as_str().expect("patch text");
    assert!(patch.lines().any(|line| line == "+++ /dev/null"), "{patch}");
}

#[test]
fn bash_is_display_terminal_not_text() {
    let mut mapper = mapper();
    let mut events = round("c1", "bash", &json!({"command":"echo hi"}), "hi\n");
    events.insert(3, exit("c1", 0));
    let frames = map_all(&mut mapper, &events);
    let kinds: Vec<&str> = frames
        .iter()
        .map(|frame| frame["sessionUpdate"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "tool_call_update",
            "tool_call_update",
            "tool_call_content_chunk",
            "terminal_update",
            "terminal_update",
            "tool_call_update",
        ],
        "{frames:?}"
    );
    assert_eq!(frames[2]["content"], json!({"type":"terminal","terminalId":"c1"}));
    assert_eq!(
        frames[3],
        json!({"sessionUpdate":"terminal_update","terminalId":"c1","command":"echo hi","cwd":"/work"})
    );
    assert_eq!(frames[4]["output"]["data"], "aGkK", "base64 of \"hi\\n\"");
    assert_eq!(frames[5]["status"], "completed");
    assert!(
        of_kind(&frames, "tool_call_content_chunk")
            .iter()
            .all(|chunk| chunk["content"]["type"] == "terminal"),
        "bash output is never text content: {frames:?}"
    );
}

#[test]
fn bash_exit_code_in_exit_status() {
    let mut mapper = mapper();
    let mut events = round("c1", "bash", &json!({"command":"exit 3"}), "");
    events.insert(3, exit("c1", 3));
    let frames = map_all(&mut mapper, &events);
    let last = *terminal_updates(&frames).last().expect("a final terminal update");
    assert_eq!(last["exitStatus"], json!({"exitCode":3,"signal":null}), "{frames:?}");

    // An exit read after the result reports the same output again, exited.
    let mut events = round("c2", "bash", &json!({"command":"kill -9 $$"}), "partial");
    events.push(json!({"type":"tool_exit","id":"c2","signal":"SIGKILL"}));
    let frames = map_all(&mut mapper, &events);
    let updates = terminal_updates(&frames);
    assert!(updates[1].get("exitStatus").is_none(), "{frames:?}");
    assert_eq!(updates[2]["exitStatus"], json!({"exitCode":null,"signal":"SIGKILL"}));
    assert_output(updates[2], "partial");
}

#[test]
fn background_bash_has_no_exit_status() {
    let mut mapper = mapper();
    let mut events = round(
        "c1",
        "bash",
        &json!({"command":"sleep 100","run_in_background":true}),
        "started bg-1",
    );
    events.insert(3, json!({"type":"tool_exit","id":"c1","backgrounded":true}));
    let frames = map_all(&mut mapper, &events);
    let last = *terminal_updates(&frames).last().expect("an output snapshot");
    assert_output(last, "started bg-1");
    assert!(last.get("exitStatus").is_none(), "{last}");

    // No exit report at all: the output, still without an exit status.
    let frames = map_all(&mut mapper, &round("c2", "bash", &json!({"command":"true"}), "ok"));
    let last = *terminal_updates(&frames).last().expect("an output snapshot");
    assert!(last.get("exitStatus").is_none(), "{last}");
}

#[test]
fn plan_is_plan_update_items() {
    let mut mapper = mapper();
    let frames = map(
        &mut mapper,
        json!({"type":"plan_updated","entries":[{"content":"write tests","priority":"high","status":"in_progress"}]}),
    );
    assert_eq!(
        frames,
        [
            json!({"sessionUpdate":"plan_update","plan":{"type":"items","planId":"todo",
            "entries":[{"content":"write tests","priority":"high","status":"in_progress"}]}})
        ]
    );
}

#[test]
fn open_tool_cancelled_on_cancel() {
    let mut mapper = mapper();
    map_all(&mut mapper, &round("finished", "read", &json!({"path":"a"}), "A"));
    map_all(
        &mut mapper,
        &[
            start("open", "bash"),
            input("open", &json!({"command":"sleep 9"})),
            exec("open", "bash"),
        ],
    );
    let frames = end(&mut mapper, "cancelled");
    assert_eq!(
        frames,
        [json!({"sessionUpdate":"tool_call_update","toolCallId":"open","status":"cancelled"})]
    );

    let mut mapper = super::mapper::V2Mapper::new("m2", Some(PathBuf::from(CWD)));
    map(&mut mapper, start("open", "read"));
    assert!(
        end(&mut mapper, "end_turn").is_empty(),
        "only a cancelled run cancels its calls"
    );
}

/// A run that errors or whose connection goes away ends its open calls as
/// failed, so nothing is left `in_progress` after the idle `error` update.
#[test]
fn open_tool_failed_on_error() {
    let mut mapper = mapper();
    map_all(&mut mapper, &round("finished", "read", &json!({"path":"a"}), "A"));
    map_all(
        &mut mapper,
        &[
            start("open", "bash"),
            input("open", &json!({"command":"sleep 9"})),
            exec("open", "bash"),
        ],
    );
    assert_eq!(
        end(&mut mapper, "error"),
        [json!({"sessionUpdate":"tool_call_update","toolCallId":"open","status":"failed"})],
        "a completed call keeps its result"
    );
}

#[test]
fn no_v1_only_variants() {
    let mut mapper = mapper();
    let mut events = vec![thought("t"), text("a")];
    events.extend(round(
        "e",
        "edit",
        &json!({"file_path":"/work/x","old_string":"o","new_string":"n"}),
        "ok",
    ));
    events.insert(
        5,
        json!({"type":"tool_diff","id":"e","diffs":[{"path":"x","old_text":"o\n","new_text":"n\n"}]}),
    );
    events.extend(round("b", "bash", &json!({"command":"echo hi"}), "hi\n"));
    events.extend([
        exit("b", 0),
        json!({"type":"text_replace","text":"a"}),
        json!({"type":"text_suffix_replace","old_text":"a","new_text":"b"}),
        json!({"type":"plan_updated","entries":[{"content":"c","priority":"low","status":"pending"}]}),
        json!({"type":"compaction","trigger":"background"}),
        json!({"type":"provider_guardrail","message":"refused"}),
        json!({"type":"turn_stopped","reason":"failure","message":"boom"}),
        json!({"type":"notification","from_session":"s2",
            "notification_type":{"kind":"shared_context","key":"k","value":"v"},"message":"hello"}),
        json!({"type":"session_renamed","session_id":"s1","display_title":"Title"}),
        json!({"type":"generated_image","id":"img","path":"/work/i.png","output_format":"png"}),
        start("open", "write"),
    ]);
    let mut frames = map_all(&mut mapper, &events);
    frames.extend(end(&mut mapper, "cancelled"));
    let kinds: std::collections::BTreeSet<&str> = frames
        .iter()
        .map(|frame| frame["sessionUpdate"].as_str().unwrap())
        .collect();
    for expected in [
        "agent_thought_chunk",
        "agent_message_chunk",
        "agent_message",
        "tool_call_update",
        "tool_call_content_chunk",
        "terminal_update",
        "plan_update",
        "compaction_update",
        "notice",
        "session_info_update",
    ] {
        assert!(kinds.contains(expected), "{expected} missing from {kinds:?}");
    }
    let compaction = of_kind(&frames, "compaction_update")[0];
    assert_eq!(
        compaction,
        &json!({"sessionUpdate":"compaction_update","compactionId":"m1:c1","status":"completed"})
    );
    let notices: Vec<(&str, &str)> = of_kind(&frames, "notice")
        .iter()
        .map(|notice| (notice["severity"].as_str().unwrap(), notice["title"].as_str().unwrap()))
        .collect();
    assert_eq!(notices, [("warning", "refused"), ("error", "boom"), ("info", "hello")]);

    let update = config_option_update(
        &[json!({"id":"mode","name":"Mode","type":"select","currentValue":"code",
        "options":[{"value":"code","name":"Code"}]})],
    );
    check(std::slice::from_ref(&update));
    assert_eq!(update["configOptions"][0]["configId"], "mode");
}
