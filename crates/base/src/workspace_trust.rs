//! Workspace trust for project-local executable configuration.
//!
//! A checkout can carry MCP servers that the agent would spawn. Consent is keyed by the
//! canonical workspace path and a SHA-256 digest over the three project-local config files,
//! so an edited file asks again instead of inheriting an earlier decision. Decisions are
//! recorded in `<agent dir>/workspace-trust.json`; nothing is written into the workspace.
//!
//! Only the workspace files are gated. User-level configuration (`~/.horon/mcp.json`,
//! `~/.claude.json`, `~/.claude/mcp.json`) and native plugin selections stay outside this
//! module: they are the user's own configuration, not a repository's.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Project-local files whose contents the agent will execute when they name MCP servers.
pub const PROJECT_CONFIG_SURFACES: [&str; 3] = [".horon/mcp.json", ".mcp.json", ".claude/mcp.json"];

/// File under the agent directory that records workspace decisions.
pub const TRUST_FILE_NAME: &str = "workspace-trust.json";

/// Environment override. `trust` allows project-local config for this process only and
/// records nothing; `deny` refuses it even when a trusted record exists. `--trust-workspace`
/// is the persistent form and records the decision.
pub const TRUST_ENV: &str = "HORON_AGENT_WORKSPACE_TRUST";

const TRUST_FILE_VERSION: u64 = 1;
const DIGEST_DOMAIN: &[u8] = b"horon-agent/workspace-trust/v1\n";
const SHORT_DIGEST_CHARS: usize = 12;

static CLI_TRUST: AtomicBool = AtomicBool::new(false);
static EXPLICIT_CLI: AtomicBool = AtomicBool::new(false);

/// Whether this process was started with `--trust-workspace`.
pub fn set_cli_trust_workspace(enabled: bool) {
    CLI_TRUST.store(enabled, Ordering::Relaxed);
}

/// Whether this process was started with `--trust-workspace`.
pub fn cli_trust_workspace() -> bool {
    CLI_TRUST.load(Ordering::Relaxed)
}

/// Allow project-local config for a command that names its target explicitly (`mcp test`,
/// `mcp list`, ...), without recording a decision. An explicit command is the consent.
pub fn set_explicit_cli(enabled: bool) {
    EXPLICIT_CLI.store(enabled, Ordering::Relaxed);
}

/// A recorded consent decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustDecision {
    Trusted,
    Denied,
}

/// The decision the store holds for the workspace's current contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustState {
    Trusted,
    Denied,
    Unknown { digest: String },
}

/// Device and inode of the file whose bytes produced the digest, where the platform has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

/// One present project-local config file, with the exact bytes that were hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfigFile {
    pub relative: &'static str,
    pub sha256: String,
    pub bytes: Vec<u8>,
    pub identity: Option<FileIdentity>,
}

/// The project-controlled surfaces of one workspace, read once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    /// Canonical workspace path.
    pub root: PathBuf,
    /// Digest over every surface, present or absent.
    pub digest: String,
    /// Present, readable surfaces in [`PROJECT_CONFIG_SURFACES`] order.
    pub files: Vec<ProjectConfigFile>,
    /// Surfaces that exist but could not be read as a regular file.
    pub unreadable: Vec<&'static str>,
}

/// Whether project-local config may be used for a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectConfigAccess {
    /// The workspace carries no project-local config; nothing to gate.
    Absent,
    /// Trusted for these exact bytes.
    Allowed { snapshot: WorkspaceSnapshot },
    /// Refused; `reason` is the single warning line's explanation.
    Skipped { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSurface {
    path: String,
    sha256: String,
    #[serde(default)]
    len: u64,
    #[serde(default)]
    identity: Option<FileIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustEntry {
    digest: String,
    decision: TrustDecision,
    #[serde(default)]
    recorded_at: Option<String>,
    #[serde(default)]
    surfaces: Vec<StoredSurface>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default = "current_version")]
    version: u64,
    #[serde(default)]
    workspaces: BTreeMap<String, TrustEntry>,
}

fn current_version() -> u64 {
    TRUST_FILE_VERSION
}

impl Default for TrustFile {
    fn default() -> Self {
        Self {
            version: current_version(),
            workspaces: BTreeMap::new(),
        }
    }
}

enum SurfaceRead {
    Absent,
    Present {
        bytes: Vec<u8>,
        identity: Option<FileIdentity>,
    },
    Unreadable,
}

