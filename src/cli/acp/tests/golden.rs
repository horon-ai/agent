//! The v1 golden frame set (IS-16): a stored request script, replayed against
//! this build, must produce exactly the frames the merged v1 adapter wrote.
//!
//! The frame set in `tests/fixtures/acp/golden-v1-frames.jsonl` was captured by
//! replaying the same script on the merged v1 commit (see the evidence file
//! `task-6-negotiation.txt`), with the daemon end scripted exactly as it is
//! here, so every value except the build version is fixed by the script.

use super::connection_core::*;
use super::*;

const FIXTURE: &str = "tests/fixtures/acp/golden-v1-frames.jsonl";

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

/// Volatile values the stored set keeps as placeholders: the build version and
/// the synthetic message ids the v1 surface mints per turn.
fn normalize(frame: &Value) -> Value {
    fn walk(value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, item)| {
                        let item = if key == "messageId" {
                            json!("<messageId>")
                        } else {
                            walk(item)
                        };
                        (key.clone(), item)
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(walk).collect()),
            other => other.clone(),
        }
    }
    let mut frame = walk(frame);
    if let Some(version) = frame.pointer_mut("/result/agentInfo/version") {
        *version = json!("<version>");
    }
    frame
}

/// `$last` and `$message` inside an emitted event: the id of the daemon request
/// the script last expected, and of the last `message` request, so one script
/// can answer requests whose ids the adapter allocates.
fn substitute(value: &Value, last: &Value, message: &Value) -> Value {
    match value {
        Value::String(text) if text == "$last" => last.clone(),
        Value::String(text) if text == "$message" => message.clone(),
        Value::Array(items) => Value::Array(items.iter().map(|item| substitute(item, last, message)).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| (key.clone(), substitute(item, last, message)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Replay one stored script and return every frame the adapter wrote, in order.
///
/// Step kinds, one per script entry:
/// - `{"daemon": true}`: take the next scripted daemon connection.
/// - `{"fake": id}` / `{"bridged": id}`: attach a fake session of that name.
/// - `{"cwd": path}`: the attached session's working directory.
/// - `{"send": frame}`: write one client frame.
/// - `{"raw": text}`: write one line that is not JSON-RPC at all.
/// - `{"expect": type}`: read daemon requests until one of that type arrives.
/// - `{"emit": [events]}`: write those events to the scripted daemon.
/// - `{"answer": {"method": m, "result": r}}`: answer the next client call of that method.
/// - `{"await": id}`: collect frames up to and including that response.
/// - `{"barrier": true}`: collect whatever arrives before an inline answer.
async fn replay(steps: &[Value], protocol_v2: bool) -> Vec<Value> {
    let (connector, mut daemons) = scripted_daemon();
    let mut client = start_client_with(Some(connector));
    if !protocol_v2 {
        client.disable_v2();
    }
    let mut daemon: Option<FakeDaemon> = None;
    let mut session: Option<Arc<DaemonSession>> = None;
    let mut written: Vec<Value> = Vec::new();
    let mut last_id = Value::Null;
    let mut message_id = Value::Null;

    for step in steps {
        if step.get("daemon").is_some() {
            daemon = Some(
                within("the scripted daemon connection", daemons.recv())
                    .await
                    .expect("a connector per session"),
            );
        } else if let Some(session_id) = step.get("fake").and_then(Value::as_str) {
            let (attached, fake) = attach_fake_session(&client, session_id).await;
            offer_models(&attached, 1).await;
            session = Some(attached);
            daemon = Some(fake);
        } else if let Some(session_id) = step.get("bridged").and_then(Value::as_str) {
            let (attached, fake) = attach_bridged_session(&client, session_id).await;
            offer_models(&attached, 1).await;
            session = Some(attached);
            daemon = Some(fake);
        } else if let Some(cwd) = step.get("cwd").and_then(Value::as_str) {
            let session = session.as_ref().expect("a session to place");
            *session.cwd.lock().await = Some(PathBuf::from(cwd));
        } else if let Some(frame) = step.get("send") {
            client.send(frame.clone()).await;
        } else if let Some(text) = step.get("raw").and_then(Value::as_str) {
            client.send_raw(&format!("{text}\n")).await;
        } else if let Some(kind) = step.get("expect").and_then(Value::as_str) {
            if daemon.is_none() {
                daemon = Some(
                    within("the scripted daemon connection", daemons.recv())
                        .await
                        .expect("a connector per session"),
                );
            }
            let fake = daemon.as_mut().expect("a scripted daemon");
            let request = fake.recv_type(kind).await;
            last_id = request["id"].clone();
            if kind == "message" {
                message_id = request["id"].clone();
            }
        } else if let Some(events) = step.get("emit").and_then(Value::as_array) {
            let fake = daemon.as_mut().expect("a scripted daemon");
            for event in events {
                fake.emit(substitute(event, &last_id, &message_id)).await;
            }
        } else if let Some(answer) = step.get("answer") {
            let call = loop {
                let frame = client.recv().await;
                written.push(frame.clone());
                if frame["method"] == answer["method"] {
                    break frame;
                }
            };
            client
                .send(json!({"jsonrpc":"2.0","id":call["id"],"result":answer["result"]}))
                .await;
        } else if let Some(id) = step.get("await") {
            let (before, response) = client.recv_response(id.clone()).await;
            written.extend(before);
            written.push(response);
        } else if step.get("barrier").is_some() {
            written.extend(client.barrier("golden").await);
        }
    }
    client.close_stdin().await;
    written
}

/// IS-16: the v1 surface writes exactly the stored frames, once with v2 on and a
/// client asking for protocol version 1, and once with the v2 switch off for the
/// version-65535 and version-0 initializes.
#[tokio::test]
async fn v1_golden_frames_unchanged() {
    let text = std::fs::read_to_string(fixture_path()).expect("the stored v1 frame set");
    let mut sections: Vec<(Value, Vec<String>)> = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record: Value = serde_json::from_str(line).expect("a fixture line");
        if record["kind"] == "script" {
            sections.push((record, Vec::new()));
        } else {
            sections
                .last_mut()
                .expect("a script record before any frame")
                .1
                .push(line.to_string());
        }
    }
    assert_eq!(sections.len(), 2, "one v2-on section and one v2-off section");

    for (script, stored) in sections {
        let steps = script["steps"].as_array().expect("the script steps").clone();
        let protocol_v2 = script["v2"].as_bool().unwrap_or(true);
        let frames = replay(&steps, protocol_v2).await;
        assert_eq!(
            frames.len(),
            stored.len(),
            "{}: the replay wrote {} frames, the stored set has {}: {frames:#?}",
            script["name"],
            frames.len(),
            stored.len()
        );
        for (index, (frame, golden)) in frames.iter().zip(stored.iter()).enumerate() {
            let replayed = serde_json::to_string(&normalize(frame)).expect("a frame serializes");
            assert_eq!(replayed, golden.trim(), "{} frame {index} changed", script["name"]);
        }
    }
}
