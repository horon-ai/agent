//! Both package layouts, run the way the host runs an installed copy.
//!
//! Each test starts the built example as a real child process, points it at a
//! staged copy of one layout under an isolated `HORON_HOME`, and speaks the
//! declared wire: initialize, a tool call, an executable command, an observed
//! event, a cancelled call and shutdown. The same binary serves both layouts,
//! and every byte the child writes to stdout has to parse as a frame.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use horon_agent_extension_sdk::wire;
use horon_agent_extension_sdk::{ExtensionManifest, ObserverEvent, PackageKind};

const TIMEOUT: Duration = Duration::from_secs(20);
const DECLARATION: &str = "extension.toml";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Extension,
    Plugin,
}

struct Staged {
    _temp: TempDir,
    root: PathBuf,
    binary: PathBuf,
    home: PathBuf,
    declaration: PathBuf,
}

fn stage(layout: Layout) -> Staged {
    let temp = tempfile::tempdir().expect("a temporary directory");
    let home = temp.path().join("horon-home");
    fs::create_dir_all(&home).expect("an isolated HORON_HOME");
    let root = temp.path().join("package");
    fs::create_dir_all(&root).expect("the package root");
    let layouts = Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts");
    let declaration = match layout {
        Layout::Extension => {
            let declaration = root.join(DECLARATION);
            fs::copy(layouts.join(DECLARATION), &declaration).expect("the declaration");
            declaration
        }
        Layout::Plugin => {
            fs::copy(layouts.join("plugin.toml"), root.join("plugin.toml")).expect("the plugin declaration");
            let bundled = root.join("extension");
            fs::create_dir_all(&bundled).expect("the bundled directory");
            let declaration = bundled.join(DECLARATION);
            fs::copy(layouts.join(DECLARATION), &declaration).expect("the bundled declaration");
            for resource in ["skills", "commands", "mcp", "rules", "prompts"] {
                copy_tree(
                    &layouts.join("resources").join(resource),
                    &root.join("resources").join(resource),
                );
            }
            declaration
        }
    };
    let artifact = Path::new("bin")
        .join(host_target())
        .join(format!("horon-package-example{}", std::env::consts::EXE_SUFFIX));
    let binary = declaration
        .parent()
        .expect("the declaration has a directory")
        .join(&artifact);
    fs::create_dir_all(binary.parent().expect("the artifact has a directory")).expect("the artifact directory");
    fs::copy(env!("CARGO_BIN_EXE_horon-package-example"), &binary).expect("the built example");
    Staged {
        _temp: temp,
        root,
        binary,
        home,
        declaration,
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("a resource directory");
    for entry in fs::read_dir(from).expect("a resource directory") {
        let entry = entry.expect("a resource entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("the entry kind").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("a resource file");
        }
    }
}

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("the package example needs a supported runner, got {os}/{arch}"),
    }
}

fn manifest(path: &Path) -> ExtensionManifest {
    let text = fs::read_to_string(path).expect("the declaration");
    toml::from_str(&text).expect("the declaration parses")
}

