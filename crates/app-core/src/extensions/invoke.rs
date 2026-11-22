//! One call to an installed package child, and the child's lifetime.
//!
//! A package tool reaches its executable through this module and nothing else.
//! The shared dispatcher has already applied plan mode, the approval mode, the
//! session and SDK tool policy and the `pre_tool` gate to the exact input this
//! module sends, so this is the first code allowed to start a process. The
//! start happens once per session and package: the copy the turn pinned is
//! verified against its record, begun under a supervised child, handshaken over
//! `ExtensionWire`, and kept for the next call. A revocation callback registered
//! for the snapshot generation ends it: the waiting request fails because the
//! connection closed, and the supervised process tree is reaped.
//!
//! Three refusals happen before a byte crosses to the child: a handle whose
//! package left the catalog after the generation this session captured, an
//! input that still holds a secret placeholder, and an input that carries a
//! value the session vault already knows. A package runs with the user's
//! permissions and its environment is cleared, but nothing here hands it the
//! user's credentials.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use anyhow::{Context, Result, anyhow, bail, ensure};
use horon_agent_extension_types::ExtensionManifest;
use horon_agent_extension_types::wire::{
    Capability, CapabilitySet, CommandInvokeParams, HOST_PROTOCOL_RANGE, HostExpectation, InitializeParams,
    ToolCallParams, WireMethod,
};
use serde_json::Value;

use crate::extensions::lifecycle::{PackageLifecycleHolder, PackageLifecycleRegistry, RevocationEvent};
use crate::extensions::package_snapshot::{self, PackageSession};
use crate::extensions::wire::{ExtensionWire, FrameReader, WireError, WireLimits};
use crate::packages::{PackageProcess, PackageRecord, PackageStore};
use crate::secrets;

/// The wire capabilities this host can dispatch.
///
/// Both package kinds travel the same connection; the tool path and the shared
/// executable-command dispatch are the two the host drives today. Observer
/// notifications stay out until the hook wiring consumes them.
static HOST_CAPABILITIES: LazyLock<CapabilitySet> =
    LazyLock::new(|| CapabilitySet::new([Capability::ToolCall, Capability::CommandInvoke]));

/// Children alive in this process, one per session and package.
static CHILDREN: LazyLock<Mutex<HashMap<(String, String), Arc<Child>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Serializes child starts so two calls for one package cannot both launch it.
///
/// The lock covers the handshake as well, because a child that has not answered
/// `initialize` cannot take a request. A failing package therefore holds it for
/// at most one wire deadline, and only a package's first call ever waits.
static START: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Distinguishes one launch's lease owner from another's.
static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

fn lock_children() -> MutexGuard<'static, HashMap<(String, String), Arc<Child>>> {
    CHILDREN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One started package child, its connection and its revocation registration.
struct Child {
    session_id: String,
    package_id: String,
    /// The copy this child was verified against, so a replacement starts a new one.
    copy: String,
    wire: Arc<ExtensionWire>,
    /// Taken out to stop the child from a revocation, which must not block.
    process: Mutex<Option<PackageProcess>>,
    /// Unregisters the revocation callback when the child leaves memory.
    holder: Mutex<Option<PackageLifecycleHolder>>,
    /// Set by a revocation so a later call refuses to reuse the child.
    closed: AtomicBool,
    /// The runtime that owns the reader task, used to reap off the callback thread.
    runtime: tokio::runtime::Handle,
}

impl Child {
    /// End this child on behalf of a revocation.
    ///
    /// The process is moved to a blocking thread so the callback that published
    /// the revocation never waits for the process group to be swept. Closing the
    /// connection is what fails the request that was waiting on it.
    fn revoke(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        crate::logging::debug(&format!(
            "package {} for session {} was revoked; ending its child",
            self.package_id, self.session_id
        ));
        // The registration belongs to a child that is ending either way; taking
        // it here unregisters now instead of when the value leaves memory.
        drop(
            self.holder
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take(),
        );
        let process = self
            .process
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(mut process) = process {
            // A panic here is contained by the lifecycle registry, which calls
            // this callback; a shut-down runtime is the only way to reach it.
            drop(self.runtime.spawn_blocking(move || {
                if let Err(error) = process.terminate() {
                    crate::logging::warn(&format!("could not end a revoked package child: {error}"));
                }
            }));
        }
    }
}

