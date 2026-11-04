use super::*;
use futures::{FutureExt, StreamExt, channel::mpsc};
use serde_json::json;

type Sender = mpsc::UnboundedSender<Result<Bytes, reqwest::Error>>;

fn stream() -> (Sender, OpenAIResponsesStream) {
    let (tx, rx) = mpsc::unbounded();
    (tx, OpenAIResponsesStream::new(rx))
}

fn send(tx: &Sender, event: Value) {
    tx.unbounded_send(Ok(Bytes::from(format!("data: {event}\n\n"))))
        .unwrap();
}

fn next(stream: &mut OpenAIResponsesStream) -> StreamEvent {
    // The upstream connection remains open, with no later event supplied. A
    // buffering parser returns Pending here instead of passing this assertion.
    stream
        .next()
        .now_or_never()
        .expect("event must be visible now")
        .expect("stream remains open")
        .expect("valid stream event")
}

fn idle(stream: &mut OpenAIResponsesStream) {
    assert!(stream.next().now_or_never().is_none());
}

fn added(tx: &Sender, id: &str, name: &str) {
    send(
        tx,
        json!({"type":"response.output_item.added", "item": {
            "type":"function_call", "id":id, "call_id":format!("call_{id}"),
            "name":name, "arguments":""
        }}),
    );
}

fn delta(tx: &Sender, id: &str, fragment: &str) {
    send(
        tx,
        json!({"type":"response.function_call_arguments.delta", "item_id":id, "delta":fragment}),
    );
}

fn done(tx: &Sender, id: &str, arguments: &str) {
    send(
        tx,
        json!({"type":"response.function_call_arguments.done", "item_id":id, "arguments":arguments}),
    );
}

fn assert_start(stream: &mut OpenAIResponsesStream, id: &str, name: &str) {
    assert!(
        matches!(next(stream), StreamEvent::ToolUseStart { id: actual_id, name: actual_name }
        if actual_id == format!("call_{id}") && actual_name == name)
    );
}

fn assert_delta(stream: &mut OpenAIResponsesStream, id: &str, expected: &str) {
    assert!(
        matches!(next(stream), StreamEvent::ToolInputDeltaFor { id: actual_id, delta }
        if actual_id == format!("call_{id}") && delta == expected)
    );
}

fn assert_end(stream: &mut OpenAIResponsesStream, id: &str) {
    assert!(matches!(next(stream), StreamEvent::ToolUseEndFor { id: actual_id }
        if actual_id == format!("call_{id}")));
}

#[test]
fn tool_name_and_argument_fragments_are_visible_before_done() {
    for name in ["read", "batch", "multi_tool_use.parallel"] {
        let (tx, mut stream) = stream();
        added(&tx, "a", name);
        assert_start(&mut stream, "a", name);
        idle(&mut stream);
        delta(&tx, "a", "{\"intent\":\"Read files\",");
        assert_delta(&mut stream, "a", "{\"intent\":\"Read files\",");
        idle(&mut stream);
        delta(&tx, "a", "\"path\":\"文档\"}");
        assert_delta(&mut stream, "a", "\"path\":\"文档\"}");
        idle(&mut stream);
        done(&tx, "a", "{\"intent\":\"Read files\",\"path\":\"文档\"}");
        assert_end(&mut stream, "a");
        idle(&mut stream);
    }
}

#[test]
fn done_snapshots_only_emit_unseen_suffix_and_never_duplicate_calls() {
    let (tx, mut stream) = stream();
    added(&tx, "a", "read");
    assert_start(&mut stream, "a", "read");
    delta(&tx, "a", "{\"path\":");
    assert_delta(&mut stream, "a", "{\"path\":");
    done(&tx, "a", "{\"path\":\"README.md\"}");
    assert_delta(&mut stream, "a", "\"README.md\"}");
    assert_end(&mut stream, "a");
    for _ in 0..2 {
        done(&tx, "a", "{\"path\":\"README.md\"}");
        send(
            &tx,
            json!({"type":"response.output_item.done", "item":{
                "id":"a", "type":"function_call", "call_id":"call_a", "name":"read",
                "arguments":"{\"path\":\"README.md\"}"
            }}),
        );
        idle(&mut stream);
    }
}

#[test]
fn output_item_done_finishes_started_call_without_arguments_done() {
    let (tx, mut stream) = stream();
    added(&tx, "a", "read");
    assert_start(&mut stream, "a", "read");
    delta(&tx, "a", "{");
    assert_delta(&mut stream, "a", "{");
    send(
        &tx,
        json!({"type":"response.output_item.done", "item":{
            "id":"a", "type":"function_call", "call_id":"call_a", "name":"read", "arguments":"{}"
        }}),
    );
    assert_delta(&mut stream, "a", "}");
    assert_end(&mut stream, "a");
    idle(&mut stream);
    assert!(stream.streaming_tool_calls.is_empty());
}

