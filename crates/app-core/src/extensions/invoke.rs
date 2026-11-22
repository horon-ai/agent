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
//! Observation notifications take this same child and connection. They carry no
//! answer and no input, and they are the one case where a child may start
//! without a tool call (`extensions::observer` drives that, never in plan mode).
//!
//! Three refusals happen before a byte crosses to the child: a handle whose
//! package left the catalog after the generation this session captured, an
//! input that still holds a secret placeholder, and an input that carries a
//! value the session vault already knows. A package runs with the user's
//! permissions and its environment is cleared, but nothing here hands it the
//! user's credentials.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use anyhow::{Context, Result, anyhow, bail, ensure};
use horon_agent_extension_types::wire::{
    Capability, CapabilitySet, CommandInvokeParams, HOST_PROTOCOL_RANGE, HostExpectation, InitializeParams,
    ObserverParams, ToolCallParams, WireMethod,
};
use horon_agent_extension_types::{ExtensionManifest, ObserverEvent};
use serde_json::Value;

use crate::extensions::lifecycle::{PackageLifecycleHolder, PackageLifecycleRegistry, RevocationEvent};
use crate::extensions::package_snapshot;
use crate::extensions::wire::{ExtensionWire, FrameReader, WireError, WireLimits};
use crate::packages::{PackageProcess, PackageRecord, PackageStore};
use crate::secrets;

/// The wire capabilities this host can dispatch.
///
/// Both package kinds travel the same connection; the tool path, the shared
/// executable-command dispatch and the observation notifications a declared
/// event set subscribes to are the ones the host drives today.
static HOST_CAPABILITIES: LazyLock<CapabilitySet> = LazyLock::new(|| {
    CapabilitySet::new([
        Capability::ToolCall,
        Capability::CommandInvoke,
        Capability::ObserverNotify,
    ])
});

/// Children alive in this process, one per session and package.
type ChildMap = HashMap<(String, String), Arc<Child>>;
static CHILDREN: LazyLock<Mutex<ChildMap>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Serializes child starts so two calls for one package cannot both launch it.
///
/// The lock covers the handshake as well, because a child that has not answered
/// `initialize` cannot take a request. A failing package therefore holds it for
/// at most one wire deadline, and only a package's first call ever waits.
static START: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Distinguishes one launch's lease owner from another's.
static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

fn lock_children() -> MutexGuard<'static, ChildMap> {
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
    /// Every event this package's child observes: the declaration's own
    /// `events` plus the events its bundled hook files subscribe to.
    events: BTreeSet<ObserverEvent>,
}

impl Declarations {
    /// The declaration digest the handshake compares.
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    /// Whether the package subscribed to `event` when the copy was installed.
    pub(crate) fn observes(&self, event: ObserverEvent) -> bool {
        self.events.contains(&event)
    }

    /// The capabilities the host may offer this package.
    ///
    /// The declaration's own tools and commands are not the whole surface: a
    /// plugin may subscribe through a bundled hook file without naming an event
    /// in `extension.toml`, so the offer has to follow the merged set.
    fn capabilities(&self) -> CapabilitySet {
        let mut capabilities = self.manifest.capabilities();
        if !self.events.is_empty() {
            capabilities.insert(Capability::ObserverNotify);
        }
        capabilities
    }
}

/// Declarations already read in this process, one per package ID.
///
/// The entry names the copy it was read from, so a replacement install is read
/// again on the first call that sees it and an unchanged copy costs one
/// comparison per turn.
type DeclarationMap = HashMap<String, Arc<Declarations>>;
static DECLARATIONS: LazyLock<Mutex<DeclarationMap>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_declarations() -> MutexGuard<'static, DeclarationMap> {
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
        let events = crate::packages::observation_events(&root, &prepared, &manifest)?;
        Ok(Declarations {
            copy,
            digest,
            manifest,
            events,
        })
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
    let child = child_for(session_id, generation, session.store(), &record, &declarations).await?;
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
    store: &PackageStore,
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
    let child = start(session_id, generation, store, record, declarations, copy).await?;
    let replaced = lock_children().insert(key, Arc::clone(&child));
    drop(replaced);
    Ok(child)
}

