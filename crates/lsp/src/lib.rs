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

/// One text replacement a server asked for.
///
/// Lines and characters are 0-based LSP positions, and a character is a UTF-16
/// code unit, so a caller must convert both before slicing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub new_text: String,
}

/// Every edit one file receives from a workspace edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub path: std::path::PathBuf,
    pub edits: Vec<TextEdit>,
}

/// A server-computed edit across one or more files.
///
/// Only text edits are represented: file creation, renaming, and deletion are
/// outside what the `lsp` tool applies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceEdit {
    pub files: Vec<FileEdit>,
}

impl WorkspaceEdit {
    pub fn is_empty(&self) -> bool {
        self.files.iter().all(|file| file.edits.is_empty())
    }

    pub fn edit_count(&self) -> usize {
        self.files.iter().map(|file| file.edits.len()).sum()
    }
}

/// One code action a server offers for a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAction {
    pub title: String,
    pub kind: Option<String>,
    /// The edits this action applies, when it carries any.
    pub edit: Option<WorkspaceEdit>,
    /// True when the only thing the action offers is a command, which the tool
    /// lists but never executes.
    pub command_only: bool,
    /// Why the server disabled the action, when it did.
    pub disabled: Option<String>,
}

/// One live language server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStatus {
    pub server: &'static str,
    pub root: std::path::PathBuf,
    pub pid: u32,
    pub idle_secs: u64,
}