#[test]
fn stalled_first_call_does_not_block_other_names_arguments_or_completion() {
    let (tx, mut stream) = stream();
    added(&tx, "a", "read");
    assert_start(&mut stream, "a", "read");
    idle(&mut stream);
    added(&tx, "b", "bash");
    assert_start(&mut stream, "b", "bash");
    idle(&mut stream);
    delta(&tx, "b", "{\"command\":");
    assert_delta(&mut stream, "b", "{\"command\":");
    added(&tx, "c", "ls");
    assert_start(&mut stream, "c", "ls");
    idle(&mut stream);
    delta(&tx, "a", "{\"path\":");
    assert_delta(&mut stream, "a", "{\"path\":");
    done(&tx, "c", "");
    assert_delta(&mut stream, "c", "{}");
    assert_end(&mut stream, "c");
    done(&tx, "b", "{\"command\":\"pwd\"}");
    assert_delta(&mut stream, "b", "\"pwd\"}");
    assert_end(&mut stream, "b");
    idle(&mut stream);
    done(&tx, "a", "{\"path\":\"README.md\"}");
    assert_delta(&mut stream, "a", "\"README.md\"}");
    assert_end(&mut stream, "a");
    idle(&mut stream);
}

#[test]
fn emitted_id_stays_stable_when_call_id_arrives_late() {
    let (tx, mut stream) = stream();
    send(
        &tx,
        json!({"type":"response.output_item.added", "item":{
            "type":"function_call", "id":"item.a", "name":"read", "arguments":""
        }}),
    );
    let StreamEvent::ToolUseStart { id, .. } = next(&mut stream) else {
        panic!("expected start")
    };
    assert_eq!(id, sanitize_tool_id("item.a"));
    send(
        &tx,
        json!({"type":"response.function_call_arguments.delta", "item_id":"item.a",
        "call_id":"late_call", "delta":"{"}),
    );
    assert!(
        matches!(next(&mut stream), StreamEvent::ToolInputDeltaFor { id: actual, delta }
        if actual == id && delta == "{")
    );
    send(
        &tx,
        json!({"type":"response.output_item.done", "item":{
            "type":"function_call", "id":"item.a", "call_id":"different_call", "name":"read", "arguments":"{}"
        }}),
    );
    assert!(
        matches!(next(&mut stream), StreamEvent::ToolInputDeltaFor { id: actual, delta }
        if actual == id && delta == "}")
    );
    assert!(matches!(next(&mut stream), StreamEvent::ToolUseEndFor { id: actual } if actual == id));
    idle(&mut stream);
}

#[test]
fn late_name_releases_accumulated_arguments_without_waiting_for_done() {
    let (tx, mut stream) = stream();
    delta(&tx, "a", "{");
    idle(&mut stream);
    send(
        &tx,
        json!({"type":"response.function_call_arguments.delta", "item_id":"a",
        "call_id":"call_a", "name":"read", "delta":"\"path\":"}),
    );
    assert_start(&mut stream, "a", "read");
    assert_delta(&mut stream, "a", "{\"path\":");
    idle(&mut stream);
}

#[test]
fn done_only_call_keeps_compatibility() {
    let (tx, mut stream) = stream();
    send(
        &tx,
        json!({"type":"response.function_call_arguments.done", "item_id":"a",
        "call_id":"call_a", "name":"read", "arguments":"{}"}),
    );
    assert_start(&mut stream, "a", "read");
    assert_delta(&mut stream, "a", "{}");
    assert_end(&mut stream, "a");
    idle(&mut stream);
}

#[test]
fn null_and_empty_arguments_are_normalized_without_delaying_start() {
    for arguments in ["", " ", "null", " null "] {
        let (tx, mut stream) = stream();
        added(&tx, "a", "ls");
        assert_start(&mut stream, "a", "ls");
        for ch in arguments.chars() {
            delta(&tx, "a", &ch.to_string());
            idle(&mut stream);
        }
        done(&tx, "a", arguments);
        assert_delta(&mut stream, "a", "{}");
        assert_end(&mut stream, "a");
        idle(&mut stream);
    }
}

#[test]
fn mismatched_done_arguments_fail_instead_of_corrupting_tool_input() {
    let (tx, mut stream) = stream();
    added(&tx, "a", "read");
    assert_start(&mut stream, "a", "read");
    delta(&tx, "a", "{\"path\":\"文档");
    assert_delta(&mut stream, "a", "{\"path\":\"文档");
    done(&tx, "a", "{}");
    assert!(matches!(next(&mut stream), StreamEvent::Error { .. }));
    idle(&mut stream);
}

#[test]
fn custom_tool_input_events_stream_before_completion() {
    let (tx, mut stream) = stream();
    send(
        &tx,
        json!({"type":"response.output_item.added", "item":{
            "id":"a", "type":"custom_tool_call", "call_id":"call_a", "name":"apply_patch", "input":""
        }}),
    );
    assert_start(&mut stream, "a", "apply_patch");
    send(
        &tx,
        json!({"type":"response.custom_tool_call_input.delta", "item_id":"a", "delta":"*** Begin Patch\n"}),
    );
    assert_delta(&mut stream, "a", "*** Begin Patch\n");
    idle(&mut stream);
    send(
        &tx,
        json!({"type":"response.custom_tool_call_input.done", "item_id":"a", "input":"*** Begin Patch\n*** End Patch"}),
    );
    assert_delta(&mut stream, "a", "*** End Patch");
    assert_end(&mut stream, "a");
    idle(&mut stream);
}