/// Read the project-controlled surfaces of `project_root` and digest them.
pub fn snapshot(project_root: &Path) -> WorkspaceSnapshot {
    let root = canonical_root(project_root);
    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    let mut files = Vec::new();
    let mut unreadable = Vec::new();

    for relative in PROJECT_CONFIG_SURFACES {
        let path = root.join(relative);
        match read_surface(&path) {
            SurfaceRead::Absent => {
                digest.update(format!("{relative}\0absent\n").as_bytes());
            }
            SurfaceRead::Present { bytes, identity } => {
                let sha256 = sha256_hex(&bytes);
                digest.update(format!("{relative}\0present\0{}\0{sha256}\n", bytes.len()).as_bytes());
                files.push(ProjectConfigFile {
                    relative,
                    sha256,
                    bytes,
                    identity,
                });
            }
            SurfaceRead::Unreadable => {
                digest.update(format!("{relative}\0unreadable\n").as_bytes());
                unreadable.push(relative);
            }
        }
    }

    WorkspaceSnapshot {
        root,
        digest: hex::encode(digest.finalize()),
        files,
        unreadable,
    }
}

/// The recorded decision for the workspace's current contents.
///
/// Reads the store and nothing else: it neither prompts nor records, so a caller can
/// decide what to do with an `Unknown` answer.
pub fn trust_state(project_root: &Path) -> TrustState {
    let snapshot = snapshot(project_root);
    let key = workspace_key(&snapshot.root);
    match stored_decision(&key, &snapshot.digest) {
        Some(TrustDecision::Trusted) => TrustState::Trusted,
        Some(TrustDecision::Denied) => TrustState::Denied,
        None => TrustState::Unknown {
            digest: snapshot.digest,
        },
    }
}

/// Record `decision` for `project_root` at `digest`.
///
/// `digest` is the [`WorkspaceSnapshot::digest`] the decision was made against; a later
/// load whose digest differs is untrusted again. The write is a read-modify-write of the
/// whole store, so two processes recording at the same instant can drop each other's entry.
/// That loses a decision, never grants one.
pub fn record(project_root: &Path, digest: &str, decision: TrustDecision) -> Result<()> {
    let snapshot = snapshot(project_root);
    record_snapshot(&snapshot, digest, decision)
}

fn record_snapshot(snapshot: &WorkspaceSnapshot, digest: &str, decision: TrustDecision) -> Result<()> {
    let path = trust_file_path()?;
    let mut file = read_trust_file(&path);
    file.version = TRUST_FILE_VERSION;
    file.workspaces.insert(
        workspace_key(&snapshot.root),
        TrustEntry {
            digest: digest.to_owned(),
            decision,
            recorded_at: Some(chrono::Utc::now().to_rfc3339()),
            surfaces: snapshot
                .files
                .iter()
                .map(|file| StoredSurface {
                    path: file.relative.to_owned(),
                    sha256: file.sha256.clone(),
                    len: file.bytes.len() as u64,
                    identity: file.identity,
                })
                .collect(),
        },
    );
    crate::storage::write_json(&path, &file).with_context(|| format!("could not write {}", path.display()))
}

/// Whether project-local config for `project_root` may be loaded, and the bytes to load.
///
/// This is the one enforcement point. An explicit `--trust-workspace` records the decision
/// and allows; `HORON_AGENT_WORKSPACE_TRUST` decides for this process only; otherwise the
/// stored decision is used and an unknown or stale workspace is refused. A refusal writes
/// nothing, so a later launch asks again.
pub fn authorize_project_config(project_root: &Path) -> ProjectConfigAccess {
    let snapshot = snapshot(project_root);
    let key = workspace_key(&snapshot.root);

    if !snapshot.unreadable.is_empty() {
        let reason = format!("{} could not be read as a regular file", snapshot.unreadable.join(", "));
        warn_skipped(&key, &snapshot.digest, &reason);
        return ProjectConfigAccess::Skipped { reason };
    }
    if snapshot.files.is_empty() {
        return ProjectConfigAccess::Absent;
    }

    match env_decision() {
        Some(TrustDecision::Denied) => {
            let reason = format!("{TRUST_ENV}=deny");
            warn_skipped(&key, &snapshot.digest, &reason);
            ProjectConfigAccess::Skipped { reason }
        }
        Some(TrustDecision::Trusted) => ProjectConfigAccess::Allowed { snapshot },
        None if EXPLICIT_CLI.load(Ordering::Relaxed) => ProjectConfigAccess::Allowed { snapshot },
        None if cli_trust_workspace() => {
            if let Err(error) = record_snapshot(&snapshot, &snapshot.digest, TrustDecision::Trusted) {
                eprintln!("MCP: workspace trust for {key} could not be recorded: {error}");
            }
            ProjectConfigAccess::Allowed { snapshot }
        }
        None => {
            let entry = stored_decision(&key, &snapshot.digest);
            match entry {
                Some(TrustDecision::Trusted) => ProjectConfigAccess::Allowed { snapshot },
                Some(TrustDecision::Denied) => {
                    let reason = "denied by a recorded decision".to_owned();
                    warn_skipped(&key, &snapshot.digest, &reason);
                    ProjectConfigAccess::Skipped { reason }
                }
                None => {
                    let reason = untrusted_reason(&key, &snapshot);
                    warn_skipped(&key, &snapshot.digest, &reason);
                    ProjectConfigAccess::Skipped { reason }
                }
            }
        }
    }
}

