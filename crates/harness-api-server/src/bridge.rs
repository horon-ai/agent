//! The API bridge's listener side: who may bind, and how long it lives.
//!
//! Split out of `lib.rs` so the translation surface and the process lifecycle
//! stay separately readable; the accept loop, the single-instance lock and the
//! idle/parent exit all live here (#429).

use crate::{allocator, run_bridge_stream_with_deadline, writer};
use anyhow::{Context, Result};
use horon_agent_transport::{Listener, Stream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the bridge keeps its socket with no client connected.
///
/// The bridge belongs to the terminal that started it, not to the machine. A
/// bridge that outlived its client kept the socket path, and the
/// single-instance lock then made the oldest bridge the survivor of every
/// later launch: after an upgrade the operator's next client reconnected to
/// the previous release's executable (#429). A minute is longer than a client
/// takes to reconnect after a dropped connection and short enough that the
/// current release answers the next launch.
pub const IDLE_EXIT: Duration = Duration::from_secs(60);

/// How often an idle bridge re-checks its reason to stay alive. Capped by the
/// idle window so a test-sized window is honoured to the tick.
const LIFECYCLE_TICK: Duration = Duration::from_secs(1);

/// Connected clients, and when that count last dropped to zero.
struct Activity {
    connected: AtomicUsize,
    idle_since: Mutex<Instant>,
}

impl Activity {
    fn new() -> Self {
        Self {
            connected: AtomicUsize::new(0),
            idle_since: Mutex::new(Instant::now()),
        }
    }

    fn clients(&self) -> usize {
        self.connected.load(Ordering::Acquire)
    }

    fn idle_for(&self) -> Duration {
        // A poisoned lock cannot make the bridge immortal: the worst reading
        // is "idle since the start", which shortens its life, never extends it.
        // A panic while the lock was held cannot leave the `Instant` wrong, so
        // a poisoned lock still answers: a guess here would decide how long the
        // bridge lives.
        let since = match self.idle_since.lock() {
            Ok(since) => since,
            Err(poisoned) => poisoned.into_inner(),
        };
        since.elapsed()
    }
}

/// One connected client's hold on the bridge's lifetime.
///
/// The count is released on drop rather than at the end of the handler, so an
/// aborted or panicking client task cannot pin the bridge open.
struct ClientSlot(Arc<Activity>);

impl ClientSlot {
    fn new(activity: Arc<Activity>) -> Self {
        activity.connected.fetch_add(1, Ordering::AcqRel);
        Self(activity)
    }
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.connected.fetch_sub(1, Ordering::AcqRel);
        let mut since = match self.0.idle_since.lock() {
            Ok(since) => since,
            Err(poisoned) => poisoned.into_inner(),
        };
        *since = Instant::now();
    }
}

/// Why the bridge decided to stop.
type ExitReason = &'static str;

/// Ask the bridge to stop once it has no client and no reason to wait.
///
/// Runs beside the accept loop instead of inside it: an accept future that is
/// dropped and re-polled on a timer would lose a connection attempt on
/// Windows, where a named-pipe server republishes an instance per accept. The
/// accept below is therefore only ever cancelled once, on the way out.
async fn watch_lifecycle(
    activity: Arc<Activity>,
    idle_exit: Duration,
    parent: Option<u32>,
    shutdown: tokio::sync::watch::Sender<Option<ExitReason>>,
) {
    let tick = LIFECYCLE_TICK.min(idle_exit);
    loop {
        tokio::time::sleep(tick).await;
        if shutdown.is_closed() {
            return;
        }
        if activity.clients() > 0 {
            continue;
        }
        // The spawning process (the terminal's `horon`, or whatever asked for
        // the bridge) is the reason this process exists. `parent_pid` is None
        // on Windows, where the platform exposes no parent, so only the idle
        // window ends a bridge there.
        let reason = if parent.is_some_and(|pid| !horon_agent_platform::is_process_running(pid)) {
            Some("its parent process is gone")
        } else if activity.idle_for() >= idle_exit {
            Some("no client is connected")
        } else {
            None
        };
        if let Some(reason) = reason {
            // The receiver being gone means the accept loop already ended, so
            // there is nobody left to tell and nothing to report.
            let _sent = shutdown.send(Some(reason));
            return;
        }
    }
}

/// Take the exclusive bridge lock beside the API socket, or report that a live
/// bridge already holds it.
///
/// The lock is released when the holder exits, however it exits, so a crashed
/// bridge never wedges the next one out.
pub(crate) fn single_instance_lock(
    api_socket: &std::path::Path,
) -> Result<Option<horon_agent_platform::ipc::InstanceLock>> {
    let path = api_socket.with_extension("lock");
    horon_agent_platform::ipc::lock_instance_file(&path).with_context(|| format!("open bridge lock {}", path.display()))
}

pub async fn run_bridge(api_socket: PathBuf, legacy_socket: PathBuf) -> Result<()> {
    run_bridge_with_deadline(api_socket, legacy_socket, writer::WRITE_STALL_DEADLINE).await
}

/// [`run_bridge`] with the write deadline injected, so a test can watch a
/// stalled peer close its connection without waiting the production interval.
pub(crate) async fn run_bridge_with_deadline(
    api_socket: PathBuf,
    legacy_socket: PathBuf,
    write_deadline: std::time::Duration,
) -> Result<()> {
    run_bridge_with_lifecycle(
        api_socket,
        legacy_socket,
        write_deadline,
        IDLE_EXIT,
        horon_agent_platform::signal::parent_pid(),
    )
    .await
}