/// The verified declarations of one installed copy.
pub(crate) struct Declarations {
    /// The copy this read came from, so a replacement is never reused.
    copy: String,
    /// Lowercase hex SHA-256 of the declaration file the record published.
    digest: String,
    pub(crate) manifest: ExtensionManifest,
}

impl Declarations {
    /// The declaration digest the handshake compares.
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
}

/// Declarations already read in this process, one per package ID.
///
/// The entry names the copy it was read from, so a replacement install is read
/// again on the first call that sees it and an unchanged copy costs one
/// comparison per turn.
static DECLARATIONS: LazyLock<Mutex<HashMap<String, Arc<Declarations>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_declarations() -> MutexGuard<'static, HashMap<String, Arc<Declarations>>> {
    DECLARATIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read the declarations of `record`'s installed copy, reusing a cached read.
///
/// The copy root comes from the record's digest, and the declaration bytes are
/// checked against the record before the manifest is used, so a call can only
/// ever see the declarations the installer published.
pub(crate) async fn declarations_for(store: &PackageStore, record: &PackageRecord) -> Result<Arc<Declarations>> {
    let copy = record
        .digest
        .clone()
        .with_context(|| format!("package {} has no published copy", record.id))?;
    if let Some(cached) = lock_declarations()
        .get(&record.id)
        .filter(|read| read.copy == copy)
        .cloned()
    {
        return Ok(cached);
    }
    let root = store.store_dir().join(&copy);
    let prepared = record.clone();
    let read = tokio::task::spawn_blocking(move || -> Result<Declarations> {
        let (manifest, digest) = crate::packages::read_recorded_declaration(&root, &prepared)?;
        ensure!(
            manifest.id == prepared.id && manifest.version == prepared.version,
            "the installed declaration of package {} names another package",
            prepared.id
        );
        Ok(Declarations { copy, digest, manifest })
    })
    .await
    .context("the package declaration read task failed")??;
    let read = Arc::new(read);
    lock_declarations().insert(record.id.clone(), Arc::clone(&read));
    Ok(read)
}

/// Call one tool a package declares and render its answer for the model.
pub(crate) async fn call_tool(session_id: &str, package_id: &str, tool: &str, arguments: Value) -> Result<String> {
    let value = dispatch(session_id, package_id, Surface::Tool(tool), arguments).await?;
    Ok(render(value))
}

/// Invoke one executable command a package declares.
///
/// This is the shared dispatch the slash-command consumer will call; it applies
/// the same generation, declaration and secret checks as a tool call, and the
/// same child lifetime.
pub async fn call_command(session_id: &str, package_id: &str, command: &str, arguments: Value) -> Result<Value> {
    dispatch(session_id, package_id, Surface::Command(command), arguments).await
}

#[derive(Clone, Copy)]
enum Surface<'a> {
    Tool(&'a str),
    Command(&'a str),
}

impl Surface<'_> {
    fn method(self) -> WireMethod {
        match self {
            Self::Tool(_) => WireMethod::ToolCall,
            Self::Command(_) => WireMethod::CommandInvoke,
        }
    }

    fn params(self, arguments: Value) -> Result<Value> {
        match self {
            Self::Tool(tool) => serde_json::to_value(ToolCallParams {
                tool: tool.to_owned(),
                arguments,
            }),
            Self::Command(command) => serde_json::to_value(CommandInvokeParams {
                command: command.to_owned(),
                arguments,
            }),
        }
        .context("encode the package call")
    }
}

