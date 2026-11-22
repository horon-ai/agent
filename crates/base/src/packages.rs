//! Validation and installation of user packages.
//!
//! [`validate_package`] reads a package directory and selects the executable
//! for this host. [`PackageStore`] publishes validated copies under their
//! content digest and records them in one `installed.json`, so an install, a
//! disable, a removal and a verification all observe the same generation.
//! [`PackageStore::sync_inbox`] installs complete directories dropped into
//! `inbox/` through the same path.
//! [`PackageStore::prepare_executable`] verifies the bytes of an installed
//! executable and [`PackageExecutable::start`] runs it as one supervised child
//! whose environment the host controls.

mod catalog;
mod hook_resources;
mod inbox;
mod manifest;
mod mcp_resources;
mod native_target;
mod process;
mod store;
mod text_resources;

pub use catalog::{
    CATALOG_VERSION, CatalogView, CollectionReport, InstallRequest, PackageLease, PackageRecord, PackageScope,
    PackageState, PackageStore, StoredFile, VerificationReport,
};
pub use hook_resources::observation_events;
pub use horon_agent_extension_types::{
    CommandDeclaration, EXTENSION_PROTOCOL, ExtensionManifest, MANIFEST_VERSION, ObserverEvent, PackageKind,
    PackageResources, PluginManifest, TARGETS, ToolDeclaration,
};
pub use inbox::{InboxEntry, InboxOutcome, InboxReport};
pub use manifest::{ValidatedPackage, read_recorded_declaration, validate_package};
pub use mcp_resources::{PackageMcpKind, PackageMcpServer};
pub use process::{PackageExecutable, PackageProcess};
pub use text_resources::{
    MAX_PACKAGE_RULE_BYTES, MAX_PROMPT_BYTES, MAX_PUBLIC_NAME, MAX_RULE_BYTES, MAX_SKILL_BYTES, PackagePrompt,
    PackageRule, PackageSkill, PackageText, PackageTextCache, PackageTextResources, validate_resources,
};