fn untrusted_reason(key: &str, snapshot: &WorkspaceSnapshot) -> String {
    let entry = trust_file_path()
        .ok()
        .and_then(|path| read_trust_file(&path).workspaces.get(key).cloned());
    match entry {
        Some(entry) if entry.digest != snapshot.digest => {
            let changed = changed_surfaces(&entry, snapshot);
            let changed = if changed.is_empty() {
                "config digest changed".to_owned()
            } else {
                format!("{} changed", changed.join(", "))
            };
            format!(
                "project config changed since it was trusted ({changed}); trusted digest {}…, current {}…; \
                 pass --trust-workspace or set {TRUST_ENV}=trust to allow it",
                short_digest(&entry.digest),
                short_digest(&snapshot.digest)
            )
        }
        _ => format!(
            "workspace is not trusted (digest {}…); pass --trust-workspace or set {TRUST_ENV}=trust to allow it",
            short_digest(&snapshot.digest)
        ),
    }
}

fn changed_surfaces(entry: &TrustEntry, snapshot: &WorkspaceSnapshot) -> Vec<&'static str> {
    PROJECT_CONFIG_SURFACES
        .iter()
        .copied()
        .filter(|relative| {
            let before = entry
                .surfaces
                .iter()
                .find(|surface| surface.path == *relative)
                .map(|surface| surface.sha256.as_str());
            let after = snapshot
                .files
                .iter()
                .find(|file| file.relative == *relative)
                .map(|file| file.sha256.as_str());
            before != after
        })
        .collect()
}

fn warn_skipped(key: &str, digest: &str, reason: &str) {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let fingerprint = format!("{key}\u{0}{digest}");
    let first = match warned.lock() {
        Ok(mut warned) => warned.insert(fingerprint),
        Err(poisoned) => poisoned.into_inner().insert(fingerprint),
    };
    if first {
        eprintln!("MCP: project-local config skipped in {key}: {reason}");
    }
}

fn env_decision() -> Option<TrustDecision> {
    let value = std::env::var(TRUST_ENV).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "trust" => Some(TrustDecision::Trusted),
        "deny" => Some(TrustDecision::Denied),
        _ => None,
    }
}

fn stored_decision(key: &str, digest: &str) -> Option<TrustDecision> {
    let path = trust_file_path().ok()?;
    read_trust_file(&path)
        .workspaces
        .get(key)
        .filter(|entry| entry.digest == digest)
        .map(|entry| entry.decision)
}

/// Record a trusted decision for `project_root` without a CLI flag.
///
/// Test-only: the tests of other modules need a trusted workspace to exercise
/// the code paths that run after authorization, and they must not depend on
/// process-global CLI state.
#[cfg(any(test, feature = "test-support"))]
pub fn trust_for_tests(project_root: &Path) {
    let snapshot = snapshot(project_root);
    if let Err(error) = record_snapshot(&snapshot, &snapshot.digest, TrustDecision::Trusted) {
        panic!("could not record test trust for {}: {error}", snapshot.root.display());
    }
}

#[cfg(any(test, feature = "test-support"))]
static TRUST_FILE_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

