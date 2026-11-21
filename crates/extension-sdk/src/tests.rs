//! Contracts of the author SDK: the declaration, the handshake, dispatch,
//! cancellation and the stdout stream.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use horon_agent_extension_types::wire::{
    Capability, CapabilitySet, FrameError, InitializeParams, MAX_FRAME_BYTES, ObserverParams, ProtocolRange,
    ProtocolVersion, RpcError, WireFrame, WireMethod, WireNotification, WireRequest, WireResponse, encode_frame,
    parse_frame,
};
use horon_agent_extension_types::{ObserverEvent, PackageKind};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::{
    CANCELLED, Context, Declaration, Extension, LoadError, NOT_INITIALIZED, PROTOCOL_RANGE, PROTOCOL_UNAVAILABLE,
    ServeError, SurfaceError, serve,
};

const MANIFEST: &str = r#"schema_version = 1
id = "hello"
version = "0.1.0"
name = "Hello"
protocol = "1.0"
events = ["turn.start", "turn.end"]

[targets]
aarch64-apple-darwin = "bin/child"

[[tools]]
name = "greet"
description = "Greet one name."
input_schema = { type = "object" }

[[tools]]
name = "wait"
description = "Wait until the host cancels."
input_schema = { type = "object" }

[[commands]]
name = "hello"
description = "Say hello."
"#;

const BUNDLED: &str = r#"schema_version = 1
id = "hello"
version = "0.1.0"
name = "Hello bundled"
protocol = "1.0"
events = ["turn.start", "turn.end"]

[targets]
aarch64-apple-darwin = "bin/child"

[[tools]]
name = "greet"
description = "Greet one name."
input_schema = { type = "object" }

[[tools]]
name = "wait"
description = "Wait until the host cancels."
input_schema = { type = "object" }

[[commands]]
name = "hello"
description = "Say hello."
"#;

const PLUGIN: &str = r#"schema_version = 1
id = "hello"
version = "0.1.0"
name = "Hello plugin"
extension = "extension/extension.toml"

[resources]
rules = ["rule.md"]
"#;

const TOOLS: [&str; 2] = ["greet", "wait"];
const COMMANDS: [&str; 1] = ["hello"];
const EVENTS: [ObserverEvent; 2] = [ObserverEvent::TurnStart, ObserverEvent::TurnEnd];

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn extension() -> Self {
        let temp = tempfile::tempdir().expect("a temporary directory");
        let root = temp.path().to_path_buf();
        std::fs::write(root.join("extension.toml"), MANIFEST).expect("the declaration");
        Self { _temp: temp, root }
    }

    fn plugin() -> Self {
        let temp = tempfile::tempdir().expect("a temporary directory");
        let root = temp.path().to_path_buf();
        std::fs::create_dir_all(root.join("extension")).expect("the bundled directory");
        std::fs::write(root.join("extension").join("extension.toml"), BUNDLED).expect("the bundled declaration");
        std::fs::write(root.join("plugin.toml"), PLUGIN).expect("the plugin declaration");
        Self { _temp: temp, root }
    }

    fn declaration(&self) -> Declaration {
        Declaration::load(&self.root).expect("the declaration loads")
    }
}

struct TestExtension {
    tools: &'static [&'static str],
    commands: &'static [&'static str],
    events: &'static [ObserverEvent],
    seen: Arc<Mutex<Vec<ObserverEvent>>>,
}

impl TestExtension {
    fn new() -> Self {
        Self {
            tools: &TOOLS,
            commands: &COMMANDS,
            events: &EVENTS,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Extension for TestExtension {
    fn tools(&self) -> &'static [&'static str] {
        self.tools
    }

    fn commands(&self) -> &'static [&'static str] {
        self.commands
    }

    fn events(&self) -> &'static [ObserverEvent] {
        self.events
    }

