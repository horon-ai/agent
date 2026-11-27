//! The worked example of a user-installed horon Rust package.
//!
//! One executable serves both package kinds, because both carry the same
//! `extension.toml`. The host starts the installed copy with the package root as
//! the first argument; the SDK reads the declaration and its digest from that
//! root and serves the wire on stdin and stdout.
//!
//! stdout is the protocol stream. Every diagnostic in this file goes to stderr.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use horon_agent_extension_sdk::{CANCELLED, Context, Declaration, Extension, ObserverEvent, RpcError, serve_stdio};

const TOOLS: [&str; 2] = ["greet", "wait"];
const COMMANDS: [&str; 1] = ["hello"];
const EVENTS: [ObserverEvent; 2] = [ObserverEvent::TurnStart, ObserverEvent::TurnEnd];
const WAIT_STEP: Duration = Duration::from_millis(5);

/// The package's whole behaviour.
struct Hello;

impl Extension for Hello {
    fn tools(&self) -> &'static [&'static str] {
        &TOOLS
    }

    fn commands(&self) -> &'static [&'static str] {
        &COMMANDS
    }

    fn events(&self) -> &'static [ObserverEvent] {
        &EVENTS
    }

    fn call_tool(&self, name: &str, arguments: &Value, context: &mut Context<'_>) -> Result<Value, RpcError> {
        match name {
            "greet" => {
                let who = arguments.get("name").and_then(Value::as_str).unwrap_or("world");
                Ok(json!({ "greeting": format!("hello, {who}") }))
            }
            "wait" => {
                // Stands in for real work: the loop polls the cancellation the
                // host sends, so a cancelled call returns promptly and the
                // server answers nothing for it.
                while !context.is_cancelled() {
                    std::thread::sleep(WAIT_STEP);
                }
                Err(cancelled())
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
        // stderr, never stdout: stdout carries protocol frames only.
        eprintln!("horon-package-example: observed {event:?} {metadata}");
        context.notify(event, json!({ "observed_by": "horon-package-example" }));
    }
}

fn cancelled() -> RpcError {
    RpcError {
        code: CANCELLED,
        message: "the host cancelled this call".to_owned(),
        data: None,
    }
}

fn package_root() -> Option<PathBuf> {
    match std::env::args_os().nth(1) {
        Some(root) => Some(PathBuf::from(root)),
        None => std::env::var_os("HORON_PACKAGE_ROOT").map(PathBuf::from),
    }
}

fn main() -> ExitCode {
    let Some(root) = package_root() else {
        eprintln!("horon-package-example: pass the installed package root as the first argument");
        return ExitCode::from(2);
    };
    let declaration = match Declaration::load(&root) {
        Ok(declaration) => declaration,
        Err(error) => {
            eprintln!("horon-package-example: {error}");
            return ExitCode::from(2);
        }
    };
    let extension = Hello;
    if let Err(error) = declaration.check(&extension) {
        eprintln!("horon-package-example: {error}");
        return ExitCode::from(2);
    }
    eprintln!(
        "horon-package-example: serving {} ({:?}) with digest {}",
        declaration.manifest().id,
        declaration.kind(),
        declaration.digest()
    );
    match serve_stdio(extension, declaration) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("horon-package-example: {error}");
            ExitCode::FAILURE
        }
    }
}
