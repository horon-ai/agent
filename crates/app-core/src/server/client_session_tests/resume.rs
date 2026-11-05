use super::*;
use crate::transport::WriteHalf;
use anyhow::{Result, anyhow};

/// Keeps the isolated runtime and agent-home directories alive for the test body.
struct IsolatedDirs {
    _runtime: tempfile::TempDir,
    _home: tempfile::TempDir,
}

/// Restores the process environment when the test body ends, including on `?`.
struct RuntimeRestore {
    prev_runtime: Option<std::ffi::OsString>,
    prev_home: Option<std::ffi::OsString>,
}

impl Drop for RuntimeRestore {
    fn drop(&mut self) {
        if let Some(prev_runtime) = self.prev_runtime.take() {
            crate::env::set_var("HORON_AGENT_RUNTIME_DIR", prev_runtime);
        } else {
            crate::env::remove_var("HORON_AGENT_RUNTIME_DIR");
        }
        if let Some(prev_home) = self.prev_home.take() {
            crate::env::set_var("HORON_AGENT_HOME", prev_home);
        } else {
            crate::env::remove_var("HORON_AGENT_HOME");
        }
    }
}

/// Isolate both the runtime dir and the agent home.
///
/// These tests persist sessions, so they must never touch the ambient home: a
/// home that still carries a pre-event-store index makes the store demand an
/// offline migration, and a home shared with another test leaks sessions
/// between cases. The store also requires every directory under the home to be
/// owner-private (0700), so `sessions/` is created with that mode instead of the
/// umask default.
fn setup_runtime_dir() -> Result<(IsolatedDirs, RuntimeRestore)> {
    let runtime = tempfile::TempDir::new().map_err(|e| anyhow!(e))?;
    let home = tempfile::TempDir::new().map_err(|e| anyhow!(e))?;
    crate::storage::create_store_dir(&home.path().join("sessions"), false).map_err(|e| anyhow!(e))?;
    let prev_runtime = std::env::var_os("HORON_AGENT_RUNTIME_DIR");
    let prev_home = std::env::var_os("HORON_AGENT_HOME");
    crate::env::set_var("HORON_AGENT_RUNTIME_DIR", runtime.path());
    crate::env::set_var("HORON_AGENT_HOME", home.path());
    Ok((
        IsolatedDirs {
            _runtime: runtime,
            _home: home,
        },
        RuntimeRestore {
            prev_runtime,
            prev_home,
        },
    ))
}

fn restore_runtime_dir(restore: RuntimeRestore) {
    drop(restore);
}

fn test_writer() -> Result<(Arc<Mutex<WriteHalf>>, crate::transport::Stream)> {
    let (stream_a, stream_b) = crate::transport::stream_pair().map_err(|e| anyhow!(e))?;
    let (_reader, writer_half) = stream_a.into_split();
    Ok((Arc::new(Mutex::new(writer_half)), stream_b))
}

include!("resume/multiple_live_attach.rs");
include!("resume/busy_existing_attach.rs");
include!("resume/reconnect_takeover_with_history.rs");
include!("resume/close_barrier.rs");
include!("resume/attach_without_local_history.rs");
include!("resume/different_client_attach.rs");
include!("resume/live_events_before_history.rs");
include!("resume/same_client_takeover.rs");
include!("resume/same_id_resubscribe.rs");
