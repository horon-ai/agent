//! Local IPC transport, one API across platforms.
//!
//! A Unix socket on Unix, a named pipe on Windows, both exposed as `Listener`
//! and `Stream` with the same shape as `tokio::net::Unix*`. Callers write one
//! code path and get the platform's native mechanism.
//!
//! The implementation lives in the platform adapter's `ipc` module, which is
//! the only place in the workspace that may speak an operating system's local
//! IPC. This crate stays as the name the bridge, the SDK and the daemon import,
//! so those three never have to know which mechanism they got.

pub use horon_agent_platform::ipc::{
    Addr, InstanceLock, Listener, ReadHalf, Stream, SyncStream, WriteHalf, is_socket_path, lock_instance_file,
    remove_socket, restrict_socket_to_owner, stream_pair, sync_stream_shutdown_handle,
};
