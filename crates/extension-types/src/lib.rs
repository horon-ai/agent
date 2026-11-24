//! Data shared by the package installer, host, and author SDK.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod control;
pub mod wire;

pub const MANIFEST_VERSION: u32 = 1;
pub const EXTENSION_PROTOCOL: &str = "1.0";

pub const TARGETS: [&str; 6] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "aarch64-pc-windows-msvc",
    "x86_64-pc-windows-msvc",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    Extension,
    Plugin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub name: String,
    pub protocol: String,
    pub targets: BTreeMap<String, String>,
    #[serde(default)]
    pub tools: Vec<ToolDeclaration>,
    #[serde(default)]
    pub events: Vec<ObserverEvent>,
    #[serde(default)]
    pub commands: Vec<CommandDeclaration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDeclaration {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ObserverEvent {
    #[serde(rename = "session.start")]
    SessionStart,
    #[serde(rename = "session.end")]
    SessionEnd,
    #[serde(rename = "turn.start")]
    TurnStart,
    #[serde(rename = "turn.end")]
    TurnEnd,
    #[serde(rename = "tool.before")]
    ToolBefore,
    #[serde(rename = "tool.after")]
    ToolAfter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDeclaration {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub name: String,
    /// Path to the one bundled extension manifest, relative to the package root.
    pub extension: String,
    pub resources: PackageResources,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackageResources {
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub hooks: Vec<String>,
    pub mcp: Vec<String>,
    pub rules: Vec<String>,
    pub prompts: Vec<String>,
}

impl PackageResources {
    /// All resource paths are relative to the package root.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        [
            &self.skills,
            &self.commands,
            &self.hooks,
            &self.mcp,
            &self.rules,
            &self.prompts,
        ]
        .into_iter()
        .flatten()
        .map(String::as_str)
    }
}
