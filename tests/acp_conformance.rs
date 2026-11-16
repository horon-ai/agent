#![cfg(unix)]
//! ACP conformance tests against the pinned schemas and, in later sections,
//! the real binary.
//!
//! The first section is the frame-level `id` rule the v2 schema states: an
//! `Error` response may carry `null` (the request's id could not be read),
//! while a `Result` response and a request may not.
//!
//! The second section drives the shipped `horon-agent acp` binary through the
//! mock client in its v2 mode, the way `acp_stdio_v2.rs` does: every test boots
//! its own isolated daemon and validates every frame of every connection
//! against `schema-v2.0.0-alpha.16.json`, asserting zero violations. It pins
//! `replay_complete`: the update a `session/resume` that carries a `replayFrom`
//! cursor sends once, after the last replayed update and before the response,
//! and the update a resume without a usable cursor never sends.
#![allow(dead_code)]

#[path = "support/acp_client.rs"]
mod acp_client;
#[path = "support/acp_daemon.rs"]
mod acp_daemon;
#[path = "support/acp_schema.rs"]
mod acp_schema;
#[path = "support/lifecycle.rs"]
mod lifecycle;

use acp_client::{AcpClient, AcpClientError, AcpClientOptions};
use acp_daemon::AcpDaemon;
use acp_schema::{Direction, FrameValidator};
use lifecycle::Script;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The violations one agent-authored v2 frame draws, printed so `--nocapture`
/// output names the frame and its count.
fn v2_violations(frame: &Value) -> Vec<acp_schema::Violation> {
    let violations = acp_schema::validate_outgoing_version(2, frame, Direction::AgentToClient);
    println!(
        "v2 agent frame {frame} -> {} violation(s): {violations:?}",
        violations.len()
    );
    violations
}

#[test]
fn validator_accepts_null_id_on_v2_error_response() {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {"code": -32600, "message": "Invalid request"},
    });
    let violations = v2_violations(&frame);
    assert!(violations.is_empty(), "{frame}: {violations:?}");
}

#[test]
fn validator_rejects_null_id_on_v2_result_response() {
    let frame = json!({"jsonrpc": "2.0", "id": null, "result": {}});
    let violations = v2_violations(&frame);
    assert_eq!(violations.len(), 1, "{frame}: {violations:?}");
    assert_eq!(violations[0].path, "/id", "{frame}: {violations:?}");
}

#[test]
fn validator_rejects_null_id_on_v2_request() {
    let frame = json!({"jsonrpc": "2.0", "id": null, "method": "session/list", "params": {}});
    let violations = acp_schema::validate_outgoing_version(2, &frame, Direction::ClientToAgent);
    println!(
        "v2 client frame {frame} -> {} violation(s): {violations:?}",
        violations.len()
    );
    assert!(
        violations.iter().any(|violation| violation.path == "/id"),
        "{frame}: {violations:?}"
    );
}

/// Bound for one response or one awaited frame. Model turns run through the
/// scripted provider, so a turn is seconds; the bound only keeps a broken run
/// from hanging.
const WAIT: Duration = Duration::from_secs(120);

/// One v2 client connection to the shipped binary. It records the requests it
/// wrote, which the end-of-connection schema tally needs to tell a response
/// from a request.
struct Conn {
    client: AcpClient,
    label: String,
    sent: Vec<(Value, String)>,
    /// The protocol version this connection negotiated, which its validator
    /// checks every frame against.
    version: u64,
}

impl Conn {
    fn v2(daemon: &AcpDaemon, label: &str) -> Self {
        Self::for_version(daemon, label, 2)
    }

    fn v1(daemon: &AcpDaemon, label: &str) -> Self {
        Self::for_version(daemon, label, 1)
    }

    fn for_version(daemon: &AcpDaemon, label: &str, version: u64) -> Self {
        let options = AcpClientOptions::new(daemon.env_pairs())
            .protocol_version(version)
            .deadline(WAIT);
        Self {
            client: AcpClient::spawn(options),
            label: label.to_string(),
            sent: Vec::new(),
            version,
        }
    }