#[test]
fn unnamed_first_call_does_not_block_a_named_call() {
    let (tx, mut stream) = stream();
    delta(&tx, "a", "{");
    idle(&mut stream);
    added(&tx, "b", "ls");
    assert_start(&mut stream, "b", "ls");
    done(&tx, "b", "{}");
    assert_delta(&mut stream, "b", "{}");
    assert_end(&mut stream, "b");
    idle(&mut stream);
    send(
        &tx,
        json!({"type":"response.function_call_arguments.done", "item_id":"a",
        "call_id":"call_a", "name":"read", "arguments":"{}"}),
    );
    assert_start(&mut stream, "a", "read");
    assert_delta(&mut stream, "a", "{}");
    assert_end(&mut stream, "a");
    idle(&mut stream);
}

#[test]
fn recovered_tool_call_uses_its_own_id_while_another_call_is_open() {
    let (tx, mut stream) = stream();
    added(&tx, "a", "read");
    assert_start(&mut stream, "a", "read");
    send(
        &tx,
        json!({"type":"response.output_text.delta",
        "delta":"to=functions.ls {}"}),
    );
    let StreamEvent::ToolUseStart { id, name } = next(&mut stream) else {
        panic!("expected recovered tool start");
    };
    assert_eq!(name, "ls");
    assert!(id.starts_with("fallback_text_call_"));
    assert!(
        matches!(next(&mut stream), StreamEvent::ToolInputDeltaFor { id: actual, delta }
        if actual == id && delta == "{}")
    );
    assert!(matches!(next(&mut stream), StreamEvent::ToolUseEndFor { id: actual } if actual == id));
    done(&tx, "a", "{}");
    assert_delta(&mut stream, "a", "{}");
    assert_end(&mut stream, "a");
    idle(&mut stream);
}

// Match the Copilot Responses shape: every event has a fresh item ID, while
// output_index is stable and call_id is supplied only on item events.
fn assert_any_delta(stream: &mut OpenAIResponsesStream, expected: &str) {
    let event = next(stream);
    assert!(
        matches!(&event, StreamEvent::ToolInputDeltaFor { delta, .. } if delta == expected),
        "expected delta {expected:?}, got {event:?}"
    );
}

fn assert_any_end(stream: &mut OpenAIResponsesStream) {
    let event = next(stream);
    assert!(
        matches!(event, StreamEvent::ToolUseEndFor { .. }),
        "expected tool end, got {event:?}"
    );
}

fn indexed_item(tx: &Sender, index: u64, id: &str, custom: bool, complete: bool, input: &str) {
    let mut item = json!({
        "type": if custom { "custom_tool_call" } else { "function_call" },
        "id": id, "call_id": format!("call_{index}"),
        "name": if custom { "apply_patch" } else { "read" },
    });
    item[if custom { "input" } else { "arguments" }] = json!(input);
    send(
        tx,
        json!({
            "type": if complete { "response.output_item.done" } else { "response.output_item.added" },
            "output_index": index, "item": item,
        }),
    );
}

fn indexed_arguments(tx: &Sender, index: u64, id: &str, custom: bool, complete: bool, input: &str) {
    let kind = if custom {
        "custom_tool_call_input"
    } else {
        "function_call_arguments"
    };
    let mut event = json!({
        "type": format!("response.{kind}.{}", if complete { "done" } else { "delta" }),
        "output_index": index, "item_id": id,
    });
    event[if !complete {
        "delta"
    } else if custom {
        "input"
    } else {
        "arguments"
    }] = json!(input);
    send(tx, event);
}

#[test]
fn unstable_item_ids_stream_incrementally_and_deduplicate_done_snapshots() {
    for custom in [false, true] {
        for item_done_only in [false, true] {
            let (tx, mut stream) = stream();
            let name = if custom { "apply_patch" } else { "read" };
            let fragments = if custom {
                vec!["*** Begin Patch\n", "*** End", " Patch"]
            } else {
                vec!["{\"path\":", "\"文档", "\"}"]
            };
            let full = fragments.concat();
            indexed_item(&tx, 0, "added", custom, false, "");
            assert_start(&mut stream, "0", name);
            idle(&mut stream);
            for (i, fragment) in fragments[..2].iter().enumerate() {
                indexed_arguments(&tx, 0, &format!("delta_{i}"), custom, false, fragment);
                assert_any_delta(&mut stream, fragment);
                idle(&mut stream);
            }
            if item_done_only {
                indexed_item(&tx, 0, "item_done", custom, true, &full);
            } else {
                indexed_arguments(&tx, 0, "arguments_done", custom, true, &full);
            }
            assert_any_delta(&mut stream, fragments[2]);
            assert_any_end(&mut stream);
            indexed_item(&tx, 0, "another_item_done", custom, true, &full);
            indexed_arguments(&tx, 0, "another_arguments_done", custom, true, &full);
            idle(&mut stream);
            assert!(stream.streaming_tool_calls.is_empty());
        }
    }
}