async fn dispatch(session_id: &str, package_id: &str, surface: Surface<'_>, arguments: Value) -> Result<Value> {
    let session = package_snapshot::for_session(session_id)
        .with_context(|| format!("session {session_id} has no package generation"))?;
    let handle = session
        .handle(package_id)
        .with_context(|| format!("package {package_id} is not part of this turn's generation"))?;
    handle.ensure_live(&session).await?;
    let generation = handle.generation();
    let record = handle
        .record()
        .with_context(|| format!("package {package_id} has no installed record"))?
        .clone();
    let declarations = declarations_for(session.store(), &record).await?;
    match surface {
        Surface::Tool(tool) => ensure!(
            declarations.manifest.tools.iter().any(|declared| declared.name == tool),
            "package {package_id} does not declare the tool {tool}"
        ),
        Surface::Command(command) => ensure!(
            declarations
                .manifest
                .commands
                .iter()
                .any(|declared| declared.name == command),
            "package {package_id} does not declare the command {command}"
        ),
    }
    refuse_secret_bearing(session_id, &arguments)?;
    let child = child_for(session_id, generation, &session, &record, &declarations).await?;
    let params = surface.params(arguments)?;
    child
        .wire
        .request_value(surface.method(), Some(params))
        .await
        .map_err(|error| anyhow!("package {package_id} did not answer the call: {error}"))
}

/// Refuse an input that would hand the child a secret.
///
/// The dispatcher restored every placeholder it knew before the `pre_tool` gate,
/// so a marker still here belongs to no value this session holds and must not
/// travel. A value the vault already knows is refused too: the child runs with
/// the user's permissions, so a credential the masker recognised is exactly
/// what a package must not receive.
fn refuse_secret_bearing(session_id: &str, arguments: &Value) -> Result<()> {
    let payload = serde_json::to_string(arguments).context("encode the package call arguments")?;
    if payload.contains("«SECRET:") {
        bail!("the package call arguments hold an unresolved secret placeholder");
    }
    let masked = secrets::session_vault_with(session_id, |vault| secrets::obfuscate(&payload, vault, &[]))
        .unwrap_or_else(|| {
            let mut vault = secrets::SecretVault::new();
            secrets::obfuscate(&payload, &mut vault, &[])
        });
    if masked != payload {
        bail!("the package call arguments carry a known secret value");
    }
    Ok(())
}

/// The child for this session's package, started now if none is live.
async fn child_for(
    session_id: &str,
    generation: u64,
    session: &PackageSession,
    record: &PackageRecord,
    declarations: &Declarations,
) -> Result<Arc<Child>> {
    let key = (session_id.to_owned(), record.id.clone());
    let copy = record
        .digest
        .clone()
        .with_context(|| format!("package {} names no copy", record.id))?;
    let _start = START.lock().await;
    if let Some(existing) = lock_children().get(&key).cloned() {
        if !existing.closed.load(Ordering::SeqCst) && existing.copy == copy {
            return Ok(existing);
        }
        existing.revoke();
        let removed = lock_children().remove(&key);
        drop(removed);
    }
    let child = start(session_id, generation, session, record, declarations, copy).await?;
    let replaced = lock_children().insert(key, Arc::clone(&child));
    drop(replaced);
    Ok(child)
}