    /// The handshake this client's version writes; asserts the version answered.
    fn initialize(&mut self) -> Value {
        let params = self.client.initialize_params();
        let result = self.ok("initialize", params);
        assert_eq!(result["protocolVersion"], self.version, "{}: {result}", self.label);
        result
    }

    fn send(&mut self, method: &str, params: Value) -> Value {
        let id = json!(self.client.next_id());
        self.sent.push((id.clone(), method.to_string()));
        self.client
            .send_request(id.clone(), method, params)
            .unwrap_or_else(|error| panic!("{}: send {method}: {error}", self.label));
        id
    }

    fn wait(&mut self, method: &str, id: &Value) -> Result<Value, Value> {
        match self.client.await_response(method, id, WAIT) {
            Ok(result) => Ok(result),
            Err(AcpClientError::Rpc { error, .. }) => Err(error),
            Err(error) => panic!("{}: {method} (id {id}): {error}", self.label),
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.wait(method, &id)
            .unwrap_or_else(|error| panic!("{}: {method} failed: {error}", self.label))
    }

    fn err(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        match self.wait(method, &id) {
            Err(error) => error,
            Ok(result) => panic!("{}: {method} succeeded unexpectedly: {result}", self.label),
        }
    }

    fn new_session(&mut self, cwd: &Path) -> String {
        let created = self.ok("session/new", json!({ "cwd": cwd_str(cwd) }));
        created["sessionId"].as_str().expect("a session id").to_string()
    }

    /// One prompt turn that ends on its own: the `messageId` the prompt was
    /// answered with and the session updates the connection wrote up to its
    /// idle state.
    fn turn(&mut self, session_id: &str, text: &str) -> (String, Vec<Value>) {
        self.client.drain_notifications();
        let cursor = self.client.state().cursor(session_id);
        let result = self.ok(
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": text}]}),
        );
        let message_id = result["messageId"].as_str().expect("a messageId").to_string();
        let stop = self
            .client
            .await_idle_after(session_id, cursor, WAIT)
            .unwrap_or_else(|| panic!("{}: no idle for {session_id} within {WAIT:?}", self.label));
        assert_eq!(stop, "end_turn", "{}: the turn ended on its own: {stop}", self.label);
        let updates = updates_of(&self.client.drain_notifications(), session_id);
        (message_id, updates)
    }

    /// Every frame of this connection, in both directions, against the v2
    /// schema; prints the tally and returns the number of violations.
    fn schema_violations(&self) -> usize {
        let mut agent = FrameValidator::for_version(self.version, Direction::AgentToClient);
        for (id, method) in &self.sent {
            agent.note_request(id, method);
        }
        let stdout = self.client.stdout_lines();
        let parsed: Vec<Value> = stdout
            .iter()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("{}: stdout line is not JSON ({error}): {line}", self.label))
            })
            .collect();
        let mut client = FrameValidator::for_version(self.version, Direction::ClientToAgent);
        for value in &parsed {
            for entry in entries(value) {
                if let (Some(method), Some(id)) = (entry.get("method").and_then(Value::as_str), entry.get("id")) {
                    client.note_request(id, method);
                }
            }
        }
        let mut violations = 0;
        for value in &parsed {
            for entry in entries(value) {
                for violation in agent.validate(entry) {
                    violations += 1;
                    eprintln!("{}-VIOLATION agent->client {violation} in {value}", self.label);
                }
            }
        }
        let stdin = self.client.stdin_lines();
        let mut client_frames = 0;
        for line in &stdin {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{}: stdin line is not JSON ({error}): {line}", self.label));
            for entry in entries(&value) {
                client_frames += 1;
                for violation in client.validate(entry) {
                    violations += 1;
                    eprintln!("{}-VIOLATION client->agent {violation} in {line}", self.label);
                }
            }
        }
        eprintln!(
            "REPLAY-SCHEMA {} agent_frames={} client_frames={client_frames} violations={violations}",
            self.label,
            stdout.len()
        );
        violations
    }

    fn assert_schema_clean(&self) {
        assert_eq!(self.schema_violations(), 0, "{}: schema violations", self.label);
    }
}

/// The `session/update` updates in `notifications` for `session`, in order.
fn updates_of(notifications: &[Value], session: &str) -> Vec<Value> {
    notifications
        .iter()
        .filter(|note| note["method"] == "session/update" && note["params"]["sessionId"] == session)
        .map(|note| note["params"]["update"].clone())
        .collect()
}