#[test]
fn unstable_item_ids_keep_interleaved_calls_separate() {
    let (tx, mut stream) = stream();
    indexed_item(&tx, 0, "a_added", false, false, "");
    assert_start(&mut stream, "0", "read");
    indexed_arguments(&tx, 0, "a_delta", false, false, "{");
    assert_any_delta(&mut stream, "{");
    indexed_item(&tx, 1, "b_added", true, false, "");
    assert_start(&mut stream, "1", "apply_patch");
    indexed_arguments(&tx, 1, "b_delta", true, false, "*** Begin Patch\n");
    assert_delta(&mut stream, "1", "*** Begin Patch\n");
    indexed_arguments(&tx, 1, "b_done", true, true, "*** Begin Patch\n*** End Patch");
    assert_delta(&mut stream, "1", "*** End Patch");
    assert_end(&mut stream, "1");
    indexed_item(&tx, 1, "b_item_done", true, true, "*** Begin Patch\n*** End Patch");
    idle(&mut stream);
    indexed_item(&tx, 0, "a_item_done", false, true, "{}");
    assert_delta(&mut stream, "0", "}");
    assert_end(&mut stream, "0");
    indexed_item(&tx, 0, "a_duplicate", false, true, "{}");
    indexed_item(&tx, 1, "b_duplicate", true, true, "*** Begin Patch\n*** End Patch");
    idle(&mut stream);
    assert!(stream.streaming_tool_calls.is_empty());
}

#[test]
fn call_id_alias_finishes_call_and_deduplicates_without_output_index() {
    let (tx, mut stream) = stream();
    added(&tx, "a", "read");
    assert_start(&mut stream, "a", "read");
    delta(&tx, "a", "{");
    assert_any_delta(&mut stream, "{");
    for id in ["new", "newer"] {
        send(
            &tx,
            json!({"type":"response.output_item.done", "item":{
                "type":"function_call", "id":id, "call_id":"call_a", "name":"read", "arguments":"{}"
            }}),
        );
        if id == "new" {
            assert_any_delta(&mut stream, "}");
            assert_any_end(&mut stream);
        }
        idle(&mut stream);
    }
}

#[test]
fn item_id_alias_handles_output_index_appearing_after_start() {
    let (tx, mut stream) = stream();
    added(&tx, "a", "read");
    assert_start(&mut stream, "a", "read");
    indexed_arguments(&tx, 2, "a", false, false, "{");
    assert_any_delta(&mut stream, "{");
    indexed_arguments(&tx, 2, "changed", false, true, "{}");
    assert_any_delta(&mut stream, "}");
    assert_any_end(&mut stream);
    done(&tx, "a", "{}");
    idle(&mut stream);
}

#[test]
fn indexed_call_preserves_item_id_fallback_without_call_id() {
    let (tx, mut stream) = stream();
    send(
        &tx,
        json!({"type":"response.output_item.added", "output_index":0, "item":{
            "type":"function_call", "id":"original", "name":"read", "arguments":""
        }}),
    );
    assert!(matches!(next(&mut stream), StreamEvent::ToolUseStart { id, name }
        if id == "original" && name == "read"));
    indexed_arguments(&tx, 0, "changed", false, false, "{");
    assert_any_delta(&mut stream, "{");
    indexed_arguments(&tx, 0, "changed_again", false, true, "{}");
    assert_any_delta(&mut stream, "}");
    assert_any_end(&mut stream);
    idle(&mut stream);
}

#[test]
fn blank_identities_do_not_merge_distinct_indexed_calls() {
    for blank in ["", " \t "] {
        for interleaved in [false, true] {
            for item_id in [None, Some(blank), Some("present")] {
                let (tx, mut stream) = stream();
                let add = |index: u64| {
                    let mut item = json!({"type":"function_call", "call_id":blank,
                        "name":"read", "arguments":""});
                    if let Some(id) = item_id {
                        item["id"] = json!(if id == "present" {
                            format!("item_{index}")
                        } else {
                            id.to_string()
                        });
                    }
                    send(
                        &tx,
                        json!({"type":"response.output_item.added", "output_index":index, "item":item}),
                    );
                };
                let finish = |index: u64| {
                    send(
                        &tx,
                        json!({"type":"response.function_call_arguments.done",
                        "output_index":index, "item_id":blank, "call_id":blank, "arguments":"{}"}),
                    );
                };
                let start_id = |stream: &mut OpenAIResponsesStream| match next(stream) {
                    StreamEvent::ToolUseStart { id, name } => {
                        assert_eq!(name, "read");
                        assert!(!id.trim().is_empty());
                        id
                    }
                    event => panic!("expected distinct tool start, got {event:?}"),
                };
                let finished = |stream: &mut OpenAIResponsesStream, expected: &str| {
                    assert!(matches!(next(stream),
                        StreamEvent::ToolInputDeltaFor { id, delta } if id == expected && delta == "{}"));
                    assert!(matches!(next(stream),
                        StreamEvent::ToolUseEndFor { id } if id == expected));
                };
                add(0);
                let first_id = start_id(&mut stream);
                let second_id = if interleaved {
                    add(1);
                    let second_id = start_id(&mut stream);
                    finish(1);
                    finished(&mut stream, &second_id);
                    idle(&mut stream);
                    finish(0);
                    finished(&mut stream, &first_id);
                    second_id
                } else {
                    finish(0);
                    finished(&mut stream, &first_id);
                    add(1);
                    let second_id = start_id(&mut stream);
                    finish(1);
                    finished(&mut stream, &second_id);
                    second_id
                };
                assert_ne!(first_id, second_id);
                if item_id == Some("present") {
                    assert_eq!(first_id, "item_0");
                    assert_eq!(second_id, "item_1");
                }
                idle(&mut stream);
                assert!(stream.streaming_tool_calls.is_empty());
            }
        }
    }
}