/// Pin the trust store for this process. Test-only: the store normally follows
/// the agent home, which tests of other modules move underneath a running suite.
#[cfg(any(test, feature = "test-support"))]
pub fn set_trust_file_for_tests(path: PathBuf) {
    let slot = TRUST_FILE_OVERRIDE.get_or_init(|| Mutex::new(None));
    match slot.lock() {
        Ok(mut slot) => *slot = Some(path),
        Err(poisoned) => *poisoned.into_inner() = Some(path),
    }
}

fn trust_file_path() -> Result<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(path) = TRUST_FILE_OVERRIDE.get().and_then(|slot| match slot.lock() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }) {
        return Ok(path);
    }
    Ok(crate::storage::horon_agent_dir()?.join(TRUST_FILE_NAME))
}

fn read_trust_file(path: &Path) -> TrustFile {
    if !path.exists() {
        return TrustFile::default();
    }
    match crate::storage::read_json::<TrustFile>(path) {
        Ok(file) if file.version <= TRUST_FILE_VERSION => file,
        Ok(file) => {
            crate::logging::warn(&format!(
                "workspace trust: {} records version {}, which this build does not read; treating every workspace as untrusted",
                path.display(),
                file.version
            ));
            TrustFile::default()
        }
        Err(error) => {
            crate::logging::warn(&format!(
                "workspace trust: {} could not be read ({error}); treating every workspace as untrusted",
                path.display()
            ));
            TrustFile::default()
        }
    }
}

fn read_surface(path: &Path) -> SurfaceRead {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SurfaceRead::Absent,
        Err(_) => return SurfaceRead::Unreadable,
    };
    if !metadata.is_file() {
        return SurfaceRead::Unreadable;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return SurfaceRead::Unreadable;
    };
    let Ok(before) = file.metadata() else {
        return SurfaceRead::Unreadable;
    };
    let identity = identity_of(&before);
    let mut bytes = Vec::new();
    if (&file).read_to_end(&mut bytes).is_err() {
        return SurfaceRead::Unreadable;
    }
    let Ok(after) = file.metadata() else {
        return SurfaceRead::Unreadable;
    };
    if identity_of(&after) != identity || after.len() != bytes.len() as u64 {
        return SurfaceRead::Unreadable;
    }
    SurfaceRead::Present { bytes, identity }
}

fn identity_of(metadata: &std::fs::Metadata) -> Option<FileIdentity> {
    let ids = horon_agent_platform::fs::file_ids(metadata);
    // A platform without a device or inode reports zeros; there the digest
    // alone decides whether the file changed.
    (ids.device != 0 || ids.inode != 0).then_some(FileIdentity {
        device: ids.device,
        inode: ids.inode,
    })
}

fn canonical_root(project_root: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(project_root) {
        return canonical;
    }
    if project_root.is_absolute() {
        return project_root.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(project_root))
        .unwrap_or_else(|_| project_root.to_path_buf())
}

