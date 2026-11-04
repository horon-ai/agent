//! A small, purpose-built Language Server Protocol client.
//!
//! The `lsp` tool needs four things from a language server: diagnostics for one
//! open file, go-to-definition, references, and symbols. This crate provides
//! exactly that over the standard JSON-RPC framing, keeps one client per
//! `(server, workspace root)`, and shuts an idle server down after ten minutes.
//!
//! Only the built-in server table in [`server`] is used: there is no config
//! file to edit and no plugin surface. A crashed server is respawned by the
//! next request instead of being reported forever.

mod client;
mod framing;
mod server;

pub use client::{Client, running_clients};
pub use server::{ServerSpec, is_installed, server_for_name, server_for_path, workspace_root};

/// How serious one diagnostic is, in the order a reader cares about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

impl DiagnosticSeverity {
    /// The lowercase word rendered in tool output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "information",
            Self::Hint => "hint",
        }
    }
}

/// One problem a language server reported for a file.
///
/// `line` and `column` are 1-based, so they can be printed as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: std::path::PathBuf,
    pub line: u32,
    pub column: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

/// One place a language server pointed at, with 1-based line and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: std::path::PathBuf,
    pub line: u32,
    pub column: u32,
}

/// One named symbol, with 1-based line and column.
///
/// Document symbols nest through `children`; workspace symbols are flat and
/// carry the file the symbol lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: &'static str,
    pub path: Option<std::path::PathBuf>,
    pub line: u32,
    pub column: u32,
    pub children: Vec<Symbol>,
}

/// One live language server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStatus {
    pub server: &'static str,
    pub root: std::path::PathBuf,
    pub pid: u32,
    pub idle_secs: u64,
}