// Captured from the gateway (POST /v1/responses, model combo/deepseek-v4.1-flash)
// for two consecutive tool turns. The delta carries the model's own spacing and
// the gateway re-serializes the same object compactly for `arguments.done` and
// for the item snapshot: one JSON value, two different strings.
const GATEWAY_SPACED_BASH_ARGUMENTS: &str = concat!(
    r#"{"command": "echo 안녕하세요", "intent": "사용자 요청대로 한국어 인사말 출력", "justification": null, "#,
    r#""stall_wake_seconds": null, "timeout": 10000, "wake": null, "notify": null, "pty": null, "#,
    r#""run_in_background": null, "accept_large_output": null}"#,
);
const GATEWAY_COMPACT_BASH_ARGUMENTS: &str = concat!(
    r#"{"command":"echo 안녕하세요","intent":"사용자 요청대로 한국어 인사말 출력","justification":null,"#,
    r#""stall_wake_seconds":null,"timeout":10000,"wake":null,"notify":null,"pty":null,"#,
    r#""run_in_background":null,"accept_large_output":null}"#,
);
const GATEWAY_SPACED_READ_ARGUMENTS: &str = concat!(
    r#"{"file_path": "/private/tmp/repro-ws/notes.txt", "intent": "Read notes.txt as requested", "#,
    r#""hashline": false, "limit": 5000, "start_line": 1, "accept_large_output": false}"#,
);
const GATEWAY_COMPACT_READ_ARGUMENTS: &str = concat!(
    r#"{"file_path":"/private/tmp/repro-ws/notes.txt","intent":"Read notes.txt as requested","#,
    r#""hashline":false,"limit":5000,"start_line":1,"accept_large_output":false}"#,
);

#[derive(Debug, Clone, Copy)]
struct GatewayToolTurn<'a> {
    index: u64,
    item_id: &'a str,
    call_id: &'a str,
    name: &'a str,
    streamed: &'a str,
    snapshot: &'a str,
}