fn digest(path: &Path) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = fs::read(path).expect("the declaration");
    let digest = Sha256::digest(&bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter().copied() {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn capabilities() -> wire::CapabilitySet {
    wire::CapabilitySet::new([
        wire::Capability::ToolCall,
        wire::Capability::CommandInvoke,
        wire::Capability::ObserverNotify,
    ])
}

enum Incoming {
    Frame(wire::WireFrame),
    Stray(Vec<u8>),
}

/// The host half of one connection: frames on the child's stdin, every stdout
/// line parsed as a frame on its own thread, and diagnostics collected at exit.
struct Client {
    child: Child,
    input: Option<ChildStdin>,
    incoming: Receiver<Incoming>,
    observations: Vec<wire::WireNotification>,
}

impl Client {
    fn start(staged: &Staged) -> Self {
        let mut child = Command::new(&staged.binary)
            .arg(&staged.root)
            .env("HORON_HOME", &staged.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the example package");
        let input = child.stdin.take().expect("the example's stdin");
        let output = child.stdout.take().expect("the example's stdout");
        let (sender, incoming) = mpsc::channel();
        let _reader = thread::spawn(move || {
            let mut reader = BufReader::new(output);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => return,
                    Ok(_) => {
                        let message = match wire::parse_frame(&line) {
                            Ok(frame) => Incoming::Frame(frame),
                            Err(_) => Incoming::Stray(line.clone()),
                        };
                        if sender.send(message).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        if sender.send(Incoming::Stray(line.clone())).is_err() {
                            return;
                        }
                        return;
                    }
                }
            }
        });
        Self {
            child,
            input: Some(input),
            incoming,
            observations: Vec::new(),
        }
    }

    fn send(&mut self, frame: &wire::WireRequest) {
        self.write(&wire::encode_frame(frame).expect("encode a frame"));
    }

    fn notify(&mut self, frame: &wire::WireNotification) {
        self.write(&wire::encode_frame(frame).expect("encode a notification"));
    }

    fn write(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().expect("the example's stdin");
        input.write_all(bytes).expect("write a frame");
        input.flush().expect("flush a frame");
    }

    fn initialize(&mut self, id: u64, declaration: &ExtensionManifest, kind: PackageKind) -> wire::InitializeResult {
        let params = wire::InitializeParams {
            protocol: wire::HOST_PROTOCOL_RANGE,
            capabilities: capabilities(),
            package_id: declaration.id.clone(),
            kind,
        };
        self.send(&wire::WireRequest::new(
            id,
            wire::WireMethod::Initialize,
            Some(serde_json::to_value(params).expect("initialize params")),
        ));
        let value = self.response(id).result.expect("initialize answers a result");
        serde_json::from_value(value).expect("the initialize result")
    }

    fn cancel(&mut self, request_id: u64) {
        let params = wire::CancelParams { request_id };
        self.notify(&wire::WireNotification::new(
            wire::WireMethod::Cancel,
            Some(serde_json::to_value(params).expect("cancel params")),
        ));
    }

    fn observe(&mut self, event: ObserverEvent) {
        let params = wire::ObserverParams {
            event,
            metadata: json!({ "turn": 1 }),
        };
        self.notify(&wire::WireNotification::new(
            wire::WireMethod::ObserverNotify,
            Some(serde_json::to_value(params).expect("the event")),
        ));
    }

    fn frame(&mut self) -> wire::WireFrame {
        match self.incoming.recv_timeout(TIMEOUT) {
            Ok(Incoming::Frame(frame)) => frame,
            Ok(Incoming::Stray(line)) => panic!(
                "stdout carried a line that is not a frame: {:?}",
                String::from_utf8_lossy(&line)
            ),
            Err(error) => panic!("the example did not answer: {error}"),
        }
    }

    fn response(&mut self, id: u64) -> wire::WireResponse {
        loop {
            match self.frame() {
                wire::WireFrame::Response(response) => {
                    assert_eq!(response.id, id, "an answer arrived for another request");
                    return response;
                }
                wire::WireFrame::Notification(notification) => self.observations.push(notification),
                wire::WireFrame::Request(request) => panic!("the example sent a request: {request:?}"),
            }
        }
    }

    fn notification(&mut self) -> wire::WireNotification {
        match self.frame() {
            wire::WireFrame::Notification(notification) => notification,
            other => panic!("expected the example's report, got {other:?}"),
        }
    }

    fn expect_eof(&mut self) {
        match self.incoming.recv_timeout(TIMEOUT) {
            Ok(Incoming::Frame(frame)) => panic!("the example sent a frame after shutdown: {frame:?}"),
            Ok(Incoming::Stray(line)) => panic!(
                "stdout carried a line that is not a frame: {:?}",
                String::from_utf8_lossy(&line)
            ),
            Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => panic!("stdout stayed open after shutdown"),
        }
    }

    fn finish(mut self) -> (ExitStatus, String) {
        self.input.take();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.child.try_wait().expect("probe the example") {
                Some(status) => {
                    let mut stderr = String::new();
                    if let Some(mut pipe) = self.child.stderr.take() {
                        pipe.read_to_string(&mut stderr).expect("the example's diagnostics");
                    }
                    return (status, stderr);
                }
                None if Instant::now() >= deadline => {
                    self.child.kill().expect("stop the example");
                    panic!("the example did not exit after shutdown");
                }
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _stopped = self.child.kill();
        }
    }
}

fn greet(client: &mut Client, id: u64) -> Value {
    client.send(&wire::WireRequest::new(
        id,
        wire::WireMethod::ToolCall,
        Some(json!({ "tool": "greet", "arguments": { "name": "Ada" } })),
    ));
    let response = client.response(id);
    assert!(response.error.is_none(), "the tool answers: {response:?}");
    response.result.expect("the tool's result")
}

fn hello(client: &mut Client, id: u64) -> Value {
    client.send(&wire::WireRequest::new(
        id,
        wire::WireMethod::CommandInvoke,
        Some(json!({ "command": "hello", "arguments": { "who": "Ada" } })),
    ));
    let response = client.response(id);
    assert!(response.error.is_none(), "the command answers: {response:?}");
    response.result.expect("the command's result")
}

fn shutdown(client: &mut Client, id: u64) {
    client.send(&wire::WireRequest::new(id, wire::WireMethod::Shutdown, None));
    let response = client.response(id);
    assert!(response.error.is_none(), "shutdown answers: {response:?}");
    client.expect_eof();
}

fn assert_isolated(staged: &Staged) {
    let left = fs::read_dir(&staged.home).expect("the isolated HORON_HOME").count();
    assert_eq!(left, 0, "the example must not write Horon state");
}

#[test]
fn the_extension_layout_serves_the_declared_surface() {
    let staged = stage(Layout::Extension);
    let declaration = manifest(&staged.declaration);
    let expected = digest(&staged.declaration);
    let mut client = Client::start(&staged);

    let result = client.initialize(1, &declaration, PackageKind::Extension);
    assert_eq!(
        result.declaration_digest, expected,
        "the digest is the declaration's SHA-256"
    );
    assert_eq!(result.protocol, wire::HOST_PROTOCOL_RANGE);
    let offered = capabilities();
    let declared = declaration.capabilities();
    let expectation = wire::HostExpectation {
        protocol: wire::HOST_PROTOCOL_RANGE,
        capabilities: &offered,
        declared: &declared,
        kind: PackageKind::Extension,
        package_id: &declaration.id,
        declaration_digest: &expected,
    };
    let negotiated = wire::negotiate(&expectation, &result).expect("the answer negotiates");
    assert!(negotiated.capabilities.contains(&wire::Capability::ToolCall));
    assert!(negotiated.capabilities.contains(&wire::Capability::CommandInvoke));
    assert!(negotiated.capabilities.contains(&wire::Capability::ObserverNotify));

    assert_eq!(greet(&mut client, 2), json!({ "greeting": "hello, Ada" }));
    assert_eq!(hello(&mut client, 3)["command"], json!("hello"));

    // The host reports an observed event; the example answers with its own report.
    client.observe(ObserverEvent::TurnStart);
    let reported: wire::ObserverParams = client.notification().parse_params().expect("the example's report");
    assert_eq!(reported.event, ObserverEvent::TurnStart);
    assert_eq!(reported.metadata["observed_by"], json!("horon-package-example"));

    // A cancelled call answers nothing; the connection keeps working.
    client.send(&wire::WireRequest::new(
        4,
        wire::WireMethod::ToolCall,
        Some(json!({ "tool": "wait", "arguments": {} })),
    ));
    // The handler blocks until the host cancels, so the cancel cannot race it.
    thread::sleep(Duration::from_millis(50));
    client.cancel(4);
    assert_eq!(greet(&mut client, 5), json!({ "greeting": "hello, Ada" }));

    shutdown(&mut client, 6);
    let (status, stderr) = client.finish();
    assert!(status.success(), "the example exits cleanly: {stderr}");
    assert!(
        stderr.contains("observed TurnStart"),
        "the observer logs on stderr: {stderr}"
    );
    assert!(
        stderr.contains("serving hello"),
        "the startup line is a diagnostic: {stderr}"
    );
    assert_isolated(&staged);
}

#[test]
fn the_plugin_layout_serves_the_same_binary() {
    let staged = stage(Layout::Plugin);
    // The plugin bundles the standalone declaration byte for byte, so one
    // manifest and one digest cover both layouts.
    let standalone = Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts").join(DECLARATION);
    assert_eq!(
        fs::read(&standalone).expect("the standalone declaration"),
        fs::read(&staged.declaration).expect("the bundled declaration")
    );
    for resource in [
        "resources/skills/greeting/SKILL.md",
        "resources/commands/greeting.md",
        "resources/mcp/servers.json",
        "resources/rules/greeting.md",
        "resources/prompts/greeting.md",
    ] {
        assert!(
            staged.root.join(resource).is_file(),
            "the plugin's declared resource {resource} must be staged"
        );
    }
    let plugin = fs::read_to_string(staged.root.join("plugin.toml")).expect("the plugin declaration");
    assert!(
        plugin.contains("extension/extension.toml"),
        "the plugin names its bundled extension"
    );

    let declaration = manifest(&staged.declaration);
    let expected = digest(&staged.declaration);
    let mut client = Client::start(&staged);
    let result = client.initialize(1, &declaration, PackageKind::Plugin);
    assert_eq!(result.declaration_digest, expected);
    assert_eq!(greet(&mut client, 2), json!({ "greeting": "hello, Ada" }));
    assert_eq!(hello(&mut client, 3)["command"], json!("hello"));
    shutdown(&mut client, 4);
    let (status, stderr) = client.finish();
    assert!(status.success(), "the example exits cleanly: {stderr}");
    assert!(
        stderr.contains("(Plugin)"),
        "the copy reports the plugin kind: {stderr}"
    );
    assert_isolated(&staged);
}

#[test]
fn both_layouts_carry_the_same_declaration() {
    let extension = stage(Layout::Extension);
    let plugin = stage(Layout::Plugin);
    let standalone = fs::read(&extension.declaration).expect("the standalone declaration");
    let bundled = fs::read(&plugin.declaration).expect("the bundled declaration");
    assert_eq!(standalone, bundled, "the two extension.toml files are byte-identical");
    assert_eq!(digest(&extension.declaration), digest(&plugin.declaration));
    let extension_manifest = manifest(&extension.declaration);
    let plugin_manifest = manifest(&plugin.declaration);
    assert_eq!(extension_manifest.capabilities(), plugin_manifest.capabilities());
}