/// The JSON-RPC entries of one stdout line: a line is one frame, or a batch.
fn entries(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(entries) => entries.iter().collect(),
        other => vec![other],
    }
}

fn cwd_str(path: &Path) -> String {
    path.to_str().expect("a UTF-8 path").to_string()
}

fn work_dir(daemon: &AcpDaemon) -> PathBuf {
    let work = daemon.root.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    work
}

/// Every decoded frame of a connection's stdout, in the order the agent wrote
/// them.
fn frames(client: &AcpClient, label: &str) -> Vec<Value> {
    client
        .stdout_lines()
        .iter()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{label}: stdout line is not JSON ({error}): {line}"))
        })
        .collect()
}

/// The `sessionUpdate` of a `session/update` notification for `session`.
fn update_kind<'a>(frame: &'a Value, session: &str) -> Option<&'a str> {
    if frame["method"] != "session/update" || frame["params"]["sessionId"] != session {
        return None;
    }
    frame["params"]["update"]["sessionUpdate"].as_str()
}

fn is_response_to(frame: &Value, id: &Value) -> bool {
    frame.get("method").is_none() && frame.get("id") == Some(id)
}

/// What one `session/resume` wrote on its connection, up to and including its
/// response: every update for the session, and where `replay_complete` sits.
struct ReplayStream {
    kinds: Vec<String>,
    updates: Vec<Value>,
    /// Frame position of each update, parallel to `kinds`.
    positions: Vec<usize>,
    /// Frame position of the `session/resume` response.
    response: usize,
    /// Frame positions of the `replay_complete` updates.
    completes: Vec<usize>,
}

impl ReplayStream {
    fn of(frames: &[Value], session: &str, id: &Value) -> Self {
        let response = frames
            .iter()
            .position(|frame| is_response_to(frame, id))
            .unwrap_or_else(|| panic!("no response to {id} in the stream: {frames:#?}"));
        let mut kinds = Vec::new();
        let mut updates = Vec::new();
        let mut positions = Vec::new();
        let mut completes = Vec::new();
        for (index, frame) in frames.iter().enumerate().take(response) {
            let Some(kind) = update_kind(frame, session) else {
                continue;
            };
            if kind == "replay_complete" {
                completes.push(index);
            }
            kinds.push(kind.to_string());
            updates.push(frame["params"]["update"].clone());
            positions.push(index);
        }
        Self {
            kinds,
            updates,
            positions,
            response,
            completes,
        }
    }

    /// Index of the `replay_complete` update among `kinds`.
    fn complete_index(&self) -> usize {
        self.kinds
            .iter()
            .position(|kind| kind == "replay_complete")
            .expect("a replay_complete update")
    }

    fn summary(&self, label: &str, session: &str) -> String {
        format!(
            "REPLAY-COMPLETE {label} session={session} replay_complete_at={:?} response_at={} updates={} kinds={:?}",
            self.completes,
            self.response,
            self.kinds.len(),
            self.kinds
        )
    }
}

/// `replay_complete` arrives once, after the last replayed update and before
/// the response, and only the session info and the state snapshot follow it.
fn assert_one_complete_before_response(stream: &ReplayStream) {
    assert_eq!(
        stream.completes.len(),
        1,
        "exactly one replay_complete: {:?}",
        stream.kinds
    );
    let at = stream.complete_index();
    assert!(
        stream.positions[at] < stream.response,
        "replay_complete {} comes before the response at {}: {:?}",
        stream.positions[at],
        stream.response,
        stream.kinds
    );
    for (index, kind) in stream.kinds.iter().enumerate() {
        match index.cmp(&at) {
            std::cmp::Ordering::Less => assert!(
                !matches!(
                    kind.as_str(),
                    "session_info_update" | "state_update" | "replay_complete"
                ),
                "frame before replay_complete is replayed history, not {kind}: {:?}",
                stream.kinds
            ),
            std::cmp::Ordering::Equal => {}
            std::cmp::Ordering::Greater => assert!(
                matches!(kind.as_str(), "session_info_update" | "state_update"),
                "only the session info and the state snapshot follow replay_complete, not {kind}: {:?}",
                stream.kinds
            ),
        }
    }
}