/// [`run_bridge_with_deadline`] with the idle window and the spawning process
/// injected, so a test can watch the bridge end without waiting a minute and
/// does not depend on which process started the test binary.
pub(crate) async fn run_bridge_with_lifecycle(
    api_socket: PathBuf,
    legacy_socket: PathBuf,
    write_deadline: std::time::Duration,
    idle_exit: Duration,
    parent: Option<u32>,
) -> Result<()> {
    // Only one bridge may own the socket.
    //
    // This is the fix for clients seeing "disconnected: harness API stream
    // closed" at random: every desktop client spawned a bridge on demand, and
    // each new bridge unlinked the live socket and bound its own. The older
    // bridges kept running with their connected clients, but the *pathname*
    // now pointed at the newest one, and each reconnect churned the same way.
    // Whoever lost the race had its clients dropped. Refusing to start when a
    // live bridge holds the lock makes on-demand spawning idempotent, which is
    // what every caller already assumes.
    let _lock = match single_instance_lock(&api_socket) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            eprintln!(
                "harness API bridge: another bridge already owns {}; exiting",
                api_socket.display()
            );
            return Ok(());
        }
        // The lock is advisory. A platform that cannot open it right now - a
        // filter driver holding the file, a directory the caller may not write -
        // must not stop the only bridge serving this desktop, so the launch
        // continues and reports why the single-owner check was skipped.
        Err(error) => {
            eprintln!(
                "harness API bridge: single-instance lock for {} is unavailable ({error:#}); continuing",
                api_socket.display()
            );
            None
        }
    };
    // A stale socket file blocks bind. Only a platform whose listening
    // endpoints are filesystem nodes has one to remove: on Windows the pipe
    // namespace is not the filesystem, and probing the pipe would consume one
    // published instance of a server that is already gone. Unlinking is safe
    // because a live bridge would have made this process exit above, whatever
    // the advisory lock answered.
    if horon_agent_platform::ipc::socket_nodes_can_be_stale() {
        horon_agent_platform::ipc::remove_socket(&api_socket);
    }
    if let Some(parent) = api_socket.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        // Bind reports the real failure if the directory is still missing; the
        // creation attempt is worth a line because a permissions problem here
        // points at the installation, not at the bridge.
        eprintln!("harness API bridge: cannot create {}: {error}", parent.display());
    }
    // Accepting takes `&mut self` on both platforms: a Windows named-pipe
    // listener republishes a pipe instance for every accept, and the adapter
    // gives the Unix socket the same shape so callers never branch.
    let mut listener =
        Listener::bind(&api_socket).with_context(|| format!("bind API socket {}", api_socket.display()))?;
    // Restrict the socket to its owner, matching the daemon socket it fronts.
    //
    // Without this the bridge widens access to everything behind it: the
    // daemon socket is 0600, but a default-umask bind here produced 0755, so
    // any local user could drive sessions, read transcripts, and spend the
    // owner's provider tokens. A bridge must never be more permissive than
    // the thing it bridges to.
    //
    // A Windows named pipe carries an ACL rather than a file mode, so the
    // adapter's restriction is empty there and the pipe keeps its own access.
    horon_agent_platform::ipc::restrict_socket_to_owner(&api_socket)
        .with_context(|| format!("restrict API socket {}", api_socket.display()))?;
    eprintln!(
        "harness API bridge: listening on {} -> {}",
        api_socket.display(),
        legacy_socket.display()
    );
    // Lifecycle: the bridge belongs to the terminal that started it.
    //
    // It used to accept forever. Nothing observed that the spawning terminal
    // had closed or that the last client had left, and the single-instance
    // lock then made the oldest bridge the survivor of every later launch, so
    // an upgraded install kept answering from the previous release's binary.
    // A bridge now ends when it has no client and either its parent is gone or
    // it has been idle for `idle_exit`, releasing the lock and unlinking the
    // socket node so the next launch binds one from the current release. A
    // client that is still connected keeps it serving.
    let activity = Arc::new(Activity::new());
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel::<Option<ExitReason>>(None);
    let watchdog = tokio::spawn(watch_lifecycle(activity.clone(), idle_exit, parent, shutdown_tx));
    loop {
        tokio::select! {
            // The watchdog decides; this side only notices. `changed` is cancel
            // safe, and the accept below is dropped at most once, on exit.
            changed = shutdown_rx.changed() => {
                if changed.is_err() || shutdown_rx.borrow().is_some() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => return Err(error.into()),
                };
                let legacy = legacy_socket.clone();
                let slot = ClientSlot::new(activity.clone());
                tokio::spawn(async move {
                    if let Err(error) = handle_api_client(stream, legacy, write_deadline).await {
                        eprintln!("harness API bridge: client ended: {error:#}");
                    }
                    // A closed client drops its translation state and buffers,
                    // and releasing the slot starts the idle window.
                    drop(slot);
                    allocator::trim();
                });
            }
        }
    }
    watchdog.abort();
    let reason = (*shutdown_rx.borrow()).unwrap_or("the accept loop ended");
    // The socket node goes before the lock is released: a launch that finds no
    // socket starts a bridge, and one that finds the lock held exits, so
    // removing the node while still holding the lock keeps a new bridge from
    // ever binding to the path this process is about to abandon.
    drop(listener);
    if horon_agent_platform::ipc::socket_nodes_can_be_stale() {
        horon_agent_platform::ipc::remove_socket(&api_socket);
    }
    eprintln!("harness API bridge: exiting because {reason}");
    Ok(())
}

async fn handle_api_client(stream: Stream, legacy_socket: PathBuf, write_deadline: std::time::Duration) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    run_bridge_stream_with_deadline(read_half, write_half, legacy_socket, write_deadline).await
}
