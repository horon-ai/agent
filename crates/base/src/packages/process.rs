//! Starting one installed package executable under a single lifetime owner.
//!
//! [`PackageStore::prepare_executable`] takes a catalog lease, reads the catalog
//! under its lock, pins the copy the record names, and verifies the bytes of the
//! executable the child will run. [`PackageExecutable::start`] verifies those
//! bytes again immediately before it starts the child, so a copy that changed,
//! or a path that was swapped for a link, is refused instead of executed.
//!
//! The child receives an absolute program path, the installed root as its first
//! argument, its installed copy as the working directory, a dedicated
//! owner-private state directory, and an environment cleared of everything the
//! host process holds. [`PackageProcess`] is the owner of that child: it hands
//! out the standard streams, stops the process tree, and reaps the leader.
//!
//! The lease is owned from preparation onwards, so it outlives every launch made
//! from that executable: a replacement install cannot reclaim the copy the
//! running child was verified against. One executable owns one lease, which is
//! why a session that runs several packages prepares each of them separately.
//!
//! The comparison is the catalog's own integrity check, not a signature: a
//! package runs with the user's file and network permissions, so the same
//! user's files stay reachable, and signing or notarizing it is the author's
//! release step, not this launch's.
//!
//! The check is not a sandbox and not an atomic launch: the executable is
//! re-opened, read and hashed, and its path is then handed to the operating
//! system's exec, which opens it again. The supported targets have no exec-by-
//! descriptor, so a same-user process that rewrites the published copy inside
//! that window can still change what runs; such a process could rewrite this
//! host's own files for the same reason. The path is rejected when it became a
//! link, the bytes are compared immediately before the spawn, and the
//! content-addressed copy stays pinned, which is the strongest check available
//! here.

use std::ffi::OsString;
use std::fs;
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, ExitStatus};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use horon_agent_extension_types::PackageKind;
use horon_agent_platform::fs::{create_private_dir, is_executable, is_path_link, set_dir_permissions_owner_only};
use horon_agent_platform::process::package_child::{self, DEFAULT_GRACE, PackageChild, PackageChildSpec};
use sha2::{Digest, Sha256};

use super::catalog::{PackageLease, PackageRecord, PackageState, PackageStore};
use super::manifest::open_package_file;
use super::store::{self, Layout};

/// Directory under the package area that holds every package's private state.
const STATE_DIR: &str = "state";
/// Temporary directory inside one package's state directory.
const TEMP_DIR: &str = "tmp";
/// Bytes read per hashing step while verifying an executable.
const HASH_BUFFER_BYTES: usize = 64 * 1024;

impl PackageStore {
    /// Verify package `id` and pin the copy its executable comes from.
    ///
    /// `lease` is taken by value and owned by the returned executable. Only the
    /// record's own copy is pinned, so a replacement install or a collection in
    /// another process cannot reclaim the bytes this launch was verified against
    /// while every other installed copy stays collectable. The executable's
    /// recorded length and SHA-256 are checked against the published file, and
    /// the leaf is opened without following a link. A launch checks the file that
    /// runs; the other recorded files are covered by [`PackageStore::verify`].
    pub fn prepare_executable(&self, mut lease: PackageLease, id: &str) -> Result<PackageExecutable> {
        let view = self
            .pin_package(&mut lease, id)
            .with_context(|| format!("read the package catalog for {id}"))?;
        let generation = view.generation();
        let record = view
            .get(id)
            .with_context(|| format!("package {id} is not in the catalog"))?
            .clone();
        ensure!(
            record.state == PackageState::Installed,
            "package {id} is {}; only an installed package starts",
            state_name(record.state),
        );
        let digest = record
            .digest
            .clone()
            .with_context(|| format!("package {id} has no published copy"))?;
        let executable = record
            .executable
            .clone()
            .with_context(|| format!("package {id} has no recorded executable"))?;
        let layout = Layout::new(self.root());
        let root = store::digest_dir(&layout, &digest)?;
        ensure!(
            root.is_dir(),
            "the installed copy {digest} of package {id} is missing: {}",
            root.display(),
        );
        verify_executable(&root, &record)?;
        let state_dir = private_state_dir(self.root(), id)?;
        Ok(PackageExecutable {
            lease,
            id: id.to_owned(),
            record,
            generation,
            executable: root.join(Path::new(&executable)),
            root,
            state_dir,
            grace: DEFAULT_GRACE,
        })
    }