/// A finished turn on a session of its own: one connection creates the
/// session, runs one scripted turn, validates the connection and closes. The
/// session id it created and the `messageId` the prompt was answered with are
/// returned.
fn finished_turn(daemon: &AcpDaemon, label: &str, prompt: &str, reply: &str, work: &Path) -> (String, String) {
    daemon.provider.script(prompt, Script::Text(reply.to_string()));
    let mut conn = Conn::v2(daemon, label);
    conn.initialize();
    let session = conn.new_session(work);
    let (message_id, updates) = conn.turn(&session, prompt);
    let kinds: Vec<&str> = updates
        .iter()
        .filter_map(|update| update["sessionUpdate"].as_str())
        .collect();
    println!("REPLAY-SETUP {label} session={session} message_id={message_id} live_kinds={kinds:?}");
    assert!(
        kinds.contains(&"agent_message_chunk"),
        "{label}: the turn streamed its answer: {kinds:?}"
    );
    conn.assert_schema_clean();
    conn.client.close_stdin();
    conn.client.wait_exit(WAIT).expect("the adapter exits on stdin EOF");
    (session, message_id)
}

/// `replay_complete_` happy path: a session whose one turn is on disk is
/// resumed over a new connection with `replayFrom: {"type": "start"}`; the
/// replayed user and agent messages come first, then `replay_complete`, then
/// the session info and the idle snapshot, and then the response.
#[test]
fn replay_complete_after_a_finished_turn() {
    let daemon = AcpDaemon::start("acp");
    let work = work_dir(&daemon);
    let (session, message_id) = finished_turn(&daemon, "rc-turn-a", "RC_TURN hello", "rc reply one", &work);

    let mut second = Conn::v2(&daemon, "rc-turn-b");
    second.initialize();
    let resume = second.send(
        "session/resume",
        json!({"sessionId": session, "cwd": cwd_str(&work), "replayFrom": {"type": "start"}}),
    );
    let resumed = second
        .wait("session/resume", &resume)
        .unwrap_or_else(|error| panic!("the resume answers: {error}"));
    assert!(resumed.get("modes").is_none(), "{resumed}");
    let stream = ReplayStream::of(&frames(&second.client, "rc-turn-b"), &session, &resume);
    println!("{}", stream.summary("rc-turn-b", &session));
    assert_one_complete_before_response(&stream);
    assert_eq!(
        stream.kinds,
        [
            "user_message",
            "agent_message",
            "replay_complete",
            "session_info_update",
            "state_update"
        ],
        "the replayed turn, then replay_complete, then the post-replay frames: {:?}",
        stream.kinds
    );
    assert_eq!(
        stream.updates[0]["messageId"],
        message_id.as_str(),
        "{:?}",
        stream.updates
    );
    assert_eq!(
        stream.updates[1]["messageId"],
        format!("{message_id}:a1").as_str(),
        "{:?}",
        stream.updates
    );
    assert!(
        stream.updates[1].to_string().contains("rc reply one"),
        "the replayed answer carries the reply: {:?}",
        stream.updates[1]
    );
    second.assert_schema_clean();
}

/// `replay_complete_` happy path for an empty history: the session exists but
/// no turn ran, and the resume over a new connection still reports the end of
/// the (empty) replay as its first update.
#[test]
fn replay_complete_for_an_empty_history() {
    let daemon = AcpDaemon::start("acp");
    let work = work_dir(&daemon);
    let mut first = Conn::v2(&daemon, "rc-empty-a");
    first.initialize();
    let session = first.new_session(&work);
    first.client.close_stdin();
    first
        .client
        .wait_exit(WAIT)
        .expect("the first adapter exits on stdin EOF");

    let mut second = Conn::v2(&daemon, "rc-empty-b");
    second.initialize();
    let resume = second.send(
        "session/resume",
        json!({"sessionId": session, "cwd": cwd_str(&work), "replayFrom": {"type": "start"}}),
    );
    let resumed = second
        .wait("session/resume", &resume)
        .unwrap_or_else(|error| panic!("the resume answers: {error}"));
    assert!(resumed.get("modes").is_none(), "{resumed}");
    let stream = ReplayStream::of(&frames(&second.client, "rc-empty-b"), &session, &resume);
    println!("{}", stream.summary("rc-empty-b", &session));
    assert_one_complete_before_response(&stream);
    assert_eq!(
        stream.kinds.first().map(String::as_str),
        Some("replay_complete"),
        "an empty history has no replayed update before replay_complete: {:?}",
        stream.kinds
    );
    assert!(
        stream.updates.iter().all(|update| !matches!(
            update["sessionUpdate"].as_str(),
            Some("user_message" | "agent_message" | "tool_call_update")
        )),
        "an empty history replays no message: {:?}",
        stream.updates
    );
    second.assert_schema_clean();
}

