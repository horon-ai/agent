//! Runtime-backed commands, shared by all non-terminal clients.
//!
//! Installed packages add `/id:command` slash commands. They travel through
//! this same enum rather than a request of their own: a client discovers them
//! with `PackageCommands` and runs one with `PackageCommand`, so an older
//! client that knows neither keeps its existing surface and an older server
//! answers the unknown variant with an error instead of dropping the frame.
use serde::{Deserialize, Serialize};

/// Commands executed by the attached runtime, never submitted as an agent prompt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCommand {
    Skills,
    Context,
    Info,
    Memory,
    Swarm,
    Agents,
    #[serde(rename = "modelpreset")]
    ModelPreset,
    SubagentModel,
    Trim,
    Autoreview,
    Autojudge,
    Transport,
    Fast,
    Overnight,
    Productivity,
    Initiatives,
    RefreshModelList,
    Continue,
    ResumeAll,
    Remote,
    ServerReload,
    Review,
    Judge,
    Subagent,
    Auth,
    Logout,
    Plan,
    /// Bounded notes about this session's visible work and lifecycle hooks.
    ///
    /// The reply is the observation view for the session that asked, never
    /// another session's, and it is a client report: nothing in it is added to
    /// the conversation or sent to the model.
    Observations,
    Goal,
    Advisor,
    Dirs,
    Mcp,
    /// List the slash commands the installed packages supply for this session.
    ///
    /// The reply's `data` is a [`PackageCommandList`]. The entries belong to the
    /// package generation the session currently observes, so a listing and an
    /// invocation name the same generation.
    PackageCommands,
    /// Run one package command.
    ///
    /// `args` carries `[package_id, command, …argument words]`. The reply's
    /// `data` is a [`PackageCommandOutcome`]: a template answers with the text
    /// a client submits as a user message, an executable command with the
    /// value its child returned after the tool policy allowed the call.
    PackageCommand,
}

/// How one package command runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackageCommandKind {
    /// A Markdown resource that expands into a user message.
    Template,
    /// A command the package's executable answers over the wire.
    Executable,
}

/// One slash command an installed package supplies.
///
/// `name` is what a user types after the slash, `<package id>:<command>`. The
/// other fields say where it came from and how it runs, so a client can list it
/// without reading the package area.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageCommandInfo {
    /// The typed name, without the leading slash.
    pub name: String,
    /// The package that declares it.
    pub package_id: String,
    /// The declared command name, the part after the colon.
    pub command: String,
    pub kind: PackageCommandKind,
    /// One line saying what the command does.
    pub description: String,
    /// The argument hint the resource declares, when it declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
}

/// The reply to [`RuntimeCommand::PackageCommands`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PackageCommandList {
    /// The package catalog generation the entries were read at. Zero when the
    /// session observes no package at all.
    pub generation: u64,
    /// Sorted by name.
    #[serde(default)]
    pub commands: Vec<PackageCommandInfo>,
}

/// The reply to [`RuntimeCommand::PackageCommand`].
///
/// Exactly one of `text` and `output` is set: a template expands to the message
/// a client sends, and an executable command answers with what its child
/// returned. `data` keeps the child's value unrendered for a client that wants
/// the structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageCommandOutcome {
    /// The typed name, without the leading colon.
    pub name: String,
    pub package_id: String,
    pub command: String,
    pub kind: PackageCommandKind,
    /// The expanded template, for [`PackageCommandKind::Template`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The rendered child answer, for [`PackageCommandKind::Executable`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// The child's value as it arrived, for a client that wants the structure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Structured runtime data plus a human-readable rendering. Data is command
/// specific and additive. Clients must ignore unknown fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeCommandOutput {
    pub session_id: String,
    pub command: RuntimeCommand,
    pub data: serde_json::Value,
    pub message: String,
}