/// Feed one gateway tool turn and return the argument text the consumer saw.
/// Any `StreamEvent::Error` is a failure: the turn must complete.
fn run_gateway_tool_turn<S>(tx: &Sender, stream: &mut S, turn: &GatewayToolTurn<'_>) -> String
where
    S: Stream<Item = Result<StreamEvent>> + Unpin,
{
    let GatewayToolTurn {
        index,
        item_id,
        call_id,
        name,
        streamed,
        snapshot,
    } = *turn;
    send(
        tx,
        json!({"type":"response.output_item.added", "output_index":index, "item":{
            "type":"function_call", "id":item_id, "call_id":call_id, "name":name,
            "arguments":"", "status":"in_progress"
        }}),
    );
    send(
        tx,
        json!({"type":"response.function_call_arguments.delta", "output_index":index,
        "item_id":item_id, "delta":streamed}),
    );
    send(
        tx,
        json!({"type":"response.function_call_arguments.done", "output_index":index,
        "item_id":item_id, "arguments":snapshot}),
    );
    send(
        tx,
        json!({"type":"response.output_item.done", "output_index":index, "item":{
            "type":"function_call", "id":item_id, "call_id":call_id, "name":name,
            "arguments":snapshot, "status":"completed"
        }}),
    );

    let mut events = Vec::new();
    while let Some(Some(Ok(event))) = stream.next().now_or_never() {
        events.push(event);
    }
    for event in &events {
        if let StreamEvent::Error { message, .. } = event {
            panic!("gateway tool turn failed after streaming {streamed:?}: {message}");
        }
    }
    assert!(
        matches!(events.first(), Some(StreamEvent::ToolUseStart { name: started, .. }) if started == name),
        "expected a tool start first, got {events:?}"
    );
    assert!(
        matches!(events.last(), Some(StreamEvent::ToolUseEndFor { .. })),
        "expected the tool call to end, got {events:?}"
    );
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolInputDeltaFor { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn gateway_compact_done_snapshot_completes_two_consecutive_tool_turns() {
    // Both turns on one stream pins intra-response state; the second pass uses a
    // fresh stream per turn, which is how the runtime runs consecutive turns.
    let (tx, mut response_stream) = stream();
    assert_eq!(
        run_gateway_tool_turn(
            &tx,
            &mut response_stream,
            &GatewayToolTurn {
                index: 1,
                item_id: "fc_first",
                call_id: "call_first",
                name: "bash",
                streamed: GATEWAY_SPACED_BASH_ARGUMENTS,
                snapshot: GATEWAY_COMPACT_BASH_ARGUMENTS,
            },
        ),
        GATEWAY_SPACED_BASH_ARGUMENTS
    );
    idle(&mut response_stream);
    assert_eq!(
        run_gateway_tool_turn(
            &tx,
            &mut response_stream,
            &GatewayToolTurn {
                index: 2,
                item_id: "fc_second",
                call_id: "call_second",
                name: "read",
                streamed: GATEWAY_SPACED_READ_ARGUMENTS,
                snapshot: GATEWAY_COMPACT_READ_ARGUMENTS,
            },
        ),
        GATEWAY_SPACED_READ_ARGUMENTS
    );
    idle(&mut response_stream);
    assert!(response_stream.streaming_tool_calls.is_empty());

    for (index, (name, streamed, snapshot)) in [
        ("bash", GATEWAY_SPACED_BASH_ARGUMENTS, GATEWAY_COMPACT_BASH_ARGUMENTS),
        ("read", GATEWAY_SPACED_READ_ARGUMENTS, GATEWAY_COMPACT_READ_ARGUMENTS),
    ]
    .into_iter()
    .enumerate()
    {
        let (tx, mut turn_stream) = stream();
        assert_eq!(
            run_gateway_tool_turn(
                &tx,
                &mut turn_stream,
                &GatewayToolTurn {
                    index: 0,
                    item_id: "fc_turn",
                    call_id: "call_turn",
                    name,
                    streamed,
                    snapshot,
                },
            ),
            streamed,
            "turn {index} must deliver the streamed arguments exactly once"
        );
        idle(&mut turn_stream);
    }
}

#[test]
fn done_snapshot_shorter_than_emitted_arguments_is_accepted_when_json_matches() {
    let (tx, mut stream) = stream();
    assert!(
        GATEWAY_COMPACT_BASH_ARGUMENTS.len() < GATEWAY_SPACED_BASH_ARGUMENTS.len(),
        "the captured compact snapshot must be shorter than the emitted text"
    );
    added(&tx, "a", "bash");
    assert_start(&mut stream, "a", "bash");
    delta(&tx, "a", GATEWAY_SPACED_BASH_ARGUMENTS);
    assert_delta(&mut stream, "a", GATEWAY_SPACED_BASH_ARGUMENTS);
    done(&tx, "a", GATEWAY_COMPACT_BASH_ARGUMENTS);
    // Nothing unseen remains, so the call closes without emitting a second copy.
    assert_end(&mut stream, "a");
    idle(&mut stream);
}

#[test]
fn identical_done_snapshot_after_full_emission_emits_no_duplicate() {
    let (tx, mut stream) = stream();
    added(&tx, "a", "bash");
    assert_start(&mut stream, "a", "bash");
    delta(&tx, "a", GATEWAY_COMPACT_BASH_ARGUMENTS);
    assert_delta(&mut stream, "a", GATEWAY_COMPACT_BASH_ARGUMENTS);
    done(&tx, "a", GATEWAY_COMPACT_BASH_ARGUMENTS);
    assert_end(&mut stream, "a");
    idle(&mut stream);
}

#[test]
fn different_json_arguments_still_fail_instead_of_being_accepted() {
    let (tx, mut stream) = stream();
    added(&tx, "a", "bash");
    assert_start(&mut stream, "a", "bash");
    delta(&tx, "a", "{\"command\":\"echo a\"}");
    assert_delta(&mut stream, "a", "{\"command\":\"echo a\"}");
    done(&tx, "a", "{\"command\":\"echo b\"}");
    assert!(matches!(next(&mut stream), StreamEvent::Error { .. }));
    idle(&mut stream);
}

#[test]
fn blank_snapshot_call_id_does_not_replace_buffered_stable_id() {
    let (tx, mut stream) = stream();
    indexed_item(&tx, 0, "a", false, false, "");
    assert_start(&mut stream, "0", "read");
    indexed_item(&tx, 1, "b", false, false, "");
    assert_start(&mut stream, "1", "read");
    send(
        &tx,
        json!({"type":"response.output_item.done", "output_index":1, "item":{
            "type":"function_call", "id":"b_done", "call_id":" \t ", "name":"read", "arguments":"{}"
        }}),
    );
    // The blank snapshot call_id must not replace the stable streamed id.
    assert_delta(&mut stream, "1", "{}");
    assert_end(&mut stream, "1");
    idle(&mut stream);
    indexed_arguments(&tx, 0, "a_done", false, true, "{}");
    assert_delta(&mut stream, "0", "{}");
    assert_end(&mut stream, "0");
    idle(&mut stream);
}

/// One scripted turn covering every family the parser routes: text, reasoning, a streamed
/// tool call with a done snapshot, a message boundary, usage and both terminal markers.
fn scripted_json_frames() -> Vec<String> {
    [
        json!({"type":"response.created","response":{"id":"resp_1"}}),
        json!({"type":"response.reasoning_summary_text.delta","delta":"thinking"}),
        json!({"type":"response.output_text.delta","delta":"The cause is "}),
        json!({"type":"response.output_text.delta","delta":"안녕 "}),
        json!({"type":"response.output_text.delta","delta":"the retry loop."}),
        json!({"type":"response.output_item.done","item":{"type":"message",
            "content":[{"type":"output_text","text":"The cause is 안녕 the retry loop."}]}}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call",
            "id":"fc_1","call_id":"call_1","name":"bash","arguments":""}}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"fc_1",
            "delta":"{\"command\":"}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"fc_1",
            "delta":"\"pwd\"}"}),
        json!({"type":"response.function_call_arguments.done","output_index":1,"item_id":"fc_1",
            "arguments":"{\"command\":\"pwd\"}"}),
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call",
            "id":"fc_1","call_id":"call_1","name":"bash","arguments":"{\"command\":\"pwd\"}"}}),
        json!({"type":"response.completed","response":{"status":"completed",
            "usage":{"input_tokens":10,"output_tokens":5}}}),
    ]
    .into_iter()
    .map(|frame| frame.to_string())
    .collect()
}

