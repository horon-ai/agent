//! Validation shared by manual installation and inbox discovery.

mod manifest;
mod native_target;

pub use horon_agent_extension_types::{
    CommandDeclaration, EXTENSION_PROTOCOL, ExtensionManifest, MANIFEST_VERSION, ObserverEvent, PackageKind,
    PackageResources, PluginManifest, TARGETS, ToolDeclaration,
};
pub use manifest::{ValidatedPackage, validate_package};