fn workspace_key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn short_digest(digest: &str) -> &str {
    let end = digest.len().min(SHORT_DIGEST_CHARS);
    &digest[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpConfig;

    struct EnvGuard {
        home: Option<std::ffi::OsString>,
        trust: Option<std::ffi::OsString>,
        // Declared last so the lock is released after `Drop` restores the environment.
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn install(home: &Path) -> Self {
            let guard = crate::storage::lock_test_env();
            let saved = Self {
                home: std::env::var_os("HORON_HOME"),
                trust: std::env::var_os(TRUST_ENV),
                _lock: guard,
            };
            crate::env::set_var("HORON_HOME", home);
            crate::env::remove_var(TRUST_ENV);
            set_cli_trust_workspace(false);
            set_trust_file_for_tests(home.join(TRUST_FILE_NAME));
            saved
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            set_cli_trust_workspace(false);
            match &self.home {
                Some(value) => crate::env::set_var("HORON_HOME", value),
                None => crate::env::remove_var("HORON_HOME"),
            }
            match &self.trust {
                Some(value) => crate::env::set_var(TRUST_ENV, value),
                None => crate::env::remove_var(TRUST_ENV),
            }
        }
    }

    fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        for (relative, contents) in files {
            let path = workspace.path().join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("config parent");
            }
            std::fs::write(path, contents).expect("write project config");
        }
        workspace
    }

    fn trust_file(home: &Path) -> PathBuf {
        home.join(TRUST_FILE_NAME)
    }

    #[test]
    fn trust_reasks_after_content_change() {
        let home = tempfile::tempdir().expect("home");
        let _env = EnvGuard::install(home.path());
        let project = workspace(&[(".mcp.json", r#"{"mcpServers":{"alpha":{"command":"alpha-bin"}}}"#)]);

        let first = snapshot(project.path());
        assert_eq!(
            trust_state(project.path()),
            TrustState::Unknown {
                digest: first.digest.clone()
            }
        );
        record(project.path(), &first.digest, TrustDecision::Trusted).expect("record trust");
        assert_eq!(trust_state(project.path()), TrustState::Trusted);
        assert!(
            McpConfig::load_for_dir(Some(project.path()))
                .servers
                .contains_key("alpha")
        );

        std::fs::write(
            project.path().join(".mcp.json"),
            r#"{"mcpServers":{"beta":{"command":"beta-bin"}}}"#,
        )
        .expect("mutate project config");

        let second = snapshot(project.path());
        assert_ne!(first.digest, second.digest);
        assert_eq!(
            trust_state(project.path()),
            TrustState::Unknown {
                digest: second.digest.clone()
            }
        );

        let loaded = McpConfig::load_for_dir(Some(project.path()));
        assert!(!loaded.servers.contains_key("alpha"));
        assert!(!loaded.servers.contains_key("beta"));

        let ProjectConfigAccess::Skipped { reason } = authorize_project_config(project.path()) else {
            panic!("a changed config must be refused");
        };
        assert!(reason.contains(".mcp.json"), "{reason}");
        assert!(reason.contains(short_digest(&first.digest)), "{reason}");
        assert!(reason.contains(short_digest(&second.digest)), "{reason}");
    }

    #[test]
    fn headless_skips_untrusted_project_config() {
        let home = tempfile::tempdir().expect("home");
        let _env = EnvGuard::install(home.path());
        let project = workspace(&[(".mcp.json", r#"{"mcpServers":{"alpha":{"command":"alpha-bin"}}}"#)]);

        let loaded = McpConfig::load_for_dir(Some(project.path()));
        assert!(!loaded.servers.contains_key("alpha"));
        assert!(
            !trust_file(home.path()).exists(),
            "a skipped load must not persist a decision"
        );

        let ProjectConfigAccess::Skipped { reason } = authorize_project_config(project.path()) else {
            panic!("an untrusted workspace must be refused");
        };
        assert!(reason.contains("not trusted"), "{reason}");
        assert!(!trust_file(home.path()).exists());
    }

    #[test]
    fn cli_trust_flag_records_decision() {
        let home = tempfile::tempdir().expect("home");
        let _env = EnvGuard::install(home.path());
        let project = workspace(&[(".mcp.json", r#"{"mcpServers":{"alpha":{"command":"alpha-bin"}}}"#)]);

        set_cli_trust_workspace(true);
        let loaded = McpConfig::load_for_dir(Some(project.path()));
        assert!(loaded.servers.contains_key("alpha"));

        let stored: TrustFile = crate::storage::read_json(&trust_file(home.path())).expect("the decision was recorded");
        let entry = stored
            .workspaces
            .get(&workspace_key(&snapshot(project.path()).root))
            .expect("the workspace entry");
        assert_eq!(entry.decision, TrustDecision::Trusted);
        assert_eq!(entry.digest, snapshot(project.path()).digest);

        set_cli_trust_workspace(false);
        assert!(
            McpConfig::load_for_dir(Some(project.path()))
                .servers
                .contains_key("alpha")
        );
    }

    #[test]
    fn env_trust_allows_without_recording_and_env_deny_wins() {
        let home = tempfile::tempdir().expect("home");
        let _env = EnvGuard::install(home.path());
        let project = workspace(&[(".mcp.json", r#"{"mcpServers":{"alpha":{"command":"alpha-bin"}}}"#)]);

        crate::env::set_var(TRUST_ENV, "trust");
        assert!(
            McpConfig::load_for_dir(Some(project.path()))
                .servers
                .contains_key("alpha")
        );
        assert!(!trust_file(home.path()).exists(), "the env override records nothing");

        crate::env::set_var(TRUST_ENV, "deny");
        assert!(
            !McpConfig::load_for_dir(Some(project.path()))
                .servers
                .contains_key("alpha")
        );
        assert!(!trust_file(home.path()).exists());
    }

    #[test]
    fn user_level_config_is_not_gated() {
        let home = tempfile::tempdir().expect("home");
        let _env = EnvGuard::install(home.path());
        let project = workspace(&[(
            ".mcp.json",
            r#"{"mcpServers":{"project-alpha":{"command":"alpha-bin"}}}"#,
        )]);
        std::fs::write(
            home.path().join("mcp.json"),
            r#"{"mcpServers":{"user-alpha":{"command":"user-bin"}}}"#,
        )
        .expect("write user config");

        let snapshot = snapshot(project.path());
        let surfaces: Vec<&str> = snapshot.files.iter().map(|file| file.relative).collect();
        assert_eq!(surfaces, [".mcp.json"], "only workspace files enter the digest");
        assert!(
            PROJECT_CONFIG_SURFACES
                .iter()
                .all(|relative| !relative.starts_with("~")),
            "no user-level path is a gated surface"
        );

        let loaded = McpConfig::load_for_dir(Some(project.path()));
        assert!(!loaded.servers.contains_key("project-alpha"));
        assert!(home.path().join("mcp.json").exists(), "the user config is left alone");
    }

    #[test]
    fn skipped_reason_never_echoes_config_contents() {
        let home = tempfile::tempdir().expect("home");
        let _env = EnvGuard::install(home.path());
        let sentinel = "IGNORE-ALL-PREVIOUS-INSTRUCTIONS-8f21";
        let project = workspace(&[(
            ".mcp.json",
            &format!(r#"{{"mcpServers":{{"x":{{"command":"x","args":["{sentinel}"],"instructions":"{sentinel}"}}}}}}"#),
        )]);

        let ProjectConfigAccess::Skipped { reason } = authorize_project_config(project.path()) else {
            panic!("an untrusted workspace must be refused");
        };
        assert!(!reason.contains(sentinel), "{reason}");
        assert!(!format!("{:?}", authorize_project_config(project.path())).contains(sentinel));
    }

    #[test]
    fn trust_operations_leave_the_workspace_untouched() {
        let home = tempfile::tempdir().expect("home");
        let _env = EnvGuard::install(home.path());
        let project = workspace(&[(".mcp.json", r#"{"mcpServers":{"alpha":{"command":"alpha-bin"}}}"#)]);
        let before: Vec<_> = std::fs::read_dir(project.path())
            .expect("list workspace")
            .map(|entry| entry.expect("entry").file_name())
            .collect();

        let _ = authorize_project_config(project.path());
        let snapshot = snapshot(project.path());
        record(project.path(), &snapshot.digest, TrustDecision::Trusted).expect("record");

        let after: Vec<_> = std::fs::read_dir(project.path())
            .expect("list workspace")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(before, after, "trust must not write into the workspace");
        assert!(trust_file(home.path()).exists());
    }

    #[test]
    fn digest_covers_the_opened_file_identity() {
        let home = tempfile::tempdir().expect("home");
        let _env = EnvGuard::install(home.path());
        let project = workspace(&[(".mcp.json", r#"{"mcpServers":{"alpha":{"command":"alpha-bin"}}}"#)]);

        let first = snapshot(project.path());
        let Some(identity) = first.files[0].identity else {
            // A platform with no inode has nothing to compare here.
            return;
        };
        let metadata = std::fs::metadata(project.path().join(".mcp.json")).expect("metadata");
        let ids = horon_agent_platform::fs::file_ids(&metadata);
        assert_eq!(identity.device, ids.device);
        assert_eq!(identity.inode, ids.inode);

        let replacement = project.path().join("replacement.json");
        std::fs::write(&replacement, r#"{"mcpServers":{"alpha":{"command":"alpha-bin"}}}"#).expect("write");
        std::fs::rename(&replacement, project.path().join(".mcp.json")).expect("replace");

        let second = snapshot(project.path());
        assert_ne!(
            first.files[0].identity, second.files[0].identity,
            "a replaced file has a new inode"
        );
    }

    #[test]
    fn absent_workspace_config_is_not_gated() {
        let home = tempfile::tempdir().expect("home");
        let _env = EnvGuard::install(home.path());
        let project = tempfile::tempdir().expect("empty workspace");

        assert_eq!(authorize_project_config(project.path()), ProjectConfigAccess::Absent);
        assert_eq!(
            trust_state(project.path()),
            TrustState::Unknown {
                digest: snapshot(project.path()).digest
            }
        );
    }
}