fn scripted_sse() -> String {
    let mut sse: String = scripted_json_frames()
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect();
    sse.push_str("data: [DONE]\n\n");
    sse
}

/// Replay raw SSE text through the real stream in fixed-size chunks, splitting the bytes
/// exactly where a socket would, and collect every event the parser yields.
fn replay_in_chunks(sse: &str, chunk: usize) -> Vec<StreamEvent> {
    let (tx, mut stream) = stream();
    let mut events = Vec::new();
    for piece in sse.as_bytes().chunks(chunk) {
        tx.unbounded_send(Ok(Bytes::copy_from_slice(piece))).unwrap();
        while let Some(Some(Ok(event))) = stream.next().now_or_never() {
            events.push(event);
        }
    }
    drop(tx);
    while let Some(Some(Ok(event))) = stream.next().now_or_never() {
        events.push(event);
    }
    events
}

fn event_trace(events: &[StreamEvent]) -> Vec<String> {
    events.iter().map(|event| format!("{event:?}")).collect()
}

#[test]
fn normal_path_parses_each_payload_once() {
    let frames = scripted_json_frames();
    let before = sse_json_parse_count();
    let events = replay_in_chunks(&scripted_sse(), 7);
    let parses = sse_json_parse_count() - before;
    assert!(!events.is_empty(), "the scripted turn must produce events");
    // Before this change each payload was parsed as a `Value` for the structured/notice
    // decision and again as the typed event, so a normal payload cost two parses.
    assert_eq!(
        parses,
        frames.len() as u64,
        "{} JSON payloads must cost {} parses, not {} (the pre-change count)",
        frames.len(),
        frames.len(),
        frames.len() * 2
    );

    let before = sse_json_parse_count();
    let single = parse_openai_response_event(
        &json!({"type":"response.output_text.delta","delta":"once"}).to_string(),
        &mut false,
        &mut false,
        &mut HashMap::new(),
        &mut HashSet::new(),
        &mut VecDeque::new(),
    );
    assert!(matches!(single, Some(StreamEvent::TextDelta(text)) if text == "once"));
    assert_eq!(sse_json_parse_count() - before, 1, "one event, one parse");

    // A payload the typed parse rejects pays the fallback's second read. The normal path
    // above never reaches it, which is the whole point of the split.
    let before = sse_json_parse_count();
    let rejected = parse_openai_response_event(
        "{not-json}",
        &mut false,
        &mut false,
        &mut HashMap::new(),
        &mut HashSet::new(),
        &mut VecDeque::new(),
    );
    assert!(rejected.is_none());
    assert_eq!(
        sse_json_parse_count() - before,
        2,
        "the fallback path re-reads the payload"
    );
}

#[test]
fn chunked_replay_yields_the_same_events_at_every_chunk_size() {
    let sse = scripted_sse();
    let baseline = replay_in_chunks(&sse, 4096);
    assert!(!baseline.is_empty(), "the scripted turn must produce events");
    let baseline_trace = event_trace(&baseline);
    println!(
        "chunk=4096 events={} parses={} trace={baseline_trace:?}",
        baseline.len(),
        scripted_json_frames().len()
    );
    for chunk in [1, 7, 64] {
        let before = sse_json_parse_count();
        let events = replay_in_chunks(&sse, chunk);
        let parses = sse_json_parse_count() - before;
        println!(
            "chunk={chunk} events={} parses={parses} trace={:?}",
            events.len(),
            event_trace(&events)
        );
        assert_eq!(
            event_trace(&events),
            baseline_trace,
            "chunk size {chunk} changed the event sequence or its order"
        );
        assert_eq!(
            parses,
            scripted_json_frames().len() as u64,
            "chunk size {chunk} must still parse each payload once"
        );
    }
}