    /// Verify the exact copy `record` names and pin it for one launch.
    ///
    /// This is the launch a session makes from a catalog generation it already
    /// captured: the record is the one the turn pinned, so a replacement install
    /// that landed after the turn began cannot redirect the child to new bytes.
    /// The copy's recorded length and SHA-256 are checked against the published
    /// file before anything starts, exactly as [`PackageStore::prepare_executable`]
    /// checks the catalog's current record, and the lease the caller supplies is
    /// the one that keeps the copy alive.
    pub fn prepare_record_executable(
        &self,
        mut lease: PackageLease,
        record: &PackageRecord,
    ) -> Result<PackageExecutable> {
        ensure!(
            record.state == PackageState::Installed,
            "package {} is {}; only an installed package starts",
            record.id,
            state_name(record.state),
        );
        let digest = record
            .digest
            .clone()
            .with_context(|| format!("package {} has no published copy", record.id))?;
        let executable = record
            .executable
            .clone()
            .with_context(|| format!("package {} has no recorded executable", record.id))?;
        self.pin_recorded_copy(&mut lease, &digest)?;
        let layout = Layout::new(self.root());
        let root = store::digest_dir(&layout, &digest)?;
        ensure!(
            root.is_dir(),
            "the installed copy {digest} of package {} is missing: {}",
            record.id,
            root.display(),
        );
        verify_executable(&root, record)?;
        let state_dir = private_state_dir(self.root(), &record.id)?;
        Ok(PackageExecutable {
            lease,
            id: record.id.clone(),
            record: record.clone(),
            generation: record.generation,
            executable: root.join(Path::new(&executable)),
            root,
            state_dir,
            grace: DEFAULT_GRACE,
        })
    }
}

/// One verified installed executable, ready to start.
///
/// This value holds a lease on the installed copy. Starting it transfers the
/// lease to the returned [`PackageProcess`], which keeps it until it is dropped.
pub struct PackageExecutable {
    /// The catalog lease this launch is pinned under; it lives as long as this
    /// value and the process started from it.
    lease: PackageLease,
    id: String,
    record: PackageRecord,
    generation: u64,
    root: PathBuf,
    executable: PathBuf,
    state_dir: PathBuf,
    grace: Duration,
}

impl PackageExecutable {
    /// The catalog lease this executable is pinned under.
    pub fn lease(&self) -> &PackageLease {
        &self.lease
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The catalog record this launch was verified against.
    pub fn record(&self) -> &PackageRecord {
        &self.record
    }

    /// The catalog generation the record was read at.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Absolute path of the installed copy the child runs from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path of the installed executable.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The owner-private directory the child may write to.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The graceful-stop grace period this launch uses.
    pub fn grace(&self) -> Duration {
        self.grace
    }

    /// Replace the graceful-stop grace period.
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// The arguments the host passes before any of its own: the installed root.
    pub fn arguments(&self) -> Vec<OsString> {
        vec![self.root.clone().into_os_string()]
    }

    /// Start the executable and take ownership of the child's lifetime.
    ///
    /// The lease this executable was prepared under is carried into the returned
    /// process, so a session that runs several packages takes one lease per child
    /// and releasing one process cannot unpin a copy another process is running
    /// from.
    pub fn start(self) -> Result<PackageProcess> {
        self.start_with_arguments(&[])
    }

    /// [`PackageExecutable::start`], with arguments after the installed root.
    ///
    /// The package root always comes first, which is the argument the author SDK
    /// reads; anything here follows it. The bytes are checked immediately before
    /// the child starts; the remaining window is the operating system's own exec
    /// step, and a process that can rewrite the published copy can rewrite this
    /// host's files as well, so the check is integrity, not isolation.
    pub fn start_with_arguments(self, extra: &[OsString]) -> Result<PackageProcess> {
        // A hosted process refuses here, before the bytes are checked and before
        // any process exists: this is the one path a package's MCP server, its
        // extension child, its commands and its observers all take.
        super::child_gate::ensure(&self.id)?;
        // Verify again: the catalog record was read before this call, and the
        // bytes that run are the ones checked here.
        verify_executable(&self.root, &self.record)?;
        let mut arguments = Vec::with_capacity(extra.len() + 1);
        arguments.push(self.root.clone().into_os_string());
        arguments.extend(extra.iter().cloned());
        let kind = kind_name(self.record.kind);
        let spec = PackageChildSpec {
            program: &self.executable,
            arguments: &arguments,
            root: &self.root,
            id: &self.id,
            kind,
            state_dir: &self.state_dir,
            grace: self.grace,
        };
        let child =
            package_child::spawn(&spec).with_context(|| format!("start the executable of package {}", self.id))?;
        Ok(PackageProcess {
            child,
            executable: self,
        })
    }
}

/// One running package child and the lease that keeps its copy on disk.
pub struct PackageProcess {
    /// Declared before the executable so the child is stopped by its own drop
    /// before the executable's catalog lease is released.
    child: PackageChild,
    executable: PackageExecutable,
}

impl PackageProcess {
    /// The executable this process runs.
    pub fn executable(&self) -> &PackageExecutable {
        &self.executable
    }

