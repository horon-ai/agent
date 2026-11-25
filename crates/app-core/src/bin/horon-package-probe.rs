//! The child half of the package tool integration test.
//!
//! This binary exists only to be *installed*: `tests/package_tools.rs` copies
//! it into a package fixture, the installer validates it like any other native
//! executable, and the host starts the installed copy. It speaks the bounded
//! wire protocol on stdin and stdout and nothing else; every diagnostic goes to
//! stderr, and it records what it received under `HORON_PACKAGE_STATE` so the
//! test can compare the host's decisions with what actually crossed the pipe.
//!
//! It is not shipped. `test = false` keeps it out of the unit-test surface, and
//! no product code depends on it: the only logic here is the protocol stream a
//! package author's binary would carry.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use horon_agent_extension_types::wire::{
    Capability, CapabilitySet, CommandInvokeParams, FrameError, InitializeParams, InitializeResult, ProtocolRange,
    ProtocolVersion, RpcError, ToolCallParams, WireFrame, WireMethod, WireRequest, WireResponse, encode_frame,
    parse_frame,
};
use serde_json::json;
use sha2::{Digest, Sha256};

/// One appended line per call the child actually received.
const CALLS: &str = "calls.log";
/// One appended line per executable command the child actually received.
const COMMANDS: &str = "commands.log";
/// One appended line per process start, so a test can prove none happened.
const STARTS: &str = "starts.log";
/// Written continuously while a `slow` tool call is pending.
const HEARTBEAT: &str = "heartbeat";
/// The tool that never answers, so a test can revoke a call in flight.
const SLOW_TOOL: &str = "slow";
/// Protocol range this probe answers with.
const PROTOCOL: ProtocolRange = ProtocolRange::exactly(ProtocolVersion::new(1, 0));

fn main() -> ExitCode {
    let Some(root) = package_root() else {
        eprintln!("horon-package-probe: no installed package root");
        return ExitCode::from(2);
    };
    let state = std::env::var_os("HORON_PACKAGE_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let Ok(package_id) = std::env::var("HORON_PACKAGE_ID") else {
        eprintln!("horon-package-probe: the launcher did not set HORON_PACKAGE_ID");
        return ExitCode::from(2);
    };
    if package_id == "probe-stderr" {
        eprintln!("boom on stderr");
        return ExitCode::from(1);
    }
    let digest = match declaration_digest(&root) {
        Ok(digest) => digest,
        Err(error) => {
            eprintln!("horon-package-probe: {error}");
            return ExitCode::from(2);
        }
    };
    append(&state, STARTS, &format!("{package_id}\n"));
    if std::env::args_os().nth(2).as_deref() == Some(std::ffi::OsStr::new("mcp")) {
        return run_mcp_probe();
    }
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let mut output = std::io::stdout().lock();
    let mut granted = CapabilitySet::default();
    let mut initialized = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        match input.read_until(b'\n', &mut line) {
            Ok(0) => return ExitCode::SUCCESS,
            Ok(_) => {}
            Err(error) => {
                eprintln!("horon-package-probe: read failed: {error}");
                return ExitCode::from(1);
            }
        }
        let frame = match parse_frame(&line) {
            Ok(frame) => frame,
            Err(error) => {
                eprintln!("horon-package-probe: refused a frame: {error}");
                continue;
            }
        };
        let request = match frame {
            WireFrame::Request(request) => request,
            // The host's `cancel` and `observer/notify` carry no answer, and a
            // response is the host's business, not this child's.
            WireFrame::Notification(_) | WireFrame::Response(_) => continue,
        };
        let id = request.id;
        if request.method == WireMethod::Initialize {
            if initialized {
                respond(
                    &mut output,
                    WireResponse::fail(id, method_not_found(WireMethod::Initialize)),
                );
                continue;
            }
            match initialize(&request, &package_id, &digest) {
                Ok(result) => {
                    granted = result.capabilities.clone();
                    initialized = true;
                    respond(&mut output, WireResponse::reply(id, json!(result)));
                }
                Err(error) => {
                    respond(&mut output, WireResponse::fail(id, error));
                }
            }
            continue;
        }
        if !initialized {
            respond(&mut output, WireResponse::fail(id, not_initialized()));
            continue;
        }
        if request.method == WireMethod::Shutdown {
            respond(&mut output, WireResponse::reply(id, json!({})));
            return ExitCode::SUCCESS;
        }
        let Some(capability) = request.method.capability() else {
            respond(&mut output, WireResponse::fail(id, method_not_found(request.method)));
            continue;
        };
        if !granted.contains(&capability) {
            respond(&mut output, WireResponse::fail(id, method_not_found(request.method)));
            continue;
        }
        match request.method {
            WireMethod::ToolCall => match request.parse_params::<ToolCallParams>() {
                Ok(params) => {
                    append(&state, CALLS, &format!("{} {}\n", params.tool, params.arguments));
                    if params.tool == SLOW_TOOL {
                        wait_until_killed(&state);
                    }
                    let result = json!({
                        "tool": params.tool,
                        "arguments": params.arguments,
                        "pid": std::process::id(),
                    });
                    respond(&mut output, WireResponse::reply(id, result));
                }
                Err(error) => respond(&mut output, WireResponse::fail(id, invalid_params(error))),
            },
            WireMethod::CommandInvoke => match request.parse_params::<CommandInvokeParams>() {
                Ok(params) => {
                    append(&state, COMMANDS, &format!("{} {}\n", params.command, params.arguments));
                    let result = json!({
                        "command": params.command,
                        "arguments": params.arguments,
                        "pid": std::process::id(),
                    });
                    respond(&mut output, WireResponse::reply(id, result));
                }
                Err(error) => respond(&mut output, WireResponse::fail(id, invalid_params(error))),
            },
            _ => respond(&mut output, WireResponse::fail(id, method_not_found(request.method))),
        }
    }
}