/// `replay_absent_` failure path: a resume that omits `replayFrom` replays
/// nothing, so the connection never carries `replay_complete` and the response
/// still answers.
#[test]
fn replay_absent_without_a_cursor() {
    let daemon = AcpDaemon::start("acp");
    let work = work_dir(&daemon);
    let (session, _) = finished_turn(&daemon, "rc-none-a", "RC_NONE hello", "rc reply none", &work);

    let mut second = Conn::v2(&daemon, "rc-none-b");
    second.initialize();
    let resume = second.send("session/resume", json!({"sessionId": session, "cwd": cwd_str(&work)}));
    let resumed = second
        .wait("session/resume", &resume)
        .unwrap_or_else(|error| panic!("the resume answers: {error}"));
    assert!(resumed.get("modes").is_none(), "{resumed}");
    let stream = ReplayStream::of(&frames(&second.client, "rc-none-b"), &session, &resume);
    println!("{}", stream.summary("rc-none-b", &session));
    assert_eq!(
        stream.completes.len(),
        0,
        "no replayFrom, no replay_complete: {:?}",
        stream.kinds
    );
    assert_eq!(
        stream.kinds,
        ["session_info_update", "state_update"],
        "no replayFrom replays nothing: {:?}",
        stream.kinds
    );
    second.assert_schema_clean();
}

/// `replay_absent_` failure path: a resume with a cursor this agent does not
/// support is refused with `-32602` before anything is attached, so no
/// `replay_complete` (and no other update) is ever written.
#[test]
fn replay_absent_with_an_unsupported_cursor() {
    let daemon = AcpDaemon::start("acp");
    let work = work_dir(&daemon);
    let (session, _) = finished_turn(&daemon, "rc-bogus-a", "RC_BOGUS hello", "rc reply bogus", &work);

    let mut second = Conn::v2(&daemon, "rc-bogus-b");
    second.initialize();
    let resume = second.send(
        "session/resume",
        json!({"sessionId": session, "cwd": cwd_str(&work), "replayFrom": {"type": "bogus"}}),
    );
    let error = match second.wait("session/resume", &resume) {
        Err(error) => error,
        Ok(result) => panic!("an unsupported cursor is refused, not resumed: {result}"),
    };
    println!("REPLAY-ABSENT rc-bogus-b error={error}");
    assert_eq!(
        error["code"], -32602,
        "an unsupported cursor is invalid params: {error}"
    );
    let stream = ReplayStream::of(&frames(&second.client, "rc-bogus-b"), &session, &resume);
    println!(
        "REPLAY-ABSENT rc-bogus-b replay_completes={} updates={} kinds={:?}",
        stream.completes.len(),
        stream.kinds.len(),
        stream.kinds
    );
    assert_eq!(
        stream.completes.len(),
        0,
        "a refused cursor replays nothing: {:?}",
        stream.kinds
    );
    assert!(
        stream.kinds.is_empty(),
        "a refused cursor writes no updates: {:?}",
        stream.kinds
    );
    second.assert_schema_clean();
}

/// One connection at `version`, from the same mock client the v2 tests use.
fn system_prompt_conn(daemon: &AcpDaemon, label: &str, version: u64) -> Conn {
    match version {
        1 => Conn::v1(daemon, label),
        _ => Conn::v2(daemon, label),
    }
}

/// How many sessions `session/list` reports on this connection.
fn session_count(conn: &mut Conn) -> usize {
    let listed = conn.ok("session/list", json!({}));
    listed["sessions"].as_array().map_or(0, Vec::len)
}