/// Launch, handshake and register one package child.
async fn start(
    session_id: &str,
    generation: u64,
    store: &PackageStore,
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
    let store = store.clone();
    let prepared = record.clone();
    let launch_id = package_id.clone();
    let started = tokio::task::spawn_blocking(move || -> Result<Started> {
        let lease = store.lease(&owner)?;
        let mut process = store.prepare_record_executable(lease, &prepared)?.start()?;
        horon_agent_perf_counters::record_package_child_start();
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
                    if let Err(error) = reader_wire.dispatch(frame)
                        && !matches!(error, WireError::UnexpectedResponse(_) | WireError::QueueFull)
                    {
                        crate::logging::debug(&format!(
                            "package {log_id} sent a frame the host could not route: {error}"
                        ));
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

    let declared = declarations.capabilities();
    // Offer only what the installed manifest declares: a child answers with
    // the capabilities it was offered, and one it never declared is refused.
    let offered = HOST_CAPABILITIES.intersection(&declared);
    let params = InitializeParams {
        protocol: HOST_PROTOCOL_RANGE,
        capabilities: offered.clone(),
        package_id: package_id.clone(),
        kind: record.kind,
    };
    let expectation = HostExpectation {
        protocol: HOST_PROTOCOL_RANGE,
        capabilities: &offered,
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

/// One package that subscribed to an observation, resolved at a generation.
///
/// The record travels with the target so the delivery task does not read the
/// catalog again for every event: the snapshot that decided the subscription
/// already pinned the copy, and the record is the one it captured.
pub(crate) struct ObservationTarget {
    package_id: String,
    generation: u64,
    record: PackageRecord,
}

impl ObservationTarget {
    pub(crate) fn package_id(&self) -> &str {
        &self.package_id
    }
}

/// Whether `package_id` was revoked after `generation`.
///
/// A revocation records the catalog generation that published it, so a target
/// resolved at that generation or later is still allowed and one resolved before
/// it is stale: the user disabled or removed the package after the snapshot the
/// observation came from.
pub(crate) fn revoked_after(registry: &Arc<PackageLifecycleRegistry>, package_id: &str, generation: u64) -> bool {
    registry
        .revocation(package_id)
        .is_some_and(|event| event.generation > generation)
}

/// The packages whose installed declaration subscribes to `event`.
///
/// Runs on the observation worker, never on a turn's path: it may capture the
/// session's first package snapshot and read declaration files. A package that
/// was revoked after the snapshot was taken is skipped here, before any child is
/// considered, so a disabled package receives nothing even while its child is
/// still being reaped.
pub(crate) async fn observation_targets(
    store: &PackageStore,
    session_id: &str,
    event: ObserverEvent,
    workspace: Option<&Path>,
) -> Vec<ObservationTarget> {
    // Only the first observation of a session may open its package area. A later
    // one, `session.end` after the session was cleaned up for instance, must not
    // recreate a slot, a lease and a child for a session that is already gone.
    let session = if event == ObserverEvent::SessionStart {
        package_snapshot::open_for(session_id, store.clone())
    } else {
        match package_snapshot::for_session(session_id) {
            Some(session) => session,
            None => return Vec::new(),
        }
    };
    let snapshot = match session.current() {
        Some(snapshot) => snapshot,
        None => match session.enter_turn(workspace).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                crate::logging::debug(&format!(
                    "package observation for session {session_id} found no generation: {error:#}"
                ));
                return Vec::new();
            }
        },
    };
    let registry = PackageLifecycleRegistry::shared();
    let mut targets = Vec::new();
    for record in snapshot.packages() {
        if revoked_after(&registry, &record.id, snapshot.generation()) {
            continue;
        }
        match declarations_for(store, record).await {
            Ok(declarations) if declarations.observes(event) => targets.push(ObservationTarget {
                package_id: record.id.clone(),
                generation: snapshot.generation(),
                record: record.clone(),
            }),
            Ok(_) => {}
            Err(error) => crate::logging::debug(&format!(
                "package {} declarations are unavailable for an observation: {error:#}",
                record.id
            )),
        }
    }
    targets
}

/// Whether an observation may start a package child for this session.
///
/// Planning is read-only, so an observation is never a reason to create a
/// process; a package tool or command call in the same session still starts its
/// child through the ordinary approved path. A child that is already running
/// keeps receiving notifications either way.
pub(crate) fn observation_may_start_child(session_id: &str) -> bool {
    !crate::plan_mode::state_for(session_id).is_read_only()
}

/// Send one observation to one package child, starting the child when needed.
///
/// The approval and plan-mode decisions are already made: this is only reached
/// for an event the host decided to publish, and no child is started while the
/// session is planning. A slow or stuck child therefore delays this delivery,
/// which runs on the observation worker, and never the turn that published it.
pub(crate) async fn deliver_observation(
    store: &PackageStore,
    session_id: &str,
    target: &ObservationTarget,
    params: &ObserverParams,
) -> Result<()> {
    let package_id = target.package_id.as_str();
    let registry = PackageLifecycleRegistry::shared();
    ensure!(
        !revoked_after(&registry, package_id, target.generation),
        "package {package_id} was revoked after this observation was published"
    );
    let copy = target
        .record
        .digest
        .clone()
        .with_context(|| format!("package {package_id} has no published copy"))?;
    let key = (session_id.to_owned(), package_id.to_owned());
    let live = lock_children()
        .get(&key)
        .filter(|child| !child.closed.load(Ordering::SeqCst) && child.copy == copy)
        .cloned();
    let child = match live {
        Some(child) => child,
        None => {
            // Planning is read-only: an observation is never a reason to create
            // a process, and a session that is ending has no use for a new one.
            // A package tool or command call still starts its child through the
            // ordinary approved path.
            ensure!(
                params.event != ObserverEvent::SessionEnd,
                "no package child is running to receive session.end"
            );
            ensure!(
                observation_may_start_child(session_id),
                "plan mode does not start a package child for an observation"
            );
            let declarations = declarations_for(store, &target.record).await?;
            ensure!(
                declarations.observes(params.event),
                "package {package_id} does not subscribe to {:?}",
                params.event
            );
            child_for(session_id, target.generation, store, &target.record, &declarations).await?
        }
    };
    child
        .wire
        .notify(params)
        .map_err(|error| anyhow!("package {package_id} did not accept the observation: {error}"))
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