    /// The catalog lease held for this process.
    pub fn lease(&self) -> &PackageLease {
        self.executable.lease()
    }

    /// The child's process ID.
    pub fn pid(&self) -> u32 {
        self.child.pid()
    }

    /// Take the child's standard input.
    ///
    /// The pipe is a std handle; a Tokio pipeline wraps it with
    /// `tokio::process::ChildStdin::from_std`.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.take_stdin()
    }

    /// Take the child's standard output.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.take_stdout()
    }

    /// Take the child's standard error.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.take_stderr()
    }

    /// Whether the leader has exited, without blocking.
    ///
    /// `Some` is reported only after the child's process group was swept and the
    /// leader reaped, so a descendant that outlived the leader is already gone.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Wait for the leader to exit, sweep its process group and reap it.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().context("wait for the package process")
    }

    /// Stop the child's process group and reap the leader.
    ///
    /// The group is asked to exit, given the configured grace period, and then
    /// killed, so a child that ignores the request still leaves nothing behind.
    /// Calling this after the leader is reaped reports that exit status and
    /// signals nothing.
    pub fn terminate(&mut self) -> Result<ExitStatus> {
        self.child.terminate().context("stop the package process")
    }
}

/// Check the executable `record` names against the bytes under `root`.
fn verify_executable(root: &Path, record: &PackageRecord) -> Result<()> {
    let relative = record
        .executable
        .as_deref()
        .with_context(|| format!("package {} records no executable", record.id))?;
    let recorded = record
        .files
        .iter()
        .find(|file| file.path == relative)
        .with_context(|| format!("package {} has no recorded executable {relative}", record.id))?;
    let mut file = open_package_file(root, Path::new(relative))
        .with_context(|| format!("open the executable of package {}", record.id))?;
    let mut hasher = Sha256::new();
    let mut len = 0u64;
    let mut buffer = vec![0u8; HASH_BUFFER_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read the executable of package {}", record.id))?;
        if read == 0 {
            break;
        }
        len += u64::try_from(read).context("the executable chunk length overflowed")?;
        hasher.update(&buffer[..read]);
    }
    let sha256 = hex::encode(hasher.finalize());
    ensure!(
        len == recorded.len && sha256 == recorded.sha256,
        "the installed executable of package {} is {len} bytes with sha256 {sha256}, recorded {} bytes with {}",
        record.id,
        recorded.len,
        recorded.sha256,
    );
    ensure!(
        is_executable(&root.join(relative)),
        "the installed executable of package {} is not runnable: {relative}",
        record.id,
    );
    Ok(())
}

/// Create one package's private state directory under the package area.
fn private_state_dir(store_root: &Path, id: &str) -> Result<PathBuf> {
    let state_root = store_root.join(STATE_DIR);
    create_package_dir(&state_root)?;
    let state = state_root.join(id);
    create_package_dir(&state)?;
    create_package_dir(&state.join(TEMP_DIR))?;
    Ok(state)
}

/// Create `path` as a directory only its owner can enter.
///
/// An existing entry is accepted only when it is a directory and not a link,
/// and its permissions are narrowed again.
fn create_package_dir(path: &Path) -> Result<()> {
    match create_private_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
            ensure!(
                metadata.is_dir() && !is_path_link(&metadata),
                "the package state path is not a directory without links: {}",
                path.display(),
            );
            set_dir_permissions_owner_only(path).with_context(|| format!("restrict {} to its owner", path.display()))
        }
        Err(error) => Err(error).with_context(|| format!("create {}", path.display())),
    }
}

fn state_name(state: PackageState) -> &'static str {
    match state {
        PackageState::Installed => "installed",
        PackageState::Disabled => "disabled",
        PackageState::Revoked => "removed",
    }
}

fn kind_name(kind: PackageKind) -> &'static str {
    match kind {
        PackageKind::Extension => "extension",
        PackageKind::Plugin => "plugin",
    }
}