/// Launch, handshake and register one package child.
async fn start(
    session_id: &str,
    generation: u64,
    session: &PackageSession,
    record: &PackageRecord,
    declarations: &Declarations,
    copy: String,
) -> Result<Arc<Child>> {
    let package_id = record.id.clone();
    let owner = format!(
        "call-{}-{}",
        std::process::id(),
        NEXT_OWNER.fetch_add(1, Ordering::Relaxed)
    );
    let store = session.store().clone();
    let prepared = record.clone();
    let launch_id = package_id.clone();
    let started = tokio::task::spawn_blocking(move || -> Result<Started> {
        let lease = store.lease(&owner)?;
        let mut process = store.prepare_record_executable(lease, &prepared)?.start()?;
        let stdin = process
            .take_stdin()
            .with_context(|| format!("package {launch_id} has no standard input pipe"))?;
        let stdout = process
            .take_stdout()
            .with_context(|| format!("package {launch_id} has no standard output pipe"))?;
        Ok(Started { process, stdin, stdout })
    })
    .await
    .context("the package launch task failed")??;

    let writer = tokio::process::ChildStdin::from_std(started.stdin)
        .with_context(|| format!("adopt the standard input of package {package_id}"))?;
    let reader = tokio::process::ChildStdout::from_std(started.stdout)
        .with_context(|| format!("adopt the standard output of package {package_id}"))?;
    let wire = Arc::new(ExtensionWire::new(writer, WireLimits::default()));
    let reader_wire = Arc::clone(&wire);
    let log_id = package_id.clone();
    let _reader = tokio::spawn(async move {
        let mut frames = FrameReader::new(reader, WireLimits::default());
        loop {
            match frames.next().await {
                Ok(Some(frame)) => {
                    // A late or unexpected frame is reported and dropped; it
                    // never ends the connection on its own.
                    if let Err(error) = reader_wire.dispatch(frame) {
                        if !matches!(error, WireError::UnexpectedResponse(_) | WireError::QueueFull) {
                            crate::logging::debug(&format!(
                                "package {log_id} sent a frame the host could not route: {error}"
                            ));
                        }
                    }
                }
                Ok(None) => break,
                Err(WireError::ReadTimeout) => continue,
                Err(error) => {
                    crate::logging::debug(&format!("package {log_id} stream ended: {error}"));
                    break;
                }
            }
        }
        reader_wire.handle_eof();
    });

    let child = Arc::new(Child {
        session_id: session_id.to_owned(),
        package_id: package_id.clone(),
        copy,
        wire: Arc::clone(&wire),
        process: Mutex::new(Some(started.process)),
        holder: Mutex::new(None),
        closed: AtomicBool::new(false),
        runtime: tokio::runtime::Handle::current(),
    });

    let declared = declarations.manifest.capabilities();
    let params = InitializeParams {
        protocol: HOST_PROTOCOL_RANGE,
        capabilities: HOST_CAPABILITIES.clone(),
        package_id: package_id.clone(),
        kind: record.kind,
    };
    let expectation = HostExpectation {
        protocol: HOST_PROTOCOL_RANGE,
        capabilities: &HOST_CAPABILITIES,
        declared: &declared,
        kind: record.kind,
        package_id: &package_id,
        declaration_digest: declarations.digest(),
    };
    let revoked = Arc::downgrade(&child);
    let callback = Arc::new(move |event: &RevocationEvent| {
        if let Some(child) = revoked.upgrade() {
            crate::logging::info(&format!(
                "package {} was {} at catalog generation {}; a child from generation {generation} stops",
                event.package_id,
                event.cause.as_str(),
                event.generation
            ));
            child.revoke();
        }
    });
    // Registered before the handshake so a revocation published in that window
    // is refused here instead of starting a package the user just disabled.
    let holder = match PackageLifecycleRegistry::shared().register(package_id.clone(), generation, callback) {
        Ok(holder) => holder,
        Err(error) => {
            child.revoke();
            return Err(error);
        }
    };
    *child.holder.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(holder);

    if let Err(error) = wire.initialize(params, &expectation).await {
        child.revoke();
        return Err(anyhow!("package {package_id} did not complete the handshake: {error}"));
    }
    // A revocation that landed during the handshake already asked this child to
    // stop; do not hand it back for a call.
    if child.closed.load(Ordering::SeqCst) {
        return Err(anyhow!("package {package_id} was revoked while its child was starting"));
    }
    Ok(child)
}

struct Started {
    process: PackageProcess,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
}

/// Render a child's result as the tool text the model reads.
fn render(value: Value) -> String {
    match value {
        Value::String(text) => text,
        other => serde_json::to_string_pretty(&other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Drop one session's children, called when its agent leaves memory.
///
/// The map entry is removed first so nothing can reuse the child, then the
/// child is revoked: a request waiting on it fails as the connection closes and
/// the process tree is reaped off this thread.
pub(crate) fn forget_session(session_id: &str) {
    let removed: Vec<Arc<Child>> = {
        let mut children = lock_children();
        let keys: Vec<(String, String)> = children
            .keys()
            .filter(|(session, _)| session == session_id)
            .cloned()
            .collect();
        keys.into_iter().filter_map(|key| children.remove(&key)).collect()
    };
    for child in removed {
        child.revoke();
    }
}