fn package_root() -> Option<PathBuf> {
    std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HORON_PACKAGE_ROOT").map(PathBuf::from))
}

/// Answer the handshake with the declaration this installed copy carries.
fn initialize(request: &WireRequest, package_id: &str, digest: &str) -> Result<InitializeResult, RpcError> {
    let params: InitializeParams = request.parse_params().map_err(invalid_params)?;
    if params.package_id != package_id {
        return Err(RpcError {
            code: -32001,
            message: format!("this copy is package {package_id}, not {}", params.package_id),
            data: None,
        });
    }
    let capabilities = CapabilitySet::new(
        params
            .capabilities
            .iter()
            .filter(|capability| matches!(capability, Capability::ToolCall | Capability::CommandInvoke)),
    );
    Ok(InitializeResult {
        protocol: PROTOCOL,
        capabilities,
        declaration_digest: digest.to_owned(),
    })
}

/// The `slow` tool's whole body: keep proving this process is alive until the
/// host stops it. A revoked call must end with the process tree, so a test reads
/// this file to tell "parked" from "reaped".
fn wait_until_killed(state: &Path) -> ! {
    let heartbeat = state.join(HEARTBEAT);
    loop {
        let appended = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&heartbeat)
            .and_then(|mut file| file.write_all(b"."));
        if let Err(error) = appended {
            // The state directory is gone, so nobody is reading the heartbeat.
            eprintln!("horon-package-probe: could not write the heartbeat: {error}");
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn respond(output: &mut impl Write, response: WireResponse) {
    let bytes = match encode_frame(&response) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("horon-package-probe: could not encode an answer: {error}");
            return;
        }
    };
    if let Err(error) = output.write_all(&bytes).and_then(|()| output.flush()) {
        eprintln!("horon-package-probe: could not write an answer: {error}");
    }
}

fn append(state: &Path, name: &str, text: &str) {
    let path = state.join(name);
    let appended = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(text.as_bytes()));
    if let Err(error) = appended {
        eprintln!("horon-package-probe: could not record {name}: {error}");
    }
}

fn method_not_found(method: WireMethod) -> RpcError {
    RpcError {
        code: RpcError::METHOD_NOT_FOUND,
        message: format!("no handler for {method:?}"),
        data: None,
    }
}

fn not_initialized() -> RpcError {
    RpcError {
        code: -32002,
        message: "initialize first".to_owned(),
        data: None,
    }
}

fn invalid_params(error: FrameError) -> RpcError {
    RpcError {
        code: -32602,
        message: format!("invalid parameters: {error}"),
        data: None,
    }
}

/// Lowercase hex SHA-256 of the declaration this copy carries.
fn declaration_digest(root: &Path) -> std::io::Result<String> {
    let path = declaration_file(root)?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|error| std::io::Error::new(error.kind(), format!("read {}: {error}", path.display())))?;
    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(&bytes) {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    Ok(out)
}

/// The declaration file an installed copy carries.
///
/// A standalone extension keeps it at the root; a plugin bundles exactly one
/// `*/extension.toml`, which is the only nested layout the fixtures use.
fn declaration_file(root: &Path) -> std::io::Result<PathBuf> {
    let root_declaration = root.join("extension.toml");
    if root_declaration.is_file() {
        return Ok(root_declaration);
    }
    let entries = std::fs::read_dir(root)
        .map_err(|error| std::io::Error::new(error.kind(), format!("read {}: {error}", root.display())))?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let candidate = entry.path().join("extension.toml");
        if candidate.is_file() {
            candidates.push(candidate);
        }
    }
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        other => Err(std::io::Error::other(format!(
            "{} holds {other} bundled extension.toml files; expected one",
            root.display()
        ))),
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Serve a minimal newline-delimited JSON-RPC MCP stdio server when invoked
/// with `mode = "mcp"` by plugin MCP tests.
fn run_mcp_probe() -> ExitCode {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(method) = frame.get("method").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let id = frame.get("id");
        if method == "shutdown" {
            if let Some(id) = id {
                let reply = json!({ "jsonrpc": "2.0", "id": id, "result": {} });
                if let Err(error) = writeln!(stdout, "{reply}").and_then(|()| stdout.flush()) {
                    eprintln!("horon-package-probe: could not write shutdown answer: {error}");
                }
            }
            return ExitCode::SUCCESS;
        }
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "package-mcp-probe", "version": "0" }
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "echo",
                    "description": "Report the package child and echo the text it was given",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "text": { "type": "string", "description": "Text to echo" } }
                    }
                }]
            }),
            "tools/call" => {
                let text = frame
                    .get("params")
                    .and_then(|params| params.get("arguments"))
                    .and_then(|arguments| arguments.get("text"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                json!({
                    "content": [{
                        "type": "text",
                        "text": format!("pid={} text={text}", std::process::id())
                    }]
                })
            }
            _ => continue,
        };
        let Some(id) = id else {
            continue;
        };
        let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        if writeln!(stdout, "{reply}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}