/// `system_prompt_null_` happy path on one protocol version: an omitted
/// `systemPrompt` and an explicit `null` are the same request, so `session/new`
/// answers with a session id for both, `initialize` advertises no capability
/// for it, and every frame of the connection passes its schema.
fn system_prompt_null_case(version: u64, label: &str) {
    let daemon = AcpDaemon::start("acp");
    let work = work_dir(&daemon);
    let mut conn = system_prompt_conn(&daemon, label, version);
    let initialize = conn.initialize();
    let capabilities = match version {
        1 => &initialize["agentCapabilities"]["sessionCapabilities"],
        _ => &initialize["capabilities"]["session"],
    };
    eprintln!("SYSTEM-PROMPT-NULL {label} v={version} initialize_capabilities={capabilities}");
    assert!(
        capabilities.get("systemPrompt").is_none(),
        "{label}: no systemPrompt capability is advertised: {capabilities}"
    );
    for (case, params) in [
        ("omitted", json!({"cwd": cwd_str(&work), "mcpServers": []})),
        (
            "null",
            json!({"cwd": cwd_str(&work), "mcpServers": [], "systemPrompt": Value::Null}),
        ),
    ] {
        let id = conn.send("session/new", params);
        let result = conn
            .wait("session/new", &id)
            .unwrap_or_else(|error| panic!("{label}: {case} session/new failed: {error}"));
        let session = result["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("{label}: {case} session/new has no sessionId: {result}"))
            .to_string();
        eprintln!("SYSTEM-PROMPT-NULL {label} v={version} method=session/new systemPrompt={case} sessionId={session}");
    }
    conn.assert_schema_clean();
    conn.client.close_stdin();
    conn.client.wait_exit(WAIT).expect("the adapter exits on stdin EOF");
}

/// `system_prompt_rejected_` failure path on one protocol version: every
/// lifecycle method that carries a `systemPrompt` this agent cannot apply is
/// refused with `-32602`, and the session count `session/list` reports does not
/// grow. Load and resume name a session that really exists, so the refusal
/// comes from the `systemPrompt` and not from an unknown id.
fn system_prompt_rejected_case(version: u64, label: &str) {
    let daemon = AcpDaemon::start("acp");
    let work = work_dir(&daemon);
    let (session, _) = finished_turn(&daemon, &format!("{label}-setup"), "SP_REJECT hello", "sp reply", &work);
    let mut conn = system_prompt_conn(&daemon, label, version);
    conn.initialize();
    let methods: &[&str] = if version == 1 {
        &["session/new", "session/load", "session/resume"]
    } else {
        &["session/new", "session/resume"]
    };
    for method in methods {
        let before = session_count(&mut conn);
        let mut params = json!({
            "cwd": cwd_str(&work),
            "mcpServers": [],
            "systemPrompt": {"mode": "append", "content": [{"type": "text", "text": "x"}]},
        });
        if *method != "session/new" {
            params["sessionId"] = json!(session.as_str());
        }
        let error = conn.err(method, params);
        let after = session_count(&mut conn);
        eprintln!(
            "SYSTEM-PROMPT-REJECTED {label} v={version} method={method} code={} sessions_before={before} sessions_after={after} message={}",
            error["code"], error["message"]
        );
        assert_eq!(error["code"], -32602, "{label}: {method}: {error}");
        assert_eq!(after, before, "{label}: a rejected {method} created no session");
    }
    conn.assert_schema_clean();
    conn.client.close_stdin();
    conn.client.wait_exit(WAIT).expect("the adapter exits on stdin EOF");
}

#[test]
fn system_prompt_null_creates_the_session_v1() {
    system_prompt_null_case(1, "sp-null-v1");
}

#[test]
fn system_prompt_null_creates_the_session_v2() {
    system_prompt_null_case(2, "sp-null-v2");
}

#[test]
fn system_prompt_rejected_on_every_lifecycle_method_v1() {
    system_prompt_rejected_case(1, "sp-reject-v1");
}

#[test]
fn system_prompt_rejected_on_every_lifecycle_method_v2() {
    system_prompt_rejected_case(2, "sp-reject-v2");
}