#[test]
fn malformed_payloads_are_dropped_and_the_next_event_still_parses() {
    let (tx, mut stream) = stream();
    for malformed in [
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"truncated\n\n",
        "data: not-json\n\n",
        "data: {\"type\":\"response.unknown.future_event\",\"delta\":\"ignored\"}\n\n",
    ] {
        let before = sse_json_parse_count();
        tx.unbounded_send(Ok(Bytes::from(malformed.to_string()))).unwrap();
        idle(&mut stream);
        println!(
            "payload={:?} parses={} pending_after={:?}",
            malformed.trim_end(),
            sse_json_parse_count() - before,
            stream.framer.pending()
        );
        assert!(
            stream.framer.pending().is_empty(),
            "a dropped payload must leave no bytes for the next event: {:?}",
            stream.framer.pending()
        );
    }
    send(&tx, json!({"type":"response.output_text.delta","delta":"still here"}));
    assert!(matches!(next(&mut stream), StreamEvent::TextDelta(text) if text == "still here"));
    idle(&mut stream);
    assert!(stream.framer.pending().is_empty());
    println!("after the valid event: pending={:?}", stream.framer.pending());
}

/// Replay `events` text deltas in fixed-size bursts and report the events produced, the
/// bytes fed and the bytes the framer moved while compacting.
fn burst_replay(events: usize, chunk: usize) -> (usize, usize, u64) {
    let mut sse = String::new();
    for index in 0..events {
        sse.push_str(&format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"token {index:04}\"}}\n\n"
        ));
    }
    let (tx, mut stream) = stream();
    let mut produced = 0usize;
    for piece in sse.as_bytes().chunks(chunk) {
        tx.unbounded_send(Ok(Bytes::copy_from_slice(piece))).unwrap();
        while let Some(Some(Ok(_))) = stream.next().now_or_never() {
            produced += 1;
        }
    }
    drop(tx);
    while let Some(Some(Ok(_))) = stream.next().now_or_never() {
        produced += 1;
    }
    assert!(stream.framer.pending().is_empty(), "the replay consumed every byte");
    (produced, sse.len(), stream.framer.compacted_bytes())
}

#[test]
fn compaction_copies_stay_bounded_per_event_as_the_stream_grows() {
    // 8 KiB bursts make the framer hold a buffer large enough to compact, which is the
    // worst case for the copy cost: the tail is repeatedly moved to the front.
    let mut measurements = Vec::new();
    for events in [500usize, 2000] {
        let (produced, fed, copied) = burst_replay(events, 8192);
        assert_eq!(produced, events, "every scripted event must reach the consumer");
        println!(
            "events={events} fed={fed} copied={copied} per_event={:.2}",
            copied as f64 / events as f64
        );
        measurements.push((events, fed, copied));
    }
    for (events, fed, copied) in &measurements {
        assert!(
            *copied <= *fed as u64,
            "{events} events: compacting copied {copied} bytes of {fed} fed; the amortized cost \
             must stay under one byte per consumed byte"
        );
    }
    let (small_events, _, small_copied) = measurements[0];
    let (large_events, _, large_copied) = measurements[1];
    let small_per_event = small_copied as f64 / small_events as f64;
    let large_per_event = large_copied as f64 / large_events as f64;
    assert!(
        large_per_event <= small_per_event * 1.5 + 1.0,
        "copied bytes per event must not grow with the stream: {small_per_event:.2} at \
         {small_events} events, {large_per_event:.2} at {large_events}"
    );
}

#[test]
fn transport_frames_keep_their_previous_meaning() {
    let parse = |data: &str| {
        parse_openai_response_event(
            data,
            &mut false,
            &mut false,
            &mut HashMap::new(),
            &mut HashSet::new(),
            &mut VecDeque::new(),
        )
    };

    let disconnected = "stream disconnected before completion: transport error";
    assert!(
        matches!(parse(disconnected), Some(StreamEvent::Error { message, retry_after_secs: None })
        if message == disconnected),
        "a plain-text disconnect notice must surface as an error carrying its text"
    );
    let both = format!("falling back from websockets to https transport. {disconnected}");
    assert!(
        parse(&both).is_none(),
        "the notice is checked first and dropped, even when the disconnect phrase follows"
    );
    assert!(parse("falling back from websockets to https transport").is_none());

    let structured = json!({
        "type": "response.output_text.delta",
        "delta": disconnected
    })
    .to_string();
    assert!(
        matches!(parse(&structured), Some(StreamEvent::TextDelta(text)) if text == disconnected),
        "a structured event that merely mentions the notice is still model output"
    );

    // The typed parse rejects this one, but its `type` still says it is a Responses event, so
    // it must be reported as a malformed event rather than a transport frame.
    let malformed_structured = json!({
        "type": "response.output_text.delta",
        "delta": 7,
        "message": disconnected
    })
    .to_string();
    assert!(
        parse(&malformed_structured).is_none(),
        "a malformed structured event is dropped, not turned into a disconnect error"
    );
}