    fn call_tool(&self, name: &str, arguments: &Value, context: &mut Context<'_>) -> Result<Value, RpcError> {
        match name {
            "greet" => {
                let who = arguments.get("name").and_then(Value::as_str).unwrap_or("world");
                Ok(json!({ "greeting": format!("hello, {who}") }))
            }
            "wait" => {
                // Stands in for a long call: it stops as soon as the host cancels.
                let deadline = Instant::now() + Duration::from_secs(3);
                while !context.is_cancelled() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(RpcError {
                    code: CANCELLED,
                    message: "the host cancelled this call".to_owned(),
                    data: None,
                })
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }

    fn invoke_command(&self, name: &str, arguments: &Value, _context: &mut Context<'_>) -> Result<Value, RpcError> {
        if name != "hello" {
            return Err(RpcError::method_not_found(name));
        }
        Ok(json!({ "command": "hello", "arguments": arguments }))
    }

    fn observe(&self, event: ObserverEvent, metadata: &Value, context: &mut Context<'_>) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(event);
        }
        context.notify(event, metadata.clone());
    }
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut sink = self.0.lock().expect("the sink");
        sink.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A live host's write half: frames arrive from the test and the stream only
/// ends after the test drops the sender, so an end of input cannot race the
/// answers the server is still writing.
struct Feed {
    receiver: Receiver<Vec<u8>>,
    current: Vec<u8>,
    offset: usize,
}

impl Feed {
    fn new(receiver: Receiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            current: Vec::new(),
            offset: 0,
        }
    }
}

impl Read for Feed {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        while self.offset >= self.current.len() {
            match self.receiver.recv() {
                Ok(chunk) => {
                    self.current = chunk;
                    self.offset = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let available = &self.current[self.offset..];
        let take = available.len().min(buffer.len());
        buffer[..take].copy_from_slice(&available[..take]);
        self.offset += take;
        Ok(take)
    }
}

fn run(extension: TestExtension, declaration: Declaration, input: Vec<u8>) -> (Result<(), ServeError>, Vec<u8>) {
    let (sender, receiver) = mpsc::channel();
    let sink = Arc::new(Mutex::new(Vec::new()));
    let output = Sink(Arc::clone(&sink));
    let worker = thread::spawn(move || serve(extension, declaration, Feed::new(receiver), output));
    sender.send(input).expect("the frames reach the server");
    let result = worker.join().expect("the server thread");
    // Dropping the sender is the host closing its write half, which happens
    // after it read every answer.
    drop(sender);
    let bytes = sink.lock().expect("the sink").clone();
    (result, bytes)
}

fn frames(bytes: &[u8]) -> Vec<WireFrame> {
    let mut parsed = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        assert!(line.ends_with(b"\n"), "every stdout line ends with its newline");
        parsed.push(parse_frame(line).expect("every stdout line is a frame"));
    }
    parsed
}

fn responses(bytes: &[u8]) -> Vec<WireResponse> {
    frames(bytes)
        .into_iter()
        .filter_map(|frame| match frame {
            WireFrame::Response(response) => Some(response),
            WireFrame::Request(_) | WireFrame::Notification(_) => None,
        })
        .collect()
}

fn ids(responses: &[WireResponse]) -> Vec<u64> {
    responses.iter().map(|response| response.id).collect()
}

fn capabilities() -> Vec<Capability> {
    vec![
        Capability::ToolCall,
        Capability::CommandInvoke,
        Capability::ObserverNotify,
    ]
}

fn initialize_input(id: u64, protocol: ProtocolRange, kind: PackageKind) -> Vec<u8> {
    let params = InitializeParams {
        protocol,
        capabilities: CapabilitySet::new(capabilities()),
        package_id: "hello".to_owned(),
        kind,
    };
    encode_frame(&WireRequest::new(
        id,
        WireMethod::Initialize,
        Some(serde_json::to_value(params).expect("initialize params")),
    ))
    .expect("encode a frame")
}

fn request_input(id: u64, method: WireMethod, params: Value) -> Vec<u8> {
    encode_frame(&WireRequest::new(id, method, Some(params))).expect("encode a frame")
}

fn notification_input(method: WireMethod, params: Value) -> Vec<u8> {
    encode_frame(&WireNotification::new(method, Some(params))).expect("encode a frame")
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes.iter().copied() {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[test]
fn the_digest_is_the_sha256_of_the_declaration_bytes() {
    let fixture = Fixture::extension();
    let declaration = fixture.declaration();
    assert_eq!(declaration.kind(), PackageKind::Extension);
    assert_eq!(hex(&Sha256::digest(MANIFEST.as_bytes())), declaration.digest());
    assert_eq!(declaration.digest().len(), 64);
}

#[test]
fn a_plugin_hashes_the_extension_it_bundles() {
    let fixture = Fixture::plugin();
    let declaration = fixture.declaration();
    assert_eq!(declaration.kind(), PackageKind::Plugin);
    assert_eq!(hex(&Sha256::digest(BUNDLED.as_bytes())), declaration.digest());
    assert_ne!(hex(&Sha256::digest(MANIFEST.as_bytes())), declaration.digest());
}

#[test]
fn a_declaration_with_another_protocol_is_refused() {
    let temp = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(
        temp.path().join("extension.toml"),
        MANIFEST.replace("protocol = \"1.0\"", "protocol = \"2.0\""),
    )
    .expect("the declaration");
    match Declaration::load(temp.path()) {
        Err(LoadError::Protocol { declared }) => assert_eq!(declared, "2.0"),
        other => panic!("expected a protocol refusal, got {other:?}"),
    }
}

#[test]
fn a_layout_with_both_manifests_is_refused() {
    let fixture = Fixture::extension();
    std::fs::write(fixture.root.join("plugin.toml"), PLUGIN).expect("the plugin declaration");
    match Declaration::load(&fixture.root) {
        Err(LoadError::Layout { .. }) => {}
        other => panic!("expected a layout refusal, got {other:?}"),
    }
}

#[test]
fn an_unsupported_host_version_is_refused() {
    let fixture = Fixture::extension();
    let declaration = fixture.declaration();
    let incompatible = ProtocolRange::new(ProtocolVersion::new(2, 0), ProtocolVersion::new(3, 0)).expect("a range");
    let mut input = initialize_input(1, incompatible, PackageKind::Extension);
    input.extend(request_input(2, WireMethod::Shutdown, Value::Null));
    let (result, bytes) = run(TestExtension::new(), declaration, input);
    assert!(
        matches!(&result, Err(ServeError::Handshake(_))),
        "the handshake must fail: {result:?}"
    );
    let responses = responses(&bytes);
    assert_eq!(ids(&responses), vec![1]);
    let error = responses[0]
        .error
        .as_ref()
        .expect("a refused initialize answers an error");
    assert_eq!(error.code, PROTOCOL_UNAVAILABLE);
}

#[test]
fn a_request_before_initialize_is_refused() {
    let fixture = Fixture::extension();
    let declaration = fixture.declaration();
    let mut input = request_input(1, WireMethod::ToolCall, json!({ "tool": "greet", "arguments": {} }));
    input.extend(initialize_input(2, PROTOCOL_RANGE, PackageKind::Extension));
    input.extend(request_input(3, WireMethod::Shutdown, Value::Null));
    let (result, bytes) = run(TestExtension::new(), declaration, input);
    assert!(result.is_ok(), "serving continues after the refusal: {result:?}");
    let responses = responses(&bytes);
    assert_eq!(ids(&responses), vec![1, 2, 3]);
    let error = responses[0].error.as_ref().expect("the first request is refused");
    assert_eq!(error.code, NOT_INITIALIZED);
    assert!(responses[1].error.is_none(), "initialize settles after the refusal");
}

#[test]
fn a_cancelled_call_answers_nothing() {
    let fixture = Fixture::extension();
    let declaration = fixture.declaration();
    let mut input = initialize_input(1, PROTOCOL_RANGE, PackageKind::Extension);
    input.extend(request_input(
        2,
        WireMethod::ToolCall,
        json!({ "tool": "wait", "arguments": {} }),
    ));
    input.extend(notification_input(WireMethod::Cancel, json!({ "request_id": 2 })));
    input.extend(request_input(
        3,
        WireMethod::ToolCall,
        json!({ "tool": "greet", "arguments": { "name": "Ada" } }),
    ));
    input.extend(request_input(4, WireMethod::Shutdown, Value::Null));
    let (result, bytes) = run(TestExtension::new(), declaration, input);
    assert!(result.is_ok(), "{result:?}");
    let responses = responses(&bytes);
    assert_eq!(ids(&responses), vec![1, 3, 4]);
    assert_eq!(responses[1].result, Some(json!({ "greeting": "hello, Ada" })));
}

#[test]
fn an_observed_event_reaches_the_handler() {
    let fixture = Fixture::extension();
    let declaration = fixture.declaration();
    let extension = TestExtension::new();
    let seen = Arc::clone(&extension.seen);
    let event = ObserverParams {
        event: ObserverEvent::TurnStart,
        metadata: json!({ "turn": 1 }),
    };
    let mut input = initialize_input(1, PROTOCOL_RANGE, PackageKind::Extension);
    input.extend(notification_input(
        WireMethod::ObserverNotify,
        serde_json::to_value(&event).expect("the event"),
    ));
    input.extend(request_input(2, WireMethod::Shutdown, Value::Null));
    let (result, bytes) = run(extension, declaration, input);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(*seen.lock().expect("the observations"), vec![ObserverEvent::TurnStart]);
    let frames = frames(&bytes);
    assert!(
        matches!(frames.last(), Some(WireFrame::Response(_))),
        "shutdown answers last"
    );
    assert!(
        frames.iter().any(|frame| matches!(frame, WireFrame::Notification(_))),
        "the handler's own report reaches the host"
    );
}

#[test]
fn a_handler_that_disagrees_with_the_declaration_is_refused() {
    const NARROW: [&str; 1] = ["greet"];
    let fixture = Fixture::extension();
    let declaration = fixture.declaration();
    let extension = TestExtension {
        tools: &NARROW,
        commands: &COMMANDS,
        events: &EVENTS,
        seen: Arc::new(Mutex::new(Vec::new())),
    };
    let (result, bytes) = run(extension, declaration, Vec::new());
    assert!(
        matches!(&result, Err(ServeError::Surface(SurfaceError::MissingTool(_)))),
        "{result:?}"
    );
    assert!(bytes.is_empty());
}

#[test]
fn a_frame_over_the_cap_ends_the_connection() {
    let fixture = Fixture::extension();
    let declaration = fixture.declaration();
    let input = vec![b'a'; MAX_FRAME_BYTES + 2];
    let (result, bytes) = run(TestExtension::new(), declaration, input);
    assert!(
        matches!(&result, Err(ServeError::Frame(FrameError::TooLarge { .. }))),
        "{result:?}"
    );
    assert!(bytes.is_empty());
}

#[test]
fn the_stdout_stream_carries_only_frames() {
    let fixture = Fixture::extension();
    let declaration = fixture.declaration();
    let event = ObserverParams {
        event: ObserverEvent::TurnEnd,
        metadata: json!({}),
    };
    let mut input = initialize_input(1, PROTOCOL_RANGE, PackageKind::Extension);
    input.extend(request_input(
        2,
        WireMethod::ToolCall,
        json!({ "tool": "greet", "arguments": { "name": "Ada" } }),
    ));
    input.extend(request_input(
        3,
        WireMethod::CommandInvoke,
        json!({ "command": "hello", "arguments": { "who": "Ada" } }),
    ));
    input.extend(notification_input(
        WireMethod::ObserverNotify,
        serde_json::to_value(&event).expect("the event"),
    ));
    input.extend(request_input(4, WireMethod::Shutdown, Value::Null));
    let (result, bytes) = run(TestExtension::new(), declaration, input);
    assert!(result.is_ok(), "{result:?}");
    // `frames` refuses any line that is not a frame, which is the contract.
    let frames = frames(&bytes);
    assert!(
        frames
            .iter()
            .all(|frame| matches!(frame, WireFrame::Response(_) | WireFrame::Notification(_))),
        "stdout carries only answers and the handler's own reports"
    );
}
