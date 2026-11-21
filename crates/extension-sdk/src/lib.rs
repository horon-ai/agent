//! Author SDK for a user-installed Rust package.
//!
//! A package is one Rust executable plus a declaration: a standalone extension
//! ships `extension.toml`, and a plugin ships `plugin.toml` that bundles one
//! byte-identical `extension.toml` with resource files. The host starts that
//! executable, speaks the shared wire contract over stdin and stdout, and hands
//! it tool calls, executable commands and the events it declared.
//!
//! The SDK owns the parts every author would otherwise copy:
//!
//! - **framing**: bounded, newline-delimited JSON-RPC frames parsed and encoded
//!   by [`horon_agent_extension_types::wire`], so both ends speak one shape;
//! - **negotiation**: [`Declaration::accept`] checks the host's `initialize`
//!   offer against the installed declaration and answers with the supported
//!   protocol range, the declared capabilities and the declaration digest;
//! - **schema**: [`Declaration`] reads the declaration it is installed with and
//!   [`Declaration::check`] refuses to serve when the code and the declaration
//!   name different tools, commands or events;
//! - **cancellation**: [`serve`] keeps reading frames while a handler runs, so a
//!   `cancel` notification reaches the handler through [`Context::is_cancelled`]
//!   and a cancelled call answers nothing.
//!
//! It does not supervise a process, approve a tool call or read Horon state:
//! the host owns those. Nothing here depends on the agent runtime, `app-core` or
//! the terminal client, and the distributed agent binary does not depend on this
//! crate.
//!
//! ```no_run
//! use serde_json::{json, Value};
//!
//! use horon_agent_extension_sdk::{Context, Declaration, Extension, ObserverEvent, RpcError, serve_stdio};
//!
//! const TOOLS: [&str; 1] = ["echo"];
//! const COMMANDS: [&str; 1] = ["status"];
//! const EVENTS: [ObserverEvent; 1] = [ObserverEvent::TurnStart];
//!
//! struct Echo;
//!
//! impl Extension for Echo {
//!     fn tools(&self) -> &'static [&'static str] {
//!         &TOOLS
//!     }
//!
//!     fn commands(&self) -> &'static [&'static str] {
//!         &COMMANDS
//!     }
//!
//!     fn events(&self) -> &'static [ObserverEvent] {
//!         &EVENTS
//!     }
//!
//!     fn call_tool(&self, name: &str, arguments: &Value, _context: &mut Context<'_>) -> Result<Value, RpcError> {
//!         match name {
//!             "echo" => Ok(json!({ "echo": arguments })),
//!             other => Err(RpcError::method_not_found(other)),
//!         }
//!     }
//!
//!     fn invoke_command(&self, name: &str, _arguments: &Value, _context: &mut Context<'_>) -> Result<Value, RpcError> {
//!         Err(RpcError::method_not_found(name))
//!     }
//! }
//!
//! // The host starts the installed copy with its package root as the first
//! // argument, so `main` loads that root and serves stdio.
//! fn run(root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
//!     let declaration = Declaration::load(root)?;
//!     declaration.check(&Echo)?;
//!     serve_stdio(Echo, declaration)?;
//!     Ok(())
//! }
//! ```

mod declaration;
mod serve;

pub use declaration::{Declaration, LoadError, PROTOCOL_RANGE};
pub use horon_agent_extension_types::wire::{self, RpcError};
pub use horon_agent_extension_types::{ExtensionManifest, ObserverEvent, PackageKind};
pub use serve::{
    CANCELLED, Context, Extension, INVALID_PARAMS, INVALID_REQUEST, NOT_INITIALIZED, PROTOCOL_UNAVAILABLE, ServeError,
    SurfaceError, serve, serve_stdio,
};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
